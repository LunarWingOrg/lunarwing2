# Pairing: secrets-store-based DM auth (WI #184 design)

> **Status (2026-10-01): DESIGN, revision 3 — answers the five open questions
> of [WI #184](https://git.lunarwing.org/lunarwing2/-/work_items/184).**
> Revision 3 corrects the two P2 findings from the !1294 review (legacy
> matching contract; name privacy) and adopts its nonblocking hot-path
> correction. The `#200` stopgap (allowFrom wipe on approve) landed separately
> via !1283 (`6a7c589be`). The legacy-matching and concurrency regressions the
> review required are **landed executable tests** in this branch (see
> [Landed regressions](#landed-regressions-in-this-branch)); no substrate
> change has landed yet.

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
`test_concurrent_add_allow_from_preserves_all_grants`, the last landed with
this design revision). Patching the wipe fixes the worst symptom; the file
design remains the problem. Plaintext JSON under `base_dir` also enumerates
who may DM the agent, and pairing codes sit unencrypted on disk.

The daemon already has the right substrate: the encrypted tenant secrets
store (`src/secrets/`), used the same way by SSH hostkey pins
(`src/bridge/ssh_hostkeys.rs`), SSH key material (`src/bridge/ssh_secrets.rs`,
`SshSecretsManager`), and webhook bearer auth.

## Verified consumption map at HEAD

Verified at `03f5650fe` on `rarity/kimi-11-20261001-0900` (all anchors
re-read in source; production line numbers, test additions excluded):

| Consumer | Path | Role |
|---|---|---|
| WeeChat (real consumer) | `ic/channels-src/weechat/channel.wit:152-176` (DM Pairing records), `channels-src/weechat/src/lib.rs:1722-1750` (`check_sender_allowed`), `:1736` (`pairing_read_allow_from`), matcher `:254-288` (`sender_allowed_by_policy`) | Gates DMs on `pairing_allowed` merged from config + daemon-side store; sends pairing instructions (`:1756-1800`, `:2317`) on first request |
| XMPP | `ic/src/channels/xmpp/mod.rs:795-805` (own matcher over `read_allow_from`), `:801`, `:820` (`upsert_request`) | `read_allow_from("xmpp")` + `upsert_request` |
| Signal | `ic/src/channels/signal.rs:175-183` (per-message `read_allow_from`), `:197` (`upsert_request`) | Sender gating + pairing requests |
| WASM wrapper host fns | `ic/src/channels/wasm/wrapper.rs:610-647` | `pairing_upsert_request` / `pairing_is_allowed` / `pairing_read_allow_from` |
| Web gateway | `ic/src/channels/web/server.rs:528-531` (routes), `:2869-2915` (handlers), `src/channels/web/handlers/extensions.rs:29,43` | `/api/pairing/{channel}` list + approve; channel activation status reads allowFrom |
| CLI | `ic/src/cli/pairing.rs:36-37`, dispatch `ic/src/main.rs:184-186` | `lunarwing pairing list|approve` approval UX |
| Daemon wiring | `ic/src/main.rs:450`, `ic/src/channels/wasm/loader.rs:27,38` | `Arc<PairingStore>` into `WasmChannelLoader` |
| DarkIRC | `ic/channels-src/darkirc/src/lib.rs:183-188` | **Nominal only** — keeps its own workspace files (`state/{tenant}/allow_from`, `state/{tenant}/dm_policy`); never consumes the daemon `PairingStore` |

**Hot-path note (adopts the review's correction):** allowFrom reads are on
the per-message DM gate, not operator-only. Signal calls `read_allow_from`
inside `is_sender_allowed_with_pairing` per message (`signal.rs:175-183`);
weechat calls `pairing_read_allow_from` inside `check_sender_allowed` per DM
(`lib.rs:1736`); the web extensions handler reads it per activation-status
render (`extensions.rs:43`); the unchanged WIT contract exposes both
allowed/list calls to every wasm channel. The backend therefore ships a
bounded cache with defined invalidation
([Bounded cache contract](#bounded-cache-contract-dm-hot-path)) rather than
assuming operator-only consumers. The earlier revision's
"decrypt-per-grant is acceptable: lists are operator-facing" claim is
withdrawn.

## Answers to the five open questions

### 1. Secrets-store grants keyed by what?

**Channel + canonical sender identity (principal) — not opaque per-DM
tokens, and not a lossy sanitization of the principal.**

#### Grant name encoding (keyed, collision-resistant)

A grant secret name is:

```
pairing_{channel}_v1_{hex(hmac_sha256(name_key, "lunarwing-pairing-name-v1" | channel | 0x00 | principal))[0..10]}
```

- `channel` — the already-normalized channel key from `safe_channel_key`
  (`src/pairing/store.rs:81-98`), lowercased alphanumerics; safe because it
  is a small fixed vocabulary (`xmpp`, `signal`, `weechat`, …), not free-form
  sender input.
- `v1` — an encoding version tag so a future derivation change can migrate
  names deterministically (`v2` names coexist; old prefix unaffected).
- `name_key` — **HKDF-SHA256(master_key, salt = zero-filled 32 bytes, info =
  "lunarwing-pairing-name-v1")**, i.e. a name-derivation subkey domain
  separated from the per-secret encryption subkey (which uses per-secret
  random salt and info `near-agent-secrets-v1`,
  `src/secrets/crypto.rs:68-79,129-139`). Derived once at composition, held
  by the pairing backend exactly as `SecretsCrypto` holds the master key.
- The principal component is the **first 10 bytes (80 bits) of the HMAC,
  hex-encoded (20 chars)** — no new dependency; `hex` is already in the
  vendored graph via `secrecy`/`uuid`.

Properties:

- **Keyed, tenant-scoped privacy.** Secret `name` is stored as a plaintext
  column outside the encrypted value (`src/secrets/store.rs:92-113`), so the
  name itself is the only publicly-shaped handle to a grant. With the HMAC
  construction, an attacker with database read access (names visible, values
  ciphertext) **cannot test a guessed IRC principal or XMPP JID against the
  stored names without the tenant master key** — the dictionary attack the
  review demonstrated against the previous unkeyed SHA-256 name no longer
  terminates. This replaces revision 2's contradictory "keyed digest" claim
  (its `:375-376`) with an actual keyed construction.
- **Injective in practice.** HMAC-SHA256 over the exact principal bytes; an
  80-bit truncated output makes an accidental name collision between two
  *actual senders* on one tenant's channel a ~2^40 birthday event, and a
  collision still fails closed on lookup (blob verification, below) — it can
  never alias one sender onto another's grant.
- **No unbounded-name hazard.** Principals are bounded at intake (see
  below); the name is always `pairing_` + channel + `_v1_` + 20 chars.
- **Key lifecycle.** The name key's lifecycle **is the master key's** — same
  resolution (`SECRETS_MASTER_KEY` or OS keychain, `resolve_master_key()`,
  `src/secrets/mod.rs:120`), no independent rotation. The secrets store
  today has no master-key rotation flow; if one is ever added it must re-key
  names and values together (enumerate grants → decrypt under the old key →
  create under the new name → delete the old row), because a name computed
  under a new key would otherwise be unreachable. Cross-tenant scoping falls
  out of the construction: two tenants with the same principal hold
  different master keys and therefore different names.

#### Canonical principal and channel identity normalization

- `user_id` = tenant id, exactly as `SshSecretsManager` already does.
- The **canonical principal** is the channel's own versioned sender identity,
  produced by the channel before it reaches the store:
  - WeeChat: `weechat_sender_identity` (`channels-src/weechat/src/lib.rs:419-433`)
    — `account:{network}:{casefolded-account}` when the relay tags carry an
    authenticated account, else `nick:{network}:{casefolded-nick}`, with the
    IRC case mapping (`ascii`/`rfc1459`/`strict-rfc1459`, `irc_casefold`,
    `:388-400`) applied. Casefolding is *part of* the canonical form, so
    `ALICE` and `alice` are intentionally the same principal on IRC while
    `alice-bob` and `alice_bob` remain distinct.
  - XMPP: bare JID localpart+domain, lowercased per RFC 6122 domain rules
    (the store already receives `sender_bare`, `channels/xmpp/mod.rs:820`).
  - Signal: the ACI UUID from the decrypted envelope sender.
- The store re-validates on write: a principal longer than 256 chars or
  containing control characters is rejected (`InvalidChannel`-style error),
  so the HMAC input is always well-formed.

#### Grant blob (the exact identity, verified on lookup, source-tagged)

Secret `value` = JSON:

```json
{"v":1,"channel":"weechat","kind":"canonical",
 "principal":"nick:libera:alice",
 "display":"alice",
 "granted_at":"2026-10-01T06:00:00Z",
 "granted_by":"code:AB12CD34",
 "source":"pairing|manual|migrate"}
```

- `kind` distinguishes **canonical grants** (principal is the channel's
  versioned identity; written by new approvals and manual adds) from
  **legacy grants** (`kind:"legacy"`, `source:"migrate"`; principal is the
  legacy allowFrom entry's bytes verbatim — often a bare nick or `@username`).
- The **principal is retained verbatim** in the encrypted value.

`CreateSecretParams::new` lowercases secret names (`src/secrets/types.rs:173-179`)
for case-insensitive lookup; hex and the channel key are already lowercase,
so normalization is a no-op on this encoding (stated so a future maintainer
does not "fix" it into a lossy case-fold of the digest).

#### Lookup contract — strict path and legacy matcher, never mixed

This is the fix for the review's first P2: revision 2 promised byte-exact
digest lookup for *every* query while also promising that migrating legacy
entry bytes unchanged preserves today's `is_sender_allowed` matching. Those
cannot both hold — a legacy `@Alice` entry would never be found by a lookup
that hashes the presented id (`platform-id`) or a username-derived
(`alice`) name. The two matchers are now explicit and source-tagged:

1. **Strict path (canonical grants).** Derive the canonical principal from
   the presented identity per the channel rules above; compute
   `name = pairing_{channel}_v1_{hmac(canonical)}`; fetch that exact name;
   **compare the blob's `principal` byte-for-byte against the derived
   canonical principal**. A hash collision fails closed (no match), it can
   never alias one sender onto another's grant. A candidate is authorized
   only on this byte-equality — which is exactly today's id comparison
   (`store.rs:450-453`, `e.trim() == id`), so a legacy entry whose bytes
   happen to equal a canonical principal (`nick:libera:alice` added by hand
   to a file) authorizes the same sender it does today.
2. **Legacy matcher (legacy grants only).** If the strict path finds
   nothing, the legacy grant list for the channel (via the
   [bounded cache](#bounded-cache-contract-dm-hot-path)) is evaluated with
   **today's exact comparison function, unchanged**
   (`src/pairing/store.rs:443-465`): trimmed-entry == trimmed-id (exact,
   case-sensitive), else trimmed-entry lowercased == username lowercased or
   == `@` + username-without-leading-`@`. Example from the review: a legacy
   `@Alice` entry authorizes `is_sender_allowed(channel, "platform-id",
   Some("alice"))` — pinned as an executable regression (below).
3. **No cross-kind broadening.** The legacy username/`@` comparison is
   applied **only** to `kind:"legacy"` grants. Canonical grants are never
   consulted by it — a new approval for `nick:libera:alice` does not start
   authorizing every sender whose username is `alice`. Conversely the
   strict path never rewrites a legacy grant's bytes into a canonical form;
   that would silently change who matches.

`read_allow_from(channel)` returns the stored principal strings (both kinds)
for the channel — weechat matches this list in-channel with
`sender_allowed_by_policy` (`lib.rs:254-288`: exact principal equality plus
`irc_casefold(entry) == normalized_nick`), and that matcher is unchanged, so
legacy bare-nick entries keep case-folding exactly as today. Enumeration is
cache-backed, not decrypt-per-grant-per-message.

#### Why not opaque per-DM tokens

Over IRC/XMPP/Signal the transport-asserted sender identity **is** the
credential available to the daemon; a bearer step would require distributing
secret material to an IRC nick with no secure side channel, and buys nothing
for a threat model of *who may open a DM conversation*. The single-use,
TTL'd, rate-limited pairing code remains the bootstrap secret and keeps its
current properties (15-min TTL, max 3 pending, approve rate limit). Moving
grants into the encrypted store with keyed names removes today's plaintext
enumeration of DM-authorized senders from `base_dir` and from
database-read-only dictionary attacks.

#### Collision and name regressions (required at implementation time)

1. `nick:libera:alice-bob` and `nick:libera:alice_bob` produce **distinct**
   grant names, and approving both yields **two** grants.
2. A hand-corrupted blob (`principal` swapped between two grants) makes
   `is_sender_allowed` return `false` for both senders — lookup verifies, it
   never trusts the name alone.
3. Same nick on different networks (`nick:libera:alice`,
   `nick:oftc:alice`) are distinct grants.
4. RFC1459 casefold equivalence: `nick:libera:ALICE[` and
   `nick:libera:alice{` are the **same** principal (one grant), while
   `nick:libera:alice-bob` / `nick:libera:alice_bob` are not.
5. **Keyed-name privacy:** the same principal under two different master
   keys produces different names, and no stored name equals the unkeyed
   `sha256`-derived form of any candidate principal (the dictionary attack
   from the review must not terminate against names).

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
`pairing-upsert-request`, `pairing-is-allowed`,
`pairing-read-allow-from` (`channel.wit:152-176`) and the
`pairing-upsert-result` record keep their exact signatures; only the
daemon-side implementation behind the wasm wrapper host functions
(`wrapper.rs:610-647`) changes from file I/O to the secrets store. This is
the point of the boundary: the weechat channel — the consumer that must not
break mid-flight — sees no change.

Async bridging: the host functions are sync; the secrets store is async. The
wrapper already establishes the pattern for exactly this — a dedicated
current-thread tokio runtime for blocking host-side work
(`wrapper.rs:105-110`, `:390-402`, the `http_runtime`). The pairing backend
uses the same dedicated-runtime `block_on` shape from within channel
execution, never a handle to the runtime the channel itself runs on. The
`FilePairingStore` fallback keeps the sync path trivially.

### 4. Migration story for existing paired senders

**Single daemon-wide cutover with auto-migration and trivial rollback.**

- On daemon start (and lazily on first `pairing list`), for each channel with
  a legacy `{channel}-allowFrom.json`: every entry not already present as a
  grant is written as a **`kind:"legacy"` grant whose principal is the
  entry's bytes verbatim** (`source:"migrate"`), keyed by
  `hmac(entry-bytes)`; the file is then renamed to
  `{channel}-allowFrom.json.migrated` (kept, not deleted). Migration
  recomputes each key from the entry's bytes — no legacy file is rewritten,
  so a partially-migrated file re-migrates idempotently.
- Migration **never normalizes** legacy entries into `nick:*`/canonical
  principals: their exact bytes are what today's matchers compare, so the
  bytes are what must be stored. Equivalence with today's decisions is
  enforced by the landed regressions (below) at the API and wasm-host level.
- Pending requests are **not** migrated — 15-min TTL makes them dead on
  arrival across an upgrade; senders simply re-request. The approve
  rate-limit counter (5-min window) also restarts fresh.
- Rollback: because migration only *adds* grants and never deletes the
  legacy file, an old binary rolled back reads the renamed file back
  (operator renames `.migrated` → `.json`) or reads the still-present
  pre-cutover file during the window before migration ran. No grant written
  by the new code is visible to old code, so no split-brain.
- Per-channel-kind cutover policy is unnecessary: all consuming channel
  kinds go through the same `PairingStore` API surface. DarkIRC is
  explicitly out of scope — it never consumed the daemon store; its
  workspace-file `allow_from` continues unchanged (optional follow-up item
  if it should adopt grants).
- WeeChat tenants in the wild need **no action**: migration runs
  daemon-side on first start after upgrade, and the channel-side matcher is
  untouched.

### 5. Does the CLI pairing flow survive?

**Yes — same commands, re-backed internals.**
`lunarwing pairing list <channel>` and `lunarwing pairing approve <channel>
<code>` (`src/cli/pairing.rs:36-37`, dispatched at `main.rs:184-186`) keep
their surface; the weechat pairing-instructions string keeps working
verbatim (`channels-src/weechat/src/lib.rs:2317`).

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

## Concurrency contract (cross-process, both backends)

**Rule: every read-modify-write of shared pairing state is a single
database-level critical section spanning both processes, on both supported
backends.** The in-process `tokio::sync::Mutex` is retained only as cheap
intra-daemon serialization — a supplement, never the guarantee (revision 1's
in-process-mutex claim was wrong across processes; that finding stands
addressed).

Pending requests (`pairing_pending_{channel}`) and the approve rate-limit
counter (`pairing_ratelimit_{channel}`) remain single-record state, so the
critical section is one row read-modify-write inside a locked transaction:

- **PostgreSQL**: `BEGIN` + `SELECT pg_advisory_xact_lock(
  hashtextextended('pairing:{tenant}:{channel}', 0))` + row
  read-modify-write + `COMMIT` — the exact pattern already used by
  `workspace/repository.rs:1568-1590` (`lock_document_chunks`). The advisory
  lock is transaction-scoped (auto-released at commit/rollback), keyed per
  tenant+channel so different channels do not serialize against each other,
  and works on both `postgres` pools the daemon and CLI open.
- **libSQL**: `BEGIN IMMEDIATE` (`libsql::TransactionBehavior::Immediate`,
  vendored `libsql/src/transaction.rs:10-16`, via
  `transaction_with_behavior`, `libsql/src/connection.rs:180`) — the write
  transaction takes the database write lock at `BEGIN`, so a concurrent
  writer's `BEGIN IMMEDIATE` blocks until the first commits. libsql's
  SQLite core serializes writers database-wide (one writer at a time), which
  is coarser than the advisory-lock keying but correct; the
  pending/ratelimit records are per-channel so contention is negligible.
- **Grant upserts** (`pairing_{channel}_v1_{hmac}`) are single-row upserts
  and need no cross-process lock at all: the DB's own `UNIQUE (user_id,
  name)` + `ON CONFLICT DO UPDATE` (`secrets/store.rs:105`, `:372`) is
  atomic per row, and two different principals hash to different rows.

### Failure semantics (approve consumption vs grant write)

`approve(channel, code)` is a two-step operation — consume the pending code,
then write the grant. The contract defines each failure mode:

1. **Consume (pending-record write) commits first, inside the locked
   transaction.** The code is removed and the failed-attempt counter (on a
   bad code) is updated in the same transaction as the read, so the
   single-use property and rate limit hold exactly as today.
2. **Grant write failure after consumption**: the approve call returns an
   error naming the code; the pending request stays consumed. The operator
   retries by having the sender re-request (new code) or by adding the
   grant manually via the web UI. This is the same behavior as today's file
   store (where `approve` writes the pairing file before `add_allow_from`
   (`store.rs:414-417`) and a failed second write loses the code) — the
   redesign does not make it worse, and now the failure surfaces as an
   error instead of a silent partial state.
3. **Lock-acquisition timeout**: bounded retry (3 attempts, jittered
   backoff), then a distinct `PairingStoreError::Concurrency` variant so
   callers and the web handler can surface "try again" rather than
   "invalid code".

### Bounded cache contract (DM hot path)

The backend keeps an in-process cache of the **decoded** grant set
(principal, kind, display) per `(tenant, channel)`:

- **Population**: one `list` + decrypt per channel per refresh — never
  decrypt-per-grant-per-message. Cache hits serve Signal's per-message
  `read_allow_from` (`signal.rs:175-183`), weechat's per-DM
  `pairing_read_allow_from` (`lib.rs:1736`), and the web activation-status
  reads (`extensions.rs:43`). This also removes today's
  `PairingStore::new()` + file parse per Signal message.
- **TTL**: 15 seconds. Cross-process updates (CLI approve) become visible
  to the daemon within one TTL; **revoke latency is bounded by the TTL and
  documented as such** for operators. Approvals through the daemon
  invalidate the affected channel synchronously before returning, so the
  common path is immediately consistent.
- **Freshness overrides**: `approve` and manual grant writes never read the
  cache for their own decisions — the pending-consume read happens inside
  the DB critical section with a fresh read; the grant write then
  invalidates the channel cache entry.
- **Bounds**: the cache holds at most 1024 grants per channel; beyond that
  the backend falls back to uncached reads (no store-side cap, preserving
  today's unbounded allowFrom behavior).

### Multi-process race regressions (required at implementation time)

1. Two concurrent `approve`s of the same code → exactly one succeeds; the
   other sees an invalid code (single-use holds across processes).
2. Concurrent `upsert_request`s from different senders → both pending
   requests survive (max-3 accounting not corrupted).
3. Concurrent failed approves interleaved with a successful one →
   rate-limit count is exact (no lost increments).
4. CLI `approve` racing the daemon's `upsert_request` (two processes, two
   connections) → same results as the intra-process runs.
5. Kill-the-process mid-transaction (libSQL and PostgreSQL) → transaction
   rolls back; no half-consumed code, no half-written grant.

## Implementation shape

Introduce a backend trait, keep the file store as an impl:

1. **Trait extraction** — `PairingStoreBackend` with today's public surface
   (`list_pending`, `upsert_request`, `approve`, `read_allow_from`,
   `is_sender_allowed`); current struct becomes `FilePairingStore`
   (`with_base_dir` preserved). The module's existing unit tests (24 at this
   revision) keep running against it unchanged.
2. **`SecretsPairingStore`** — `Arc<dyn SecretsStore + Send + Sync>` +
   tenant id + the HKDF name key + the concurrency and cache contracts
   above; tested against `InMemorySecretsStore` for logic and real
   Postgres/libSQL test targets for the transaction/race contracts.
3. **Wiring** — daemon composes secrets backend after the secrets store is
   built (AppBuilder order), file fallback when no DB; CLI gains master-key +
   DB bootstrap; wasm loader and web handlers switch to the trait object.
4. **Migration** — legacy import + `.migrated` rename, idempotent, logged at
   info with grant counts.
5. **Docs + runbook** — update
   `docs/architecture/WEECHAT-CHANNEL-ARCHITECTURE.md` pairing section and
   the operator runbook (migration, verification, rollback, cache/TTL
   revoke-latency note).

## Landed regressions in this branch

These pin the compatibility contract as executable tests, ahead of the
substrate change (all passing; see the goals trail for run evidence):

- `ic/src/pairing/store.rs` —
  `test_legacy_username_entry_matches_by_username` (the review's exact
  example: legacy `@Alice` authorizes `("platform-id", Some("alice"))` and
  `Some("Alice")`, never the id alone, never an unrelated username);
  `test_legacy_trimmed_id_entry_matches_exact_id` (trimmed exact id match;
  `alice`/`alice-bob`/`alice_bob` stay distinct);
  `test_concurrent_add_allow_from_preserves_all_grants` (eight concurrent
  approvers; the allowlist is never observed truncated).
- `ic/src/channels/wasm/wrapper.rs` —
  `test_pairing_host_functions_preserve_legacy_matching`: the review's
  required regression on the **actual wasm path**
  (`pairing_is_allowed` host fn, `wrapper.rs:610-647`), exercising
  `ChannelStoreData`'s `Host` impl against a legacy-shaped allowFrom file.

## Security notes

- Removes plaintext DM-grant enumeration and plaintext pairing codes from
  `base_dir`; grants and codes become AES-256-GCM ciphertext in the tenant
  secrets store.
- Grant names are **keyed HMACs under a master-key-derived subkey**: an
  attacker with database read access cannot map names to candidate
  principals without the master key (revision 2's unkeyed-SHA-256
  dictionary leak is closed; see regression 5 above).
- TTL (15 min), max pending (3), approve rate limit (10/5 min), and
  case-insensitive code matching all preserved.
- No new listeners, routes, or auth changes; web handler surfaces unchanged.
- Cache holds decoded grants in daemon memory only; bounded, per-channel,
  never logged (principal strings are treated as identifiers, not secrets,
  consistent with today's in-memory allowlists).
- The stopgap fix (!1283) closes the wipe symptom independently, so #200 can
  close on merge and this design remains the substrate fix.

## Out of scope

- DarkIRC's private workspace-file allowFrom (separate follow-up).
- Any change to the `dm_policy` vocabulary or channel configs.
- WIT/version churn in channel WASM contracts.
- A secrets master-key rotation flow (names and values would re-key
  together when one exists).
