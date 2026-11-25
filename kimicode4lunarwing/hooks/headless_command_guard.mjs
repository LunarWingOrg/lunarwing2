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
const MAX_SHELL_BODY_DEPTH = 4;
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

/** Read an ordinary literal heredoc delimiter, without performing expansions. */
function heredocRedirect(text, i) {
  if (!text.startsWith("<<", i) || text[i - 1] === "<" || text[i + 2] === "<") return null;
  const stripTabs = text[i + 2] === "-";
  const word = /[ \t]*((?:'(?:[^'\n]*)'|"(?:\\[^\n]|[^"\\\n])*"|\\[^\n]|[^ \t\n;|&()<>'"\\])+)/y;
  word.lastIndex = i + (stripTabs ? 3 : 2);
  const match = word.exec(text);
  if (!match) return null;
  const delimiter = match[1].replace(/'([^']*)'|"((?:\\.|[^"\\])*)"|\\(.)/g,
    (_match, single, double, escaped) => single ?? double?.replace(/\\(["\\$`])/g, "$1") ?? escaped);
  return { delimiter, stripTabs, end: word.lastIndex };
}

// WI#889: a redirection may stand anywhere in a simple command, and the words
// after it are still that command's arguments (`man bash`, REDIRECTION). The
// reader below blanks every redirection out of the command line and keeps its
// targets in a list, so a rule reads the words bash runs and a rule about a
// target still sees it. The forms are `[n]<word`, `[n]>word`, `[n]>|word`,
// `[n]>>word`, `&>word`, `>&word`, `&>>word`, `[n]<<[-]word`, `[n]<<<word`,
// `[n]<&word`, `[n]>&word` and `[n]<>word`, each also with `{varname}` in
// place of `n`, and with or without blanks before the target.
const REDIRECT_WORD_BREAK = " \t\n;|&()<>";

/** The redirection operator at `i`, or "" when the character starts none. */
function redirectOperatorAt(text, i) {
  const ch = text[i];
  if (ch === "&") {
    if (text[i + 1] !== ">") return "";
    return text[i + 2] === ">" ? "&>>" : "&>";
  }
  if (ch === ">") {
    // `>(…)` is a process substitution, not a redirection.
    if (text[i + 1] === "(") return "";
    if (text[i + 1] === ">") return ">>";
    if (text[i + 1] === "|") return ">|";
    if (text[i + 1] === "&") return ">&";
    return ">";
  }
  if (ch === "<") {
    if (text[i + 1] === "(") return "";
    if (text[i + 1] === "&") return "<&";
    if (text[i + 1] === ">") return "<>";
    if (text[i + 1] !== "<") return "<";
    if (text[i + 2] === "<") return "<<<";
    if (text[i + 2] === "-") return "<<-";
    return "<<";
  }
  return "";
}

/**
 * The start of the whole redirection token: bash reads a run of digits (or a
 * `{varname}`) that forms a word of its own immediately before the operator as
 * the descriptor, so `a2>f` writes fd 1 from the word `a2` while `2>f` writes
 * fd 2. The digits must be the whole word, which is why the character before
 * them has to be a word break.
 */
function redirectPrefixStart(text, operatorIndex) {
  let start = operatorIndex;
  let i = operatorIndex - 1;
  if (text[i] === "}") {
    let open = i - 1;
    while (open >= 0 && /[A-Za-z0-9_]/.test(text[open])) open--;
    if (open >= 0 && text[open] === "{" && open < i - 1 &&
        (open === 0 || REDIRECT_WORD_BREAK.includes(text[open - 1]))) {
      start = open;
      i = open - 1;
    }
  }
  let digits = 0;
  while (i >= 0 && text[i] >= "0" && text[i] <= "9") {
    i--;
    digits++;
  }
  if (digits > 0 && (i < 0 || REDIRECT_WORD_BREAK.includes(text[i]))) start = i + 1;
  return start;
}

/** The span of the word a redirection takes as its target, blanks skipped. */
function redirectWordSpan(text, scan, from) {
  let i = from;
  while (i < text.length && (text[i] === " " || text[i] === "\t")) i++;
  const start = i;
  while (i < text.length) {
    const close = scan.substitutions.get(i);
    if (close !== undefined) {
      i = close + 1;
      continue;
    }
    if (scan.states[i] === QUOTE_UNQUOTED && REDIRECT_WORD_BREAK.includes(text[i])) break;
    i++;
  }
  return { start, end: Math.min(i, text.length) };
}

/**
 * The command line with every redirection blanked out, the targets it wrote
 * or read, and the here-string bodies, which bash feeds to the command as its
 * input and which the listener rule therefore reads as commands. The blanks
 * keep every other offset where it was. `|&` is bash's shorthand for
 * `2>&1 |` (`man bash`, Pipelines), so its `&` is blanked and the bar kept.
 */
function stripRedirections(text, escapes = true) {
  const scan = quoteScan(text, escapes);
  const chars = text.split("");
  const targets = [];
  const hereStrings = [];
  for (let i = 0; i < text.length; i++) {
    const close = scan.substitutions.get(i);
    if (close !== undefined) {
      i = close;
      continue;
    }
    if (scan.states[i] !== QUOTE_UNQUOTED) continue;
    if (text[i] === "|" && text[i + 1] === "&") {
      chars[i + 1] = " ";
      i++;
      continue;
    }
    const operator = redirectOperatorAt(text, i);
    if (operator === "") continue;
    const start = redirectPrefixStart(text, i);
    let end = i + operator.length;
    if (operator === "<<" || operator === "<<-") {
      const redirect = heredocRedirect(text, i);
      if (redirect) end = redirect.end;
    } else {
      const span = redirectWordSpan(text, scan, i + operator.length);
      if (span.end > span.start) end = span.end;
      const target = text.slice(span.start, span.end);
      if (operator === "<<<") hereStrings.push(stripShellQuotes(target));
      else if (target.length > 0) {
        targets.push({ target, write: operator !== "<" && operator !== "<&" });
      }
    }
    for (let j = start; j < end; j++) chars[j] = " ";
    i = end - 1;
  }
  const view = chars.join("");
  return { view, targets, hereStrings };
}

let redirectScanText = null;
let redirectScanEscapes = true;
let redirectScanResult = null;

function redirectScan(text, escapes = true) {
  if (text === redirectScanText && escapes === redirectScanEscapes) return redirectScanResult;
  const result = stripRedirections(text, escapes);
  redirectScanText = text;
  redirectScanEscapes = escapes;
  redirectScanResult = result;
  return result;
}

/** The command line with its redirections taken out. */
function redirectionFreeView(text, escapes = true) {
  return redirectScan(text, escapes).view;
}

/**
 * The index after the redirection whose operator is at `i`, or null when `i`
 * starts none. A redirection glues to the word before it (`a&>f`), so the
 * digits or `{varname}` immediately before the operator are part of the token
 * and must not end the word being read.
 */
function redirectionSkip(text, i) {
  const operator = redirectOperatorAt(text, i);
  if (operator === "") return null;
  const scan = quoteScan(text);
  const start = redirectPrefixStart(text, i);
  let end = i + operator.length;
  if (operator === "<<" || operator === "<<-") {
    const redirect = heredocRedirect(text, i);
    if (redirect) end = redirect.end;
  } else {
    const span = redirectWordSpan(text, scan, i + operator.length);
    if (span.end > span.start) end = span.end;
  }
  return end > start ? end : null;
}

/**
 * The redirection token that starts at the word boundary `i`, with its
 * `[n]`/`{varname}` descriptor prefix, or null when `i` starts none. Only a
 * token at a word start carries a descriptor: bash reads `a2>f` as the word
 * `a2` and a plain `>f`, because the digits are not a word of their own.
 */
function redirectTokenSpan(text, i) {
  let operatorIndex = i;
  if (text[i] >= "0" && text[i] <= "9") {
    while (operatorIndex < text.length && text[operatorIndex] >= "0" &&
        text[operatorIndex] <= "9") operatorIndex++;
  } else if (text[i] === "{") {
    const name = /^\{[A-Za-z_][A-Za-z0-9_]*\}/.exec(text.slice(i, i + 64));
    if (name === null) return null;
    operatorIndex = i + name[0].length;
  }
  const end = redirectionSkip(text, operatorIndex);
  return end === null ? null : { start: i, end };
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
/**
 * Split a command tail into bash words (space and tab only). A redirection is
 * not a word, so `tee 2>&1 /dev/sda1` has the one operand `/dev/sda1`, and
 * `find /tmp/x 2>&1 -delete` the one start path `/tmp/x` (WI#889). The
 * targets a rule needs are read from `redirectScan`, not from this list.
 */
function argumentWords(rest) {
  return redirectionFreeView(rest).split(/[ \t]+/).filter((word) => word.length > 0);
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
      if (isQuoteEscape(command, i, quote)) {
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
      atWordStart = SHELL_TOKEN_BOUNDARIES.includes(ch);
    }
    joined += ch;
  }
  return joined;
}

/** The word view bash builds: one escaping level dropped, quote characters removed. */
function shellWordView(command) {
  return command.replace(/\\(.)/g, "$1").replace(/['"]/g, "");
}

const QUOTED_LITERAL_WORD = /^[^\s;&|`$\\]*$/;

/**
 * The words bash hands the program: one shell quoting level removed from a
 * quoted span that is a plain literal word. Unlike `shellWordView` this does
 * not touch a span that carries whitespace, a clause separator or an
 * expansion, so the span stays one word and the clause structure the readers
 * walk is unchanged (`find /tmp -name "a;b" | xargs rm` keeps its pipe), and
 * an interpreter operand keeps the quotes its own language reads
 * (`perl -e 'unlink("/etc/passwd")'`). A quoted or escaped verb reads as the
 * plain word bash runs (`"tee" /dev/sda1`, `\find /app -delete`), which is
 * the spelling the raw-shape family was missing (WI#873).
 */
function shellWordLevelView(text) {
  const states = quoteStates(text);
  let view = "";
  let wordStart = 0;
  let hasEmptySpan = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    const state = states[i];
    if (state === QUOTE_UNQUOTED && SHELL_TOKEN_BOUNDARIES.includes(ch)) {
      // A splice contributes no characters, but an empty word is an argument.
      if (hasEmptySpan && view.length === wordStart) view += '""';
      view += ch;
      wordStart = view.length;
      hasEmptySpan = false;
      continue;
    }
    if (isEscapedState(state) && ch === "\\") {
      const escaped = text[++i] ?? "";
      if (escaped !== "\n") view += escaped;
      continue;
    }
    if (state !== QUOTE_OPEN || (ch !== "'" && ch !== '"')) {
      if (state === QUOTE_UNQUOTED && ch === "$" && text[i + 1] === "$") {
        // The PID parameter is not a locale quote opener.
        view += "$$";
        i++;
        continue;
      }
      // The `$` of either a $'…' or a $"…" opener is not part of its word.
      const quotePrefix = ch === "$" && states[i + 1] === QUOTE_OPEN &&
        ((state === QUOTE_OPEN && text[i + 1] === "'") ||
          (state === QUOTE_UNQUOTED && text[i + 1] === '"'));
      if (!quotePrefix) view += ch;
      continue;
    }
    let close = i + 1;
    while (close < text.length && states[close] !== QUOTE_CLOSE) close++;
    const body = text.slice(i + 1, close);
    if (close < text.length && QUOTED_LITERAL_WORD.test(body)) {
      if (body === "") hasEmptySpan = true;
      else view += body;
    } else {
      view += text.slice(i, close + 1);
    }
    i = close;
  }
  return hasEmptySpan && view.length === wordStart ? view + '""' : view;
}

// A command word that carries an unquoted parameter expansion or a command
// substitution can be the plain verb when the variable is empty or unset
// (`${EMPTY}tee /dev/sda1` runs `tee`), and the guard does not evaluate the
// shell (WI#883). `RM_DYNAMIC_WORD` is the same predicate `checkRm` already
// refuses with.
const GLUED_EXPANSION = /[$`]/;

/**
 * The words of the text with every unquoted parameter expansion, command
 * substitution and backtick removed, so a verb glued to an expansion reads as
 * the verb bash may run (WI#883): `${EMPTY}tee /dev/sda1` reads as
 * `tee /dev/sda1`, `t${EMPTY}ee` as `tee`, and `$EMPTY"tee"` as `tee`. A
 * word made only of an expansion becomes empty, which keeps `$EDITOR` and
 * `"$HOME"/bin/tool` as the ordinary words they are. An expansion inside
 * quotes is not expanded here: the quoted spelling is the word's own text,
 * and a raw-shape reader sees it in the quoted views.
 *
 * A `${IFS…}` expansion is a word separator, so it becomes a space rather
 * than disappearing, matching the reading `checkRm` already uses. Only
 * unquoted expansions are removed; the view is an additional reading that
 * can add denials, never remove one.
 *
 * With `defaults`, an operator expansion is read as the word bash may run in
 * its place instead of being dropped (AME-3 of WI#883): `${X:-tee} /dev/sda1`
 * reads as `tee /dev/sda1`, `t${X:-e}e` as `tee`, and a double-quoted
 * `"${X:-tee}"` as `"tee"`. Both readings are real — the variable's own value
 * or the operator's word — so the caller checks this view beside the plain
 * one, never in its place.
 */
function gluedExpansionView(text, defaults = false) {
  if (!GLUED_EXPANSION.test(text)) return text;
  const { states, substitutions } = quoteScan(text);
  const braceEnds = defaults ? parameterExpansionEnds(text) : null;
  let view = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    const state = states[i];
    if (state === QUOTE_UNQUOTED || state === QUOTE_ESCAPED) {
      const close = substitutions.get(i);
      if (close !== undefined) {
        // A substitution body is a command of its own; it cannot be the
        // verb this word names, so its unknown output is dropped.
        i = close;
        continue;
      }
      const ifs = ifsSeparatorLength(text, i);
      if (ifs > 0) {
        view += " ";
        i += ifs - 1;
        continue;
      }
      if (ch === "$") {
        const next = text[i + 1] ?? "";
        if (/[A-Za-z_]/.test(next)) {
          // `$NAME` is a parameter; `$$` is the PID and `$'…'`/`$"…"` are
          // quote openers, which the shared reader already marks.
          i++;
          while (i + 1 < text.length && /[A-Za-z0-9_]/.test(text[i + 1])) i++;
          continue;
        }
        if (next === "{") {
          const end = defaults ? braceEnds.get(i) ?? -1 : text.indexOf("}", i + 2);
          if (end !== -1) {
            if (defaults) {
              const word = parameterOperatorWord(text.slice(i + 2, end));
              if (word !== null) view += gluedExpansionView(word, true);
            }
            i = end;
            continue;
          }
        }
        if (/[0-9@*#?$!-]/.test(next)) {
          i++;
          continue;
        }
      }
    } else if (defaults && state === QUOTE_DOUBLE && ch === "$" && text[i + 1] === "{") {
      // Inside double quotes the expansion still happens, and its word is
      // still the text bash may run; the surrounding quotes stay in the view.
      const end = braceEnds.get(i) ?? -1;
      const word = end === -1 ? null : parameterOperatorWord(text.slice(i + 2, end));
      if (word !== null) {
        view += gluedExpansionView(word, true);
        i = end;
        continue;
      }
    }
    view += ch;
  }
  return view;
}

// The operators of `${NAME<op>WORD}` after which bash may use WORD in place
// of the expansion (bash manual, Shell Parameter Expansion): use a default
// (`:-`, `-`), assign a default (`:=`, `=`), use an alternate value (`:+`,
// `+`). `:?`/`?` only print WORD, and every other form (`#`, `%`, `/`, `^`,
// `,`, `@`, a `:offset`) transforms the variable's own value.
const PARAMETER_WORD_OPERATORS = [":-", ":=", ":+", "-", "=", "+"];

/**
 * The `}` that closes each `${` of the text, by the index of its `$`. A `}`
 * closes the innermost open `${`, so nested expansions pair as bash pairs
 * them; an unclosed `${` has no entry. One pass, so a command full of
 * expansions is read in linear time (the 2 s analysis deadline).
 */
function parameterExpansionEnds(text) {
  const ends = new Map();
  const open = [];
  for (let i = 0; i < text.length; i++) {
    if (text[i] === "\\") {
      i++;
    } else if (text[i] === "$" && text[i + 1] === "{") {
      open.push(i);
      i++;
    } else if (text[i] === "}" && open.length > 0) {
      ends.set(open.pop(), i);
    }
  }
  return ends;
}

/** The WORD of a `${NAME<op>WORD}` body when bash may substitute it, else null. */
function parameterOperatorWord(body) {
  const name = /^!?(?:[A-Za-z_][A-Za-z0-9_]*(?:\[[^\]]*\])?|[0-9]+|[@*#?$!-])/.exec(body);
  if (name === null) return null;
  const rest = body.slice(name[0].length);
  for (const operator of PARAMETER_WORD_OPERATORS) {
    if (rest.startsWith(operator)) return rest.slice(operator.length);
  }
  return null;
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
  return splitOutsideQuotes(command, isClauseTerminator, escapes);
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
  // A redirection is neither a clause end nor an operand (WI#889): the
  // clause is read with its redirections taken out, so `rm 2>&1 -rf /x` is
  // one command and `rm -rf /tmp/x 2> /dev/null` names one path.
  segment = redirectionFreeView(segment);
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
// The names one of these interpreters ships under, with the version suffix a
// name may carry (`python3.11`, `perl5.36.0`). The perl version changes with
// the base image, so no one version number is held here (WI#890).
const ONE_LINER_NAMES = "(python3?|perl5(?:\\.\\d+)+|perl|nodejs|node|ruby)(?:\\.\\d+)?";
// An interpreter may be named by a path of any depth, absolute or relative,
// with `~`, a variable or a quote in it, so the prefix is every non-blank
// character up to the last `/` and the name is the last part of the word
// (WI#890). Options may precede the eval flag (`perl -MFile::Path -e …`), so
// the eval flag is found by walking the words (oneLinerCode).
const ONE_LINER_CALL = new RegExp(
  `(?:^|[\\s;&|()])(?:[^\\s;&|()]*\\/)?${ONE_LINER_NAMES}\\b([^\\n]*)`, "g");
// The letters an interpreter's own option syntax uses for its eval flag
// (python `-c`, perl `-e`/`-E`, node `-e`/`-p`, ruby `-e`), read inside a
// short-option cluster.
const EVAL_FLAG_LETTERS = { python: "c", perl: "eE", node: "ep", ruby: "e" };
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
  for (const { rest } of clauseTails(text, FIND_CALL)) {
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
  const lists = commandLists(text.slice(0, index));
  return lists[lists.length - 1] ?? "";
}

/** The start paths of the last find in a pipeline, or null when there is none. */
function lastFindPaths(pipeline) {
  let paths = null;
  for (const { rest } of clauseTails(pipeline, FIND_CALL)) paths = findStartPaths(rest);
  return paths;
}

/** Deny `xargs rm` unless its feeding find is provably temp-rooted. */
function checkXargsDeletion(text) {
  for (const { match, rest } of clauseTails(text, XARGS_CALL)) {
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
  for (const { rest } of clauseTails(text, RSYNC_CALL)) {
    const words = argumentWords(rest).map(stripShellQuotes);
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

// cp/mv's value-taking options: `-t`/`--target-directory` names the
// destination without being an operand word, and `-S`/`--suffix` takes a
// value that is not a path. A walk that only skips `-`-prefixed words leaves
// the value behind as an operand, so `cp -t /dev/sda1 /tmp/a` read
// `/dev/sda1` as a source and `/tmp/a` as the destination (WI#869).
const COPY_SHORT_ARG = new Set("tS");
const COPY_LONG_ARG = ["target-directory", "suffix"];

/** The value-taking cp/mv long option a name spells, exactly or by prefix. */
function copyLongValueOption(name) {
  if (COPY_LONG_ARG.includes(name)) return name;
  // GNU accepts a unique prefix; `--t` is `--target-directory`.
  return COPY_LONG_ARG.find((option) => option.startsWith(name)) ?? null;
}

/**
 * The path operands of a `cp`/`mv` invocation, and the destination a
 * `-t`/`--target-directory` names without being an operand word (WI#849,
 * WI#869). `-S`/`--suffix` takes a value that is not a path.
 */
function copyOperands(rest) {
  const words = argumentWords(rest).map(stripShellQuotes);
  const operands = [];
  let target;
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (word === "--") {
      operands.push(...words.slice(i + 1));
      break;
    }
    if (word.startsWith("--")) {
      const equals = word.indexOf("=");
      const option = copyLongValueOption(word.slice(2, equals === -1 ? undefined : equals));
      if (option === null) continue;
      const { value, last } = optionValue(equals === -1 ? "" : word.slice(equals + 1), words, i);
      if (option === "target-directory") target = value;
      i = last;
      continue;
    }
    if (word.startsWith("-") && word.length > 1) {
      const cluster = word.slice(1);
      for (let j = 0; j < cluster.length; j++) {
        if (!COPY_SHORT_ARG.has(cluster[j])) continue;
        const { value, last } = optionValue(cluster.slice(j + 1), words, i);
        if (cluster[j] === "t") target = value;
        i = last;
        break;
      }
      continue;
    }
    operands.push(word);
  }
  // A redirection target is not an operand (e.g. `2>/tmp/log`).
  return { operands: operands.filter((operand) => !/^\d*>/.test(operand)), target };
}

/**
 * The path operands of an `mv` invocation: every source and the destination,
 * including a `-t`/`--target-directory` value, which is a destination without
 * being an operand word (WI#849).
 */
function mvOperands(rest) {
  const { operands, target } = copyOperands(rest);
  return target === undefined ? operands : [...operands, target];
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
  for (const { rest } of clauseTails(text, MV_CALL)) {
    const hit = mvOperands(rest).find(mvTouchesProtected);
    if (hit !== undefined) return `rename of a protected tree or a system root: mv ${hit}`;
  }
  for (const { rest } of clauseTails(text, TAR_CALL)) {
    const tar = tarInvocation(rest);
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

// The interpreter options that take the next word as their value. The eval
// flag is found by walking the words, and a walk that only steps over
// `-`-prefixed words ends at a value: `perl -I /tmp -e 'unlink(…)'` read as
// carrying no one-liner, so the deletion rule never ran (WI#869, the same
// shape as `listenerShellBody`). Only options whose value is a separate word
// are listed; an attached value (`-I/tmp`, `-Mstrict`) needs no step, and an
// option whose value can only be attached or is optional (`perl -C`, `-x`,
// `-D`, `-F`; `ruby -T`) is absent because eating the next word would hide a
// following `-e`: `perl -D -e '…'` and `perl -F -e '…'` run the code
// (run-531 ALQ-1).
const INTERPRETER_VALUE_OPTIONS = {
  perl: new Set(["-I", "-M", "-m"]),
  python: new Set(["-X", "-W", "-Q", "-m"]),
  node: new Set([
    "-r", "--require", "--loader", "--experimental-loader", "-C", "--conditions",
    "--input-type",
  ]),
  ruby: new Set(["-I", "-r", "-C", "-E", "--enable"]),
};

/**
 * The language family of an interpreter name (WI#890): the version suffix and
 * the second name a name may carry are dropped, so `perl5.36.0` reads as
 * `perl` and `nodejs` as `node`. The family, not the spelling, chooses the
 * option tables and the deletion readers.
 */
function interpreterFamily(name) {
  if (/^node/.test(name)) return "node";
  if (/^perl/.test(name)) return "perl";
  if (/^ruby/.test(name)) return "ruby";
  return "python";
}

/** The value-option table for an interpreter name, by language family. */
function interpreterValueOptions(name) {
  const family = interpreterFamily(name);
  if (family === "perl") return INTERPRETER_VALUE_OPTIONS.perl;
  if (family === "node") return INTERPRETER_VALUE_OPTIONS.node;
  if (family === "ruby") return INTERPRETER_VALUE_OPTIONS.ruby;
  return INTERPRETER_VALUE_OPTIONS.python;
}

/**
 * The code text of an interpreter invocation, or null when the words after the
 * interpreter carry no eval flag. The eval flag may follow other options
 * (`perl -MFile::Path -e …`, `node --no-warnings -e …`), so the words are
 * walked rather than matched by one regex (WI#832), and an option that takes
 * a separate value is stepped over whole so its value cannot end the walk
 * before the flag (WI#869).
 *
 * The walk is fail-closed (WI#876): a word the table does not name does not
 * end it. An unlisted option with a separate value (`node --env-file /dev/null
 * -e …`, `node --disable-warning X -e …`, `python3 --check-hash-based-pycs
 * always -c …`) used to put its value where the walk expected the flag, so the
 * code was never read. Reading a positional word as if a later flag were the
 * interpreter's own can only add denials, which is the accepted cost; missing
 * the flag is not.
 *
 * The code may sit in the flag word itself (WI#890): a short-option cluster
 * is walked letter by letter as the interpreter reads it, so `-c"…"`, `-Ic"…"`
 * and `-le'…'` hand over the rest of the word, and `--eval=…` hands over the
 * text after the `=`. Where the walk cannot tell whether a word is the eval
 * flag, it keeps walking rather than returning, so doubt cannot allow.
 */
function oneLinerCode(rest, name) {
  const values = interpreterValueOptions(name);
  const family = interpreterFamily(name);
  const letters = EVAL_FLAG_LETTERS[family];
  // Perl's numeric -0 and -l modifiers can precede more flags in the cluster.
  const clusterPattern = family === "perl" ? /^-([a-zA-Z0-9]+)(.*)$/ : /^-([a-zA-Z]+)(.*)$/;
  let i = 0;
  while (i < rest.length) {
    while (i < rest.length && /[ \t]/.test(rest[i])) i++;
    const start = i;
    while (i < rest.length && !/[ \t]/.test(rest[i])) i++;
    const word = rest.slice(start, i);
    if (word.length === 0) return null;
    if (/^(?:--eval|--print)$/.test(word)) return rest.slice(i);
    // `--eval=…`/`--print=…` carry the code in the word itself (WI#890).
    if (/^--(?:eval|print)=/.test(word)) return word.slice(word.indexOf("=") + 1) + rest.slice(i);
    // A long value option's separate word is stepped over (WI#869).
    if (values.has(word)) {
      while (i < rest.length && /[ \t]/.test(rest[i])) i++;
      while (i < rest.length && !/[ \t]/.test(rest[i])) i++;
      continue;
    }
    // A short-option cluster is read the way the interpreter reads it: a
    // letter that takes a value consumes the rest of the word, and the eval
    // letter hands it the rest of the word as code, or the next word when
    // nothing follows (`-c`, `-Ic`, `-le'…'`, `-c"…"`). A cluster that
    // carries no eval letter is stepped over, and a value option with no
    // attached text takes the next word (WI#890).
    const cluster = clusterPattern.exec(word);
    if (cluster !== null) {
      const flags = cluster[1];
      for (let at = 0; at < flags.length; at++) {
        const letter = flags[at];
        if (letters.includes(letter)) {
          const tail = at === flags.length - 1 ? cluster[2] : flags.slice(at + 1) + cluster[2];
          return tail === "" ? rest.slice(i) : tail + rest.slice(i);
        }
        if (values.has(`-${letter}`)) {
          if (at === flags.length - 1 && cluster[2] === "") {
            while (i < rest.length && /[ \t]/.test(rest[i])) i++;
            while (i < rest.length && !/[ \t]/.test(rest[i])) i++;
          }
          break;
        }
      }
      continue;
    }
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
    const code = oneLinerCode(invocation[2], invocation[1]);
    if (code === null) continue;
    const aliases = sinkAliases(code);
    if (interpreterFamily(invocation[1]) === "perl") {
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
  for (const { rest } of clauseTails(text, TEE_CALL)) {
    for (const word of argumentWords(rest)) {
      if (word.startsWith("-")) continue;
      const target = deviceTarget(word);
      if (target !== null) return `direct block-device write: tee ${target.slice(0, 80)}`;
    }
  }
  for (const { rest } of clauseTails(text, CP_CALL)) {
    // `-t`/`--target-directory` names the destination without being an
    // operand word, so `cp -t /dev/sda1 /tmp/a` must read it as the target
    // (WI#869).
    const { operands, target } = copyOperands(rest);
    const destination = target ?? operands[operands.length - 1];
    const device = destination === undefined ? null : deviceTarget(destination);
    if (device !== null) return `direct block-device write: cp ${device.slice(0, 80)}`;
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
const STAGE_PREFIXES = ["sudo", "exec", "busybox"];
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
  const states = quoteStates(text);
  let view = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    const state = states[i];
    // The shared reader decides what a backslash escapes, so a `\"` inside
    // double quotes is the escape bash reads and a `\;` is literal text.
    if (isEscapedState(state)) {
      view += ch + text[++i];
      continue;
    }
    if (state === QUOTE_SINGLE || state === QUOTE_OPEN || state === QUOTE_CLOSE) {
      view += ch;
    } else if (ch === "`") {
      view += " ";
    } else if ((ch === "$" || ch === "<" || ch === ">") && text[i + 1] === "(") {
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
  for (const { rest } of clauseTails(segment, ROOT_CHANGE_CALL)) {
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
 * The sorted indices of the unquoted clause terminators in `text`, computed in
 * one pass and cached for the text a check is walking: the reader is called
 * once per command word, and rescanning the rest of a 16 KB command from each
 * of them took the analysis past its deadline (WI#856).
 */
let clauseScanText = null;
let clauseScanEnds = [];

function clauseEnds(text) {
  if (text === clauseScanText) return clauseScanEnds;
  // A redirection's own operator characters are not control operators: the
  // `&` of `2>&1`, `&>`, `>&`, `<&` and the bar of `>|` end no clause, and
  // `|&` keeps its bar (WI#889). The redirection-free view has the same
  // offsets, so the indices stay valid for `text`.
  const view = redirectionFreeView(text);
  const ends = [];
  for (const i of unquotedOffsets(view)) {
    if (isClauseTerminator(view[i])) ends.push(i);
  }
  clauseScanText = text;
  clauseScanEnds = ends;
  return ends;
}

/**
 * The text of the command clause that starts at `from`: it runs to the first
 * control operator or newline in the enclosing scope, outside quotes and
 * substitutions. An argument that carries a separator cannot hide the rest
 * of its command (WI#856, WI#867).
 */
function clauseText(text, from) {
  const ends = clauseEnds(text);
  let low = 0;
  let high = ends.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if (ends[middle] < from) low = middle + 1;
    else high = middle;
  }
  return text.slice(from, low < ends.length ? ends[low] : text.length);
}

/**
 * The rest of the clause after each command-word match, read once per clause.
 * A clause is one simple command, so the first match's tail already contains
 * every later match in it; re-reading and re-parsing that tail for each word
 * made a 16 KB command of repeated verbs take the analysis past its deadline
 * (WI#856). Matches arrive in ascending order, so one running clause end is
 * enough to spot a repeat.
 */
function clauseTails(text, pattern) {
  const tails = [];
  let clauseStop = -1;
  for (const match of text.matchAll(pattern)) {
    const from = match.index + match[0].length;
    if (from <= clauseStop) continue;
    const rest = clauseText(text, from);
    clauseStop = from + rest.length;
    tails.push({ match, rest });
  }
  return tails;
}

/**
 * The bash word at `from`, with its quoting removed, and the index after it.
 * A word ends at space, tab or an unquoted clause terminator, so a quoted
 * option value that carries a space or a separator reads as one word
 * (WI#856). The scan starts with a fresh quote context at `from` and stops at
 * the word's end, so it costs the word's length, not the text's: a caller
 * reading a tail that begins inside a double-quoted span (a punctuation view)
 * still reads the word bash would build there. A substitution opens a level of
 * its own (WI#867).
 */
function bashWord(text, from) {
  let i = from;
  let word = "";
  const levels = [{ quote: "", closer: "", depth: 0 }];
  while (i < text.length) {
    const level = levels[levels.length - 1];
    const quote = level.quote;
    const ch = text[i];
    if (isQuoteEscape(text, i, quote)) {
      word += text[i + 1];
      i += 2;
      continue;
    }
    if (ch === "`" && (quote === "" || quote === '"')) {
      const close = backtickEnd(text, i, true);
      word += text.slice(i + 1, close);
      i = Math.min(close + 1, text.length);
      continue;
    }
    if (quote !== "") {
      if (ch === quote.at(-1)) {
        level.quote = "";
        i++;
        continue;
      }
      if (quote === '"' && ch === "$" && text[i + 1] === "(") {
        levels.push({ quote: "", closer: ")", depth: 0 });
        i += 2;
        continue;
      }
      word += ch;
      i++;
      continue;
    }
    if (level.closer !== "") {
      if (ch === level.closer) {
        if (level.depth > 0) level.depth--;
        else levels.pop();
        i++;
        continue;
      }
      if (ch === "(") {
        level.depth++;
        i++;
        continue;
      }
    }
    if ((ch === "$" || ch === "<" || ch === ">") && text[i + 1] === "(") {
      levels.push({ quote: "", closer: ")", depth: 0 });
      i += 2;
      continue;
    }
    // A redirection is not a word: `git 2>/dev/null reset` reads `reset` as
    // the verb, and `rm 2>&1 -rf /x` reads `-rf` and `/x` (WI#889). The
    // scan's quote state says where the operator and its target end. At a
    // word start the token also carries its `[n]`/`{varname}` descriptor
    // (`2>/dev/null`), which belongs to the redirection, not to a word. The
    // test comes before the separator one because `&>f` starts with `&`.
    if (levels.length === 1) {
      const skip = word.length === 0 ? redirectTokenSpan(text, i)?.end ?? null
        : redirectionSkip(text, i);
      if (skip !== null) {
        i = skip;
        continue;
      }
    }
    if (ch === " " || ch === "\t" || isClauseTerminator(ch)) {
      // A separator inside a substitution body is part of the word the
      // substitution sits in; only the outermost level ends it.
      if (levels.length === 1) break;
      word += ch;
      i++;
      continue;
    }
    const opener = openingQuote(text, i);
    if (opener !== "") {
      level.quote = opener;
      i += opener.length;
      continue;
    }
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
    const start = i;
    const { word, end } = bashWord(text, i);
    i = end;
    if (word.length === 0) {
      // A redirection standing where the verb would be is not the verb
      // (`git 2>/dev/null reset --hard`): the word reader steps over it, so
      // the walk continues at the word after it (WI#889).
      if (end > start) continue;
      break;
    }
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

// The reason a git alias or include definition is refused (WI#867). It is a
// refusal of its own, not a discard: an alias body is an arbitrary verb, and
// an include pulls in a file that can define one, so the guard refuses the
// definition because it cannot evaluate what the alias runs.
const GIT_ALIAS_REASON =
  "git alias or include definition (the guard does not evaluate alias bodies)";

/**
 * The reason for a git alias or include definition (WI#842, WI#867). The
 * refusal is not a discard: an alias body is an arbitrary verb the guard never
 * reads, so it refuses the definition itself and says so. Returns the reason
 * text, or null when the segment defines neither.
 */
function checkGitAliasDefinition(segment) {
  if (hasGitAliasEnv(segment)) return GIT_ALIAS_REASON;
  for (const match of segment.matchAll(GIT_CALL)) {
    if (gitAliasDefinition(segment, match.index + 3)) return GIT_ALIAS_REASON;
  }
  return null;
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
  for (const match of segment.matchAll(GIT_CALL)) {
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

// `git submodule foreach <command>` and `git rebase --exec/-x <command>` hand
// their operand to `sh -c`, so the body is a command line of its own even
// though no shell word carries it (WI#895). git reads the body from its own
// argument list, so the guard must walk the same options git walks: a
// separate or attached value (`--exec=…`, `-x…`), a value at the end of a
// cluster (`-ix …`), and `foreach`'s own leading options.

// rebase's long options that take the next word as their value.
const GIT_REBASE_VALUE_OPTIONS = new Set([
  "--onto", "--whitespace", "--strategy", "--strategy-option", "--trailer", "--empty",
]);
// rebase's short options that take the next word when they end a cluster.
const GIT_REBASE_SHORT_VALUES = "sXC";

/** The bodies a `git rebase` invocation hands to `sh -c`, in argument order. */
function gitRebaseExecBodies(words) {
  const bodies = [];
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    // After `--` every word is an operand, so git reads no more options.
    if (word === "--") break;
    if (word.startsWith("--") && word.length > 2) {
      const equals = word.indexOf("=");
      const name = word.slice(2, equals === -1 ? undefined : equals);
      // `--no-exec` turns the option off; the next word is not a body.
      if (name.startsWith("no-")) continue;
      // git resolves an unambiguous prefix (`--ex`, `--exe`), so every
      // prefix of `exec` names it here, which can only add denials.
      if ("exec".startsWith(name)) {
        if (equals !== -1) bodies.push(word.slice(equals + 1));
        else if (i + 1 < words.length) bodies.push(words[++i]);
        continue;
      }
      if (equals === -1 && GIT_REBASE_VALUE_OPTIONS.has(name)) i++;
      continue;
    }
    if (word.startsWith("-") && word.length > 1) {
      const cluster = word.slice(1);
      for (let j = 0; j < cluster.length; j++) {
        if (cluster[j] === "x") {
          // The rest of the cluster is the body when it carries one
          // (`-x'kimi server'`); otherwise the next word is (`-ix …`).
          if (j + 1 < cluster.length) bodies.push(cluster.slice(j + 1));
          else if (i + 1 < words.length) bodies.push(words[++i]);
          break;
        }
        if (GIT_REBASE_SHORT_VALUES.includes(cluster[j])) {
          if (j === cluster.length - 1) i++;
          break;
        }
      }
    }
    // An operand (`HEAD~1`, a branch) does not end git's option scan.
  }
  return bodies;
}

/** The bodies a `git submodule foreach` invocation hands to `sh -c`. */
function gitSubmoduleForeachBodies(words) {
  let i = 0;
  // `git submodule [--quiet] foreach …`.
  while (i < words.length && words[i].startsWith("-")) i++;
  if (words[i] !== "foreach") return [];
  // `foreach` reads its own options (`-q`, `--recursive`) before the command
  // operand; every word after the first operand is part of the body.
  for (i++; i < words.length && words[i].startsWith("-"); i++);
  return i < words.length ? [words.slice(i).join(" ")] : [];
}

/** Return Git's shell bodies, or null if this is not a recognized carrier. */
function gitShellBodies(words, start) {
  let i = start;
  while (i < words.length && words[i].startsWith("-")) {
    i += GIT_VALUE_OPTIONS.has(words[i]) ? 2 : 1;
  }
  const verb = listenerWord(words[i] ?? "");
  if (verb === "rebase") return gitRebaseExecBodies(words.slice(i + 1));
  if (verb === "submodule" || verb === "submodule--helper") {
    return gitSubmoduleForeachBodies(words.slice(i + 1));
  }
  return null;
}

/** The command word of a pipeline stage, after env/exec/busybox prefixes. */
function commandBasename(word) {
  return stripShellQuotes(word).replace(/^[({]+/, "").replace(/[)}]+$/, "").split("/").pop() ?? "";
}

/**
 * The words of one pipeline stage, read as the listener reader reads a
 * command line: quotes keep a word together and an unquoted brace group
 * expands. A brace expansion too large to read falls back to the naive
 * split, which can only add words.
 */
function stageWords(stage) {
  const words = [];
  for (const segment of listenerSegments(stage)) {
    const list = listenerWordList(segment);
    if (list === null) return argumentWords(stage).map(stripShellQuotes);
    words.push(...list);
  }
  return words;
}

/**
 * The command word of a word list, after env/exec/busybox prefixes. `env`'s
 * options are stepped over, and an `env -S`/`--split-string` value is one
 * word to the shell but a command line to env, so the words env runs replace
 * it (WI#872). Invalid invocations have no stage, and the listener rule
 * separately refuses unverifiable strings. A stage the reader cannot resolve
 * — an unreadable `env -S` string or `env` nesting past the depth bound —
 * carries `unverifiable`, because an empty name would read as "runs nothing"
 * and hide the command (WI#885).
 */
function stageCommandFrom(words, depth) {
  let i = 0;
  while (i < words.length) {
    const name = commandBasename(words[i]);
    if (name === "env") {
      const command = envCommandWords(words, i + 1);
      if (command === null) return { name: "", word: "", args: [], unverifiable: false };
      if (command.unverifiable || depth >= MAX_SHELL_BODY_DEPTH) {
        return { name: "", word: "", args: [], unverifiable: true };
      }
      return stageCommandFrom(command.words, depth + 1);
    }
    const isPrefix = STAGE_PREFIXES.includes(name) || /^[A-Za-z_]\w*=/.test(words[i]);
    if (!isPrefix) break;
    i++;
  }
  const word = words[i] ?? "";
  return { name: commandBasename(word), word, args: words.slice(i + 1), unverifiable: false };
}

/**
 * The words of one pipeline stage, with every `env -S`/`--split-string`
 * string split into the words env runs (WI#872). A string the hook cannot
 * read makes the stage unverifiable, which the decode rule treats as a denial
 * (WI#883). Null when the words cannot be read.
 */
function expandStageEnvWords(words, depth) {
  const out = [];
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (commandBasename(listenerWord(word)) === "env" && depth < MAX_SHELL_BODY_DEPTH) {
      const command = envCommandWords(words, i + 1);
      if (command === null) continue;
      if (command.unverifiable) return null;
      if (command.split) {
        const nested = expandStageEnvWords(command.words, depth + 1);
        if (nested === null) return null;
        out.push(word, ...nested);
        return out;
      }
    }
    out.push(word);
  }
  return out;
}

/** The words of a pipeline stage, or null when an `env -S` string is unreadable. */
function stagePropertyWords(stage) {
  return expandStageEnvWords(stageWords(stage), 0);
}

// The commands that turn an encoded payload back into text (WI#826, WI#832).
// Only a name in this set is examined, so a stage of ordinary words is read
// in one pass (WI#883).
const DECODER_NAMES = new Set([
  "base64", "xxd", "openssl", "gzip", "gunzip", "zcat", "bzip2", "bzcat",
  "bunzip2", "xz", "unxz", "xzcat", "zstd", "unzstd", "zstdcat", "printf",
  "perl", "python", "python3", "tr", "rev",
]);

/**
 * True when the word at `index` names a decoder with its decoding option:
 * `base64 -d`, `xxd -r`, `rev`, a decompressor, `openssl base64 -d`,
 * `perl`/`python` base64 one-liners, `printf` with hex or octal escapes, and
 * `tr` — but only when `tr` deletes nothing, because `tr -d '\r'` normalises
 * text rather than decoding it.
 */
function decoderAt(words, index) {
  const name = commandBasename(listenerWord(words[index]));
  if (!DECODER_NAMES.has(name)) return false;
  const args = words.slice(index + 1);
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

// Interpreters that run a program read from stdin when they are given no
// script operand (WI#883). `awk` is the most portable of the set and reads
// its program from the first operand, so it is walked separately.
const STDIN_INTERPRETERS = new Set(["perl", "ruby", "node", "python", "python3", "php", "lua", "awk"]);

// The files that name the shell's own stdin: `source /dev/stdin` and
// `. -` run the decoded text in the current shell (WI#883).
const STDIN_PROGRAM_FILES = new Set(["/dev/stdin", "-", "/proc/self/fd/0", "/dev/fd/0"]);

// The option that hands an interpreter its program without stdin. `-c` for
// python is kept as an interpreter reading, as it was before WI#883.
const INTERPRETER_PROGRAM_FLAG = {
  perl: /^-[a-zA-Z]*[eE]/,
  ruby: /^-[a-zA-Z]*e/,
  node: /^(?:-[a-zA-Z]*[ep]|--eval|--print)(?:$|=)/,
  python: /^-[a-zA-Z]*c/,
  python3: /^-[a-zA-Z]*c/,
  php: /^(?:-[a-zA-Z]*r|--run)(?:$|=)/,
  lua: /^-[a-zA-Z]*e/,
};

/**
 * True when an interpreter invocation runs a program it reads from stdin
 * (WI#883): it names no script operand, or names `-`. An eval flag
 * (`perl -e`, `node -p`, `python -c`) and python's `-m` supply the program
 * another way, and a plain operand is a script file.
 */
function interpreterStdinProgram(name, rest) {
  if (name === "awk") return awkStdinProgram(rest);
  const values = interpreterValueOptions(name);
  const flag = INTERPRETER_PROGRAM_FLAG[name];
  let i = 0;
  while (i < rest.length) {
    const word = rest[i];
    if (word === "-") return true;
    if (word === "--") return (rest[i + 1] ?? "-") === "-";
    if (!word.startsWith("-")) return false;
    if (flag !== null && flag !== undefined && flag.test(word)) return true;
    if ((name === "python" || name === "python3") && (word === "-m" || word === "--module" || /^-m./.test(word))) {
      return false;
    }
    if (values.has(word)) {
      i += 2;
      continue;
    }
    i++;
  }
  return true;
}

/** awk runs the first non-option operand, and `-f -` names stdin. */
function awkStdinProgram(rest) {
  for (let i = 0; i < rest.length; i++) {
    const word = rest[i];
    if (word === "-f" || word === "--file") return (rest[i + 1] ?? "-") === "-";
    if (word === "-v" || word === "-F") {
      i++;
      continue;
    }
    if (word === "-") return true;
    if (!word.startsWith("-")) return false;
  }
  return true;
}

/**
 * True when the word at `index` runs a program from stdin: a shell, a
 * `source`/`.` of the shell's stdin, or an interpreter with no script
 * operand (WI#883). A word property, like the listener rule (WI#858), so a
 * launcher the hook does not know cannot hide one. `eval` is the shell
 * itself: what it is handed — a substitution that reads stdin, a variable a
 * `read` filled — runs in this shell, so the word alone counts (AME-1).
 */
function interpreterAt(words, index) {
  const name = commandBasename(listenerWord(words[index]));
  if (SHELL_NAME.test(name) || name === "eval") return true;
  if (name === "source" || name === ".") {
    const operand = words[index + 1];
    return operand !== undefined && STDIN_PROGRAM_FILES.has(stripShellQuotes(operand));
  }
  if (!STDIN_INTERPRETERS.has(name)) return false;
  return interpreterStdinProgram(name, words.slice(index + 1));
}

/**
 * True for a stage that turns an encoded payload back into text. The rule is
 * a property of the words (WI#883), not the stage's command position, so a
 * launcher (`nice`, `nohup`, `timeout`, `xargs`, …) cannot hide the decoder.
 */
function isDecodeStage(stage) {
  const words = stagePropertyWords(stage);
  if (words === null) return true;
  return words.some((_word, index) => decoderAt(words, index));
}

/**
 * True when the stage executes its stdin: a shell, an interpreter with no
 * script operand, or a `source`/`.` of the shell's stdin, at any word
 * position the stage executes (WI#883).
 */
function isInterpreterStage(stage) {
  const words = stagePropertyWords(stage);
  if (words === null) return true;
  return words.some((_word, index) => interpreterAt(words, index));
}

// The commands of WI#883's own table that run their first operand as a
// command. Behind one of them the command word is the first word that is
// neither an option, nor the separate value of one of the options below, nor
// a bare number (`nice -n 5`, `timeout 5`, `taskset -c 0`, `ionice -c 3`).
// The list is the item's table, not a claim that every launcher is known: a
// launcher outside it keeps the word reading only.
const STAGE_LAUNCHERS = new Set([
  "nice", "nohup", "timeout", "command", "setsid", "stdbuf", "taskset",
  "ionice", "xargs", "time", "chrt",
]);
// Launcher options whose value is a separate word that need not be a number
// (`timeout -s KILL`, `stdbuf -o L`, `xargs -I {}`), read from each manual.
const STAGE_LAUNCHER_VALUE_OPTIONS = new Map([
  ["timeout", new Set(["-s", "--signal", "-k", "--kill-after"])],
  ["stdbuf", new Set(["-i", "--input", "-o", "--output", "-e", "--error"])],
  ["xargs", new Set(["-I", "-i", "-d", "--delimiter", "-E", "-e", "-a", "--arg-file", "-n", "-P", "-L", "-s"])],
  ["ionice", new Set(["-c", "--class", "-n", "--classdata", "-p", "--pid"])],
  ["nice", new Set(["-n", "--adjustment"])],
  ["time", new Set(["-f", "--format", "-o", "--output"])],
]);
const MAX_STAGE_LAUNCHERS = 8;

// Shell words that stand in front of a command inside a compound command,
// and the ones whose list names no command of its own (`for x in …`).
const COMPOUND_LEAD_WORDS = new Set(["do", "then", "else", "elif", "while", "until", "if", "!", "{", "("]);
const COMPOUND_HEAD_ONLY = new Set(["for", "select", "case"]);

/**
 * True when the command word of one simple command cannot be read (AME-2 of
 * WI#883): it is, or carries, an expansion or a substitution (`$SHELL`,
 * `"$SHELL"`, `${SHELL}`, `$(which bash)`, `/bin/$SH`), or it sits behind
 * `env` nesting past the depth bound. Only the command position is read —
 * after assignments, `env`/`exec`/`sudo`/`busybox` and the launchers above —
 * so an expansion used as an argument (`grep "$PATTERN"`, `tee "$OUT"`,
 * `head -n $N`) stays ordinary.
 */
function commandWordUnverifiable(words) {
  let current = words;
  for (let hop = 0; hop <= MAX_STAGE_LAUNCHERS; hop++) {
    let lead = 0;
    while (lead < current.length && COMPOUND_LEAD_WORDS.has(current[lead])) lead++;
    current = current.slice(lead);
    if (current.length === 0 || COMPOUND_HEAD_ONLY.has(current[0])) return false;
    const { name, word, args, unverifiable } = stageCommandFrom(current, 0);
    if (unverifiable || GLUED_EXPANSION.test(word)) return true;
    if (!STAGE_LAUNCHERS.has(name)) return false;
    const values = STAGE_LAUNCHER_VALUE_OPTIONS.get(name);
    let next = 0;
    while (next < args.length && (args[next].startsWith("-") || /^[0-9][0-9A-Za-z.,:-]*$/.test(args[next]))) {
      next += values?.has(args[next]) ? 2 : 1;
    }
    current = args.slice(next);
  }
  // More launchers in a row than the guard reads: refuse rather than guess.
  return true;
}

/** True when any simple command of the text has a command word the guard cannot read. */
function runsUnverifiableCommand(text) {
  for (const list of commandLists(text)) {
    for (const stage of splitOutsideQuotes(list, "|")) {
      if (commandWordUnverifiable(stageWords(stage))) return true;
    }
  }
  return false;
}

// The word that closes each compound command a pipeline stage can open. A
// subshell is closed by its parenthesis, which is counted, not matched as a
// word.
const COMPOUND_CLOSERS = new Map([
  ["while", "done"], ["until", "done"], ["for", "done"], ["select", "done"],
  ["if", "fi"], ["case", "esac"], ["{", "}"],
]);
const COMPOUND_SKIP_WORDS = new Set(["do", "then", "else", "elif", "!", "time"]);

/** The compound word a list opens in command position (`(` for a subshell), or "". */
function compoundOpener(text) {
  const words = text.trim().split(/[ \t]+/);
  let i = 0;
  while (i < words.length && COMPOUND_SKIP_WORDS.has(words[i])) i++;
  const first = words[i] ?? "";
  if (COMPOUND_CLOSERS.has(first)) return first;
  return first.startsWith("(") && !first.startsWith("((") ? "(" : "";
}

/** Unquoted `(` minus `)` in the text; a `$(…)` pair cancels itself. */
function parenBalance(text) {
  let depth = 0;
  let quote = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quote === "'") {
      if (ch === "'") quote = "";
    } else if (ch === "\\") {
      i++;
    } else if (quote === '"') {
      if (ch === '"') quote = "";
    } else if (ch === "'" || ch === '"') {
      quote = ch;
    } else if (ch === "(") {
      depth++;
    } else if (ch === ")") {
      depth--;
    }
  }
  return depth;
}

/**
 * The text a pipeline stage runs when it opens a compound command: the stage
 * and the following lists up to its closing word (AME-1 of WI#883). A pipe
 * into `while read l; do eval "$l"; done`, `{ read x; eval "$x"; }` or
 * `( read x; eval "$x" )` feeds every command of the body, and the `;` lists
 * otherwise end the stage at the loop's first word. A body with no closing
 * word runs to the end of the command, which can only add words to read.
 */
function compoundStageText(lists, li, stage) {
  const opener = compoundOpener(stage);
  if (opener === "") return stage;
  const closers = opener === "(" ? [] : [COMPOUND_CLOSERS.get(opener)];
  let parens = parenBalance(stage);
  let end = li;
  while ((closers.length > 0 || parens > 0) && end + 1 < lists.length) {
    end++;
    const text = lists[end];
    const first = text.trim().split(/[ \t]+/)[0] ?? "";
    if (closers.length > 0 && first === closers[closers.length - 1]) {
      closers.pop();
    } else {
      const inner = compoundOpener(text);
      if (inner !== "" && inner !== "(") closers.push(COMPOUND_CLOSERS.get(inner));
    }
    parens += parenBalance(text);
  }
  return [stage, ...lists.slice(li + 1, end + 1)].join(";");
}

// A compound body is read once per decode pipe that feeds it, and a body
// with no closing word runs to the end of the command. The bodies of one
// command may add up to this many characters; past it the command is refused
// instead of read, so many unclosed loops cannot cost a rescan apiece (the
// 2 s analysis deadline). Closed loops add up to the command's own length.
const MAX_COMPOUND_SCAN_CHARS = 3 * MAX_COMMAND_CHARS;

// The shared shell-syntax scan (WI#867). Bash escapes with a backslash
// outside quotes and, inside double quotes, only `"`, `\`, `$`, a backtick and
// a newline; nothing escapes inside single quotes. A `$(…)` or backtick
// substitution is a command of its own, so it reads with a fresh quote context
// and the enclosing one resumes after it. Its operators cannot split the
// enclosing clause. Backtick delimiters are paired before reading their body.
const QUOTE_UNQUOTED = 0;
const QUOTE_ESCAPED = 1;
const QUOTE_SINGLE = 2;
const QUOTE_DOUBLE = 3;
const QUOTE_ANSI = 4;
const QUOTE_DOUBLE_ESCAPED = 5;
const QUOTE_ANSI_ESCAPED = 6;
const QUOTE_OPEN = 7;
const QUOTE_CLOSE = 8;
const SHELL_DATA = 9;

const DOUBLE_QUOTE_ESCAPES = '\\"$`\n';
const BACKTICK_ESCAPES = "\\$`";
const SHELL_TOKEN_BOUNDARIES = " \t\n;|&()<>";

function openingQuote(text, i) {
  if (text[i] === "$" && text[i + 1] === "'") return "$'";
  return text[i] === "'" || text[i] === '"' ? text[i] : "";
}

function isQuoteEscape(text, i, quote) {
  if (text[i] !== "\\" || quote === "'" || i + 1 >= text.length) return false;
  return quote !== '"' || DOUBLE_QUOTE_ESCAPES.includes(text[i + 1]);
}

function isEscapedState(state) {
  return state === QUOTE_ESCAPED || state === QUOTE_DOUBLE_ESCAPED ||
    state === QUOTE_ANSI_ESCAPED;
}

function quoteState(quote) {
  if (quote === "'") return QUOTE_SINGLE;
  if (quote === '"') return QUOTE_DOUBLE;
  return quote === "$'" ? QUOTE_ANSI : QUOTE_UNQUOTED;
}

/** Backtick pairing does not interpret quotes inside the body. */
function backtickEnd(text, start, escapes) {
  for (let i = start + 1; i < text.length; i++) {
    if (text[i] === "`") return i;
    if (escapes && text[i] === "\\" && i + 1 < text.length &&
        BACKTICK_ESCAPES.includes(text[i + 1])) i++;
  }
  return text.length;
}

function shellLevel(opener = -1, inherited = QUOTE_UNQUOTED) {
  return { quote: "", parentheses: 0, opener, inherited, wordStart: true, heredocs: [] };
}

/** Skip queued heredocs without feeding their bytes back into the quote state. */
function skipHeredocs(text, from, heredocs, lines) {
  let next = from;
  for (const { delimiter, stripTabs } of heredocs) {
    while (next < text.length) {
      const lf = text.indexOf("\n", next);
      const end = lf === -1 ? text.length : lf;
      const line = text.slice(next, end);
      next = lf === -1 ? end : end + 1;
      if ((stripTabs ? line.replace(/^\t+/, "") : line) === delimiter) break;
      lines?.push(line);
    }
  }
  return next;
}

/**
 * The quote context of each offset for the supported shell constructs.
 * `states[i]` is a quote state or opaque data; `substitutions` maps the index of
 * each `$(`, `<(`, `>(` or backtick opener to the index of its closer, so a
 * rule can read a body without a second scanner. A substitution is a command
 * of its own: it opens a level with a fresh quote context, and the enclosing
 * level — quote and all — resumes after its closer. Comment and heredoc
 * data are opaque to the scan; top-level heredoc lines are kept separately.
 */
function computeQuoteStates(text, escapes) {
  const states = new Uint8Array(text.length);
  const substitutions = new Map();
  const heredocLines = [];
  const levels = [shellLevel()];
  for (let i = 0; i < text.length; i++) {
    const level = levels[levels.length - 1];
    const quote = level.quote;
    const ch = text[i];
    const state = quoteState(quote);
    states[i] = state;
    if (escapes && isQuoteEscape(text, i, quote)) {
      const escaped = quote === "$'" ? QUOTE_ANSI_ESCAPED
        : quote === '"' ? QUOTE_DOUBLE_ESCAPED : QUOTE_ESCAPED;
      states[i] = escaped;
      states[++i] = escaped;
      if (text[i] !== "\n") level.wordStart = false;
      continue;
    }
    if (quote !== "" && ch === quote.at(-1)) {
      level.quote = "";
      states[i] = QUOTE_CLOSE;
      continue;
    }
    if (ch === "`" && (quote === "" || quote === '"')) {
      const close = backtickEnd(text, i, escapes);
      substitutions.set(i, close);
      states.fill(SHELL_DATA, i + 1, close);
      if (close < text.length) states[close] = state;
      level.wordStart = false;
      i = close;
      continue;
    }
    // `$$` is one parameter, the shell's PID: bash's parser consumes the
    // second `$`, so a `(` right after `$$` is a literal character and opens
    // no substitution. Reading it as `$(` hid the rest of the clause:
    // `echo "$$(x"` then a newline ran the next line (run-531 ALR-1).
    if (ch === "$" && text[i + 1] === "$" && (quote === "" || quote === '"')) {
      states[++i] = state;
      level.wordStart = false;
      continue;
    }
    const opensSubstitution = text[i + 1] === "(" &&
      ((ch === "$" && (quote === "" || quote === '"')) ||
        (quote === "" && (ch === "<" || ch === ">")));
    if (opensSubstitution) {
      levels.push(shellLevel(i, state));
      states[++i] = state;
      level.wordStart = false;
      continue;
    }
    if (quote !== "") continue;
    if (ch === ")" && level.opener !== -1 && level.parentheses === 0) {
      levels.pop();
      substitutions.set(level.opener, i);
      states[i] = level.inherited;
      continue;
    }
    if (ch === "(") level.parentheses++;
    if (ch === ")" && level.parentheses > 0) level.parentheses--;
    const opener = openingQuote(text, i);
    if (opener !== "") {
      level.quote = opener;
      level.wordStart = false;
      states[i] = QUOTE_OPEN;
      if (opener.length === 2) states[++i] = QUOTE_OPEN;
    } else if (ch === "#" && level.wordStart) {
      const lf = text.indexOf("\n", i);
      const end = lf === -1 ? text.length : lf;
      states.fill(SHELL_DATA, i, end);
      i = end - 1; // The newline still ends the clause and starts its heredocs.
    } else if (ch === "<" && text[i + 1] === "<") {
      const redirect = heredocRedirect(text, i);
      if (redirect) {
        level.heredocs.push(redirect);
        states.fill(SHELL_DATA, i, redirect.end);
        i = redirect.end - 1;
      }
      level.wordStart = true;
    } else if (ch === "\n" && level.heredocs.length !== 0) {
      const next = skipHeredocs(text, i + 1, level.heredocs,
        levels.length === 1 ? heredocLines : null);
      states.fill(SHELL_DATA, i + 1, next);
      level.heredocs.length = 0;
      level.wordStart = true;
      i = next - 1;
    } else {
      level.wordStart = SHELL_TOKEN_BOUNDARIES.includes(ch);
    }
  }
  // Even an unterminated body is not part of its surrounding clause.
  for (const { opener } of levels) {
    if (opener !== -1) substitutions.set(opener, text.length);
  }
  return { states, substitutions, heredocLines };
}

// The scan is recomputed once per text and kept for the readers walking it: a
// clause scan asks for it once per command word, and rescanning the whole
// command from each of them took the analysis past its deadline (WI#856). A
// miss only costs one linear pass.
let quoteScanText = null;
let quoteScanEscapes = true;
let quoteScanResult = null;

function quoteScan(text, escapes = true) {
  if (text === quoteScanText && escapes === quoteScanEscapes) return quoteScanResult;
  const result = computeQuoteStates(text, escapes);
  quoteScanText = text;
  quoteScanEscapes = escapes;
  quoteScanResult = result;
  return result;
}

function quoteStates(text, escapes = true) {
  return quoteScan(text, escapes).states;
}

// Do not let encoded bytes be checked as one word and executed as another.
// An escaped apostrophe is already handled by the word and clause readers.
function hasUnverifiableAnsiEscape(text) {
  const states = quoteStates(text);
  for (let i = 0; i < text.length; i++) {
    // Both characters of a backslash pair carry the escaped state; only the
    // backslash itself starts one.
    if (states[i] !== QUOTE_ANSI_ESCAPED) continue;
    if (text[i] !== "\\") continue;
    if (text[i + 1] !== "'") return true;
    i++;
  }
  return false;
}

/** Unquoted offsets in the enclosing command, not in its substitution bodies. */
function* unquotedOffsets(text, escapes = true) {
  const { states, substitutions } = quoteScan(text, escapes);
  for (let i = 0; i < text.length; i++) {
    const close = substitutions.get(i);
    if (close !== undefined) i = close;
    else if (states[i] === QUOTE_UNQUOTED) yield i;
  }
}

/** Preserve the exact text of each piece; only unquoted separators split it. */
function splitOutsideQuotes(text, separator, escapes = true) {
  // A redirection's operator characters are not control operators, and the
  // target is not a word (WI#889). The view keeps every offset, so the
  // pieces below are the caller's own text.
  const view = redirectionFreeView(text);
  const pieces = [];
  let start = 0;
  for (const i of unquotedOffsets(view, escapes)) {
    if (i < start) continue;
    const width = typeof separator === "string"
      ? Number(view[i] === separator)
      : Number(separator(view[i], view[i + 1]));
    if (!width) continue;
    pieces.push(text.slice(start, i));
    start = i + width;
  }
  pieces.push(text.slice(start));
  return pieces;
}

/** The `;`, LF, `&&` and `||` lists of the text, read outside quotes. */
function commandLists(text) {
  return splitOutsideQuotes(text, (ch, next) => {
    if (ch === ";" || ch === "\n") return 1;
    return ch === next && (ch === "&" || ch === "|") ? 2 : 0;
  });
}

/** Deny a decode command piped into a shell or interpreter (WI#826, WI#832). */
function checkDecodedPipe(text) {
  const lists = commandLists(text);
  let scanned = 0;
  for (let li = 0; li < lists.length; li++) {
    const stages = splitOutsideQuotes(lists[li], "|");
    for (let i = 1; i < stages.length; i++) {
      if (!isDecodeStage(stages[i - 1])) continue;
      // The stage a decoder feeds may open a compound command, whose body
      // reads the same stdin (AME-1). Only a list's last stage can run on
      // into the following lists.
      let fed = stages[i];
      if (i === stages.length - 1 && compoundOpener(fed) !== "") {
        fed = compoundStageText(lists, li, fed);
        scanned += fed.length;
        if (scanned > MAX_COMPOUND_SCAN_CHARS) return "decoded content piped into a shell or interpreter";
      }
      // A command word the guard cannot read may be the shell (AME-2).
      if (isInterpreterStage(fed) || runsUnverifiableCommand(fed)) {
        return "decoded content piped into a shell or interpreter";
      }
    }
  }
  return null;
}

const BLOCK_DEVICE_WRITE_PATTERN =
  />\|?\s*\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)|\bof=\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)|\btruncate\b[^;&|]*\s+\/dev\/(sd|vd|nvme|hd|mmcblk|loop|mapper\/|disk\/by-)/;

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
    name: GIT_ALIAS_REASON,
    // An alias body is an arbitrary verb and an include pulls in a file that
    // can define one, so the definition is refused before the verb rules read
    // the literal verb (WI#842). Since WI#867 this refusal has its own reason
    // instead of reusing the discard rule's name.
    check: checkGitAliasDefinition,
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
    // `>&word` and `n>&word` write `word` when it is not a descriptor
    // number, and `&>word`/`&>>word` write it too, so the target the
    // redirection reader collected is checked in every write form (WI#889).
    check: (segment) => {
      if (BLOCK_DEVICE_WRITE_PATTERN.test(segment)) return true;
      for (const { target, write } of redirectScan(segment).targets) {
        if (write && DEVICE_TARGET.test(stripShellQuotes(target))) return true;
      }
      return false;
    },
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
    if (!hit) continue;
    // A check may return its own reason text when the rule's name would be
    // wrong for this row (WI#867, the git alias refusal).
    return typeof hit === "string" ? hit : rule.name;
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

function evaluate(command, depth = 0) {
  if (command.length > MAX_COMMAND_CHARS) {
    return `command exceeds ${MAX_COMMAND_CHARS} chars`;
  }
  // Structural rules (fork bombs) see the whole command: segment splitting
  // on ;|& would shred the very shape they match. Keep them on the raw text.
  const rawDenial = checkDenyRulesEverywhere(command) ?? checkRawShapes(command) ??
    checkTrustedUserConfig(command) ?? checkListeners(command, depth);
  if (rawDenial) return rawDenial;
  // Bash can join a backslash-LF pair into the word around it, so a rule
  // anchored on a word never matches the raw text (`su\<LF>do`). Run every
  // rule on the joined text, on its lexically normalised absolute paths and
  // on the word view bash builds, the same three readings rm uses (WI#813,
  // WI#826). An extra quote-preserving reading separates the shell
  // punctuation bash glues to a word, so `(git reset --hard)` and
  // `$(chmod -R 777 /etc)` read as the commands they run (WI#853).
  //
  // Every reading is also taken with its redirections blanked out (WI#889):
  // a redirection is neither a clause end nor an argument, so a rule must see
  // the words bash runs. The raw-shape family's own readers step over a
  // redirection too, which keeps a target visible to a rule about targets.
  for (const [lineBreak, quotedLineBreak] of JOIN_READINGS) {
    const joined = joinLineContinuations(command, lineBreak, quotedLineBreak);
    const unredirected = redirectionFreeView(joined);
    const punctuation = shellPunctuationView(joined);
    const wordLevel = shellWordLevelView(joined);
    // A verb glued to an unquoted expansion (`${EMPTY}tee /dev/sda1`) is the
    // plain verb when the variable is empty or unset, which the raw-shape and
    // listener readers would otherwise walk past (WI#883).
    const glued = gluedExpansionView(joined);
    // An operator expansion (`${X:-tee}`, `${X:=tee}`, `${X:+tee}`) can run
    // its own word instead of the variable's value, so that reading is taken
    // beside the plain one (AME-3 of WI#883).
    const defaulted = gluedExpansionView(joined, true);
    const defaultedIsNew = defaulted !== joined && defaulted !== glued;
    const joinedDenial = checkDenyRulesEverywhere(joined) ??
      checkDenyRulesEverywhere(normalizeAbsolutePaths(joined)) ??
      (punctuation === joined ? null : checkDenyRulesEverywhere(punctuation)) ??
      checkDenyRulesEverywhere(shellWordView(joined)) ??
      checkDenyRulesEverywhere(normalizeAbsolutePaths(shellWordView(joined))) ??
      checkTrustedUserConfig(joined) ??
      checkTrustedUserConfig(shellWordView(joined)) ??
      checkRawShapes(joined) ??
      // The raw-shape family reads bash's word view too: a quoted or escaped
      // character on the verb (`"tee" /dev/sda1`, `\find /app -delete`)
      // leaves every rule intact in bash, and the family's own
      // `argumentWords`/`stripShellQuotes` expect quote-stripped input
      // (WI#873). The word-level view, not `shellWordView`, keeps an
      // interpreter operand's own quotes.
      (wordLevel === joined ? null : checkRawShapes(wordLevel)) ??
      (glued === joined ? null : checkRawShapes(glued)) ??
      // The glued view leaves a quoted verb quoted (`$EMPTY"tee"` reads as
      // `"tee"`), so the WI#873 word-level reading applies to it as well.
      (glued === joined ? null : checkRawShapes(shellWordLevelView(glued))) ??
      (glued === joined ? null : checkListeners(glued)) ??
      (glued === joined ? null : checkListeners(shellWordLevelView(glued))) ??
      (defaultedIsNew ? checkRawShapes(defaulted) : null) ??
      (defaultedIsNew ? checkRawShapes(shellWordLevelView(defaulted)) : null) ??
      (defaultedIsNew ? checkListeners(defaulted) : null) ??
      (defaultedIsNew ? checkListeners(shellWordLevelView(defaulted)) : null) ??
      checkMoveAndTarDeletion(shellWordView(joined)) ??
      // The redirection-free reading: a rule sees the words bash runs, and a
      // target it does not own is still visible to a rule about targets
      // (WI#889 properties 1, 2 and 6).
      (unredirected === joined ? null : checkDenyRulesEverywhere(unredirected)) ??
      (unredirected === joined ? null : checkRawShapes(unredirected)) ??
      (unredirected === joined ? null : checkMoveAndTarDeletion(shellWordView(unredirected))) ??
      (unredirected === joined ? null : checkListeners(unredirected, depth)) ??
      (joined === command ? null : checkListeners(joined, depth));
    if (joinedDenial) return joinedDenial;
    for (const segment of commandSegments(glued === joined ? joined : glued)) {
      const denied = checkRm(segment);
      if (denied) return denied;
    }
    if (defaultedIsNew) {
      for (const segment of commandSegments(defaulted)) {
        const denied = checkRm(segment);
        if (denied) return denied;
      }
    }
    if (glued === joined) continue;
    for (const segment of commandSegments(joined)) {
      const denied = checkRm(segment);
      if (denied) return denied;
    }
  }
  return null;
}

// WI#833: keep listener checks separate from the other raw command rules.
// This recognizes literal command shapes, not general Bash execution.
//
// WI#858: the rule is a property of the words, not of a closed launcher
// list. A `kimi` word followed by a service subcommand, or a listener verb
// with its listener options, is denied at ANY word position outside quotes,
// so a launcher the hook has never heard of (`taskset`, `chrt`, `flock`,
// `watch`, `fd -x`, `parallel`, `eval`, `xargs`, `find -exec`, …) cannot
// hide one. The accepted cost is a refusal of a command that only mentions
// the words unquoted (`echo kimi server`); quoted text stays data, and so
// does a `kimi -p` prompt value. Adding launchers one at a time never
// closes the class, which is why the prefix list is gone.

// An unquoted IFS expansion separates words exactly like a space does, and a
// slice of it (`${IFS:0:1}`, the first character — a space) does the same
// (WI#867, as the rm assembly check already reads it). Any `${IFS…}` form
// counts, so an unusual one can only add a denial.
function ifsSeparatorLength(text, i) {
  if (text[i] !== "$") return 0;
  if (text.startsWith("${IFS", i) && !/[A-Za-z0-9_]/.test(text[i + 5] ?? "")) {
    const close = text.indexOf("}", i + 5);
    if (close !== -1 && !text.slice(i + 5, close).includes("\n")) return close - i + 1;
  }
  if (text.startsWith("$IFS", i) && !/[A-Za-z0-9_]/.test(text[i + 4] ?? "")) return 4;
  return 0;
}

// This marker is internal to listener words. Bash cannot carry a NUL byte in
// an argument. Quoted IFS text is kept verbatim and never becomes a marker.
const LISTENER_FIELD_BREAK = "\0";

/**
 * Read simple command words, keeping quoted values (including empty ones)
 * together. Each word carries a quote mask beside its text: one `1` per
 * character that came from a quoted or escaped span (or a substitution's
 * unknown output), `0` otherwise. Brace expansion reads its own `{`, `,` and
 * `}` from the mask, because bash expands the group first and removes the
 * quotes afterwards — a quoted alternative does not make the group literal
 * (WI#868). The quote context is the shared reader's, so a substitution's own
 * quotes do not disturb the word around it (WI#867). `quoted` records whether
 * any of the word was quoted, so a quoted empty word (`""`) stays a word.
 */
function* listenerSegments(text) {
  const { states, substitutions } = quoteScan(text);
  let words = [];
  let word = "";
  let mask = "";
  let started = false;
  let quoted = false;
  let wordStart = -1;
  const append = (chars, fromQuotes) => {
    if (!started) wordStart = -1;
    word += chars;
    mask += (fromQuotes ? "1" : "0").repeat(chars.length);
    started = true;
  };
  const flush = () => {
    if (started) words.push({ text: word, mask, quoted });
    word = "";
    mask = "";
    started = false;
    quoted = false;
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    const state = states[i];
    const close = substitutions.get(i);
    if (close !== undefined) {
      // The body is a separate command; its unknown output is not a literal
      // word, and none of it is a brace delimiter.
      append("$?", true);
      i = close;
    } else if (state === SHELL_DATA) {
      flush();
    } else if (isEscapedState(state)) {
      append(text[i + 1] ?? "", true);
      i++;
      quoted = true;
    } else if (state === QUOTE_OPEN) {
      if (ch === "$") i++;
      started = true;
      quoted = true;
    } else if (state !== QUOTE_UNQUOTED) {
      if (state !== QUOTE_CLOSE) append(ch, true);
    } else {
      // A redirection is not a word: `kimi 2>&1 server` names the service
      // subcommand, and `python3 -m 2>/dev/null http.server` the module
      // (WI#889). The token ends the word it glues to (`a&>f` is `a`), and
      // only at a word start does it carry a `[n]` descriptor.
      const skip = started ? redirectionSkip(text, i) : redirectTokenSpan(text, i);
      if (skip !== null) {
        flush();
        i = (typeof skip === "number" ? skip : skip.end) - 1;
        continue;
      }
      const separator = ifsSeparatorLength(text, i);
      if (separator > 0) {
        // Keep the shell word intact until its brace alternatives are known.
        append(LISTENER_FIELD_BREAK, false);
        i += separator - 1;
      } else if (" \t;|&\n".includes(ch)) {
        flush();
        if (";|&\n".includes(ch) && words.length > 0) {
          yield words;
          words = [];
        }
      } else {
        append(ch, false);
      }
    }
  }
  flush();
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

/**
 * The bodies bash runs as commands of their own: `$(…)`, backticks and the
 * process substitutions `<(…)` / `>(…)`. A substitution inside double quotes
 * still runs, so it counts; text in single quotes is literal and is skipped
 * (WI#839, WI#858). Yield immediate bodies only; callers examine nested ones
 * with their own syntax scan and depth budget.
 */
function* substitutionBodies(text) {
  const { substitutions } = quoteScan(text);
  for (let i = 0; i < text.length; i++) {
    const close = substitutions.get(i);
    if (close === undefined) continue;
    const backtick = text[i] === "`";
    const body = text.slice(i + (backtick ? 1 : 2), close);
    yield backtick ? body.replace(/\\([\\$`])/g, "$1") : body;
    i = close;
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
 *
 * Every reading moves at least one word ahead, so the walk is a forward-only
 * table built from the end in one pass (WI#858). Since the property asks at
 * every `kimi` word, a per-start walk was quadratic: 16 KB of `kimi -x`
 * took 1.7 s of the 2 s deadline. The table answers each start in O(1).
 */
function kimiServiceSubcommands(words) {
  const found = new Array(words.length + 2).fill(null);
  for (let i = words.length - 1; i >= 0; i--) {
    const word = words[i];
    // The subcommand word may carry the punctuation bash glues to it
    // (`server)` in `(kimi -m X server)`), read as in WI#839.
    if (word === "--") {
      const after = listenerWord(words[i + 1] ?? "");
      found[i] = KIMI_SERVICE_SUBCOMMANDS.has(after) ? after : null;
    } else if (!word.startsWith("-")) {
      const subcommand = listenerWord(word);
      found[i] = KIMI_SERVICE_SUBCOMMANDS.has(subcommand) ? subcommand : null;
    } else if (word.includes("=") || KIMI_FLAG_OPTIONS.has(word)) {
      found[i] = found[i + 1];
    } else if (KIMI_VALUE_OPTIONS.has(word)) {
      found[i] = found[i + 2];
    } else {
      found[i] = found[i + 1] ?? found[i + 2];
    }
  }
  return found;
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

// Launcher options that take the next word as their value. A walk that only
// steps over `-`-prefixed words stops at the value and never reaches the `-c`
// that carries the body, so `bash -o pipefail -c "kimi server"` read as no
// shell body at all (WI#869).
const SHELL_VALUE_OPTIONS = new Set(["-o", "+o", "-O", "+O", "--rcfile", "--init-file"]);
// `script`'s own option list: every entry takes a separate value, and
// `-c`/`--command` is the one that carries the body. `-t`/`--timing` is
// absent on purpose — its argument is optional, so it must not eat a word.
const SCRIPT_VALUE_OPTIONS = new Set([
  "-I", "--log-in", "-O", "--log-out", "-B", "--log-io", "-T", "--log-timing",
  "-m", "--logging-format", "-E", "--echo", "-o", "--output-limit",
  "-c", "--command",
]);

/**
 * The shell text a launcher runs, or undefined when it carries none (WI#858).
 * A shell's `-c` takes its body as the next word, and `script`'s
 * `-c`/`--command` takes it as the option value; `script -- <command>` runs
 * the words after the marker. An option that takes a separate value is
 * stepped over whole, because its value is not the body (`bash -o pipefail -c
 * "kimi server"`, `bash --rcfile /dev/null -c …`) (WI#869).
 *
 * The text after `-c` is returned whole rather than as one word, so a body
 * split by a further option word (`bash -oc pipefail -c "kimi server"`) is
 * still read by the property rule.
 */
function listenerShellBody(words, start, launcher) {
  const values = launcher === "script" ? SCRIPT_VALUE_OPTIONS : SHELL_VALUE_OPTIONS;
  for (let i = start; i < words.length; i++) {
    const word = words[i];
    if (word === "--") {
      return launcher === "script" ? words.slice(i + 1).join(" ") : undefined;
    }
    if (values.has(word)) {
      // `script -c`/`--command` carries the body itself; every other value
      // is an option argument that is not a command.
      if (launcher === "script" && (word === "-c" || word === "--command")) {
        return words[i + 1];
      }
      i++;
      continue;
    }
    if (/^[-+][a-zA-Z]*c[a-zA-Z]*$/.test(word)) return words.slice(i + 1).join(" ");
    if (word.startsWith("-") || word.startsWith("+")) {
      // A cluster whose last letter is `o`/`O` takes the next word
      // (`bash -xo pipefail -c …`); an attached value needs no step.
      if (/^[-+][a-zA-Z]*[oO]$/.test(word)) i++;
      continue;
    }
    // A shell stops reading options at its script-file operand; `script`
    // reads its log-file operand and keeps going.
    if (launcher !== "script") return undefined;
  }
  return undefined;
}

const FIND_EXEC_ACTIONS = new Set(["-exec", "-execdir", "-ok", "-okdir"]);

/**
 * A shell body is a command line of its own, so it gets the rules the
 * top-level command gets — not just the listener rule (WI#874). Bash runs the
 * `-c` body as the command it spells, so a device write or a deletion inside
 * it must be read exactly as the unwrapped spelling is. `listenerShellBody`
 * hands the body over with its quoting already resolved by the listener word
 * reader, which is the text bash would run.
 */
function checkShellBody(body, depth) {
  const raw = checkRawShapes(body) ??
    (shellWordLevelView(body) === body ? null : checkRawShapes(shellWordLevelView(body))) ??
    checkMoveAndTarDeletion(shellWordView(body));
  if (raw) return raw;
  for (const segment of commandSegments(body)) {
    const denied = checkRm(segment);
    if (denied) return denied;
  }
  return checkListeners(body, depth);
}

/**
 * Deny a listener started from a find `-exec`/`-ok` body (WI#846). Bash runs
 * that body exactly as it runs an `xargs` body; the body is the words up to
 * the `;` or `+` terminator. A quoted or escaped terminator arrives as its
 * own `;` word.
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

// A launcher that runs its own string as a shell command: `script -c` runs it
// with `sh -c`, and `eval` joins its words and runs them. Neither is a
// command position — the words are read wherever they sit (WI#858).
const SHELL_STRING_LAUNCHERS = new Set(["script"]);

/**
 * The words bash's brace expansion would also produce from one word
 * (WI#858): `{kimi,server}` runs `kimi server`. Bash expands the group
 * first and removes the quotes afterwards, so quoting an alternative —
 * `{kimi,"server"}`, `{"kimi",server}`, `{kimi,'server'}`, `{kimi,$'server'}`
 * — leaves the group a group and only its delimiters decide (WI#868). The
 * word's quote mask says which `{`, `,` and `}` bash read before it strips
 * the quotes: an unquoted group holding an unquoted comma expands; `{}`,
 * `{x}` and `${…}` stay as they are. The alternatives replace the word in
 * place, so the expansion keeps its neighbours. Linear in the input: no
 * cross product, and one level of nesting is read.
 */
function listenerBraceWords({ text, mask }) {
  for (let open = 0; open < text.length; open++) {
    if (text[open] !== "{" || mask[open] !== "0") continue;
    if (open > 0 && text[open - 1] === "$" && mask[open - 1] === "0") continue;
    let depth = 0;
    let close = -1;
    for (let i = open; i < text.length; i++) {
      if (mask[i] !== "0") continue;
      if (text[i] === "{") depth++;
      else if (text[i] === "}" && --depth === 0) {
        close = i;
        break;
      }
    }
    // An unmatched `{` is literal, and so is every later `{` it contains.
    if (close === -1) return null;
    const bounds = [open + 1];
    let nested = 0;
    for (let i = open + 1; i < close; i++) {
      if (mask[i] !== "0") continue;
      const ch = text[i];
      if (ch === "{") nested++;
      else if (ch === "}") nested--;
      else if (ch === "," && nested === 0) bounds.push(i, i + 1);
    }
    // No top-level comma: bash expands a nested group instead, if one holds
    // a comma (`{a{b,c}}` runs `{ab} {ac}`).
    if (bounds.length === 1) continue;
    bounds.push(close);
    const head = text.slice(0, open);
    const headMask = mask.slice(0, open);
    const tail = text.slice(close + 1);
    const tailMask = mask.slice(close + 1);
    const alternatives = [];
    for (let i = 0; i < bounds.length; i += 2) {
      alternatives.push({
        text: head + text.slice(bounds[i], bounds[i + 1]) + tail,
        mask: headMask + mask.slice(bounds[i], bounds[i + 1]) + tailMask,
      });
    }
    return alternatives;
  }
  return null;
}

/**
 * Bash's own cross product, without its blow-up: `{a,b}` is five bytes, so a
 * 16 KB word can hold thousands of groups. One word may expand to at most this
 * many words. Past it the guard cannot read the expansion, so the command is
 * refused rather than read as literal text: keeping the rest literal let
 * `{,}` ten times plus `{kimi,server}` run `kimi server` (run-530 ALP-1).
 */
const BRACE_EXPANSION_LIMIT = 1024;

/**
 * One segment's words: brace groups expanded as bash would, then unquoted IFS
 * markers split into fields (WI#867, WI#868), or null when a word's expansion
 * passes `BRACE_EXPANSION_LIMIT`.
 */
function listenerWordList(segment) {
  const words = [];
  for (const entry of segment) {
    const pending = [entry];
    let head = 0;
    // Words this entry currently stands for: expanded words plus pending
    // ones. Every group has at least two alternatives, so the words walked
    // stay under twice this count.
    let live = 1;
    while (head < pending.length) {
      const word = pending[head++];
      const alternatives = listenerBraceWords(word);
      if (alternatives === null) {
        pushListenerFields(words, word);
        continue;
      }
      live += alternatives.length - 1;
      if (live > BRACE_EXPANSION_LIMIT) return null;
      pending.push(...alternatives);
    }
  }
  return words;
}

/**
 * Add one expanded word, split at its unquoted IFS markers. An empty word is
 * kept only when the shell word was quoted (`""`); an empty brace alternative
 * disappears, as in bash.
 */
function pushListenerFields(words, { text, quoted }) {
  if (!text.includes(LISTENER_FIELD_BREAK)) {
    if (text !== "" || quoted) words.push(text);
    return;
  }
  for (const field of text.split(LISTENER_FIELD_BREAK)) {
    if (field !== "") words.push(field);
  }
}

// GNU env's full long-option table (`env --help`, coreutils 9.12). Resolve
// abbreviations before looking for required arguments. The signal options'
// optional arguments are always attached, so those options never eat a word.
const ENV_REQUIRED_LONG_VALUES = new Set(["--unset", "--chdir", "--argv0", "--env0-from", "--split-string"]);
const ENV_LONG_OPTIONS = [
  ...ENV_REQUIRED_LONG_VALUES,
  "--ignore-environment", "--null", "--debug", "--list-signal-handling",
  "--block-signal", "--default-signal", "--ignore-signal", "--help", "--version",
];
// Bound option restarts independently of actual shell/launcher nesting.
const MAX_ENV_SPLITS = 16;

function envLongOption(name) {
  if (ENV_LONG_OPTIONS.includes(name)) return name;
  const matches = ENV_LONG_OPTIONS.filter((option) => option.startsWith(name));
  return matches.length === 1 ? matches[0] : null;
}

/**
 * The words GNU `env -S`/`--split-string` makes of its string, or null when
 * the string makes env fail before it runs anything. Measured on coreutils
 * 9.12: unquoted whitespace separates; `'…'` is literal; `"…"` takes the
 * same escapes; `#` at a word start begins a comment; `\c` ends the string;
 * `\_` separates outside quotes and is a space inside them; `\f`, `\n`,
 * `\r`, `\t`, `\v` are the control characters they name, and a tab stays in
 * its word; only `${NAME}` expands, so a bare `$` or an unknown escape is an
 * env error. `${NAME}` is unknown text a task can set (`${A} ${B}` runs
 * `kimi server`), and so is a substitution's unknown output, so both are
 * reported as unverifiable rather than read as a word.
 */
function envSplitStringWords(text) {
  const escapes = { f: "\f", n: "\n", r: "\r", t: "\t", v: "\v", "\\": "\\", '"': '"', "'": "'", $: "$" };
  const words = [];
  let word = "";
  let started = false;
  let unverifiable = false;
  let quote = "";
  const flush = () => {
    if (started) words.push(word);
    word = "";
    started = false;
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quote === "'") {
      if (ch === "'") quote = "";
      else word += ch;
      continue;
    }
    if (ch === "\\") {
      const escaped = text[++i];
      if (escaped === "c") break;
      if (escaped === "_") {
        if (quote === '"') {
          word += " ";
          started = true;
        } else {
          flush();
        }
        continue;
      }
      const value = escapes[escaped];
      if (value === undefined) return null;
      word += value;
      started = true;
      continue;
    }
    if (ch === "$") {
      // `${NAME}`, `$NAME` and a substitution's unknown output are all text
      // the task can choose, and env's own expansion only accepts `${NAME}`
      // anyway, so the words are not readable from the command line.
      unverifiable = true;
      started = true;
      continue;
    }
    if (quote === '"') {
      if (ch === '"') quote = "";
      else word += ch;
      started = true;
      continue;
    }
    if (ch === "'" || ch === '"') {
      quote = ch;
      started = true;
      continue;
    }
    if (" \t\n\r\v\f".includes(ch)) {
      flush();
      continue;
    }
    if (ch === LISTENER_FIELD_BREAK) {
      // An unquoted `${IFS}` the shell expands into the string: where its
      // fields fall depends on the task's environment.
      unverifiable = true;
      started = true;
      continue;
    }
    if (ch === "#" && !started) break;
    word += ch;
    started = true;
  }
  if (quote !== "") return null;
  flush();
  return unverifiable ? { unverifiable: true } : { words };
}

/**
 * The operands of an `env` invocation, after walking its options. Like GNU
 * env, restart the option walk after replacing `-S` and its value with the
 * split words plus the remaining argv (WI#872). An assignment, a command
 * word, `--` or the legacy `-` ends option parsing. Return null for an
 * invalid split or no operands, and `{ unverifiable: true }` when a split
 * cannot be read within the bound.
 */
function envCommandWords(words, start) {
  let splits = 0;
  let i = start;
  while (i < words.length) {
    const word = words[i];
    if (word === "--") {
      i++;
      break;
    }
    if (word === "-" || !word.startsWith("-")) break;
    i++;
    let value;
    let splitOption;
    if (word.startsWith("--")) {
      const equals = word.indexOf("=");
      const option = envLongOption(equals === -1 ? word : word.slice(0, equals));
      // An unknown literal option is invalid in GNU env, but the word may
      // be assembled by the shell. Keep looking for a later literal -S.
      if (!ENV_REQUIRED_LONG_VALUES.has(option)) continue;
      splitOption = option === "--split-string";
      value = equals === -1 ? words[i++] : word.slice(equals + 1);
    } else {
      // The first value-taking letter consumes the rest of the cluster,
      // or the next argv word. Other letters do not take a value.
      const index = word.search(/[uCaS]/);
      if (index === -1) continue;
      splitOption = word[index] === "S";
      value = word.slice(index + 1) || words[i++];
    }
    if (value === undefined) return null;
    if (!splitOption) continue;
    if (splits >= MAX_ENV_SPLITS) return { unverifiable: true };
    const split = envSplitStringWords(value);
    if (split === null || split.unverifiable) return split;
    splits++;
    words = [...split.words, ...words.slice(i)];
    i = 0;
  }
  if (words[i] === "-") i++;
  return i < words.length ? { words: words.slice(i), split: splits > 0 } : null;
}

/**
 * An `env -S`/`--split-string` value is one shell word but a whole command
 * line to env, so the words env runs get the same treatment a shell body gets
 * (WI#874): env's own reader has already removed the shell quoting, so the
 * words are joined back into the argv text env executes.
 */
function checkEnvSplitBody(words, depth) {
  const body = words.join(" ");
  const raw = checkRawShapes(body) ?? checkMoveAndTarDeletion(shellWordView(body));
  if (raw) return raw;
  for (const segment of commandSegments(body)) {
    const denied = checkRm(segment);
    if (denied) return denied;
  }
  return checkListenerWords(words, depth);
}

/**
 * Classify every word of one listener segment (WI#858). The rule is a
 * property, not a command position: any `kimi` word followed by a service
 * subcommand, any listener verb with its listener options, any shell body and
 * any `find -exec` body is read wherever it sits. A launcher the hook does
 * not know therefore cannot hide one. Option words and assignments need no
 * skipping: `kimi` is matched by name at every index, and the option table
 * decides what its own arguments are.
 *
 * An `env -S`/`--split-string` value is one word to the shell but a command
 * line to env, so it is split into the words env runs before the rule reads
 * them (WI#872); otherwise `env -S "kimi server"` names a listener the shell
 * never shows as two words.
 */
function checkListenerWords(words, depth) {
  // One forward-only table per segment, so a segment full of `kimi` words is
  // linear (WI#858 deadline).
  let serviceSubcommands;
  let sawGitCarrier = false;
  for (let i = 0; i < words.length; i++) {
    const word = listenerWord(words[i]);
    const name = commandBasename(word);
    if (name === "git" && !sawGitCarrier) {
      // Scan a recognized carrier once to keep repeated git words linear.
      // The word reader keeps an entire quoted description in one argument.
      const bodies = gitShellBodies(words, i + 1);
      if (bodies !== null) {
        sawGitCarrier = true;
        for (const body of bodies) {
          if (body.length === 0) continue;
          if (depth >= MAX_SHELL_BODY_DEPTH) return "cannot check listeners in a deeply nested shell command";
          const denied = checkShellBody(body, depth + 1);
          if (denied) return denied;
        }
      }
    }
    if (name === "busybox" && listenerWord(words[i + 1] ?? "") === "httpd") {
      return "listener started from a task: busybox httpd";
    }
    if (name === "env") {
      // The words env runs are not the shell's words when `-S` is used.
      const command = envCommandWords(words, i + 1);
      if (command?.unverifiable) return "cannot check listeners behind an env -S string";
      if (command && command.split) {
        if (depth >= MAX_SHELL_BODY_DEPTH) return "cannot check listeners in a deeply nested shell command";
        const denied = checkEnvSplitBody(command.words, depth + 1);
        if (denied) return denied;
      }
      continue;
    }
    if (name === "kimi") {
      serviceSubcommands ??= kimiServiceSubcommands(words);
      const subcommand = serviceSubcommands[i + 1] ?? null;
      if (subcommand !== null) return `kimi service mode from a task: kimi ${subcommand}`;
      continue;
    }
    if (name === "eval") {
      // `eval kimi server` and `eval 'kimi server'` both run `kimi server`.
      const body = words.slice(i + 1).join(" ");
      if (body.length > 0) {
        if (depth >= MAX_SHELL_BODY_DEPTH) return "cannot check listeners in a deeply nested shell command";
        const denied = checkShellBody(body, depth + 1);
        if (denied) return denied;
      }
      continue;
    }
    if (SHELL_NAME.test(name) || SHELL_STRING_LAUNCHERS.has(name)) {
      const body = listenerShellBody(words, i + 1, name);
      if (body !== undefined) {
        if (depth >= MAX_SHELL_BODY_DEPTH) return "cannot check listeners in a deeply nested shell command";
        const denied = checkShellBody(body, depth + 1);
        if (denied) return denied;
      }
      continue;
    }
    if (name === "find") {
      const denied = checkFindExecListeners(words, i + 1, depth);
      if (denied) return denied;
      continue;
    }
    const rule = LISTENER_COMMANDS.find((candidate) => candidate.name.test(name));
    // Slice only when a listener verb matched: a segment of ordinary words
    // would otherwise copy its tail once per word (WI#858 deadline).
    if (rule?.test(words.slice(i + 1).map(listenerWord))) {
      return `listener started from a task: ${name}`;
    }
  }
  return null;
}

/** Deny recognized service commands without treating prompt text as a subcommand. */
function checkListeners(text, depth = 0) {
  const denied = checkListenerRegion(text, depth);
  if (denied) return denied;
  for (const line of quoteScan(text).heredocLines) {
    const denied = checkListenerRegion(line, depth);
    if (denied) return denied;
    // Heredocs can be shell input. Check literal words too: quotes in the
    // data must not hide command shapes from the conservative listener rule.
    if (/['"]/.test(line)) {
      const denied = checkListenerRegion(line.replace(/['"]/g, " "), depth);
      if (denied) return denied;
    }
  }
  // A here-string is the command's input, read as the command line it spells
  // (`bash <<<"kimi server"`), the way a here-document body is (WI#889).
  for (const body of redirectScan(text).hereStrings) {
    const denied = checkListenerRegion(body, depth);
    if (denied) return denied;
  }
  return null;
}

function checkListenerRegion(text, depth) {
  // A substitution, a backtick or a process substitution body runs its words
  // as a command, so scan it the same way even when it sits in an argument
  // (`echo $(kimi server)`, `cat <(kimi server)`).
  for (const body of substitutionBodies(text)) {
    if (depth >= MAX_SHELL_BODY_DEPTH) return "cannot check listeners in a deeply nested shell command";
    const denied = checkListeners(body, depth + 1);
    if (denied) return denied;
  }
  for (const segment of listenerSegments(text)) {
    const words = listenerWordList(segment);
    if (words === null) return "cannot check listeners in a brace expansion this large";
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

function classify(command, depth = 0) {
  const denial = evaluate(command) ?? checkRmAssembly(command) ?? checkSameLineLink(command);
  if (denial) return denial;
  if (hasUnverifiableAnsiEscape(command) ||
      (command.includes("\\\n") && JOIN_READINGS.some(([outside, inside]) =>
        hasUnverifiableAnsiEscape(joinLineContinuations(command, outside, inside))))) {
    return "unverifiable ANSI-C escape; use literal shell words";
  }
  // An inner command's operators are masked in the outer clause scan.
  // Apply the rest of the deny rules to the body in its own context too.
  for (const body of substitutionBodies(command)) {
    if (depth >= MAX_SHELL_BODY_DEPTH) return "cannot check a deeply nested shell command";
    const denied = classify(body, depth + 1);
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
