# GitLab CI/CD Plan for LunarWing

> **Current status (source-reverified 2026-08-25): PHASE 1 ACTIVE AND
> VERIFIED; PHASE 2 PARTIAL.** This document originated as items #12–13 in the
> retired v2.0.2.0 working checklist. The root pipeline and `.gitlab/`
> implementation remain active. Public GitLab evidence proves activation:
> default-branch pipeline [1093](https://git.lunarwing.org/lunarwing/lunarwing2/-/pipelines/1093)
> passed on 2026-08-03 with every required job green, all four Rust check shapes,
> libSQL tests, cargo-deny, Cavepony/WASM coverage, and all three manual worker
> image builds; merge-request pipeline
> [1118](https://git.lunarwing.org/lunarwing/lunarwing2/-/pipelines/1118) also
> passed on 2026-08-04. Manual Kaniko jobs now build the Pebble, OpenCode, and
> Kimi Code images, force HTTP/1.1 for the GitLab registry proxy, retry pushes,
> and publish both moving version/`latest` tags and an immutable short-SHA tag;
> this is image publication, not the still-missing worker smoke-test matrix.
> Full Phase 2 exit criteria remain open: PostgreSQL service tests, general
> browser tests consuming one prebuilt binary, standalone-crate coverage, and
> measured coverage are absent. Package, release, and deployment automation
> (Phases 3–4) also remain absent. Project and runner configuration guidance
> lives in `.gitlab/README.md`.

**Repository audited:** `lunarwing/lunarwing2`
**Primary project:** `ic/`
**Target platform:** self-hosted GitLab

## 1. Executive Summary

Before item #13, LunarWing had no root `.gitlab-ci.yml`. The initial pipeline
now composes local files under `.gitlab/`, runs the four Rust feature shapes,
hermetic Rust/Python/shell tests, version and panic policy, registry parsing,
cargo-deny, and current-tree secret detection. Known architecture, Clippy,
ShellCheck, and history-scan debt remains explicitly observational.

The recommended rollout has four operational layers:

1. **Merge request CI:** deterministic, hermetic, unprivileged checks with no
   secrets. This blocks regressions in Rust, Python, shell, registry metadata,
   architecture boundaries, and dependency policy.
2. **Default-branch and scheduled integration:** PostgreSQL, libSQL, browser,
   WASM, standalone crates, worker containers, and external-service tests on
   runners matched to their trust and resource needs.
3. **Release packaging:** protected release candidates produce immutable
   native packages, checksums, SBOMs, signatures, and worker image digests.
   Final releases promote the exact tested package; they do not rebuild it.
4. **Protected deployment:** the same package moves through systemd-user and
   OpenRC staging, one canary tenant, then tenant-by-tenant production rollout.
   Every tenant lifecycle operation goes through `lunarwing-mt-admin.sh`.

Production artifact deployment must not be enabled until mt-admin can verify
and atomically activate prebuilt packages. Its current `upgrade-tenant` path
checks out source and rebuilds inside each tenant, so the deployed binary is
not the artifact tested by CI.

## 2. Verified Current State

### 2.1 Existing automation

| Area | Current state | Consequence |
|---|---|---|
| Root GitLab pipeline | `.gitlab-ci.yml` composes local files under `.gitlab/ci/` | Active on push and merge-request pipelines; green default-branch and MR evidence is linked above. |
| Nested GitLab files | Generic Auto DevOps files exist under `lunarwing_weechat_wss/` and `git-lunarwing-unix-socket-client-repo/` | They do not govern the root project. |
| Mirror workflow | No `.forgejo/` or `.github/` workflow is present; the old Forgejo mirror was deleted | Mirror credentials are outside the current root CI surface. |
| Local quality gate | `ic/scripts/ci/quality_gate.sh` anchors itself to `ic/` and bounds Cargo CPU/jobs/tests | Available for local hooks; GitLab uses smaller attributed jobs. |
| Strict quality gate | `ic/scripts/ci/quality_gate_strict.sh` anchors itself to `ic/` and applies resource limits | Available locally once cargo-deny is installed. |
| Version checks | `ic/scripts/check-version-bumps.sh` accepts `BASE_BRANCH`, `BASE_SHA`, and `CHANGE_LABELS` | GitLab wrapper supplies deterministic MR/push metadata; bypass requires a second maintainer-controlled variable. |
| Boundary checks | `ic/scripts/check-boundaries.sh` passes its hard gates while reporting cleanup warnings | Still observational in CI; pipeline 1093 passed the job. |
| Dependency policy | `ic/deny.toml` plus checksum-pinned cargo-deny 0.20.2 installer | Blocking phase 1 job passes after `spin` 0.9.8 was updated to 0.9.9. |
| Coverage | `ic/scripts/coverage.sh` and `ic/codecov.yml` exist | Measure first; do not enforce unverified 80/90 thresholds on first pipeline. |
| Release flow | `docs/ops/RELEASE_COMMANDS.md` describes manual tag, API, and package upload commands | It references stale repository paths and a nonexistent earlier release job. |
| Cargo dist | `ic/Cargo.toml` configures cargo-dist for GitHub and many targets | Do not use unchanged for GitLab or promise unsupported runners. |

### 2.2 Build and test constraints

- Core workspace is rooted at `ic/Cargo.toml`, uses Rust 2024, and declares
  MSRV 1.96.1.
- Core manifests, Docker builders, root README, `ic/rust-toolchain.toml`, and CI
  images are reconciled on Rust 1.96.1. Historical release notes retain their
  original toolchain claims.
- GitLab's `rust-x86_64` runner is intentionally limited to CPUs `0-5`:
  `.gitlab/scripts/cargo-ci.sh` enforces that affinity, while individual jobs
  use `-j1`/`-j2` to stay inside the 16 GiB runner. The `taskset -c 0-5`
  examples below are therefore CI contracts, not the current local development
  limit; repository-local Cargo work uses `taskset -c 0-11` and at most `-j12`.
- Compile verification uses `cargo check`, never a full debug build.
- A recorded native release build compiled 769 crates, used about 2.8 GB of
  target data, and took about 11 minutes under the historical six-CPU limit.
  CI runners need enough RAM and disk for Wasmtime, libSQL, C/C++ dependencies,
  and test linking.
- PostgreSQL is primary. libSQL parity checks remain required.
- `ic/Cargo.toml` has explicit heavy targets behind the `integration` feature.
- `ic/tests/workspace_integration.rs` currently gates PostgreSQL behavior only
  on `postgres`, conflicting with the intended `postgres + integration` rule.
- `ic/scripts/build-wasm-extensions.sh --strict` now fails on missing manifest
  fields, capabilities, source directories, build failures, and expected output.
  CI currently builds only the XMPP and WeeChat WASM test artifacts directly;
  a full registry-driven strict build/reuse job remains Phase 2 work.
- Legacy browser fixtures can still trigger their own full debug build. Engine V2
  E2E already fails closed without an executable `LUNARWING_E2E_BINARY`; CI has
  not yet added the one-build artifact job needed to run either browser suite.
- Standalone Rust projects outside the core workspace need path-triggered jobs;
  the core lockfile does not validate them.
- `.gitmodules` declares `pebble`, but current checkout does not contain a
  submodule gitlink. Keep `GIT_SUBMODULE_STRATEGY: none` until this is cleaned.

### 2.3 Deployment reality

`ic/scripts/lunarwing-mt-admin.sh upgrade-tenant` currently performs:

1. PostgreSQL backup unless explicitly disabled.
2. Git fetch and checkout of a branch, tag, or commit.
3. Tenant stop through the detected init system.
4. Local daemon and XMPP bridge builds in the tenant checkout.
5. WASM build and install.
6. Unit rendering and environment patching.
7. Tenant start.

Important gaps for CD:

- It accepts mutable branches, not only immutable release tags.
- It rebuilds once per tenant instead of deploying the CI-tested artifact.
- WASM build failure can warn and continue.
- Binary replacement is not an atomic release switch.
- Final health verification is suggested, not enforced.
- No fleet rollout command or automatic rollback transaction exists.
- Database migrations are forward-only; binary rollback can require restoring
  the matching pre-deployment PostgreSQL dump.

These gaps do not block CI. They block production artifact CD.

## 3. Goals

- One root pipeline for merge requests, default branch, schedules, release
  candidates, final tags, and protected deployments.
- No duplicate push and merge-request pipelines for the same commit.
- Fast required MR path with no credentials, privileged containers, live
  providers, or host service mutation.
- Exact feature coverage for default, PostgreSQL-only, libSQL-only, and all
  features.
- Strict format, Clippy, test, architecture, dependency, and secret gates.
- Native Linux x86_64 and aarch64 release artifacts first.
- Build once, verify once, promote the same bytes through release and deploy.
- First-class systemd user-manager and OpenRC staging.
- Protected, manual canary and production gates with serialized host access.
- Auditable backup, migration, health, and rollback evidence per tenant.

## 4. Non-Goals

- Item #12 does not add `.gitlab-ci.yml` or change runtime code.
- Initial rollout does not automate production deployment.
- Initial rollout does not promise macOS, Windows, or musl artifacts.
- CI does not replace human release-note, known-issue, codename, or security
  review.
- Live LLM, XMPP, Gotify, TensorZero, or tenant credentials never enter
  untrusted merge-request pipelines.
- CI does not call bare `systemctl` or `rc-service` for tenant lifecycle.
- CI does not treat warning-only, skipped, or missing-source jobs as evidence
  that a required release check passed.

## 5. Repository Layout

GitLab requires the entrypoint at repository root. Keep implementation details
in their own directory for item #13:

```text
.gitlab-ci.yml                    # Small composition root; local includes only
.gitlab/
  ci/
    workflow.yml                 # Pipeline-source rules
    defaults.yml                 # Images, cache, retries, runner tags
    validate.yml
    rust.yml
    integration.yml
    security.yml
    package.yml
    release.yml
    deploy.yml
  scripts/
    verify-release-version.sh
    package-release.sh
    publish-package.sh
    verify-package.sh
    deploy-release.sh
```

Generic project checks remain near their owner, for example
`ic/scripts/ci/quality_gate.sh`. GitLab wrappers should call those checks, not
copy their policy. Any helper used outside GitLab should take neutral inputs
such as `BASE_BRANCH` rather than GitLab-specific variable names.

## 6. Pipeline Sources and Trust Boundaries

Use `workflow: rules` at pipeline level, then job-level `rules`. Use
`$CI_DEFAULT_BRANCH`; never hardcode `main` or `master`.

| Pipeline source | Required scope | Secrets / privileged runners |
|---|---|---|
| Merge request | Validate, Rust checks, hermetic tests, dependency policy | None |
| Feature branch without open MR | Same fast checks or a reduced developer tier | None |
| Default branch | MR tier plus broader integration, coverage, package smoke | No production secrets |
| Schedule | Full integration, advisory refresh, coverage, ignored-test observation, worker matrix | Protected test secrets only where needed |
| RC tag `vX.Y.Z.W-rcN` | Full release gates, immutable candidate package, staging | Protected package/staging access |
| Final tag `vX.Y.Z.W` | Promote approved RC package, create GitLab Release, enable canary | Protected release access |
| Manual deploy | Canary or one production batch | Protected environment only |

Workflow rules must suppress a branch pipeline after an MR pipeline exists for
the same branch. Final and RC tag patterns must be explicit, and those tags
must be protected in GitLab settings.

MR pipelines from forks or unprotected branches must never receive protected
variables or protected runner access. Changes under `.gitlab/`, deployment
scripts, package scripts, or mt-admin require CODEOWNERS approval before merge.

## 7. Pipeline Graph

Stages provide readable grouping; `needs:` provides the actual DAG:

```text
validate
  |-- rust-check-default ------|-- rust-test-default
  |-- rust-check-postgres -----|-- postgres-integration
  |-- rust-check-libsql -------|-- libsql-integration -- browser-e2e
  |-- rust-check-all-features -|-- clippy-all-features
  |-- registry-validation -----|-- wasm-build
  |-- shell-policy ------------|-- shell-harnesses
  `-- python-policy ----------- `-- python-tests

required test/security gates
  `-- package candidate
        `-- verify package
              `-- publish immutable candidate
                    `-- staging systemd + staging OpenRC
                          `-- promote final release
                                `-- canary
                                      `-- production batches
```

Jobs should use `needs:` with `artifacts: true` only when consuming an exact
upstream artifact. Unrelated jobs run in parallel. MR jobs are `interruptible`
so superseded pipelines stop consuming runner capacity. Deployment jobs use a
per-host `resource_group` so two pipelines cannot mutate one host together.

## 8. Required Job Design

### 8.1 Validate

| Job | Action | Initial blocking state |
|---|---|---|
| `rustfmt` | `cargo fmt --all -- --check` from `ic/` | Blocking immediately |
| `cargo-metadata` | Parse locked workspace metadata | Blocking immediately |
| `version-policy` | WIT, extension, release, and tag version consistency | Blocking after GitLab variable adaptation |
| `architecture-boundaries` | Run `ic/scripts/check-boundaries.sh` | Blocking after current baseline is green |
| `panic-policy` | Run changed-line panic scanner and self-tests | Blocking immediately |
| `shell-syntax` | `bash -n` maintained shell directories | Blocking immediately |
| `shellcheck` | Warning-level checks on maintained scripts | Blocking after current warnings are fixed or explicitly scoped |
| `python-unit` | Explicit onboarding, web, Kawarimi, and DarkIRC suites | Blocking immediately |
| `python-static` | Compile maintained Python plus Pyright/Ruff after pinned setup | Phase 2 blocking |
| `registry-json` | Parse JSON and run registry validation | Blocking immediately |
| `secret-detection` | GitLab detection or pinned Gitleaks full-tree scan | Blocking immediately |
| `docs-links` | Check changed internal Markdown links | Phase 2 blocking |

Do not run `python -m compileall` over known experimental working copies until
they are repaired or deliberately excluded with a documented reason.

### 8.2 Rust checks and lint

All commands run from `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6 --locked
taskset -c 0-5 cargo check -j6 --locked --no-default-features --features postgres
taskset -c 0-5 cargo check -j6 --locked --no-default-features --features libsql
taskset -c 0-5 cargo check -j6 --locked --all-features
taskset -c 0-5 cargo clippy -j6 --locked --all --benches --tests --examples --all-features -- -D warnings
```

Use one job per feature shape. This preserves useful failure attribution and
allows parallel execution. All-feature success does not replace single-backend
checks because conditional compilation can differ.

Clippy should become blocking only after one baseline pipeline proves the tree
is green or a bounded cleanup change lands. Do not leave it permanently
`allow_failure`.

### 8.3 Rust tests

Required hermetic MR jobs:

```bash
taskset -c 0-5 cargo test -j6 --locked --workspace --lib -- --test-threads=1
taskset -c 0-5 cargo test -j6 --locked --no-default-features --features libsql --no-fail-fast -- --test-threads=1
```

Full-suite test cases run serially because existing config/setup tests mutate
the process environment. Keep Cargo compilation at `-j6`; raise test-thread
parallelism only after those tests use subprocess isolation or stop mutating
process-global environment state.

Default-branch or scheduled full jobs:

```bash
taskset -c 0-5 cargo test -j6 --locked --workspace --no-fail-fast -- --test-threads=1
taskset -c 0-5 cargo test -j6 --locked --no-default-features --features "libsql integration" --no-fail-fast -- --test-threads=1
```

PostgreSQL integration uses an isolated `pgvector/pgvector:pg16` service and a
unique disposable database:

```bash
taskset -c 0-5 cargo test -j6 --locked --no-default-features --features "postgres integration" --no-fail-fast -- --test-threads=1
```

Fix feature gates and silent skips before this job becomes release-blocking.
Missing `DATABASE_URL`, unavailable `vector`, or absent migrations must fail
the dedicated PostgreSQL job rather than skip tests.

Do not run blanket `--ignored`. Create named scheduled jobs for ignored tests
after providing their exact WASM, binary, DB, or network prerequisites.

### 8.4 Non-Rust hermetic tests

Required MR coverage should include:

```bash
python3 -m unittest \
  lunarwing_mt_onboard.tests \
  lunarwing_mt_onboard.export_tests \
  lunarwing_mt_onboard.import_tenant_tests \
  lunarwing_mt_onboard.verify_tests \
  lunarwing_mt_onboard.secrets_tests \
  lunarwing_mt_onboard.upgrade_tests \
  lunarwing_mt_onboard.upgrade_runtime_tests \
  lunarwing_mt_onboard_web.web_tests

bash ic-infrastructure-health-check/tests/run-all.sh
bash scripts/tests/test-build-lunarwing.sh
```

Also run Kawarimi and DarkIRC adapter unit tests with explicit module or file
paths. Do not rely on default discovery because several suites use nonstandard
filenames.

### 8.5 Path-triggered standalone crates

Use a matrix for deployable crates outside the core workspace. At minimum:

- `ic/bridges/xmpp-bridge`
- `ic/channels-src/xmpp`
- `ic/channels-src/multica`
- `darkirc_channel_for_lunarwing/darkirc`
- `lunarwing_weechat_wss/weechat_relay`
- `lunarwing-gotify-tool`
- `projects/ocr-sidecar`
- `pebble4lunarwing`

Each job runs format, locked check, and tests where a lockfile exists. Path
rules must also include shared WIT, protocol, registry, and CI files. Start
with conservative triggers; optimize only after observing real runtimes.

### 8.6 WASM

Before making the full registry-driven WASM suite blocking:

1. **Done:** `ic/scripts/build-wasm-extensions.sh --strict` fails on missing
   source, manifest fields, output, capability files, and build failures.
2. Pin `cargo-component`, `wasm-tools`, and `wasm32-wasip2` toolchain setup in
   the CI image.
3. Limit child Cargo builds with inherited CPU affinity and
   `CARGO_BUILD_JOBS=6`.
4. Store built components once and reuse them in browser, package, and staging
   jobs.

Proposed invocation from `ic/`:

```bash
CARGO_BUILD_JOBS=6 taskset -c 0-5 ./scripts/build-wasm-extensions.sh --strict
```

### 8.7 Browser and worker integration

Browser tests use Python 3.11+, Playwright Chromium, a temporary HOME, libSQL,
and mock services. The Engine V2 fixture already requires an explicit executable
`LUNARWING_E2E_BINARY`; the legacy `lunarwing_binary` fixture still rebuilds a
debug binary when stale. Add one bounded executable-build artifact job, migrate
the legacy fixture to consume it, and reuse that same artifact across browser
jobs.

Split browser jobs into:

- Hermetic gateway/UI scenarios with mock LLM and mock MCP.
- Engine V2 scenarios using the same packaged binary.
- Network-dependent ClawHub or clipboard scenarios, scheduled and reported
  separately.

Worker Docker smoke tests run only on a dedicated ephemeral privileged runner.
Never expose a Docker socket to fork or normal MR jobs. Fixed ports and Compose
project names must become per-job before parallel worker execution.

### 8.8 Security

Required security jobs:

- `taskset -c 0-5 cargo deny check` on every MR and release candidate.
- Scheduled RustSec refresh using a pinned advisory tool and explicit review of
  every waiver in `ic/deny.toml`.
- Full-tree secret detection; local added-line regex remains defense in depth.
- Lockfile and source-policy checks for core and deployable standalone crates.
- Hadolint for maintained Dockerfiles after baseline cleanup.
- Trivy or equivalent scanning for release images.
- CycloneDX SBOM for native packages and each published image.
- Provenance containing Git SHA, compiler, target, features, lockfile hash, and
  package checksum.

GitLab security templates may be enabled if supported by the installed edition,
but the baseline must not depend on paid-tier scanners. Pin all scanner images
by digest.

## 9. Runner Topology

Do not use one general-purpose runner for every trust level.

| Runner class | Minimum role | Trust | Suggested capacity |
|---|---|---|---|
| `lint` | Format, policy, shell, Python | Untrusted MR safe | 2-4 vCPU, 4-8 GB RAM |
| `rust-x86_64` | Core check, Clippy, Rust tests | Untrusted MR safe, isolated executor | 6 vCPU, 16 GB RAM, 25+ GB free disk |
| `rust-aarch64` | Native aarch64 release package | Protected tags only | 6 vCPU, 16 GB RAM, 25+ GB free disk |
| `docker-ephemeral` | Worker builds and smoke tests | Protected/default/schedule only | Disposable privileged VM, no production secrets |
| `staging-systemd` | Real user-manager deployment tests | Protected | Disposable or resettable Linux host |
| `staging-openrc` | Real Gentoo/OpenRC deployment tests | Protected | Disposable or resettable Gentoo host |
| `production-deploy` | Canary and fleet operations | Protected environment only | Host-local runner or tightly scoped SSH executor |

Runner requirements:

- Build runners cannot reach tenant production networks.
- Deployment runners cannot execute untrusted MR code.
- Protected runners accept protected refs only.
- Production runner credentials allow only the required mt-admin command set,
  not unrestricted root shells.
- Host keys are pinned. No `StrictHostKeyChecking=no`.
- Ephemeral Docker runners are destroyed after Docker-socket jobs.
- Runner concurrency and job-level affinity must both preserve the six-CPU
  Cargo limit.

## 10. Cache, Artifacts, and Reports

### 10.1 Cache policy

Cache only reproducible acceleration data:

- Cargo registry and Git dependency caches keyed by Rust version and
  `Cargo.lock` hash.
- Optional `sccache` with a project-specific namespace.
- Separate target caches per target triple, feature class, compiler, and job
  type. Never share one writable target directory between concurrent jobs.
- Python wheels or Playwright browser cache keyed by lock/input hashes.

Verify GitLab's separate-cache policy for protected and unprotected refs is
enabled, and prohibit `cache:unprotect`. Protected RC/final jobs use a distinct
`protected-release/<tag>/<gitsha>` cache and sccache namespace with credentials
unavailable to MR jobs. Release package builds start with a clean target
directory and never restore MR-produced object or target caches; only verified
dependency downloads may be reused.

Never cache secrets, tenant homes, database volumes, generated env files, SSH
state, or worker tokens. A 2.8 GB Cargo target directory is expensive to upload
on every job; prefer runner-local cache or `sccache` over blind full-target
archives.

### 10.2 Artifact policy

Short-lived pipeline artifacts:

- Test logs and machine-readable reports.
- Coverage output.
- Built WASM components used by downstream integration jobs.
- Candidate native package consumed by package verification and staging.

Durable release files belong in GitLab Generic Package Registry, not expiring
job artifacts. Disable duplicate Generic Package uploads in project settings
before publishing releases. Release jobs must verify that policy, use unique
`candidates/<exact-rc-tag>/<gitsha>/<target>` and
`final/<exact-final-tag>/<gitsha>/<target>` namespaces, and refuse a path that
already exists.
After upload, download the package again and verify its digest before attaching
it to a GitLab Release. A preflight existence check is defense in depth, not a
replacement for server-side duplicate rejection.

Every job that produces release-relevant output writes a small metadata file
containing pipeline ID, job ID, Git SHA, toolchain, input lock hash, and output
hash. Downstream jobs compare this metadata before use.

## 11. Release and Version Contract

LunarWing uses a four-part release identifier while Rust crates use three-part
SemVer. CI must enforce the mapping explicitly:

```text
Release/tag: v2.0.2.0
Release notes: RELEASE-v2.0.2.0.md
Core Cargo version: 2.0.2
```

Release candidate tags use `v2.0.2.0-rcN`. Final tags use `v2.0.2.0`. The
release verification job must fail unless:

1. Tag syntax is valid and tag is protected.
2. Tag points to the approved release commit.
3. Final tag points to the exact commit tested as one explicitly selected,
   approved RC.
4. Tag commit is reachable from the protected release/default branch, except
   for a documented emergency procedure.
5. Root release notes exist and contain the exact four-part version.
6. Codename is present and is not `UNKNOWN`.
7. Core and internal crate versions match the expected three-part version.
8. Lockfile is committed and `--locked` metadata succeeds.
9. WIT host constants, WIT package versions, and changed extension manifests
   satisfy `check-version-bumps.sh`.
10. No required job is skipped or allowed to fail.

Selection cannot be inferred from SHA alone because multiple RC tags can point
to one commit. Final promotion receives one exact protected RC tag, verifies
its signed release-evidence object, and writes a signed promotion record binding
the final tag to that RC tag, evidence digest, source SHA, and package digests.

Refactor `check-version-bumps.sh` to accept:

```text
BASE_BRANCH
CHANGE_LABELS
BASE_SHA (optional, preferred for deterministic MR comparison)
```

GitLab wrappers map `CI_MERGE_REQUEST_TARGET_BRANCH_NAME`,
`CI_MERGE_REQUEST_LABELS`, and target SHAs to these neutral inputs. Local and
other forge callers can set the same neutral contract.

## 12. Release Package Contract

### 12.1 Initial supported targets

- `x86_64-unknown-linux-gnu` on a native x86_64 runner.
- `aarch64-unknown-linux-gnu` on a native aarch64 runner.

Do not advertise musl, macOS, or Windows until matching native/cross runners,
install tests, and runtime smoke tests exist. Existing cargo-dist declarations
are aspiration, not current release evidence.

### 12.2 Package contents

Each target archive payload should contain:

```text
manifest.json
bin/lunarwing
bin/xmpp-bridge
wasm/channels/*.wasm
wasm/tools/*.wasm
capabilities/**/*.json
registry/**/*.json
admin/lunarwing-mt-admin.sh
admin/lunarwing-openrc-env-exec.sh
admin/templates/**
admin/mt-admin-lib/** (when decomposition lands)
health/**
migrations/MANIFEST.sha256
RELEASE-vX.Y.Z.W.md
SBOM.cdx.json
```

Publish detached integrity files alongside, not inside, the archive:

```text
lunarwing-<exact-release-or-rc-tag>-<gitsha>-<target>.tar.gz
lunarwing-<exact-release-or-rc-tag>-<gitsha>-<target>.tar.gz.sha256
lunarwing-<exact-release-or-rc-tag>-<gitsha>-<target>.tar.gz.sig
release-evidence.json
release-evidence.json.sig
```

`manifest.json` records immutable candidate identity:

- Base four-part release and Cargo three-part version.
- Git SHA and exact protected `candidate_tag` (the RC tag).
- Rust compiler `-Vv` output and target triple.
- Enabled Cargo features.
- `Cargo.lock` digest.
- WIT package versions.
- Every archive payload file except `manifest.json` itself, including mode,
  size, and SHA-256 digest.
- Build pipeline and job IDs.

This avoids recursive integrity claims. `manifest.json` covers the payload;
the detached archive checksum covers `manifest.json` and the complete archive;
the detached signature covers that archive digest. Verification starts with
the detached signature, then archive digest, then payload manifest.

The admin bundle needs an explicit runtime-dependency manifest. Current
mt-admin uses `scripts/templates/**` and
`scripts/lunarwing-openrc-env-exec.sh`; future sourced libraries must be added
to the same manifest. Package verification extracts into an empty directory and
runs no-mutation fixture tests for both systemd-user and OpenRC rendering. This
must prove no required file is resolved from a source checkout.

The package job runs release builds only on protected candidate/tag pipelines.
It uses explicit target directories and six jobs. `scripts/build-lunarwing.sh`
can build the daemon, but a package helper must also build XMPP bridge, strict
WASM outputs, metadata, and the final archive.

Suggested names:

```text
lunarwing-v2.0.2.0-rc1-<gitsha>-x86_64-unknown-linux-gnu.tar.gz
lunarwing-v2.0.2.0-rc1-<gitsha>-aarch64-unknown-linux-gnu.tar.gz
lunarwing-v2.0.2.0-<gitsha>-x86_64-unknown-linux-gnu.tar.gz
lunarwing-v2.0.2.0-<gitsha>-aarch64-unknown-linux-gnu.tar.gz
```

Candidate names and registry paths always include the exact RC tag even when
multiple RC tags point to one SHA. Final promotion changes the final namespace
and filename only after verifying the candidate bytes; it does not rebuild.
The archive manifest intentionally retains `candidate_tag`; final tag identity
lives in the signed promotion record that binds final tag to candidate tag,
manifest digest, archive digest, and source SHA.

Worker images are separate release objects. Push release and Git-SHA tags, then
record and deploy the immutable registry digest. Never deploy `latest`.

### 12.3 Promotion, not rebuild

RC pipeline publishes a candidate package keyed by release, RC, target, and
Git SHA. Staging uses that exact digest. After both init matrices and rollback
tests pass, a protected job publishes signed `release-evidence.json` containing:

- Exact RC tag and source SHA.
- Per-target package names and digests.
- Candidate pipeline and package job IDs.
- systemd-user and OpenRC staging job IDs and results.
- Backup/restore and rollback result identifiers.
- Required approver identities and approval timestamps.

Final promotion receives the exact RC tag as protected input, verifies evidence
signature and all referenced digests, and requires final tag to point to the
same Git SHA. It then copies or links approved bytes into the final version
namespace, writes a signed final-tag promotion record, and never recompiles.

This gives one evidence chain:

```text
source SHA -> candidate package digest -> staging evidence -> final release
           -> canary evidence -> production deployment records
```

## 13. Continuous Deployment Design

### 13.1 Required mt-admin capability

Current source-build upgrade remains available for operators, but cannot be
called artifact CD. Before production CD, add mt-admin-owned operations with a
contract equivalent to:

```text
stage-release <package>
deploy-release <tenant> <release>
rollback-release <tenant> <release>
```

Exact command names may change. Required behavior may not:

- Verify package signature, manifest, target architecture, and every digest.
- Validate archive entry count and expanded size; reject absolute paths,
  traversal, device/FIFO entries, and unexpected symlink or hardlink targets
  before extracting into a root-owned staging directory.
- Refuse mutable or mismatched release identities.
- Extract into a new versioned directory without touching active files.
- Keep at least current and previous package versions.
- Switch daemon, bridge, and matching WASM set as one controlled activation.
- Render both systemd-user and OpenRC units through existing mt-admin
  abstractions.
- Record previous revision, new revision, package digest, backup path, and
  migration state.
- Run `mt-admin status` plus authenticated gateway smoke after start.
- Return nonzero on failed required health checks.
- Never call bare system-bus `systemctl` for tenant units.
- Never hardcode multi-tenant ports; use `/etc/lunarwing/ports.json`.

A versioned layout can use a stable `current` link, for example:

```text
/home/<tenant>/lunarwing/releases/<release>-<gitsha>/
/home/<tenant>/lunarwing/current -> releases/<release>-<gitsha>/
```

Activation details belong in mt-admin and its renderers, not GitLab shell
snippets. Unit templates must point at the stable activation path rather than
Cargo target output.

### 13.2 Staging

Every RC runs on two protected environments:

- `staging/mt-systemd`
- `staging/mt-openrc`

Required matrix on each real init system:

1. Verify and stage package.
2. Provision a fresh tenant through mt-admin.
3. Upgrade a previous-release tenant with representative state.
4. Verify PostgreSQL backup and restore before destructive migration testing.
5. Start and run automatic migrations.
6. Verify rendered units and tenant-user ownership.
7. Verify daemon, XMPP bridge, WASM inventory, gateway, and ports registry.
8. Run `ic/scripts/release-test.sh` with dedicated staging auth.
9. Exercise enabled worker/channel combinations supported by that environment.
10. Roll back to previous package and, when schema changed, restore the matching
    DB backup.
11. Re-run health and data-integrity checks after rollback.

Mock OpenRC renderer tests remain useful but do not replace one real OpenRC
release gate.

### 13.3 Canary

Final protected tag enables a manual `production/canary` job for one low-risk
tenant. Before mutation, deployment must prove:

- Package digest equals final GitLab Release digest.
- Tenant and host pass mt-admin doctor/status preflight.
- Current release and config hashes are recorded.
- Fresh PostgreSQL dump exists, has expected `PGDMP` form, and is retained.
- Tenant env/state and ports registry backup exist without exposing values.
- No other deployment holds the host resource group.

Deploy, run authenticated smoke, inspect service and health evidence, then soak
for a release-defined period. Schema-affecting releases should use at least a
day unless incident response requires a documented exception. Do not hold a
runner job sleeping during soak; record evidence and require a later approval.

### 13.4 Production

Production is manual, protected, and tenant-by-tenant:

1. Verify canary approval and unchanged package digest.
2. Back up and verify one tenant.
3. Deploy through mt-admin.
4. Verify status, gateway, channels, workers, and health window.
5. Record result.
6. Continue to next tenant or halt the fleet.

Prefer two-person approval: release owner approves artifact; ops owner approves
deployment. If installed GitLab edition lacks deployment approval rules, use a
protected manual environment plus an external signed approval record. Never
silently reduce to one unprotected button.

### 13.5 Rollback

Before a migration starts, rollback can reactivate the previous package and
matching WASM set, re-render units, start, and verify.

After forward SQL migration starts, binary-only rollback is unsafe unless old
binary/schema compatibility was explicitly tested. Required sequence:

1. Stop tenant through mt-admin.
2. Keep PostgreSQL available for restore.
3. Restore verified pre-deploy `pg_dump -Fc` through mt-admin.
4. Restore matching env/state/WASM data when release changed them.
5. Activate previous package.
6. Re-render and start through mt-admin.
7. Verify migration history, owner scope, secret decryption, channels,
   routines, and memory.
8. Keep remaining fleet halted pending review.

Do not implement blind automatic rollback on a transient probe failure. Halt,
preserve evidence, determine migration state, then choose package rollback or
database restore.

## 14. Secrets and Supply-Chain Rules

- Build/test MR jobs receive no protected variables.
- Use `CI_JOB_TOKEN` for package upload and release access where supported.
- Use a project access token only when job-token permissions are insufficient;
  scope it narrowly, mask it, protect it, and rotate it.
- Keep tenant secrets on deployment hosts. GitLab should not store raw tenant
  env files, DB URLs, master keys, XMPP passwords, gateway tokens, worker
  bearer tokens, or migration passphrases.
- Environment-scoped staging credentials must use disposable non-production
  accounts.
- Artifact signing key should be KMS/HSM-backed or otherwise unavailable to MR
  runners. If GitLab OIDC/keyless signing is selected, document trust roots and
  offline verification first.
- Signed JSON uses a versioned canonical serialization, records signing key ID,
  and has documented key rotation and revocation behavior.
- Pin CI images by digest and Cargo/Python helper versions. Do not install an
  unpinned `latest` tool during every pipeline.
- Preserve `--locked`; reject mutable upstream worker tags and unchecked
  downloads before treating worker images as release-grade.
- Remove insecure package-manager fallbacks and `|| true` build fallbacks from
  release image paths.
- Mirror credentials must not appear in Git remote URLs printed by jobs or
  process listings.

## 15. Rollout Phases

### Phase 0 - Baseline and platform inventory

Required before blocking CI:

1. Confirm actual GitLab version, edition/tier, Package Registry, Container
   Registry, protected environment, approval, and runner capabilities.
2. Disable duplicate Generic Package uploads and document how release jobs
   verify that setting before publication.
3. Enable separate protected/unprotected caches, prohibit `cache:unprotect`,
   and provision an isolated protected release cache/sccache namespace.
4. Inventory native x86_64, native aarch64, systemd-user, OpenRC, and ephemeral
   Docker runners.
5. **Done:** pin and reconcile Rust 1.96.1 across manifests, toolchain, CI images, and current docs.
6. Fix `quality_gate_strict.sh` working directory and CPU limits.
7. Make version checks forge-neutral and use `$CI_DEFAULT_BRANCH` mappings.
8. Make architecture-boundary baseline green.
9. Fix PostgreSQL feature gates and fail dedicated DB jobs on missing DB.
10. Scope/fix ShellCheck and maintained Python syntax baselines.
11. **Partially done:** strict registry/source/output validation exists in `build-wasm-extensions.sh`; full CI invocation and artifact reuse remain.
12. Resolve stale `.gitmodules` behavior.
13. Correct stale GitLab repository paths in release docs.

### Phase 1 - Blocking merge-request CI

Implement root include structure and enable:

- Rust format and four-shape check matrix.
- Strict Clippy after baseline cleanup.
- Core library and hermetic libSQL tests.
- Explicit Python unit suites.
- Shell syntax and hermetic health/build-script harnesses.
- Registry JSON and extension version checks.
- Cargo deny and secret detection.

Exit criteria:

- Pipeline starts once per MR commit.
- Required jobs are blocking and reproducible on a clean runner.
- No MR job sees protected variables or privileged runner tags.
- Warm-cache required path target is 30 minutes or less at p95; measure before
  optimizing or weakening coverage.

### Phase 2 - Integration and coverage

Add:

- PostgreSQL/pgvector service tests.
- Heavy `integration` feature jobs.
- Strict WASM build and artifact reuse.
- Browser tests using one prebuilt binary.
- Path-triggered standalone crate matrix.
- Coverage reporting with measured baseline.
- Named scheduled ignored/network test jobs.
- Worker smoke on isolated ephemeral runners.

Exit criteria:

- Required jobs fail on unexpected skips or missing prerequisites.
- PostgreSQL and libSQL results are distinct and visible.
- Browser and worker jobs cannot mutate developer/production services.

### Phase 3 - Package and release

Add:

- Native x86_64 and aarch64 package jobs.
- Strict package manifest and post-upload download verification.
- Generic Package Registry publishing.
- SBOM, scanner, checksum, and signature generation.
- Complete admin runtime-dependency manifest and extracted-package tests.
- Signed RC evidence and final-tag promotion records.
- RC-to-final promotion without rebuild.
- GitLab Release creation from exact release notes and package links.

Exit criteria:

- Released bytes equal staged bytes.
- Package can be installed and verified without source checkout or compiler.
- Exact RC evidence unambiguously binds staging results to final promotion.
- Release page identifies exact tag, SHA, target, digest, SBOM, and signature.

### Phase 4 - Protected CD

Add only after mt-admin artifact operations exist:

- Real systemd-user and OpenRC staging.
- Backup/restore and rollback rehearsal.
- Protected canary environment.
- Serialized production batches.
- Deployment evidence tied to package digest and tenant.

Exit criteria:

- Both init systems deploy and roll back the same package.
- No deployment script bypasses mt-admin.
- Schema-changing rollback is rehearsed with DB restore.
- Production deploy requires protected approval and an approved canary.

## 16. Item #13 Implementation Breakdown

Keep changes reviewable and independently revertible:

1. **Baseline fixes:** repair generic checks without adding GitLab config.
2. **Pipeline skeleton:** root include plus `.gitlab/ci/` defaults/workflow.
3. **MR validate jobs:** format, policy, shell, Python, registry, secrets.
4. **Rust matrix:** checks, Clippy, hermetic tests, caches, reports.
5. **DB/integration:** pgvector service, integration features, strict skip rules.
6. **WASM/browser:** strict WASM artifacts and prebuilt E2E binary flow.
7. **Standalone projects:** path-triggered child/matrix jobs.
8. **Security/package:** pinned tools, SBOM, scanning, native packages.
9. **Release promotion:** Generic Package Registry and GitLab Release.
10. **Artifact deployment:** separate high-risk mt-admin change with both-init
    tests and rollback coverage.
11. **Protected environments:** staging, canary, then production.

Do not combine mt-admin artifact activation and first root CI pipeline in one
change. Their risk and rollback surfaces differ.

## 17. Acceptance Criteria

### CI

- Root pipeline is present and locally included files validate in GitLab.
- MR pipelines are unprivileged, secret-free, interruptible, and nonduplicate.
- All Cargo commands obey affinity, job, and test-thread limits.
- No compile-only job uses `cargo build`; executable builds happen once where
  integration/package tests require them.
- PostgreSQL, libSQL, and all-feature coverage are explicit.
- Required missing prerequisites fail rather than skip.
- Existing green local checks remain green in clean CI containers.
- `allow_failure` is limited to named observational jobs with owners and a
  removal condition.

### Release

- Base release in release notes, crate versions, WIT versions, and candidate
  manifest agrees; signed promotion record binds final tag to that immutable
  candidate manifest and archive digest.
- Final release promotes approved RC bytes without rebuild.
- Generic Package duplicates are disabled and release paths are SHA-addressed.
- Packages are immutable, downloadable, covered by detached checksum and
  signature, and accompanied by SBOM/provenance.
- Extracted admin bundle renders systemd-user and OpenRC fixtures without a
  source checkout.
- Signed RC evidence and final promotion record identify exact RC, SHA,
  package digests, staging jobs, rollback results, and approvals.
- x86_64 and aarch64 artifacts are built and smoke-tested natively.
- GitLab Release links durable package-registry objects, not expiring job files.

### Deployment

- Package signature and digest are verified before tenant stop.
- systemd user-manager and OpenRC staging both pass.
- Tenant lifecycle uses mt-admin only.
- Backup path and integrity are recorded before migration.
- Canary approval precedes production.
- Production host access is serialized.
- Rollback restores matching package, WASM, and DB state when required.

## 18. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Pipeline becomes too slow | Parallel DAG, path-trigger standalone projects, measured caches, scheduled heavy tiers; never drop core gates blindly. |
| Existing debt makes first pipeline red | Phase 0 baseline cleanup; temporary nonblocking visibility only with explicit exit condition. |
| Cache contamination | Separate protected/unprotected caches, protected release namespaces, no MR target cache in release builds, isolated writable targets. |
| Fork steals secrets or Docker socket | No protected variables/runners on MR; protected refs and environments; ephemeral privileged runner. |
| CI artifact differs from deployment | RC promotion and mt-admin prebuilt package activation. |
| Package registry accepts duplicate versions | Disable duplicates server-side, use SHA-addressed paths, and refuse existing paths. |
| Recursive/self-referential checksum design | Payload manifest excludes itself; detached digest and signature cover complete archive. |
| Wrong RC promoted when tags share a SHA | Require explicit RC tag plus signed evidence and final promotion record. |
| Admin package omits runtime files | Maintain explicit runtime dependency manifest and test extracted package without source tree. |
| WASM/host ABI mismatch | Package and activate daemon, WIT metadata, tools, channels, and capabilities as one release set. |
| DB migration breaks rollback | Verified pre-deploy dump, migration-aware rollback, canary, no blind binary rollback. |
| systemd-only automation regresses Gentoo | Real OpenRC staging and all lifecycle through mt-admin abstraction. |
| GitLab feature unavailable in CE tier | Open-source scanners and protected manual jobs; external two-person approval when native approval rules are unavailable. |
| Mutable dependencies undermine provenance | Locked Cargo inputs, pinned CI images/tools/upstream revisions, checksum verification. |

## 19. Decisions to Confirm Before Item #13

Recommended defaults are shown first:

1. **Rust release toolchain:** pin declared MSRV-compatible exact version, then
   test current stable separately on schedule.
2. **GitLab approval fallback:** protected manual job plus external signed
   two-person record if native deployment approvals are unavailable.
3. **Signing:** KMS/HSM-backed signing key; use keyless only after documenting
   self-hosted GitLab OIDC trust and offline verification.
4. **Production executor:** host-local protected runner invoking a narrowly
   allowed mt-admin command; SSH is fallback with pinned host keys.
5. **First release targets:** Linux GNU x86_64 and aarch64 only.
6. **Coverage:** publish measured baseline first; set blocking thresholds after
   excluding generated/vendor code and repairing known ignored tests.

## 20. References

Repository sources reviewed:

- `AGENTS.md`
- `ic/AGENTS.md`
- `ic/Cargo.toml`
- `ic/deny.toml`
- `ic/scripts/ci/quality_gate.sh`
- `ic/scripts/ci/quality_gate_strict.sh`
- `ic/scripts/check-boundaries.sh`
- `ic/scripts/check-version-bumps.sh`
- `ic/scripts/build-wasm-extensions.sh`
- `ic/scripts/lunarwing-mt-admin.sh`
- `ic/scripts/release-test.sh`
- `scripts/build-lunarwing.sh`
- `scripts/tests/test-build-lunarwing.sh`
- `ic-infrastructure-health-check/tests/run-all.sh`
- `docs/ops/RELEASE_COMMANDS.md`
- `docs/ops/PRE-RELEASE-TESTING.md`
- `docs/ops/MULTITENANCY-PRODUCTION.md`
- `docs/releases/RELEASE-v2.0.1.0.md`

GitLab platform behavior checked against official GitLab 19.1 documentation
for merge-request pipelines, `workflow: rules`, release CI/CD, Generic Package
Registry assets, protected environments, and deployment approvals. Repository
release docs claim a self-hosted GitLab 19.2 instance; item #13 must validate
the actual instance version, edition, and enabled features before relying on
tier-specific behavior.
