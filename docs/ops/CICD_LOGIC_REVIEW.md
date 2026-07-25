# CI/CD Logic Review — LunarWing GitLab CI

> **Purpose:** Review the current `.gitlab/` CI/CD logic and surface concrete
> improvement suggestions. This document is the deliverable for item #2 in
> `docs/ops/AGENT_GOALS_2.0.3.0.md`. It is **review only**; it does not change
> CI files.
>
> **Scope reviewed on 2026-07-25:** `.gitlab-ci.yml`, `.gitlab/ci/*.yml`,
> `.gitlab/scripts/*`, `.gitlab/README.md`, and the originating plan
> `docs/proposals/GITLAB_CI_CD_PLAN.md`. All findings are verified against the
> current tree at that date.
>
> **Overall assessment:** The implemented Phase 1 pipeline is well-structured,
> secret-free, properly DAG'd, digest-pinned, and faithful to the plan. The
> items below are targeted hardening, parity fixes, and low-risk gaps — not a
> rearchitecture.

## 1. Current pipeline summary (verified)

- **Stages:** `validate` → `check` → `test` → `security` → `observe`.
- **Entry:** `.gitlab-ci.yml` includes six local files under `.gitlab/ci/`.
- **Workflow rules** suppress duplicate branch pipelines once an MR exists
  (`.gitlab/ci/workflow.yml`).
- **Required (blocking) jobs:** `ci-config`, `shell-syntax`, `python-syntax`,
  `registry-json`, `version-policy`, `panic-policy`, `rustfmt`,
  `cargo-metadata`, four-shape `cargo check` matrix, `rust-test-lib`,
  `rust-test-libsql`, `python-unit`, `shell-harnesses`, `cavepony-wasm`,
  `cargo-deny`, `secret-detection`.
- **Observational (`allow_failure`) jobs:** `rust-clippy-all`,
  `architecture-boundaries`, `shellcheck`, `secret-history`.
- **Toolchain pin:** Rust **1.96.1** Bookworm, digest-pinned
  (`.gitlab/ci/defaults.yml:54`).
- **Cargo discipline:** every Cargo invocation routes through
  `.gitlab/scripts/cargo-ci.sh`, which enforces `taskset -c 0-5` and `cd ic/`.
- **Secret hygiene:** Gitleaks scans a `git archive` of the exact commit;
  no production variables are declared; runners are documented as
  unprivileged and unprotected.

This is a solid Phase 1. The following sections list issues by priority.

---

## 2. Priority findings

### P0 — Stale README image digest vs. actual image (must fix)

`.gitlab/README.md:121` instructs operators to configure the runner
`config.toml` with:

```
image = "docker.io/library/rust:1.92-bookworm@sha256:e90e846…"
```

but the pipeline actually runs on Rust **1.96.1** Bookworm
(`.gitlab/ci/defaults.yml:54`, hash `a339861a…`). An operator following the
README runbook would provision a runner whose default image does not match
the jobs, and the hardcoded `cpuset_cpus = "0-5"` example silently assumes a
CPU layout this project relies on elsewhere.

**Why it matters:** the README is the operator runbook (`.gitlab/README.md`
§"Required runners"). A mismatched pinned image defeats the digest-pinning
guarantee the pipeline relies on.

**Suggested fix:**
1. Update the `config.toml` example in `.gitlab/README.md` to the same
   `rust:1.96.1-bookworm@sha256:a339861a…` image used in `defaults.yml`.
2. Reconcile the "Pinned images" list in the same README (it still lists the
   Rust 1.92 hash at line ~37) so all three locations agree.
3. Consider adding a tiny assertion job (`image-consistency`) that greps the
   README image hashes against `defaults.yml` so this drift cannot recur.

### P0 — README claims MSRV 1.92, CI pins 1.96.1, no MSRV verification

`ic/Cargo.toml:15` declares `rust-version = "1.92"` and `AGENTS.md` lists
MSRV 1.92, but the only Rust job uses 1.96.1. Nothing in CI proves the crate
still builds on its declared MSRV. The plan explicitly flags this
(`docs/proposals/GITLAB_CI_CD_PLAN.md` §15.5: "Pin one supported Rust
toolchain and reconcile 1.92 versus 1.96 docs/builds").

**Suggested fix:** add one scheduled (non-MR) `rust-msrv-check` job using
`rust:1.92-bookworm` that runs `cargo check --locked` on the default feature
set. Keep it observational until green, then make it required on the default
branch. This is low-cost and directly closes the reconciliation item from the
plan.

### P1 — Parallel Cargo jobs can overlap on `0-5` within one runner host

`cargo-ci.sh` pins `taskset -c 0-5`, and the README correctly notes a per-host
job limit of one Rust job. However that constraint lives only in the README,
not in `.gitlab/ci/defaults.yml` or any `concurrency:`/`resource_group:`
declaration. If an operator runs two `rust-x86_64` jobs concurrently on one
host (e.g. misconfigured `limit = 2`), the two `taskset -c 0-5` processes will
 contend for the same six cores with no detection.

**Suggested fix:**
- Add a `resource_group:` (e.g. `rust-x86_64-host`) to the `.rust-job`
  template, or document that the runner's `config.toml` `limit = 1` is a hard
  requirement and is validated during activation evidence (`.gitlab/README.md`
  §"Activation evidence").
- At minimum, add the `limit = 1` constraint to the validation checklist in
  the README rather than leaving it as prose.

### P1 — `validate_config.py` does not detect this class of drift

`validate_config.py` checks image digest pinning and direct `cargo` usage, but
does not check that the image digest referenced in `README.md` matches the one
in `defaults.yml`. The P0 image drift above would not be caught by CI.

**Suggested fix:** extend `validate_config.py` (or a new sibling check) to
extract the Rust image digest from `defaults.yml` and assert the same digest
appears in `.gitlab/README.md`. This is a ~15-line addition and closes the
gap permanently.

### P2 — Four-shape `cargo check` matrix runs serially per host

The four `rust-check-*` jobs each `need: cargo-metadata` only, so they are
designed to run in parallel. Good. But because each rebuilds from a cold
target dir on a single-host runner (README says one job per host), in practice
they serialize and each pays a full compile. The cache key
(`defaults.yml:62`) is keyed only on `ic/Cargo.lock`, shared across all four
feature shapes, and the cache paths include only the cargo registry — **not**
`target/`. So warm-cache behavior depends entirely on registry cache hits,
not on compiled artifacts.

**Suggested fix (low priority, measure first per plan §15 Phase 1 exit
criteria):**
- The plan deliberately excludes sharing writable `target/` across concurrent
  jobs (§10.1). Keep that rule. Instead, consider runner-local `sccache` with
  a project namespace (plan §10.1) to cut recompile without sharing `target/`.
- Record actual warm-cache p95 (plan exit criterion: ≤30 min) before adding
  complexity.

### P2 — `rust-test-libsql` sets `-j1` and `CARGO_PROFILE_TEST_DEBUG=0` but
the comment lives in YAML, not in `cargo-ci.sh`

`rust.yml:89-91` lowers debug info and serializes rustc for the libSQL link
because it approaches the 16 GB runner limit. This is correct, but the
rationale is captured only as a YAML comment. If `cargo-ci.sh` ever grows a
default `-j` flag or the runner RAM changes, the special-case is easy to miss.

**Suggested fix:** either move the memory rationale into a named CI variable
(e.g. `LIBSQL_LINK_MEM_CONSTRAINED: "1"`) consumed by `cargo-ci.sh`, or add a
one-line note in `.gitlab/README.md` §"Required jobs" describing the libSQL
link constraint. Documentation-only; no behavior change.

### P2 — `wasm-channel-test-artifacts` rebuilds WASM in two places

`rust.yml:33-48` builds the xmpp and weechat_relay WASM channels and exposes
them as artifacts consumed by `rust-test-lib`/`rust-test-libsql`. Good — this
implements the plan's "build once, reuse" rule for WASM. However
`cavepony-wasm` (`integration.yml:18-30`) separately builds the cavepony tool
WASM from a different manifest path and does **not** publish it as an artifact
for reuse. If any later job needs the cavepony wasm it would rebuild.

**Suggested fix:** if no downstream job currently needs the cavepony wasm,
document that `cavepony-wasm` is intentionally self-contained. If a future job
needs it, lift it into the shared artifact producer. No change needed today.

---

## 3. Parity gaps vs. the plan (Phase 1 scope)

These are items the plan lists under Phase 1 that are **not yet** implemented.
They are tracked here for visibility; most are intentionally deferred and
documented as such in `.gitlab/README.md`. Confirm each is a deliberate defer
rather than an oversight.

| Plan item (§) | Status in pipeline | Note |
|---|---|---|
| 8.1 `python-static` (Pyright/Ruff) | Not present | Plan marks "Phase 2 blocking". OK to defer. |
| 8.1 `docs-links` (internal Markdown link check) | Not present | Plan marks "Phase 2 blocking". Consider adding; `docs/` is large and link rot is visible in other review tasks (e.g. goals items #19–#24). |
| 8.3 PostgreSQL integration job | Not present | Plan defers to Phase 2 with pgvector service. Confirmed deferred in README. |
| 8.5 Path-triggered standalone crates matrix | Not present | Eight standalone lockfiles exist (see §5 below). Currently the core `ic/Cargo.lock` does not validate them. Plan defers to Phase 2; reasonable, but see suggestion in §5. |
| 8.6 Strict WASM (`build-wasm-extensions.sh --strict`) | Partial | `wasm-channel-test-artifacts` builds two channels directly; the strict-mode flag described in the plan is not exercised. |
| 15.5 Toolchain reconciliation (1.92 vs 1.96) | **Unresolved** | See P0 above — this is the one Phase 0 item still visibly open. |

---

## 4. Security review of the CI logic

Reviewed `.gitlab/scripts/*.sh` and `*.py` for the runtime invariants in
`AGENTS.md` (no secret exposure, no privileged escalation, no tenant access):

- **`scan-secrets.sh`** scans a `git archive` of `CI_COMMIT_SHA` (or `HEAD`)
  into a temp dir with a trap cleanup. Correct: it does not scan the working
  tree (which could include uncommitted secrets) and redacts at 100. Good.
- **`install-cargo-deny.sh`** downloads a checksum-pinned musl binary, runs
  `sha256sum --check`, installs to `$CI_PROJECT_DIR/.ci-bin`. Clean.
- **`bootstrap-rust.sh`** hard-pins `rustc 1.96.1` and exits nonzero on
  mismatch. Good supply-chain hygiene.
- **`cargo-ci.sh`** is minimal and correct: fails if `taskset` is missing,
  `cd`s to `ic/`, execs cargo. No injection surface.
- **`validate_config.py`** rejects unpinned images and direct `cargo`
  invocations. Strong guardrail.
- **No `systemctl`/`rc-service`** usage anywhere in `.gitlab/scripts/` — the
  multi-tenant hard rule is respected. `shell-harnesses.sh` invokes
  `ic/scripts/tests/*.sh` and mt-admin test scripts, which is the correct
  abstraction path.

**One observation:** `secret-detection` runs on every pipeline, while
`secret-history` (full `--all` history scan) is scheduled-only and
observational. This is the correct split — current-tree is blocking, history
is advisory until triaged. No change needed; recorded as correct.

No security weaknesses found in the CI logic itself.

---

## 5. Standalone crate coverage observation

There are **18** `Cargo.lock` files in the repo (verified), of which only
`ic/Cargo.lock` is validated by `cargo-metadata` and used as a cache key.
Standalone deployable crates with their own lockfiles — e.g.
`ic/bridges/xmpp-bridge/Cargo.lock`,
`lunarwing_weechat_wss/weechat_relay/Cargo.lock`,
`darkirc_channel_for_lunarwing/darkirc/Cargo.lock`,
`pebble4lunarwing/Cargo.lock`, `projects/ocr-sidecar/Cargo.lock` — are not
format-checked or metadata-validated by any current job.

**Suggestion (Phase 2, low cost):** add a single observational
`standalone-cargo-metadata` job that runs `cargo metadata --locked --no-deps`
against each standalone lockfile, so a malformed lock or a stale version in a
shipped crate is caught before release. This is cheaper than the full
path-triggered matrix in plan §8.5 and catches the most common failure.

---

## 6. Documentation / metadata suggestions

- `.gitlab/README.md` §"Pinned images" lists Rust 1.92 and cargo-deny 0.20.2;
  `defaults.yml` uses Rust 1.96.1. Reconcile (P0 above).
- The "Required jobs" list in the README does not mention `cavepony-wasm`,
  `python-unit`, or `shell-harnesses` by name in its prose summary, though
  they are required. Consider enumerating them for operator clarity.
- `docs/proposals/GITLAB_CI_CD_PLAN.md` is the authoritative design; the
  README is the runbook. Add a one-line cross-reference from the README to
  the plan's Phase 1 exit criteria so operators know "what done means".

---

## 7. Recommended next actions (ordered)

1. **(P0)** Reconcile `.gitlab/README.md` Rust image digest with
   `defaults.yml` (1.96.1, hash `a339861a…`).
2. **(P0)** Add an observational MSRV check job on Rust 1.92 to close plan
   §15.5.
3. **(P1)** Extend `validate_config.py` to assert README image digest ==
   `defaults.yml` image digest, preventing future drift.
4. **(P1)** Make the one-Rust-job-per-host constraint explicit (README
   checklist or `resource_group`).
5. **(Phase 2)** Add `standalone-cargo-metadata` observational job for the 17
   non-core lockfiles.
6. **(Phase 2)** Add a `docs-links` job; several other goals items depend on
   doc accuracy.
7. *(Defer)* PostgreSQL integration, package/release, and CD remain correctly
   gated behind their plan prerequisites; do not advance without the mt-admin
   artifact-activation work.

---

## 8. What is already good (do not regress)

- DAG via `needs:` is correct; no job blocks on more than it must.
- `interruptible: true` + workflow rules prevent wasted runner time.
- Every image is digest-pinned; `validate_config.py` enforces it.
- `cargo-ci.sh` centralizes the `taskset -c 0-5` rule — no direct `cargo`
  calls leak through (and the validator catches any that do).
- Observational jobs each carry an `Owner:` line and a removal condition in
  the YAML, matching the plan's "no permanent `allow_failure`" rule.
- No secrets, no Docker socket, no tenant access in any MR job — consistent
  with `AGENTS.md` security invariants.
- Gitleaks uses exact fingerprints in `/.gitleaksignore`; broad rule
  exclusions are forbidden (per README). Good discipline.
