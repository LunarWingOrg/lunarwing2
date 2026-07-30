# Idea

> **Current status (2026-07-30, branch `aj/kimi-8-20260730-1803`): IMPLEMENTED (v1).**
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
      "order": 100,
      "disable": false,
      "probability": 80,
      "useProbability": true,
      "scanDepth": 4,
      "caseSensitive": false
    }
  }
}
```

At the start of each chat turn (`Agent::run_agentic_loop`), the newest
user/assistant messages are scanned for trigger keywords and the activated
entries are appended to the system prompt as a `## Lorebook` block of
`<lore_entry>` elements.

- Parsing/model: `ic/src/lorebook/model.rs` (SillyTavern field aliases,
  `disable`→`enabled` inversion, probability gating, bare array/map forms).
- Activation engine: `ic/src/lorebook/engine.rs` (constant entries, selective
  secondary keys, probability rolls, per-entry scan depth, insertion ordering,
  approximate token budget).
- Workspace loading + prompt block: `ic/src/lorebook/mod.rs`
  (`lorebooks/` dir, primary-scope reads like other identity files).
- Runtime knobs: `LOREBOOK_ENABLED` (default true), `LOREBOOK_SCAN_DEPTH`
  (10), `LOREBOOK_MAX_ENTRIES` (20), `LOREBOOK_TOKEN_BUDGET` (1200, 0 =
  unlimited).
- Security: writes under `lorebooks/` go through the same prompt-injection
  scan as other system-prompt files (`ic/src/workspace/mod.rs`).

Deferred from full SillyTavern parity (v1 intentionally excludes): in-chat
`@depth` insertion positions, recursive triggering, vectorized matching,
entry groups/sticky/cooldown, and a dedicated lorebook management tool or
web UI (files are managed as ordinary workspace documents for now).
