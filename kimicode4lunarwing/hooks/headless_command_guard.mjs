#!/usr/bin/env node
import { lstatSync, realpathSync } from "node:fs";
import { basename, dirname } from "node:path";
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

/** Split on control operators and LF, leaving other line terminators in the text. */
function commandSegments(command) {
  return command.split(/[;|&]{1,2}|\n+/);
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

const FIND_CALL = /(?:^|[\s;&|()])find\s+([^;&|\n]*)/g;
// Deletion actions inside a find expression: `-delete`, and `-exec`/`-ok`
// running a deleting utility (WI#826, WI#832: unlink).
const FIND_DELETE_ACTION =
  /(?:^|[\s(])(?:-delete|-exec(?:dir)?\s+(?:[\w.+-]*\/)?(?:rm|unlink|rmdir|shred)\b|-ok(?:dir)?\s+(?:[\w.+-]*\/)?(?:rm|unlink|rmdir|shred)\b)/;
const XARGS_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?xargs\b([^;&|\n]*)/g;
const XARGS_RM = /(?:^|\s)(?:[\w.+-]*\/)?rm(?:\s|$)/;
const RSYNC_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?rsync\b([^;&|\n]*)/g;
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
// Perl calls deletion without parentheses: `rmtree "/x"`, `unlink glob "/x/*"`.
const PERL_BAREWORD_DELETE = /\b(?:unlink|rmdir|rmtree|remove_tree)\b(?!\s*\()\s*(?:glob\s+)?(['"])([^'"]*)\1/g;
const PERL_BAREWORD_VARIABLE = /\b(?:unlink|rmdir|rmtree|remove_tree)\b(?!\s*\()\s+(?:glob\s+)?[$@%]/g;
const DEVICE_TARGET = /^\/dev\/(?:sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)/;
const TEE_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?tee\b([^;&|\n]*)/g;
const CP_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?cp\b([^;&|\n]*)/g;

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
    if (!FIND_DELETE_ACTION.test(match[1])) continue;
    const paths = findStartPaths(match[1]);
    if (!paths.every(isSafeTempPath)) return outsideTempReason("find", paths);
  }
  return null;
}

/** The pipeline list that feeds an `xargs` call (pipes stay in the list). */
function feedingPipeline(text, index) {
  const lists = text.slice(0, index).split(/[;\n]|&&|\|\|/);
  return lists[lists.length - 1] ?? "";
}

/** The start paths of the last find in a pipeline, or null when there is none. */
function lastFindPaths(pipeline) {
  let paths = null;
  for (const match of pipeline.matchAll(FIND_CALL)) paths = findStartPaths(match[1]);
  return paths;
}

/** Deny `xargs rm` unless its feeding find is provably temp-rooted. */
function checkXargsDeletion(text) {
  for (const match of text.matchAll(XARGS_CALL)) {
    if (!XARGS_RM.test(match[1])) continue;
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
    const words = argumentWords(match[1]).map(stripShellQuotes);
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
 * `'print "unlink"'` becomes `print "unlink"` (WI#832).
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
  return end === text.length - 1 ? text.slice(1, -1) : text;
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

/**
 * The Perl parenthesis-free deletion verdict: `rmtree "dir"`, `unlink glob
 * "dir/*"`, and a variable operand the guard cannot resolve (WI#832). A call
 * name inside a string literal (`print "unlink"`) is text, not a call.
 */
function perlDeletionVerdict(rawCode) {
  const code = stripOuterShellQuotes(rawCode);
  for (const match of code.matchAll(PERL_BAREWORD_DELETE)) {
    if (insideQuotedString(code, match.index)) continue;
    if (!isSafeTempPath(match[2])) return outsideTempReason("interpreter deletion", [match[2]]);
  }
  for (const match of code.matchAll(PERL_BAREWORD_VARIABLE)) {
    if (insideQuotedString(code, match.index)) continue;
    return "interpreter deletion whose target the guard cannot verify";
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
    for (const word of argumentWords(match[1])) {
      if (word.startsWith("-")) continue;
      const target = deviceTarget(word);
      if (target !== null) return `direct block-device write: tee ${target.slice(0, 80)}`;
    }
  }
  for (const match of text.matchAll(CP_CALL)) {
    const operands = argumentWords(match[1]).filter((word) => !word.startsWith("-"));
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
    checkInterpreterDeletion(text) ??
    checkDeviceWrites(text) ??
    checkDecodedPipe(text);
}

const SYSTEM_ROOTS = ["/etc", "/usr", "/var", "/home", "/root", "/boot", "/bin", "/sbin", "/lib", "/lib64", "/opt", "/srv"];
const GIT_CALL = /\bgit\b/g;
const ROOT_CHANGE_CALL = /\bch(?:mod|own|grp)\b/g;
const STAGE_PREFIXES = ["sudo", "env", "exec", "busybox"];
const SHELL_NAME = /^(?:ba|z|da|a|k)?sh$/;

/** True for the filesystem root or a path at or under a listed system root. */
function isSystemRootPath(candidate) {
  if (!candidate.startsWith("/")) return false;
  const path = normalizePath(candidate);
  if (path === "/") return true;
  return SYSTEM_ROOTS.some((root) => path === root || path.startsWith(`${root}/`));
}

/**
 * Deny a recursive mode/ownership change on a system root, however the root is
 * spelled. A linear scan, not a regex: the regex form rescanned the rest of
 * the command from every `chmod` and took ten seconds on 12 KB of them.
 */
function checkRecursiveRootChange(segment) {
  for (const match of segment.matchAll(ROOT_CHANGE_CALL)) {
    const rest = segment.slice(match.index + match[0].length).split(/[;&|\n]/, 1)[0];
    if (!/(?:^|[ \t])(?:-[a-zA-Z]*R[a-zA-Z]*|--recursive)(?=[ \t]|$)/.test(rest)) continue;
    for (const pathMatch of rest.matchAll(/(?:^|[ \t])(\/[^ \t;&|]*)/g)) {
      if (isSystemRootPath(stripShellQuotes(pathMatch[1]))) return true;
    }
  }
  return false;
}

/** The git verb after any leading options, and the argument text after it. */
function gitVerb(rest) {
  let i = 0;
  while (i < rest.length) {
    while (i < rest.length && /[ \t]/.test(rest[i])) i++;
    const start = i;
    while (i < rest.length && !/[ \t]/.test(rest[i])) i++;
    const word = rest.slice(start, i);
    if (word.length === 0) break;
    if (!word.startsWith("-")) return { verb: word, after: rest.slice(i) };
    if (word === "-C" || word === "-c") {
      // These options take a value, which is not the verb.
      while (i < rest.length && /[ \t]/.test(rest[i])) i++;
      while (i < rest.length && !/[ \t]/.test(rest[i])) i++;
    }
  }
  return { verb: "", after: "" };
}

/** True when any short flag cluster in the text carries one of the letters. */
function hasShortFlag(text, letters) {
  for (const match of text.matchAll(/(?:^|[ \t])-([a-zA-Z]+)(?=[ \t]|$)/g)) {
    if ([...match[1]].some((letter) => letters.includes(letter))) return true;
  }
  return false;
}

/**
 * Deny git commands that irrecoverably discard uncommitted work (WI#826,
 * WI#832): forced `clean` (a dry run passes), `reset --hard`, a whole-tree
 * checkout or restore, forced checkout/switch, and a `stash` that takes
 * untracked or ignored files. Linear scan over the git invocations.
 */
function checkGitDiscard(segment) {
  for (const match of segment.matchAll(GIT_CALL)) {
    const { verb, after } = gitVerb(segment.slice(match.index + 3).split(/[;&|\n]/, 1)[0]);
    const dryRun = hasShortFlag(after, "n") || /(?:^|[ \t])--dry-run(?=[ \t]|$)/.test(after);
    if (verb === "clean" && !dryRun &&
        (hasShortFlag(after, "fdxX") || /(?:^|[ \t])--force(?=[ \t]|$)/.test(after))) {
      return true;
    }
    if (verb === "reset" && /(?:^|[ \t])--hard(?=[ \t]|$)/.test(after)) return true;
    if ((verb === "checkout" || verb === "restore") && /(?:^|[ \t])\.(?=[ \t]*$)/.test(after)) {
      return true;
    }
    if ((verb === "checkout" || verb === "switch") &&
        (hasShortFlag(after, "f") || /(?:^|[ \t])--(?:force|discard-changes)(?=[ \t]|$)/.test(after))) {
      return true;
    }
    if (verb === "stash" &&
        (hasShortFlag(after, "ua") || /(?:^|[ \t])--(?:include-untracked|all)(?=[ \t]|$)/.test(after))) {
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
 * and after a backslash, so a quoted interpreter body (`-c 'a;b'`) stays one
 * word for the pipeline rules (WI#832).
 */
function splitOutsideQuotes(text, separator) {
  const pieces = [];
  let piece = "";
  let quote = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\" && quote !== "'" && i + 1 < text.length) {
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
  const rawDenial = checkDenyRulesEverywhere(command) ?? checkRawShapes(command) ?? checkListeners(command);
  if (rawDenial) return rawDenial;
  // bash joins every backslash-LF pair into the word around it, so a rule
  // anchored on a word never matches the raw text (`su\<LF>do`). Run every
  // rule on the joined text, on its lexically normalised absolute paths and
  // on the word view bash builds, the same three readings rm uses (WI#813,
  // WI#826).
  for (const [lineBreak, quotedLineBreak] of JOIN_READINGS) {
    const joined = joinLineContinuations(command, lineBreak, quotedLineBreak);
    const joinedDenial = checkDenyRulesEverywhere(joined) ??
      checkDenyRulesEverywhere(normalizeAbsolutePaths(joined)) ??
      checkDenyRulesEverywhere(shellWordView(joined)) ??
      checkDenyRulesEverywhere(normalizeAbsolutePaths(shellWordView(joined))) ??
      checkRawShapes(joined) ??
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

function kimiServiceSubcommand(words, start) {
  let i = start;
  for (; i < words.length && words[i].startsWith("-"); i++) {
    if (words[i] === "--") {
      i++;
      break;
    }
    // Kimi's -p and --prompt consume one whole argument. --prompt=value
    // needs no special case; its value is already part of the option word.
    if (words[i] === "-p" || words[i] === "--prompt") i++;
  }
  const subcommand = words[i];
  return subcommand === "server" || subcommand === "web" ? subcommand : null;
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

/** Deny recognized service commands without treating prompt text as a subcommand. */
function checkListeners(text, depth = 0) {
  for (const words of listenerSegments(text)) {
    for (let i = 0; i < words.length; i++) {
      const word = words[i];
      const name = commandBasename(word);
      if (name === "busybox" && words[i + 1] === "httpd") return "listener started from a task: busybox httpd";
      if (LISTENER_PREFIXES.has(word) || LISTENER_PREFIXES.has(name) ||
          /^[A-Za-z_][A-Za-z0-9_]*=|^\d+$|^-/.test(word)) continue;

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
      } else {
        const rule = LISTENER_COMMANDS.find((candidate) => candidate.name.test(name));
        if (rule?.test(words.slice(i + 1))) return `listener started from a task: ${name}`;
      }
      break;
    }
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
    for (const segment of commandSegments(inSubstitution)) {
      const denied = checkRm(segment);
      if (denied) return denied;
    }

    const opaque = maskSubstitutions(separated).replace(/[()]/g, ";");
    for (const segment of commandSegments(opaque)) {
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
    for (const segment of commandSegments(view)) {
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
