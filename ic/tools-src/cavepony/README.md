# Cavepony WASM Tool

Rust/WASM port of Cavepony v0.3.0 for LunarWing. The tool performs local text
transforms with no host capabilities.

## Actions

- `compress`: apply token substitutions and the selected mode.
- `expand`: expand recognized Cavepony tokens to their canonical phrases.
- `stats`: compress text and report byte, character, word, and estimated-token changes.

Modes are `tokens`, `lite`, `full`, `ultra`, `pony`, and `canterlot`. `tokens`
only applies phrase-to-token substitutions. Other compression modes are
destructive; `pony` substitutions and removed words cannot be reconstructed.
All modes are lossy: shared tokens expand to their first canonical phrase,
case is not encoded, and text already containing a token is ambiguous.
Canterlot preserves v0.3.0's ordered substitutions, including substitutions
that can further expand words introduced by an earlier rule.

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
