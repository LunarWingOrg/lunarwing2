# Idea

> **Current status (2026-07-31): IMPLEMENTED (v1).**
> Lorebook data model, keyword activation engine, and chat-prompt integration
> exist in `ic/src/lorebook/`. See "Implemented shape" below. This supersedes
> the rejected `lb/v2.0.4.0` MR with an independently reviewed reimplementation.

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

- Parsing/model: `ic/src/lorebook/model.rs` — SillyTavern field aliases,
  `disable`→`enabled` inversion, probability gating on `useProbability`, all
  four `selectiveLogic` modes (0=and-any, 1=not-all, 2=not-any, 3=and-all),
  per-entry case-sensitivity and whole-word matching, per-entry `scanDepth`,
  bare array/map forms. Malformed individual entries are skipped without
  dropping valid siblings; map IDs and file paths load deterministically.
- Activation engine: `ic/src/lorebook/engine.rs` — constant entries, selective
  secondary keys, context-stable probability rolls (seeded from the scanned
  conversation: stable across retries of the same turn, changing when the
  conversation does), insertion ordering, approximate token budget. Budget
  selection prioritizes constant entries, then descending `order`; selected
  entries render in ascending `order` so larger values sit closer to the end
  of the prompt (SillyTavern budget semantics).
- Workspace loading + prompt block: `ic/src/lorebook/mod.rs` — `lorebooks/`
  directory, primary-scope reads only (same rule as other identity files).
- Runtime knobs: `LOREBOOK_ENABLED` (default true), `LOREBOOK_SCAN_DEPTH`
  (10), `LOREBOOK_MAX_ENTRIES` (20), `LOREBOOK_TOKEN_BUDGET` (1200; 0 = no
  configurable token cap). Hard maxima: 100 scanned messages, 100 active
  entries, 256 KiB scanned text, 32,768 approximate configured tokens, 256 KiB
  rendered prompt bytes.
- Resource bounds: at most 64 files, 1 MiB per file, 4 MiB aggregate source,
  1,024 entries per file, and 4,096 aggregate entries load per turn. Entry
  names (512 B), keys (256 B), key counts (128), and content (64 KiB) have
  hard parser caps.
- Security: default-scope and named-layer writes under `lorebooks/` scan raw
  JSON **and** decoded prompt-visible entry text for prompt injection (JSON
  escapes such as `A` cannot hide dangerous text). Loading repeats the
  decoded scan so legacy/direct-database documents cannot smuggle
  instructions into the prompt. Wrapper-like `<lore_entry>` tags inside entry
  content are escaped so lore text cannot break out of or fake the wrapper.
  Lorebook appends are rejected; write the complete JSON document with
  replace semantics instead.

## Deferred from full SillyTavern parity

v1 intentionally excludes the following SillyTavern behaviors. Do not assume
they work:

- Insertion `position` / in-chat `@depth` and out-of-chat roles — activated
  entries always append to the end of the system prompt in ascending `order`.
- Recursive triggering (entry content re-scanned for further keys).
- JavaScript regex keys — keys are always treated as plaintext; a `/regex/`
  key matches the literal string, not the pattern.
- Vectorized/embedding-based matching (`vectorized` field is parsed-tolerated
  but ignored).
- Global case-sensitivity / whole-word fallback settings — only per-entry
  flags are honored; the effective default is case-insensitive substring.
- Entry groups, group weight, sticky, cooldown, and delay effects.
- Macros (`{{user}}`, `{{char}}`, etc.) — content is injected verbatim.
- Character-embedded lorebooks, persona lorebooks, and chat-linked lorebooks —
  only workspace `lorebooks/*.json` documents participate.
- Budget based on a real tokenizer — the token budget uses a conservative
  approximation (4 chars/token ASCII, 2 tokens/non-ASCII scalar), and a hard
  rendered-byte cap always applies regardless of the token budget.
- Probability is re-rolled deterministically per turn context rather than per
  message generation, so retries of the same turn see identical activation.
- A dedicated lorebook management tool or web UI — files are managed as
  ordinary workspace documents for now.
