# Secure Automated DarkIRC Key Exchange

**Status:** Proposal only. None of the commands, state files, APIs, or web
surfaces described here exist yet.

**Scope:** LunarWing multi-tenant DarkIRC deployments managed through
`ic/scripts/lunarwing-mt-admin.sh`.

## Decision Summary

The primary design should be a tenant-scoped, staged out-of-band exchange
managed by `lunarwing-mt-admin.sh` and a small typed key-management helper.
Each side generates a unique keypair locally, exports only a versioned public
offer, verifies the peer public-key fingerprint through an independently
authenticated channel, and then installs the contact through a journaled,
recoverable transaction. A secret must never be printed, placed in an argument or
environment variable, written to the LunarWing workspace, or sent to the peer.

This design is intentionally not a zero-click trust system. Automation should
remove secret-handling and TOML-editing mistakes, not decide that an IRC
nickname is a cryptographic identity.

There is one prerequisite: LunarWing must first stop replacing the complete
DarkIRC TOML during `patch-env`. Current rendering can erase manually managed
`[contact]` sections. Key automation must not ship until all writers share a
locked, semantic, journaled config-update path that preserves contacts.

The recommended migration sequence is:

1. Make DarkIRC config regeneration contact-preserving and crash-recoverable.
2. Add read-only inspection and adoption of existing manual contacts.
3. Add staged public-offer exchange with mandatory fingerprint confirmation.
4. Add rotation, revocation, same-host pairing, and a targeted DarkIRC restart.
5. Consider web delivery or a mutual protocol over an already trusted channel
   only after a separate security review.

## Goals

- Generate a different DarkIRC DM keypair for every tenant, contact, and key
  generation without exposing the private half outside that tenant's protected
  state.
- Export and transport only public material in a format suitable for a file,
  paste, or QR code.
- Bind an operator-confirmed peer fingerprint to a local contact name. Treat
  the name as display metadata, not proof of identity.
- Update `darkirc_config.toml` with a structured TOML editor, validation,
  locking, a journaled commit, and rollback on activation failure.
- Preserve existing manual contacts through `patch-env`, upgrade, export,
  import, and adoption into the managed flow.
- Keep tenant selection, ownership, and service lifecycle under mt-admin so
  systemd user units and OpenRC remain first-class.
- Support unattended use only when the caller supplies a peer fingerprint
  obtained from an independent trusted source.
- Produce useful status and audit events without logging raw keys or public
  exchange bundles.

## Non-Goals

- Designing or replacing DarkIRC's DM encryption protocol.
- Treating LunarWing sender pairing, an IRC nickname, or receipt of a DarkIRC
  message as cryptographic identity proof.
- Sending a private key to the peer, a central broker, a gateway, a browser, or
  another tenant.
- Providing private-key escrow, recovery, or fleet-wide key reuse.
- Making the current DarkIRC protocol provide forward secrecy, a ratchet,
  post-quantum security, or overlapping key generations.
- Automatically discovering or trusting contacts from IRC, DarkIRC P2P, a
  public directory, DNS, or a chat transcript.
- Provisioning Tor, onion services, seed nodes, or general P2P reachability.
- Building a web UI or a new online rendezvous service in the first release.
- Protecting keys after the tenant account, host root, kernel, or DarkIRC
  process has been compromised.

## Current State

The current operational model is documented in
[DarkIRC Multitenant Operations](../ops/DARKIRC-MULTITENANT.md). Each tenant has
its own DarkIRC daemon, adapter, ports, config, datastore, logs, and adapter
secret. The active daemon config is:

```text
/home/<TENANT>/lunarwing/state/darkirc/darkirc_config.toml
```

The current repo-owned template and the DarkFi source revision used as
mt-admin's default build input describe an active contact with two assignments:

```toml
[contact."nickname"]
dm_chacha_public = "<peer public key>"
my_dm_chacha_secret = "<this tenant's private key for the contact>"
```

`darkirc --gen-chacha-keypair` generates the tenant's local public and private
halves. The local public half is exchanged with the peer; it is not a third
required assignment in the current checked-in template. The peer public half
and local private half are placed inline in the contact table. A separate YAML
file is not daemon input.

This is a source and template observation, not an attestation of every
installed binary. Mt-admin can build from an existing DarkFi checkout and
installs one shared `darkirc` binary. That checkout or binary can differ from
the repo's default source revision. Before automation parses generator output
or edits a live config, it must identify the actual binary by an approved
compatibility profile and binary digest (and source revision when recorded).

The repository documentation is inconsistent on these details:

- [DarkIRC Multitenant Adapter](../guides/darkirc_channel_for_lunarwing/DARKIRC_MT_ADAPTER.md)
  correctly says key material is inline and exchange is manual, but it also
  describes `my_dm_chacha_public` as a third field and claims a placeholder
  `darkirc_keypair.yaml` is generated.
- [The June 23 session audit](SESSION-AUDIT-MT-DARKIRC-EWE-2026-06-23.md)
  records the YAML claim as stale; the current generator does not create that
  file.
- The current template at
  `ic/scripts/templates/darkirc_config.toml.template` defines only
  `dm_chacha_public` and `my_dm_chacha_secret` in a contact table.
- The operations runbook presents `LUNARWING_MT_DARKIRC_REV=master` as the
  default, while current mt-admin source supplies a specific default revision.
  An existing checkout can still be used as-is, so neither document nor script
  default proves which binary is installed.
- [The issue status report](../ops/ISSUE_STATUS_REPORT.md) confirms that no key
  exchange automation currently exists.

The later implementation must test its key-generator parser and TOML schema
against the actual attested DarkIRC binary. It must fail closed on an unknown
binary digest, CLI output, key format, or config schema instead of copying
stale examples.

### Existing Config-Ownership Hazard

`patch-env` currently calls `generate_darkirc_config()` for a DarkIRC-enabled
tenant. `generate_darkirc_config()` renders the baseline template, and the
shared `render_template()` function truncates the destination directly. As a
result, a normal patch or upgrade can remove manually added contacts.

This is not only a migration concern. A future contact helper and the current
renderer would also be competing writers, creating lost-update and partial-file
risks. The recommended design therefore makes contact-preserving config
ownership a release prerequisite rather than a later cleanup.

## Threat Model

### Assets

| Asset | Required property |
| --- | --- |
| `my_dm_chacha_secret` | Confidential to the owning tenant and host root |
| Peer public-key binding | The confirmed key remains bound to the intended human/contact |
| Active TOML | Valid, complete, atomic, and not silently reset by maintenance |
| Pending exchanges | Secret-bearing state is short-lived, tenant-isolated, and replay-safe |
| Exchange ledger | Consumed offers, generations, and revocations cannot be rolled back casually |
| Service availability | A bad offer or failed update cannot leave the tenant stack permanently broken |
| Relationship metadata | Nicknames, fingerprints, and exchange history are not emitted unnecessarily |

### Adversaries and Failures We Address

- A remote attacker or malicious peer substitutes a public key, replays an old
  offer, lies about a nickname, or supplies malformed structured input.
- Another unprivileged tenant or local user attempts to read pending or active
  private keys or redirect writes across tenant paths.
- Secrets leak through shell history, `/proc` arguments, inherited environment,
  command output, logs, traces, crash messages, temporary files, backups, or
  chat plaintext.
- An operator selects the wrong tenant or contact, accepts a key without
  checking it, or attempts to overwrite an existing contact accidentally.
- Concurrent `patch-env`, upgrade, contact, rotation, or removal operations race
  and corrupt or replace the config.
- The process crashes between key generation, staging, TOML replacement,
  service activation, and cleanup.
- Activation fails because the TOML is invalid or the DarkIRC service cannot
  restart.

### Threats We Do Not Solve

- Compromise of root, the tenant OS account, the kernel, the DarkIRC binary, or
  the random-number generator.
- A compromised authenticated out-of-band channel, combined with an operator
  who does not compare the independently obtained fingerprint.
- A malicious contact who owns the confirmed key.
- Traffic analysis, endpoint anonymity, peer discovery, Tor availability, or
  DarkIRC network denial of service.
- Recovery of deleted secrets from filesystem snapshots, storage media, or an
  already-created backup. Deletion limits future exposure but is not guaranteed
  secure erasure.
- Retrospective deletion of keys held by a peer or of messages and ciphertext
  already copied elsewhere.

### Required Invariants

1. A private key never crosses the public-exchange boundary.
2. A private key may exist transiently only inside the key generator, a private
   OS pipe, and zeroized helper memory. It never appears in user-visible argv,
   environment variables, stdout, stderr, logs, audit fields, or a browser
   response.
3. Keypairs are never reused across tenants, contacts, or generations.
4. A nickname is never sufficient to accept or replace a public key.
5. Interactive completion requires an explicit fingerprint check. Unattended
   completion requires an expected peer fingerprint; `--yes` alone is invalid.
6. A validated tenant identifier resolves to one fixed config root. The helper
   does not accept arbitrary config paths and rejects symlinks.
7. Every config writer takes the same per-tenant lock and uses semantic TOML
   parsing, a same-directory `0600` temporary file, `fsync`, and atomic rename.
8. Existing contacts survive baseline regeneration unless an explicit contact
   remove or rotation transaction changes them.
9. Config, pending state, transaction journal, and ledger updates have a
   defined crash-recovery order; individual atomic renames are not treated as
   a multi-file transaction.
10. Replayed, expired, revoked, or conflicting offers fail closed.
11. Service status and lifecycle actions go through mt-admin, never bare
    `systemctl` or `rc-service` in an onboard, web, or helper wrapper.

## Options

### Option 1: Hardened Manual Out-of-Band Exchange

Keep the existing operator process: run `darkirc --gen-chacha-keypair`, share
the public half through an independently authenticated channel, edit the TOML
as the tenant, and restart through mt-admin. Improve the runbook with strict
`umask`, file modes, fingerprint verification, and warnings against shell
history and chat disclosure.

This has the smallest implementation cost and remains a useful break-glass
fallback. It does not remove the riskiest steps: the key generator can expose
its private output to a terminal or transcript, the operator handles and edits
the private value, TOML edits are not transactional, and config regeneration
can erase contacts. Documentation alone cannot enforce the security
invariants.

### Option 2: Managed Staged Public Offers

Add a tenant-scoped helper behind mt-admin. `offer` invokes an attested,
supported DarkIRC key generator under the tenant identity, captures its output
through private pipes, validates the pair, stores the private half in protected
pending state, and emits only a versioned public offer and fingerprint. The
responder returns a public response bound to the first offer. `complete`
validates the paired artifacts, requires fingerprint confirmation, and installs
the active contact through a recoverable transaction.

This is the recommended primary path. It removes routine private-key handling,
does not require a new network service or long-term identity PKI, works between
hosts, and leaves the trust decision visible to the operator. Its main costs
are a small amount of secret-bearing pending state, a structured state machine,
and the need to make config rendering safe first.

### Option 3: Signed Asynchronous Offers

Create a separate long-term tenant signing identity and use it to sign
canonical offers, responses, rotations, and revocations. Once the signing
identity is pinned, the public artifacts can travel over an untrusted mailbox
or store while retaining origin and transcript integrity.

This is genuinely more automated than Option 2, but it does not remove the
first-contact problem: each signing identity still needs authenticated
out-of-band pinning. It also introduces a second private-key lifecycle,
recovery and rotation authority, signature canonicalization, and a larger
backup blast radius. Signing-key compromise could authorize DarkIRC key
replacement until revocation reaches the peer. This option becomes attractive
only if LunarWing establishes a general tenant identity key with a reviewed
lifecycle; DarkIRC should not invent one in isolation.

### Option 4: Mutual Exchange Over an Already Trusted Channel

Reuse the same public-offer format and state machine, but deliver offers and
confirmations over a channel that already authenticates both operators or
tenant identities. Examples could include an authenticated administrative
session or an end-to-end encrypted channel whose identities were verified
before this exchange. Only public offers travel over the transport.

This can reduce copying and QR scanning, but the trusted channel becomes part
of the authorization boundary. It also creates endpoint, replay, confused-
deputy, rate-limit, and audit requirements. It must not treat ordinary DarkIRC
plaintext, LunarWing sender pairing, or a bearer token alone as a durable peer
identity. A future protocol should use an established, reviewed authenticated
handshake rather than inventing signatures or repurposing ChaCha keys. This is
a migration target, not the first implementation.

### Comparison

| Option | Trust anchor | Automation and cost | Principal residual risk | Fit |
| --- | --- | --- | --- | --- |
| 1. Manual | Human OOB comparison | Low automation, no new component | Secret handling and edit mistakes | Break-glass fallback |
| 2. Managed offers | Enforced human/expected fingerprint | High local automation, no online service | Human verification and protected pending state | Recommended now |
| 3. Signed offers | Pinned long-term signing identity | High async automation; new identity-key lifecycle | Signing-key compromise authorizes replacement | Future general identity layer |
| 4. Trusted channel | Existing authenticated channel identity | High delivery automation; new endpoint integration | Channel compromise or identity-mapping error | Future migration target |

All managed options use the same config transaction and tenant isolation, so
none changes the DarkIRC message-path performance or memory profile. Options 3
and 4 add operational recovery, availability, and incident-response work that
Option 2 avoids. Option 1 remains a documented fallback. Option 3 becomes
preferable if LunarWing gains a general, recoverable tenant signing identity;
Option 4 becomes preferable if an existing channel has a separately reviewed
identity-to-tenant mapping. Under current constraints, Option 2 gives the
strongest improvement without creating a new trust service.

## Recommended Design

### Component Boundaries

```text
operator
  |
  | tenant + contact selection, fingerprint confirmation
  v
lunarwing-mt-admin.sh
  |-- validates tenant registry membership and DarkIRC enablement
  |-- owns systemd-user/OpenRC lifecycle calls
  v
tenant-scoped DarkIRC key helper
  |-- invokes darkirc key generation with captured output
  |-- owns pending exchange state and replay ledger
  |-- owns locked, semantic, atomic TOML contact updates
  v
darkirc_config.toml -> tenant DarkIRC daemon

Only public exchange artifacts cross to the peer.
```

Mt-admin is the operator entry point because it already owns tenant lookup and
init-specific behavior. Secret parsing and TOML transactions should live in a
small typed helper, not shell string substitution. The wrapper must pass tenant
identity, contact label, offer file descriptors, and control flags only. It
must not pass a private value.

The helper runs key generation and tenant-file operations under the tenant UID.
Root remains able to read all tenant files, as it does today, but a helper bug
must not turn an arbitrary nickname or file argument into a cross-tenant path.

### Public Exchange V1

The exchange artifact is structured public data, not a daemon config fragment.
JSON is appropriate for file, paste, and QR transport. The exact canonical
encoding must be frozen and tested before implementation.

| Field | Purpose |
| --- | --- |
| `schema` | Fixed value such as `lunarwing.darkirc-exchange/v1` |
| `artifact_role` | `initiator_offer` or `responder_response` |
| `offer_id` | At least 128 bits of random uniqueness and replay identity |
| `in_reply_to` | Empty for the initiator; initiator offer ID for the response |
| `sender_contact_id` | Random stable local ID for this relationship, not a global identity |
| `intended_peer_contact_id` | Initiator contact ID copied into a response |
| `intended_peer_fingerprint` | Initiator public fingerprint copied into a response |
| `generator_profile` | Approved compatibility profile and binary digest |
| `key_format` | DarkIRC key format tied to that supported profile |
| `public_key` | The sender's generated public half |
| `public_fingerprint` | SHA-256 of decoded canonical public-key bytes |
| `generation` | Monotonic local generation for initial setup or rotation |
| `created_at` / `expires_at` | Bounded offer lifetime with documented clock skew |
| `sender_label` | Optional display hint; explicitly untrusted |
| `previous_fingerprint` | Rotation linkage when present; not proof by itself |

The importer recomputes the fingerprint and rejects a mismatch. It treats every
other field as attacker-controlled until validation and operator confirmation.
In particular, sender-controlled `generation`, `previous_fingerprint`, labels,
and contact IDs are consistency hints, not authority to rotate or replace a
key. The artifact contains no tenant OS path, private key, bearer token, or
service credential. Avoiding the OS username by default also reduces
unnecessary deployment metadata in QR codes and shared files.

The responder response must name the initiator offer ID, contact ID, and public
fingerprint. The helper computes the transcript as SHA-256 over the frozen
domain separator `lunarwing.darkirc.exchange.v1`, followed by the
canonical initiator artifact and canonical responder artifact in that fixed
role order. This avoids sorting ambiguities and prevents a response from being
silently paired with another offer.

On completion, the ledger binds the stable tenant scope ID, local contact ID,
peer contact ID, both public fingerprints, both offer IDs, transcript hash,
operation, accepted generation, and generator profiles. Operators compare the
peer fingerprint through the authenticated out-of-band channel. Transcript
validation is automatic: each helper recomputes it from the role-bound
artifacts and refuses a mismatch. It may display the transcript hash for
diagnostics, but human transcript entry is not a second trust requirement.

### Stable Tenant Scope ID

The OS username is mutable and is therefore not the key-management identity.
Mt-admin should generate a random 128-bit `darkirc_scope_id` when DarkIRC is
first enabled or an existing tenant is adopted. The root-owned tenant entry in
`/etc/lunarwing/ports.json` is the source of truth; the ledger copies the value
and rejects a mismatch. Scope IDs must be unique in the registry and are not
secrets.

A tenant rename moves the registry entry without changing its scope ID. A
same-tenant machine migration carries the ID in the structured migration
manifest and may preserve it only after the destination confirms that the ID
is not already active. A clone, import under a different logical tenant, or
restore while the source remains active receives a new scope ID and cannot
activate copied contacts until verified rotation. Phase 1 adoption creates the
ID before writing any ledger state.

### State and File Layout

The authoritative active private key remains where DarkIRC requires it: inline
in the active TOML after completion. Asynchronous exchange and recoverable
updates require the bounded protected copies enumerated below.

Proposed paths:

```text
/home/<TENANT>/lunarwing/state/darkirc/
  darkirc_config.toml                    mode 0600
  key-exchange/                          mode 0700
    pending/<offer-id>.json              mode 0600, contains local private key
    ledger.json                          mode 0600, contains no private keys
    transactions/<transaction-id>.json   mode 0600, contains hashes, no keys
    candidates/                           mode 0700, may contain protected keys
    rollback/<transaction-id>.toml        mode 0600, one bounded old config
    update.lock                          mode 0600
```

The exchange subtree remains `0700` even if another service path uses a less
restrictive parent mode. All files are tenant-owned and created with `umask
077`. The helper rejects symlinks and non-regular files, opens relative to a
validated directory handle, and never stages under `/tmp`, a repo checkout,
the LunarWing workspace, or an environment directory.

The pending record contains the local pair, offer metadata, requested contact,
generation, and state. It is deleted after successful local activation and
according to the round-trip verification policy, or after explicit
cancellation/expiry cleanup. The ledger retains only fingerprints,
generations, consumed offer IDs, revocation tombstones, timestamps, and
sanitized outcomes.

Persistent private-key copies are allowed only in the pending record, active
TOML, same-directory config candidate, one bounded rollback snapshot, and an
explicitly protected tenant migration archive. Generator and helper memory and
the private pipe are permitted transient locations and must be zeroized or
closed promptly. The transaction journal and ledger never contain raw keys.

There is at most one rollback snapshot per contact. Its default decision
window is 24 hours and the configured extension may not exceed seven days.
Peer verification deletes it promptly. If the window expires first, the helper
marks the contact `VerificationOverdue`, blocks another mutation for that
contact, and requires an operator to keep the locally installed config and
delete the snapshot, roll back an uncompromised generation, or revoke. It does
not choose automatically. Pending transactions, candidates, journals, and
rollback snapshots are excluded from migration exports; their presence makes
export fail closed until resolved.

### Exchange State Machine

| State | Meaning | Allowed next state |
| --- | --- | --- |
| `Prepared` | Local pair generated; initiator offer or responder response available | `PeerReceived`, `Expired`, `Cancelled` |
| `PeerReceived` | The role-bound peer artifact is structurally valid | `Confirmed`, `Expired`, `Cancelled` |
| `Confirmed` | Operator or expected fingerprint accepted | `Installed`, `Cancelled` |
| `Installed` | Config and ledger transaction committed; service not yet checked | `LocallyActivated`, `RolledBack` |
| `LocallyActivated` | Strict DarkIRC service and adapter checks succeeded locally | `PeerVerified`, `VerificationOverdue`, `Revoked`, rotation flow |
| `PeerVerified` | A bidirectional encrypted DM round trip confirmed matching peer state | `Revoked`, rotation flow |
| `VerificationOverdue` | Peer round trip was not confirmed within the rollback window | Explicit keep, rollback, or revoke decision |
| `RolledBack` | Installation was reversed; the same transcript may be retried explicitly | `Installed`, `Cancelled` |
| `Expired` / `Cancelled` | Pending secret removed; offer ID retained | Terminal |
| `Revoked` | Contact removed and replay tombstone retained | New explicitly verified exchange only |

A repeated operation with the same offer, fingerprint, and state is
idempotent. A different key for an active nickname is a conflict, not an
implicit update. The caller must use the rotation flow.

### CLI Flow

The following syntax is a proposed contract, not an existing implementation:

```text
lunarwing-mt-admin.sh darkirc-contact prepare <tenant> <contact> --out <offer.json> [--expires <duration>] [--json]
lunarwing-mt-admin.sh darkirc-contact respond <tenant> <contact> --in <offer.json> --out <response.json> --expect-peer-fingerprint <sha256> [--json]
lunarwing-mt-admin.sh darkirc-contact complete <tenant> <contact> (--in <response.json> | --exchange-id <id>) --expect-peer-fingerprint <sha256> [--defer-apply] [--json]
lunarwing-mt-admin.sh darkirc-contact apply <tenant> <contact> [--json]
lunarwing-mt-admin.sh darkirc-contact confirm-roundtrip <tenant> <contact> --exchange-id <id> --expect-peer-fingerprint <sha256> [--json]
lunarwing-mt-admin.sh darkirc-contact list <tenant> [--json]
lunarwing-mt-admin.sh darkirc-contact status <tenant> <contact> [--json]
lunarwing-mt-admin.sh darkirc-contact cancel <tenant> --exchange-id <id> [--json]
lunarwing-mt-admin.sh darkirc-contact doctor (<tenant> | --all) [--json]
lunarwing-mt-admin.sh darkirc-health <tenant> --strict [--json]
```

`prepare`, `respond`, `complete`, `list`, `status`, `cancel`, `doctor`, `apply`,
`confirm-roundtrip`, and strict health verification are the first-release
surface. Proposed later names are `darkirc-contact rotate`,
`darkirc-contact revoke`, `darkirc-contact pair-local`,
`darkirc-contact resolve-overdue`, and `restart-darkirc`.

Human output contains contact, offer/transaction ID, state, fingerprints,
expiry, and artifact path only. `--json` writes the same public/status fields as
one structured stdout document; sanitized diagnostics go to stderr. Artifact
files are created exclusively with mode `0600` and never overwritten. Repeating `respond` with
the same initiator artifact returns the existing byte-identical response.
Repeating `complete` or `apply` for the same committed transcript returns the
current state successfully. A different key/transcript for an occupied contact
returns a nonzero `conflict` result without changing files; expired, revoked,
unknown-profile, and operator-action-required outcomes are also distinct
nonzero result categories.

An operator flow is:

1. The initiator runs `prepare`. The command writes protected pending state and
   returns an initiator offer, public fingerprint, and expiry. It never
   displays the private key.
2. Send that offer over an authenticated out-of-band channel or show its QR.
   The peer runs `respond`, which binds its public response to the initiator
   offer, requires the independently obtained initiator fingerprint, and stores
   its own protected pending secret.
3. Each side runs `complete` using the paired artifacts by file or stdin. Raw
   key material is not accepted as an argv value.
4. Confirm the independently obtained peer fingerprint. Interactive mode
   requires entering or scanning the full fingerprint; a generic yes/no prompt
   and `--yes` are not accepted. Unattended mode requires
   `--expect-peer-fingerprint` from a separate trust source. Transcript binding
   is recomputed and checked internally from the two artifacts.
5. The helper installs the contact transactionally. `apply` activates it
   through mt-admin and reports `LocallyActivated` after service health. Only a
   successful bidirectional encrypted DM check can mark it `PeerVerified`.
6. After both operators observe a bidirectional encrypted DM round trip, each
   records that fact with `confirm-roundtrip`. This is an explicit operator
   attestation, not a claim inferred from service health. It advances
   `PeerVerified` and triggers the applicable rollback cleanup.

`pair-local` is a later optimization for two tenants on the same host. It must
lock tenant names in a stable order, stage and validate both configs before
committing either, exchange only public halves between tenant contexts, and
roll back both sides on a partial commit. Naming two tenants as root is the
explicit trust decision; the command must still show the resulting bindings
before commit.

### Config and Ledger Transaction

The active TOML is the sole authoritative source of active contact secrets.
Pending records, candidates, rollback snapshots, and migration archives are
bounded transactional or operational copies, not a second contact database.
Do not introduce a second long-lived contact-secret sidecar merely to make
rendering easier.

All config writers must share one updater and lock. The baseline template owns
the generated listener, RPC, network, seed, and default channel values. The
`[contact]` subtree is user/key-manager-owned and is preserved during
`patch-env`. A multi-file transaction cannot rely on independent atomic
renames. The updater must use this commit order:

1. Acquire the tenant DarkIRC config lock and recover or block on any unfinished
   journal before accepting a new operation.
2. Reject an invalid owner, mode, symlink, hard-link anomaly, or non-regular
   destination. Verify the stable tenant scope ID.
3. Parse the existing TOML and proposed baseline structurally. Preserve every
   contact unless this transaction explicitly changes it.
4. Validate contact names, key encoding/length, duplicate fields, duplicate
   keys, generator compatibility, and conflicts with the ledger.
5. Build complete config and ledger candidates in protected same-directory
   files, parse them again, calculate their hashes, and `fsync` both. Run a
   non-starting config validation if the attested binary exposes one.
6. Write and `fsync` a secret-free transaction journal containing the
   transaction ID, operation, tenant/contact IDs, both offer IDs, transcript,
   generation, expected old hashes, proposed new hashes, and candidate paths.
7. Atomically commit and `fsync` the ledger first. For install/rotation it
   reserves the offer, transcript, and generation as `Installing`; for
   revocation it durably records the tombstone before contact removal.
8. Atomically commit and `fsync` the config second, then advance and `fsync` the
   journal to `Installed`.
9. Activate through mt-admin. On local health success, advance the ledger to
   `LocallyActivated` and the journal to `Committed`. A later bidirectional DM
   test advances the ledger to `PeerVerified`.
10. Clean up candidates, pending state, and the bounded rollback snapshot only
    according to the operation's verification policy. Rotation retains the old
    snapshot until peer verification or an explicit maintenance-timeout
    decision.

This ordering favors security over temporary availability: an offer can be
reserved before its config is live, and a revocation can be recorded while the
old contact still needs removal. It must never produce a live new contact with
an unconsumed offer or remove a revocation tombstone merely to restore service.

On startup and before every write, recovery compares the journal's expected
hashes with the config and ledger. If only the ledger commit happened, recovery
finishes the exact recorded config commit or marks the install rolled back; it
does not free the offer for a different transcript. If both commits happened,
it resumes activation. The unexpected state "new config, old ledger" blocks
service activation until the ledger is reconstructed from the journal. If no
safe deterministic action matches the hashes, the helper fails closed and
requires operator repair.

Regex replacement, `sed`, shell interpolation of nicknames, and append-only
contact edits are not acceptable. Before the journal is durable, parse or
validation failure leaves the existing files untouched. After that point,
failure follows the recorded recovery procedure rather than ad hoc cleanup.

### Activation, Failure, and Rollback

The currently documented portable activation is
`lunarwing-mt-admin.sh restart-tenant <TENANT>`, which restarts the full tenant
stack. The first implementation may use that existing path, but it must make
the blast radius explicit and allow an operator to defer activation while
batching contacts.

A later `restart-darkirc` or reload command belongs inside mt-admin, where it
can implement equivalent systemd user and OpenRC behavior. No onboard helper,
web process, or key helper may call bare `systemctl` or `rc-service`. A HUP path
must not become the default until it is verified for the attested daemon and
both init layouts.

Current mt-admin start behavior is not a strict DarkIRC activation check: the
main tenant can be healthy while an optional DarkIRC service failed. Before
managed writes ship, mt-admin needs the proposed `darkirc-health <tenant>
--strict` contract. It must use the internal init abstraction to require both
the tenant DarkIRC daemon and adapter service to be running, then call the
authenticated adapter `/health` internally and require `status=ok` and
`irc_connected=true`. It must never print the adapter bearer secret. The same
contract and result categories apply on systemd user units and OpenRC, with a
bounded reconnect wait and timeout rather than an unbounded poll.

`apply` advances to `LocallyActivated` only after that strict check. This still
does not prove that the peer installed the matching keys. If activation fails,
the transaction journal
drives restoration of the protected previous TOML, advances the ledger to
`RolledBack` while keeping the offer reserved to the same transcript, runs the
same mt-admin lifecycle path, retains retry state, and reports only the stage
and sanitized error category. A failed rollback is a high-priority operator
condition; it must not trigger repeated restart loops.

Rollback is appropriate for a deployment or parser failure. Suspected private-
key compromise requires a newly verified rotation and revocation, not reuse of
the old config.

### Rotation and Revocation

The current daemon schema exposes one active local secret and peer public key
per contact. Rotation therefore cannot be zero-downtime unless DarkIRC first
adds multi-generation support. The managed rotation flow must be explicit:

1. Keep the current generation active while each peer prepares a new role-bound
   offer/response. The rotation transcript binds both stable contact IDs, both
   current fingerprints, both proposed fingerprints, and the next locally
   expected generations. Bundle generation values are still untrusted hints;
   each ledger authorizes its own next generation.
2. Require both operators to confirm the new peer fingerprint, then choose a
   bounded maintenance window. Each helper checks the role-bound transcript
   internally; confirmation does not change the active TOML.
3. Each peer commits its local rotation through the normal journal. Because the
   two hosts cannot atomically switch together, DMs may fail between the first
   and second cutover. A side that has switched reports `LocallyActivated`, not
   success for the pair.
4. Complete a bidirectional encrypted DM round trip with the new generation.
   Only then mark both sides `PeerVerified`, retire the old rollback snapshot,
   and clean up the old private key under the retention policy.
5. If one side cannot cut over before the maintenance timeout and the old key
   is not suspected compromised, both operators may use their journals to
   restore the old generation. If compromise is suspected, neither side may
   roll back to it; disable/revoke the contact until a fresh exchange succeeds.

Messages delayed under the old generation may become undecryptable after
cutover. The UI and CLI must say this before commit.

Revocation requires the current peer fingerprint, not only a nickname. The
ledger commits the revocation tombstone before the config removes the contact.
If config removal or activation then fails, the tombstone remains and recovery
retries removal; it is not rolled back to make the contact usable. Revocation
on one tenant cannot force the peer to delete its copy, so both operators must
revoke independently when that is the intent.

## Security Properties

| Property | Design control | Residual risk |
| --- | --- | --- |
| Private-key confidentiality | Private pipes and zeroized memory, no user-visible secret argv/env/output, `0700` directories, `0600` files | Tenant/root/host compromise can read protected copies |
| Peer-key authenticity | Independent fingerprint confirmation plus role-bound transcript validation | Human can verify the wrong channel or identity |
| Replay resistance | Random offer IDs, expiry, local generations, consumed-ID ledger, revocation tombstones | Ledger rollback by root or restored snapshots |
| Config integrity | Shared lock, semantic TOML edit, journaled commit order, validation, atomic per-file rename | Undetected daemon semantic changes across profiles |
| Multi-tenant isolation | Registry-derived fixed paths, tenant UID, no arbitrary paths, no symlink following | Root remains a fleet-wide trust point |
| Availability | Candidate validation, bounded rollback, explicit activation | Full tenant restart has wider impact until targeted restart exists |
| Secret minimization | Enumerated protected copies, bounded retention, metadata-only ledger/journal | Snapshots and storage may retain deleted blocks |
| Init agnosticism | All lifecycle actions delegated to mt-admin | Both init implementations require parity tests |
| Auditability | State transitions, IDs, generations, and results without raw keys/bundles | Metadata can still disclose relationships |

Public keys are not confidential, but the default logs should still record
fingerprints rather than full keys or bundles. This minimizes metadata, avoids
normalizing key material in log pipelines, and makes accidental secret logging
easier to detect.

## What Not To Do

- Do not pass a private key in a command argument, environment variable,
  service unit, shell variable printed under `set -x`, or HTTP request.
- Do not print the raw key-generator output. Capture stdout and stderr; on
  failure, log the exit status and a sanitized category only.
- Do not write secrets to `/tmp`, a repo checkout, the LunarWing workspace, a
  world/group-readable file, a QR code, clipboard history, ticket, or chat.
- Do not create, commit, or rely on `darkirc_keypair.yaml`. It is not the
  daemon's active config.
- Do not store private keys in `/etc/lunarwing/ports.json`, tenant env files, a
  public exchange artifact, or a central fleet registry.
- Do not reuse a tenant-wide or fleet-wide keypair for multiple contacts.
- Do not trust a nickname, sender pairing approval, public directory result,
  or first key seen without explicit confirmation.
- Do not accept `--yes` as a substitute for an expected fingerprint.
- Do not silently replace an active contact when a new public key arrives.
- Do not edit TOML with regexes or direct append operations.
- Do not let web, onboarding, or migration wrappers reimplement tenant
  lifecycle or init detection outside mt-admin.

## Existing Contact Migration

Existing daemon-valid manual contacts should continue working when adoption
finds no known key reuse or tenant-scope conflict. Adoption does not
retroactively prove that a legacy contact satisfies the new uniqueness and
identity invariants:

1. Ship the contact-preserving config updater first.
2. Add `doctor` in read-only mode. It parses current contact tables using the
   attested compatibility profile and reports contact names plus public
   fingerprints only.
3. With explicit operator approval, create metadata-only ledger entries marked
   `legacy-active`. Do not copy active private keys into the ledger or a second
   store.
4. Compare legacy private values in protected process memory to detect reuse
   within a tenant. An optional root-run fleet doctor may compare ephemeral,
   non-persisted keyed equality tags across tenants without logging the tags or
   values. Derive and compare local public fingerprints only if the attested
   compatibility profile supports safe derivation. Mark reuse or unverifiable
   derivation as `legacy-noncompliant`, not fully managed.
5. Preserve unknown accepted fields during the first release and flag them for
   review. In particular, do not depend on the stale documented
   `my_dm_chacha_public` assignment; reconcile it only after profile-specific
   compatibility is tested.
6. Exercise `patch-env`, upgrade, export, import, and tenant rename against the
   adopted contacts before enabling write commands.

The current generic tenant export includes nested DarkIRC state, and import can
restore that state before target-specific DarkIRC enablement and ports are
reconciled. With managed exchanges, that behavior could also capture pending
secrets, candidates, journals, and rollback snapshots. Managed writes must not
ship until migration either implements the contract below or fail-closes every
export, import, and upgrade when managed key state exists.

The proposed `darkirc-contacts-v1` migration manifest contains the tenant scope
ID, active contact tables, metadata ledger/tombstones, attested generator
profile, and checksums. It is secret-bearing because active contact tables
contain local private keys. The containing export must be owner-only and
handled as a credential archive. It excludes listener/RPC/port settings,
pending exchanges, candidates, transaction journals, and rollback snapshots.
Export refuses to run while any excluded transactional state or unresolved
verification decision exists.

On import, mt-admin first reconciles the DarkIRC enablement flag and scope-ID
uniqueness, renders a fresh target-specific baseline with target ports, and
then structurally imports contacts and ledger metadata through the normal
journal. It validates the target binary profile and strict health before start.
A same-tenant move may preserve the scope ID only while the old host remains
stopped. Importing or cloning keys into a different tenant scope is rejected
until contacts complete a verified rotation. Keep the old host stopped but
intact until both peers complete a DM round trip on the target.

## Onboarding and Future Web UX

Onboarding may offer "set up a DarkIRC contact" only after DarkIRC is explicitly
enabled. It should invoke the same mt-admin/key-helper contract. It must not
generate unused keys by default, auto-accept a peer, or make successful sender
pairing imply key confirmation.

A future authenticated web surface can expose these views:

- Contacts: nickname, peer fingerprint, generation, local activation, and peer
  verification state.
- Pending exchanges: expiry, public offer download/QR, and cancel action.
- Complete exchange: public offer upload/paste and required fingerprint entry.
- Rotation/revocation: explicit current and proposed fingerprints.
- History: sanitized state transitions and results.

The browser may receive public offers and fingerprints only. Private keys,
private generator output, secret-bearing pending records, and rollback files
never enter a browser response. The web service must call a narrowly
privileged, allowlisted broker that invokes mt-admin with structured values; it
must not build a root shell command. Existing authentication, CSRF/origin
checks, body limits, rate limits, and audit controls remain mandatory.

The server derives the allowed `darkirc_scope_id` from the authenticated
principal and server-side authorization data. Request bodies do not supply an
OS tenant name, scope ID, config path, or arbitrary command. A fleet operator
authorized for several tenants selects from a server-generated allowlist, and
the broker resolves that selection to one structured mt-admin action. A
browser-controlled value can never directly select another tenant's path.

Possible future API names, all explicitly unimplemented:

```text
POST   /api/admin/darkirc/contacts/offers
POST   /api/admin/darkirc/contacts/responses
POST   /api/admin/darkirc/contacts/complete
GET    /api/admin/darkirc/contacts
GET    /api/admin/darkirc/exchanges
DELETE /api/admin/darkirc/exchanges/{offer_id}
POST   /api/admin/darkirc/contacts/{contact_id}/rotate
POST   /api/admin/darkirc/contacts/{contact_id}/revoke
POST   /api/admin/darkirc/apply
```

## Rollout Plan

### Phase 0: Confirm Compatibility and Config Ownership

- Record and attest the actual shared DarkIRC binary digest, source revision
  when known, and approved compatibility profile.
- Test that profile's exact `darkirc --gen-chacha-keypair` output contract and
  confirm its contact schema.
- Replace direct whole-file truncation with the shared locked, semantic,
  contact-preserving updater.
- Define `darkirc_scope_id`, the transaction journal, rollback path/retention,
  and strict `darkirc-health` contract.
- Make export/import/upgrade fail closed on managed state until the structured
  migration contract is available.
- Add regression tests proving `patch-env` and repeated provisioning preserve
  contacts and file modes.
- Update stale DarkIRC docs when implementation begins.

**Exit condition:** existing manual contacts survive normal config maintenance,
unresolved state cannot enter generic migration, and strict DarkIRC health is
available without any key-exchange write command enabled.

### Phase 1: Adoption and Migration Safety

- Add `list`, `status`, and `doctor`.
- Inventory existing contacts using fingerprints only.
- Generate stable scope IDs, add the metadata-only ledger, and add recovery
  diagnostics.
- Implement `darkirc-contacts-v1` export/import with fresh target config render,
  scope checks, transactional-state exclusion, and target-port validation.
- Test invalid TOML, legacy fields, duplicate names, tenant rename, and migrated
  state without modifying active secrets.

**Exit condition:** operators can account for every current contact, detect
unsafe state, and migrate a settled contact set without stale ports or
transactional files before enabling exchange writes.

### Phase 2: Managed Out-of-Band Exchange

- Add `prepare`, `respond`, `complete`, `cancel`, `apply`, and
  `confirm-roundtrip`.
- Freeze Public Exchange V1 canonicalization, role binding, and fingerprint
  rules.
- Support public offer files, stdin/stdout public-only mode, and optional QR.
- Require explicit fingerprint confirmation and fail closed for unattended
  calls without an expected fingerprint.
- Activate through mt-admin and require the strict DarkIRC service plus adapter
  health contract before local success.

**Exit condition:** two test tenants can exchange role-bound artifacts,
activate contacts, and complete encrypted DM round trips with private keys
confined to the enumerated protected and transient locations.

### Phase 3: Lifecycle Completion

- Add rotation, revocation, same-host `pair-local`, and bounded history.
- Add `restart-darkirc` inside mt-admin after systemd-user/OpenRC parity tests.
- Exercise the defined rollback retention and compromise-specific no-rollback
  behavior for rotation and revocation.

**Exit condition:** rotation, revocation, rollback, and same-host partial-commit
recovery are tested on both init systems with no cross-tenant key exposure.

### Phase 4: Optional Delivery Surfaces

- Add an onboarding wizard and web UI using the same commands and state machine.
- Evaluate a mutual protocol over an existing authenticated channel.
- Require a separate threat model and cryptographic review before allowing
  protocol-driven confirmation or signing identities.

**Exit condition:** the new surface changes only public-offer delivery; it does
not create a second secret store or bypass fingerprint/identity policy.

Each phase is opt-in and reversible. Disabling new commands must leave valid
active TOML contacts usable through the documented manual fallback.

## Testing Strategy

### Unit and Property Tests

- Parse every approved generator profile and reject unknown binary digests,
  unknown output, partial, duplicate, malformed, all-zero, or wrong-length keys
  without echoing captured output.
- Canonicalize Public Exchange V1 and test role binding, transcript domain
  separation, and fingerprints over decoded key bytes.
- Reject a response bound to another offer/contact/scope and prove that remote
  generation or predecessor fields cannot authorize local replacement.
- Exercise expiry, clock skew, replayed IDs, monotonic generations, revocation
  tombstones, idempotency, and conflicting active contacts.
- Test scope-ID creation, uniqueness, rename preservation, same-tenant import,
  and clone/different-scope rejection.
- Test nickname normalization, case collisions, TOML quoting/injection, path
  separators, control characters, oversized bundles, and unknown fields.
- Test every legal and illegal exchange state transition.
- Prove semantic TOML updates preserve unrelated settings and contacts.

### Filesystem and Secret-Handling Tests

- Assert `0700` directories, `0600` files, tenant owner/group, `umask 077`, and
  same-filesystem atomic rename.
- Reject symlinks, hard-link surprises, non-regular files, changed ownership,
  and tenant/path mismatches.
- Capture argv, environment, stdout, stderr, traces, and logs across success and
  every failure path; scan them for generated private values.
- Inject crashes before and after pending write, both candidates, journal
  `fsync`, ledger rename, config rename, activation, rollback, tombstone, and
  cleanup; verify deterministic hash-based recovery.
- Run concurrent `patch-env`, prepare, complete, revoke, and rotation attempts
  and verify serialization without lost updates.
- Enforce one rollback snapshot per contact, the 24-hour decision window,
  seven-day extension cap, verification cleanup, and overdue mutation block.

### Integration Tests

- Use a fake generator for deterministic failure cases and every supported
  attested DarkIRC binary profile for compatibility tests.
- Complete a full exchange between two OS-user tenants and prove neither can
  read the other's pending or active private key.
- Verify DarkIRC-disabled tenants fail before generating state.
- Verify `patch-env`, repeated provisioning, upgrade, export/import, and tenant
  rename preserve active contacts and target-specific ports.
- Prove export rejects pending/journal/candidate/rollback state and that
  `darkirc-contacts-v1` excludes those files while preserving settled contacts,
  ledger tombstones, and scope identity.
- Feed the installed config to the attested daemon and complete encrypted DM
  round trips in both directions.
- Force activation and health-check failure, then verify config and service
  rollback without repeated restart loops.
- Test same-host two-tenant partial commits before enabling `pair-local`.

### Init-System Matrix

- Run lifecycle and rollback scenarios through mt-admin on systemd user units.
- Run the same scenarios through mt-admin on OpenRC/Gentoo.
- Require strict daemon/adapter service status and authenticated adapter health
  with `irc_connected=true` on both init systems.
- Grep onboard, web, and helper code to ensure no bare tenant `systemctl` or
  `rc-service` paths were introduced.

No performance benchmark is needed on the DarkIRC message path because the
design does not change it. Admin-time tests should bound key generation,
locking, and activation duration and ensure one stalled exchange cannot hold a
tenant lock indefinitely.

## Open Questions

1. How should mt-admin record and attest the actual shared DarkIRC binary,
   source revision, and compatibility profile? Does each supported profile
   expose machine-readable keypair output or a non-starting config-validation
   mode? If not, should LunarWing add a profile-specific parser or request an
   upstream JSON mode?
2. What exact public-key decoding and fingerprint display format should be
   frozen for interoperability? The underlying hash should cover decoded key
   bytes, not presentation text.
3. What pending-offer lifetime balances asynchronous operator work with
   secret minimization? Twenty-four hours is a reasonable starting point, but
   production operations may require a configurable bounded window.
4. Should the first release activate automatically after completion, or default
   to `Installed` and require an explicit `apply` to avoid a full tenant
   restart?
5. Can each supported attested daemon reliably reload contacts without a full
   restart on both init systems? This must be demonstrated before a reload
   command is preferred.
6. What encryption, retention, and custody policy should apply to operator
   backups and `darkirc-contacts-v1` archives after they leave tenant state?
7. Should `darkirc-contacts-v1` be embedded in the existing tenant archive or
   be a separately approved secret-bearing artifact?
8. Which existing channel, if any, has a strong enough identity and
   authorization contract to carry future mutual exchange messages?

## Explicit Deferrals

- Tor hidden-service provisioning, onion identity management, global DarkIRC
  P2P peering, and additional seed infrastructure remain separate work tracked
  conceptually in [DarkIRC Things to Add](DARKIRC_THINGS_TO_ADD.md).
- Automatic exchange over DarkIRC public messages or unencrypted DMs is
  rejected, not merely postponed.
- A public contact directory, nickname-based TOFU, DNS discovery, and automatic
  trust-on-first-message are deferred unless a separate identity design is
  approved.
- A central LunarWing key broker, private-key escrow, and fleet-wide recovery
  service are out of scope.
- Option 3 signed offers remain unselected until LunarWing has a general tenant
  signing-identity lifecycle. PAKE/SAS rendezvous and a new online relay require
  a separate protocol design using reviewed cryptographic building blocks.
- Seamless rotation with dual active keys, delayed-message decryption across
  generations, forward secrecy, ratchets, and post-quantum changes require
  DarkIRC protocol support and are not part of this proposal.
- Single-node/non-multi-tenant packaging may reuse the typed helper later, but
  the initial operator and lifecycle contract is multi-tenant mt-admin.

## Implementation Acceptance Criteria

This proposal is ready to be considered implemented only when all of the
following are demonstrated:

- Generated private keys appear only in generator/private-pipe/zeroized helper
  memory and the enumerated `0600` pending, candidate, active, rollback, or
  explicitly protected migration copies. They never appear in user-visible
  process metadata, output, logs, public artifacts, or browser responses.
- An unattended completion without an independently supplied expected
  fingerprint fails closed.
- Interactive completion requires full fingerprint entry or scan; a generic
  confirmation and `--yes` cannot bypass it.
- Unknown binary digests, compatibility profiles, key formats, artifact roles,
  offer bindings, and transcript hashes fail before state changes.
- Replayed identical operations return the same public artifact or committed
  state; conflicting operations return a nonzero result without mutation.
- Existing manual contacts survive `patch-env`, upgrade, export/import, and
  adoption without forced rotation unless reuse or a tenant-scope conflict is
  detected.
- Generic export/import cannot capture pending, candidate, journal, or rollback
  files. Structured migration renders target ports, verifies scope identity,
  and passes strict DarkIRC health before completion.
- Config and ledger updates are structured, locked, validated, journaled in the
  specified order, and recoverable at every crash point.
- Cross-tenant path, symlink, nickname-injection, replay, and concurrent-update
  tests pass.
- Two tenants complete bidirectional encrypted DM tests with distinct
  per-contact keys.
- Local activation requires strict DarkIRC daemon/adapter status plus
  authenticated adapter health with `irc_connected=true`; activation and
  rollback pass through mt-admin on systemd user units and OpenRC, with no bare
  init calls in wrappers.
- Rollback storage obeys the one-per-contact count, 24-hour decision window,
  seven-day extension cap, overdue mutation block, and peer-verification
  cleanup rule.
- Rotation and revocation require the current and proposed fingerprints, obey
  the two-party cutover/tombstone ordering, recover same-host partial commits,
  and never roll back a suspected-compromised key.
- Documentation is updated to remove the stale YAML and third-field claims and
  to describe the public-only exchange flow without claiming automatic trust.
