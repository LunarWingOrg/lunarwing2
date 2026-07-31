# Lorebooks

> **Status (2026-07-31): IMPLEMENTED (v1 import subset).**

LunarWing loads bounded JSON lorebooks from primary workspace scope at
`lorebooks/*.json`. Matching entries become a `## Lorebook Reference` block in
the system prompt for that turn.

## Formats

SillyTavern World Info object exports and a native list form are accepted:

```json
{
  "entries": {
    "0": {
      "key": ["dragon", "drake"],
      "keysecondary": ["fire"],
      "comment": "Dragon lore",
      "content": "Dragons guard the northern pass.",
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

Native fields use `keys`, `secondary_keys`, `name`, and `enabled`; a bare entry
array or map is also accepted.

## Activation

- Primary keys use plaintext substring matching by default.
- `caseSensitive` and `matchWholeWords` override matching per entry.
- `constant` entries need no key.
- `selective` supports all four `selectiveLogic` values: AND ANY (`0`), NOT ALL
  (`1`), NOT ANY (`2`), and AND ALL (`3`).
- `scanDepth` overrides global message depth; depth `0` scans no messages.
- Probability is deterministic for thread, the last 100 visible messages
  (bounded to 128 KiB), source path, and entry ID. Same-turn retries remain
  stable; unrelated entries do not alter another entry's roll.
- Activated entries compete for budget by descending `order`. `constant` does
  not grant extra priority. Selected entries render by ascending `order`, with
  source path and source entry order as deterministic ties.
- Only visible user messages and assistant messages without tool calls are
  scanned. System, tool, and tool-call assistant messages cannot trigger lore.

Lorebooks are primary-scope identity configuration. Secondary workspace read
scopes never contribute entries. Group-chat activation is disabled by default;
operators must explicitly set `LOREBOOK_ALLOW_GROUP_CHATS=true` after accepting
the privacy implications of participant-controlled triggers.

## Configuration

Configuration is resolved once with normal agent startup validation:

| Variable | Default | Hard maximum / behavior |
|---|---:|---|
| `LOREBOOK_ENABLED` | `true` | Boolean |
| `LOREBOOK_ALLOW_GROUP_CHATS` | `false` | Boolean |
| `LOREBOOK_SCAN_DEPTH` | `10` | `100` messages |
| `LOREBOOK_MAX_ENTRIES` | `20` | `64`; `0` injects none |
| `LOREBOOK_TOKEN_BUDGET` | `1200` | `16000`; `0` injects none |

Invalid booleans, integers, and above-limit values fail configuration instead
of silently changing behavior.

## Security And Bounds

Lorebooks are prompt-visible. Default and named-layer writes scan raw JSON and
decoded prompt-visible fields for prompt injection; any unsafe entry or
cross-entry combination rejects the write. Scanning applies Unicode NFKC,
removes invisible formatting characters, collapses whitespace, and rejects
direct instruction-override phrases in addition to high-severity sanitizer
matches. Loading repeats validation for legacy or direct-database documents:
unsafe entries are skipped, while an unsafe cross-entry combination skips that
file. Decoded content from all selected entries receives one more combined scan
across file boundaries before rendering. Lorebook wrapper tags in names and
content are neutralized.

This lexical defense is intentionally conservative. Benign lore containing
instruction-override wording can be rejected and should be rephrased.

Write complete lorebook documents with `memory_write` and `append: false`.
Lorebook append is rejected before creating or mutating storage.

Hard runtime bounds:

- 16 direct files (database listing reads at most 17), 1 MiB per file, 2 MiB
  aggregate source.
- 512 entries per file, 1,024 entries aggregate.
- 64 primary or secondary keys per entry, 256 bytes per key.
- 512-byte labels and 64 KiB entry content.
- 100-message maximum per-entry scan depth.
- 128 KiB scan text, 131,072 key checks, 16 MiB aggregate key-comparison
  haystack bytes, and 128 KiB rendered output.
- Token budget includes header, labels, wrappers, escaped content, and separators.

Malformed or unsupported individual entries are skipped with warnings so valid
legacy siblings can load. Invalid outer JSON or shape rejects the complete
public write.

## Deferred SillyTavern Parity

V1 intentionally does not implement insertion positions/roles (`@depth`,
before/after character, Author's Note, example messages, outlets), recursion,
vector matching, groups and group scoring, sticky/cooldown/delay, macros,
character/persona filters, additional matching sources, or global matching
fallback settings. JavaScript regex keys are rejected instead of being treated
as plaintext. A dedicated management UI/tool and tokenizer-aware remaining-
context budget are also deferred.
