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
//   keeps its logic free of third-party dependencies and always exits 0 or 2.
//
// This guards the dangerous classes from the upstream analyzer
// (power/privilege/disk destruction, fork bombs, pipe-to-shell) plus
// worker-lane specifics (SSH-agent socket use). It is deliberately NOT a
// general command parser: everything it cannot positively classify as safe
// within its ruleset is allowed unless it matches a deny rule, EXCEPT
// unparseable input, which denies (fail closed at the transport layer).

import { realpathSync } from "node:fs";
import { basename, dirname } from "node:path";

const MAX_COMMAND_CHARS = 16384;
const RM_SAFE_TEMP_ROOTS = ["/tmp", "/temp", "/var/tmp"];
// Characters that make bash rewrite the word before rm sees it: parameter and
// command expansion, quote removal, globs and brace lists. The operand below is
// read as rm's literal argument, so a word carrying any of them would be
// checked in one shape and deleted in another.
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

// Resolve the longest existing prefix of a path, following symlinks, and
// re-attach the components that do not exist yet: rm deletes those names
// rather than traversing them, so there is no symlink to follow there. Returns
// null when the prefix cannot be resolved (symlink loop, permission error),
// which denies.
function resolveExistingPrefix(target) {
  let prefix = target;
  const missing = [];
  for (;;) {
    try {
      const resolved = realpathSync.native(prefix);
      if (missing.length === 0) return resolved;
      const base = resolved.endsWith("/") ? resolved : `${resolved}/`;
      return `${base}${missing.reverse().join("/")}`;
    } catch (error) {
      if (error?.code !== "ENOENT" && error?.code !== "ENOTDIR") return null;
      const parent = dirname(prefix);
      if (parent === prefix) return null;
      missing.push(basename(prefix));
      prefix = parent;
    }
  }
}

// Both sides are resolved: a temp root can itself be a symlink (macOS /tmp),
// and an operand can reach a root through one. A root that cannot be resolved
// is dropped, so a broken root can only deny.
const RM_RESOLVED_SAFE_ROOTS = RM_SAFE_TEMP_ROOTS
  .map((root) => resolveExistingPrefix(root))
  .filter((root) => root !== null);

// Containment is decided on the resolved path (WI#812): the operand as typed
// says nothing about where a symlink under a temp root leads. Words the shell
// would expand into other words are refused before resolving, as is anything
// not absolute — its root would depend on the working directory.
function isSafeRmOperand(operand) {
  if (!operand.startsWith("/") || operand.includes("..")) return false;
  for (const ch of operand) {
    if (RM_SHELL_EXPANDED_CHARS.includes(ch)) return false;
  }
  const resolved = resolveExistingPrefix(operand);
  if (resolved === null) return false;
  return RM_RESOLVED_SAFE_ROOTS.some((root) => resolved === root || resolved.startsWith(`${root}/`));
}

// Join backslash-LF pairs before splitting for rm. A pair is handled in every
// quote state, because the hook cannot tell a real quote from an apostrophe
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
  const rest = match[1];
  // Bash splits words on space and tab here, not JS's broader whitespace set.
  const words = rest.split(/[ \t]+/);
  const endOfOptions = words.indexOf("--");
  const optionWords = endOfOptions === -1 ? words : words.slice(0, endOfOptions);
  // Keep the existing conservative flag checks as well as GNU abbreviations.
  if (!/(^|[ \t])-[a-zA-Z]*[rRfF]|--recursive|--force/.test(rest) &&
      !optionWords.some(isRmLongDeleteOption)) return null;
  const operands = rmOperands(words, endOfOptions);
  if (operands.length === 0) return "rm -r/-f without an explicit operand";
  if (!operands.every(isSafeRmOperand)) {
    return `recursive/forced rm outside ${RM_SAFE_TEMP_ROOTS.join(", ")}: ${operands.join(" ").slice(0, 80)}`;
  }
  return null;
}

const DENY_RULES = [
  {
    name: "privilege escalation",
    pattern: /(^|[\s;&|(])(sudo|doas)\b/,
  },
  {
    name: "power/disk destruction command",
    pattern:
      /\b(shutdown|halt|poweroff|reboot|mkfs(\.[a-z0-9]+)?|wipefs|bcdedit|diskpart|fdisk|sfdisk|parted)\b/,
  },
  {
    name: "fork bomb",
    // Classic bash fork bomb shapes: `:(){ :|:& };:`, `f(){ f|f& }; f`, and
    // equivalents — a function definition whose body pipes itself into
    // itself in the background.
    pattern: /\([^)]*\)\s*\{[^}]*\|[^}]*&\s*\}/,
  },
  {
    name: "remote content piped into a shell or interpreter",
    pattern:
      /\b(curl|wget|fetch|nc|ncat|netcat)\b[^|]*\|\s*(sudo\s+)?(ba|z|da|a|k)?sh\b|\b(curl|wget)\b[^|]*\|\s*python3?\b/,
  },
  {
    name: "recursive mode/ownership change on system roots",
    pattern:
      /\bch(mod|own|grp)\b[^;&|]*(-R|--recursive)[^;&|]*\s+\/(etc|usr|var|home|root|boot|bin|sbin|lib|lib64|opt|srv)(\/|\s|$)/,
  },
  {
    name: "direct block-device write",
    pattern: />\s*\/dev\/(sd|vd|nvme|hd|mmcblk|loop)|\bof=\/dev\/(sd|vd|nvme|hd|mmcblk|loop)/,
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

function evaluate(command) {
  if (command.length > MAX_COMMAND_CHARS) {
    return `command exceeds ${MAX_COMMAND_CHARS} chars`;
  }
  // Structural rules (fork bombs) see the whole command: segment splitting
  // on ;|& would shred the very shape they match.
  const wholeCommandDenial = checkDenyRules(command);
  if (wholeCommandDenial) return wholeCommandDenial;
  // Keep those rules on the original text. Only rm needs the joined view.
  for (const segment of commandSegments(command)) {
    const denied = checkDenyRules(segment);
    if (denied) return denied;
  }
  for (const [lineBreak, quotedLineBreak] of [["", ""], [" ", " "], ["", " "]]) {
    const joined = joinLineContinuations(command, lineBreak, quotedLineBreak);
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
