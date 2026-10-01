# Pairing: secrets-store-based DM auth (WI #184 design)

> **Status (2026-10-01): DESIGN — answers the five open questions of
> [WI #184](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/184).**
> The `#200` stopgap (allowFrom wipe on approve) landed separately in the same
> branch as `ffe69ed62` with regression tests. This document is the design the
> tracker item asks for; no substrate change has landed yet.

## Problem

DM pairing state lives in per-channel JSON files under `lunarwing_base_dir()`:

- `{channel}-pairing.json` — pending requests (code, sender id, TTL)
- `{channel}-allowFrom.json` — granted senders
- `{channel}-approve-attempts.json` — approve rate-limit counter

The substrate is the fragility the tracker calls out: manual flock dances,
read-through-one-fd/write-through-another asymmetry, and truncate discipline
that two sibling writers got right and `add_allow_from` got wrong (every
approval wiped all prior grants — fixed as the #200 stopgap in `ffe69ed62`,
with regressions `test_approve_preserves_existing_allow_from_entries` and
`test_approve_preserves_preexisting_file_entries`). Patching the wipe fixes
the worst symptom; the file design remains the problem. Plaintext JSON under
`base_dir` also enumerates who may DM the agent, and pairing codes sit
unencrypted on disk.

The daemon already has the right substrate: the encrypted tenant secrets
store (`src/secrets/`), used the same way by SSH hostkey pins
(`src/bridge/ssh_hostkeys.rs`), SSH key material (`src/bridge/ssh_secrets.rs`,
`SshSecretsManager`), and webhook bearer auth.

## Verified consumption map at HEAD

Verified at `64a2c8e95` (+ stopgap `ffe69ed62`):

| Consumer | Path | Role |
|---|---|---|
| WeeChat (real consumer) | `ic/channels-src/weechat/channel.wit` (DM Pairing section), `ic/channels-src/weechat/src/lib.rs:1736,1757` | Gates DMs on `pairing_allowed` merged from config + daemon-side store; sends pairing instructions on first request |
| XMPP | `ic/src/channels/xmpp/mod.rs:791-820` | `read_allow_from("xmpp")` + `upsert_request` |
| Signal | `ic/src/channels/signal.rs:165-197` | `read_allow_from("signal")` + `upsert_request` |
| WASM wrapper host fns | `ic/src/channels/wasm/wrapper.rs:610-647` | `pairing_upsert_request` / `pairing_is_allowed` / `pairing_read_allow_from` |
| Web gateway | `ic/src/channels/web/server.rs:2316,2872-2915`, `ic/src/channels/web/handlers/extensions.rs:29,43` | `/api/pairing/{channel}` list + approve; channel activation status reads allowFrom |
| CLI | `ic/src/cli/pairing.rs`, dispatch `ic/src/main.rs:186` | `lunarwing pairing list|approve` approval UX |
| Daemon wiring | `ic/src/main.rs:450` | `Arc<PairingStore>` into `WasmChannelLoader` |
| DarkIRC | `ic/channels-src/darkirc/src/lib.rs:185-186` | **Nominal only** — keeps its own workspace files (`state/{tenant}/allow_from`, `state/{tenant}/dm_policy`); never consumes the daemon `PairingStore` |

## Answers to the five open questions

### 1. Secrets-store grants keyed by what?

**Channel + sender identity (principal) — not opaque per-DM tokens.**

- `user_id` = tenant id, exactly as `SshSecretsManager` already does
  (`src/bridge/ssh_secrets.rs:42`).
- Secret `name` = `pairing_{sanitized-channel}_{sanitized-principal}`,
  following `secret_name_for_host`'s sanitization convention. `CreateSecretParams`
  lowercases names, matching the store's case-insensitive lookup.
- Secret `value` = JSON grant blob:
  `{"v":1,"channel":"weechat","principal":"irc/libera/alice!~a@host","display":"alice","granted_at":"…","granted_by":"code:AB12CD34","source":"pairing|manual|migrate"}`
- Channel enumeration (`read_allow_from`) = `list(user_id)` filtered by the
  `pairing_` prefix, principal recovered from the grant blob.

Why not tokens: over IRC/XMPP/Signal the transport-asserted sender identity
**is** the credential available to the daemon; a bearer step would require
distributing secret material to an IRC nick with no secure side channel, and
buys nothing for a threat model of *who may open a DM conversation*. The
single-use, TTL'd, rate-limited pairing code remains the bootstrap secret and
keeps its current properties (15-min TTL, max 3 pending, approve rate limit).
Moving grants into the encrypted store still removes today's plaintext
enumeration of DM-authorized senders from `base_dir`.

Pending requests and the approve counter also move into the store, as one
record per channel each (`pairing_pending_{channel}`,
`pairing_ratelimit_{channel}`) — single-record upserts preserve the max-3 and
unique-code invariants that multi-record writes could race on.

### 2. Does `dm_policy: open|allowlist|pairing` survive?

**Yes, unchanged.** This redesign replaces the storage substrate of the
`allowlist` and `pairing` modes, not the policy vocabulary:

- `open` — no store consult (unchanged).
- `allowlist` — config `allow_from` ∪ store grants; weechat already merges
  config + daemon-side lists (`channels-src/weechat/src/lib.rs:1736-1751`),
  and that merge behavior is preserved verbatim.
- `pairing` — unknown sender triggers `upsert_request`; approved code creates
  a grant in the secrets store.

Folding the vocabulary into one grant model would touch every channel's
config surface for zero security gain. Out of scope by design.

### 3. WeeChat `channel.wit` pairing records — new WIT surface or reuse?

**Reuse. No WIT change, no channel re-build forced by this item.**
`pairing-upsert-request`, `pairing-is-allowed`, `pairing-read-allow-from` and
the `pairing-upsert-result` record keep their exact signatures; only the
daemon-side implementation behind the wasm wrapper host functions
(`wrapper.rs:610-647`) changes from file I/O to the secrets store. This is the
point of the boundary: the weechat channel — the consumer that must not break
mid-flight — sees no change.

Async bridging: the host functions are sync; the secrets store is async. The
wrapper already establishes the pattern for exactly this — a dedicated
current-thread tokio runtime for blocking host-side work
(`wrapper.rs:105,387-400`, the `http_runtime`). The pairing backend uses the
same dedicated-runtime `block_on` shape from within channel execution, never
a handle to the runtime the channel itself runs on. The `FilePairingStore`
fallback keeps the sync path trivially.

### 4. Migration story for existing paired senders

**Single daemon-wide cutover with auto-migration and trivial rollback.**

- On daemon start (and lazily on first `pairing list`), for each channel with
  a legacy `{channel}-allowFrom.json`: every entry not already present as a
  grant is written as a grant with `source: "migrate"`; the file is then
  renamed to `{channel}-allowFrom.json.migrated` (kept, not deleted).
- Pending requests are **not** migrated — 15-min TTL makes them dead on
  arrival across an upgrade; senders simply re-request. The approve
  rate-limit counter (5-min window) also restarts fresh.
- Rollback: because migration only *adds* grants and never deletes the legacy
  file, an old binary rolled back reads the renamed file back (operator
  renames `.migrated` → `.json`) or reads the still-present pre-cutover file
  during the window before migration ran. No grant written by the new code is
  visible to old code, so no split-brain.
- Per-channel-kind cutover policy is unnecessary: all consuming channel kinds
  go through the same `PairingStore` API surface. DarkIRC is explicitly out of
  scope — it never consumed the daemon store; its workspace-file `allow_from`
  continues unchanged (optional follow-up item if it should adopt grants).
- WeeChat tenants in the wild need **no action**: migration runs daemon-side
  on first start after upgrade; principals (the versioned sender principal
  at `channels-src/weechat/src/lib.rs:335`) carry over byte-for-byte as grant
  keys.

### 5. Does the CLI pairing flow survive?

**Yes — same commands, re-backed internals.**
`lunarwing pairing list <channel>` and `lunarwing pairing approve <channel>
<code>` (`src/cli/pairing.rs`, dispatched at `main.rs:186`) keep their
surface; the weechat pairing-instructions string
(`lunarwing pairing approve <channel> <code>`,
`channels-src/weechat/src/lib.rs:2320-2322`) keeps working verbatim.

The CLI gains the daemon's existing bootstrap as a precondition for pairing
commands: master-key resolution (`SECRETS_MASTER_KEY` or OS keychain via
`resolve_master_key()`) and a DB handle — the same `create_secrets_store`
composition the daemon already performs. Routing CLI approve through the
gateway API instead was considered and rejected: it adds an auth surface and
breaks offline/harness use.

**No-DB mode** (`--no-db`, where `create_secrets_store` returns `None`):
pairing falls back to the file backend explicitly — a runtime choice at
composition time, not parallel per-channel code paths.

## Implementation shape

Introduce a backend trait, keep the file store as an impl:

1. **Trait extraction** — `PairingStoreBackend` with today's public surface
   (`list_pending`, `upsert_request`, `approve`, `read_allow_from`,
   `is_sender_allowed`); current struct becomes `FilePairingStore`
   (`with_base_dir` preserved). Existing 21 unit tests keep running against
   it unchanged.
2. **`SecretsPairingStore`** — `Arc<dyn SecretsStore + Send + Sync>` +
   tenant id + naming scheme above; in-process `tokio::sync::Mutex`
   re-establishes the serialization flock provided today; tested against
   `InMemorySecretsStore`.
3. **Wiring** — daemon composes secrets backend after the secrets store is
   built (AppBuilder order), file fallback when no DB; CLI gains master-key +
   DB bootstrap; wasm loader and web handlers switch to the trait object.
4. **Migration** — legacy import + `.migrated` rename, idempotent, logged at
   info with grant counts.
5. **Docs + runbook** — update `docs/architecture/WEECHAT-CHANNEL-ARCHITECTURE.md`
   pairing section and the operator runbook (migration, verification,
   rollback).

## Security notes

- Removes plaintext DM-grant enumeration and plaintext pairing codes from
  `base_dir`; both become AES-256-GCM ciphertext in the tenant secrets store.
- TTL (15 min), max pending (3), approve rate limit (10/5 min), and
  case-insensitive code matching all preserved.
- No new listeners, routes, or auth changes; web handler surfaces unchanged.
- The stopgap fix (`ffe69ed62`) closes the wipe symptom independently, so
  #200 can close on merge and this design remains the substrate fix.

## Out of scope

- DarkIRC's private workspace-file allowFrom (separate follow-up).
- Any change to the `dm_policy` vocabulary or channel configs.
- WIT/version churn in channel WASM contracts.
