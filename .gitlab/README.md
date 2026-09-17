# GitLab CI/CD

This directory implements unprivileged, secret-free merge request validation
plus protected worker-image candidate publication. Production package and tenant
deployment remain absent: current mt-admin upgrades rebuild per tenant and
cannot yet activate one verified CI artifact safely.

## Pipeline behavior

- Merge request pipelines replace branch pipelines once an MR is open.
- Branch, tag, schedule, API, trigger, and manual pipelines remain supported.
- Validation jobs are interruptible and receive no production credentials.
  Registry-writing candidate/promotion jobs are non-interruptible and exist only
  on a protected default branch or protected, version-shaped push tag.
- Cargo always runs through `scripts/cargo-ci.sh`, with CPU affinity `0-5` and
  at most two build jobs to bound peak memory. Rust test cases run serially
  because many mutate the process environment.
- PostgreSQL integration now uses a disposable pgvector service. Browser,
  standalone-crate, native package, and production deployment work remains
  deferred until its separate prerequisites are met.

## Required runners

Create unprotected, unprivileged runners with these tags:

| Tag | Required capacity | Access |
|---|---|---|
| `lint` | Isolated Docker/Kubernetes executor; 2-4 CPUs, 4-8 GB RAM | Public package mirrors only; no protected variables or Docker socket |
| `rust-x86_64` | Isolated Docker/Kubernetes executor; 6 CPUs numbered `0-5`, 16 GB RAM, 40 GiB free disk | Public Cargo/Git/package mirrors only; no protected variables or Docker socket |
| `container-build` | Isolated Docker/Kubernetes executor with rootless BuildKit user-namespace support | Protected registry credentials only; no Docker socket, privileged mode, or production network |

Override `CI_LINT_RUNNER_TAG` or `CI_RUST_RUNNER_TAG` at project level if local
runner tags differ. Do not map either variable to protected or privileged
runners. Keep `container-build` protected; unlike validation runners, it can
write only to this project's container registry.

Pinned images:

- Rust 1.96.1 Bookworm OCI index (primary and MSRV jobs):
  `sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663`
- Python 3.12 slim Bookworm OCI index:
  `sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b`
- Gitleaks 8.30.1 OCI index:
  `sha256:c00b6bd0aeb3071cbcb79009cb16a60dd9e0a7c60e2be9ab65d25e6bc8abbb7f`
- cargo-deny 0.20.2 x86-64 musl archive:
  `sha256:9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f`
- BuildKit 0.25.1 rootless OCI index:
  `sha256:0c0ce4929976ace509f7a53878303504b2de8857829aea4932af9f7325a30f51`
- Trivy 0.74.0 OCI index:
  `sha256:62b1e65e8869bc4b4c6aa4fa2b21595256c7c2f6018a9d9ad61caf87187c1969`
- pgvector PostgreSQL 16 OCI index:
  `sha256:ccc6e83d6e35e931dc7c5def2022729d5a6c370318d099181995567ff1fb4d6b`

`ci-config` enforces Rust 1.96.1 across `ic/Cargo.toml` `rust-version`,
`ic/rust-toolchain.toml`, and both primary/MSRV image tags. It also requires
both Rust jobs to use same digest-pinned image and keeps README digest in sync.
Job images are declared through the `CI_LINT_IMAGE`, `CI_RUST_IMAGE`, and
`CI_GITLEAKS_IMAGE` variables in `defaults.yml`; `ci-config` expands those
variables before checking pins, so overrides must stay digest-pinned too.

## Docker Hub pull rate limits

Symptom — a job fails before any test runs, with a log like:

```
ERROR: Job failed: failed to pull image "docker.io/library/python:3.12-slim-bookworm@sha256:..."
  with specified policies [always]: Error response from daemon: toomanyrequests:
  You have reached your unauthenticated pull rate limit.
```

This is a **runner/infrastructure failure, not a test defect**. Docker Hub
caps anonymous pulls at 100 per 6 hours per public egress IPv4. Our runners
pull without Docker Hub credentials, so every container on the runner host
(and anything else behind the same NAT IP) shares that quota. Once it is
exhausted, the Docker executor cannot fetch the job image and GitLab Runner
(`manager.go` in its source) aborts the job before the checkout or any
script executes. Because jobs share base images, the failures cluster: the
lint/validate jobs (`ci-config`, `shell-syntax`, `python-syntax`,
`registry-json`, `version-policy`, `panic-policy`, `release-version`) all
use the `.lint-job` Python image, and the Rust jobs (`rust-check-*`,
`wasm-channel-test-artifacts`, `rust-test-*`, `cargo-deny`) all use the
`.rust-job` image — one exhausted quota takes down both groups at once.

Immediate action: retrying inside the 6-hour window does not help (the
built-in `runner_system_failure` retry already fired and failed the same
way). Wait for the quota window to reset, then retry the pipeline.

Permanent mitigations, in order of preference:

1. **Authenticate runner pulls to Docker Hub.** A free Docker Hub account
   gets its own per-account quota instead of the shared per-IP anonymous
   one. Add `DOCKER_AUTH_CONFIG` to the runner host's `config.toml`
   (`environment = ["DOCKER_AUTH_CONFIG=..."]` under `[[runners]]`), never
   as a CI variable — credentials stay on the runner host per the
   secret-handling rules above.
2. **Mirror the pinned images into the project container registry** and
   override the image variables. The registry from
   `docs/ops/GITLAB_CONTAINER_REGISTRY.md` is already live. From any host
   with podman/docker and a `read_registry`+`write_registry` PAT, for each
   of the three pinned images:

   ```bash
   podman pull  docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b
   podman tag   docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b \
                registry.git.lunarwing.org/lunarwing/lunarwing2/ci/python:3.12-slim-bookworm
   podman push  registry.git.lunarwing.org/lunarwing/lunarwing2/ci/python:3.12-slim-bookworm
   ```

   Digests are content-addressed, so the mirrored copy keeps the same
   `@sha256` pin. Then set project/group CI variables (unprotected,
   unmasked — image names are not secrets), e.g.
   `CI_LINT_IMAGE=registry.git.lunarwing.org/lunarwing/lunarwing2/ci/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b`
   and likewise for `CI_RUST_IMAGE` / `CI_GITLEAKS_IMAGE`. Runners
   authenticate to the GitLab registry automatically with the job token, so
   pulls stop counting against Docker Hub entirely. When bumping an image,
   update the pinned default in `defaults.yml` and re-mirror.
3. **GitLab Dependency Proxy** (requires an admin to enable it instance-
   wide): prefix image names with
   `${CI_DEPENDENCY_PROXY_GROUP_IMAGE_PREFIX}` instead of option 2's manual
   mirror. Equivalent effect; not currently enabled on this instance.

If runner image allowlists are enabled (see above), add the mirrored
registry paths to the allowlist when adopting option 2.

## Rust runner memory budget (OOM / signal 9)

Symptom — a Rust check/clippy/test job fails with no compiler diagnostic at
all, only:

```
sccache: Compile terminated by signal 9
error: could not compile `lunarwing` (lib test)
  ... process didn't exit successfully: .../rustc ... (exit status: 2)
```

This is the **kernel OOM killer**, not a code defect: rustc compiling the
`lunarwing` lib alone approaches the 16 GB runner ceiling, and two of those
compiles on one host exceed it. Evidence: pipelines #3654 (`rust-clippy-all`),
#3702 (`rust-check-postgres`), #3707 (`rust-check-postgres` +
`rust-check-libsql`), and #3742 (`rust-test-lib`) all died this way while an
overlapping pipeline's identical compile ran on the same runner host — job
traces show the same
system ID (`lunarwing-rust-docker`, `r_dLo2MDLTpMm2`) serving build slots
`concurrent-0`, `concurrent-1`, and `concurrent-3` simultaneously, i.e. the
documented `limit = 1` runner contract had drifted at the host.

Two independent guards, both required:

1. **Repo-side (enforced here):** every job that compiles the `lunarwing` lib
   carries `resource_group: rust-compile-16gb`. GitLab queues same-group jobs
   project-wide, so overlapping pipelines can never run two giant-crates
   compiles at once, whatever the runner concurrency setting is. That includes
   the MSRV check (same lib compile under the pinned toolchain) and
   `cavepony-wasm`, whose `wit_compat` step compiles the lunarwing lib test
   binary (MR !997 review). Cheaper jobs (rustfmt, cargo-metadata, wasm
   channel artifacts) are not in the group.
2. **Host-side (runner config):** keep `limit = 1` (see §6 runner setup). The
   resource group cannot see non-project load on the host, so the per-host
   limit remains the real fence for anything outside this repository.

Recovery: rerun the failed job — nothing in the tree is broken. If failures
persist, check the runner host for concurrent build slots (`docker ps | grep
concurrent-` on the runner host) and restore `limit = 1` before anything else.

## container-build runner disk (ENOSPC)

Symptom — `worker-*-candidate` fails inside BuildKit with:

```
error: failed to write /build/pebble/target/release/deps/...: No space left on device (os error 28)
```

and every downstream `worker-*-scan` then fails within seconds with the
explicit `worker-image.env is missing — the candidate job for this worker
failed or did not publish an image` guard (added after pipeline #3684, where
the raw error was a bare `can't open './worker-image.env'`).

This is runner-host disk exhaustion on the `container-build` tag. Recovery:
free space on that host (prune dangling images/build cache), then re-run the
candidate job — the scan can only succeed after a candidate publishes
`worker-image.env`.

## Required jobs

- CI composition, shell syntax, shellcheck, Python syntax, registry JSON, version
  policy, changed-line panic policy, rustfmt, and locked Cargo metadata.
- Protected release-tag syntax, release-note codename, and four-part to
  three-part Cargo version mapping.
- Default and all-feature `cargo check` jobs plus `--tests` compilation for
  PostgreSQL-only and libSQL-only feature shapes.
- Workspace library tests, hermetic libSQL tests, and a non-skipping PostgreSQL
  workspace integration target against disposable pgvector/PostgreSQL 16.
- Explicit onboarding, web, Kawarimi, and DarkIRC Python suites with zero skips.
- Fixture-only shell harnesses for mt-admin rendering, build policy, and health.
- `python-unit`, `shell-harnesses`, and `cavepony-wasm` (cavepony version assert
  + `wit_compat` test) are all required jobs; failures block merge.
- Blocking ratchets: `rust-clippy-all` (all-features, `-D warnings`) and
  `architecture-boundaries` (changed-line hard violations only; printed
  full-tree debt counts stay informational). Both run in the full-validation
  context since goals 2.1.1.0 item 23.
- The changed-line panic policy (`panic-policy` job) checks `ic/src` and
  `ic/crates` only; test bodies are excluded via the `#[test]`-attribute and
  `mod tests` walk in `ic/scripts/check_no_panics.py`.
- `standalone-deliverables` verifies locked Cargo metadata for every unit
  classified in `.gitlab/deliverables.json`; an unclassified tracked lockfile
  fails `ci-config` validation.
- `ci-config` validation rejects jobs whose `stage:` is missing from the root
  `stages` list, so stage renames/removals fail at validation time instead of
  killing the whole pipeline after push.
- Worker promotion runs a 15-second registry auth-realm smoke test
  (`.gitlab/scripts/registry-realm-smoke.py`) before the promote script, so a
  broken realm fails cheaply instead of after the multi-hour candidate and
  scan jobs.
- JUnit reports: `python-unit` emits `junit-python.xml`, `shell-harnesses`
  emits `junit-shell.xml`; the worker scan emits a GitLab
  `container_scanning` report alongside its blocking CRITICAL gate.
- cargo-deny and redacted current-tree Gitleaks scans.
- One Rust job per runner host: `limit = 1` in runner `config.toml` is a hard
  constraint, verified in the activation checklist (§6). Parallel jobs contend for
  CPUs 0-5 with no in-pipeline guard.
- CI parallelism is intentional: `-j2` (libSQL `-j1`) caps peak memory within
  the 16 GB runner for four-way check jobs; `-j6` in AGENTS.md is the local-dev
  budget. Normal test jobs retain line tables only; libSQL test jobs omit debug
  info entirely to avoid OOM near 16 GB.
- Rust runner memory budget (cross-pipeline guard): every job that compiles the
  `lunarwing` lib — the giant-crates rustc/clippy-driver invocation — shares the
  project-wide `rust-compile-16gb` resource group, so only one such compile runs
  at a time **across all pipelines**, not just within one. This is the repo-side
  half of the runner budget; `limit = 1` in the runner `config.toml` is the
  host-side half. See "Rust runner memory budget" below for why both are needed.

Known false-positive Gitleaks findings are listed by exact fingerprint in
`/.gitleaksignore`. Broad path or rule exclusions are forbidden. Worker-image
CRITICAL findings that Debian will not fix for the pinned toolchain
dependencies are listed by exact CVE with rationale and expiry in
`.gitlab/trivy-ignore`; expired entries stop being ignored and fail the
scan gate again.

`skip-version-check` is inert unless a maintainer also supplies
`ALLOW_VERSION_CHECK_SKIP=1`; commit messages cannot bypass version policy.

## Observational jobs

These run with `allow_failure: true`:

- `rust-msrv-check`: becomes blocking once the declared MSRV (1.96.1) is confirmed
  green, closing the plan §15.5 reconciliation.
- `secret-history`: scheduled only; becomes blocking after historical findings
  are reviewed.

Formerly observational, now blocking (goals 2.1.1.0 item 23): `rust-clippy-all`
(clean all-features baseline landed) and `architecture-boundaries` (changed-line
hard-violation ratchet; the script reserves its exit code for regressions).

Each job names its owner and removal condition in `.gitlab/ci/*.yml`. Do not add
new permanent `allow_failure` jobs.

## GitLab setup runbook

The public `/help` page confirms this instance runs GitLab Community Edition.
An administrator must confirm the exact version before activation; the version
API requires authentication. Phase 1 uses no paid-tier security scanner or
deployment-approval feature.

### 1. Create isolated project runners

In the project, open **Settings > CI/CD > Runners**, then create two project
runners. Use the generated runner authentication token and registration command
shown by GitLab. Tokens are secrets: enter them only on the runner host; never
commit them or save them as project variables.

Create the lint runner with:

- Tag: `lint`.
- **Run untagged jobs**: disabled.
- **Protected**: disabled. This runner must accept unprotected MR jobs safely.
- Executor: isolated Docker or Kubernetes, never shell executor.
- Capacity: 2-4 CPUs and 4-8 GB RAM.

Create the Rust runner with:

- Tag: `rust-x86_64`.
- **Run untagged jobs**: disabled.
- **Protected**: disabled.
- Executor: isolated Docker or Kubernetes, never shell executor.
- Capacity: six available CPUs numbered `0-5`, 16 GB RAM, and at least 40 GiB
  free disk. Budget approximately 1.5 GiB for checkout/vendor, 10 GiB for
  sccache, 10 GiB for Cargo registries and test targets, and at least 18 GiB of
  safety headroom. The former 80 GiB cache cap on a 25 GB runner let the
  filesystem fail before sccache could evict entries.
- Per-host job limit: one. Every Cargo job pins itself to CPUs `0-5`; parallel
  Rust jobs therefore require separate runner hosts or VMs, not overlapping
  jobs on one host.
- Maximum job timeout: at least eight hours for the protected
  `container-build` runner and at least two hours for Rust runners. The project
  timeout must be at least eight hours; GitLab applies whichever project/runner
  limit is lower. Ordinary validation jobs still use their two-hour YAML cap.

Essential Docker executor settings belong in runner-host `config.toml`, not in
this repository:

```toml
[[runners]]
  executor = "docker"
  limit = 1

  [runners.docker]
    image = "docker.io/library/rust:1.96.1-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663"
    privileged = false
    cpuset_cpus = "0-5"
    memory = "16g"
    volumes = ["/cache"]
```

Do not mount `/var/run/docker.sock`, host service directories, tenant homes,
`/etc/lunarwing`, SSH state, or secret files. Keep build runners unable to route
to production or tenant networks. If runner image allowlists are enabled, allow
only the three digest-pinned images listed above.

Runner egress must reach:

- This GitLab repository.
- Docker Hub authentication and registry endpoints.
- Debian Bookworm package mirrors.
- crates.io index/static files and the pinned Git dependencies in `Cargo.lock`.
- PyPI and Python package file hosts.
- GitHub release assets for checksum-pinned cargo-deny and the RustSec advisory
  database.

Block all production, tenant, service-management, and private control-plane
networks. Phase 1 jobs need no Docker daemon, PostgreSQL, XMPP, LLM, deployment,
or tenant access.

### 2. Configure project CI/CD settings

Open **Settings > CI/CD** and apply:

1. Disable Auto DevOps for this project.
2. Under **General pipelines**, keep **Use separate caches for protected
   branches** enabled. Never allow protected and unprotected refs to share one
   cache namespace.
3. Set the project build timeout to at least eight hours. Set Rust runner
   maximum timeout to at least two hours and protected `container-build` runner
   maximum timeout to at least eight hours; the lower project/runner limit wins.
4. Keep **Allow merge request pipelines to access protected variables and
   runners** disabled under **Variables**.
5. Add no secrets for phase 1. Jobs need no API keys, database URLs, signing
   keys, registry credentials, tenant tokens, or deployment credentials.
6. If local runner tags differ, add unprotected, unmasked project variables
   `CI_LINT_RUNNER_TAG` and `CI_RUST_RUNNER_TAG`. Their values must identify only
   unprivileged MR-safe runners.
7. Do not define `ALLOW_VERSION_CHECK_SKIP` persistently. A temporary value of
   `1` is valid only for a maintainer-approved exception paired with the
   `skip-version-check` MR label and a recorded reason.
8. Under **Job token permissions**, keep the inbound/outbound allowlist limited
   to this project unless one reviewed dependency needs broader access.

Runner cache credentials, if a shared cache backend is used, stay in runner
`config.toml`. Never expose them as merge-request CI variables. Merge requests,
tags, schedules, and manually started pipelines use pull-only caches; only the
`rust-cache-refresh` job on a protected default-branch push publishes compiler
cache state. Rust jobs report
before/after disk use, target/cache sizes, and sccache statistics. Cache only
the dependency paths declared in `defaults.yml`; do not add `target/`, tenant
data, environment files, or credentials.

### 3. Protect refs and merges

Open **Settings > Repository**:

1. Protect `master`.
2. Set **Allowed to merge** to Maintainers.
3. Set **Allowed to push and merge** to **No one** for direct pushes when the
   installed version offers that control; otherwise restrict direct push to
   Maintainers. Merge through reviewed MRs.
4. Under **Protected tags**, protect `v*` and allow creation only by Maintainers.
   The pipeline rejects malformed or unprotected release-looking tags.

Open **Settings > Merge requests > Merge checks**:

1. Enable **Pipelines must succeed**.
2. Enable **All discussions must be resolved** if that matches project policy.
3. Do not treat skipped required pipelines as successful.
4. Keep merged-result pipelines and merge trains disabled until this pipeline is
   separately validated in those modes.

Add a real owner group to `CODEOWNERS`, then require its review for
`.gitlab/**`, future release/package scripts, and
`ic/scripts/lunarwing-mt-admin.sh`. GitLab CE installations without enforceable
CODEOWNERS approvals must use protected `master` plus a documented Maintainer
review policy.

Create a protected `container-build` runner. Keep `privileged = false`, do not
mount a Docker socket, and permit only the user-namespace/mount syscalls required
by rootless BuildKit through a reviewed seccomp profile. Do not use
`seccomp:unconfined`. The runner must be unable to route to production or tenant
networks and must have at least an eight-hour maximum timeout: each candidate is
rebuilt once with `--no-cache` and promotion requires identical digests.

### 4. Validate and activate

Local includes cannot be fully linted until this branch exists on GitLab.

1. Push the implementation branch without merging it.
2. Open **Build > Pipeline editor > Validate** (CI Lint), select the branch, and
   validate the merged configuration. Inspect merged YAML, not only root syntax.
3. Simulate merge-request, branch, default-branch, schedule, and `v*` tag
   contexts when the installed CI Lint UI supports pipeline simulation.
4. Open a draft MR. Confirm all required jobs are assigned only to `lint` or
   `rust-x86_64` runners.
5. Push a second commit to the same MR. Confirm exactly one pipeline starts for
   that commit and no duplicate branch pipeline appears.
6. Confirm no MR job receives protected variables, uses a protected runner,
   mounts a Docker socket, or reaches tenant/production networks.
7. Confirm required jobs block merge. Observational jobs may fail only where
   documented in `.gitlab/ci/*.yml`.
8. Merge after CI Lint and one clean runner pipeline pass. Confirm the resulting
   default-branch pipeline starts.
9. Create a protected RC tag on that exact SHA. Confirm the complete feature,
   test, Cavepony, source-policy, secret, and cargo-deny gates appear. Trigger
   one worker candidate. It must finish an uncached second build with the same
   digest before its scan, SBOM, and promotion become reachable.
10. Confirm an ordinary branch, MR, web/API/trigger pipeline, and an unprotected
    tag receive no registry-writing jobs. Confirm re-running a candidate creates
    a new full-SHA/pipeline/job tag and cannot replace the first candidate.
11. Promote once and retain `worker-image-evidence.json`. Promotion writes a
    worker-specific tag such as `v2.1.0.0-rc1-kimi`; a second promotion to that
    tag must be idempotent for the same digest and fail if it points to different
    bytes. Tenant configuration must use the recorded digest, never `latest`.

### 5. Add the nightly observation schedule

Open **Build > Pipeline schedules > New schedule**:

- Description: `Nightly CI observation`.
- Target branch: `master`.
- Cron: `0 3 * * *`.
- Time zone: `UTC`.
- Variables: none.
- Owner: a maintained service/operator account, not a departing individual.

Run it once manually. Confirm `rust-clippy-all` and `architecture-boundaries`
appear as blocking jobs and `rust-msrv-check` and `secret-history` as
observational jobs. This schedule must not receive production secrets or use
deployment runners.

### 6. Activation evidence

Record these results in the MR before making the pipeline mandatory:

- Exact GitLab and GitLab Runner versions.
- Runner executor, tags, protected status, CPU/RAM/disk limits, and network
  policy.
- Confirmation that `limit = 1` is set in each Rust runner's `config.toml`
  (prevents CPU 0-5 contention between parallel Rust jobs on one host).
- CI Lint result and merged-YAML review.
- One MR commit with exactly one pipeline.
- One fully green required-job pipeline on clean runners.
- One default-branch pipeline and one manual schedule run.
- Confirmation that protected variables/runners were absent from MR jobs.
- Current durations and cache hit rates for each Rust job. The plan's Phase 1
  exit criteria (see `docs/proposals/GITLAB_CI_CD_PLAN.md`) require warm-cache
  p95 ≤ 30 min; record this so subsequent phases can compare.

Phase 1 scope and exit criteria are defined in
`docs/proposals/GITLAB_CI_CD_PLAN.md`; this README is the operator runbook.

## Local validation

```bash
python3 .gitlab/scripts/validate_config.py
bash .gitlab/scripts/shell-syntax.sh
bash .gitlab/scripts/install-shellcheck.sh && PATH="$PWD/.ci-bin:$PATH" bash .gitlab/scripts/shellcheck.sh
python3 .gitlab/scripts/python_syntax.py
python3 .gitlab/scripts/registry_json.py
taskset -c 0-5 cargo fmt --manifest-path ic/Cargo.toml --all -- --check
```

The shellcheck scan requires ShellCheck **0.11.0+** (the project floor since
MR !988 — Debian bookworm's apt 0.9.0 false-flags SC2218 on the mt-admin
harnesses). `install-shellcheck.sh` installs a checksum-pinned 0.11.0 into
`.ci-bin`; CI jobs do the same and prepend it to PATH. The installer needs
`curl`, `sha256sum`, and `xz` to unpack the upstream `.tar.xz` — CI images get
them from `bootstrap-shell.sh` (`xz-utils`).

GitLab CI Lint remains required. Generic YAML parsing cannot validate merged
GitLab semantics, runner availability, protected variables, or project-level
settings.
