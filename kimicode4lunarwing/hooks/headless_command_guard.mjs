#!/usr/bin/env node
import { lstatSync, realpathSync } from "node:fs";
import { basename, dirname, resolve as resolvePath } from "node:path";
import { runInNewContext } from "node:vm";

// headless_command_guard.mjs — LunarWing Kimi worker fail-closed command policy
// (WI#206).
//
// The worker runs `kimi -p --output-format stream-json` headless. Verified
// against 2.0.2 (live-verified; 2.5.2 real-CLI contract gate passes): print mode
// forces the auto permission policy, and the upstream dangerous-command guard
// is skipped in auto mode. Since 2.6.0 config [[permission.rules]] do load,
// in print mode too (the baked config's rules and, for a trusted workspace,
// those of <project>/.kimi-code/local.toml); the lane bakes no rule, keeps
// the workspace untrusted, and this hook fires in the earlier phase, before
// permission checks in every mode, blocking with exit code 2 — the only
// in-CLI fail-closed mechanism available.
//
// The hook receives the PreToolUse event as JSON on stdin. Exit codes:
//   0 — allow, 2 — block (stderr reason is surfaced to the model),
//   anything else — upstream treats as allow (fail-open), so this script
//   keeps its logic dependency-free and always exits 0 or 2.
//
// This guards the dangerous classes from the upstream analyzer
// (power/privilege/disk destruction, fork bombs, pipe-to-shell, decoded
// payloads into a shell) plus worker-lane specifics (deletion outside the
// temp roots whatever tool performs it, block-device writes, git commands
// that discard uncommitted work, SSH agent use — the mount path, the
// SSH_AUTH_SOCK variable and `ssh-add`). It is deliberately
// NOT a general command parser: everything it cannot positively classify as
// safe within its ruleset is allowed unless it matches a deny rule, EXCEPT
// unparseable input, which denies (fail closed at the transport layer) and
// destructive shapes whose target it cannot verify, which also deny.

const MAX_COMMAND_CHARS = 16384;
const RM_SAFE_TEMP_ROOTS = ["/tmp", "/temp", "/var/tmp"];
// Characters that make bash rewrite a word before the deleting command sees
// it: parameter and command expansion, quote removal, globs and brace lists.
// An operand is read as the literal argument, so a word carrying any of them
// would be checked in one shape and deleted in another (WI#830, from WI#812).
const RM_SHELL_EXPANDED_CHARS = "$`'\"*?[]{}";

function deny(reason) {
  process.stderr.write(`headless-guard: denied — ${reason}\n`);
  process.exit(2);
}

function allow() {
  process.exit(0);
}

function readStdin() {
  return new Promise((resolve, reject) => {
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => resolve(input));
    process.stdin.on("error", reject);
  });
}

/** Extract the operand tokens of an `rm` invocation (flags and redirects stripped). */
function rmOperands(words, endOfOptions) {
  return words
    .filter((token, i) => token.length > 0 &&
      (!token.startsWith("-") || (endOfOptions !== -1 && i > endOfOptions)))
    .map((token) => token.replace(/^["']|["']$/g, ""))
    // A redirection target is not an rm operand (e.g. `2>/dev/null`).
    .filter((token) => !/^\d*>/.test(token));
}

/**
 * The containment test shared by every deletion rule (WI#826, WI#830): a
 * target is provably inside the temp roots only as a literal absolute path,
 * with no expansion, no `..` segment and no glob, whose *resolved* form is
 * under a resolved root. Resolving is what makes a symlink under a temp root
 * useless as a bridge to somewhere else (WI#812, WI#830).
 */
function isSafeTempPath(operand) {
  if (!operand.startsWith("/") || operand.includes("..")) return false;
  for (const ch of operand) {
    if (RM_SHELL_EXPANDED_CHARS.includes(ch)) return false;
  }
  const resolved = resolveExistingPrefix(operand);
  if (resolved === null) return false;
  return RM_RESOLVED_SAFE_ROOTS.some((root) => resolved === root || resolved.startsWith(`${root}/`));
}

/**
 * Resolve the longest existing prefix of a path, following symlinks, and
 * re-attach the components that do not exist yet: a deletion removes those
 * names rather than traversing them, so there is no symlink to follow there.
 * Returns null when the prefix cannot be resolved (symlink loop, permission
 * error), or when a component exists but is a symlink that does not resolve:
 * its target can be created before the deletion runs, and the name below it
 * would then be removed through the link.
 */
function resolveExistingPrefix(target) {
  // A trailing slash makes every filesystem call follow the final component,
  // which hides a dangling link from the check below; realpath treats the two
  // spellings the same otherwise.
  let prefix = target.replace(/(.)\/+$/, "$1");
  const missing = [];
  for (;;) {
    try {
      const resolved = realpathSync.native(prefix);
      if (missing.length === 0) return resolved;
      const base = resolved.endsWith("/") ? resolved : `${resolved}/`;
      return `${base}${missing.reverse().join("/")}`;
    } catch (error) {
      if (error?.code !== "ENOENT" && error?.code !== "ENOTDIR") return null;
      if (isDanglingSymlink(prefix)) return null;
      const parent = dirname(prefix);
      if (parent === prefix) return null;
      missing.push(basename(prefix));
      prefix = parent;
    }
  }
}

function isDanglingSymlink(target) {
  try {
    return lstatSync(target).isSymbolicLink();
  } catch {
    return false;
  }
}

/**
 * Both sides are resolved: a temp root can itself be a symlink (macOS /tmp),
 * and a target can reach a root through one. A root that cannot be resolved
 * is dropped, so a broken root can only deny.
 */
const RM_RESOLVED_SAFE_ROOTS = RM_SAFE_TEMP_ROOTS
  .map((root) => resolveExistingPrefix(root))
  .filter((root) => root !== null);

/** Split a command tail into bash words (space and tab only). */
function argumentWords(rest) {
  return rest.split(/[ \t]+/).filter((word) => word.length > 0);
}

function stripShellQuotes(word) {
  return word.replace(/^["']+|["']+$/g, "");
}

/** Lexical path join: `//`, `.` and `..` collapse the way the kernel resolves them. */
function normalizePath(path) {
  const segments = [];
  for (const segment of path.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") segments.pop();
    else segments.push(segment);
  }
  return `/${segments.join("/")}`;
}

/**
 * Rewrite absolute path tokens to their lexical resolution, so `/./etc`,
 * `/tmp/../etc` and `//etc` read as `/etc` for every deny rule (WI#826). This
 * is an additional reading: the raw spellings stay checked, so normalising
 * can only add denials, never unmask one.
 */
function normalizeAbsolutePaths(text) {
  return text.replace(/(^|[\s;&|()<>"'`=,])(\/[^\s;&|()<>"'`]*)/g,
    (_match, lead, path) => lead + normalizePath(path));
}

function outsideTempReason(what, paths) {
  return `deletion outside ${RM_SAFE_TEMP_ROOTS.join(", ")}: ${what} ${paths.join(" ").slice(0, 80)}`;
}

// Join backslash-LF pairs before matching, for every rule. A pair is handled
// in every quote state, because the hook cannot tell a real quote from an apostrophe
// the shell reads as plain text (a heredoc body, `$'...'`). Each pair becomes
// `lineBreak`, or `quotedLineBreak` while the joiner believes it is inside
// single quotes. The caller reads the whole command three ways: every pair
// dropped (a continuation), every pair as a space (the line break that ends a
// comment or stays in quoted text), and dropped outside single quotes but a
// space inside them, as bash reads real single quotes. Dropping alone can glue
// a comment's last word to a following `rm`; not dropping can hide a continued
// rm (WI#810). Outside single quotes, escaped backslashes and backslashes in
// comments do not continue a shell line.
function joinLineContinuations(command, lineBreak = "", quotedLineBreak = lineBreak) {
  if (!command.includes("\\\n")) return command;
  let joined = "";
  let quote = "";
  let atWordStart = true;
  for (let i = 0; i < command.length; i++) {
    const ch = command[i];
    if (ch === "\\" && i + 1 < command.length) {
      if (command[i + 1] === "\n") {
        i++;
        joined += quote === "'" ? quotedLineBreak : lineBreak;
        continue;
      }
      if (quote !== "'") {
        joined += ch + command[++i];
        atWordStart = false;
        continue;
      }
    }
    if (quote) {
      if (ch === quote) quote = "";
    } else if (ch === "'" || ch === '"') {
      quote = ch;
      atWordStart = false;
    } else if (ch === "#" && atWordStart) {
      const lf = command.indexOf("\n", i);
      if (lf === -1) return joined + command.slice(i);
      joined += command.slice(i, lf);
      i = lf - 1; // Leave the comment's newline intact.
      continue;
    } else {
      atWordStart = " \t\n;|&()<>".includes(ch);
    }
    joined += ch;
  }
  return joined;
}

/** The word view bash builds: one escaping level dropped, quote characters removed. */
function shellWordView(command) {
  return command.replace(/\\(.)/g, "$1").replace(/['"]/g, "");
}

/**
 * Split on control operators and LF, leaving other line terminators in the
 * text. The separators are read outside quotes: an argument that legitimately
 * carries `;`, `|`, `&` or a newline stays in its own command's segment, so
 * the rest of that command does not read as a segment without the verb
 * (WI#856). A de-escaped view passes `escapes: false`, because its remaining
 * backslashes are literal and must not hide a separator.
 */
function commandSegments(command, escapes = true) {
  const split = (piece, separator) => splitOutsideQuotes(piece, separator, escapes);
  return split(command, ";")
    .flatMap((piece) => split(piece, "\n"))
    .flatMap((piece) => split(piece, "|"))
    .flatMap((piece) => split(piece, "&"));
}

function isRmLongDeleteOption(word) {
  // GNU rm accepts any unique prefix of these options. An adjacent redirect
  // ends the option word; the bare -- marker is not an option prefix.
  const option = /^--[a-z]+(?=$|[<>])/.exec(word)?.[0];
  return option !== undefined && ("--recursive".startsWith(option) || "--force".startsWith(option));
}

/** True when an `rm` argument text carries a recursive or forced flag. */
function rmDeleteOption(rest) {
  const words = rest.split(/[ \t]+/);
  const endOfOptions = words.indexOf("--");
  const optionWords = endOfOptions === -1 ? words : words.slice(0, endOfOptions);
  // Keep the conservative flag test as well as GNU abbreviations.
  return /(^|[ \t])-[a-zA-Z]*[rRfF]|--recursive|--force/.test(rest) ||
    optionWords.some(isRmLongDeleteOption);
}

function checkRm(segment) {
  const match = /\brm[ \t]+(.*)$/s.exec(segment);
  if (!match) return null;
  // `xargs rm` takes its operands from stdin: the guard classifies that shape
  // by the pipeline's find paths (checkXargsDeletion), WI#826.
  const fromXargs = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?xargs\b[^;&|]*\brm[ \t]/.test(segment);
  const rest = match[1];
  // Bash splits words on space and tab here, not JS's broader whitespace set.
  const words = rest.split(/[ \t]+/);
  const endOfOptions = words.indexOf("--");
  if (!rmDeleteOption(rest)) return null;
  const operands = rmOperands(words, endOfOptions)
    // `xargs -I {}` fills the placeholder from its input; the feeding find
    // paths are what checkXargsDeletion classifies (WI#826).
    .filter((operand) => !fromXargs || operand !== "{}");
  if (operands.length === 0) {
    return fromXargs ? null : "rm -r/-f without an explicit operand";
  }
  if (!operands.every(isSafeTempPath)) {
    return outsideTempReason("rm", operands);
  }
  return null;
}

const FIND_CALL = /(?:^|[\s;&|()])find\b/g;
// Deletion actions inside a find expression: `-delete`, and `-exec`/`-ok`
// running a deleting utility (WI#826, WI#832: unlink).
const FIND_DELETE_ACTION =
  /(?:^|[\s(])(?:-delete|-exec(?:dir)?\s+(?:[\w.+-]*\/)?(?:rm|unlink|rmdir|shred)\b|-ok(?:dir)?\s+(?:[\w.+-]*\/)?(?:rm|unlink|rmdir|shred)\b)/;
const XARGS_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?xargs\b/g;
const XARGS_RM = /(?:^|\s)(?:[\w.+-]*\/)?rm(?:\s|$)/;
const RSYNC_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?rsync\b/g;
// Interpreter options may precede the eval flag (`perl -MFile::Path -e …`),
// so the eval flag is found by walking the words (oneLinerCode).
const ONE_LINER_CALL =
  /(?:^|[\s;&|()])(?:[\w.+-]*\/)?(python3?|perl|node|ruby)(?:\.\d+)?\b([^\n]*)/g;
const DELETION_CALL =
  /\b(?:shutil\.rmtree|os\.(?:remove|unlink|rmdir|removedirs)|fs\.(?:rmSync|rmdirSync|unlinkSync|rm|rmdir|unlink)|rmtree|remove_tree|rimraf|rmSync|rmdirSync|unlinkSync|unlink|rmdir)\s*\(/g;
// The names a one-liner can bind to a deletion, however the binding is spelled.
const DELETION_SINK_NAMES = new Set([
  "rmtree", "remove_tree", "rimraf", "rmSync", "rmdirSync", "unlinkSync",
  "removedirs", "rmdir", "unlink", "remove", "rm",
]);
const GETATTR_SINK = /\bgetattr\s*\(\s*([\w.]+)\s*,\s*(['"])([\w$]+)\2\s*\)/g;
const FROM_IMPORT = /\bfrom\s+([\w.]+)\s+import\s+([^;\n]+)/g;
const MODULE_IMPORT_ALIAS = /\bimport\s+([\w.]+)\s+as\s+([\w$]+)/g;
const ASSIGNED_SINK =
  /\b([A-Za-z_$][\w$]*)\s*=\s*((?:[\w$]+\.)*)(rmtree|remove_tree|rimraf|rmdir|unlink|remove|removedirs|rmSync|rmdirSync|unlinkSync)\b(?!\s*\()/g;
// Perl calls deletion with or without parentheses: `rmtree "/x"`,
// `rmtree("/x")`, `unlink glob "/x/*"`. The call head is matched alone and
// its operands are read by perlDeletionCall, so a comma list is checked in
// full rather than only its first operand (WI#840).
const PERL_DELETE_CALL = /\b(?:unlink|rmdir|rmtree|remove_tree)\b/g;
const DEVICE_TARGET = /^\/dev\/(?:sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)/;
const TEE_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?tee\b/g;
const CP_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?cp\b/g;

/** The start paths of a find invocation: the words before its expression. */
function findStartPaths(rest) {
  const paths = [];
  for (const word of argumentWords(rest)) {
    const path = stripShellQuotes(word);
    if (path.startsWith("-") || path === "!" || path === "(") break;
    paths.push(path);
  }
  // find defaults to the current directory when no path is named.
  return paths.length > 0 ? paths : ["."];
}

/** Deny `find -delete`, `-exec rm` and `-ok rm` whose start paths leave the temp roots. */
function checkFindDeletion(text) {
  for (const match of text.matchAll(FIND_CALL)) {
    const rest = clauseText(text, match.index + match[0].length);
    if (!FIND_DELETE_ACTION.test(rest)) continue;
    const paths = findStartPaths(rest);
    if (!paths.every(isSafeTempPath)) return outsideTempReason("find", paths);
  }
  return null;
}

/**
 * The pipeline list that feeds an `xargs` call (pipes stay in the list). The
 * list boundaries are read outside quotes, so a quoted `;` or newline in an
 * earlier command does not move the start of the pipeline past its feeding
 * `find` (WI#856).
 */
function feedingPipeline(text, index) {
  const lists = splitOutsideQuotes(text.slice(0, index), ";")
    .flatMap((piece) => splitOutsideQuotes(piece, "\n"))
    .flatMap((piece) => piece.split(/&&|\|\|/));
  return lists[lists.length - 1] ?? "";
}

/** The start paths of the last find in a pipeline, or null when there is none. */
function lastFindPaths(pipeline) {
  let paths = null;
  for (const match of pipeline.matchAll(FIND_CALL)) {
    paths = findStartPaths(clauseText(pipeline, match.index + match[0].length));
  }
  return paths;
}

/** Deny `xargs rm` unless its feeding find is provably temp-rooted. */
function checkXargsDeletion(text) {
  for (const match of text.matchAll(XARGS_CALL)) {
    const rest = clauseText(text, match.index + match[0].length);
    if (!XARGS_RM.test(rest)) continue;
    const paths = lastFindPaths(feedingPipeline(text, match.index));
    // Without a find in the pipeline the deleted set is unknown; fail closed.
    if (paths === null) return "xargs rm over an input set the guard cannot verify";
    if (!paths.every(isSafeTempPath)) return outsideTempReason("xargs rm", paths);
  }
  return null;
}

/**
 * Deny `rsync --delete`, which deletes whatever the destination has and the
 * source lacks, and `--remove-source-files` (WI#832). The destination is the
 * last operand; `--remove-source-files` deletes the operands before it.
 */
function checkRsyncDeletion(text) {
  for (const match of text.matchAll(RSYNC_CALL)) {
    const words = argumentWords(clauseText(text, match.index + match[0].length)).map(stripShellQuotes);
    const option = words.find((word) => /^--del(?:ete(?:-[a-z-]+)?)?$/.test(word) ||
      word === "--remove-source-files");
    if (option === undefined) continue;
    const operands = words.filter((word) => !word.startsWith("-"));
    const deleted = option === "--remove-source-files" ? operands.slice(0, -1) : operands.slice(-1);
    if (deleted.length === 0) return `rsync ${option} without a verifiable target`;
    if (!deleted.every(isSafeTempPath)) return outsideTempReason(`rsync ${option}`, deleted);
  }
  return null;
}

const MV_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)*mv\b/g;
const TAR_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)*tar\b/g;
// tar's argument-taking short options. Every other letter is a flag, so an
// old-style first word (`tar cf archive …`) is read letter by letter.
const TAR_SHORT_ARG = new Set("bCfFgHIKLNTVX");
// tar's long options whose value is the rest of the word or the next word.
// `--atime-preserve` and the other optional-argument options are absent on
// purpose: they never take a separate word, and eating one could hide a
// member from the check below.
const TAR_LONG_ARG = [
  "file", "directory", "files-from", "exclude", "exclude-from", "transform",
  "xform", "owner", "group", "mode", "mtime", "format", "blocking-factor",
  "record-size", "tape-length", "label", "newer", "after-date",
  "listed-incremental", "use-compress-program", "rmt-command", "rsh-command",
  "index-file", "checkpoint-action", "info-script", "new-volume-script",
  "volno-file", "suffix", "strip-components", "to-command", "quoting-style",
  "sort", "pax-option", "sparse-version", "level", "hole-detection",
];

/** An option value: the attached rest of the word, otherwise the next word. */
function optionValue(attached, words, index) {
  if (attached.length > 0) return { value: attached, last: index };
  return { value: words[index + 1] ?? "", last: index + 1 };
}

/**
 * The path operands of an `mv` invocation: every source and the destination,
 * including a `-t`/`--target-directory` value, which is a destination without
 * being an operand word. `-S`/`--suffix` takes a value that is not a path
 * (WI#849).
 */
function mvOperands(rest) {
  const words = argumentWords(rest).map(stripShellQuotes);
  const operands = [];
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (word === "--") {
      operands.push(...words.slice(i + 1));
      break;
    }
    if (word.startsWith("--")) {
      const equals = word.indexOf("=");
      const name = word.slice(2, equals === -1 ? undefined : equals);
      const attached = equals === -1 ? null : word.slice(equals + 1);
      // GNU mv accepts unique prefixes: `--t` is `--target-directory`.
      const target = name.length >= 1 && "target-directory".startsWith(name);
      const suffix = name.length >= 2 && "suffix".startsWith(name);
      if (!target && !suffix) continue;
      const { value, last } = optionValue(attached ?? "", words, i);
      if (target) operands.push(value);
      i = last;
      continue;
    }
    if (word.startsWith("-") && word.length > 1) {
      const cluster = word.slice(1);
      for (let j = 0; j < cluster.length; j++) {
        if (cluster[j] !== "t" && cluster[j] !== "S") continue;
        const { value, last } = optionValue(cluster.slice(j + 1), words, i);
        if (cluster[j] === "t") operands.push(value);
        i = last;
        break;
      }
      continue;
    }
    operands.push(word);
  }
  // A redirection target is not an operand (e.g. `2>/tmp/log`).
  return operands.filter((operand) => !/^\d*>/.test(operand));
}

/** The argument-taking long option a name spells, exactly or by unique prefix. */
function tarLongValueOption(name) {
  if (TAR_LONG_ARG.includes(name)) return name;
  if (name.length < 4) return null;
  return TAR_LONG_ARG.find((option) => option.startsWith(name)) ?? null;
}

/** Read one tar short-option cluster; returns the index of its last consumed word. */
function tarCluster(cluster, words, index, state) {
  for (let j = 0; j < cluster.length; j++) {
    if (!TAR_SHORT_ARG.has(cluster[j])) continue;
    const { last } = optionValue(cluster.slice(j + 1), words, index);
    if (cluster[j] === "T") state.unverifiable = true;
    // An argument ends the cluster: the rest of the word is its value, or the
    // next word when nothing is attached.
    return last;
  }
  return index;
}

/** Read one tar long option; returns the index of its last consumed word. */
function tarLongOption(word, words, index, state) {
  const equals = word.indexOf("=");
  const name = word.slice(2, equals === -1 ? undefined : equals);
  const attached = equals === -1 ? null : word.slice(equals + 1);
  if (attached === null && "remove-files".startsWith(name)) {
    state.removeFiles = true;
    return index;
  }
  const valueOption = tarLongValueOption(name);
  if (valueOption === "files-from") state.unverifiable = true;
  return valueOption === null ? index : optionValue(attached ?? "", words, index).last;
}

/**
 * Read a tar invocation for `--remove-files` (WI#849): the member words tar
 * removes after archiving, and whether a `-T`/`--files-from` list replaces
 * them with contents the guard cannot see. Only literal member paths matter:
 * a relative member is refused by the shared containment test anyway, so a
 * `-C` base cannot point the removal at a protected tree.
 */
function tarInvocation(rest) {
  const words = argumentWords(rest).map(stripShellQuotes);
  const state = { removeFiles: false, members: [], unverifiable: false };
  let i = 0;
  // Old-style options (`tar cf archive …`) carry the cluster without a dash.
  if (words.length > 0 && /^[A-Za-z]+$/.test(words[0]) && /[cxtruA]/.test(words[0])) {
    i = tarCluster(words[0], words, 0, state) + 1;
  }
  for (; i < words.length; i++) {
    const word = words[i];
    if (word === "--") {
      state.members.push(...words.slice(i + 1));
      break;
    }
    if (word.startsWith("--")) {
      i = tarLongOption(word, words, i, state);
      continue;
    }
    if (word.startsWith("-") && word.length > 1) {
      i = tarCluster(word.slice(1), words, i, state);
      continue;
    }
    state.members.push(word);
  }
  state.members = state.members.filter((member) => !/^\d*>/.test(member));
  return state;
}

/**
 * Deny the two shapes that remove a tree without a deleting verb (WI#849): an
 * `mv` whose source or destination leaves the temp roots — a rename moves the
 * tree out of the protected namespace, and a later `rm -rf` of the new name is
 * then trivially inside them — and `tar --remove-files`, whose members are
 * deleted after archiving. Both take the shared temp-root test `rm` uses.
 */
// The guard's own trees: the CLI's config home and the image's code and
// config (WI#849). A rename that moves one of them, a directory above one, or
// a system root out of place is the launder; any other rename is ordinary
// work, so a workspace `mv a b` or `git mv` passes.
const MV_PROTECTED_ROOTS = ["/home/kimi/.kimi-code", "/app"];

function mvProtectedRoots() {
  const home = process.env.KIMI_CODE_HOME;
  if (!home || !home.startsWith("/")) return MV_PROTECTED_ROOTS;
  return [...MV_PROTECTED_ROOTS, home.replace(/(.)\/+$/, "$1")];
}

/** A glob path component as a regular expression over the names it can match. */
function globComponentRegex(component) {
  let pattern = "^";
  for (let i = 0; i < component.length; i++) {
    const ch = component[i];
    const close = ch === "[" ? component.indexOf("]", i + 1) : ch === "{" ? component.indexOf("}", i + 1) : -1;
    if (ch === "*") pattern += ".*";
    else if (ch === "?") pattern += ".";
    else if (close !== -1) {
      // A bracket class or a brace list: either way, any name can follow.
      pattern += ".*";
      i = close;
    } else pattern += ch.replace(/[.+^$()|\\[\]{}]/g, "\\$&");
  }
  return new RegExp(`${pattern}$`);
}

const mvOperandVerdicts = new Map();

/**
 * True when an `mv` operand names, contains or sits inside a protected tree,
 * or is a system root itself. `~`/`$HOME` are expanded and a relative operand
 * is read from the hook's working directory. An operand holding a variable, a
 * command substitution, `~user` or a find/xargs `{}` placeholder can stand
 * for any path, so it is refused. A `.kimi-code` component, or a dot-glob
 * that can match it, counts wherever it is; any other glob stands for any
 * name in its literal directory. Verdicts are cached for the readings of one
 * command.
 */
function mvTouchesProtected(operand) {
  if (!mvOperandVerdicts.has(operand)) mvOperandVerdicts.set(operand, mvOperandProtected(operand));
  return mvOperandVerdicts.get(operand);
}

function mvOperandProtected(operand) {
  const home = (process.env.HOME || "/home/kimi").replace(/(.)\/+$/, "$1");
  const text = operand.replace(/^(?:~|\$\{HOME\}|\$HOME)(?=\/|$)/, home);
  if (/[$`]/.test(text) || text.startsWith("~") || text.includes("{}")) return true;
  if (/(?:^|\/)\.kimi-code(?:\/|$)/.test(text)) return true;
  const roots = mvProtectedRoots();
  const glob = [...text].findIndex((ch) => "*?[]{}".includes(ch));
  if (glob !== -1) {
    const dotGlob = text.split("/").some((part) => part.startsWith(".") &&
      /[*?[\]{}]/.test(part) && globComponentRegex(part).test(".kimi-code"));
    if (dotGlob) return true;
    const prefix = text.slice(0, glob);
    const directory = prefix.endsWith("/") ? resolvePath(prefix) : dirname(resolvePath(`${prefix}x`));
    return roots.some((root) => directory === "/" || root === directory ||
      root.startsWith(`${directory}/`) || directory.startsWith(`${root}/`));
  }
  const resolved = resolveExistingPrefix(resolvePath(text));
  if (resolved === null) return true;
  if (resolved === "/" || SYSTEM_ROOTS.includes(resolved)) return true;
  return roots.some((root) => resolved === root || resolved.startsWith(`${root}/`) ||
    root.startsWith(`${resolved}/`));
}

function checkMoveAndTarDeletion(text) {
  for (const match of text.matchAll(MV_CALL)) {
    const hit = mvOperands(clauseText(text, match.index + match[0].length)).find(mvTouchesProtected);
    if (hit !== undefined) return `rename of a protected tree or a system root: mv ${hit}`;
  }
  for (const match of text.matchAll(TAR_CALL)) {
    const tar = tarInvocation(clauseText(text, match.index + match[0].length));
    if (!tar.removeFiles) continue;
    if (tar.unverifiable) return "tar --remove-files over a member list the guard cannot verify";
    if (tar.members.length === 0) return "tar --remove-files without an explicit member";
    if (!tar.members.every(isSafeTempPath)) return outsideTempReason("tar --remove-files", tar.members);
  }
  return null;
}

/** Read a quoted first argument starting after a call's `(`; null if it is not a literal. */
function quotedFirstArgument(code, from) {
  let i = from;
  // A nested call (`getattr(m, 'rmtree')(…)`) leaves its own `(` here.
  while (i < code.length && /[ \t(]/.test(code[i])) i++;
  const quote = code[i];
  if (quote !== "'" && quote !== '"' && quote !== "`") return null;
  let value = "";
  for (i++; i < code.length; i++) {
    if (code[i] === "\\" && i + 1 < code.length) {
      value += code[++i];
      continue;
    }
    if (code[i] === quote) return value;
    value += code[i];
  }
  return null;
}

/**
 * The code text of an interpreter invocation, or null when the words after the
 * interpreter carry no eval flag. The eval flag may follow other options
 * (`perl -MFile::Path -e …`, `node --no-warnings -e …`), so the words are
 * walked rather than matched by one regex (WI#832).
 */
function oneLinerCode(rest) {
  let i = 0;
  while (i < rest.length) {
    while (i < rest.length && /[ \t]/.test(rest[i])) i++;
    const start = i;
    while (i < rest.length && !/[ \t]/.test(rest[i])) i++;
    const word = rest.slice(start, i);
    if (word.length === 0) return null;
    if (/^(?:--eval|--print)$/.test(word) || /^-[a-zA-Z]*[ceEp][a-zA-Z]*$/.test(word)) {
      return rest.slice(i);
    }
    if (!word.startsWith("-")) return null;
  }
  return null;
}

/**
 * The names a one-liner binds to a deletion (WI#832): `from shutil import
 * rmtree as r`, `import shutil as s`, and `r = shutil.rmtree`. Each binding
 * maps a local name to a dotted module path.
 */
function sinkAliases(code) {
  const aliases = new Map();
  for (const match of code.matchAll(FROM_IMPORT)) {
    const module = match[1];
    for (const piece of match[2].split(",")) {
      const [imported, local] = piece.trim().split(/\s+as\s+/);
      const name = imported.trim();
      if (!DELETION_SINK_NAMES.has(name)) continue;
      aliases.set((local ?? name).trim(), `${module}.${name}`);
    }
  }
  for (const match of code.matchAll(MODULE_IMPORT_ALIAS)) aliases.set(match[2], match[1]);
  for (const match of code.matchAll(ASSIGNED_SINK)) {
    const target = DELETION_SINK_NAMES.has(match[3]) ? `${match[2]}${match[3]}` : match[3];
    aliases.set(match[1], target);
  }
  return aliases;
}

/** The call name in an alias's dotted path, or null when the path is not a deletion. */
function aliasSinkName(dotted, aliases) {
  const head = dotted.split(".")[0];
  const bound = aliases.get(head);
  const resolved = bound === undefined ? dotted : bound + dotted.slice(head.length);
  const name = resolved.split(".").pop();
  return DELETION_SINK_NAMES.has(name) ? name : null;
}

/** Every position in a one-liner where a deletion call's argument list starts. */
function deletionCallOffsets(code, aliases) {
  const offsets = [];
  for (const call of code.matchAll(DELETION_CALL)) offsets.push(call.index + call[0].length);
  for (const match of code.matchAll(GETATTR_SINK)) {
    if (aliasSinkName(`${match[1]}.${match[3]}`, aliases)) offsets.push(match.index + match[0].length);
  }
  for (const name of aliases.keys()) {
    if (aliasSinkName(name, aliases) === null) continue;
    const escaped = name.replace(/[$]/g, "\\$");
    for (const call of code.matchAll(new RegExp(`\\b${escaped}\\s*\\(`, "g"))) {
      offsets.push(call.index + call[0].length);
    }
  }
  return offsets;
}

/**
 * Strip one layer of shell quoting that wraps the whole one-liner body, so the
 * interpreter's own string literals are the only quotes the readers see:
 * `'print "unlink"'` becomes `print "unlink"` (WI#832). A double-quoted shell
 * word also passes `\"`, `\\`, `\$` and `` \` `` to the interpreter without
 * the backslash, so those escapes are removed too; the readers then see the
 * code the interpreter runs, not the shell's spelling of it (WI#840).
 */
function stripOuterShellQuotes(code) {
  const text = code.trim();
  if (text.length < 2) return text;
  const quote = text[0];
  if (quote !== "'" && quote !== '"') return text;
  let end = -1;
  for (let i = 1; i < text.length; i++) {
    if (quote === '"' && text[i] === "\\") {
      i++;
      continue;
    }
    if (text[i] === quote) end = i;
  }
  if (end !== text.length - 1) return text;
  const inner = text.slice(1, -1);
  return quote === '"' ? inner.replace(/\\(["\\$`])/g, "$1") : inner;
}

/** True when `index` in the code sits inside a single- or double-quoted string. */
function insideQuotedString(code, index) {
  let quote = "";
  for (let i = 0; i < index; i++) {
    const ch = code[i];
    if (ch === "\\" && quote !== "'") {
      i++;
      continue;
    }
    if (quote) {
      if (ch === quote) quote = "";
    } else if (ch === "'" || ch === '"' || ch === "`") {
      quote = ch;
    }
  }
  return quote !== "";
}

function skipBlanks(code, i) {
  while (i < code.length && (code[i] === " " || code[i] === "\t")) i++;
  return i;
}

/** The value of the Perl string literal that opens at `from` and the index after it. */
function perlStringLiteral(code, from) {
  const quote = code[from];
  if (quote !== "'" && quote !== '"') return null;
  let value = "";
  for (let i = from + 1; i < code.length; i++) {
    if (code[i] === "\\" && i + 1 < code.length) {
      value += code[++i];
      continue;
    }
    if (code[i] === quote) return { value, next: i + 1 };
    value += code[i];
  }
  return null;
}

// Perl's paired delimiters; a delimiter without a pair (`q/…/`) closes on
// itself. Quote-like bodies and File::Path option hashes are skipped with it.
const PERL_DELIMITERS = { "(": ")", "[": "]", "{": "}", "<": ">" };

/**
 * The index after the Perl delimiter pair that opens at `open`, or -1 when it
 * never closes. A backslash escapes the next character and bracket pairs nest.
 */
function perlDelimitedEnd(code, open) {
  const openCh = code[open];
  const closeCh = PERL_DELIMITERS[openCh] ?? openCh;
  const paired = openCh !== closeCh;
  let depth = 0;
  for (let i = paired ? open : open + 1; i < code.length; i++) {
    if (code[i] === "\\") {
      i++;
      continue;
    }
    if (paired && code[i] === openCh) depth++;
    else if (code[i] === closeCh && (!paired || --depth === 0)) return i + 1;
  }
  return -1;
}

/**
 * The index after the `}` that closes the File::Path option hash at `i`, or
 * -1. Quotes are skipped so a brace inside an option value does not close it.
 */
function perlHashEnd(code, i) {
  let depth = 0;
  let quote = "";
  for (let j = i; j < code.length; j++) {
    const ch = code[j];
    if (quote) {
      if (ch === "\\") j++;
      else if (ch === quote) quote = "";
      continue;
    }
    if (ch === '"' || ch === "'") quote = ch;
    else if (ch === "{") depth++;
    else if (ch === "}" && --depth === 0) return j + 1;
  }
  return -1;
}

// Quote-like string operators whose body is text, not code: a deletion keyword
// spelled inside one (`qw(unlink)`) is not a call. `q` must not follow a sigil
// (so `$q = …` is not read as a literal) and takes a bracketed delimiter
// (WI#851).
const PERL_QUOTE_LIKE = /(?<![\w$@%&\\])(?:qw|qq|qx|q)\s*[([{]/g;

/** True when `index` sits inside a Perl quote-like literal (`qw(…)`, `q{…}`). */
function insidePerlQuoteLike(code, index) {
  for (const match of code.matchAll(PERL_QUOTE_LIKE)) {
    const open = match.index + match[0].length - 1;
    if (open >= index) return false;
    const end = perlDelimitedEnd(code, open);
    if (end !== -1 && index < end) return true;
  }
  return false;
}

/** A `qw(…)` list of literal words at `i`, or null when there is none. */
function perlQuoteWordList(code, i) {
  if (!code.startsWith("qw", i) || /\w/.test(code[i + 2] ?? "")) return null;
  const open = skipBlanks(code, i + 2);
  if (PERL_DELIMITERS[code[open]] === undefined && code[open] !== "/") return null;
  const end = perlDelimitedEnd(code, open);
  if (end === -1) return { unverifiable: true };
  const values = code.slice(open + 1, end - 1).split(/[ \t\n]+/).filter((word) => word.length > 0);
  return { next: end, values };
}

/** The pattern a `glob(…)` operand names: a quoted literal or its bare text. */
function perlGlobTarget(text) {
  const literal = perlStringLiteral(text, 0);
  return literal !== null && literal.next === text.length ? literal.value : text;
}

/** A Perl `glob(…)` or `glob …` operand at `i`, or null when there is none. */
function perlGlobOperand(code, i) {
  if (!code.startsWith("glob", i) || /\w/.test(code[i + 4] ?? "")) return null;
  const open = skipBlanks(code, i + 4);
  if (code[open] === "(") {
    const end = perlDelimitedEnd(code, open);
    if (end === -1) return { unverifiable: true };
    return { next: end, values: [perlGlobTarget(code.slice(open + 1, end - 1).trim())] };
  }
  const literal = perlStringLiteral(code, open);
  if (literal === null) return { unverifiable: true };
  return { next: literal.next, values: [literal.value] };
}

/** A `<…>` glob operand at `i`. */
function perlAngleOperand(code, i) {
  const end = code.indexOf(">", i + 1);
  if (end === -1) return { unverifiable: true };
  const value = code.slice(i + 1, end).trim();
  return value.length === 0 ? { unverifiable: true } : { next: end + 1, values: [value] };
}

/**
 * One element of a Perl deletion call's operand list: a path target, an
 * option that is not a target, or an expression the guard cannot verify. The
 * File::Path option hash of `rmtree`/`remove_tree` and `rmtree`'s legacy
 * numeric verbose/safe flags are the only options (WI#851); `remove_tree` has
 * no legacy flags, so a bare number there is a path (`remove_tree "x", 0`
 * removes `./0`).
 */
function perlDeletionElement(code, i, options, numericFlags) {
  const ch = code[i] ?? "";
  if (options && ch === "{") {
    const end = perlHashEnd(code, i);
    return end === -1 ? { unverifiable: true } : { next: end, skipped: true, hash: true };
  }
  if (numericFlags && /[0-9]/.test(ch)) {
    let end = i;
    while (end < code.length && /[0-9]/.test(code[end])) end++;
    return { next: end, skipped: true, number: true };
  }
  if (ch === "<") return perlAngleOperand(code, i);
  const glob = perlGlobOperand(code, i);
  if (glob !== null) return glob;
  const wordList = perlQuoteWordList(code, i);
  if (wordList !== null) return wordList;
  const literal = perlStringLiteral(code, i);
  if (literal !== null) return { next: literal.next, values: [literal.value] };
  return { unverifiable: true };
}

/**
 * True when `i` ends a list operator's arguments: `;`, a closing block, a
 * comment, the end of the code, the low-precedence `or`/`and` a guarded
 * deletion ends with (`unlink "/tmp/x" or die`), or a statement modifier
 * (`unlink "/tmp/x" if -e "/tmp/x"`). `&&`, `||` and `//` bind tighter than a
 * list operator, so they continue the operand: `unlink "/tmp/x" && "/w/f"`
 * deletes `/w/f` and must read as unverifiable.
 */
function perlStatementEnd(code, i) {
  if (i >= code.length) return true;
  if (";})\n#".includes(code[i])) return true;
  return /^(?:or|and|if|unless|while|until|for|foreach)(?![\w])/.test(code.slice(i));
}

/**
 * The operands of a Perl deletion call whose keyword `keyword` ends at `from`,
 * and whether any part of its operand list is not a single string literal. The
 * call may be spelled with or without parentheses and may take a comma list;
 * every operand is read, because checking only the first leaves the rest
 * unverified (WI#840). `glob(…)`, `<…>`, `qw(…)` and the implicit-`$_` form
 * are call shapes too (WI#851). Null means the keyword is not a call at all
 * (a `=>` hash key).
 */
function perlDeletionCall(code, from, keyword) {
  let i = skipBlanks(code, from);
  if (code.startsWith("=>", i)) return null;
  const parenthesized = code[i] === "(";
  if (parenthesized) i++;
  const options = keyword === "rmtree" || keyword === "remove_tree";
  const callEnd = (at) => (parenthesized ? code[at] === ")" : perlStatementEnd(code, at));
  const operands = [];
  // File::Path reads `rmtree`'s numbers as the legacy verbose/safe flags only
  // in the second and third place of an old-style call (no option hash); in
  // any other place, or next to a hash, a number is a path (`./2024`).
  let index = 0;
  let hash = false;
  let number = false;
  for (;;) {
    i = skipBlanks(code, i);
    // No target at all is the implicit `$_` form, which cannot be verified.
    if (callEnd(i)) return { operands, unverifiable: true };
    const numericFlags = keyword === "rmtree" && (index === 1 || index === 2);
    const element = perlDeletionElement(code, i, options, numericFlags);
    if (element.unverifiable) return { operands, unverifiable: true };
    if (!element.skipped) operands.push(...element.values);
    hash ||= element.hash === true;
    number ||= element.number === true;
    index++;
    i = skipBlanks(code, element.next);
    if (code[i] === ",") {
      i++;
      continue;
    }
    return { operands, unverifiable: !callEnd(i) || operands.length === 0 || (hash && number) };
  }
}

/**
 * The Perl deletion verdict: `rmtree "dir"`, `unlink glob "dir/*"`,
 * `rmtree("a", "b")`, and a target the guard cannot resolve (WI#832). Every
 * operand of the comma list is read, and the shell escapes of a quoted
 * one-liner are removed before the calls are read (WI#840). `glob(…)`,
 * `<…>`, `qw(…)` and the implicit-`$_` forms are calls too, and the File::Path
 * options of `rmtree`/`remove_tree` are not targets (WI#851). A call name
 * inside a string literal (`print "unlink"`) or a quote-like literal
 * (`qw(unlink)`) is text, not a call.
 */
function perlDeletionVerdict(rawCode) {
  const code = stripOuterShellQuotes(rawCode);
  for (const match of code.matchAll(PERL_DELETE_CALL)) {
    if (insideQuotedString(code, match.index) || insidePerlQuoteLike(code, match.index)) continue;
    const call = perlDeletionCall(code, match.index + match[0].length, match[0]);
    if (call === null) continue;
    if (call.unverifiable) return "interpreter deletion whose target the guard cannot verify";
    for (const operand of call.operands) {
      if (!isSafeTempPath(operand)) return outsideTempReason("interpreter deletion", [operand]);
    }
  }
  return null;
}

/**
 * Deny interpreter deletion in a one-liner whose target cannot be verified, and
 * outside the temp roots otherwise (WI#826, WI#832). The deletion is read from
 * the call shape, from an `getattr(module, 'name')` sink, from a name an
 * import or assignment bound to a deletion, and from the parenthesis-free Perl
 * form.
 */
function checkInterpreterDeletion(text) {
  for (const invocation of text.matchAll(ONE_LINER_CALL)) {
    const code = oneLinerCode(invocation[2]);
    if (code === null) continue;
    const aliases = sinkAliases(code);
    if (invocation[1] === "perl") {
      const perlDenial = perlDeletionVerdict(code);
      if (perlDenial) return perlDenial;
    }
    for (const offset of deletionCallOffsets(code, aliases)) {
      const target = quotedFirstArgument(code, offset);
      if (target === null) return "interpreter deletion whose target the guard cannot verify";
      if (!isSafeTempPath(target)) return outsideTempReason("interpreter deletion", [target]);
    }
  }
  return null;
}

function deviceTarget(word) {
  const operand = stripShellQuotes(word);
  return DEVICE_TARGET.test(operand) ? operand : null;
}

/** Deny block-device writes by `tee` (every operand) and `cp` (its last operand). */
function checkDeviceWrites(text) {
  for (const match of text.matchAll(TEE_CALL)) {
    for (const word of argumentWords(clauseText(text, match.index + match[0].length))) {
      if (word.startsWith("-")) continue;
      const target = deviceTarget(word);
      if (target !== null) return `direct block-device write: tee ${target.slice(0, 80)}`;
    }
  }
  for (const match of text.matchAll(CP_CALL)) {
    const operands = argumentWords(clauseText(text, match.index + match[0].length))
      .filter((word) => !word.startsWith("-"));
    const target = operands.length > 0 ? deviceTarget(operands[operands.length - 1]) : null;
    if (target !== null) return `direct block-device write: cp ${target.slice(0, 80)}`;
  }
  return null;
}

/** Deletion, decode and device checks that need the command text with its quoting intact. */
function checkRawShapes(text) {
  return checkFindDeletion(text) ??
    checkXargsDeletion(text) ??
    checkRsyncDeletion(text) ??
    checkMoveAndTarDeletion(text) ??
    checkInterpreterDeletion(text) ??
    checkDeviceWrites(text) ??
    checkDecodedPipe(text);
}

const SYSTEM_ROOTS = ["/etc", "/usr", "/var", "/home", "/root", "/boot", "/bin", "/sbin", "/lib", "/lib64", "/opt", "/srv"];
const GIT_CALL = /\bgit\b/g;
const ROOT_CHANGE_CALL = /\bch(?:mod|own|grp)\b/g;
const STAGE_PREFIXES = ["sudo", "env", "exec", "busybox"];
const SHELL_NAME = /^(?:ba|z|da|a|k)?sh$/;

/**
 * The word view bash builds around shell punctuation (WI#853): an unquoted
 * `(`, `)` or backtick ends the word around it, because a subshell, a `$(…)`
 * or a backtick body runs a command of its own. A substitution and a backtick
 * still run inside double quotes; text in single quotes is literal. Each
 * syntax character becomes a space, so an option or path rule sees the end of
 * its word (`--hard)` reads as `--hard `). A backslash-escaped character is
 * kept as text. This is a reading, not a parser: it only adds matches.
 */
function shellPunctuationView(text) {
  if (!text.includes("(") && !text.includes(")") && !text.includes("`")) return text;
  let view = "";
  let quote = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\" && quote !== "'" && i + 1 < text.length) {
      view += ch + text[++i];
      continue;
    }
    if (quote === "'") {
      view += ch;
      if (ch === "'") quote = "";
    } else if (ch === "'") {
      quote = "'";
      view += ch;
    } else if (ch === '"') {
      quote = quote === '"' ? "" : '"';
      view += ch;
    } else if (ch === "`") {
      view += " ";
    } else if (ch === "$" && text[i + 1] === "(") {
      view += "  ";
      i++;
    } else if (ch === "(" || ch === ")") {
      view += " ";
    } else {
      view += ch;
    }
  }
  return view;
}

/** True for the filesystem root or a path at or under a listed system root. */
function isSystemRootPath(candidate) {
  if (!candidate.startsWith("/")) return false;
  const path = normalizePath(candidate);
  if (path === "/") return true;
  return SYSTEM_ROOTS.some((root) => path === root || path.startsWith(`${root}/`));
}

// The recursive flag as chmod/chown/chgrp parse it: the short cluster, or the
// GNU long option, which getopt accepts down to its shortest unique prefix
// (`--rec`: `rec` and `ref` differ in the third letter, so only `--re` is
// ambiguous with `--reference`; coreutils 9.12 recurses on `--rec`). An
// attached value is rejected by getopt, but matching it costs nothing and
// keeps the rule fail-closed.
const RECURSIVE_CHANGE_FLAG =
  /(?:^|[ \t])(?:-[a-zA-Z]*R[a-zA-Z]*|--rec[a-z]*)(?=[ \t=]|$)/;

/**
 * Deny a recursive mode/ownership change on a system root, however the root is
 * spelled. A linear scan, not a regex: the regex form rescanned the rest of
 * the command from every `chmod` and took ten seconds on 12 KB of them. The
 * clause is read outside quotes, so a quoted argument cannot end it early
 * (WI#856).
 */
function checkRecursiveRootChange(segment) {
  for (const match of segment.matchAll(ROOT_CHANGE_CALL)) {
    const rest = clauseText(segment, match.index + match[0].length);
    if (!RECURSIVE_CHANGE_FLAG.test(rest)) continue;
    for (const pathMatch of rest.matchAll(/(?:^|[ \t])(\/[^ \t;&|]*)/g)) {
      if (isSystemRootPath(stripShellQuotes(pathMatch[1]))) return true;
    }
  }
  return false;
}

/** True for a character that ends the current command clause. */
function isClauseTerminator(ch) {
  return ch === ";" || ch === "&" || ch === "|" || ch === "\n";
}

/**
 * The text of the command clause that starts at `from`: it runs to the first
 * control operator or newline, read outside quotes and past a backslash. Every
 * reader that captures "the rest of the invocation" uses this, so an argument
 * that carries a separator cannot hide the rest of its command (WI#856).
 */
function clauseText(text, from) {
  let quote = "";
  for (let i = from; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\" && quote !== "'" && i + 1 < text.length) {
      i++;
      continue;
    }
    if (quote !== "") {
      if (ch === quote) quote = "";
      continue;
    }
    if (ch === "'" || ch === '"') quote = ch;
    else if (isClauseTerminator(ch)) return text.slice(from, i);
  }
  return text.slice(from);
}

/**
 * The bash word at `from`, with its quoting removed, and the index after it.
 * A word ends at space, tab or an unquoted clause terminator, so a quoted
 * option value that carries a space or a separator reads as one word
 * (WI#856).
 */
function bashWord(text, from) {
  let i = from;
  let word = "";
  let quote = "";
  while (i < text.length) {
    const ch = text[i];
    if (ch === "\\" && quote !== "'" && i + 1 < text.length) {
      word += text[i + 1];
      i += 2;
      continue;
    }
    if (quote !== "") {
      if (ch === quote) quote = "";
      else word += ch;
      i++;
      continue;
    }
    if (ch === "'" || ch === '"') {
      quote = ch;
      i++;
      continue;
    }
    if (ch === " " || ch === "\t" || isClauseTerminator(ch)) break;
    word += ch;
    i++;
  }
  return { word, end: i };
}

/** The bash words of a command text, with their quoting removed. */
function bashWords(text) {
  const words = [];
  let i = 0;
  while (i < text.length) {
    while (i < text.length && (text[i] === " " || text[i] === "\t")) i++;
    if (i >= text.length) break;
    const { word, end } = bashWord(text, i);
    if (word.length === 0) break;
    words.push(word);
    i = end;
  }
  return words;
}

// git's own options that take the next word as their value (`git -C dir`,
// `git --work-tree dir`). The value is neither the verb nor an option, so a
// scan of the leading words must step over it.
const GIT_VALUE_OPTIONS = new Set([
  "-C", "-c", "--git-dir", "--work-tree", "--namespace", "--super-prefix",
  "--config-env", "--attr-source", "--list-cmds",
]);

/** The git verb after any leading options, and the index after it. */
function gitVerb(text, from) {
  let i = from;
  while (i < text.length) {
    while (i < text.length && (text[i] === " " || text[i] === "\t")) i++;
    const { word, end } = bashWord(text, i);
    i = end;
    if (word.length === 0) break;
    if (!word.startsWith("-")) return { verb: word, after: i };
    if (GIT_VALUE_OPTIONS.has(word)) {
      // These options take a value, which is not the verb. It is read as one
      // bash word, so `-C "/tmp/a b"` steps over the value whole (WI#856).
      while (i < text.length && (text[i] === " " || text[i] === "\t")) i++;
      i = bashWord(text, i).end;
    }
  }
  return { verb: "", after: text.length };
}

/** Index of the next clause terminator at or after `from`, or the text end. */
function clauseEnd(text, from) {
  return from + clauseText(text, from).length;
}

// `GIT_CONFIG_KEY_*` (with `GIT_CONFIG_COUNT`) and `GIT_CONFIG_PARAMETERS` are
// git's environment spellings of `-c`; an alias planted that way is the same
// bypass as `-c alias.*` (WI#842). The value is read as one shell word, so an
// `alias.` later in the same command line is not attributed to this variable.
const GIT_ALIAS_ENV = /(?:^|[\s;&|(])GIT_CONFIG_(?:KEY_\d+|PARAMETERS)=("[^"]*"|'[^']*'|\S+)/g;

// Config keys that move a git command around the verb rules: an alias is an
// arbitrary verb, and an `include`/`includeIf` path pulls in a file that can
// define one. git compares section names without regard to case, so
// `Alias.z` is `alias.z` (WI#842).
const GIT_REDIRECTING_KEY = /^(?:alias|include|includeif)\./i;
// A section rename names the bare section (`--rename-section foo alias`).
const GIT_REDIRECTING_SECTION = /^(?:alias|include|includeif)(?:\.|$)/i;

// A config file named in the environment can define an alias the same way
// an include does.
const GIT_CONFIG_FILE_ENV = /(?:^|[\s;&|(])GIT_CONFIG_(?:GLOBAL|SYSTEM)=/;

/** True when the segment's environment names a redirecting key or config file. */
function hasGitAliasEnv(segment) {
  if (GIT_CONFIG_FILE_ENV.test(segment)) return true;
  for (const match of segment.matchAll(GIT_ALIAS_ENV)) {
    if (/(?:^|[\s'"])(?:alias|include|includeif)\./i.test(match[1])) return true;
  }
  return false;
}

// `git config`'s long options, for resolving the unambiguous prefixes git
// accepts (`--fil` is `--file`, `--ren` is `--rename-section`).
const GIT_CONFIG_LONG_OPTIONS = [
  "global", "system", "local", "worktree", "file", "blob", "get", "get-all",
  "get-regexp", "get-urlmatch", "get-color", "get-colorbool", "replace-all",
  "add", "unset", "unset-all", "rename-section", "remove-section", "list",
  "fixed-value", "edit", "type", "no-type", "null", "name-only", "includes",
  "no-includes", "show-origin", "show-scope", "default", "comment", "value",
  "bool", "int", "bool-or-int", "path", "expiry-date", "all", "regexp", "url",
  "append",
];
const GIT_CONFIG_VALUE_LONG = new Set(["file", "blob", "type", "default", "comment", "value"]);
const GIT_CONFIG_VALUE_SHORT = new Set(["-f", "-t"]);

/** The `git config` long option a `--name` spells, exactly or by unique prefix. */
function gitConfigLongOption(name) {
  if (GIT_CONFIG_LONG_OPTIONS.includes(name)) return name;
  const matches = GIT_CONFIG_LONG_OPTIONS.filter((option) => option.startsWith(name));
  return matches.length === 1 ? matches[0] : null;
}

/**
 * Read a `git config` call: its name operands after options and their
 * values, whether it opens the editor, and whether it renames a section. An
 * edit option counts only before the first operand, where git reads it; a
 * later `-e` is a value (`core.editor 'vim -e'`).
 */
function gitConfigCall(after) {
  const words = argumentWords(after);
  const call = { operands: [], edit: false, rename: false };
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (word.startsWith("--") && word.length > 2) {
      const equals = word.indexOf("=");
      const option = gitConfigLongOption(word.slice(2, equals === -1 ? undefined : equals));
      if (option === "edit" && call.operands.length === 0) call.edit = true;
      if (option === "rename-section") call.rename = true;
      if (equals === -1 && GIT_CONFIG_VALUE_LONG.has(option)) i++;
      continue;
    }
    if (word.startsWith("-") && word.length > 1) {
      if (word === "-e" && call.operands.length === 0) call.edit = true;
      if (GIT_CONFIG_VALUE_SHORT.has(word)) i++;
      continue;
    }
    call.operands.push(stripShellQuotes(word));
  }
  return call;
}

/**
 * True when a `git config` call names a redirecting key (WI#842): the classic
 * `git config alias.z …` form, the modern `set`/`get` subcommands
 * (`git config set alias.z …`), a section rename into or out of one, in the
 * subcommand or the `--rename-section` option form (or its prefixes), or the
 * `edit` subcommand and `--edit`/`-e`, whose editor can write any key. Reads of those keys are refused with the writes; other
 * keys stay ordinary config work.
 */
function gitConfigAliasVerdict(after) {
  const { operands, edit, rename } = gitConfigCall(after);
  if (edit || operands[0] === "edit") return true;
  if (operands[0] === "rename-section") {
    return operands.slice(1, 3).some((name) => GIT_REDIRECTING_SECTION.test(name));
  }
  if (rename) return operands.slice(0, 2).some((name) => GIT_REDIRECTING_SECTION.test(name));
  if (operands[0] === "set" || operands[0] === "get") return GIT_REDIRECTING_KEY.test(operands[1] ?? "");
  return GIT_REDIRECTING_KEY.test(operands[0] ?? "");
}

/**
 * True when a git invocation would define an alias (WI#842): an inline
 * `-c`/`--config-env` alias assignment, or a `git config` call naming an
 * `alias.*` key (the classic form, the `set`/`get` subcommands, or a section
 * rename into `alias`). An alias name is an arbitrary verb, so the discard
 * rules cannot classify what it runs — `git -c alias.z='reset --hard' z` reads as
 * the unknown verb `z`; an include does the same through a file. These
 * refusals cover the spellings a command line shows; an alias already in a
 * config file the task wrote directly, or one `HOME`/`XDG_CONFIG_HOME` points
 * at, stays an accepted residual the guard cannot read cheaply.
 *
 * Only the leading option words and, for `config`, its own clause are read,
 * so the verb-first scan of WI#853 keeps a long command linear (WI#824).
 */
function gitAliasDefinition(segment, from) {
  const { verb, after } = gitVerb(segment, from);
  // The leading words are read as bash reads them, so a quoted `-C`/`-c`
  // value that carries a space stays one value and cannot break the option
  // scan before a later `-c` (WI#856).
  const words = bashWords(segment.slice(from, after));
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (!word.startsWith("-")) break;
    let value = "";
    if (word.startsWith("--config-env=")) value = word.slice("--config-env=".length);
    else if (GIT_VALUE_OPTIONS.has(word)) {
      // Step over every option value (`git -C . -c alias.z=…`); only a `-c`
      // or `--config-env` value can name a key.
      const next = words[++i] ?? "";
      if (word === "-c" || word === "--config-env") value = next;
    }
    if (GIT_REDIRECTING_KEY.test(value)) return true;
  }
  if (verb !== "config") return false;
  // A config clause is read whole; one past 4 KB is refused unread, which
  // keeps repeated `git config` words linear and fails closed.
  const end = clauseEnd(segment, after);
  return end - after > 4096 || gitConfigAliasVerdict(segment.slice(after, end));
}

/** True when any short flag cluster in the text carries one of the letters. */
function hasShortFlag(text, letters) {
  for (const match of text.matchAll(/(?:^|[ \t])-([a-zA-Z]+)(?=[ \t]|$)/g)) {
    if ([...match[1]].some((letter) => letters.includes(letter))) return true;
  }
  return false;
}

/**
 * True when the argument text carries a long option that GNU getopt resolves
 * to one of `options`: any prefix of the spelled option counts (`--har` for
 * `--hard`), the way `isRmLongDeleteOption` reads `rm` (WI#841). A bare `--`
 * marker, a `--name=value` word and a longer word sharing the letters do not
 * match; an adjacent redirect ends the option word.
 */
function hasGitLongOption(text, options) {
  for (const match of text.matchAll(/(?:^|[ \t])(--[a-z][a-z-]*)(?=$|[ \t<>])/g)) {
    if (options.some((option) => option.startsWith(match[1]))) return true;
  }
  return false;
}

/**
 * Deny git commands that irrecoverably discard uncommitted work (WI#826,
 * WI#832): forced `clean` (a dry run passes), `reset --hard`, a whole-tree
 * checkout or restore, forced checkout/switch, and a `stash` that takes
 * untracked or ignored files. Long options are read by any GNU-unambiguous
 * prefix, like the `rm` rules (WI#841). Linear scan over the git invocations.
 *
 * The verb rules read the literal verb, so an alias is refused before them
 * (WI#842): the alias name would be an arbitrary verb they have never seen.
 */
function checkGitDiscard(segment) {
  if (hasGitAliasEnv(segment)) return true;
  for (const match of segment.matchAll(GIT_CALL)) {
    if (gitAliasDefinition(segment, match.index + 3)) return true;
    const { verb, after } = gitVerb(segment, match.index + 3);
    // Parse the verb before reading any options: a long clause full of
    // ordinary `git log` words then costs one word apiece (WI#824 deadline).
    if (verb !== "clean" && verb !== "reset" && verb !== "checkout" &&
        verb !== "restore" && verb !== "switch" && verb !== "stash") continue;
    const args = segment.slice(after, clauseEnd(segment, after));
    const dryRun = verb === "clean" &&
      (hasShortFlag(args, "n") || hasGitLongOption(args, ["--dry-run"]));
    if (verb === "clean" && !dryRun &&
        (hasShortFlag(args, "fdxX") || hasGitLongOption(args, ["--force"]))) {
      return true;
    }
    if (verb === "reset" && hasGitLongOption(args, ["--hard"])) return true;
    if ((verb === "checkout" || verb === "restore") && /(?:^|[ \t])\.\/?(?=[ \t]*$)/.test(args)) {
      return true;
    }
    if ((verb === "checkout" || verb === "switch") &&
        (hasShortFlag(args, "f") || hasGitLongOption(args, ["--force", "--discard-changes"]))) {
      return true;
    }
    if (verb === "stash" &&
        (hasShortFlag(args, "ua") || hasGitLongOption(args, ["--include-untracked", "--all"]))) {
      return true;
    }
  }
  return false;
}

/** The command word of a pipeline stage, after env/exec/busybox prefixes. */
function commandBasename(word) {
  return stripShellQuotes(word).replace(/^[({]+/, "").replace(/[)}]+$/, "").split("/").pop() ?? "";
}

function stageCommand(stage) {
  const words = argumentWords(stage).map(stripShellQuotes);
  let i = 0;
  while (i < words.length) {
    const isPrefix = STAGE_PREFIXES.includes(commandBasename(words[i])) || /^[A-Za-z_]\w*=/.test(words[i]);
    if (!isPrefix) break;
    i++;
  }
  return { name: commandBasename(words[i] ?? ""), args: words.slice(i + 1) };
}

/**
 * True for a stage that turns an encoded payload back into text (WI#826,
 * WI#832): `base64 -d`, `xxd -r`, `rev`, a decompressor, `openssl base64 -d`,
 * `perl`/`python` base64 one-liners, `printf` with hex or octal escapes, and
 * `tr` — but only when `tr` deletes nothing, because `tr -d '\r'` normalises
 * text rather than decoding it.
 */
function isDecodeStage(stage) {
  const { name, args } = stageCommand(stage);
  if (name === "base64") return args.some((word) => /^-[a-zA-Z]*d[a-zA-Z]*$/.test(word) || word === "--decode");
  if (name === "xxd") return args.some((word) => /^-[a-zA-Z]*r[a-zA-Z]*$/.test(word) || word === "--revert");
  if (name === "openssl") {
    return args[0] === "base64" && args.some((word) => /^-[a-zA-Z]*d[a-zA-Z]*$/.test(word) || word === "-d");
  }
  if (["gzip", "gunzip", "zcat", "bzip2", "bzcat", "bunzip2", "xz", "unxz", "xzcat", "zstd", "unzstd", "zstdcat"]
    .includes(name)) return true;
  if (name === "printf") return args.some((word) => /\\x[0-9a-fA-F]{2}|\\[0-7]{3}/.test(word));
  if (name === "perl") {
    return args.some((word) => /-M(?:MIME::)?Base64|decode_base64/.test(word));
  }
  if (name === "python" || name === "python3") {
    return args.some((word) => /\bb64decode\b|\ba85decode\b|\bb32decode\b/.test(word));
  }
  if (name === "tr") return !args.some((word) => /^-[a-zA-Z]*d[a-zA-Z]*$/.test(word) || word === "--delete");
  return name === "rev";
}

/** True when the stage executes its stdin: a shell, or an interpreter one-liner. */
function isInterpreterStage(stage) {
  const { name, args } = stageCommand(stage);
  if (SHELL_NAME.test(name)) return true;
  if (name === "python" || name === "python3") return args.length === 0 || args.includes("-c") || args.includes("-");
  if (name === "perl") return args.some((word) => /^-[a-zA-Z]*[eE]/.test(word));
  if (name === "ruby") return args.some((word) => /^-[a-zA-Z]*e/.test(word));
  if (name === "node") {
    return args.some((word) => ["-e", "--eval", "-p", "--print", "-"].includes(word));
  }
  return false;
}

/**
 * Split text on a one-character separator, ignoring separators inside quotes
 * and after a backslash. A quoted interpreter body (`-c 'a;b'`) stays one
 * word for the pipeline rules (WI#832), and the segment splitter
 * (`commandSegments`) reads its control operators the same way, so every
 * segment-scoped rule sees a quoted argument whole (WI#856). A de-escaped
 * view (`shellWordView`) passes `escapes: false`: the backslashes left in it
 * are literal characters, not escapes.
 */
function splitOutsideQuotes(text, separator, escapes = true) {
  const pieces = [];
  let piece = "";
  let quote = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (escapes && ch === "\\" && quote !== "'" && i + 1 < text.length) {
      piece += ch + text[++i];
      continue;
    }
    if (quote) {
      if (ch === quote) quote = "";
    } else if (ch === "'" || ch === '"') {
      quote = ch;
    } else if (ch === separator) {
      pieces.push(piece);
      piece = "";
      continue;
    }
    piece += ch;
  }
  pieces.push(piece);
  return pieces;
}

/** The `;`, LF, `&&` and `||` lists of the text, read outside quotes. */
function commandLists(text) {
  return splitOutsideQuotes(text, ";").flatMap((piece) => splitOutsideQuotes(piece, "\n"))
    .flatMap((piece) => piece.split(/&&|\|\|/));
}

/** Deny a decode command piped into a shell or interpreter (WI#826, WI#832). */
function checkDecodedPipe(text) {
  for (const list of commandLists(text)) {
    const stages = splitOutsideQuotes(list, "|");
    for (let i = 1; i < stages.length; i++) {
      if (isInterpreterStage(stages[i]) && isDecodeStage(stages[i - 1])) {
        return "decoded content piped into a shell or interpreter";
      }
    }
  }
  return null;
}

const DENY_RULES = [
  {
    name: "privilege escalation",
    pattern: /(^|[\s;&|(])(?:[\w.+-]*\/)*(sudo|doas)\b/,
  },
  {
    name: "power/disk destruction command",
    pattern:
      /\b(shutdown|halt|poweroff|reboot|mkfs(\.[a-z0-9]+)?|wipefs|mkswap|blkdiscard|sgdisk|bcdedit|diskpart|fdisk|sfdisk|parted)\b/,
  },
  {
    name: "fork bomb",
    // Classic bash fork bomb shapes: `:(){ :|:& };:`, `f(){ f|f& }; f`,
    // `x(){ x|x&; }; x`, and equivalents — a function definition whose body
    // pipes itself into itself in the background.
    pattern: /\([^)]*\)\s*\{[^}]*\|[^}]*&[^}]*\}/,
  },
  {
    name: "remote content piped into a shell or interpreter",
    // The interpreter is matched by any path (| /bin/bash), an env/exec/busybox
    // prefix (| /usr/bin/env bash) and the usual shell names.
    pattern:
      /\b(curl|wget|fetch|nc|ncat|netcat)\b[^|]*\|\s*(?:[\w.+-]*\/)*(?:sudo\s+|env\s+|exec\s+|busybox\s+)*(?:ba|z|da|a|k)?sh\b|\b(curl|wget)\b[^|]*\|\s*(?:[\w.+-]*\/)*(?:sudo\s+|env\s+|exec\s+|busybox\s+)*python3?\b/,
  },
  {
    name: "git command that discards workspace changes",
    // Irrecoverable discards of uncommitted work in the task workspace
    // (WI#826): forced `git clean`, `git reset --hard`, a whole-tree checkout,
    // and `git stash -u`. Dry runs (`clean -n`), `git stash` without `-u` and
    // `git status` stay ordinary work.
    check: checkGitDiscard,
  },
  {
    name: "recursive mode/ownership change on system roots",
    // The bare filesystem root and every listed root count, with the root
    // spelled however bash passes it (WI#826).
    check: checkRecursiveRootChange,
  },
  {
    name: "direct block-device write",
    // Device namespaces beyond the legacy names: LVM (/dev/mapper/…) and the
    // udev links (/dev/disk/by-id/…). truncate is only denied at a block device.
    // The `>|` noclobber override writes the same device as `>` (WI#832).
    pattern:
      />\|?\s*\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)|\bof=\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)|\btruncate\b[^;&|]*\s+\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)/,
  },
  {
    name: "SSH agent use (worker lane is not a credential broker)",
    // Every lane that mounts an agent shares a directory at /tmp/ssh-agent
    // with the socket inside, exported as
    // SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock
    // (ic/scripts/mt-admin-lib/09-worker-config.sh). Matching the directory
    // covers that real spelling, the socket inside it, and
    // `ssh -o IdentityAgent=<socket>`; the bare /tmp/ssh-agent.sock literal
    // is the one spelling no lane uses (WI#829). `ssh-add` is the step that
    // loads a key into an agent, i.e. the credential-broker action itself.
    // Direct key-file auth (`ssh -i <key>`, the lane's own ~/.ssh behind
    // `ssh -T`) stays allowed on purpose — see WORKER-CONTAINERS.md.
    pattern:
      /\bSSH_AUTH_SOCK\b|\/tmp\/ssh-agent(?:\.sock)?(?=$|[\/\s"'`.,)\]};|&])|(?:^|[\s;&|()])(?:[\w.+-]*\/)*ssh-add(?![\w-])/,
  },
];

function checkDenyRules(segment) {
  for (const rule of DENY_RULES) {
    const hit = rule.check ? rule.check(segment) : rule.pattern.test(segment);
    if (hit) return rule.name;
  }
  return null;
}

/** Run every deny rule over the whole text and over each command segment. */
function checkDenyRulesEverywhere(text) {
  const wholeTextDenial = checkDenyRules(text);
  if (wholeTextDenial) return wholeTextDenial;
  for (const segment of commandSegments(text)) {
    const denied = checkDenyRules(segment);
    if (denied) return denied;
  }
  return null;
}

// How a backslash-LF pair may read: a continuation dropped, a line break kept
// as a space (it ends a comment or stays in quoted text), and a continuation
// dropped only outside single quotes. bash reads one of these, and the hook
// cannot tell which apostrophe is a real quote (WI#810).
const JOIN_READINGS = [["", ""], [" ", " "], ["", " "]];

function evaluate(command) {
  if (command.length > MAX_COMMAND_CHARS) {
    return `command exceeds ${MAX_COMMAND_CHARS} chars`;
  }
  // Structural rules (fork bombs) see the whole command: segment splitting
  // on ;|& would shred the very shape they match. Keep them on the raw text.
  const rawDenial = checkDenyRulesEverywhere(command) ?? checkRawShapes(command) ??
    checkTrustedUserConfig(command) ?? checkListeners(command);
  if (rawDenial) return rawDenial;
  // bash joins every backslash-LF pair into the word around it, so a rule
  // anchored on a word never matches the raw text (`su\<LF>do`). Run every
  // rule on the joined text, on its lexically normalised absolute paths and
  // on the word view bash builds, the same three readings rm uses (WI#813,
  // WI#826). An extra quote-preserving reading separates the shell
  // punctuation bash glues to a word, so `(git reset --hard)` and
  // `$(chmod -R 777 /etc)` read as the commands they run (WI#853).
  for (const [lineBreak, quotedLineBreak] of JOIN_READINGS) {
    const joined = joinLineContinuations(command, lineBreak, quotedLineBreak);
    const punctuation = shellPunctuationView(joined);
    const joinedDenial = checkDenyRulesEverywhere(joined) ??
      checkDenyRulesEverywhere(normalizeAbsolutePaths(joined)) ??
      (punctuation === joined ? null : checkDenyRulesEverywhere(punctuation)) ??
      checkDenyRulesEverywhere(shellWordView(joined)) ??
      checkDenyRulesEverywhere(normalizeAbsolutePaths(shellWordView(joined))) ??
      checkTrustedUserConfig(joined) ??
      checkTrustedUserConfig(shellWordView(joined)) ??
      checkRawShapes(joined) ??
      checkMoveAndTarDeletion(shellWordView(joined)) ??
      (joined === command ? null : checkListeners(joined));
    if (joinedDenial) return joinedDenial;
    for (const segment of commandSegments(joined)) {
      const denied = checkRm(segment);
      if (denied) return denied;
    }
  }
  return null;
}

// WI#833: keep listener checks separate from the other raw command rules.
// This recognizes literal command shapes, not general Bash execution.
const LISTENER_PREFIXES = new Set([
  "command", "env", "exec", "busybox", "nohup", "setsid", "nice", "ionice", "timeout", "npx", "stdbuf",
  "xargs", "time", "{}", "if", "then", "elif", "else", "while", "until", "do", "done", "!", "{", "}", "(", ")",
]);

/** Read simple command words, keeping quoted values (including empty ones) together. */
function* listenerSegments(text) {
  let words = [];
  let word = "";
  let started = false;
  let quote = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\" && quote !== "'" && i + 1 < text.length) {
      const next = text[i + 1];
      word += quote === '"' && !'\\"$`\n'.includes(next) ? ch : text[++i];
      started = true;
    } else if (quote) {
      if (ch === quote) quote = "";
      else word += ch;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      started = true;
    } else if (" \t;|&\n".includes(ch)) {
      if (started) words.push(word);
      word = "";
      started = false;
      if (";|&\n".includes(ch) && words.length > 0) {
        yield words;
        words = [];
      }
    } else {
      word += ch;
      started = true;
    }
  }
  if (started) words.push(word);
  if (words.length > 0) yield words;
}

// WI#852: every 2.7.2 command that starts a listener or opens the Remote
// Control relay. `vis` binds the session visualizer's HTTP port; `rc` (alias
// `remote`) runs the local server through Remote Control. Re-read
// `kimi --help` against this set at every bump: a new subcommand is not
// covered until it is named here.
const KIMI_SERVICE_SUBCOMMANDS = new Set(["server", "web", "rc", "remote", "vis"]);

// bash glues shell punctuation to a command word: `(kimi`, `$(kimi`,
// `` `kimi ``, `server)`, `${…}`, `kimi server}`. This reader has no parser,
// so strip the punctuation a listener word may carry; quoted prompt text is
// already one word and is unaffected (WI#839).
function listenerWord(word) {
  return word.replace(/^[({`$]+/, "").replace(/[)}`]+$/, "");
}

/** Body between the `(` at openIndex and its matching `)`, or undefined. */
function listenerParenBody(text, openIndex) {
  let depth = 0;
  let quote = "";
  for (let i = openIndex; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\") {
      if (quote !== "'") i++;
      continue;
    }
    if (quote) {
      if (ch === quote) quote = "";
    } else if (ch === "'" || ch === '"') {
      quote = ch;
    } else if (ch === "(") {
      depth++;
    } else if (ch === ")" && --depth === 0) {
      return text.slice(openIndex + 1, i);
    }
  }
  return undefined;
}

/** Body between the backtick at openIndex and the next unescaped backtick. */
function listenerBacktickBody(text, openIndex) {
  for (let i = openIndex + 1; i < text.length; i++) {
    if (text[i] === "\\") i++;
    else if (text[i] === "`") return text.slice(openIndex + 1, i);
  }
  return undefined;
}

// The bodies bash runs as commands of their own: `$(…)` and backticks
// outside single quotes. A substitution inside double quotes still runs, so
// it counts; text in single quotes is literal and is skipped (WI#839).
function* listenerBodies(text) {
  let quote = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\") {
      if (quote !== "'") i++;
      continue;
    }
    if (quote === "'") {
      if (ch === "'") quote = "";
      continue;
    }
    if (quote === '"' && ch === '"') {
      quote = "";
      continue;
    }
    if (!quote && (ch === "'" || ch === '"')) {
      quote = ch;
      continue;
    }
    if (ch === "$" && text[i + 1] === "(") {
      const body = listenerParenBody(text, i + 1);
      // An unclosed substitution is a bash syntax error; its remainder is
      // still scanned rather than skipped, and scanning stops there (linear).
      yield body ?? text.slice(i + 2);
      if (body === undefined) return;
      i += body.length + 1;
    } else if (ch === "`") {
      const body = listenerBacktickBody(text, i);
      yield body ?? text.slice(i + 1);
      if (body === undefined) return;
      i += body.length + 1;
    }
  }
}

// `kimi --help` at the pinned CLI version (2.7.2, WI#845). A value-taking
// option consumes the next word, so the subcommand is the first word that is
// neither an option nor consumed by one. `-S`/`--session` takes an optional
// id; consuming it as a value matches what the CLI does with a following word.
const KIMI_VALUE_OPTIONS = new Set([
  "-S", "--session", "-m", "--model", "-p", "--prompt", "--output-format",
  "--skills-dir", "--agent", "--agent-file", "--add-dir",
]);
const KIMI_FLAG_OPTIONS = new Set([
  "-V", "--version", "-c", "--continue", "-y", "--yolo", "--auto", "--plan", "-h", "--help",
]);

/**
 * Deny a service subcommand unless every reading of the option words agrees
 * there is none (WI#845). Only options the table knows are read as the CLI
 * reads them; an unknown option may or may not consume the next word and the
 * hook cannot ask the CLI, so both readings are walked and a service
 * subcommand (`KIMI_SERVICE_SUBCOMMANDS`) reached by either one is a denial.
 * `--option=value` needs no special case: its value is part of the option
 * word.
 */
function kimiServiceSubcommand(words, start) {
  const pending = [start];
  const seen = new Set();
  while (pending.length > 0) {
    const i = pending.pop();
    if (i >= words.length || seen.has(i)) continue;
    seen.add(i);
    const word = words[i];
    // The subcommand word may carry the punctuation bash glues to it
    // (`server)` in `(kimi -m X server)`), read as in WI#839.
    if (word === "--") {
      const after = listenerWord(words[i + 1] ?? "");
      if (KIMI_SERVICE_SUBCOMMANDS.has(after)) return after;
      continue;
    }
    if (!word.startsWith("-")) {
      const subcommand = listenerWord(word);
      if (KIMI_SERVICE_SUBCOMMANDS.has(subcommand)) return subcommand;
      continue;
    }
    if (word.includes("=")) pending.push(i + 1);
    else if (KIMI_VALUE_OPTIONS.has(word)) pending.push(i + 2);
    else if (KIMI_FLAG_OPTIONS.has(word)) pending.push(i + 1);
    else pending.push(i + 1, i + 2);
  }
  return null;
}

const LISTENER_COMMANDS = [
  {
    name: /^python[0-9.]*$/,
    test: (args) => {
      const moduleIndex = args.indexOf("-m");
      return moduleIndex !== -1 && args[moduleIndex + 1] === "http.server";
    },
  },
  {
    name: /^(?:nc|ncat|netcat)$/,
    test: (args) => args.some((word) => /^-[a-zA-Z]*l[a-zA-Z]*$/.test(word) || word === "--listen"),
  },
  { name: /^socat$/, test: (args) => args.some((word) => /TCP[46]?-LISTEN:/.test(word)) },
  { name: /^php[0-9.]*$/, test: (args) => args.includes("-S") },
  { name: /^ruby$/, test: (args) => args.includes("-run") && args.includes("httpd") },
];

function listenerShellBody(words, start) {
  for (let i = start; i < words.length && words[i].startsWith("-"); i++) {
    if (words[i] === "--") break;
    if (/^-[a-zA-Z]*c[a-zA-Z]*$/.test(words[i])) return words[i + 1];
  }
  return undefined;
}

const FIND_EXEC_ACTIONS = new Set(["-exec", "-execdir", "-ok", "-okdir"]);

/**
 * Deny a listener started from a find `-exec`/`-ok` body (WI#846). Bash runs
 * that body exactly as it runs an `xargs` body, which LISTENER_PREFIXES
 * already reads; the body is the words up to the `;` or `+` terminator. A
 * quoted or escaped terminator arrives as its own `;` word.
 */
function checkFindExecListeners(words, start, depth) {
  for (let i = start; i < words.length; i++) {
    if (!FIND_EXEC_ACTIONS.has(words[i])) continue;
    const body = [];
    for (i++; i < words.length && words[i] !== ";" && words[i] !== "+"; i++) body.push(words[i]);
    if (body.length === 0) continue;
    const denied = checkListenerWords(body, depth + 1);
    if (denied) return denied;
  }
  return null;
}

/** Classify the command word of one listener segment and its arguments. */
function checkListenerWords(words, depth) {
  for (let i = 0; i < words.length; i++) {
    const raw = words[i];
    const word = listenerWord(raw);
    const name = commandBasename(word);
    if (name === "busybox" && listenerWord(words[i + 1] ?? "") === "httpd") {
      return "listener started from a task: busybox httpd";
    }
    if (LISTENER_PREFIXES.has(raw) || LISTENER_PREFIXES.has(word) ||
        LISTENER_PREFIXES.has(name) ||
        /^[A-Za-z_][A-Za-z0-9_]*=|^\d+$|^-/.test(raw)) continue;

    if (name === "kimi") {
      const subcommand = kimiServiceSubcommand(words, i + 1);
      if (subcommand !== null) return `kimi service mode from a task: kimi ${subcommand}`;
    } else if (SHELL_NAME.test(name)) {
      const body = listenerShellBody(words, i + 1);
      if (body !== undefined) {
        if (depth >= 4) return "cannot check listeners in a deeply nested shell command";
        const denied = checkListeners(body, depth + 1);
        if (denied) return denied;
      }
    } else if (name === "find") {
      const denied = checkFindExecListeners(words, i + 1, depth);
      if (denied) return denied;
    } else {
      const args = words.slice(i + 1).map(listenerWord);
      const rule = LISTENER_COMMANDS.find((candidate) => candidate.name.test(name));
      if (rule?.test(args)) return `listener started from a task: ${name}`;
    }
    break;
  }
  return null;
}

/** Deny recognized service commands without treating prompt text as a subcommand. */
function checkListeners(text, depth = 0) {
  // A substitution or backtick body runs its words as a command, so scan it
  // the same way even when it sits in an argument (`echo $(kimi server)`).
  for (const body of listenerBodies(text)) {
    if (depth >= 4) return "cannot check listeners in a deeply nested shell command";
    const denied = checkListeners(body, depth + 1);
    if (denied) return denied;
  }
  for (const words of listenerSegments(text)) {
    const denied = checkListenerWords(words, depth);
    if (denied) return denied;
  }
  return null;
}

// WI#848: the user-level MCP registration is trusted input, not task state.
// The CLI spawns every server listed in `$KIMI_CODE_HOME/mcp.json` before the
// first model turn, outside this Bash hook — the same self-opened inbound
// path the WI#833 listener rule denies, reached through a file instead of a
// command. The file is in the task's own home, so a task can rewrite it (a
// redirect, an interpreter json edit, `sed -i`). These two shapes name the
// protected file through the config directory; a bare `mcp.json` after
// `cd`-ing into that directory is caught by the directory rule. A task that
// builds the path from fragments, or writes it from a script file, is the
// residual the executor's per-spawn restore covers (WI#848). Ordinary repo
// work — `grep -rn mcp.json docs/`, `grep -rn KIMI_CODE_HOME .`, reading the
// read-only /app/config copy — stays allowed.
const TRUSTED_USER_MCP_FILE =
  /KIMI_CODE_HOME\b[^\s;&|]{0,24}mcp\.json|\.kimi-code\/mcp\.json/;
const TRUSTED_USER_CONFIG_DIR =
  /(?:^|[\s;&|()])(?:cd|pushd)\s+["']?(?:\$?\{?KIMI_CODE_HOME\}?|~\/\.kimi-code|\$\{?HOME\}?\/\.kimi-code|\/home\/kimi\/\.kimi-code)\/?["']?(?=$|[\s;&|)])/;

function checkTrustedUserConfig(text) {
  if (TRUSTED_USER_MCP_FILE.test(text) || TRUSTED_USER_CONFIG_DIR.test(text)) {
    return "the user-level Kimi MCP config is trusted state, not task input (WI#848)";
  }
  return null;
}

// Leave substitutions opaque in this reading. In particular, an operator
// inside a substitution must not separate the outer command from its args.
// Scan once without trying to run the substitution or predict its output.
function maskSubstitutions(text) {
  let masked = "";
  let depth = 0;
  let backticks = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "`") {
      if (depth === 0 && !backticks) masked += "$";
      backticks = !backticks;
    } else if (backticks) {
      continue;
    } else if (depth > 0) {
      if (ch === "(") depth++;
      if (ch === ")") depth--;
    } else if (ch === "$" && text[i + 1] === "(") {
      masked += "$";
      depth = 1;
      i++;
    } else {
      masked += ch;
    }
  }
  return masked;
}

const RM_DYNAMIC_WORD = /[$`*?[\]{}]/;
const RM_WRAPPERS = new Set(["command", "env", "exec", "busybox"]);

function hasRmDeleteOption(words) {
  for (const word of words) {
    if (word === "--") return false;
    const shortFlags = /^-([A-Za-z]+)(?=$|[<>])/.exec(word)?.[1];
    if ((shortFlags && /[rRfF]/.test(shortFlags)) || isRmLongDeleteOption(word)) return true;
  }
  return false;
}

function hasAssembledDeleteOption(words) {
  // Inspect brace alternatives without expanding them. A slice of IFS may
  // also separate words; keep it opaque except when looking for flag tokens.
  return hasRmDeleteOption(words.flatMap((word) => word.split(/\$\{IFS:[^{}\n]*\}|[{},]/)));
}

function checkAmbiguousRm(segment) {
  const words = segment.split(/[ \t]+/).filter(Boolean);
  let first = 0;
  while (first < words.length) {
    const word = words[first];
    if (!/^[A-Za-z_][A-Za-z0-9_]*=/.test(word) &&
        !RM_WRAPPERS.has(word.slice(word.lastIndexOf("/") + 1))) break;
    first++;
  }
  const command = words[first];
  if (!command) return null;

  if (RM_DYNAMIC_WORD.test(command) && hasAssembledDeleteOption(words.slice(first))) {
    return "recursive/forced rm with a dynamically assembled command word; use a literal command and temp path";
  }
  // As in checkRm, also recognize a literal rm after a wrapper. The opaque
  // reading is for adding denials only, never for accepting a rewritten path.
  const rm = words.findIndex((word) => word === "rm" || word.endsWith("/rm"));
  if (rm !== -1) {
    const args = words.slice(rm + 1);
    // Empty braces are not Bash expansion. Leave any placeholder handling
    // to the existing rm and pipeline rules.
    if (hasAssembledDeleteOption(args) &&
        rmOperands(args, args.indexOf("--")).some((word) => word !== "{}" && RM_DYNAMIC_WORD.test(word))) {
      return "recursive/forced rm with an expanded operand; use a literal temp path";
    }
  }
  return null;
}

function checkRmAssembly(command) {
  const readings = new Set(JOIN_READINGS.map(([outside, inside]) => joinLineContinuations(command, outside, inside)));
  for (const reading of readings) {
    const words = shellWordView(reading);
    // Check plain IFS separators as spaces without evaluating parameters.
    const separated = words.replace(/\$\{IFS\}|\$IFS(?![A-Za-z0-9_])/g, " ");
    const inSubstitution = separated.replace(/\$\(|[`()]/g, " ");
    for (const segment of commandSegments(inSubstitution, false)) {
      const denied = checkRm(segment);
      if (denied) return denied;
    }

    const opaque = maskSubstitutions(separated).replace(/[()]/g, ";");
    for (const segment of commandSegments(opaque, false)) {
      const denied = checkAmbiguousRm(segment);
      if (denied) return denied;
    }
  }
  return null;
}

// A symbolic link created anywhere in the same command line makes containment
// unverifiable for that line: the link does not exist at check time, so a
// recursive or forced deletion below it resolves as a name to be created and
// bash then removes through the link (WI#830 AIT-1, from WI#812 review).
const LINK_CREATORS = new Set(["ln", "cp"]);
const SYMLINK_FLAG = /^-[a-zA-Z]*s[a-zA-Z]*$|^--symbolic(?:-link)?$/;

/** True when a segment runs `ln`/`cp` with a symbolic-link flag. */
function createsSymbolicLink(segment) {
  const words = argumentWords(segment).map(stripShellQuotes);
  for (let i = 0; i < words.length; i++) {
    if (!LINK_CREATORS.has(commandBasename(words[i]))) continue;
    for (let j = i + 1; j < words.length && !/^[;&|]/.test(words[j]); j++) {
      if (SYMLINK_FLAG.test(words[j])) return true;
      // A word that is not an option starts the operands; keep scanning, a
      // later flag is still ln's (`ln a b -s` is accepted by GNU ln).
    }
  }
  return false;
}

/** Deny a recursive or forced deletion in a line that also creates a symlink. */
function checkSameLineLink(command) {
  const readings = new Set(JOIN_READINGS.map(([outside, inside]) => joinLineContinuations(command, outside, inside)));
  for (const reading of readings) {
    const view = shellWordView(reading);
    if (!createsSymbolicLink(view)) continue;
    for (const segment of commandSegments(view, false)) {
      const rest = /\brm[ \t]+(.*)$/s.exec(segment)?.[1];
      if (rest !== undefined && rmDeleteOption(rest)) {
        return "recursive/forced rm in a command line that also creates a symbolic link; use a separate call";
      }
    }
  }
  return null;
}

// The CLI can allow a hook that misses its 10 s deadline. Interrupt our own
// synchronous classifier much earlier, including any legacy regex checks.
const ANALYSIS_TIMEOUT_MS = 2_000;

function classify(command) {
  return evaluate(command) ?? checkRmAssembly(command) ?? checkSameLineLink(command);
}

async function main() {
  let payload;
  try {
    payload = JSON.parse(await readStdin());
  } catch {
    deny("PreToolUse payload is not valid JSON");
    return;
  }
  const command = payload?.tool_input?.command;
  if (typeof command !== "string" || command.trim().length === 0) {
    deny("Bash tool call without a command string");
    return;
  }
  let reason;
  try {
    // The script source is constant: the shell command is only passed as
    // data to the classifier and is never executed by the hook.
    reason = runInNewContext("classify(command)", { classify, command }, {
      timeout: ANALYSIS_TIMEOUT_MS,
      contextCodeGeneration: { strings: false, wasm: false },
    });
  } catch (error) {
    if (error?.code !== "ERR_SCRIPT_EXECUTION_TIMEOUT") throw error;
    deny("command analysis exceeded its time budget; use simpler commands");
  }
  if (reason) deny(reason);
  else allow();
}

main().catch(() => deny("guard failed unexpectedly"));
