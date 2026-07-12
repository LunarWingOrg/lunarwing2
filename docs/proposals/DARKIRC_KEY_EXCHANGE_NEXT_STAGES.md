# DarkIRC Secure Key Exchange: Next Stages

**Status:** Implementation plan only. This document does not change the
authoritative design in `docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md` and does
not claim that the proposed APIs or commands exist.

**Scope:** The next managed-key stages for multi-tenant DarkIRC, building on the
Phase 1 and read-only Phase 2 foundations already present in this worktree.

**Primary implementation boundary:** `ic/src/darkirc_key_manager.rs` remains the
typed owner of key state, parsing, validation, and config/ledger transactions.
`ic/src/bin/lunarwing-darkirc-key-helper.rs` remains the short-lived process
boundary. `ic/scripts/lunarwing-mt-admin.sh` remains the only operator and
service-lifecycle entry point for both systemd user units and OpenRC.

## 1. Purpose and Phase Boundary

The authoritative proposal describes a staged, public-only exchange in which
each side generates its own per-contact keypair, confirms the peer fingerprint
through an independently authenticated channel, and commits the contact through
a recoverable transaction. The landed work provides the prerequisite ownership,
inspection, adoption, and migration layer. It does not yet provide that exchange.

The following is complete in this worktree:

- Contact-preserving baseline updates. `generate_darkirc_config()` renders the
  non-secret baseline and pipes it to helper command `rewrite-baseline`, which
  calls `update_baseline`; existing contacts and accepted unknown TOML fields are
  preserved.
- A shared, journaled config/ledger commit path. `commit_transaction()` writes
  and validates same-directory candidates, persists a secret-free journal, then
  commits ledger before config. `recover()` deterministically handles the landed
  journal phases.
- Read-only `list`, `status`, and `doctor`, backed by `inspect_contacts()` and
  `doctor_contacts()`. These report names, fingerprints, state, and sanitized
  issue codes without exposing private keys.
- Explicit legacy adoption through `adopt_contacts()` /
  `adopt_contacts_at()`. Adoption leaves the active TOML unchanged, rejects
  local private-key or peer-key reuse, and writes metadata-only generation-zero
  ledger entries as `legacy-noncompliant`; it does not force rotation.
- The secret-bearing `darkirc-contacts-v1` export/stage/import/validate path,
  including scope binding, compatibility attestation, target-baseline merge,
  replay-safe staging, generic-archive exclusion, and migration readiness checks.
- Stable 128-bit `darkirc_scope_id` allocation and uniqueness enforcement in the
  root-owned ports registry.
- A strict health probe already exposed as `darkirc-health <tenant> --strict`.
  It checks both DarkIRC services through mt-admin's init abstraction and
  requires authenticated adapter health with `status=ok` and
  `irc_connected=true`. No exchange or activation transaction calls it yet.

The following is not implemented:

- Generator invocation for managed per-contact keypairs.
- Public Exchange V1 parsing/canonicalization, pending exchange records,
  transcript binding, fingerprint-confirmation enforcement, consumed-offer
  replay records, or the `prepare`, `respond`, `complete`, and `cancel` verbs.
- An activation-aware journal, `apply`, `confirm-roundtrip`, per-contact rollback
  snapshots, decision deadlines, overdue resolution, or targeted
  `restart-darkirc` lifecycle control.
- Rotation, revocation tombstones, compromised-key no-rollback handling, or
  same-host two-tenant commit coordination.
- Onboarding, web, QR, or trusted-channel delivery surfaces.

Two details in the landed code should be stated precisely:

1. `public_fingerprint()` exists at
   `ic/src/darkirc_key_manager.rs:864`, but it is currently a private function,
   not part of the module's public API. It validates canonical Base58 encoding,
   decodes exactly 32 nonzero bytes, and returns `sha256:<lowercase hex>` over
   those decoded bytes. The exchange work should expose this behavior through a
   typed public API rather than duplicate it.
2. `darkirc-health --strict` is already implemented in
   `ic/scripts/lunarwing-mt-admin.sh:6631` and dispatched near line 8156. The
   missing work is to make it a mandatory activation gate and connect its result
   to the durable exchange state and rollback policy.

## 2. Existing Contracts and Extension Seams

### 2.1 Typed key-manager surface

The public surface in `ic/src/darkirc_key_manager.rs` currently includes:

| Existing API | Landed contract | Next-stage use |
| --- | --- | --- |
| `TenantPaths::for_tenant()` | Resolves only `/home/<tenant>/lunarwing/state/darkirc`, requires expected UID/GID, and rejects arbitrary paths | Keep as the only production path constructor; all new state stays relative to this capability-scoped root |
| `inspect_contacts()` | Shared-lock snapshot of active TOML plus optional ledger state | Extend the returned status projection with managed exchange, activation, deadline, and generation metadata without returning raw public artifacts or secrets |
| `doctor_contacts()` | Read-only consistency, reuse, unknown-field, and transactional-state diagnostics | Add expired exchange, replay conflict, rollback deadline, tombstone/config drift, and activation-recovery issue codes |
| `adopt_contacts()` / `adopt_contacts_at()` | Metadata-only, idempotent legacy adoption with injected crash points | Preserve unchanged; adopted contacts may enter managed operation only through an explicitly verified rotation |
| `read_ledger()` | Reads and scope-checks `key-exchange/ledger.json` | Add a version-aware normalized view; do not silently reinterpret existing v1 records |
| `export_migration()` / `stage_migration()` / `import_migration()` | Structured same-scope contact migration with exact attestation and secret-safe staging | Extend only after managed ledger/tombstone versioning is frozen; unresolved pending, activation, rotation, or rollback state must continue to block export |
| `validate_migration()` / `validate_migration_unbound()` | Read-only typed validation returning only the public scope ID | Retain for old archives and add explicit validation for the managed migration version rather than accepting ambiguous fields |
| `migration_ready()` | Requires settled directories and a complete config/ledger fingerprint mapping | Add managed-state readiness rules: only settled terminal states may migrate |
| `update_baseline()` / `update_baseline_at()` | Contact-preserving baseline update through the shared journal | Keep every managed writer on the same lock and semantic merge path |
| `recover()` | Explicit recovery for landed config/ledger journals | Extend to pending, activation, rollback, rotation, revocation, and local-pair coordinator phases; normal commands must run or require recovery before mutation |

The current public `LedgerContact` contains only `state`, `peer_fingerprint`, and
`generation`. `JournalFile` is private and currently contains hashes, candidate
paths, operation, scope, and one of three commit phases. Its parser permits only
`baseline-update`, `legacy-adopt`, and `migration-import`. These types are too
small to authorize exchange transitions. The next implementation must version
them explicitly rather than encode security decisions in free-form strings.

`validate_ledger()` already accepts the future-looking strings `Installed`,
`LocallyActivated`, `PeerVerified`, `Expired`, `Cancelled`, and `Revoked`, but no
landed mutator creates or authorizes those transitions. It does not accept the
proposal's internal `Installing`, `RolledBack`, or `VerificationOverdue` states.
String acceptance must not be mistaken for an implemented state machine.

`ContactSummary.peer_fingerprint` and `LedgerContact.peer_fingerprint` are both
derived from the active contact's `dm_chacha_public`; inspection defaults the
state to `legacy-unmanaged` and overlays the ledger state only when scope and
fingerprint match. The ledger contains no private key. Landed journal phases are
exactly `Prepared`, `LedgerCommitted`, and `ConfigCommitted`, with crash
injection available after the journal, ledger, and config commits.

### 2.2 Existing state layout

The module already defines these paths:

```text
/home/<tenant>/lunarwing/state/darkirc/
  darkirc_config.toml
  key-exchange/
    update.lock
    ledger.json
    candidates/
    transactions/
    pending/                 reserved, not yet created or used
    rollback/                reserved, not yet created or used
    darkirc-contacts-v1.json
    darkirc-contacts-v1.staged.json
```

`prepare_directories()` currently creates only `key-exchange/`, `candidates/`,
and `transactions/`. `pending/` and `rollback/` are only reserved names and
settled-state guards. New code must create them as tenant-owned mode `0700`
directories using the existing no-follow and ownership checks; every contained
file must be single-link mode `0600` and created with `create_new` semantics.

The active TOML remains the sole authoritative long-lived store of active
private contact keys. Pending records, config candidates, one rollback snapshot,
and an explicitly protected migration archive are bounded operational copies,
not a second contact database.

### 2.3 Helper and mt-admin seams

`lunarwing-darkirc-key-helper` currently accepts bounded stdin for baselines and
migration manifests, validates the installed `/usr/local/bin/darkirc` against
the root-owned compatibility attestation, resolves only `TenantPaths`, and emits
metadata-only JSON. New commands should follow the same structure:

- public artifacts enter through stdin or a no-follow file descriptor;
- private generator output remains in captured pipes and zeroized memory;
- only public artifacts or sanitized status objects may reach stdout;
- no raw key value is accepted through argv or the environment;
- the helper remains short-lived and runs as the tenant with a scrubbed
  environment.

`lunarwing-mt-admin.sh` already provides the seams that the next stages should
extend:

- `darkirc_load_compatibility()` and `run_darkirc_key_helper()` for attested,
  tenant-identity helper invocation;
- `ensure_darkirc_scope_id()` and `validate_existing_darkirc_scope_id()` for the
  stable registry identity;
- `darkirc_contact_helper()` and the top-level `darkirc-contact` dispatcher;
- `darkirc_writer_lock()` for root-side non-config writers, alongside the Rust
  helper's parent and `update.lock` coordination;
- `_systemctl_user()` and the OpenRC branches inside mt-admin for init-specific
  lifecycle work;
- `darkirc_health_strict()` for the strict daemon/adapter/IRC connectivity gate.

The existing public mt-admin verbs are `list`, `status`, `doctor`, `adopt`,
`export-migration`, `stage-migration`, `import-migration`, `migration-ready`,
`recover`, and `validate-migration`. No next-stage verb should be exposed by a
web, onboarding, or migration wrapper before it exists here.

### 2.4 Migration compatibility seam

The shell export preflight already treats `PeerVerified`, `Revoked`, `Expired`,
and `Cancelled` as settled states. The Rust `export_migration()` path currently
calls `validate_manifest_ledger(..., true)`, which accepts only
`legacy-active` and `legacy-noncompliant`. Therefore managed contacts cannot yet
be exported even though the shell anticipates them.

Before enabling managed writes, choose and implement an explicit managed ledger
and migration version. The recommended approach is:

1. Continue to parse and validate the landed ledger and
   `darkirc-contacts-v1` archives exactly as they exist.
2. Add a versioned managed ledger representation with stable contact IDs,
   exchange history, consumed IDs, activation decisions, and revocation
   tombstones.
3. Upgrade a v1 ledger only under the existing lock and journal, preserving
   legacy generation-zero semantics.
4. Use a new managed migration schema if the additional fields change the
   meaning of `darkirc-contacts-v1`; do not silently broaden a supposedly frozen
   secret-bearing archive contract.
5. Continue to reject export when pending, candidate, journal, rollback,
   `Installed`, `LocallyActivated`, rotation-in-progress, or
   `VerificationOverdue` state exists.

## 3. Cross-Stage Security Contract

Every next-stage API and verb must uphold these rules, regardless of operation:

1. **Fingerprint confirmation is an authorization input.** `respond`,
   `complete`, rotation completion, revocation, and round-trip confirmation take
   a validated full fingerprint value. An unattended call without the expected
   fingerprint fails. Interactive mode requires entry or scan of the full
   fingerprint. `--yes` may acknowledge an operational warning but can never
   substitute for fingerprint entry.
2. **Validation precedes state change.** Reject unknown schema or digest,
   compatibility profile, key format, artifact role, offer binding, contact
   binding, generation relationship, transcript hash, duplicate JSON field,
   unknown field, expiry, or fingerprint mismatch before creating pending state,
   reserving an ID, changing the ledger, or touching the active TOML.
3. **Private material stays inside enumerated boundaries.** A private key may
   exist only in the generator, its captured private pipe, zeroized helper
   memory, protected pending/candidate/active/rollback files, or an explicitly
   protected migration archive. It never appears in argv, environment variables,
   stdout, stderr, logs, traces, audit fields, public artifacts, QR data,
   browser responses, repo files, or `/tmp`.
4. **Replay is idempotent; conflict is inert.** The same canonical artifact,
   operation, fingerprint, scope, contact ID, and state returns the same public
   artifact or committed status. Reuse of an ID with different canonical bytes,
   role, contact, binding, generation, or transcript returns a distinct nonzero
   conflict without mutation.
5. **Names are display metadata.** Authorization binds the stable tenant scope,
   random local/peer contact IDs, decoded-key fingerprints, offer IDs,
   transcript, operation, and locally authorized generation. A nickname alone
   never installs, replaces, rotates, or revokes a key.
6. **All writers serialize.** The Rust parent lock and `update.lock` protect
   key state and config/ledger transactions. Mt-admin takes its fixed root-side
   writer lock when coordinating env or service-adjacent writes. Multi-tenant
   local operations acquire tenant locks in a deterministic scope-ID order.
7. **Recovery is explicit and hash-based.** New operations first prove that old
   migration and transaction state is settled. If recovery is required, the
   supported `darkirc-contact recover` path runs before retry. No code guesses
   from filenames or frees a consumed offer after a partial commit.
8. **Lifecycle is init-agnostic.** The typed helper never calls init tools.
   Mt-admin owns systemd user and OpenRC actions. Onboarding, web, import, and
   health wrappers contain no bare tenant `systemctl` or `rc-service` calls.

## 4. Next Stage A: Staged Public-Offer Exchange

### 4.1 Objective

Implement Public Exchange V1 and the core state machine without adding an
online trust service. Each side generates a unique local keypair, exchanges only
role-bound public JSON, verifies the independently obtained peer fingerprint,
and installs the contact through the existing semantic, journaled updater.

The initial delivery should support `prepare`, `respond`, `complete`, `cancel`,
read-only status, and explicit deferred activation. Automatic activation remains
disabled until Stage C wires the strict health and rollback lifecycle.

### 4.2 Artifact and type design

Add deny-unknown-fields typed structures for the fields defined by Public
Exchange V1:

- fixed `schema` and `artifact_role` (`initiator_offer` or
  `responder_response`);
- random at-least-128-bit `offer_id` and response `in_reply_to`;
- random stable `sender_contact_id` and responder copies of
  `intended_peer_contact_id` and `intended_peer_fingerprint`;
- approved generator profile, measured binary digest, key format, public key,
  and recomputed public fingerprint;
- local generation, bounded creation/expiry timestamps, optional untrusted
  display label, and optional rotation predecessor fingerprint.

The public artifact intentionally omits the OS tenant name, filesystem path, and
`darkirc_scope_id`. Scope binding is local: the helper stores and checks the
registry-derived scope in protected pending, ledger, and journal state whenever
an artifact is accepted. An artifact replayed under another local scope therefore
fails against local state rather than disclosing that scope to the peer.

Freeze one canonical JSON byte encoding before enabling any write verb. The
freeze must define field order, integer and timestamp encoding, string escaping,
duplicate-key rejection, unknown-field rejection, size limits, and whether an
otherwise valid non-canonical input is normalized or rejected. Check in fixed
initiator, responder, and transcript vectors. The transcript is SHA-256 over the
frozen domain separator `lunarwing.darkirc.exchange.v1` followed by the
canonical initiator then canonical responder artifacts in that role order, with
the exact framing included in the vectors.

Promote fingerprint handling to a typed public contract. Prefer a
`PublicFingerprint` strong type with `parse()` and
`from_encoded_public_key()` methods, while keeping a compatibility
`public_fingerprint()` wrapper if callers need the named function. Hash decoded
canonical 32-byte key data exactly as the landed private helper does; reject any
other digest prefix, length, case, or key encoding.

Represent states and operations as enums, not strings:

```text
ExchangeRole       = InitiatorOffer | ResponderResponse
ExchangeOperation  = Initial | Rotate
ExchangeState      = Prepared | PeerReceived | Confirmed | Installed
                     | LocallyActivated | PeerVerified | VerificationOverdue
                     | RolledBack | Expired | Cancelled | Revoked
```

Add a versioned, secret-bearing pending record under
`key-exchange/pending/<offer-id>.json`. It contains the local private key,
canonical public artifacts, local scope/contact binding, requested nickname,
operation, generation, timestamps, and state. It is never serialized through a
public result type. Add separate public result/status types whose fields are
explicitly limited to IDs, fingerprints, timestamps, state, and public artifact
bytes or a public artifact path.

### 4.3 Proposed key-manager APIs

The exact Rust parameter grouping can follow local style, but the module should
expose these capabilities as typed functions:

```rust
pub fn validate_public_exchange(bytes: &[u8], expected_role: ExchangeRole)
    -> Result<ValidatedPublicArtifact>;

pub fn prepare_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    request: &PrepareExchange,
    generator: &AttestedGenerator,
) -> Result<PublicExchangeResult>;

pub fn respond_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    offer_bytes: &[u8],
    request: &RespondExchange,
    expected_peer_fingerprint: &PublicFingerprint,
    generator: &AttestedGenerator,
) -> Result<PublicExchangeResult>;

pub fn complete_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    peer_artifact: &[u8],
    request: &CompleteExchange,
    expected_peer_fingerprint: &PublicFingerprint,
) -> Result<ExchangeStatus>;

pub fn cancel_exchange(
    paths: &TenantPaths,
    scope_id: &str,
    exchange_id: &ExchangeId,
) -> Result<ExchangeStatus>;

pub fn inspect_exchanges(
    paths: &TenantPaths,
    scope_id: &str,
) -> Result<Vec<ExchangeStatus>>;
```

Add `_at` variants or an injected clock for prepare/respond/complete/expiry
tests, matching the existing deterministic `*_at` testing pattern. Add an
internal generator interface so unit tests can provide deterministic output,
while production uses only the attested `/usr/local/bin/darkirc` profile. The
production generator command contains only the fixed generator flag; both
stdout and stderr are captured, parsed without echoing, and zeroized. Unknown,
partial, duplicate, malformed, all-zero, wrong-length, or non-canonical output
maps to sanitized error categories.

Generalize `commit_transaction()` behind a typed `TransactionOperation` and a
versioned journal. Exchange journals add contact IDs, offer IDs, transcript,
generation, expected old/new hashes, and pending/candidate references, but no
raw keys. Keep the existing v1 journal recovery parser intact for already
landed baseline/adoption/migration transactions.

### 4.4 Mt-admin and helper verbs

Add the following supported mt-admin surface and mirror it in the helper
dispatcher and `run_darkirc_key_helper()` compatibility whitelist:

```text
darkirc-contact prepare <tenant> <contact> --out <file|-> [--expires <duration>] [--json]
darkirc-contact respond <tenant> <contact> --in <file|-> --out <file|-> \
  --expect-peer-fingerprint <sha256> [--json]
darkirc-contact complete <tenant> <contact> --in <file|-> \
  --expect-peer-fingerprint <sha256> --defer-apply [--json]
darkirc-contact cancel <tenant> --exchange-id <id> [--json]
darkirc-contact exchanges <tenant> [--json]
```

Rules for this surface:

- `prepare` is the only first-contact write that does not yet know a peer
  fingerprint. It creates protected local pending state and emits only the
  public initiator artifact.
- `respond` requires the independently obtained initiator fingerprint before it
  creates the responder's pending secret. Structural validation alone is not
  sufficient.
- `complete` requires the independently obtained peer fingerprint on both
  roles. For the first release it must use `--defer-apply` and stop at
  `Installed`; Stage C removes the mandatory deferral only after rollback is
  proven.
- Interactive mt-admin may prompt for the full fingerprint and then pass that
  public value to the helper. Non-interactive mode requires
  `--expect-peer-fingerprint`. A supplied `--yes` with no expected fingerprint
  is rejected before generator invocation or file creation.
- `--in -` and `--out -` are public-only streams. Named output files use
  no-follow `create_new`, mode `0600`, and never overwrite an existing path.
  Input and output paths must not select tenant state, env files, or arbitrary
  config paths.
- `--out -` and `--json` are mutually exclusive because both would own stdout.
  With `--out -`, stdout is exactly the canonical public artifact and status goes
  to sanitized stderr. With a named `--out`, `--json` may emit the public status
  object on stdout. Input from `--in -` remains compatible with either output
  mode.
- Human and JSON output contain only contact label, stable contact ID,
  exchange/offer/transaction ID, state, fingerprints, expiry, and public
  artifact/path. Sanitized diagnostics go to stderr.

### 4.5 State transitions

| Current state | Event and preconditions | Next state | Durable effect |
| --- | --- | --- | --- |
| none | `prepare`; attested generator succeeds; contact is unoccupied | `Prepared` | Create pending local pair and canonical initiator artifact |
| none | `respond`; canonical initiator is valid and its full fingerprint matches expected | `Prepared` | Create responder pending pair and byte-stable response bound to initiator ID/contact/fingerprint |
| `Prepared` | Valid opposite-role artifact is bound to this exchange and unexpired | `PeerReceived` | Record canonical peer artifact digest and transcript candidate |
| `PeerReceived` | Full expected fingerprint matches recomputed peer fingerprint | `Confirmed` | Record authorization evidence as fingerprint and timestamp, never as a generic yes flag |
| `Confirmed` | Config/ledger candidates validate and ledger-first journal commits | `Installed` | Install contact and reserve offer IDs/transcript/generation; do not claim service health |
| `Prepared` or `PeerReceived` | Expiry observed | `Expired` | Delete pending private key; retain consumed/expired ID tombstone |
| `Prepared`, `PeerReceived`, or `Confirmed` | Explicit cancel | `Cancelled` | Delete pending private key; retain cancelled ID tombstone |
| `Installed` | Identical retry | `Installed` | Return current status, no rewrite |

Any different key or transcript for an occupied nickname is a conflict and must
direct the operator to rotation. A replayed offer ID with byte-identical
canonical content returns the same response or status. A reused ID with
different content fails nonzero without changing pending state, ledger, journal,
config, or artifact files.

### 4.6 Stage A security invariants

- Artifact schema, role, key format, generator profile/digest, decoded public
  key, fingerprint, expiry, `in_reply_to`, intended contact ID/fingerprint, and
  transcript are all checked inside the same locked operation before mutation.
- Sender labels, remote generation, remote contact IDs, and
  `previous_fingerprint` are untrusted consistency hints. Only local ledger state
  authorizes contact occupation and generation.
- Each generated local private key is checked for non-reuse against existing
  local contacts and pending operations using ephemeral equality tags; the tags
  are not persisted or logged.
- Pending records are bounded by count, bytes, and expiry. Cleanup retains only
  metadata needed to reject replay.
- Cancellation and expiry never make an offer ID reusable.
- Public artifact output types cannot serialize the pending private field by
  construction.

### 4.7 Stage A test strategy

Extend Rust unit tests in `ic/src/darkirc_key_manager.rs` and helper tests in
`ic/src/bin/lunarwing-darkirc-key-helper.rs` with:

- fixed canonical initiator/response/transcript vectors and property tests for
  round-trip canonicalization;
- duplicate/unknown fields, unknown schema/role/digest/profile/key format,
  malformed Base58, wrong lengths, all-zero keys, non-canonical encodings,
  oversized artifacts, bad timestamps, expiry, and bounded clock skew;
- every legal and illegal state transition;
- wrong `in_reply_to`, swapped roles, wrong intended contact ID/fingerprint,
  response reuse across two offers, transcript mismatch, and remote-generation
  attempts to replace local state;
- missing expected fingerprint, partial/prefix/case-mismatched fingerprint, and
  `--yes`-only rejection before mutation;
- byte-identical replay success and conflicting replay nonzero with before/after
  filesystem hashes proving no mutation;
- concurrent prepare/respond/complete, concurrent `patch-env`, case-colliding
  nicknames, TOML-quoted nickname injection, separators/control characters,
  cross-tenant scope/path attempts, intermediate/final symlinks, hard links,
  wrong owner/mode, and lock timeouts;
- generated-private-value scans across argv, environment, stdout, stderr,
  helper errors, shell tracing, logs, public JSON, and artifacts.

Extend the existing shell harnesses:

- `test-darkirc-config-ownership.sh`: race/retry a baseline rewrite with a staged
  exchange and prove unrelated settings, contacts, and secrets survive.
- `test-darkirc-writer-lifecycle.sh`: dispatch all new verbs through mt-admin,
  prove read-only exchange inspection does not allocate scope, and prove helper
  invocation remains tenant-scoped and init-free.
- `test-darkirc-migration-safeguards.sh`: reject export/import/upgrade while
  pending, confirmed, installed, or exchange-journal state is unresolved; prove
  public offers are not smuggled into the secret migration contract.
- `test-darkirc-adapter-secret-preservation.sh` and
  `test-darkirc-tenant-env-boundary.sh`: prove new dispatch does not rewrite or
  expose adapter credentials.
- `test-darkirc-health.sh`: prove Stage A never reports health or
  `LocallyActivated` merely because contact installation succeeded.

A focused `test-darkirc-public-exchange.sh` may be added during implementation
for end-to-end CLI fixtures, but it supplements rather than replaces the named
regression harnesses above.

## 5. Next Stage B: Rotation and Revocation

### 5.1 Objective and dependency

Reuse Public Exchange V1 and the same transaction engine for managed lifecycle
changes. Rotation and revocation must not be enabled until Stage A role binding,
replay handling, and stable contact IDs are complete and Stage C's activation
and rollback primitives are available. The API can be designed alongside Stage
C, but live cutover is gated on strict activation and rollback tests.

### 5.2 Rotation design

Rotation is a new exchange operation, never an implicit response to a changed
key. Keep the current generation active while both peers prepare and confirm the
next generation. The rotation transcript binds:

- both stable contact IDs;
- both current public fingerprints;
- both proposed public fingerprints;
- initiator and responder offer IDs;
- the role-bound transcript hash;
- each side's locally authorized next generation;
- the bounded maintenance deadline.

Remote generation values and `previous_fingerprint` remain hints. Each local
ledger requires the current peer fingerprint and computes exactly
`current_generation + 1`; skips, rollback of generation counters, and wraparound
fail closed.

Add typed APIs along these lines:

```rust
pub fn prepare_rotation(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &ContactId,
    current_peer_fingerprint: &PublicFingerprint,
    generator: &AttestedGenerator,
) -> Result<PublicExchangeResult>;

pub fn respond_rotation(
    paths: &TenantPaths,
    scope_id: &str,
    offer_bytes: &[u8],
    request: &RotationRequest,
    current_peer_fingerprint: &PublicFingerprint,
    expected_new_peer_fingerprint: &PublicFingerprint,
    generator: &AttestedGenerator,
) -> Result<PublicExchangeResult>;

pub fn complete_rotation(
    paths: &TenantPaths,
    scope_id: &str,
    peer_artifact: &[u8],
    request: &RotationRequest,
    current_peer_fingerprint: &PublicFingerprint,
    expected_new_peer_fingerprint: &PublicFingerprint,
) -> Result<ExchangeStatus>;

pub fn revoke_contact(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &ContactId,
    current_peer_fingerprint: &PublicFingerprint,
    disposition: RevocationDisposition,
) -> Result<ExchangeStatus>;
```

Extend mt-admin with:

```text
darkirc-contact rotate prepare <tenant> <contact> \
  --current-peer-fingerprint <sha256> --out <file|->
darkirc-contact rotate respond <tenant> <contact> --in <file|-> --out <file|-> \
  --current-peer-fingerprint <sha256> --expect-peer-fingerprint <sha256>
darkirc-contact rotate complete <tenant> <contact> --in <file|-> \
  --current-peer-fingerprint <sha256> --expect-peer-fingerprint <sha256> \
  --maintenance-deadline <timestamp>
darkirc-contact revoke <tenant> <contact> \
  --current-peer-fingerprint <sha256> [--compromised]
darkirc-contact pair-local <tenant-a> <contact-a> <tenant-b> <contact-b> ...
```

The CLI must warn before rotation commit that DMs may fail between the two
cutovers and delayed messages from the old generation may become undecryptable.
That warning can require an acknowledgement, but the acknowledgement still does
not replace either current or proposed fingerprint input.

### 5.3 Rotation transitions and cutover ordering

Use an operation-specific phase in addition to the shared contact state so an
old active generation remains unambiguous:

| Phase | Active TOML | Allowed next event |
| --- | --- | --- |
| `RotationPrepared` | Old generation | Receive role-bound peer artifact or cancel |
| `RotationPeerReceived` | Old generation | Confirm current and proposed fingerprints |
| `RotationConfirmed` | Old generation | Enter maintenance window and install locally |
| `RotationInstalling` | Ledger reserves next generation | Commit config, recover, or fail closed |
| `RotationLocallyActivated` | New generation | Peer cutover and bidirectional encrypted DM check |
| `RotationPeerVerified` | New generation | Delete old rollback snapshot and old pending private material |
| `RotationRolledBack` | Old generation, only if not compromised | Retry the same transcript or cancel |

Two hosts cannot commit atomically. The first side to switch reports only local
activation; pair success requires explicit round-trip confirmation after both
cut over. If the maintenance deadline expires and the old generation is not
suspected compromised, each operator may explicitly roll back its own side. If
compromise is suspected, record an irreversible `compromised` disposition,
disable or revoke the contact, and require a fresh verified exchange. No
recovery or convenience flag may restore the compromised generation.

For `pair-local`, mt-admin resolves both registry tenants, rejects identical or
conflicting scopes, and acquires locks in stable scope-ID order. Each tenant
helper stages and validates its own config without exporting its private half.
A root-owned coordinator journal contains only scope/contact/transaction IDs,
hashes, and phases. It commits both local ledger reservations, then both configs,
and records each activation result. A partial commit is recovered from the
coordinator plus the two tenant-local journals; it never copies one tenant's
private key into the other tenant's state or the coordinator.

### 5.4 Revocation ordering

Revocation requires the current peer fingerprint and stable contact ID, not a
nickname. Its ordering is intentionally availability-sacrificing:

1. Validate scope, contact ID, current fingerprint, ledger state, and absence of
   another unresolved mutation.
2. Write and fsync a secret-free revocation journal.
3. Commit and fsync the ledger tombstone, including contact ID, fingerprint,
   generation, operation ID, timestamp, and compromised disposition.
4. Remove the contact semantically from a validated config candidate and commit
   the config.
5. Activate through Stage C and verify strict health.
6. Retain the tombstone permanently within bounded history even if config
   removal or activation fails; recovery retries removal and never re-enables
   the revoked key to restore availability.

Revocation is local. It cannot claim that the peer deleted its copy; both
operators revoke independently when bilateral removal is intended.

### 5.5 Stage B security invariants

- Current and proposed fingerprints are mandatory and independently checked.
  The same fingerprint cannot occupy both generations.
- Rotation cannot overwrite a legacy or managed contact by nickname; the
  stable contact ID and current ledger fingerprint must both match.
- A replayed rotation transcript is idempotent only for the same locally
  authorized generation and phase. A stale generation, alternate proposed key,
  or reused offer ID is a no-mutation conflict.
- A compromise marker is monotonic and cannot be cleared by rollback, import,
  clock change, or replay.
- Revocation tombstones commit before contact removal and are never rolled back.
- Cross-tenant local pairing exchanges public halves only and uses no shared
  private sidecar.

### 5.6 Stage B test strategy

Add Rust unit/property tests for monotonic generations, predecessor hints,
current/proposed fingerprint enforcement, rotation transcript binding, stale and
future generations, tombstone replay, compromise monotonicity, and every
rotation/revocation transition. Inject crashes before and after both candidates,
journal fsync, ledger reservation/tombstone, config rename, activation result,
rollback decision, and cleanup.

Extend shell coverage as follows:

- `test-darkirc-config-ownership.sh`: rotate or revoke one contact while a
  baseline rewrite and an unrelated contact update contend; prove no lost
  update or stale full-config rollback.
- `test-darkirc-writer-lifecycle.sh`: prove rotation/revocation/pair-local use
  mt-admin lifecycle and deterministic two-tenant lock ordering.
- `test-darkirc-migration-safeguards.sh`: allow only settled managed contacts and
  tombstones in the managed manifest; block rotation, partial pair, rollback,
  compromise-decision, and revocation-removal state.
- `test-darkirc-health.sh`: force first-side/second-side cutover failures and
  prove neither is labeled pair-success before a bidirectional round trip.
- `test-darkirc-adapter-secret-preservation.sh` and tenant-env boundary tests:
  scan all new lifecycle paths for adapter-secret leakage.

Add same-host two-tenant tests for failure after tenant A's ledger, A's config,
tenant B's ledger, and tenant B's config commits. Include foreign scope,
symlinked tenant root, nickname injection, conflicting local contact IDs,
concurrent pair attempts, and recovery replay. Cross-host integration tests must
exercise first-side cutover timeout, safe old-generation rollback, and the
compromised-key path that refuses rollback.

## 6. Next Stage C: Activation and Rollback Lifecycle Completion

### 6.1 Objective and existing seam

Turn `Installed` into an honest lifecycle state rather than treating a config
rename as success. The existing `darkirc_health_strict()` is the required health
primitive, but the current `commit_transaction()` removes its journal as soon as
config and ledger are renamed. Exchange transactions need activation-aware
journal phases and bounded rollback storage.

Do not hold a filesystem lock across a service restart. Instead, persist an
opaque transaction ID, exact expected hashes, and state before mt-admin performs
lifecycle work. A later helper call verifies that same transaction and hashes
before recording success or executing rollback. Other mutations see the durable
in-progress state and fail closed.

### 6.2 Proposed key-manager APIs and mt-admin verbs

Add typed APIs:

```rust
pub fn begin_activation(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &ContactId,
    transaction_id: &TransactionId,
) -> Result<ActivationRequest>;

pub fn mark_locally_activated(
    paths: &TenantPaths,
    scope_id: &str,
    transaction_id: &TransactionId,
) -> Result<ExchangeStatus>;

pub fn rollback_activation(
    paths: &TenantPaths,
    scope_id: &str,
    transaction_id: &TransactionId,
    reason: ActivationFailure,
) -> Result<ExchangeStatus>;

pub fn confirm_roundtrip(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &ContactId,
    exchange_id: &ExchangeId,
    expected_peer_fingerprint: &PublicFingerprint,
) -> Result<ExchangeStatus>;

pub fn resolve_verification_overdue(
    paths: &TenantPaths,
    scope_id: &str,
    contact: &ContactId,
    decision: OverdueDecision,
) -> Result<ExchangeStatus>;
```

Extend `recover()` to understand activation-pending, local-activation,
rollback-pending, rollback-activated, overdue, revocation-removal, and
same-host-coordinator phases. Extend `CrashPoint` with targeted lifecycle points
instead of reusing the three config-only variants.

Add mt-admin verbs:

```text
darkirc-contact apply <tenant> <contact> [--transaction-id <id>] [--json]
darkirc-contact confirm-roundtrip <tenant> <contact> --exchange-id <id> \
  --expect-peer-fingerprint <sha256> [--json]
darkirc-contact resolve-overdue <tenant> <contact> \
  --decision <keep|rollback|revoke> [--extend <duration>] \
  --current-peer-fingerprint <sha256> [--json]
restart-darkirc <tenant>
darkirc-health <tenant> --strict [--json]
```

`restart-darkirc` belongs inside mt-admin. Its systemd branch must use the
tenant's user manager through `_systemctl_user`; its OpenRC branch controls the
matching daemon and adapter services with equivalent ordering. Until targeted
restart/reload parity is demonstrated, `apply` may use the existing
`restart-tenant` path and explicitly report the wider blast radius. A HUP/reload
path must not become the default without attested daemon and both-init tests.

### 6.3 Apply and rollback flow

1. `complete` commits the contact and ledger as `Installed`, creates the one
   permitted rollback snapshot for that contact, and leaves an activation-aware
   journal. The offer remains reserved to the transcript.
2. `apply` asks the helper to validate the transaction ID, current hashes,
   rollback record, deadline, and absence of another mutation.
3. Mt-admin performs targeted restart or the approved full-tenant fallback, then
   calls `darkirc-health <tenant> --strict --json` with a bounded reconnect wait.
4. Only daemon active, adapter active, authenticated `status=ok`, and
   `irc_connected=true` permit `mark_locally_activated()` to advance the ledger
   to `LocallyActivated`.
5. On lifecycle or strict-health failure, mt-admin calls
   `rollback_activation()`. The helper verifies hashes and compromise policy,
   restores the previous contact semantics through the shared journal, and
   records `RolledBack` while keeping the offer reserved to the same transcript.
6. Mt-admin runs the same lifecycle path once for the restored config and
   reports a sanitized result. A failed rollback is an operator-required state;
   it never enters a restart loop.
7. A later explicit bidirectional encrypted DM test is recorded with
   `confirm-roundtrip`. Service health alone cannot infer peer verification.

The rollback snapshot is `key-exchange/rollback/<transaction-id>.toml`, linked
to exactly one contact in ledger metadata. Enforce at most one live rollback
record per contact. The snapshot may contain old active secrets, so it uses the
same no-follow, owner, mode, size, zeroization, and migration-exclusion rules as
other secret-bearing state.

Never blindly replace a newer full config with an old snapshot. If the active
config hash has changed after installation, reconstruct a semantic contact-only
rollback under the shared lock and preserve unrelated contacts/settings; if the
recorded hashes and contact binding cannot prove safety, fail closed for
operator repair.

The activation state transitions are:

| Current state | Event and preconditions | Next state | Durable effect |
| --- | --- | --- | --- |
| `Installed` | `apply`; lifecycle succeeds and strict health passes | `LocallyActivated` | Record local activation time and start the verification window |
| `Installed` | Lifecycle or strict health fails; old key is rollback-eligible | `RolledBack` | Restore previous contact semantics and keep offer/transcript reserved |
| `Installed` | Failure occurs but old key is marked compromised | operator-required/revocation path | Never restore the old generation |
| `RolledBack` | Explicit retry of the same transaction/transcript | `Installed` | Reinstall only the recorded candidate after hash validation |
| `RolledBack` | Explicit cancel | `Cancelled` | Remove retry-private state, retain consumed IDs |
| `LocallyActivated` | Matching exchange/fingerprint and explicit bidirectional DM attestation | `PeerVerified` | Delete pending and rollback private copies |
| `LocallyActivated` | Persisted verification deadline passes | `VerificationOverdue` | Block contact mutation pending an explicit decision |
| `VerificationOverdue` | Explicit keep, eligible rollback, or revoke | resolved local state, `RolledBack`, or `Revoked` | Record the decision without ever inferring peer verification |
| `LocallyActivated` or `PeerVerified` | Fingerprint-bound revoke or verified rotation | `Revoked` or rotation sub-state | Follow the Stage B ordering |

A resolved `keep` remains explicitly unverified; a `Revoked` contact can return
only through a new, independently fingerprint-verified exchange.

### 6.4 Verification window and mutation block

- The default decision window is 24 hours from local activation.
- A configured or operator-granted extension is explicit, audited, and capped at
  seven days. Clock rollback cannot lengthen a previously persisted deadline.
- `confirm-roundtrip` checks exchange ID and full current peer fingerprint,
  advances to `PeerVerified`, deletes the pending private record and rollback
  snapshot promptly, and retains sanitized replay/history metadata.
- When the deadline passes first, any read or write that observes it durably
  marks `VerificationOverdue`. Every further mutation of that contact fails with
  an operator-action-required category.
- `resolve-overdue keep` deletes the rollback snapshot and records that the
  operator retained an unverified local installation; it must not label the
  contact `PeerVerified`. `rollback` is allowed only for a generation not marked
  compromised. `revoke` follows the irreversible tombstone-first flow.

### 6.5 Stage C security invariants

- `LocallyActivated` is impossible without the strict gate result; `PeerVerified`
  is impossible without explicit round-trip attestation and fingerprint match.
- Adapter credentials are read under the tenant identity and delivered to curl
  without argv/environment/log/stdout exposure. Structured health output
  contains no bearer or raw response body.
- One rollback record exists per contact, never enters migration, and cannot
  outlive the resolved decision.
- Activation failure preserves offer/transcript reservation. Retrying a
  different transcript is a conflict.
- A compromised old generation cannot be restored by apply failure, timeout,
  overdue resolution, `recover`, import, or same-host compensation.
- All service actions remain inside mt-admin and support systemd user and OpenRC
  with equivalent result categories and bounded waits.

### 6.6 Stage C test strategy

Extend Rust tests with activation-aware crash recovery, exact-hash transaction
resumption, one-snapshot enforcement, semantic rollback after an unrelated
contact update, 24-hour deadline, monotonic deadline under clock rollback,
seven-day extension cap, overdue mutation block, explicit keep/rollback/revoke,
peer-verification cleanup, retry idempotence, and compromised-key rollback
refusal.

Extend shell harnesses:

- `test-darkirc-health.sh`: cover daemon inactive, adapter inactive, malformed
  JSON, `status != ok`, `irc_connected=false`, auth failure, retry timeout,
  systemd user and OpenRC parity, sanitized JSON, and no secret in argv/env/logs.
- `test-darkirc-writer-lifecycle.sh`: cover `restart-darkirc`, approved full-stack
  fallback, apply/recover dispatch, bounded retries, and greps that forbid bare
  init calls in helper/onboard/web/import wrappers.
- `test-darkirc-config-ownership.sh`: exercise activation failure and semantic
  rollback without losing unrelated fields, contacts, or concurrent baseline
  updates.
- `test-darkirc-migration-safeguards.sh`: block export/import/upgrade for
  rollback, activation, overdue, and unresolved journal state; allow only the
  settled post-verification/tombstone forms supported by the managed manifest.
- `test-darkirc-adapter-secret-preservation.sh`,
  `test-darkirc-tenant-env-boundary.sh`, and
  `test-darkirc-openrc-env-boundary.sh`: prove the health/restart additions do
  not introduce root-side secret sourcing, shell evaluation, symlink following,
  or credential regeneration.

Run integration scenarios on both init systems: successful apply; daemon start
failure; adapter start failure; adapter connected timeout; config rollback and
single reactivation; failed rollback requiring operator action; 24-hour overdue
resolution; and bidirectional encrypted DM confirmation. Cross-tenant tests must
prove one tenant cannot read another tenant's pending, active, or rollback key.

## 7. Next Stage D: Optional Delivery Surfaces - Deferred

Do not start onboarding, web, QR, trusted-channel delivery, signed offers, or an
online rendezvous protocol as part of Stages A-C.

Stage D adds no new key-manager API or mt-admin verb before review. An approved
delivery surface consumes the already-reviewed Stages A-C commands and public
status types; any proposed new broker action returns to security review first.

A separate security review is mandatory before any Stage D implementation
begins. The review must produce an updated threat model and approve:

- authenticated principal-to-tenant authorization and server-derived
  `darkirc_scope_id` selection;
- a narrow structured broker that invokes allowlisted mt-admin actions without
  constructing root shell commands;
- CSRF/origin, body-size, rate-limit, replay, audit, and confused-deputy controls;
- a public-only response schema proving private generator output, pending
  records, active/rollback secrets, and migration archives never reach a browser;
- QR/clipboard/download retention rules for public artifacts and relationship
  metadata;
- the identity contract of any already-trusted delivery channel;
- separate cryptographic review before protocol-driven confirmation, signing
  identities, PAKE/SAS, or a relay is considered.

After approval, onboarding and web code must call exactly the same mt-admin
verbs and state machine. The surface may improve public-offer delivery and
status presentation only; it may not create a second secret store, accept an OS
username/scope/path from a request body, auto-trust sender pairing, or bypass the
full expected-fingerprint policy.

Stage D tests, if approved, must include cross-tenant authorization, request
smuggling and injection, CSRF/origin, body and rate limits, replay, browser
response leak scans, and parity with direct CLI state transitions. Delivery over
ordinary DarkIRC plaintext or unencrypted DMs remains rejected, not deferred.

## 8. Ordered Roadmap and Dependencies

The implementation order should follow the dependency graph rather than expose
all proposed verbs at once:

### Step 0: Freeze contracts and compatibility

- Freeze Public Exchange V1 canonical JSON, transcript framing, fingerprint
  display/parse rules, timestamp/expiry bounds, error categories, and fixed test
  vectors.
- Choose the managed ledger and migration version. Preserve landed v1
  journal/ledger/archive recovery and validation.
- Define strong ID, fingerprint, role, operation, state, generation, and deadline
  types.
- Confirm the attested generator's exact output contract and failure behavior.

**Dependency:** Required before any managed generator invocation or pending write.

### Step 1: Build the read-only exchange parser and normalized state model

- Add typed parsing, canonicalization, fingerprint/transcript computation, and
  read-only validation/status APIs.
- Add property/fuzz-style tests and fixed vectors before enabling writer calls.
- Add version-aware ledger reads and explicit v1 upgrade planning.

**Exit:** Hostile public artifacts can be validated/rejected with zero mutation,
and every state transition is table-tested.

### Step 2: Enable staged exchange through `Installed`

- Add attested generator capture, pending storage, prepare/respond/complete,
  cancellation/expiry, replay ledger, and exchange-aware journal commits.
- Require expected fingerprints and mandatory `--defer-apply`.
- Extend migration and upgrade fail-closed checks for new unresolved state.

**Dependency:** Steps 0-1 complete. **Exit:** Two tenants can produce role-bound
public artifacts and install matching contacts without exposing private keys,
but no command claims local or peer activation.

### Step 3: Complete activation and rollback

- Add activation-aware journals, one-per-contact rollback storage, `apply`,
  `restart-darkirc` or approved full-stack fallback, strict health integration,
  round-trip confirmation, deadlines, overdue block/resolution, and recovery.
- Pass systemd user and OpenRC parity tests before removing mandatory deferred
  apply.

**Dependency:** Step 2 installed-state contract. **Exit:** `LocallyActivated`,
`PeerVerified`, `RolledBack`, and `VerificationOverdue` are evidence-backed and
recoverable.

### Step 4: Enable rotation, revocation, and same-host pairing

- Add current/proposed fingerprint-bound rotation, monotonic generations,
  maintenance windows, tombstone-first revocation, compromised-key disposition,
  and pair-local coordinator recovery.
- Exercise two-host cutover and every same-host partial commit.

**Dependency:** Steps 2-3. **Exit:** Rotation/revocation obey irreversible
security ordering and never report pair success from one local activation.

### Step 5: Close migration, operations, and documentation parity

- Export/import settled managed contacts and tombstones using the approved
  version; continue to exclude all transient/rollback state.
- Verify patch-env, upgrade, tenant rename, same-tenant move, and clone rejection.
- Update the authoritative proposal status, operations docs, API help,
  `FEATURE_PARITY.md`, and changelog only when the implementation actually lands.

**Dependency:** Stable terminal states from Steps 3-4.

### Step 6: Security-review gate for optional delivery

- Conduct the separate Stage D threat-model and cryptographic review.
- Start no onboarding/web/trusted-channel code until the review explicitly
  approves the chosen surface and trust mapping.

**Dependency:** Stages A-C complete and operationally stable. This is the only
stage in this plan that categorically requires a separate review before
implementation starts. Rotation/revocation and canonicalization should also
receive focused security review before production enablement because mistakes
are irreversible, but that review does not authorize Stage D.

## 9. Verification Matrix

Implementation verification should remain targeted and resource-bounded:

```text
Rust unit/property tests
  taskset -c 0-5 cargo test -j6 darkirc_key_manager -- --test-threads=6
  taskset -c 0-5 cargo test -j6 --bin lunarwing-darkirc-key-helper -- --test-threads=6

Shell regression harnesses
  bash ic/scripts/tests/test-darkirc-config-ownership.sh
  bash ic/scripts/tests/test-darkirc-writer-lifecycle.sh
  bash ic/scripts/tests/test-darkirc-migration-safeguards.sh
  bash ic/scripts/tests/test-darkirc-adapter-secret-preservation.sh
  bash ic/scripts/tests/test-darkirc-health.sh
  bash ic/scripts/tests/test-darkirc-tenant-env-boundary.sh
  bash ic/scripts/tests/test-darkirc-openrc-env-boundary.sh
```

The implementation plan should add `proptest` or an equivalent property-test
dependency only if needed for canonicalization/state-machine generation; the
current `ic/Cargo.toml` dev-dependencies do not include one. No full debug build
is needed. Compile verification should use the repository's constrained
`taskset -c 0-5 cargo check -j6` commands only after code exists.

Every stage must include explicit before/after mutation assertions for:

- cross-tenant scope and path attempts;
- intermediate and final symlinks, hard links, owner/mode changes, and
  non-regular files;
- nickname case collision, TOML quoting/injection, separators, control
  characters, and oversized input;
- identical replay, conflicting replay, stale generation, and concurrent update;
- private-value absence from argv, environment, stdout, stderr, logs, traces,
  public artifacts, archives, and browser responses;
- systemd user and OpenRC behavior through mt-admin, including the required grep
  for forbidden bare init calls outside the owning lifecycle layer.

## 10. Open Questions and Risks

1. **Canonical JSON and transcript framing:** The exact byte grammar and framing
   must be frozen with independent vectors. A serializer upgrade must not alter
   transcript hashes silently.
2. **Ledger and migration versioning:** The landed v1 ledger is publicly exposed
   by `read_ledger()` and embedded in `darkirc-contacts-v1`. Managed exchange
   metadata and tombstones need an explicit compatibility strategy; changing v1
   meaning in place is high risk.
3. **Generator output:** The compatibility attestation is landed, but managed
   generator parsing is not. Confirm whether the pinned daemon provides a
   machine-readable output or requires a narrowly versioned parser, and whether
   it can validate a candidate config without starting.
4. **Offer lifetime and clock skew:** Select a bounded pending-offer lifetime and
   permitted skew before Step 0 exits. Persist deadlines so wall-clock rollback
   cannot revive expired state or extend rollback retention.
5. **Activation default:** This plan keeps `--defer-apply` mandatory until Stage
   C passes both init matrices. Decide afterward whether completion should apply
   automatically or keep explicit `apply` as the safer default.
6. **Targeted restart versus full tenant restart:** The existing portable path is
   `restart-tenant`. A targeted restart reduces blast radius but needs proven
   daemon/adapter ordering and health parity on systemd user and OpenRC. HUP is
   not acceptable without compatibility evidence.
7. **Rollback granularity:** A full old-config snapshot is simple but can contain
   every contact secret and can clobber unrelated later updates. Recovery must
   remain hash-guarded and semantic; consider a contact-specific protected
   rollback payload if it can restore daemon-valid TOML without creating a
   second long-lived store.
8. **Same-host coordinator custody:** A coordinator is needed for deterministic
   partial-commit recovery, but it must contain public metadata and hashes only.
   Root remains a fleet-wide trust point; no design can make root unable to read
   tenant state.
9. **Compromise declaration:** Define the exact operator syntax and durable audit
   field that makes old-generation rollback impossible. False compromise
   declarations reduce availability but must remain safer than silently
   restoring a possibly exposed key.
10. **Managed migration custody:** Decide whether the managed secret-bearing
    contact manifest is embedded in the tenant archive or separately approved,
    and define encryption, retention, old-host shutdown, and deletion policy.
11. **History bounds and relationship metadata:** Consumed IDs and tombstones are
    needed for replay resistance, but indefinite nickname/fingerprint history
    leaks relationship metadata. Define bounded sanitized history while keeping
    security-critical tombstones durable.
12. **Round-trip evidence:** `confirm-roundtrip` is an operator attestation, not
    an automatically proven cryptographic event. The CLI and any future UI must
    not overstate that distinction.

## 11. Completion Criteria

Stages A-C are complete only when two distinct tenants can perform initial
exchange, strict local activation, explicit bidirectional round-trip
confirmation, verified rotation, and independent revocation while:

- each contact and generation uses a distinct local keypair;
- mandatory full fingerprints and transcript bindings are enforced before
  mutation;
- identical retries are idempotent and conflicts are nonzero/no-mutation;
- all crash points recover deterministically;
- rollback count, 24-hour window, seven-day cap, overdue block, and compromised
  no-rollback policy are enforced;
- managed migration carries only settled contacts/tombstones and target-specific
  config remains fresh;
- cross-tenant path/symlink/nickname/replay/concurrency tests pass;
- private keys and adapter credentials stay out of every public and observable
  surface; and
- all activation and rollback lifecycle actions pass through mt-admin on both
  systemd user units and OpenRC.

Stage D remains deferred until its separate security review is complete.
