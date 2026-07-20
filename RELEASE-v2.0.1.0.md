# Release Notes for LunarWing v2.0.1.0 - Codename `Togishi (研師)`

**Release Date:** TBD

**Status:** Draft

**Git Comparison:** `v2.0.0.0` (`02cb832`) through `32c2940` on 2026-07-20

> A *togishi* is a Japanese sword polisher. LunarWing v2.0.1.0 follows that
> theme: it does not replace the v2 foundation, but sharpens its channel
> boundaries, Engine V2 compatibility, skill lifecycle, migration tooling, and
> multi-tenant operations.

## Overview

LunarWing v2.0.1.0 is a compatibility, security, and operations release built
on v2.0.0.0. Its largest change is the completion of the local Engine V2
compatibility matrix and the associated channel-parity work for WeeChat and
DarkIRC. The release also completes the current B-1/B-2/B-3 self-improving
skill lifecycle behind its default-off feature gate, adds automatic and
secret-safe WeeChat relay bootstrap, introduces encrypted Kawarimi bundles,
adds source-registry validation, and hardens OpenRC tenant environment loading.

The compared tree contains 83 commits, including 58 non-merge commits, and
changes 284 files. The release uses two related version forms:

- The release, tag, and this document use `2.0.1.0`.
- Rust crates and package manifests use `2.0.1`.
- The release toolchain is Rust 1.96.
- No new SQL migration file was added after `v2.0.0.0`.

Historical v1 notes remain in the [release archive](docs/releases/README.md),
including the [v1.1.9.0 release notes](docs/releases/RELEASE-v1.1.9.0.md). The
v2.0.0.0 notes remain available from the `v2.0.0.0` Git tag.

## Highlights

- Engine V2 now has hermetic local compatibility coverage through real MCP
  HTTP/OAuth, Wasmtime tool execution, skill migration and selection,
  TensorZero-shaped streaming, channel delivery, and the browser gateway.
- CHPAR-001 through CHPAR-011 are implemented and verified: secret-safe channel
  configuration, WeeChat proactive delivery, UTF-8-safe output, auth privacy,
  pairing-by-default DMs, Engine V2 attachments, external-waiting statuses,
  secure owner routing, real IRC WASM tests included in a new comprehensive
  testing suite, and versioned IRC identities.
- WeeChat fresh installs now default DMs to `pairing`; existing persisted policy
  remains authoritative unless an operator explicitly overrides it.
- Ordinary Engine V2 turns can receive sanitized attachment context and
  transient image bytes without persisting image payloads or data URLs.
- Engine V2 authentication now resumes the exact waiting thread after token or
  OAuth completion, refreshes newly activated tool leases, and avoids duplicate
  completion delivery.
- Self-improving skills gained scoped terminal feedback, automatic-demotion
  quarantine, reviewed pruning, registry publication, update detection,
  validated downloads, and owner-only shared-skill mutation. The subsystem
  remains disabled by default.
- Fresh multi-tenant installs can generate a loopback-only WeeChat relay
  configuration automatically, with a supported opt-out and recovery path.
- Kawarimi exports now default to encrypted, header-protected 7z bundles;
  imports accept both the new format and legacy plaintext tar bundles.
- `lunarwing registry validate` reports malformed, duplicate, or semantically
  invalid source registry manifests before installation.
- OpenRC services load tenant environment files only after privilege drop
  through a literal parser with strict path, ownership, mode, symlink, and
  hardlink checks.
- DarkIRC-enabled tenants seed the adapter credential into the encrypted
  secrets store through the authenticated loopback gateway without putting the
  credential or gateway token in `curl` arguments.
- Bug, architecture, and proposal documentation was reconciled against current
  source and tests.

## Compatibility

| Area | v2.0.1.0 behavior |
|---|---|
| Release version | `2.0.1.0` |
| Crate/package version | `2.0.1` |
| Rust toolchain | Rust 1.96 |
| SQL migrations since v2.0.0.0 | None |
| Engine V2 binary default | Off unless `ENGINE_V2=true` |
| Newly rendered multi-tenant environments | `ENGINE_V2=true` |
| Gateway Engine V2 routing | Enabled whenever Engine V2 is enabled |
| XMPP, DarkIRC, and WeeChat Engine V2 routing | Exact opt-in through `ENGINE_V2_CHANNELS` |
| WASM channel streaming | Final responses and statuses; incremental token chunks remain a no-op |
| WeeChat new-install DM policy | `pairing` |
| Existing WeeChat DM policy | Persisted `open`, `allowlist`, or `pairing` is preserved unless explicitly overridden |
| IRC conversation identity | Versioned account-or-nick principals; old conversations are retained but not auto-merged |
| WASM owner actor configuration | New string `wasm_channel_owner_actor_ids`; legacy numeric map remains supported |
| Channel WIT | Adds `external-waiting`; rebuild all channel components |
| Skill self-improvement | Off unless `SKILL_SELF_IMPROVEMENT=true` |
| Skill prune quarantine | 30 days by default; configurable with `SKILL_PRUNE_QUARANTINE_DAYS` |
| Kawarimi export format | Encrypted `.7z` by default; plaintext `.tar` only with `--no-encrypt` |
| Kawarimi import format | Encrypted `.7z` and legacy plaintext `.tar` |

## Changes

### Engine V2 Local Compatibility

The Phase 5 local compatibility work moved from source-level confidence to
production-boundary tests. The matrix now covers:

- real MCP initialization, tool listing, tool calls, OAuth discovery, dynamic
  client registration, token exchange, encrypted token storage, same-thread
  callback resume, and exactly-once execution;
- a real Wasmtime component using the normal capability and policy path,
  including denied-capability behavior;
- v1 skill migration, idempotence, deterministic selection, activation status,
  and model-visible context injection;
- TensorZero-shaped OpenAI SSE through the real Rig adapter, including text,
  usage, fragmented tool fields, mid-stream JSON errors, and premature EOF;
- browser streaming, approval, authentication, secret non-disclosure,
  interrupt acknowledgement, cancelled-terminal suppression, and same-thread
  recovery; and
- channel-neutral terminal delivery, original metadata preservation, scoped
  controls, and the intentional WASM `StreamChunk` no-op boundary.

The compatibility work also fixed several production paths exposed by those
tests:

- OAuth callbacks and direct token submissions resume the matching Engine V2
  authentication gate rather than only updating the legacy session path.
- Authentication completion is routed through the originating channel once,
  even when a token was already stored by an HTTP authentication flow.
- Resumed threads refresh capability leases so tools activated while a thread
  waited are available immediately after resume.
- Authentication error detection is case-insensitive.
- Provider-safe tool names with mixed `-` and `_` separators resolve only when
  the normalized match is unique.
- The orchestrator accepts both the engine action-call representation and the
  Python wire shape (`call_id`, `name`, and `params`) when rebuilding its
  internal transcript.
- `/interrupt` and `/clear` discard matching conversation-scoped pending gates
  as well as active thread state, preventing a completed-thread credential gate
  from consuming a later ordinary message.
- Credential-response attachments are not grafted onto the original request
  when authentication fallback retries it.

See [Engine V2 Architecture](docs/architecture/ENGINE-V2.md) and the
[Phase 5 implementation record](docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-5.md).

### Engine V2 Channel Parity

The channel-parity audit was converted into twelve tracked work items.

| Work item | Result in v2.0.1.0 |
|---|---|
| CHPAR-001 | Removed raw secret-bearing WASM and guest channel configuration logging; safe summaries retain key names and non-secret state only |
| CHPAR-002 | Implemented WeeChat proactive delivery for explicit `irc.<network>.<target>` destinations, including relay failure propagation and attachment rejection |
| CHPAR-003 | Made WeeChat status and message chunking UTF-8 safe |
| CHPAR-004 | Suppressed `AuthRequired` and `AuthCompleted` from WeeChat group buffers while retaining DM delivery |
| CHPAR-005 | Changed the WeeChat fresh-install DM default from `open` to `pairing`; unknown values fail closed and persisted values survive upgrades |
| CHPAR-006 | Added sanitized text and transient image input to Engine V2 while keeping control/auth messages and durable history free of binary payloads |
| CHPAR-007 | Added a dedicated `ExternalWaiting` status across the host and channel WIT; IRC group delivery remains privacy-filtered |
| CHPAR-008 | Added secure string owner actors and validated persisted protocol targets while preserving legacy numeric owner IDs |
| CHPAR-009 | Added real DarkIRC WASM integration coverage for pairing, routing, owner delivery, gates, auth, and scoped interruption |
| CHPAR-010 | Added real WeeChat WASM integration coverage for Basic auth, proactive delivery, failure propagation, isolation, gates, auth, and interruption |
| CHPAR-011 | Added versioned, case-normalized IRC principals and documented their trust and migration boundaries |

The full evidence and acceptance criteria are in the
[channel-parity work items](docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md),
the [source audit](docs/reviews/ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md),
and the [live validation record](docs/reviews/ENGINE_V2_CHANNEL_PARITY_LIVE_VALIDATION_2026-07-19.md).

### IRC Identity and Owner Routing

DarkIRC and WeeChat now use explicit versioned principals for pairing,
conversation continuity, and owner matching:

- DarkIRC uses `darkirc:nick:<rfc1459-folded-nick>`.
- WeeChat prefers `account:<network>:<case-folded-account>` when an authenticated
  account tag exists and otherwise uses `nick:<network>:<case-folded-nick>`.
- WeeChat honors ASCII, RFC1459, and strict-RFC1459 case mappings supplied by
  the adapter and defaults to RFC1459.
- WeeChat groups remain scoped to the full IRC buffer rather than an individual
  sender.
- Hostmasks and original nicks remain routing/audit metadata, not authenticated
  identity.

The new setting
`channels.wasm_channel_owner_actor_ids.<channel>` accepts string principals and
takes precedence over the existing numeric
`channels.wasm_channel_owner_ids.<channel>` map. Only an exact owner-principal
match may replace owner-scoped proactive routing metadata. WeeChat persists a
validated full buffer target; DarkIRC persists one validated DM nick.

This intentionally changes new IRC DM conversation keys. Existing conversation
records are retained and are not automatically merged into a new principal,
avoiding silent continuity transfer after nick or account reuse. See
[IRC Sender Identity](docs/architecture/IRC-SENDER-IDENTITY.md).

### Engine V2 Attachments

Attachments are processed only after submission parsing and pending-auth
resolution identify an ordinary user turn:

- document extraction and audio transcription are represented in sanitized
  effective text;
- source URLs and host storage paths are excluded from model-visible durable
  context;
- input validation, policy checks, and inbound secret scanning run before the
  engine thread starts;
- bounded image bytes become `TransientContentPart::Image` values;
- transient parts are skipped by serde and use redacted debug output;
- only opaque image IDs enter the Python orchestrator transcript;
- image bytes become provider-native data URLs only at the final LLM adapter
  boundary; and
- durable Engine state, traces, and compatibility history retain descriptions
  and extracted text, but never image bytes or data URLs.

Image bytes do not survive a process restart. The user must resend the image if
visual analysis is needed after restart. Channel/host ingress still owns count,
MIME, per-file, and aggregate limits.

### Self-Improving Skills

The Rust terminal feedback path is now wired into normal Engine V2 completion.
It records activated skill outcomes as follows:

- `Completed` records success.
- `Failed` and `MaxIterations` record failure.
- `Stopped` records no observation.
- `GatePaused` preserves feedback metadata until resume.
- duplicate skill IDs are recorded once;
- project, user, and shared visibility are checked before mutation; and
- one terminal pass processes at most 64 unique skill IDs.

A disposable OpenRC tenant test confirmed a selected skill moving from zero to
one use and one success, with feedback metadata cleared and no duplicate
recording in the normal non-crash flow.

The B-1/B-2/B-3 lifecycle was also completed after the initial audit:

- B-1 patch proposals retain optimistic content-hash checks, bounded patch
  history, pre-patch metrics, and a fresh evaluation epoch. Applying a patch
  reactivates only automatically demoted skills; explicit operator demotion is
  preserved.
- B-2 records typed automatic-demotion provenance. An automatically demoted
  skill becomes eligible for a reviewed prune proposal after a configurable
  quarantine rather than requiring usage it can no longer accumulate. Pruning
  remains user-approved soft archival, never automatic deletion.
- B-3 can publish eligible trusted skills, stamp local registry provenance,
  check installed skills for updates every 24 hours, download and leak-scan
  replacements, stage hash-bound update proposals, and replace prompt,
  activation, and code-snippet content only after approval.
- Registry skill downloads are bounded to 10 MiB and 64 archive files.
- Shared skill mutations require the configured instance owner.
- Authored and externally installed skills retain demotion/prune protections.
- Skill documents are excluded from generic retrieval so deterministic skill
  selection remains the only prompt-injection path.

`SKILL_SELF_IMPROVEMENT` remains false by default. Enabling it turns on terminal
feedback, automatic demotion, maintenance missions, proposal hooks, and the
registry update sweep together. `SKILL_PRUNE_QUARANTINE_DAYS` defaults to 30.

The original [self-improving skills audit](docs/reviews/SELF_IMPROVING_SKILLS_AUDIT_2026-07-15.md)
records the problems found before the later fixes. The current implementation
and [feature parity matrix](ic/FEATURE_PARITY.md) supersede its resolved
findings.

### WeeChat Relay Bootstrap and Recovery

Fresh multi-tenant provisioning now attempts a one-shot WeeChat relay bootstrap
before service rendering:

- the password is stored in `relay.conf` only as the literal
  `${env:RELAY_PASSWORD}` expression;
- the resolved password enters WeeChat only through inherited environment;
- `allow_empty_password` is off;
- IPv6 relay mode is off before binding to `127.0.0.1`;
- the API relay uses the tenant's registry-assigned port;
- generated configuration is validated before atomic promotion; and
- the dedicated tenant-owned `env/weechat.env` is mode `0600` and contains only
  `RELAY_PASSWORD`, not the database, LLM, XMPP, or gateway environment.

Bootstrap is preserve-and-fail: an existing non-empty WeeChat configuration is
not overwritten. A bootstrap failure is non-fatal to base tenant provisioning
and reports a recovery action.

Operators can skip bootstrap with `--no-weechat-bootstrap` on `add-tenant` or
`add-tenants`. The option is also carried through the Python onboarding CLI,
browser wizard, and OpenRC bulk provisioner. Later recovery uses:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <tenant>
sudo ic/scripts/lunarwing-weechat-preflight.sh <tenant>
```

See [WeeChat Services](docs/ops/WEECHAT-SERVICES.md).

### Kawarimi Encrypted Bundles

`export-tenant.sh` now creates a 7z archive with AES-256 encryption and header
encryption by default. The passphrase is selected in this order:

1. `KAWARIMI_PASS`
2. `KAWARIMI_PASS_FILE`
3. hidden interactive prompt and confirmation

The bundle remains root-owned, mode `0600`, under a mode `0700` directory. It
contains the PostgreSQL dump, required source identity and master-key material,
operator configuration, and filtered state. Host-local gateway, bridge,
webhook, and relay tokens are not carried; the destination creates new ones.

`import-tenant.sh` auto-detects encrypted `.7z` and legacy plaintext `.tar`
bundles. A wrong passphrase or corrupt encrypted archive fails before tenant
restore. Legacy tar import remains available with an explicit warning.

Important current constraints:

- `7z`/p7zip is checked unconditionally by export, including `--no-encrypt` and
  dry-run invocations.
- `--no-encrypt` is intended for testing and produces a sensitive plaintext tar.
- Both export and import pass the 7z password through `-p...`; it can be visible
  briefly to same-host process inspection while 7z runs. Use a trusted host and
  account boundary.
- Current checklist evidence does not record a completed live encrypted
  export/wrong-password/import/restore matrix for this release candidate.
- Migration remains PostgreSQL-only and is a staged cutover with downtime.

### Registry Validation

The new command:

```bash
lunarwing registry validate
```

scans source registry manifests without installing or executing them. It reads
the `tools`, `channels`, and `mcp-servers` namespaces raw-first so one malformed
file cannot prevent reporting other findings. Reports are deterministically
sorted and use stable finding codes for:

- read failures and malformed JSON;
- invalid typed manifests and duplicate names;
- unsupported MCP transports;
- simultaneous or missing HTTP URL/stdio transport configuration;
- invalid stdio commands, arguments, environment entries, or authentication;
  and
- stdio definitions that do not explicitly use `auth: none`.

The command exits successfully only for a clean source registry.

### OpenRC Environment Boundary

OpenRC tenant services now use
`/usr/local/libexec/lunarwing-openrc-env-exec` after `command_user` has dropped
privileges. The helper:

- refuses to run as root;
- requires an absolute executable;
- rejects symlinked parent directories and environment files;
- requires a tenant-owned mode `0700` parent and mode `0600`, single-link file;
- accepts only valid `KEY=value` records; and
- exports values literally without shell sourcing, command substitution, or
  parameter expansion.

The boundary is used for the LunarWing daemon, XMPP bridge, proxy, WeeChat,
WeeChat adapter, and DarkIRC adapter OpenRC units. The installed helper itself
is checked as root-owned, single-link, and mode `0755`.

### DarkIRC Secret Seeding

For a DarkIRC-enabled tenant, `start-tenant` now waits for the loopback gateway
and submits `DARKIRC_ADAPTER_SECRET` to the DarkIRC setup API so the encrypted
SecretsStore can provide it to the WASM channel.

- The adapter secret is sent in the JSON body through stdin.
- The gateway bearer header is supplied through `curl -K` on a file descriptor.
- Neither value is placed in the `curl` argument list.
- Failure output is not echoed because a remote error body could reflect a
  credential.
- The operation is idempotent and best-effort; a failure warns without printing
  a secret.

The automated contact-key exchange proposal remains separate and unimplemented.
The work will ship in a finished state in a future release.
See [DarkIRC Multi-Tenant Operations](docs/ops/DARKIRC-MULTITENANT.md).

### CodeAct and Tool Calling

Regression coverage now proves that Monty/CodeAct preserves:

- keyword arguments as top-level JSON fields;
- positional arguments under `_args`;
- keyword arguments across `asyncio.gather()`; and
- parameters through `EffectBridgeAdapter`, capability leases, safety checks,
  built-in tools, and error results.

These tests protect the existing argument-conversion behavior; this cycle did
not introduce a new Monty wire format.

### Versioning, Tests, and Documentation

- The main daemon, internal crates, XMPP bridge/channel, Lunartica/Multica, DarkIRC,
  WeeChat relay, and self-heal version marker moved from `2.0.0` to `2.0.1`.
- The skills catalog added bounded ZIP decoding for registry downloads.
- New dedicated integration targets cover IRC WASM, MCP compatibility, skill
  selection, and TensorZero streaming behind `libsql,integration`.
- Architecture documents were refreshed for current Engine V2, memory, SSH,
  XMPP, WeeChat, and self-heal behavior.
- All active bug documents were reconciled into canonical fixed, partial, open,
  and unverified records.
- All tracked proposal records received a dated implementation-status note and
  the documentation index now covers the complete proposal set.
- The root README was made release-agnostic; version-specific detail lives in
  release notes such as this document.

## Security Fixes

- Removed raw channel configuration values from shared WASM and channel guest
  logs. Sentinel tests cover XMPP and WeeChat passwords.
- Prevented OAuth URLs, state, and auth-completion details from appearing in
  WeeChat group buffers.
- Changed fresh WeeChat DMs from implicit open access to pairing and made unknown
  policy values reject senders.
- Restricted owner-route persistence to exact configured owner principals and
  validated channel-specific targets before storing them.
- Kept Engine image payloads transient, non-serializing, and debug-redacted.
- Prevented credential-reply attachments from entering retried user prompts.
- Required thread-visible skill IDs for usage, patch, and prune host functions;
  shared-skill mutation requires the instance owner.
- Added leak scanning and bounded archive handling to registry skill update and
  publication paths.
- Replaced root-side OpenRC sourcing of tenant-controlled environment files
  with a post-drop literal parser.
- Moved DarkIRC setup credentials out of `curl` arguments and suppressed
  reflected failure bodies.
- Generated WeeChat relay configuration is loopback-only, disallows empty
  passwords, and exposes only its dedicated credential to the WeeChat process.

## Bug Fixes and Polish

- Fixed WeeChat proactive sends reporting success without an observable relay
  request.
- Fixed long multibyte WeeChat responses and statuses being cut inside UTF-8
  code points.
- Fixed invalid WeeChat proactive targets and relay failures being silently
  accepted.
- Fixed stale or completed Engine gates surviving `/interrupt` or `/clear` and
  consuming later input.
- Fixed duplicate authentication-completion notifications during Engine resume.
- Fixed Engine V2 OAuth completion activating an extension without resuming the
  waiting thread.
- Fixed newly activated tools being absent from a resumed thread's leases.
- Fixed mixed-separator MCP tool names failing provider-safe resolution.
- Fixed case-sensitive matching of authentication errors.
- Fixed the orchestrator dropping provider action-call context when restoring
  its Python wire transcript.
- Fixed automatic skill demotion becoming a dead end: patches can recover an
  auto-demoted skill and quarantine can lead to reviewed archival.
- Fixed skill prune hooks bypassing the feature gate or thread visibility.
- Fixed registry skill updates changing metadata without replacing validated
  content.
- Fixed successful skill publication failing to record local provenance.
- Fixed shared-skill proposal actions being available to any authenticated user
  instead of only the instance owner.
- Fixed OpenRC services reading tenant-controlled environment data in root-side
  hooks.

## Upgrade Notes

1. **Back up PostgreSQL, tenant state, and environment files.** No SQL migration
   was added, but this release changes channel identity, WIT, routing metadata,
   service units, and migration archives.
2. **Rebuild every WASM channel component.** `external-waiting` changes the
   channel WIT. Rebuild and reinstall XMPP, DarkIRC, and WeeChat components with
   the matching host binary before enabling channels.
3. **Re-render OpenRC units.** Existing OpenRC tenants need newly rendered units
   to use the post-drop environment launcher. Review the diff, restart through
   `lunarwing-mt-admin.sh`, and verify status afterward.
4. **Review WeeChat DM policy.** New installations use `pairing`. Existing
   persisted `open`, `allowlist`, or `pairing` remains in effect. Set `open`
   explicitly only when its security tradeoff is intentional.
5. **Do not overwrite an existing WeeChat configuration.** Automatic bootstrap
   and `configure-weechat-relay` preserve any non-empty config directory. Run
   the preflight first and use manual recovery when existing config needs repair.
6. **Account for new IRC principal keys.** New DMs use versioned network/account
   or nick principals. Old history remains stored but is not auto-merged. Review
   identity before any manual history migration.
7. **Configure string owner actors where appropriate.** IRC and JID channels can
   use `channels.wasm_channel_owner_actor_ids.<channel>`. Numeric owner IDs
   remain supported and require no migration.
8. **Keep channel rollout explicit.** The gateway remains Engine V2 eligible
   whenever `ENGINE_V2=true`; add only exact `xmpp`, `darkirc`, or `weechat`
   entries to `ENGINE_V2_CHANNELS`. WASM channels still receive terminal text,
   not live token edits.
9. **Install p7zip before Kawarimi export.** The current export script requires
   `7z` even for dry-run or `--no-encrypt`. Protect passphrases and migration
   bundles, transfer them over a trusted channel, and delete them after verified
   cutover.
10. **Leave skill self-improvement off unless deliberately evaluating it.** If
    enabled, review proposal ownership, the combined feature gate, the 30-day
    prune quarantine, and the crash-window limitation described below.
11. **Restart DarkIRC-enabled tenants through mt-admin.** The start path seeds
    the adapter secret after the gateway is reachable; inspect warnings if setup
    activation fails.
12. **Do not use the legacy in-place upgrader as a v1-to-v2 path.** Provision a
    separate v2 deployment and use a rehearsed, staged PostgreSQL Kawarimi
    migration.

### Configuration Quick Reference

| Setting | Default | Effect |
|---|---|---|
| `ENGINE_V2` | Binary `false`; new managed tenants `true` | Enables Engine V2 |
| `ENGINE_V2_CHANNELS` | Empty | Gateway only; exact `xmpp`, `darkirc`, and `weechat` entries opt in channels |
| `channels.wasm_channel_owner_actor_ids.<channel>` | Unset | String owner principal for non-numeric protocols |
| `SKILL_SELF_IMPROVEMENT` | `false` | Enables feedback, mutation missions/hooks, demotion, and registry update checks |
| `SKILL_PRUNE_QUARANTINE_DAYS` | `30` | Delay before automatically demoted skills can be proposed for archival |
| `KAWARIMI_PASS` | Unset | Migration archive passphrase supplied directly |
| `KAWARIMI_PASS_FILE` | Unset | File from which migration scripts read the passphrase |
| `--no-weechat-bootstrap` | Off | Skips one-shot relay configuration while still rendering services and minimal env |

## Testing and Verification

The following results are recorded in the implementation and review documents.
They were not rerun solely for this documentation change.

### Local Engine V2 compatibility checkpoint

- `engine_v2_interrupt_ingress`: 6 passed.
- `engine_v2_channel_delivery`: 2 passed.
- `engine_v2_mcp_compatibility`: 3 passed.
- `engine_v2_wasm_tool`: 3 passed.
- `engine_v2_skill_selection`: 5 passed.
- `engine_v2_tensorzero_streaming`: 5 passed.
- Rig adapter: 46 passed.
- Bridge router: 40 passed at that checkpoint.
- Browser Engine V2 scenarios: 4 passed using a release binary.
- Default, PostgreSQL-only, libSQL-only, and all-feature compile checks passed.
- Engine, effect-adapter, gate, WASM wrapper, XMPP, DarkIRC, and WeeChat focused
  suites passed at the recorded checkpoint.
- Default and all-feature Clippy passed with warnings denied.

### Channel-parity checkpoint

- Real `engine_v2_irc_wasm`: 8 passed.
- WeeChat adapter: 56 passed.
- DarkIRC adapter: 28 passed.
- Owner-routing filter: 42 passed.
- WASM channel setup: 6 passed.
- Core message tool: 25 passed.
- Engine transient/orchestrator attachment path: 4 passed.
- Attachment helper, provider adapter, and host-limit tests: 15 passed.
- Engine V2 attachment/control delivery matrix: 2 passed.
- XMPP, DarkIRC, and WeeChat components rebuilt successfully for
  `wasm32-wasip2`.

## Known Issues

This list is reconciled with the active [bug tracker](docs/bugs/README.md), the
channel live-validation record, current source, and current operator docs.

### Active Canonical Bug Reports

| Area | Status | Current issue |
|---|---|---|
| [Agent and worker lifecycle](docs/bugs/BUG-agent-worker-lifecycle.md) | Partial | `create_job(wait=true)` still occupies the originating turn; ordinary messages queue behind it even though `/interrupt` and `/stop` remain responsive |
| [E2E test bugs](docs/bugs/BUG-e2e-test-bugs.md) | Partial / unverified | Bootstrap and tool timeout cases are fixed; the synthetic clipboard browser scenario remains unverified in the current environment |
| [External worker config persistence](docs/bugs/BUG-external-worker-config-persistence.md) | Partial | Full TOML rewrites can omit worker bearer tokens even though provisioning and in-memory merge behavior are fixed |
| [Nanocode image distribution](docs/bugs/BUG-mt-nanocode-image-size.md) | Open | Large worker images are copied into every rootless tenant store with `save | load`, consuming time and disk |
| [Kawarimi import flag parity](docs/bugs/BUG-kawarimi-import-flag-parity.md) | Open | Import always builds WASM but rejects an explicit operator-supplied `--with-wasm` flag |
| [SSH Git ref and remote HEAD](docs/bugs/BUG-ssh-git-ref-and-remote-head.md) | Partial | Null-like refs are fixed; a bare remote with a mismatched HEAD can still produce an empty checkout reported as successful |
| [WeeChat `rand_check`](docs/bugs/BUG-weechat-relay-rand-check.md) | Partial | The unused helper always returns false; no current production call site exists |
| [Worker workspace paths](docs/bugs/BUG-worker-workspace-path-expansion.md) | Partial | Structured OpenCode/Nanocode paths normalize `~`; prompt-generated paths and Pebble remain open |
| [XMPP polling/backpressure](docs/bugs/BUG-xmpp-polling-and-backpressure.md) | Partial | Poll supervision is present, but a full downstream queue can leave delivery awaiting capacity without triggering supervisor recovery |
| [XMPP OMEMO fallback/processing](docs/bugs/BUG-xmpp-omemo-warmup-and-processing.md) | Unverified | Generic stuck-processing recovery is fixed; historical encrypted-MUC fallback spam has not been reproduced or closed |

### Release-Specific and Operational Limitations

- **WASM channels remain final-response-only.** Gateway users receive token
  deltas, but the current channel WIT has no message-edit contract and ignores
  `StreamChunk` updates.
- **Engine images are transient.** Descriptions and extracted text persist;
  image bytes must be resent after process restart.
- **Skill feedback is not transactionally exactly-once across a crash.** Metric
  updates and the thread checkpoint are separate store writes. A crash between
  them can replay or lose an observation.
- **Skill patches are prompt-only.** Registry updates replace code snippets, but
  B-1 patch proposals do not patch snippet bodies.
- **The skill gate is combined.** There is no supported switch for feedback-only
  collection without also enabling automatic demotion and maintenance hooks.
- **Kawarimi 7z passwords enter the 7z process arguments.** They can be visible
  to same-host process inspection during archive creation or extraction.
- **Machine migration remains PostgreSQL-only.** Rootless export specifically
  supports Podman, not rootless Docker, and migration is a downtime cutover.
- **Worker selection is one-directional.** Re-running `add-tenant` can enable a
  persisted worker; there is no matching disable verb or automatic teardown.
- **XMPP inbound media URLs lack an SSRF guard.** Sender-supplied OOB and
  `aesgcm://` downloads require deployment-level outbound network controls.
- **DarkIRC secure contact-key exchange is not implemented.** Contact keys are
  manual, and current config regeneration can replace manually managed contact
  sections. See the [open design](docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md).
- **WeeChat long-poll callbacks still inherit the host 30-second minimum poll
  schedule.** An early long-poll return can be followed by an idle gap.
- **WeeChat buffer restoration after host reboot remains deferred.** Operators
  should verify expected buffers after service restart.
- **Stale WeeChat DB setup fields can shadow environment and capability
  defaults.** Inspect `extensions.weechat.setup_fields` when config edits appear
  ineffective.
