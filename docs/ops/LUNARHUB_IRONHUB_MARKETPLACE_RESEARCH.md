# LunarHub / IronHub Extension Marketplace Research

> **Status (2026-08-09): RESEARCH COMPLETE; IMPLEMENTATION DEFERRED.**
> LunarWing has useful registry, lifecycle, CAS, sandbox, and local provenance
> pieces, but it does not yet have packaged daemon releases, published extension
> artifacts, a signed remote catalog, or a transactional marketplace lifecycle.
> A public marketplace should not ship before those foundations. Realistic target:
> v2.0.8.0 or later, not v2.0.7.0.

- **Goal:** Item 9 in the v2.0.6.0 cycle; the checklist was not preserved, so see
  [`RELEASE-v2.0.6.0.md`](../releases/RELEASE-v2.0.6.0.md) for the shipped record.
- **Earlier analysis:** [`IRONCLAW_IDEAS_TAKEAWAYS.md`](IRONCLAW_IDEAS_TAKEAWAYS.md)
- **LunarWing source inspected:** commit `9cb27d4b31a1acae4938671eb98a0b529e48e7cb`
- **IronClaw source inspected:**
  [`0b5fcde996910137358fc1dfd25bce39ef98df91`](https://github.com/nearai/ironclaw/tree/0b5fcde996910137358fc1dfd25bce39ef98df91),
  committed 2026-08-08
- **IronHub source inspected:**
  [`daa470764fc3749f8d6d6370e3417a2f2b2cbe3b`](https://github.com/nearai/ironhub/tree/daa470764fc3749f8d6d6370e3417a2f2b2cbe3b),
  committed 2026-08-05

## Executive Decision

Build LunarHub as four separate layers, in this order:

1. **Release plane:** reproducible, packaged LunarWing daemon releases and
   immutable release metadata.
2. **Artifact plane:** content-addressed extension bundles with complete package
   digests, checksums, SBOMs, and retained prior versions.
3. **Trust/catalog plane:** signed catalog, version, revocation, publisher, and
   moderation records hosted on LunarWing-controlled infrastructure.
4. **Lifecycle plane:** staged install, explicit grants/auth, activation, update,
   rollback, soft removal, and restart repair using durable CAS state.

Do not begin with a marketplace UI or deep links. Those are delivery surfaces,
not trust or lifecycle foundations. CLI and REPL must remain first-class; WebUI
should call the same typed operations.

Recommended initial package scope:

- Native WASM tools and channels.
- Markdown skills with bounded auxiliary files.
- Declarative remote MCP definitions after discovery bounds land.
- No automatic npm/PyPI package execution in first marketplace release.
- No generic native executable packages until signatures, dependency locks,
  sandbox defaults, revocation, and rollback are complete.

## Corrections to the Earlier Analysis

The direction in `IRONCLAW_IDEAS_TAKEAWAYS.md` is useful, but source review requires four
corrections.

### 1. IronClaw owns lifecycle; IronHub owns catalog delivery

CAS-protected installation mutations, embedded hash-pinned manifests,
`removed_at` tombstones, mutation leases, and restart repair are implemented in
IronClaw, principally in
[`installations.rs`](https://github.com/nearai/ironclaw/blob/0b5fcde996910137358fc1dfd25bce39ef98df91/crates/extensions/ironclaw_extension_registry/src/installations.rs).
They are not IronHub registry features.

IronHub builds and serves catalog documents and artifacts. Its inspected source
does not implement extension installation CAS, lifecycle tombstones, restart
repair, client rollback, or a durable revocation feed.

### 2. "OpenAPI-derived" does not mean OpenAPI import

IronClaw does not fetch or parse an OpenAPI document for custom MCP registration.
It initializes an MCP server and consumes paginated `tools/list` responses. Some
servers generate large JSON Schemas from OpenAPI, which explains the wording in
the changelog. Calling this "OpenAPI import" would overstate the feature.

### 3. Deep-link installation is not proven end to end at the pinned commits

IronHub emits a browser redirect shaped like:

```text
<agent-url>/#/install/<slug>?slug=...&version=...&nonce=...&sig=...&artifact_digest=...
```

IronClaw exposes an authenticated backend endpoint at
`POST /api/webchat/v2/ironhub/install`, but its inspected frontend has no
IronHub hash-route or install-link handler. Current source therefore does not
show the browser handoff from IronHub's redirect into that authenticated POST.

Tool digest framing also differs:

- IronHub deep-link digest:
  `SHA256(wasm_sha256 + ":" + capabilities_sha256)`.
- IronClaw expected tool digest:
  `SHA256("wasm:" + wasm_sha256 + NUL + "capabilities:" +
  capabilities_sha256 + NUL + optional manifest/schema/prompt path material)`.

These values cannot match for tools, even when no optional assets exist. IronHub
also omits manifest, schema, and prompt artifacts from its deep-link digest.
IronHub's signed version index does cover those files with a different digest,
but that safer digest is not used by the install intent. Skill deep links bind
only `SKILL.md`; IronClaw's current IronHub skill model does not carry IronHub's
auxiliary skill files.

Conclusion: reuse the concept, not the current cross-repository wire contract.
LunarHub needs one versioned, shared digest specification with cross-language
test vectors.

### 4. Provenance labels are not publisher proof

IronHub's `official`, `trusted`, `verified`, `private`, and `new` values are
catalog classifications. The catalog signature proves that IronHub emitted the
classification; it does not prove publisher identity, reproducible build output,
or independent review. LunarHub must keep these concepts separate:

- Catalog operator signature.
- Publisher signature and key identity.
- Source/build attestation.
- Moderator/reviewer decision.
- Runtime trust/grant decision.

## What IronHub Actually Implements

IronHub is a contribution repository, release packager, catalog web service, and
agent-installation intent issuer. It is a useful artifact/catalog reference, not
a complete marketplace control plane.

### Artifact and catalog shape

The signed unified catalog contains:

```text
HubManifest
  version
  generated_at
  release_tag
  repo
  tools[]
    name, crate_name, version, description, provenance
    wasm { url, size_bytes, sha256 }
    capabilities { url, size_bytes, sha256 }
    manifest? { url, size_bytes, sha256 }
    schemas? { path -> artifact }
    prompts? { path -> artifact }
  skills[]
    name, trunk, version, description, provenance
    skill_md { url, size_bytes, sha256 }
    files?[] { path, url, size_bytes, sha256 }
```

Relevant source:

- [`manifest-types.ts`](https://github.com/nearai/ironhub/blob/daa470764fc3749f8d6d6370e3417a2f2b2cbe3b/web/lib/catalog/manifest-types.ts)
- [`generate-manifest.sh`](https://github.com/nearai/ironhub/blob/daa470764fc3749f8d6d6370e3417a2f2b2cbe3b/scripts/generate-manifest.sh)
- [`generate-extension-manifest.py`](https://github.com/nearai/ironhub/blob/daa470764fc3749f8d6d6370e3417a2f2b2cbe3b/scripts/generate-extension-manifest.py)

The web service signs exact serialized document bytes with Ed25519 and returns:

```text
SignedManifestEnvelope
  v: 1
  key_id
  manifest_b64
  sig
```

Signing exact carried bytes avoids JSON canonicalization disagreement. Key IDs
permit multiple verifier keys, but inspected source has one fixed signing key ID
and no signed key-rotation or key-revocation document.

Relevant source:

- [`manifest-signing.server.ts`](https://github.com/nearai/ironhub/blob/daa470764fc3749f8d6d6370e3417a2f2b2cbe3b/web/lib/catalog/manifest-signing.server.ts)
- `GET /api/catalog/manifest.json`
- `GET /api/catalog/versions.json?since=<release-tag>`

The signed version index computes a full entry digest over artifact paths and
hashes. It can return `unchanged: true` when the caller already has the current
release tag. It does not provide version history, yanks, revocations, channels,
dependency resolution, compatibility ranges, or rollback metadata.

### Publication flow

Official content uses a repository workflow:

1. Issue proposal and maintainer triage.
2. Tool/skill implementation in a pull request.
3. CI validation and maintainer review.
4. Merge to `main`.
5. Automatic GitHub Release containing WASM, capabilities, manifests, schemas,
   prompts, skills, and `tools.json`.

This is easy to audit, but it is not a durable moderation state machine. No
machine-readable quarantine, appeal, malware-review evidence, publisher-key
enrollment, or revocation workflow exists.

The release workflow uses unpinned Rust `stable`, invokes `cargo build --release`
without `--locked`, and publishes on every `main` push independently of the CI
workflow. Artifact hashes provide integrity after build, not reproducibility.

Relevant source:

- [`CONTRIBUTING.md`](https://github.com/nearai/ironhub/blob/daa470764fc3749f8d6d6370e3417a2f2b2cbe3b/CONTRIBUTING.md)
- [`.github/workflows/release.yml`](https://github.com/nearai/ironhub/blob/daa470764fc3749f8d6d6370e3417a2f2b2cbe3b/.github/workflows/release.yml)

### Deep-link intent

IronHub stores an encrypted per-agent shared key, issues a five-minute HMAC-signed
intent, and redacts nonce/signature/token data from its audit URL. Agent URL
validation requires HTTPS and public resolved addresses, providing useful SSRF and
DNS-rebinding defense.

Useful patterns:

- Length-prefixed signature fields.
- Short expiry.
- Random nonce.
- Encrypted shared key.
- Version and digest pins.
- Redacted audit representation.

Missing or incomplete patterns:

- Install intents remain `issued`; no consumption callback updates IronHub state.
- Tool intent digest omits part of the package graph and disagrees with IronClaw.
- No complete browser-to-agent flow exists in inspected IronClaw frontend.
- Private manifest credentials travel in URL material rather than an opaque
  one-time handle or authorization header.

## What IronClaw Implements

IronClaw contains stronger host-side security and lifecycle patterns than IronHub.

### Signed catalog admission and artifact verification

IronClaw verifies the signed envelope with an embedded Ed25519 key table, then
checks version/digest pins, HTTPS/origin policy, public/private address policy,
response size, declared artifact size, and SHA-256 for each artifact. It assembles
a package only after manifest and referenced asset paths validate.

Useful LunarHub patterns:

- Separate public and private catalog-origin policies.
- Exact origin matching for private manifests and artifacts.
- Per-artifact size plus digest checks.
- Aggregate digest that binds paths as well as bytes.
- Community content requiring explicit acknowledgement.

Do not copy provenance tiers as publisher proof, IronHub-specific keys/hosts, or
private bearer URLs.

### CAS lifecycle, tombstones, and repair

IronClaw's installation store separates:

- Installation records containing embedded, hash-carrying manifests.
- User membership records.
- Credential binding records.

Multi-row mutation uses a CAS-protected lease. Leased aggregates are hidden from
typed reads, soft removal uses `removed_at`, and `removal_cleanup_pending` keeps the
embedded definition authoritative until cleanup converges. Startup repairs orphaned
leases, removed child rows, and compatibility projections.

This is strong design evidence for LunarWing. Copy semantics, not filesystem
storage. LunarWing should place authoritative installation state behind its shared
DB trait with PostgreSQL and libSQL implementations.

### Hosted MCP registration and bounded discovery

IronClaw's WebUI can register a custom HTTPS MCP endpoint without accepting secrets
in the registration body. Server-side canonicalization repeats browser validation.
Registration creates a durable definition; install/auth/activation remain separate
lifecycle steps.

Discovery uses MCP `initialize` and paginated `tools/list`, with these inspected
host bounds:

| Bound | IronClaw value |
| --- | ---: |
| Host maximum tools | 1,024 |
| Maximum pages | 50 |
| Aggregate catalog JSON | 16 MiB |
| Tool name | 128 bytes |
| Tool description | 2,048 bytes |
| Schema depth | 32 |
| Schema nodes per tool | 8,192 |
| Schema string/key | 16 KiB |

Other useful controls:

- Strict tool-name grammar.
- Atomic publication: unsafe generation does not replace old generation.
- Prompt-injection scanning across names, descriptions, and schema text.
- Discovered tools default to approval-required.
- Authority fence rechecks package, manifest hash, tool budget, and credentials
  after network discovery but before publication.
- Refresh failure retains previous valid generation.

One poor default should not be copied: user-registered hosted MCP manifests hardcode
`max_tools = 1024`. LunarWing should require a smaller operator/user-selected budget
with a conservative default.

### Deep-link cautions

IronClaw durably consumes install nonces with CAS and scopes nonce storage to the
authenticated caller. It installs into the authenticated caller's resource scope,
not the signed `uid`/`aid` fields. Current tests explicitly permit those signed
fields to differ from the caller.

LunarHub should instead define field meaning unambiguously and bind any signed
target identity to the authenticated tenant/user/agent, or omit redundant signed
identity fields. A deep link should open a review page; it must not directly grant
capabilities or activate code.

## LunarWing Current State

### Readiness matrix

| Requirement | Current state | Evidence / gap |
| --- | --- | --- |
| Daemon binary distribution | **Design metadata only** | `ic/Cargo.toml` declares cargo-dist targets/installers, but targets GitHub CI; no GitHub release workflow exists. GitLab docs state package publication/deployment are absent. |
| Worker image distribution | **Partial** | GitLab container registry and manual worker image jobs exist; no daemon image/package job. |
| Extension artifact distribution | **Scaffold only** | 13 tool/channel manifests exist, but every `wasm32-wasip2` URL and SHA-256 is null. GitLab builds only short-lived test WASM artifacts. |
| Local manifest catalog | **Implemented** | `RegistryCatalog`, embedded catalog, bundle definitions, search/resolve, setup and CLI installer. No remote snapshot/version/revocation protocol. |
| Verified WASM installer | **Implemented on one path** | `RegistryInstaller` requires HTTPS, allowed hosts, and SHA-256. |
| Unified verified install | **Not implemented** | Manifest-to-`RegistryEntry` conversion drops SHA-256. Web/chat `ExtensionManager` download verifies size/magic only and accepts arbitrary HTTPS hosts. |
| WASM atomic update/rollback | **Not implemented** | `upgrade_one` deletes current WASM/capabilities before reinstall and reports old files removed on failure. |
| Generic extension CAS/tombstones | **Not implemented** | No durable generic installation aggregate, mutation revision, previous generation, or soft-removal record. |
| MCP config CAS | **Implemented** | DB mutations use `compare_and_set_setting` with 16 bounded retries; both PostgreSQL and libSQL implement setting CAS. |
| MCP lifecycle/restart repair | **Implemented for host-local stdio runtime** | Install/configure, auth, activate, deactivate, update, remove, supervised restart, circuit breaker, generation-safe tool replacement. Package install repair remains absent. |
| MCP provenance | **Partial** | Operator-pinned executable identity/digest and local `unverified`/`pinned`/`revoked` state. No publisher signatures, trust roots, dependency lock, or distributed revocation. |
| MCP host sandbox | **Partial** | Opt-in Unix/Linux stdio sandbox landed. Mandatory profiles and remaining device/process exposure are deferred. |
| Bounded dynamic MCP discovery | **Not implemented** | One unpaginated `tools/list`; no manifest tool budget, count cap, aggregate schema cap, depth/node bounds, or catalog-generation fence. |
| Skills marketplace | **Partial/external** | ClawHub search/install/publish/update pieces exist, but lifecycle and provenance differ by web/chat surface and do not form LunarHub. |
| Deep-link install | **Absent** | No signed marketplace intent or custom protocol route. Existing web install endpoints are direct authenticated operations. |
| Publisher/moderator workflow | **Absent** | No publisher keys, review state, quarantine, revocation, appeal, or signed moderation evidence. |

### Immediate correctness blockers

These should be fixed before any remote marketplace work:

1. **Unify installer integrity.** Preserve expected digest and origin policy from
   manifest through `RegistryEntry` into `ExtensionManager`. CLI, setup, web, and
   conversational install must call one verified installer.
2. **Make WASM install/update atomic.** Stage and verify complete bundle, fsync it,
   then atomically switch generation. Never delete current files first.
3. **Populate immutable artifact coordinates.** Build/publish versioned bundles and
   replace null URL/hash fields through release automation.
4. **Bound MCP discovery.** Add tool/page/byte/schema/time limits before admitting
   arbitrary hosted MCP definitions.
5. **Normalize lifecycle state.** Files alone cannot represent version, digest,
   desired/active state, prior generation, tombstone, repair status, or grants.

## Proposed LunarHub Architecture

### 1. Release and artifact storage

Use GitLab protected-tag pipelines and LunarWing-controlled storage:

- GitLab Generic Package Registry for versioned daemon archives and extension
  bundles.
- GitLab Container Registry for daemon/worker OCI images where appropriate.
- GitLab Release links as human-facing release index, not artifact authority.
- Content-addressed host cache keyed by complete package digest.
- Immutable version paths; never overwrite bytes behind an existing version.

Daemon release jobs should produce:

- Supported target archive or OCI image.
- SHA-256 checksum document.
- SBOM.
- Build provenance/attestation containing source commit, locked dependencies,
  Rust toolchain, target, and builder identity.
- Signature over exact release metadata.

For multi-tenant hosts, mt-admin remains sole lifecycle owner. One host-global,
root-owned verified cache may serve many tenants, but each tenant needs independent
desired state, auth, grants, active generation, health, and rollback state. Both
systemd-user and OpenRC paths must consume identical verified artifacts.

### 2. Package lock and digest

Define one canonical package-lock document before service or UI work. Digest must
bind:

- Package ID, version, kind, runtime, protocol, ABI/WIT compatibility.
- Every artifact path, byte size, and SHA-256.
- Capability/policy manifest and schema paths.
- Publisher key ID and signature.
- Source/build provenance references.

Use explicit domain separation and length-prefix every field. Publish cross-language
fixtures for Rust, shell/Python release tooling, and any future web service. Do not
invent separate "version" and "deep-link" digest algorithms.

### 3. Signed catalog documents

At minimum publish separately signed documents:

```text
catalog snapshot
  epoch, generated_at, previous_snapshot_digest
  package/version -> package_lock_digest, state, compatibility

publisher keys
  publisher_id, key_id, public_key, valid_from, valid_until, status

revocations
  sequence, package/version/digest or publisher key, reason, effective_at

version index
  package -> available versions, channels, yanks, minimum LunarWing version
```

Client must fail closed on invalid signature, unknown key, rollback epoch, expired
metadata, digest mismatch, or revoked package/key. Define bounded stale-cache policy
for temporary registry failure without permitting catalog rollback.

### 4. Durable lifecycle model

Add operations to shared DB trait first, then PostgreSQL and libSQL backends.
Conceptual records:

```text
extension_installations
  owner_scope, installation_id, package_id, current_digest
  previous_digest, desired_state, active_generation
  revision, installed_at, removed_at, cleanup_pending

extension_grants
  installation_id, capability, decision, constraints, revision

extension_credentials
  installation_id, secret_handle, status, removed_at, revision

extension_mutations
  operation_id, expected_revision, phase, staged_digest, error
```

Required semantics:

- Expected-revision CAS on every mutation.
- Install/configure/auth/activate remain distinct states.
- Staged artifact validation before authoritative mutation.
- Atomic generation switch.
- Previous generation retained until health gate passes.
- Soft removal first; cleanup is retryable and restart-repaired.
- Revocation blocks install, activation, restart repair, and rollback to revoked
  content.
- Compatibility views may be repaired; authoritative record must never be inferred
  from partially written files.

### 5. Review and moderation

Start with GitLab issue/MR review for official first-party packages. Before third-
party publication, add:

- Publisher key enrollment and rotation.
- Submission state machine: draft, validating, review, approved, rejected,
  quarantined, published, yanked, revoked.
- Automated manifest/schema validation, secret scan, malware scan, license/SBOM
  checks, and capability diff.
- Reviewer identity and signed decision record.
- Audit log, report, appeal, and emergency revocation process.
- Namespace reservation and anti-typosquatting policy.

Registry signature means "LunarHub served this record." It must not silently mean
"code is safe."

### 6. Install and deep-link UX

CLI-first flow:

```text
lunarwing hub search <query>
lunarwing hub info <package>@<version>
lunarwing hub install <package>@<version> --tenant <tenant>
lunarwing hub grant <installation> ...
lunarwing hub activate <installation>
lunarwing hub update <installation> --to <version>
lunarwing hub rollback <installation>
lunarwing hub remove <installation>
```

WebUI should call same typed service. A future deep link should carry only a signed,
short-lived install intent and open an authenticated review screen showing:

- Target tenant/user/agent.
- Publisher and trust evidence.
- Exact version and package digest.
- Runtime/placement.
- Capability and credential requests.
- New capability diff for upgrades.
- Rollback availability.

Consumption must be durable and idempotent. Signed target fields must match the
authenticated target. Confirmation creates a staged install; it does not imply
activation or capability grants.

## Ordered Roadmap and Gates

### Phase 0 - Repair current foundations

- Route all registry installs through one checksum/origin-verifying path.
- Make WASM install/update generation-atomic with rollback.
- Add MCP discovery budgets and pagination.
- Run semantic Rust registry validation in CI, not JSON parsing alone.

**Exit gate:** Every install surface verifies identical package bytes and failed
update preserves active generation.

### Phase 1 - Binary and artifact distribution

- Protected-tag GitLab release jobs for supported daemon targets.
- Generic Package and OCI publication with checksum, SBOM, attestation, signature.
- Versioned WASM bundles; populate catalog URL/size/hash fields.
- mt-admin verified artifact activation for systemd-user and OpenRC.

**Exit gate:** Fresh and upgraded tenants can install one verified CI-built daemon
artifact without local source build, with rollback tested on both init systems.

### Phase 2 - Local signed catalog and lifecycle

- Freeze package-lock and signed document schemas.
- Add DB-backed installation/grant/credential/mutation records for both backends.
- Add CAS leases, tombstones, restart repair, retained prior generation.
- Keep catalog file/Git-backed and official-only initially.

**Exit gate:** Crash tests at every mutation phase converge to old or new complete
generation; no torn install becomes visible.

### Phase 3 - Hosted LunarHub

- Host signed snapshots/version/revocation/key documents on
  `git.lunarwing.org` infrastructure.
- Add bounded client cache and anti-rollback state.
- Add publisher/reviewer workflow and quarantine.
- Add CLI search/info/install/update/remove against hosted catalog.

**Exit gate:** Registry compromise without signing keys cannot replace package
bytes; publisher/key revocation blocks future activation and repair.

### Phase 4 - Custom MCP and deep links

- Add canonical HTTPS MCP endpoint admission and SSRF/DNS-rebinding defense.
- Add bounded paginated discovery with authority fence and atomic generation.
- Add user-selected tool budget below host ceiling.
- Add signed install intents and WebUI review handoff.

**Exit gate:** End-to-end browser and CLI tests use real signed intents, reject
replay/cross-caller delivery, bind complete package digest, and never grant or
activate without explicit review.

## What to Reuse and What to Reject

### Reuse from IronHub

- Exact-byte signed envelope with key ID.
- Per-artifact URL, byte size, and SHA-256.
- Full path-aware version digest concept.
- Repository issue -> MR -> CI -> publish workflow for official packages.
- Length-prefixed HMAC intent fields, short expiry, nonce, audit redaction.
- DNS-vetted HTTPS agent URL validation.

### Reuse from IronClaw

- Embedded hash-pinned install manifest.
- CAS mutation lease and hidden torn aggregate.
- Soft-removal tombstone plus cleanup-pending state.
- Restart reconciliation and repairable compatibility projections.
- Origin-specific artifact policy, exact size/hash checks, bounded downloads.
- MCP page/tool/catalog/schema bounds.
- Authority fence after unlocked network discovery.
- Atomic publication retaining previous valid generation.
- Conservative default permissions and model-visible catalog safety scan.

### Reject or redesign

- IronHub/IronClaw's incompatible tool digest contracts.
- A "deep link" without a tested browser handoff.
- Provenance inferred from a catalog label.
- One hardcoded maximum tool budget for every custom MCP server.
- Private bearer material in a manifest URL.
- Mutable release assets or mutable private content under an unchanged version.
- Release builds using an unpinned toolchain or unlocked dependencies.
- Hard-delete-before-reinstall updates.
- WebUI-only lifecycle.
- Filesystem-only lifecycle records copied into LunarWing instead of DB-backed
  PostgreSQL/libSQL state.

## Immediate Next Steps

1. File one implementation proposal for **verified binary and extension artifact
   distribution**, not a broad marketplace implementation.
2. File one correctness issue for checksum loss between `ExtensionManifest` and
   `ExtensionManager`, plus destructive WASM update ordering.
3. Add MCP discovery-budget proposal/tests before custom server registration.
4. Draft canonical package-lock, catalog, key, and revocation schemas with digest
   fixtures.
5. Decide supported v1 LunarHub package kinds and explicitly exclude automatic
   npm/PyPI installation.
6. Define GitLab protected-tag, Generic Package, OCI, SBOM, signature, retention,
   and rollback policy.
7. Prototype DB installation records through shared trait with PostgreSQL/libSQL
   parity.
8. Revisit hosted LunarHub and deep links only after Phases 0-2 pass.

## Source Index

### LunarWing

- `ic/Cargo.toml` - cargo-dist metadata without active release pipeline.
- `.gitlab/README.md` - package/release/deployment jobs explicitly deferred.
- `.gitlab/ci/rust.yml` - short-lived WASM test artifacts only.
- `docs/ops/GITLAB_CONTAINER_REGISTRY.md` - worker image registry and current gaps.
- `docs/ops/RELEASE_COMMANDS.md` - manual Generic Package recipe.
- `ic/registry/` - local manifests with null artifact URLs/hashes.
- `ic/src/registry/{manifest,catalog,installer}.rs` - local catalog and verified
  installer.
- `ic/src/extensions/manager.rs` - web/chat lifecycle, unverified URL downloader,
  destructive update path, MCP runtime recovery.
- `ic/src/tools/mcp/{config,provenance,factory,client,sandbox}.rs` - config CAS,
  operator pinning/revocation, discovery, and host sandbox.
- `docs/ops/MCP_ADDITIONS.md` - authoritative landed/deferred MCP status.
- `docs/proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md` - runtime-neutral
  package direction.

### IronClaw

- `crates/extensions/ironclaw_extension_manager/src/ironhub/` - signed catalog,
  install intent, artifact assembly, and digest contract.
- `crates/extensions/ironclaw_extension_registry/src/installations.rs` - CAS
  lifecycle, tombstones, leases, and repair.
- `crates/extensions/ironclaw_extension_host/src/hosted_mcp_*` - custom MCP
  registration, authority fence, preparation, and manifest synthesis.
- `crates/extensions/ironclaw_extension_host/src/mcp_discovery.rs` - discovery
  admission and safety scan.
- `crates/lanes/ironclaw_mcp/src/{client,discovery}.rs` - paginated bounded
  `tools/list` parsing.
- `crates/product/ironclaw_webui/src/webui_v2/` and
  `crates/product/ironclaw_webui/frontend/` - backend routes and frontend surface.

### IronHub

- `web/lib/catalog/` - unified manifest, signed envelope, version index.
- `web/lib/agent-installations/` - shared-key registration, HMAC intent, redirect,
  and incomplete tool digest.
- `web/app/api/catalog/` - public signed catalog and artifact routes.
- `web/app/api/private-artifacts/` - organization/private artifact service.
- `scripts/generate-manifest.sh` and `scripts/generate-extension-manifest.py` -
  release packaging and extension manifest generation.
- `.github/workflows/{ci,release}.yml` - validation and publication workflow.
- `CONTRIBUTING.md` - issue/MR/review contribution lifecycle.
