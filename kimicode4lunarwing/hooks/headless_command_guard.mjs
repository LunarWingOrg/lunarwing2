#!/usr/bin/env node
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
// that discard uncommitted work, SSH-agent socket use). It is deliberately
// NOT a general command parser: everything it cannot positively classify as
// safe within its ruleset is allowed unless it matches a deny rule, EXCEPT
// unparseable input, which denies (fail closed at the transport layer) and
// destructive shapes whose target it cannot verify, which also deny.

const MAX_COMMAND_CHARS = 16384;
const RM_SAFE_TEMP_ROOTS = ["/tmp", "/temp", "/var/tmp"];

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
 * The containment test shared by every deletion rule (WI#826): a target is
 * provably inside the temp roots only as a literal path, with no expansion,
 * no `..` segment and no glob.
 */
function isSafeTempPath(operand) {
  if (operand.length === 0 || operand.startsWith("$") || operand.includes("..")) return false;
  if (operand.includes("*") || operand.includes("?") || operand.includes("[")) return false;
  return RM_SAFE_TEMP_ROOTS.some((root) => operand === root || operand.startsWith(`${root}/`));
}

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
  const optionWords = endOfOptions === -1 ? words : words.slice(0, endOfOptions);
  // Keep the existing conservative flag checks as well as GNU abbreviations.
  if (!/(^|[ \t])-[a-zA-Z]*[rRfF]|--recursive|--force/.test(rest) &&
      !optionWords.some(isRmLongDeleteOption)) return null;
  const operands = rmOperands(words, endOfOptions)
    // `xargs -I {}` fills the placeholder from its input; the feeding find
    // paths are what checkXargsDeletion classifies (WI#826).
    .filter((operand) => !fromXargs || operand !== "{}");
  if (operands.length === 0) {
    return fromXargs ? null : "rm -r/-f without an explicit operand";
  }
  if (!operands.every(isSafeTempPath)) {
    return `recursive/forced rm outside ${RM_SAFE_TEMP_ROOTS.join(", ")}: ${operands.join(" ").slice(0, 80)}`;
  }
  return null;
}

const FIND_CALL = /(?:^|[\s;&|()])find\s+([^;&|\n]*)/g;
// Deletion actions inside a find expression: `-delete`, `-exec rm`, `-ok rm`.
const FIND_DELETE_ACTION =
  /(?:^|[\s(])(?:-delete|-exec(?:dir)?\s+(?:[\w.+-]*\/)?rm\b|-ok(?:dir)?\s+(?:[\w.+-]*\/)?rm\b)/;
const XARGS_CALL = /(?:^|[\s;&|()])(?:[\w.+-]*\/)?xargs\b([^;&|\n]*)/g;
const XARGS_RM = /(?:^|\s)(?:[\w.+-]*\/)?rm(?:\s|$)/;
const ONE_LINER_CALL =
  /(?:^|[\s;&|()])(?:[\w.+-]*\/)?(?:python3?|perl|node|ruby)\s+(?:-[a-zA-Z]*[ceEp][a-zA-Z]*|--eval)\b([^\n]*)/g;
const DELETION_CALL =
  /\b(?:shutil\.rmtree|rmtree|rmSync|rmdirSync|unlinkSync|os\.(?:remove|unlink|rmdir|removedirs)|fs\.(?:rmSync|rmdirSync|unlinkSync|rm|rmdir|unlink)|rimraf|unlink|rmdir)\s*\(/g;
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

/** Read a quoted first argument starting after a call's `(`; null if it is not a literal. */
function quotedFirstArgument(code, from) {
  let i = from;
  while (i < code.length && /[ \t]/.test(code[i])) i++;
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

/** Deny rmtree/rmSync-style deletion in a Python, Perl, Ruby or Node one-liner. */
function checkInterpreterDeletion(text) {
  for (const invocation of text.matchAll(ONE_LINER_CALL)) {
    const code = invocation[1];
    for (const call of code.matchAll(DELETION_CALL)) {
      const target = quotedFirstArgument(code, call.index + call[0].length);
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

/**
 * Deny git commands that irrecoverably discard uncommitted work (WI#826):
 * forced `clean`, `reset --hard`, a whole-tree checkout and `stash -u`.
 * Linear scan over the git invocations; dry runs and `git status` pass.
 */
function checkGitDiscard(segment) {
  for (const match of segment.matchAll(GIT_CALL)) {
    const { verb, after } = gitVerb(segment.slice(match.index + 3).split(/[;&|\n]/, 1)[0]);
    if (verb === "clean" && /(?:^|[ \t])-[a-zA-Z]*[fdxX]|(?:^|[ \t])--force(?=[ \t]|$)/.test(after)) {
      return true;
    }
    if (verb === "reset" && /(?:^|[ \t])--hard(?=[ \t]|$)/.test(after)) return true;
    if (verb === "checkout" && /(?:^|[ \t])\.(?=[ \t]*$)/.test(after)) return true;
    if (verb === "stash" &&
        /(?:^|[ \t])-[a-zA-Z]*u[a-zA-Z]*(?=[ \t]|$)|(?:^|[ \t])--include-untracked(?=[ \t]|$)/.test(after)) {
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

function isDecodeStage(stage) {
  const { name, args } = stageCommand(stage);
  if (name === "base64") return args.some((word) => /^-[a-zA-Z]*d[a-zA-Z]*$/.test(word) || word === "--decode");
  if (name === "xxd") return args.some((word) => /^-[a-zA-Z]*r[a-zA-Z]*$/.test(word) || word === "--revert");
  return name === "rev" || name === "tr";
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

/** Deny a decode command piped into a shell or interpreter (WI#826). */
function checkDecodedPipe(text) {
  for (const list of text.split(/[;\n]|&&|\|\|/)) {
    const stages = list.split("|");
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
    pattern:
      />\s*\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)|\bof=\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)|\btruncate\b[^;&|]*\s+\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)/,
  },
  {
    name: "SSH agent socket use (worker lane is not a credential broker)",
    pattern: /\bSSH_AUTH_SOCK\b|\/tmp\/ssh-agent\.sock/,
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
  const rawDenial = checkDenyRulesEverywhere(command) ?? checkRawShapes(command);
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
      checkRawShapes(joined);
    if (joinedDenial) return joinedDenial;
    for (const segment of commandSegments(joined)) {
      const denied = checkRm(segment);
      if (denied) return denied;
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

// The CLI can allow a hook that misses its 10 s deadline. Interrupt our own
// synchronous classifier much earlier, including any legacy regex checks.
const ANALYSIS_TIMEOUT_MS = 2_000;

function classify(command) {
  return evaluate(command) ?? checkRmAssembly(command);
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
