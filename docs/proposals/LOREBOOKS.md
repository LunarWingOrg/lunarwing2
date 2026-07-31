# Idea

> **Current status (2026-07-31): IMPLEMENTED (v1).**
> Lorebook data model, keyword activation engine, and chat-prompt integration
> exist in `ic/src/lorebook/`. See "Implemented shape" below.

Char Lorebooks for agents similar to what I developed for OC

## Implemented shape

Lorebooks are JSON documents stored in the workspace under `lorebooks/*.json`
(agent-writable via the normal memory tools). Both the SillyTavern World Info
export shape and a simpler native shape are accepted:

```json
{
  "entries": {
    "0": {
      "key": ["dragon", "drake"],
      "keysecondary": ["fire"],
      "comment": "Dragon lore",
      "content": "Dragons are extinct in this setting.",
      "constant": false,
      "selective": true,
      "selectiveLogic": 0,
      "order": 100,
      "disable": false,
      "probability": 80,
      "useProbability": true,
      "scanDepth": 4,
      "caseSensitive": false,
      "matchWholeWords": true
    }
  }
}
```

At the start of each chat turn (`Agent::run_agentic_loop`), the newest
user/assistant messages are scanned for trigger keywords and the activated
entries are appended to the system prompt as a `## Lorebook` block of
`<lore_entry>` elements.

- Parsing/model: `ic/src/lorebook/model.rs` (SillyTavern field aliases,
  `disable`→`enabled` inversion, probability gating, all four
  `selectiveLogic` modes, per-entry case/whole-word matching, bare array/map
  forms). Malformed individual entries are skipped without dropping valid
  siblings; map IDs and file paths load deterministically.
- Activation engine: `ic/src/lorebook/engine.rs` (constant entries, selective
  secondary keys, context-stable probability rolls, per-entry scan depth,
  insertion ordering, approximate token budget). Budget selection prioritizes
  constant entries, then descending order; selected entries render in ascending
  order so larger values remain closer to the end of the prompt.
- Workspace loading + prompt block: `ic/src/lorebook/mod.rs`
  (`lorebooks/` dir, primary-scope reads like other identity files).
- Runtime knobs: `LOREBOOK_ENABLED` (default true), `LOREBOOK_SCAN_DEPTH`
  (10), `LOREBOOK_MAX_ENTRIES` (20), `LOREBOOK_TOKEN_BUDGET` (1200, 0 =
  no configurable token cap). Hard maxima are 100 scanned messages, 100 active
  entries, 256 KiB scanned text, 32,768 approximate configured tokens, and 256
  KiB rendered prompt bytes.
- Resource bounds: at most 64 files, 1 MiB per file, 4 MiB aggregate source,
  1,024 entries per file, and 4,096 aggregate entries load per turn. Entry
  names, keys, key counts, and content also have hard parser caps.
- Security: default and named-layer writes under `lorebooks/` scan raw JSON and
  decoded prompt-visible fields for prompt injection. Loading repeats the
  decoded scan for legacy/direct-database documents. Wrapper-like
  `<lore_entry>` tags are escaped. Lorebook appends are rejected; write the
  complete JSON document with replace semantics instead.

Deferred from full SillyTavern parity (v1 intentionally excludes): in-chat
`@depth` and other insertion positions/roles, recursive triggering, JavaScript
regex keys (currently treated as plaintext), vectorized matching, global
case/whole-word fallback settings, entry groups/sticky/cooldown, macros,
additional character/persona matching sources, and a dedicated lorebook
management tool or web UI (files are managed as ordinary workspace documents
for now).
