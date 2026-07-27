# Cavepony WASM Tool

Rust/WASM port of Cavepony v0.3.0 for LunarWing. Tool release 0.4.0 performs
local text transforms with no host capabilities.

## Actions

- `compress`: apply token substitutions and the selected mode.
- `expand`: expand recognized Cavepony tokens to their canonical phrases.
- `stats`: compress text and report byte, character, word, and estimated-token changes.

Modes are `tokens`, `lite`, `full`, `ultra`, `pony`, `canterlot`, and
`tony-soprano`. `tokens` only applies phrase-to-token substitutions. Other
compression and speech modes are destructive; style substitutions and removed
words cannot be reconstructed.
All modes are lossy: shared tokens expand to their first canonical phrase,
case is not encoded, and text already containing a token is ambiguous.
Canterlot preserves v0.3.0's ordered substitutions, including substitutions
that can further expand words introduced by an earlier rule.

Release 0.3.1 uses bounded literal matching instead of compiling dictionary
regexes inside each fresh WASM instance, keeping short requests in every action
within the 10-million-fuel compatibility budget.

Release 0.3.2 handles ASCII word boundaries without initializing or searching
the Unicode word class, keeping Pony substitutions inside that same budget.

Release 0.4.0 adds `tony-soprano`, a deterministic speech transform using
contractions and blunt, informal phrasing. It does not generate threats, slurs,
criminal advice, or dialogue from the show. Natural model responses receive
additional tone guidance from the bundled Cavepony skill.

Token counts use LunarWing's coarse four-bytes-per-token estimate. Actual token
counts depend on model and tokenizer; Unicode symbols can cost more than one
model token.

Fenced code, inline code, straight/curly quoted text, HTTP(S) URLs, paths, flags,
and identifier-like tokens are excluded from transforms. Input is capped at
1,536 Unicode characters (at most 6 KiB UTF-8) so even expanding modes remain
below LunarWing's default tool-output cap.
Unformatted multi-word shell commands cannot be identified reliably; format
commands as code to guarantee preservation.

Source basis: [Cavepony v0.3.0](https://clawhub.ai/chrismcfee/skills/cavepony)
by Baud & sun and the supplied [IronClaw Caveman Skill
Specification](https://i.desu.si/BrTNxfKb.md). The ClawHub release declares
MIT-0 while its archive `package.json` declares MIT; this port conservatively
retains the MIT terms shipped in package metadata.
