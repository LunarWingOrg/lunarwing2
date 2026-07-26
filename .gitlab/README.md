# GitLab CI/CD

This directory implements phase 1 of
`docs/proposals/GITLAB_CI_CD_PLAN.md`: unprivileged, secret-free merge request
CI plus visible nonblocking debt checks. Package publication and deployment are
intentionally absent. Current mt-admin upgrades rebuild per tenant and cannot
yet activate one verified CI artifact safely.

## Pipeline behavior

- Merge request pipelines replace branch pipelines once an MR is open.
- Branch, tag, schedule, API, trigger, and manual pipelines remain supported.
- Required jobs are interruptible and receive no production credentials.
- Cargo always runs through `scripts/cargo-ci.sh`, with CPU affinity `0-5` and
  at most two build jobs to bound peak memory. Rust test cases run serially
  because many mutate the process environment.
- PostgreSQL integration, WASM, browser, standalone-crate, package, release,
  and deployment jobs remain deferred until their prerequisites in the plan
  are fixed.

## Required runners

Create unprotected, unprivileged runners with these tags:

| Tag | Required capacity | Access |
|---|---|---|
| `lint` | Isolated Docker/Kubernetes executor; 2-4 CPUs, 4-8 GB RAM | Public package mirrors only; no protected variables or Docker socket |
| `rust-x86_64` | Isolated Docker/Kubernetes executor; 6 CPUs numbered `0-5`, 16 GB RAM, 25 GB free disk | Public Cargo/Git/package mirrors only; no protected variables or Docker socket |

Override `CI_LINT_RUNNER_TAG` or `CI_RUST_RUNNER_TAG` at project level if local
runner tags differ. Do not map either variable to protected or privileged
runners.

Pinned images:

- Rust 1.96.1 Bookworm OCI index (primary and MSRV jobs):
  `sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663`
- Python 3.12 slim Bookworm OCI index:
  `sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b`
- Gitleaks 8.30.1 OCI index:
  `sha256:c00b6bd0aeb3071cbcb79009cb16a60dd9e0a7c60e2be9ab65d25e6bc8abbb7f`
- cargo-deny 0.20.2 x86-64 musl archive:
  `sha256:9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f`

`ci-config` enforces Rust 1.96.1 across `ic/Cargo.toml` `rust-version`,
`ic/rust-toolchain.toml`, and both primary/MSRV image tags. It also requires
both Rust jobs to use same digest-pinned image and keeps README digest in sync.

## Required jobs

- CI composition, shell syntax, Python syntax, registry JSON, version policy,
  changed-line panic policy, rustfmt, and locked Cargo metadata.
- Protected release-tag syntax, release-note codename, and four-part to
  three-part Cargo version mapping.
- Default, PostgreSQL-only, libSQL-only, and all-feature `cargo check` jobs.
- Workspace library tests and hermetic libSQL tests.
- Explicit onboarding, web, Kawarimi, and DarkIRC Python suites with zero skips.
- Fixture-only shell harnesses for mt-admin rendering, build policy, and health.
- `python-unit`, `shell-harnesses`, and `cavepony-wasm` (cavepony version assert
  + `wit_compat` test) are all required jobs; failures block merge.
- cargo-deny and redacted current-tree Gitleaks scans.
- One Rust job per runner host: `limit = 1` in runner `config.toml` is a hard
  constraint, verified in the activation checklist (§6). Parallel jobs contend for
  CPUs 0-5 with no in-pipeline guard.
- CI parallelism is intentional: `-j2` (libSQL `-j1`) caps peak memory within
  the 16 GB runner for four-way check jobs; `-j6` in AGENTS.md is the local-dev
  budget. Lower debug info for libSQL link jobs avoids OOM near 16 GB.

Known false-positive Gitleaks findings are listed by exact fingerprint in
`/.gitleaksignore`. Broad path or rule exclusions are forbidden.

`skip-version-check` is inert unless a maintainer also supplies
`ALLOW_VERSION_CHECK_SKIP=1`; commit messages cannot bypass version policy.

## Observational jobs

These run on default-branch and scheduled pipelines with `allow_failure: true`:

- `rust-msrv-check`: becomes blocking once the declared MSRV (1.96.1) is confirmed
  green, closing the plan §15.5 reconciliation.
- `rust-clippy-all`: becomes blocking after a clean-runner baseline passes.
- `architecture-boundaries`: becomes blocking after current heuristic and real
  violations are fixed.
- `shellcheck`: becomes blocking after current warnings reach zero.
- `secret-history`: scheduled only; becomes blocking after historical findings
  are reviewed.

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
- Capacity: six available CPUs numbered `0-5`, 16 GB RAM, and at least 25 GB
  free disk.
- Per-host job limit: one. Every Cargo job pins itself to CPUs `0-5`; parallel
  Rust jobs therefore require separate runner hosts or VMs, not overlapping
  jobs on one host.
- Maximum job timeout: at least 60 minutes.

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
3. Set project build timeout to at least 60 minutes. Set each Rust runner's
   maximum timeout to at least the same value.
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
`config.toml`. Never expose them as merge-request CI variables. Cache only the
dependency paths declared in `defaults.yml`; do not add `target/`, tenant data,
environment files, or credentials.

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

### 5. Add the nightly observation schedule

Open **Build > Pipeline schedules > New schedule**:

- Description: `Nightly CI observation`.
- Target branch: `master`.
- Cron: `0 3 * * *`.
- Time zone: `UTC`.
- Variables: none.
- Owner: a maintained service/operator account, not a departing individual.

Run it once manually. Confirm `rust-clippy-all`, `architecture-boundaries`,
`shellcheck`, and `secret-history` appear as observational jobs. This schedule
must not receive production secrets or use deployment runners.

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
python3 .gitlab/scripts/python_syntax.py
python3 .gitlab/scripts/registry_json.py
taskset -c 0-5 cargo fmt --manifest-path ic/Cargo.toml --all -- --check
```

GitLab CI Lint remains required. Generic YAML parsing cannot validate merged
GitLab semantics, runner availability, protected variables, or project-level
settings.
