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
// (power/privilege/disk destruction, fork bombs, pipe-to-shell) plus
// worker-lane specifics (SSH-agent socket use). It is deliberately NOT a
// general command parser: everything it cannot positively classify as safe
// within its ruleset is allowed unless it matches a deny rule, EXCEPT
// unparseable input, which denies (fail closed at the transport layer).

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

function isSafeRmOperand(operand) {
  if (operand.length === 0 || operand.startsWith("$") || operand.includes("..")) return false;
  if (operand.includes("*") || operand.includes("?") || operand.includes("[")) return false;
  return RM_SAFE_TEMP_ROOTS.some((root) => operand === root || operand.startsWith(`${root}/`));
}

// Join backslash-LF pairs before splitting for rm. A backslash-LF pair is
// dropped in every quote state, because the hook cannot tell a real quote from
// an apostrophe the shell reads as plain text (a heredoc body, `$'...'`).
// Joining too much only merges lines, and an rm is still found in the merged
// line; not joining can hide a continued rm. Escaped backslashes and
// backslashes in comments do not continue a shell line.
function joinLineContinuations(command) {
  if (!command.includes("\\\n")) return command;
  let joined = "";
  let quote = "";
  let atWordStart = true;
  for (let i = 0; i < command.length; i++) {
    const ch = command[i];
    if (ch === "\\" && i + 1 < command.length) {
      if (command[i + 1] === "\n") {
        i++;
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
  for (const segment of commandSegments(joinLineContinuations(command))) {
    const denied = checkRm(segment);
    if (denied) return denied;
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
