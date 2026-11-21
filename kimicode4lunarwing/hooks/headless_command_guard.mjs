#!/usr/bin/env node
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
  const operands = rmOperands(words, endOfOptions);
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

/** Deletion and device checks that need the command text with its quoting intact. */
function checkRawShapes(text) {
  return checkFindDeletion(text) ??
    checkXargsDeletion(text) ??
    checkInterpreterDeletion(text) ??
    checkDeviceWrites(text);
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
    name: "decoded content piped into a shell or interpreter",
    // A decode command on the left of a pipe runs attacker-chosen text just as
    // a fetched script does (WI#826); the interpreter alternation mirrors the
    // pipe rule above. `python3 -m <module>` reads its input as data and stays
    // ordinary, and `rev`/`tr` must be a whole word so `rev-list` does not match.
    pattern:
      /(?:^|[\s;&|()])(?:base64\b[^|]*?\s(?:-[a-zA-Z]*d[a-zA-Z]*|--decode)\b|xxd\b[^|]*?\s(?:-[a-zA-Z]*r[a-zA-Z]*|--revert)\b|(?:rev|tr)(?=[\s|]))[^|]*\|\s*(?:[\w.+-]*\/)*(?:sudo\s+|env\s+|exec\s+|busybox\s+)*(?:(?:ba|z|da|a|k)?sh\b|python3?\s+(?:-c\b|-(?=\s|$))|perl\s+(?:-e|-E)\b|ruby\s+-e\b|node\s+(?:-e|--eval)\b)/,
  },
  {
    name: "git command that discards workspace changes",
    // Irrecoverable discards of uncommitted work in the task workspace
    // (WI#826): forced `git clean`, `git reset --hard`, a whole-tree checkout,
    // and `git stash -u`. Dry runs (`clean -n`), `git stash` without `-u` and
    // `git status` stay ordinary work.
    pattern:
      /\bgit\b[^;&|]*\bclean\b[^;&|]*\s(?:-[a-zA-Z]*[fdxX]|--force)\b|\bgit\b[^;&|]*\breset\b[^;&|]*--hard\b|\bgit\b[^;&|]*\bcheckout\b[^;&|]*\s--?\s*\.(?:\s|$)|\bgit\b[^;&|]*\bcheckout\b[^;&|]*\s\.(?:\s|$)|\bgit\b[^;&|]*\bstash\b[^;&|]*\s(?:-[a-zA-Z]*u[a-zA-Z]*|--include-untracked)\b/,
  },
  {
    name: "recursive mode/ownership change on system roots",
    // The system-root alternation is optional so the bare filesystem root (/)
    // is covered too; a named directory still matches with or without a trailing slash.
    pattern:
      /\bch(mod|own|grp)\b[^;&|]*(-R|--recursive)[^;&|]*\s+\/(?:(?:etc|usr|var|home|root|boot|bin|sbin|lib|lib64|opt|srv)?(?:\/|\s|$))/,
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
    if (rule.pattern.test(segment)) return rule.name;
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
      checkRawShapes(joined);
    if (joinedDenial) return joinedDenial;
    for (const segment of commandSegments(joined)) {
      const denied = checkRm(segment);
      if (denied) return denied;
    }
  }
  return null;
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
  const reason = evaluate(command);
  if (reason) deny(reason);
  else allow();
}

main().catch(() => deny("guard failed unexpectedly"));
