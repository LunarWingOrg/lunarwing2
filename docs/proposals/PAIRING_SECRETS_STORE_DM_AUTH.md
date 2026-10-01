# Pairing: secrets-store-based DM auth (WI #184 design)

> **Status (2026-10-01): DESIGN — answers the five open questions of
> [WI #184](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/184).**
> The `#200` stopgap (allowFrom wipe on approve) landed separately via !1283
> (`6a7c589be`) with regression tests, plus the concurrent-grant regression
> from the first attempt at this design. This document is the design the
> tracker item asks for; no substrate change has landed yet.

## Problem

DM pairing state lives in per-channel JSON files under `lunarwing_base_dir()`:

- `{channel}-pairing.json` — pending requests (code, sender id, TTL)
- `{channel}-allowFrom.json` — granted senders
- `{channel}-approve-attempts.json` — approve rate-limit counter

The substrate is the fragility the tracker calls out: manual flock dances,
read-through-one-fd/write-through-another asymmetry, and truncate discipline
that two sibling writers got right and `add_allow_from` got wrong (every
approval wiped all prior grants — fixed as the #200 stopgap in !1283, with
regressions `test_approving_second_sender_preserves_first_allow_from_entry`,
`test_duplicate_allow_from_entry_does_not_rewrite_file`, and
`test_concurrent_add_allow_from_preserves_all_grants`). Patching the wipe fixes
the worst symptom; the file design remains the problem. Plaintext JSON under
`base_dir` also enumerates who may DM the agent, and pairing codes sit
unencrypted on disk.

The daemon already has the right substrate: the encrypted tenant secrets
store (`src/secrets/`), used the same way by SSH hostkey pins
(`src/bridge/ssh_hostkeys.rs`), SSH key material (`src/bridge/ssh_secrets.rs`,
`SshSecretsManager`), and webhook bearer auth.

## Verified consumption map at HEAD

Verified at `44447d984` (+ !1283 stopgap `6a7c589be`):

| Consumer | Path | Role |
|---|---|---|
| WeeChat (real consumer) | `ic/channels-src/weechat/channel.wit` (DM Pairing section, :153-175), `ic/channels-src/weechat/src/lib.rs:1736` | Gates DMs on `pairing_allowed` merged from config + daemon-side store; sends pairing instructions on first request |
| XMPP | `ic/src/channels/xmpp/mod.rs:801,820` | `read_allow_from("xmpp")` + `upsert_request` |
| Signal | `ic/src/channels/signal.rs:181,197` | `read_allow_from("signal")` + `upsert_request` |
| WASM wrapper host fns | `ic/src/channels/wasm/wrapper.rs:610-647` | `pairing_upsert_request` / `pairing_is_allowed` / `pairing_read_allow_from` |
| Web gateway | `ic/src/channels/web/server.rs:528-531,2316,2869-2915`, `ic/src/channels/web/handlers/extensions.rs:29,43` | `/api/pairing/{channel}` list + approve; channel activation status reads allowFrom |
| CLI | `ic/src/cli/pairing.rs`, dispatch `ic/src/main.rs:184-186` | `lunarwing pairing list|approve` approval UX |
| Daemon wiring | `ic/src/main.rs:450,569` | `Arc<PairingStore>` into `WasmChannelLoader` |
| DarkIRC | `ic/channels-src/darkirc/src/lib.rs:183-188` | **Nominal only** — keeps its own workspace files (`state/{tenant}/allow_from`, `state/{tenant}/dm_policy`); never consumes the daemon `PairingStore` |

## Answers to the five open questions

### 1. Secrets-store grants keyed by what?

**Channel + canonical sender identity (principal) — not opaque per-DM
tokens, and not a lossy sanitization of the principal.**

The first revision of this design proposed
`pairing_{sanitized-channel}_{sanitized-principal}` following
`SshSecretsManager::secret_name_for_host` (`src/bridge/ssh_secrets.rs:50-58`),
which maps every non-alphanumeric character to `_`. That encoding is
**not identity-preserving**: the real-format WeeChat principals
`nick:libera:alice-bob` and `nick:libera:alice_bob` are distinct senders but
both sanitize to `pairing_weechat_nick_libera_alice_bob`, and because the
store's create operation upserts on `(user_id, name)`
(`src/secrets/store.rs:105`, `:372`) approving the second would silently
overwrite the first grant. That finding is accepted; the encoding below
replaces it.

#### Key encoding (collision-resistant)

A grant secret name is:

```
pairing_{channel}_{v1}_{base32(sha256(canonical_principal))[..16]}
```

- `channel` — the already-normalized channel key from `safe_channel_key`
  (`src/pairing/store.rs:81-98`), lowercased alphanumerics; safe because it is
  a small fixed vocabulary (`xmpp`, `signal`, `weechat`, …), not free-form
  sender input.
- `v1` — an encoding version tag so a future canonicalization change can
  migrate keys deterministically (`v2` names coexist; old prefix unaffected).
- The principal component is **`base32(SHA-256(canonical_principal))`
  truncated to 16 chars (80 bits)**, not a character-substituted form.

Properties:

- **Injective in practice.** SHA-256 over the exact canonical principal bytes;
  a 16-char base32 prefix gives an 80-bit digest. Two distinct principals
  colliding requires a ~2^40 birthday pair of *actual senders* on one tenant's
  channel — unreachable by any realistic adversary, and verified below on
  lookup regardless (a wrong-key grant fails closed, never aliases).
- **No unbounded-name hazard.** Principals are bounded at intake (IRC nicks
  ≤ 63 chars by protocol; the versioned principal is
  `{nick|account}:{network}:{casefolded-name}`, itself < 128 chars), and the
  name is `pairing_` + channel + `_v1_` + 16 chars — always short, unlike raw
  principal embedding.
- **Not secret, only collision-free.** The digest is a *name*, chosen to be
  unambiguous; the authorization decision never depends on its secrecy. The
  canonical principal is recoverable from the grant blob (below), so hashing
  the name leaks nothing that the grant value does not already hold encrypted.

#### Canonical principal and channel identity normalization

- `user_id` = tenant id, exactly as `SshSecretsManager` already does.
- The **canonical principal** is the channel's own versioned sender identity,
  produced by the channel before it reaches the store:
  - WeeChat: `weechat_sender_identity` (`channels-src/weechat/src/lib.rs:416-433`)
    — `account:{network}:{casefolded-account}` when the relay tags carry an
    authenticated account, else `nick:{network}:{casefolded-nick}`, with the
    IRC case mapping (`ascii`/`rfc1459`/`strict-rfc1459`,
    `irc_casefold`, `:405-417`) applied. Casefolding is *part of* the canonical
    form, so `ALICE` and `alice` are intentionally the same principal on IRC
    while `alice-bob` and `alice_bob` remain distinct.
  - XMPP: bare JID localpart+domain, lowercased per RFC 6122 resourceprep
    domain rules (the store already receives `sender_bare`,
    `channels/xmpp/mod.rs:820`).
  - Signal: the ACI UUID from the decrypted envelope sender.
- The store re-validates on write: a principal longer than 256 chars or
  containing control characters is rejected (`InvalidChannel`-style error), so
  the hashing input is always well-formed.

#### Grant blob (the exact canonical identity, verified on lookup)

Secret `value` = JSON:

```json
{"v":1,"channel":"weechat",
 "principal":"nick:libera:alice!~a@host",
 "display":"alice",
 "granted_at":"2026-10-01T06:00:00Z",
 "granted_by":"code:AB12CD34",
 "source":"pairing|manual|migrate"}
```

The **canonical principal is retained verbatim** in the encrypted value. Every
lookup path recomputes the digest from the presented identity, fetches that
exact name, then **compares the blob's `principal` byte-for-byte against the
canonically re-derived principal**. A hash collision therefore fails closed
(the blob comparison fails, the sender is not allowed) — it can never alias
one sender onto another's grant, which is what the lossy encoding could do.

`CreateSecretParams::new` lowercases secret names (`src/secrets/types.rs:177-179`)
for case-insensitive lookup; base32 and the channel key are already lowercase,
so normalization is a no-op on this encoding (stated so a future maintainer
does not "fix" it into a lossy case-fold of the digest).

Channel enumeration (`read_allow_from`) = `list(user_id)` filtered by the
`pairing_{channel}_` prefix, principals recovered from the decrypted grant
blobs (decrypt-per-grant on list is acceptable: lists are operator-facing,
not per-message).

#### Why not opaque per-DM tokens

Over IRC/XMPP/Signal the transport-asserted sender identity **is** the
credential available to the daemon; a bearer step would require distributing
secret material to an IRC nick with no secure side channel, and buys nothing
for a threat model of *who may open a DM conversation*. The single-use, TTL'd,
rate-limited pairing code remains the bootstrap secret and keeps its current
properties (15-min TTL, max 3 pending, approve rate limit). Moving grants into
the encrypted store still removes today's plaintext enumeration of
DM-authorized senders from `base_dir`.

#### Collision regressions (required at implementation time)

1. `nick:libera:alice-bob` and `nick:libera:alice_bob` produce **distinct**
   grant names, and approving both yields **two** grants.
2. A hand-corrupted blob (`principal` swapped between two grants) makes
   `is_sender_allowed` return `false` for both senders — lookup verifies, it
   never trusts the name alone.
3. Same nick on different networks (`nick:libera:alice`,
   `nick:oftc:alice`) are distinct grants.
4. RFC1459 casefold equivalence: `nick:libera:ALICE[` and `nick:libera:alice{`
   are the **same** principal (one grant), while
   `nick:libera:alice-bob` / `nick:libera:alice_bob` are not.

### 2. Does `dm_policy: open|allowlist|pairing` survive?

**Yes, unchanged.** This redesign replaces the storage substrate of the
`allowlist` and `pairing` modes, not the policy vocabulary:

- `open` — no store consult (unchanged).
- `allowlist` — config `allow_from` ∪ store grants; weechat already merges
  config + daemon-side lists (`channels-src/weechat/src/lib.rs:1736-1744`),
  and that merge behavior is preserved verbatim.
- `pairing` — unknown sender triggers `upsert_request`; approved code creates
  a grant in the secrets store.

Folding the vocabulary into one grant model would touch every channel's
config surface for zero security gain. Out of scope by design.

### 3. WeeChat `channel.wit` pairing records — new WIT surface or reuse?

**Reuse. No WIT change, no channel re-build forced by this item.**
`pairing-upsert-request`, `pairing-is-allowed`, `pairing-read-allow-from`
(`channel.wit:153-175`) and the `pairing-upsert-result` record keep their
exact signatures; only the daemon-side implementation behind the wasm wrapper
host functions (`wrapper.rs:610-647`) changes from file I/O to the secrets
store. This is the point of the boundary: the weechat channel — the consumer
that must not break mid-flight — sees no change.

Async bridging: the host functions are sync; the secrets store is async. The
wrapper already establishes the pattern for exactly this — a dedicated
current-thread tokio runtime for blocking host-side work
(`wrapper.rs:107,392-400`, the `http_runtime`). The pairing backend uses the
same dedicated-runtime `block_on` shape from within channel execution, never
a handle to the runtime the channel itself runs on. The `FilePairingStore`
fallback keeps the sync path trivially.

### 4. Migration story for existing paired senders

**Single daemon-wide cutover with auto-migration and trivial rollback.**

- On daemon start (and lazily on first `pairing list`), for each channel with
  a legacy `{channel}-allowFrom.json`: every entry not already present as a
  grant is written as a grant with `source: "migrate"` under the new key
  encoding; the file is then renamed to `{channel}-allowFrom.json.migrated`
  (kept, not deleted). Migration recomputes each key from the entry's bytes —
  no legacy file is rewritten, so a partially-migrated file re-migrates
  idempotently.
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
  on first start after upgrade. One caveat vs the first revision: legacy
  allowFrom entries are plain strings (often bare nicks like `alice`), so
  migrated entries keep their **legacy string** as the canonical principal
  (source `migrate`), preserving today's `is_sender_allowed` matching
  semantics (`store.rs:443-465`, exact or lowercased-`@nick` comparison)
  byte-for-byte. They are not re-derived as `nick:*` principals — that would
  silently change who matches. New approvals always use the versioned
  principal.

### 5. Does the CLI pairing flow survive?

**Yes — same commands, re-backed internals.**
`lunarwing pairing list <channel>` and `lunarwing pairing approve <channel>
<code>` (`src/cli/pairing.rs`, dispatched at `main.rs:184-186`) keep their
surface; the weechat pairing-instructions string keeps working verbatim
(`channels-src/weechat/src/lib.rs`, pairing instructions section).

The CLI gains the daemon's existing bootstrap as a precondition for pairing
commands: master-key resolution (`SECRETS_MASTER_KEY` or OS keychain via
`resolve_master_key()`) and a DB handle — the same `create_secrets_store`
composition the daemon already performs (`src/secrets/mod.rs:84-112`).
Routing CLI approve through the gateway API instead was considered and
rejected: it adds an auth surface and breaks offline/harness use.

**No-DB mode** (`--no-db`, where `create_secrets_store` returns `None`):
pairing falls back to the file backend explicitly — a runtime choice at
composition time, not parallel per-channel code paths.

Because the CLI and the daemon are **separate OS processes** sharing one DB,
the CLI's participation in the concurrency contract below is mandatory, not
optional.

## Concurrency contract (replaces the first revision's in-process mutex)

The first revision claimed an in-process `tokio::sync::Mutex` re-establishes
today's flock serialization. That is wrong across processes: the CLI
(`main.rs:184-186`) and the daemon both bootstrap their own store against the
same database, and their mutexes cannot see each other. Two concurrent
`approve`/`upsert_request` callers could read the same pending list or failed
attempt count and overwrite each other's writes; row uniqueness does not make
a read-modify-write atomic. The `SecretsStore` trait exposes CRUD/list only —
no CAS. That finding is accepted; the contract below is the replacement.

**Rule: every read-modify-write of shared pairing state is a single
database-level critical section spanning both processes, on both supported
backends. The in-process mutex is retained only as cheap intra-daemon
serialization — a supplement, never the guarantee.**

Pending requests (`pairing_pending_{channel}`) and the approve rate-limit
counter (`pairing_ratelimit_{channel}`) remain single-record state, so the
critical section is one row read-modify-write inside a locked transaction:

- **PostgreSQL**: `BEGIN` + `SELECT pg_advisory_xact_lock(
  hashtextextended('pairing:{tenant}:{channel}', 0))` + row read-modify-write +
  `COMMIT` — the exact pattern already used by
  `workspace/repository.rs:1570-1586` (`lock_document_chunks`). The advisory
  lock is transaction-scoped (auto-released at commit/rollback), keyed per
  tenant+channel so different channels do not serialize against each other,
  and works on both `postgres` pools the daemon and CLI open.
- **libSQL**: `BEGIN IMMEDIATE` (`libsql::TransactionBehavior::Immediate`,
  vendored `libsql/src/transaction.rs:10-16`, via
  `transaction_with_behavior`) — the write transaction takes the database
  write lock at `BEGIN`, so a concurrent writer's `BEGIN IMMEDIATE` blocks
  until the first commits. libsql's SQLite core serializes writers database-
  wide (one writer at a time), which is coarser than the advisory-lock keying
  but correct; the pending/ratelimit records are per-channel so contention is
  negligible.
- **Grant upserts** (`pairing_{channel}_v1_{digest}`) are single-row upserts
  and need no cross-process lock at all: the DB's own `UNIQUE (user_id, name)`
  + `ON CONFLICT DO UPDATE` (`secrets/store.rs:105`, `:372`) is atomic per
  row, and two different principals hash to different rows. (The lossy-name
  overwrite hazard the review flagged is gone with the encoding change.)

### Failure semantics (approve consumption vs grant write)

`approve(channel, code)` is a two-step operation — consume the pending code,
then write the grant. The contract defines each failure mode:

1. **Consume (pending-record write) commits first, inside the locked
   transaction.** The code is removed and the failed-attempt counter (on a
   bad code) is updated in the same transaction as the read, so the
   single-use property and rate limit hold exactly as today.
2. **Grant write failure after consumption**: the approve call returns an
   error naming the code; the pending request stays consumed. The operator
   retries by having the sender re-request (new code) or by adding the grant
   manually via the web UI. This is the same behavior as today's file store
   (where `approve` writes the pairing file before `add_allow_from`
   (`store.rs:414-417`) and a failed second write loses the code) — the
   redesign does not make it worse, and now the failure surfaces as an error
   instead of a silent partial state.
3. **Lock-acquisition timeout**: bounded retry (3 attempts, jittered backoff),
   then a distinct `PairingStoreError::Concurrency` variant so callers and
   the web handler can surface "try again" rather than "invalid code".

### Multi-process race regressions (required at implementation time)

1. Two concurrent `approve`s of the same code → exactly one succeeds; the
   other sees an invalid code (single-use holds across processes).
2. Concurrent `upsert_request`s from different senders → both pending
   requests survive (max-3 accounting not corrupted).
3. Concurrent failed approves interleaved with a successful one → rate-limit
   count is exact (no lost increments).
4. CLI `approve` racing the daemon's `upsert_request` (two processes, two
   connections) → same results as the intra-process runs.
5. Kill-the-process mid-transaction (libSQL and PostgreSQL) → transaction
   rolls back; no half-consumed code, no half-written grant.

The in-process `tokio::sync::Mutex` remains around the async backend purely
to serialize the daemon's own callers cheaply before they reach the DB lock —
reducing lock convoys, not providing correctness.

## Implementation shape

Introduce a backend trait, keep the file store as an impl:

1. **Trait extraction** — `PairingStoreBackend` with today's public surface
   (`list_pending`, `upsert_request`, `approve`, `read_allow_from`,
   `is_sender_allowed`); current struct becomes `FilePairingStore`
   (`with_base_dir` preserved). Existing 22 unit tests keep running against
   it unchanged.
2. **`SecretsPairingStore`** — `Arc<dyn SecretsStore + Send + Sync>` +
   tenant id + the key encoding above + the concurrency contract above;
   tested against `InMemorySecretsStore` for logic and real
   Postgres/libSQL test targets for the transaction/race contracts.
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
- The stopgap fix (!1283) closes the wipe symptom independently, so #200 can
  close on merge and this design remains the substrate fix.
- Grant names expose only a keyed digest, not sender identity; enumeration of
  who may DM requires the master key (decrypt of each blob).

## Out of scope

- DarkIRC's private workspace-file allowFrom (separate follow-up).
- Any change to the `dm_policy` vocabulary or channel configs.
- WIT/version churn in channel WASM contracts.
