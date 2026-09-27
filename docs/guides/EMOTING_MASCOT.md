# Emoting mascot widget (web gateway)

The web gateway shows a small animated pony/unicorn in a draggable corner
widget that mirrors the agent's expressed emotions. When the agent replies
with a roleplay action phrase — `*stamps hooves*`, `*ears perk up*` — the
mascot plays the matching sprite animation instead of staying static.

The whole feature is presentation-layer only: no message content, agent
behavior, or session state is affected.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0).** The emote parser,
sprite allowlist (`Emote::ALL_SPRITES`), wire contract, and mascot widget
were re-checked against `ic/src/channels/web/emote.rs` and
`ic/src/channels/web/static/mascot.js`; no changes were needed.

## Pipeline

```text
agent reply text
   │  (gateway outgoing-message hook: AppEvent -> SseEvent conversion)
   ▼
ic/src/channels/web/emote.rs   phrase → Emote enum → sprite name
   │  (`emotes: [...]` field on the assistant `response` SSE/WS payload,
   │   omitted when empty)
   ▼
static/mascot.js               sprite state machine + draggable widget
```

## Emote parser (`ic/src/channels/web/emote.rs`)

- Extracts italic roleplay action spans (`*stamps hooves*`) — single
  asterisks, one-line, ≤ 120 chars.
- Case-insensitive keyword table maps a phrase to one `Emote`
  (`greet`, `love`, `rage`, `jump`, `walk`, `sleep`, `alert`, `fiery`);
  first table match wins.
- Per message: duplicates removed (order preserved), capped at 4 emotes.
- Widely-used phrases map as follows: `*stamps hooves*` → `jump`,
  `*ears perk up*` → `alert`, `*nuzzles you*` → `love`,
  `*horn glows*` → `fiery`, `*trots over*` → `walk`, `*yawns*` → `sleep`,
  `*glares*` → `rage`, `*waves hello*` → `greet`.

## Wire contract

The assistant `response` SSE/WS payload carries an `emotes: [...]` array of
canonical names (`greet`, `love`, `rage`, `jump`, `walk`, `sleep`, `alert`,
`fiery`), omitted when empty:

```json
{"type": "response", "thread_id": "…", "content": "*stamps hooves* Done!",
 "emotes": ["jump"]}
```

## Widget (`static/mascot.js`, styles in `style.css`)

- 64×64 animated sprites served from `/mascot/<file>` (allowlist; embedded
  in the binary from `static/mascot/`).
- State machine: `idle` (`lunar_sleep.gif`) → `working` (`lunar_walk.gif`)
  while the agent thinks/runs tools → `emote` plays queued emote sprites
  (~2.6 s each, FIFO) → settles back to working/idle. Errors flash
  `lunar_rage.gif`; connecting plays a one-time `lunar_greet.gif`.
- Draggable via pointer events; position persists in
  `localStorage["lunar.mascot.pos"]`. The `×` button hides the widget
  (`localStorage["lunar.mascot.hidden"]`) and a small restore chip brings it
  back.
- `window.LunarMascot.onAgentEvent(type, data)` is the bridge `app.js`
  forwards SSE activity to (`open`, `working`, `response`, `error`).

## Sprite pack

64×64 animated GIFs of the pony/unicorn mascot, embedded in the binary from
`ic/src/channels/web/static/mascot/` (source art: repo `sprites/new/`):

| Emote   | File             | Fallback use       |
|---------|------------------|--------------------|
| greet   | `lunar_greet.gif`| connect greeting   |
| love    | `lunar_love.gif` |                    |
| rage    | `lunar_rage.gif` | error flash        |
| jump    | `lunar_jump.gif` |                    |
| walk    | `lunar_walk.gif` | working (busy)     |
| sleep   | `lunar_sleep.gif`| idle fallback      |
| alert   | `lunar_sup.gif`  |                    |
| fiery   | `lunar_fiery.gif`|                    |

To add a sprite: drop `lunar_<name>.gif` into
`ic/src/channels/web/static/mascot/`, add the name to
`Emote::ALL_SPRITES` + keyword table in `emote.rs`, embed it in the
`mascot_sprite_handler` match in `server.rs`, and map the wire name in
`EMOTE_SPRITES` in `static/mascot.js`.

## Tests

- `cargo test --lib channels::web::emote` — phrase→enum→sprite mapping.
- `cargo test --lib channels::web::types` — `emotes` field is parsed on the
  `AppEvent::Response` → `SseEvent` conversion and omitted when empty.
