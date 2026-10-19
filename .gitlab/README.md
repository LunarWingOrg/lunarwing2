# GitLab CI/CD

This directory implements unprivileged, secret-free merge request validation
plus protected worker-image candidate publication. Production package and tenant
deployment remain absent: current mt-admin upgrades rebuild per tenant and
cannot yet activate one verified CI artifact safely.

## Pipeline behavior

- Merge request pipelines replace branch pipelines once an MR is open.
- Branch, tag, schedule, API, trigger, and manual pipelines remain supported.
- MRs targeting the default branch **or** `aragents/kimil00p/*` run the full
  required validation matrix, including backend checks/tests, Clippy,
  architecture boundaries, Cavepony WASM, and cargo-deny. Do not use a reduced
  feature-branch pipeline as proof that integration will pass. An ordinary
  scratch branch can request that same matrix with the non-secret pipeline
  variable `LUNARWING_CI_FULL_VALIDATION=1` (for example, in **Run pipeline**).
  This flag does not enable protected worker publication or cache writes.
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
Job images are declared through the `CI_LINT_IMAGE`, `CI_RUST_IMAGE`,
`CI_GITLEAKS_IMAGE`, `CI_BUN_IMAGE`, and `CI_NODE_IMAGE` variables in `defaults.yml`; `ci-config` expands those
variables before checking pins, so overrides must stay digest-pinned too.
`CI_DEBIAN_SNAPSHOT` (same file) is not an image but is tied the same way:
it must equal the valued `ARG DEBIAN_SNAPSHOT` in
`kimicode4lunarwing/Dockerfile`, and the `worker-kimi-ts` lane installs its
python3 prerequisite from that snapshot archive instead of the live
deb.debian.org mirror.

### Rustup component downloads and runner DNS

The pinned Rust image uses Rustup's **minimal** profile. Its `rustfmt` and
`cargo-clippy` commands are Rustup proxies, which exist even when those components
are absent. `bootstrap-rust.sh` calls `ensure-rust-components.sh` to execute each
component's `--version` through the pinned toolchain, install missing components,
and verify them before any Cargo job enters `ic/`. Otherwise the components in
`ic/rust-toolchain.toml` trigger an implicit download during the Cargo command.

Component installation gets three attempts with 5s/10s backoff for temporary
network failures; an unusable component still fails the job. This does not retry
compilation or make any required check optional. Preinstalled components need no
download. Runners need working DNS and HTTPS access to `static.rust-lang.org` for
missing components and explicit WASM target installs.

Pipeline #4708's required `rust-check-libsql` job #86164 failed on the `laptop`
runner while resolving that host, before any compilation. A persistent failure
needs runner DNS/egress repair and a retry of the failed check **and** its skipped
`rust-test-libsql` dependent; a green retry of the check alone is insufficient.

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
   and likewise for `CI_RUST_IMAGE` / `CI_GITLEAKS_IMAGE` / `CI_BUN_IMAGE`. Runners
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

Concurrency is now enforced by runner eligibility and host-side limits. The
operator confirmed that the fleet's runner configuration limits which jobs
memory-constrained machines accept. The temporary project-wide compile lock
has been removed so independent runners can work concurrently, including across
pipelines. This applies to the Rust checks, tests, Clippy, MSRV, and cavepony.

On a 16 GB host, keep one heavy compile at a time (`limit = 1` for a dedicated
Rust runner); larger hosts may use their configured capacity. Account for all
runner registrations sharing a host when setting limits. If OOMs recur, inspect
that host's eligible jobs and aggregate concurrency before retrying the job.

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

## worker-kimi digest mismatch (directory mtime flake)

Symptom — `worker-kimi-candidate` fails with:

```
clean rebuild digest mismatch: sha256:... != sha256:...
```

and the layer-level diagnosis shows a single differing layer (the npm CLI
install layer), e.g. pipelines 5158, 5263, 5390, 5481, 5734. Retrying the job
usually goes green, which masks the defect.

Root cause: the CLI install `RUN` does not deterministically modify the
`/usr/local` directory itself, so `rewrite-timestamp=true` records either the
base image mtime (untouched run) or the clamped `SOURCE_DATE_EPOCH` (touched
run) for that one tar entry. All file contents are identical — only the
directory mtime races. Since 2026-10-15 the Dockerfile pins the directory
mtime explicitly (`touch -d "@${SOURCE_DATE_EPOCH}" /usr/local` at the end of
the `RUN`), so both builds record the same value.

If a new single-layer mismatch appears, fetch the two blobs named by the
diagnosis from the registry and diff their contents/metadata the same way
before assuming real nondeterminism; directory mtimes of install prefixes are
the prime suspect, and the fix is the same normalization in the offending
`RUN`.

## Required jobs

- CI composition, shell syntax, shellcheck, Python syntax, registry JSON, version
  policy, changed-line panic policy, rustfmt, and locked Cargo metadata.
- Protected release-tag syntax, release-note codename, and four-part to
  three-part Cargo version mapping.
- `worker-opencode-ts`: the five OpenCode worker Bun self-tests (including
  bridge-to-executor control-channel regressions) and strict `tsc --noEmit`.
  Runs on every MR/branch push and protected release tag to catch cross-file
  protocol drift; it intentionally has no path-only `changes:` filter.
  Dependencies use `bun install --frozen-lockfile`; this MR validation job
  publishes no shared cache, preserving the protected-cache-writer policy.
  Override `CI_BUN_IMAGE` with a mirror of the pinned digest to avoid
  Docker Hub quotas. Worker image rebuilds remain a separate protected gate.
  `validate_config.py` pins this lane's script line-for-line (MR !1644
  review HN-5), so any script edit must update the validator's
  `WORKER_TS_LANE_SCRIPTS` tuple in the same commit.
- `worker-kimi-ts`: the kimi worker Node/TS suites (bridge, executor, tool
  proxy relay, MCP shim, runtime, entrypoint/hook policy) via the
  kimicode4lunarwing Dockerfile builder stage's `npm ci --ignore-scripts` +
  `npx tsc` + `npm test` sequence. The lane first installs the builder
  stage's python3 apt prerequisite — the bare `CI_NODE_IMAGE` node image
  lacks python3 and the entrypoint config tests bash-run `entrypoint.sh`,
  whose config section embeds python3 heredocs — from the pinned
  `CI_DEBIAN_SNAPSHOT` archive (tied to the Dockerfile `ARG DEBIAN_SNAPSHOT`
  by `validate_config.py`), not the live deb.debian.org mirror. That archive
  is a best-effort, throttled service, so the lane's `apt-get update` and
  `apt-get install` both run with
  `Acquire::Retries=5`/`Acquire::http::Timeout=60`; a snapshot outage
  still fails this required lane until the service recovers (retry the job).
  Runs on every MR/branch
  push and protected release tag on the digest-pinned `CI_NODE_IMAGE`, which
  `validate_config.py` ties to the node FROM digest(s) in
  `kimicode4lunarwing/Dockerfile` (fail-closed, like the bun tie). The tie
  follows the FROM repository's last path segment, so `docker.io/node`, a
  mirror path and the bare `node` spelling are all cross-checked while a
  `nodejs`-style name still fails closed (MR !1669 review IK-1). The tie is
  line-based: a FROM split by a line continuation, or a `FROM ${VAR}` whose
  `ARG` default drifted, is untied — the same textual-limit class the apt
  source rules record below (MR !1691 run-137 review IX-6). The
  real-CLI contract lane (`kimi_cli_contract_test`) self-skips here — the
  image carries no kimi binary — so the protected candidate image build
  remains the only place that contract is asserted against the real CLI.
  Like the opencode lane, the script is pinned line-for-line by
  `validate_config.py` (`WORKER_TS_LANE_SCRIPTS`, MR !1644 review HN-5):
  any script edit must update the tuple in the same commit.
  The pinned worker TS lanes also have an allowlist of job keywords after
  `extends` resolution (`WORKER_TS_LANE_KEYS`, MR !1647 review HP-1) and
  require exactly `tags: ["$CI_LINT_RUNNER_TAG"]`. Job-level `variables`,
  `parallel` matrices and `hooks` are refused, including inherited entries:
  they can bypass the pinned script without editing a command. Adding any
  other keyword (including `timeout`, `retry`, `inherit` or `after_script`)
  requires reviewing and updating the allowlist in the same commit.
  The same bypasses are refused when they arrive outside the job block
  (MR !1647 review HP-2, MR !1658 review IA-1..IA-7): the validator merges
  the CI files the way GitLab does — includes in order, the root file last
  with a deep merge — so a root-file entry for a lane (or a root-file
  `CI_NODE_IMAGE`/`CI_BUN_IMAGE` variable override, in string or
  `value:`/`description:` hash form, which likewise fails the digest pins)
  cannot replace the checked definition unseen; the workflow is read from
  the same merged view; an inherited `default.before_script`,
  `default.hooks` or deprecated top-level `before_script` is refused; every
  top-level variable must be in `GLOBAL_VARIABLE_ALLOWLIST` (a name denylist
  cannot enumerate the environment routes — `NODE_OPTIONS`, `npm_config_*`,
  `BUN_OPTIONS`, `PATH` — so adding a global variable means editing the
  allowlist in the same commit); `workflow:rules` and lane `rules` must not
  set `variables`; and root include entries must be plain `local:` files
  that do not themselves `include` anything, because nested or conditional
  includes are merged by GitLab but invisible here. `extends` chains resolve
  up to a depth budget of 11; a longer chain or a cycle fails closed instead
  of being certified from the keys visible at truncation. The runner tag
  variables' values are pinned too (`EXPECTED_RUNNER_TAGS`, MR !1658 review
  IA-9): an override of `CI_LINT_RUNNER_TAG`/`CI_RUST_RUNNER_TAG` reroutes
  the pinned jobs to another runner with the `tags:` lists untouched, so a
  deliberate runner change edits `defaults.yml` and the validator table in
  the same commit — and no job may name a runner tag variable in a job-level
  or rule-level `variables:` block either, since that overrides the pinned
  value for just that job (MR !1665 review IF-2). The release audit's
  advisory gates (`cargo-deny`, `satellite-advisory-sweep`,
  `lockfile-only-advisory-guard`, `default-graph-advisory-guard`) must not
  be `allow_failure` at any level: each gate must exist, carry exactly
  `.full-validation-context`'s rules (whose entries may carry only `if`,
  plus the final `when: never`), set no job-level `when`, and run on exactly
  the pinned runner tag — a rule-level `allow_failure`, a replaced rules
  list, a deleted gate, or a retag all fail the same way (MR !1665 review
  IF-1). Each gate's extends-resolved `script` and `before_script` are
  pinned exactly too, and only a fixed key set (`after_script`,
  `before_script`, `cache`, `image`, `needs`, `rules`, `script`, `stage`,
  `tags`) may survive extends resolution — a gate that never runs its
  checker (`exit 0` in the script or the job's or `.rust-job`'s
  `before_script`, `|| true` on the wrapper, an echoed wrapper name) is as
  optional as an `allow_failure` one, and job-level `variables`, `hooks`,
  `inherit`, `parallel` or `timeout` can hollow it out just as well, so a
  deliberate gate or `.rust-job` bootstrap change updates the pinned tuples
  in the same commit (MR !1680 review IP-1). The allowed inputs are also
  pinned by `ADVISORY_GATE_PINNED_VALUES`: the `$CI_RUST_IMAGE` image, the
  exact pull-only Rust caches, and only `cargo-metadata` with
  `artifacts: false` — blocking both checkout-overwriting artifacts and
  higher-precedence dotenv variables from upstream jobs (MR !1688, IU-1).
  `secret-history` is the marked, deliberate exception while
  historical findings are triaged. Three limits of the YAML validator
  remain, by design: it cannot see repo files the pinned scripts still
  honour — a project `.npmrc`, a `bunfig.toml` `preload`, or the
  `package.json` `test` script in the worker script directories stay trust
  surface reviewed in the diff, not pinned here (MR !1658 review IA-9);
  project-, group- or pipeline-level overrides of the `CI_*` variables
  themselves are allowed (the `defaults.yml` comment documents mirroring
  the pinned images to the project registry for Docker Hub quota) and
  invisible to a static tie (MR !1644 review HN-7); and a `sed`-style
  rewrite of the stamp inside `/etc/apt/sources.list` that never names
  `DEBIAN_SNAPSHOT` — or a source URL built from variables — is invisible
  to any line-based rule (MR !1665 review IF-4, MR !1680 review IP-2); the
  structural backstop would be an in-image assertion after the apt step,
  which runs only at the protected image build. What the tie does cover is every in-repo build entrypoint: a
  `DEBIAN_SNAPSHOT` build-arg or env reference in `build-worker-image.sh`,
  mt-admin's `05-tenant-build.sh`, `tests/docker-compose.test.yml`, or
  `kimicode4lunarwing/docker-compose.yml` is refused, and every
  `DEBIAN_SNAPSHOT` mention in `kimicode4lunarwing/Dockerfile` must be a
  plain `ARG` declaration or a `${DEBIAN_SNAPSHOT}` snapshot URL, with every
  apt source URI on every line — `deb`/`deb-src` with or without a
  bracketed options list under any apt URL scheme (`http(s)`,
  `mirror+http`, `tor+https`, `file:`), and every URI of a deb822 `URIs:`
  field — matched case-insensitively (printf-escaped separators included)
  and pointing at snapshot.debian.org, so one line carrying a snapshot
  source beside a live mirror fails too (MR !1644
  review HN-6, broadened by MR !1665 review IF-3 and MR !1680 review IP-2).
  YAML the validator cannot parse at all (e.g. a `!reference` tag, which
  `yaml.safe_load` rejects) fails closed with a clean one-line error naming
  the file, not a traceback (MR !1647 review HP-5).
- `worker-opencode2-ts`: the opencode2 worker's dependency-free Bun suites
  (`path_expand_test.ts`, `task_prompt_test.ts`, `opencode_result_test.ts`).
  The executor suite needs the image's built `@opencode/client` and stays
  gated at the (protected) worker image build. Runs on the same digest-pinned
  `CI_BUN_IMAGE` as the v1 lane on every MR/branch push and protected release
  tag, and is pinned by the same `WORKER_TS_LANE_SCRIPTS` machinery (WI#375
  DR-3), so a script edit must update the tuple in the same commit. The
  `CI_BUN_IMAGE` tie cross-checks both `opencode4lunarwing/Dockerfile` and
  `opencode2.4lunarwing/Dockerfile` (MR !1669 review IK-2), so a drifted bun
  digest in either worker image fails `ci-config`.
- `worker-pebble-rust`: the pebble worker's Rust suite via
  `bash .gitlab/scripts/cargo-ci.sh test -j2 --locked --manifest-path
  ../pebble4lunarwing/Cargo.toml`, resolving under ic's vendored context on
  every MR/branch push and protected release tag (previously gated only at
  the protected image build; WI#375 DR-3). The lane extends `.rust-job` for
  the toolchain bootstrap and pull-only cache; `validate_pebble_rust_lane`
  pins the extends, the resolved `$CI_RUST_IMAGE` image and
  `$CI_RUST_RUNNER_TAG` tag, the merge_request_event rule, and the exact
  script, and refuses job-level `variables`/`hooks`/`parallel`/
  `before_script`/`after_script`/`image`/`tags` overrides of the audited
  template. The same holds at the template level (MR !1669 review IK-8): the
  extends-resolved job may carry only the audited `.rust-job` keys, and its
  inherited `before_script` must equal the pinned bootstrap line for line —
  a template `variables`/`hooks`/`inherit`/`parallel` entry or an `exit 0`
  appended to the shared bootstrap would otherwise hollow the lane out. The
  image value is pinned whole (MR !1691 review IX-1): an inherited
  `.rust-job` image `entrypoint` (or any other image key) fails the lane,
  not just a changed image name. Every worker lane must also declare
  `needs: []` exactly (MR !1691 review IX-2): without it a later-stage lane
  receives every earlier-stage job's artifacts and dotenv variables, which
  outrank the allowlisted YAML variables (`NODE_OPTIONS`/`BUN_OPTIONS`/
  `CARGO_TARGET_<triple>_RUNNER` would turn the pinned tests into no-ops).
  Any
  script edit must update `PEBBLE_RUST_LANE_SCRIPT` (and a deliberate
  bootstrap change `PEBBLE_RUST_LANE_BEFORE_SCRIPT`) in the same commit.
- Default and all-feature `cargo check` jobs plus `--tests` compilation for
  PostgreSQL-only and libSQL-only feature shapes.
- Workspace library tests, hermetic libSQL tests, and a non-skipping PostgreSQL
  workspace integration target against disposable pgvector/PostgreSQL 16.
- Explicit onboarding, web, Kawarimi, and DarkIRC Python suites with zero skips.
- Fixture-only shell harnesses for mt-admin rendering, build policy, and health.
- `python-unit`, `shell-harnesses`, and `cavepony-wasm` (cavepony version assert
  + `wit_compat` test) are all required jobs; failures block merge.
- `ci-config` cheaply checks Cavepony's registry, capabilities, Cargo package,
  local lockfile entry, and CI version expectation together. The WASM build
  uses `--locked`; its runtime compatibility job still requires a fresh
  artifact and the existing 10-million-fuel budget.
  The host capabilities-schema test derives Cavepony's expected version from
  its Cargo manifest, so valid tool releases do not leave a stale version
  literal in `rust-test-lib`; WIT and no-host-capability assertions remain.
  Agreement alone does not satisfy `version-policy`: any change under a
  tool's source directory, including metadata or lockfile corrections, also
  needs a registry version newer than `CI_MERGE_REQUEST_DIFF_BASE_SHA`.
  Run `.gitlab/scripts/version-policy.sh` with the actual MR base before push.
- Blocking ratchets: `rust-clippy-all` (all-features, `-D warnings`) and
  `architecture-boundaries` (changed-line hard violations only; printed
  full-tree debt counts stay informational). Both run in the full-validation
  context since goals 2.1.1.0 item 23.
- The changed-line panic policy (`panic-policy` job) checks `ic/src` and
  `ic/crates` only; test bodies are excluded via the `#[test]`-attribute and
  `mod tests` walk in `ic/scripts/check_no_panics.py`, and cargo-fuzz targets
  under `fuzz/fuzz_targets/` are skipped outright as test-only harness code.
- `standalone-deliverables` verifies locked Cargo metadata for every unit
  selected for that job in `.gitlab/deliverables.json`. Every non-vendor
  lockfile must be classified: Cargo roots belong in `deliverables`, while
  lock-only worker overrides belong in `build_inputs` (no invented sibling
  Cargo.toml). Build inputs must name a production consumer, the Dockerfile
  that copies them, and both `ci-config` and worker-candidate coverage;
  unclassified locks still fail validation. Pebble's patched upstream lock is
  one such input. `ci-config` checks its reviewed SHA256 default and runs
  `.gitlab/tests` regressions for the inventory and full package-record guard.
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
- cargo-deny runs unvendored: `.gitlab/scripts/cargo-deny-check.sh` executes
  from the repo root with an explicit `--manifest-path ic/Cargo.toml` so
  ic/'s vendored source replacement cannot hide the crates.io index. Run from
  inside ic/, the `yanked = "deny"` policy silently degraded to per-crate
  `warning[index-failure]` while the job still passed, and those diagnostics
  also pushed pipeline 4811's trace past GitLab's 4 MB log cap, hiding the
  verdict. The wrapper hides inclusion graphs and fails the job on any
  `index-failure` diagnostic, so the gate can never be silently skipped;
  `validate_config.py` rejects a cargo-deny job that bypasses it.
- The cargo-deny wrapper and installer tolerate transient runner egress
  flakes (pipeline #5294's cargo-deny job went red on a tree identical to
  the green MR pipeline minutes earlier). `install-cargo-deny.sh` retries
  the checksum-pinned GitHub release download three times (5s/10s backoff,
  checksum verified outside the retry loop), and `cargo-deny-check.sh`
  retries the audit up to three times with a 600s `timeout` per attempt —
  but accepts only a completed run with zero `index-failure` diagnostics,
  and never retries a run that reached a real verdict (`error[...]`), so
  the yanked and advisory gates keep their semantics. The retry contract
  is pinned by hermetic stubbed tests in
  `.gitlab/tests/test_cargo_deny_gate.py` (`CargoDenyRetryTests`).
- The cargo-deny gate queries `--workspace --all-features` (a superset of
  every CI compile graph — the `--workspace` lib-test job plus the
  root-package `--all-features` check/clippy jobs — including the vendored
  libsignal path deps); a root-package default query misses crates such as
  openssl and `lunarwing_health_metrics`. Two companion full-validation jobs
  complete the advisory surface (see `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`):
  `satellite-advisory-sweep` runs
  `.gitlab/scripts/check-satellite-advisories.sh`, which discovers every
  tracked `Cargo.lock` via `git ls-files` and applies the pinned cargo-deny
  `check advisories bans` with the shared `ic/deny.toml` policy to each manifest
  root (licenses/sources stay owned by the ic workspace gate), and
  `lockfile-only-advisory-guard` runs
  `.gitlab/scripts/check-lockfile-only-advisories.sh` for pins cargo-deny
  cannot see (per-entry `absent`/`no-dev` modes; an entry whose crate leaves
  the lockfile fails as stale).
- WI#418 scopes the libSQL advisory acceptances with bans: only the exact
  legacy h2/rustls/webpki pins may remain below the modern version floors,
  and only libsql may depend directly on hyper-rustls 0.25.0. The
  `default-graph-advisory-guard` also rejects literal `libsql-remote-replica`
  or `--all-features` flags in the four shipped daemon build paths, and fails
  if a listed path disappears. This textual check is supplemented by a
  per-release review of effective flags and newly added build paths; it
  cannot resolve variable indirection.
- Rust concurrency is host-managed: memory-constrained runners restrict eligible
  jobs and concurrent compiles; larger hosts can accept parallel work.
- CI parallelism is intentional: `-j2` (libSQL `-j1`) caps peak memory within
  the 16 GB runner for four-way check jobs; `-j6` in AGENTS.md is the local-dev
  budget. Normal test jobs retain line tables only; libSQL test jobs omit debug
  info entirely to avoid OOM near 16 GB.
- Rust jobs have no project-wide resource lock. Worker promotion retains its
  per-worker resource groups to serialize registry publication.

Known false-positive Gitleaks findings are listed by exact fingerprint in
`/.gitleaksignore`. Broad path or rule exclusions are forbidden. Worker-image
CRITICAL findings that Debian has not fixed for the pinned toolchain
dependencies are listed by exact CVE with a rationale comment and an expiry
date in Trivy's own syntax (`CVE-… exp:YYYY-MM-DD`, one entry per line) in
`.gitlab/trivy-ignore`. Trivy's ignore-file parser only recognises the `exp:`
prefix and stops suppressing an entry on its date (00:00 UTC), so expired
entries fail the scan gate again until the finding is re-reviewed (WI#361:
the earlier `expiring:` spelling was never Trivy syntax, so those entries
silently never expired). The scan runs with `--config /dev/null`, so neither
a checkout-root `trivy.yaml` nor any un-allowlisted `TRIVY_*` variable can
hide an unfixed CRITICAL behind `ignore-unfixed`. `validate_config.py`
rejects any non-comment line in another shape and any expiry more than six
months out.

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
- For the 16 GB host profile below, allow one heavy Rust job at a time.
  Higher-capacity hosts may accept more according to their configured memory
  budgets. Cargo jobs pin themselves to CPUs `0-5`, so also account for CPU
  contention when sizing concurrency.
- Maximum job timeout: at least eight hours for the protected
  `container-build` runner and at least two hours for Rust runners. The project
  timeout must be at least eight hours; GitLab applies whichever project/runner
  limit is lower. Ordinary validation jobs still use their two-hour YAML cap.

Example Docker executor settings for the 16 GB host profile belong in runner-host
`config.toml`, not in this repository:

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
cache state. On those pushes it also serves as the default-feature compile gate:
`rust-check-default` is omitted, and `rust-test-lib`/`cavepony-wasm` wait for
`rust-cache-refresh` instead. Other contexts run `rust-check-default`. This
avoids checking the same feature set twice
without allowing merge-request jobs to write caches. Rust jobs report
before/after disk use, target/cache sizes, and sccache statistics. Cache only
the dependency paths declared in `defaults.yml`; do not add `target/`, tenant
data, environment files, or credentials.

The cache policy validator checks merged, extends-resolved definitions:
validation caches must have literal `policy: pull` (including inherited and
default caches), `cache:unprotect` is forbidden, and the sole writer's rules
must match the protected default-branch push predicate exactly, with no
job-variable overrides. Push-only, implicit `pull-push`, variable policies,
and an extra MR condition on the writer are refused (MR !1688, IU-3).

`bootstrap-sccache.sh` verifies the pinned SHA256 of the sccache **binary**
before accepting a cache hit or executing a new installation, in addition to
verifying the release archive. A matching `--version` string is not trusted.
Rejected cached binaries are removed before downloading, and a failed binary
check removes the installation so `after_script` cannot execute it. Verified
cache hits stay offline; rejected entries require a verified download (IU-2).
Both Rust templates also use this helper's `--show-stats` mode in
`after_script`: it rechecks the same binary pin before execution, removes
rejected entries, and skips missing binaries without downloading. This
protects telemetry even if an earlier bootstrap step fails before the normal
sccache verification runs (IZ-1).

**Compiler-cache trust limit (IU-3):** the binary pin does not authenticate
compiled objects in `.ci-cache/sccache/`. The checked-in pipeline permits
writes only from `rust-cache-refresh` on protected default-branch pushes;
MRs and unprotected target branches read only. This still trusts the cache
backend, protected writers, and GitLab's separate protected/non-protected
cache setting above, which the YAML validator cannot verify. Discard caches
produced under weaker policies before relying on cached test results.

### Worker scheduling and pipeline latency

Worker candidates use `needs: []`: on protected default-branch/release-tag pushes,
play `worker-<name>-candidate` immediately, without waiting for the Rust matrix.
Its scan runs automatically using that candidate's artifacts; there is no second
manual scan step. On `master` these lanes remain optional. On release tags the
candidate and scan remain blocking, and promotion remains manual and requires
both. A failed candidate may still cause an optional scan to fail for missing
artifacts; finish/retry the candidate first, then retry the scan if needed.
Canceled candidates cannot produce a usable scan or promotion.

Each candidate also records `worker-inputs.sha256`: the reviewed per-worker
list of build-read files (the Dockerfile, lockfiles, and every script the
build executes — CY-4 precedent) hashed into the promotion evidence in
`worker-image-evidence.json`. `validate_config.py` requires every listed
entry to exist under the repo root and, for the kimi lane, to cover the
fixed build-read set plus every on-disk `kimicode4lunarwing/scripts/*.ts`
(the Dockerfile's `COPY scripts/*.ts` glob), so a new bridge source must
join the list in `build-worker-image.sh` in the same commit. The kimi core
set includes the shipped `health_server.py` and `agent_comm_protocol.json`
(MR !1670 IL-1). Every worker block must assign `input_files` exactly once,
with no assignments outside the blocks (IL-3); `export`, `declare`,
`readonly` and `+=` assignment lines also count, so a later assignment
cannot replace the checked list before hashing.

The former `rust-compile-16gb` lock serialized heavy Rust work across pipelines:
for example, pipeline #3812 waited while #3811 ran its backend tests. That lock
is now removed; ready jobs can run concurrently on eligible runners within their
configured limits. Container builds also bypass the Rust stage barrier, and the
default-feature check runs only once per pipeline.
Existing pipelines retain their original job graph; the new scheduling applies
to pipelines created from the updated configuration.

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
- Confirmation that runner eligibility and aggregate host concurrency fit each
  host's memory budget (`limit = 1` for the dedicated 16 GB Rust profile)
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
python3 -m unittest discover -s .gitlab/tests -p 'test_*.py'
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
