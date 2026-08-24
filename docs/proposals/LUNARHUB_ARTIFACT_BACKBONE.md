# LunarHub Fleet Artifact Backbone

> **Status (2026-08-24): PROPOSED — open for review.**
> Originates from the LunarHub/IronHub marketplace research
> ([`../ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md`](../ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md),
> status: RESEARCH COMPLETE; IMPLEMENTATION DEFERRED) and an IRC design
> discussion (rarity, 2026-08-24) reframing LunarHub short-term as a
> **fleet-private artifact backbone** rather than a public marketplace. This
> document scopes Phase 0–1 of that research (release + artifact planes) for
> the fleet and defers marketplace Phases 2–4 to the roadmap rows already
> scheduled at v2.1.4.0 / v2.1.5.0 (see `../ops/ROADMAP_2026.md`).
> All code references below were verified against `lunarhub3/v2.0.9.0` on
> 2026-08-24.

## Overview

A fresh multi-tenant provision today compiles the daemon from source on the
target host: on slow hardware (N150) that is a ~3-hour wait. The root cause
is not compile slowness — it is that **there are no prebuilt, verifiable
artifacts to pull**. LunarHub, in its first useful form, is the fleet's
private release backbone: authorized builder boxes publish signed, attested
artifacts; slow boxes pull-verify-activate instead of compiling. The fast
boxes become foundries; the N150 becomes a client, not a foundry.

This is deliberately **not** the public extension marketplace. The research
doc is explicit that a marketplace must not ship before foundations exist:
packaged daemon releases, published extension artifacts, a signed catalog,
and a transactional lifecycle. The fleet backbone *is* the release plane and
artifact plane of that four-layer model, built and proven on our own fleet
before any third-party publication is contemplated.

Naming used here:

- **Backbone** — the fleet-private artifact distribution (this document).
- **Marketplace** — the public LunarHub extension marketplace (the research
  doc; deferred; not covered here except as exit criteria for revisiting it).

## Goals and non-goals

Goals:

1. Every fresh or upgraded tenant can install one **verified, CI/builder-built
   daemon artifact without a local source build**, on both systemd-user and
   OpenRC hosts.
2. One canonical **manifest/package-lock digest format**, versioned, with
   cross-language fixtures (Rust, shell, Python), binding every artifact
   byte, provenance, and builder signature.
3. Builder authority expressed as **key possession**, with signed manifests,
   attestations, and a signed last-known-good (LKG) pointer per artifact kind.
4. Host-global, content-addressed, verified artifact cache shared by tenants
   while mt-admin remains **sole lifecycle owner** per AGENTS.md.
5. Pull-first build routines: resolve manifest → verify → activate; local
   compile only on genuine miss.

Non-goals (explicitly, for this stage):

- Public third-party package publication, moderation, quarantine, publisher
  workflows (Marketplace, deferred).
- Deep links, web UI install flows, `lunarwing hub …` CLI surface.
- npm/PyPI package execution or generic native executable packages
  (research doc §1 exclusions still stand).
- Changes to WASM extension registry lifecycle beyond what verified
  distribution requires.

## Artifact taxonomy

One manifest format; kinds differ only in **pointer semantics** (channel
policy over immutable content), not in schema.

| Kind | Versioning | Pointer | Consumer | Lifecycle class |
|---|---|---|---|---|
| `daemon-release` | semver, immutable | none (pinned by upgrade request) | mt-admin | version-pinned |
| `worker-image` (pebble / opencode / kimi) | content digest | floating signed tag `last-known-good` | paseo / orchestrator | tag-floated |
| `wasm-ext-set` | content digest | per-tenant config pins digest | tenant daemon | per-tenant-config |
| `fixture-bundle` | date/commit stamp | none | CI / tests | ephemeral |
| `study-mcp-checkout` | source-commit keyed | disposable cache | note pipeline | cache-derived |

Open taxonomy questions are listed in the Open Questions section (A-group).

## Design

### 1. Storage and layout (no new service)

Use the existing GitLab (`git.lunarwing.org`) per the research doc §1:
protected-tag pipelines, GitLab Generic Package Registry for versioned
archives/bundles, Container Registry for worker OCI images. Immutable version
paths; never overwrite bytes behind an existing version.

```text
hub/
  daemon/lunarwing/<semver>/<target-triple>/
    lunarwing.tar.zst  SHA256SUMS  sbom.spdx.json
    attestation.json   manifest.json  manifest.json.sig
  workers/<image>/<tag>/…  +  channels/last-known-good.json{,.sig}
  wasm-sets/<name>/<digest>/…
  fixtures/<name>/<stamp>/…
  publishers.json            # authorized builder keys (signed doc)
  revocation.json{,.sig}     # monotonic sequence, per research doc §3
```

### 2. Manifest and digest

Adopt the research doc §2 package-lock digest: bind package ID, version,
kind, runtime/protocol, every artifact path+size+SHA-256, capability manifest
paths, builder key ID + signature, and build provenance (source commit,
locked dependencies, toolchain, target triple, feature baseline, builder
identity). Explicit domain separation and length-prefixed fields; publish
cross-language fixtures. One digest algorithm — no separate "deep-link" or
ad-hoc variants.

### 3. Trust

Reuse the MCP marketplace trust stack already in-repo:

- `ic/src/tools/mcp/marketplace.rs` — ed25519 `verify_strict` publisher
  signatures over canonical manifests, `PublisherTrustStore`, signed
  lockfiles, `ManifestDiff`, signed revocation lists with monotonic sequence
  + `RevocationStore` (fail-closed, rollback/refusal semantics).
- `ic/src/tools/mcp/config.rs:1229-1327` — bounded, fail-closed trust store
  loading; trust config gating described in `README.md` (marketplace section
  of `mcp-servers.json` fails closed at config load and activation once
  populated).
- `channels::wasm::signature` — existing ed25519 signature verification for
  WASM channels.
- `docs/ops/v2.0.8.0_ITEM_8_MCP_TENANT_TEST.md` — Python `gen_fixtures.py`
  generator, precedent for cross-language signing fixtures.

Requirements specific to the backbone:

- **Domain separation distinct** from MCP packages and from WASM channel
  signatures; same JSON trust-store format, separate namespaces.
- Pull-time verification must be **fail-closed**: bad signature, unknown key,
  digest mismatch, revoked artifact/key, or epoch rollback all abort.
  "LunarHub signed this" means "an authorized builder produced exactly these
  bytes" — never "code is safe" (research doc §5).
- LKG pointers are themselves signed documents with monotonic epochs;
  unsigned or stale-pointer activation is prohibited.

### 4. Who builds / pull vs build

- Authorized builders (initially: AJ's box; the tablet when healthy; CI
  protected-tag jobs as they mature) publish. Slow boxes consume.
- Build routine contract: check hub → if manifest+digest valid, pull; else
  compile locally; authorized builders *also* publish with attestation after
  a successful local build.
- Pull eligibility requires exact manifest match including **target triple,
  toolchain, and feature baseline** (e.g. x86-64-v2) so the fast box's
  artifacts are only served to the N150 if the binary actually runs there.
- Host-global content-addressed cache (research doc §1): root-owned, one
  verified cache serving many tenants; each tenant keeps independent desired
  state, auth, grants, and rollback state via mt-admin.

### 5. Lifecycle integration

- mt-admin gains pull-verify-activate for daemon artifacts; wrappers
  (`lunarwing_mt_onboard*`, web, verify) stay thin drivers and must not
  reimplement init or verification logic (AGENTS.md hard rule).
- systemd-user and OpenRC paths consume **identical verified artifacts**;
  activation/rollback tested on both init systems (research doc Phase 1 exit
  gate).
- Previous daemon generation retained until health gate passes; restore
  semantics align with existing self-heal/watchdog behavior.
- Compose with the health pipeline (`ic-infrastructure-health-check/`,
  ports-registry >= v16) for post-activation gating; no new host-port
  hardcoding.

## Phased rollout and gates

### Phase A — Foundations (preconditions, mostly from research Phase 0)

- Unified checksum/origin-verifying install path for anything pulled.
- Manifest schema + cross-language digest fixtures frozen at v1.
- Publishers file + builder key enrollment; domain-separated signing format.

**Exit gate:** every pull surface verifies identical bytes; an unsigned or
mis-signed artifact is rejected on both a fast box and the N150.

### Phase B — Backbone live (research Phase 1, fleet-scoped)

- Builder publishes daemon release + attestation + signature to GitLab
  Generic Package Registry.
- mt-admin pull-verify-activate on systemd-user and OpenRC; previous
  generation rollback verified on both.
- Signed LKG pointer for worker images consumed by the container path.
- Provisioning-time benchmark: fresh tenant on N150 becomes
  pull-configure-start (target: minutes, current baseline ~3h).

**Exit gate:** fresh and upgraded tenants install one verified artifact
without local source build, with rollback tested on both init systems.

### Deferred (NOT this proposal)

Marketplace Phases 2–4 (local signed catalog + DB-backed CAS lifecycle,
hosted catalog, custom MCP and deep links) remain scheduled at v2.1.4.0 /
v2.1.5.0 and are governed by the research doc. Reconsider only after the
backbone demonstrates: N releases served with zero unsigned activations,
rollback drills on both inits, and a practiced revocation.

## Security model and invariants

- Do not weaken existing auth, webhook auth, CORS/origin checks, body limits,
  rate limits, allowlists, or secret handling (AGENTS.md).
- Signing keys per builder box; secured offline/TBD storage; rotation and
  revocation procedures documented before first publish.
- Fail-closed everywhere on verification failure; bounded stale-cache only
  without permitting catalog rollback (research doc §3).
- Pull credentials on consumer boxes are read-only and per-host revocable.
- Key compromise story: revoking a builder key blocks future activation and
  LKG promotion fleet-wide on next verified doc fetch.

## Open questions

These must be answered (and the answers recorded back into this document)
before Phase A is considered complete. Grouped, numbered for referral.

### A. Taxonomy and schema

1. **A1.** Final artifact kinds for v1: confirm the five kinds above —
   should `study-mcp-checkout` be a distributed artifact or purely a
   cache-derived convenience? Do fixture bundles belong in the same registry
   at all?
2. **A2.** Versioning per kind: exact scheme (semver vs digest vs date
   stamp) and the LKG channel naming/promotion convention for worker images.
3. **A3.** Manifest schema governance: who owns schema bumps, and what is
   the compatibility rule between manifest v1 consumers and a later v2?
4. **A4.** `wasm-ext-set` composition: what constitutes a "set" (tools +
   channels manifest?), and the exact per-tenant config syntax that pins a
   digest.

### B. Trust and signing

5. **B1.** Signing key model: one key per builder box vs one fleet release
   key; storage (offline file, TPM, age); rotation cadence; enrollment into
   `publishers.json` (signed by whom?).
6. **B2.** Exact domain-separation strings/versioning for backbone
   signatures, distinct from MCP package and WASM channel signatures; shared
   crate extraction vs mirroring JSON formats for non-Rust verifiers.
7. **B3.** Non-Rust verification path: mt-admin is shell — verify ed25519
   via `openssl pkeyutl`, a vendored static verifier, or Python? Must be
   identical across systemd-user and OpenRC hosts without adding heavy deps.
8. **B4.** Revocation distribution before a hosted catalog exists: cadence
   of `revocation.json` fetch, monotonic-epoch anti-rollback enforcement in
   shell/python matching `RevocationStore` semantics, and the bounded
   stale-cache policy during registry outage.
9. **B5.** Pull authentication on consumer boxes: GitLab deploy tokens per
   host, signed short-lived URLs, or anonymous read on a private registry?
   Behavior for offline/air-gapped hosts (export/import path)?

### C. Build and publish

10. **C1.** Builder roster and precedence: AJ's box + tablet-when-healthy;
    do CI protected-tag jobs supersede or coexist? Resolution when two
    builders produce the same version with different bytes (should be fatal).
11. **C2.** Attestation contents and client-side verification requirements;
    is build provenance advisory or enforced at pull time?
12. **C3.** Reproducibility: require two builders yield identical digests
    before LKG promotion? Known non-reproducible inputs (timestamps,
    proc-macro fingerprints, incremental artifacts) need an inventory.
13. **C4.** Binary portability: baseline target (x86-64-v2 vs v3), glibc
    floor, build containers to pin both; future ARM support in or out of v1?

### D. Distribution and cache

14. **D1.** Cache location, ownership (root-owned), permissions for
    per-tenant read-via-verifier, quota and GC/eviction policy.
15. **D2.** Fallback policy: precise fail-closed semantics on registry
    outage (staleness bounds); when local compile is permitted, and whether
    an operator can force-build bypassing the hub (and whether that builds
    gets *published* from an authorized box or not).
16. **D3.** mt-admin surface: new subcommand(s), state layout in
    mt-admin-lib, and how `upgrade-tenant` consumes verified artifacts;
    confirmation that ports-registry and health contracts are unaffected.

### E. Activation and lifecycle

17. **E1.** Daemon rollback semantics: previous-generation retention count,
    health-gate definition before switching generation, watchdog interplay
    (does self-heal restore the previous generation automatically?).
18. **E2.** Worker-image verify story: cosign vs our ed25519 manifest
    signature over the OCI digest; podman pull policy on consumer boxes;
    floating-tag vs pinned-digest in tenant env.
19. **E3.** Relationship to the existing WASM extension registry/ClawHub
    pieces (research doc §3 readiness matrix): is WASM set distribution
    folded into this artifact plane, or kept separate until Marketplace?

### F. Operations and transition

20. **F1.** Hub observability: registry-availability healthpoint and
    health-metrics integration; alert on unsigned-activation attempts.
21. **F2.** Storage accounting on `git.lunarwing.org`: retention policy for
    superseded versions, project quota limits, cleanup cadence.
22. **F3.** Marketplace readiness criteria: concrete measurements (releases
    served, drills) that flip the v2.1.4.0 row from "blocked on foundations"
    to "ready."
23. **F4.** Docs/AGENTS.md updates required at ship time (build-routine
    contract, mt-admin contract, ports) and version bumps for the ports
    registry if any new host-global slot appears.
24. **F5.** Naming/CLI for the backbone stage: is everything host-side via
    mt-admin, or does any `lunarwing …` daemon-side CLI appear now (research
    doc §6 defines it for Marketplace)?

## References

- [`../ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md`](../ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md) — authoritative marketplace architecture, phases, gates.
- [`../ops/ROADMAP_2026.md`](../ops/ROADMAP_2026.md) — marketplace rows (v2.1.4.0, v2.1.5.0).
- [`../ops/IRONCLAW_IDEAS_TAKEAWAYS.md`](../ops/IRONCLAW_IDEAS_TAKEAWAYS.md) — earlier analysis corrected by the research doc.
- `ic/src/tools/mcp/marketplace.rs`, `ic/src/tools/mcp/config.rs` — trust stack to reuse.
- `docs/ops/v2.0.8.0_ITEM_8_MCP_TENANT_TEST.md` — ed25519 fixture generation precedent.
- [`wasm-expansion-proposal-2.0.6.0.md`](wasm-expansion-proposal-2.0.6.0.md) — WASM expansion context citing the research doc.
