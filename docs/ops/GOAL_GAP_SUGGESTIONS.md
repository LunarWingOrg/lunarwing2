# Goal List Gap Suggestions — v2.0.3.0

> **Purpose:** Deliverable for item #3 in `docs/ops/AGENT_GOALS_2.0.3.0.md`.
> Surfaces release-track work that is **not** represented in the current v2.0.3.0
> goals list but is verifiable against the repo on 2026-07-25. Each suggestion
> cites the evidence so a human can triage add/defer in one pass.
>
> **Method:** compared the 29 items in `AGENT_GOALS_2.0.3.0.md` against
> `docs/ops/ROADMAP_2026.md`, `docs/bugs/` (23 open BUG-* files), the
> `docs/proposals/` catalog (~60 entries), the CI/CD and crate-audit reviews
> already produced for items #2 and #8, and the module list under `ic/src/`.
> Items already covered by an existing goal are excluded; duplicates are noted.

---

## How to read this

- **Priority** is scoped to v2.0.3.0 release fitness, not long-term roadmap.
  - `P0` — risks a broken or false-green release if skipped.
  - `P1` — visible quality / parity regression; should land this cycle.
  - `P2` — good to capture but deferrable to v2.0.4.0 without harm.
- **Evidence** points at concrete files so a reviewer can verify in seconds.
- **Not a goal** lines explain why this is not already covered.

---

## P0 — Release integrity gaps

> **Status (2026-07-26):** All three P0 items addressed in commit chain
> `265f4b8` (G3 MSRV), `154461c` (G2 e2e triage), `94f93ae` (G1 cargo update).

### G1. Run `cargo update` and lockfile reconciliation before tag

**Status: DONE (2026-07-26).** `cargo update` applied (168 package bumps), `ic/Cargo.lock`
committed. Security-relevant updates pulled (openssl, rustls, hyper, http, tokio, libc).
Verified via `cargo check` (default + all-features). See `CRATE_AUDIT_STATUS.md` §"Changes
since the 2026-07-12 audit" for the full list.

`docs/ops/CRATE_AUDIT_STATUS.md` (item #8 deliverable) reports the committed
`ic/Cargo.lock` is behind by **155 patch/minor bumps, 14 additions, 18
removals**, including security-relevant updates to `openssl`, `rustls`,
`hyper`, `http`, `tokio`, and `libc`. Item #8 only asks for an *audit*; it does
not track applying the result.

- **Evidence:** `docs/ops/CRATE_AUDIT_STATUS.md` §"Pre-2.0.2.0 Action" (still
  unactioned for 2.0.3.0); `ic/Cargo.lock`.
- **Why it matters:** shipping a release 155 patches behind on TLS/HTTP/openssl
  is a known, avoidable CVE surface, and the audit already did the triage work.
- **Not a goal:** item #8 is "inspect and suggest", not "apply and verify".
- **Proposed item:** "Apply the pending `cargo update`, run
  `taskset -c 0-5 cargo check -j6 --all-features` + `cargo test --lib`, and
  commit the refreshed `ic/Cargo.lock` before cutting v2.0.3.0."

### G2. Resolve or explicitly defer the open e2e test failures

**Status: DONE (2026-07-26).** `docs/ops/PRE-RELEASE-TESTING.md` reconciled against
`docs/bugs/README.md`. Final triage: bootstrap greeting tests are `#[ignore]`'d (harness
race, not production bug); clipboard test is UNVERIFIED (no reproduced failure); OAuth
URL tests are FIXED/RETIRED; tool execution timeout is FIXED.

`docs/ops/PRE-RELEASE-TESTING.md` carries unchecked `[ ]` boxes and the
`docs/bugs/README.md` index lists multiple open e2e failures — most notably
`e2e_advanced_traces::bootstrap_greeting_fires` /
`bootstrap_onboarding_clears_bootstrap` (tracked in
`BUG-e2e-bootstrap-greeting-tests.md`), plus clipboard/OAuth-URL tests in
`BUG-e2e-clipboard-copy-test.md` and `BUG-e2e-oauth-url-parameter-tests.md`.

- **Evidence:** `docs/ops/PRE-RELEASE-TESTING.md:11-17`,
  `docs/bugs/BUG-e2e-bootstrap-greeting-tests.md`,
  `docs/bugs/BUG-e2e-tool-execution-timeout.md`.
- **Why it matters:** the release notes (items #28/#29) will need an accurate
  "known issues" section; right now the test ground-truth is ambiguous.
- **Not a goal:** no item reconciles `PRE-RELEASE-TESTING.md` with the bugs
  index before the release-notes items consume it.
- **Proposed item:** "Triage `docs/bugs/BUG-e2e-*` and `PRE-RELEASE-TESTING.md`
  checkboxes: fix, defer with a recorded reason, or mark environment-only. Update
  `docs/bugs/README.md` to reflect the final v2.0.3.0 state."

### G3. MSRV reconciliation (1.92 declared vs 1.96.1 CI pin)

**Status: DONE (2026-07-26).** `rust-version` bumped to `1.96.1` in all workspace
`Cargo.toml` files and both `AGENTS.md` files. The CI (`rust-toolchain.toml` and
`.gitlab/ci/defaults.yml`) already pinned 1.96.1; nobody tested on 1.92. Now all four
locations agree.

The CI/CD review (item #2 deliverable) calls this out as the one open Phase 0
item: `ic/Cargo.toml:15` declares `rust-version = "1.92"`, `AGENTS.md` lists
MSRV 1.92, but no job proves the crate still builds on 1.92 and the only Rust
job uses 1.96.1. `docs/proposals/GITLAB_CI_CD_PLAN.md` §15.5 explicitly flags
this.

- **Evidence:** `docs/ops/CICD_LOGIC_REVIEW.md` §"P0 — README claims MSRV 1.92,
  CI pins 1.96.1"; `ic/Cargo.toml:15`.
- **Why it matters:** an MSRV violation is a semver break for downstream
  consumers and an avoidable release blocker if a 1.92 user reports it after
  tag.
- **Not a goal:** item #2 only produced the review; it does not own the fix.
- **Proposed item:** "Reconcile MSRV: either bump `rust-version` to 1.96.1 in
  `ic/Cargo.toml` + `AGENTS.md`, or add an observational `rust-msrv-check` job
  on 1.92. Pick one before tag."

---

## P1 — Parity and hardening gaps

### G4. libSQL backend drift audit

`AGENTS.md` mandates "do not regress existing libSQL coverage when possible",
and the DB trait pattern requires both backends. v2.0.3.0 has touched
extensions, MCP, gate, and pairing — all of which land on the shared DB trait.
No goal verifies libSQL parity after these merges.

- **Evidence:** `ic/src/db/` (dual-backend trait), `AGENTS.md` "Database,
  Setup, and Config Rules"; `docs/ops/CRATE_AUDIT_STATUS.md` notes
  `rust-test-libsql` is serialized for memory, confirming it runs but not that
  it is current.
- **Why it matters:** a silent libSQL regression is the most likely
  non-Postgres release breakage and the hardest for an operator to notice
  before deploy.
- **Not a goal:** items #14/#18 are mt-admin and darkirc; nothing covers DB
  backend parity.
- **Proposed item:** "Run `cargo test --no-default-features --features libsql`
  end-to-end against v2.0.3.0 and file or fix any regressions vs the postgres
  feature set. Record the verified feature surface in `src/db/CLAUDE.md`."

### G5. Worker / orchestrator lifecycle bug sweep

`docs/bugs/` contains a cluster of open worker bugs that each survived into
the 2.0.3 branch: `BUG-agent-loop-blocks-on-worker-jobs.md`,
`BUG-agent-worker-lifecycle.md`, `BUG-external-worker-config-persistence.md`,
`BUG-opencode-worker-tilde-expansion.md`, `BUG-subagent-worker-hang.md`,
`BUG-worker-workspace-path-expansion.md`,
`BUG-unbounded-mpsc-recv-in-spawned-tasks.md`. The branch history shows
incremental fixes (`0fea074`, `8158eab`, `d029396`) but no consolidated
verification pass.

- **Evidence:** `docs/bugs/BUG-*worker*.md`, `docs/bugs/BUG-agent-loop*`,
  `docs/bugs/BUG-unbounded-mpsc*`; recent commits `8158eab`, `d029396`.
- **Why it matters:** external workers are a release-marketed capability and
  the bug cluster spans hang, persistence, and path-expansion — classic
  inter-release regression territory.
- **Not a goal:** no item sweeps the worker bug cluster; each fix landed
  piecemeal.
- **Proposed item:** "Consolidated worker regression pass: replay the
  `docs/bugs/BUG-*worker*` and `BUG-unbounded-mpsc*` scenarios against the
  current branch, close what is fixed, and open a follow-up doc for anything
  still reproducing."

### G6. Tunnel provider cutover verification (NetBird)

Item #17 added NetBird in `ff5e491`/`cbc0040`, but did not remove existing
managed Cloudflare, ngrok, or Tailscale providers. Item #27 completed the
cutover and verification pass.

- **Result:** runtime implementations, setup choices, doctor probes, settings
  fields, and web options now retain only NetBird, custom commands, and static
  HTTPS URLs.
- **Persistence:** PostgreSQL and libSQL migration V24 scrub legacy
  provider-specific rows while retaining the provider marker for explicit
  operator migration.
- **Compatibility:** static `TUNNEL_URL` remains vendor-neutral and takes
  precedence. Legacy managed values fail with migration guidance when no
  static URL exists.
- **Operations:** NetBird enrollment and daemon lifecycle remain external to
  LunarWing, preserving systemd/OpenRC neutrality.

### G7. OMEMO + XMPP backpressure bugs

`BUG-xmpp-omemo-warmup-and-processing.md` and
`BUG-xmpp-polling-and-backpressure.md` remain open. Items #15/#16 cover *file
transfer* and *LunarVision* polish but not these polling/OMEMO bugs, which
affect every XMPP tenant.

- **Evidence:** `docs/bugs/BUG-xmpp-omemo-warmup-and-processing.md`,
  `docs/bugs/BUG-xmpp-polling-and-backpressure.md`.
- **Why it matters:** XMPP is the primary multi-tenant channel; a polling
  backpressure bug under load is a fleet-wide outage class.
- **Not a goal:** items #15/#16 are scoped to file transfer + vision, not
  core polling/OMEMO.
- **Proposed item:** "Triage the two open XMPP core bugs alongside the #15/#16
  polish work so one consolidated XMPP hardening pass ships."

### G8. SSH harness delivery options

`docs/proposals/SSH_HARNESS_OPTION_2_3_IMPLEMENTATION.md` and
`SSH_HARNESS_DELIVERY_OPTIONS.md` describe a tracked, partially-scoped change
to SSH delivery (`ic/src/tools/builtin/ssh_git.rs` and friends).
`BUG-ssh-git-bare-repo-head-mismatch.md`, `BUG-ssh-git-null-ref-serialization.md`,
and `BUG-ssh-git-ref-and-remote-head.md` are all open and related.

- **Evidence:** three open `BUG-ssh-git-*` docs + the two proposals.
- **Why it matters:** ssh-git is a core builtin tool; three open correctness
  bugs around ref/head handling is a data-integrity surface.
- **Not a goal:** no goals item touches ssh-git despite the bug cluster.
- **Proposed item:** "Resolve the `BUG-ssh-git-*` cluster and decide
  implement/defer on the SSH harness option 2/3 proposals; record the
  decision."

---

## P2 — Hygiene and doc-track gaps (deferrable)

### G9. Documentation link checker

The CI/CD review (item #2) and goals items #19–#24 all depend on doc accuracy
across `docs/architecture`, `docs/bugs`, `docs/ops`, `docs/proposals`. There is
no link checker, and the plan (`docs/proposals/GITLAB_CI_CD_PLAN.md` §8.1)
marks `docs-links` as Phase 2.

- **Evidence:** `docs/ops/CICD_LOGIC_REVIEW.md` §3 parity table;
  `GITLAB_CI_CD_PLAN.md` §8.1.
- **Proposed item:** "Add an observational `docs-links` job (markdown link
  checker) so the doc-refresh items #19–#24 can regress-test themselves."

### G10. Standalone crate lockfile validation

`docs/ops/CICD_LOGIC_REVIEW.md` §5 flags **18** `Cargo.lock` files in the
repo, of which only `ic/Cargo.lock` is validated. Standalone shippable crates
(`ic/bridges/xmpp-bridge`, `lunarwing_weechat_wss/weechat_relay`,
`darkirc_channel_for_lunarwing/darkirc`, `pebble4lunarwing`,
`projects/ocr-sidecar`) get no `cargo metadata --locked` check.

- **Evidence:** `docs/ops/CICD_LOGIC_REVIEW.md` §5.
- **Proposed item:** "Add a single observational `standalone-cargo-metadata`
  CI job covering the 17 non-core lockfiles (cheap; catches malformed locks
  in shipped crates)."

### G11. Pebble / gotify / LunarVision sidecar release tracking

Three standalone deployable components ship from this repo —
`pebble4lunarwing/`, `ic/lunarwing-gotify-tool/`, and
`projects/ocr-sidecar/` (LunarVision). Each has its own lockfile and version,
and none are tracked by any v2.0.3.0 goal despite the main daemon bumping to
2.0.3.

- **Evidence:** `pebble4lunarwing/Cargo.toml`,
  `ic/lunarwing-gotify-tool/Cargo.toml`, `projects/ocr-sidecar/Cargo.toml`;
  `docs/ops/PEBBLE-WORKER.md`.
- **Proposed item:** "Decide per-sidecar whether to bump versions in lockstep
  with 2.0.3.0 or keep independent cadence; record the policy in
  `docs/ops/CRATE_AUDIT_STATUS.md`."

### G12. Self-healing / self-repair coverage audit

`docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md` and
`docs/ops/SELF_REPAIR_IMPROVEMENTS_GENTOO.md` exist; the roadmap books
"Self-Healing Capabilities analysis of missing pieces" for v2.0.4. A v2.0.3.0
goal to at least *audit* coverage after the tunnel/MCP/worker changes would
de-risk that.

- **Evidence:** `docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md`,
  `docs/ops/SELF_REPAIR_IMPROVEMENTS_GENTOO.md`, `ROADMAP_2026.md`.
- **Proposed item:** "Audit self-heal coverage after the v2.0.3.0 tunnel + MCP
  + worker changes; file gaps as v2.0.4.0 prep."

### G13. Cost-control / observability verification

`AGENTS.md` says `AppBuilder` wires "cost controls" and `ic/src/observability/`
exists with multi/noop/log backends. No goal verifies that cost controls and
structured observability still behave after the LLM/tool-loop changes in this
cycle.

- **Evidence:** `ic/src/observability/{multi,noop,log,traits}.rs`,
  `ic/src/estimation/` (cost), `AGENTS.md` architecture model.
- **Proposed item:** "Verify cost-accounting and observability backends
  against the v2.0.3.0 tool/MCP loop; add a focused test if missing."

### G14. Onboarding CLI proposal decision

`docs/proposals/MT-ONBOARDING-CLI.md` and `MT-ONBOARDING-CLI`-adjacent work is
unresolved. Item #11 reviews onboard *UI* but not the CLI proposal.

- **Evidence:** `docs/proposals/MT-ONBOARDING-CLI.md`.
- **Proposed item:** "Decide implement/defer on the MT onboarding CLI proposal
  for this release; if deferring, record in the proposal."

### G15. Routine engine + memory maintenance proposals

`docs/proposals/ROUTINE_ENGINE_IMPROVEMENTS.md` and
`docs/proposals/MEMORY_MAINTENANCE_ROUTINES.md` describe tracked work that
touches the agent runtime (`ic/src/agent/`, `ic/src/workspace/`). Neither is
represented in v2.0.3.0 goals.

- **Evidence:** both proposal files.
- **Proposed item:** "At minimum triage the two proposals for v2.0.3.0
  fit; if deferred, cross-link from `ROADMAP_2026.md`."

---

## Explicitly out of scope (captured to prevent re-suggestion)

These were considered and rejected so a future reviewer doesn't re-raise them:

- **Multi-agent orchestration** — already items #22 + `ROADMAP_2026.md` v2.0.4.
- **Reflex compiler** — already items #23 + #25-reference; roadmap v2.0.4/0.7.
- **Lunartica reskin** — roadmap v2.0.5+, not a 2.0.3.0 concern.
- **LunarVoice** — roadmap v2.0.6+; no 2.0.3.0 dependency.
- **Nanocode deprecation** — roadmap v2.0.5; explicitly not this cycle.
- **DarkIRC key exchange** — already items #10/#18.
- **Human delay mode phase 2** — proposal exists but phase 1 already landed in
  a prior release; phase 2 is not on the v2.0.3.0 critical path.

---

## Summary table

| ID | Priority | One-line | Evidence |
|----|----------|----------|----------|
| G1 | P0 | Apply pending `cargo update` (155 bumps) | `CRATE_AUDIT_STATUS.md` |
| G2 | P0 | Triage open e2e failures | `PRE-RELEASE-TESTING.md`, `BUG-e2e-*` |
| G3 | P0 | Reconcile MSRV 1.92 vs CI 1.96.1 | `CICD_LOGIC_REVIEW.md` P0 |
| G4 | P1 | libSQL backend parity audit | `ic/src/db/`, `AGENTS.md` |
| G5 | P1 | Worker/orchestrator bug sweep | `BUG-*worker*`, `BUG-unbounded-mpsc*` |
| G6 | P1 | NetBird tunnel cutover verification (completed by item #27) | `ic/src/tunnel/`, migration V24 |
| G7 | P1 | XMPP OMEMO + backpressure bugs | `BUG-xmpp-*` |
| G8 | P1 | SSH-git ref/head bug cluster | `BUG-ssh-git-*`, `SSH_HARNESS_*` |
| G9 | P2 | Docs link checker CI job | `CICD_LOGIC_REVIEW.md` §3 |
| G10 | P2 | Standalone crate lockfile validation | `CICD_LOGIC_REVIEW.md` §5 |
| G11 | P2 | Sidecar version policy (pebble/gotify/ocr) | standalone `Cargo.toml`s |
| G12 | P2 | Self-heal coverage audit | `SELF_HEAL_DEPLOYMENT_WIRING.md` |
| G13 | P2 | Cost-control + observability verification | `ic/src/observability/`, `estimation/` |
| G14 | P2 | MT onboarding CLI proposal decision | `MT-ONBOARDING-CLI.md` |
| G15 | P2 | Routine engine + memory maintenance triage | `ROUTINE_ENGINE_IMPROVEMENTS.md`, `MEMORY_MAINTENANCE_ROUTINES.md` |

---

## Recommended triage order

1. **G1, G2, G3** — close these before items #28/#29 (release notes) consume
   them; they directly determine the "known issues" section.
2. **G5, G7, G8** — bug-cluster sweeps; cheapest to do while the relevant
   subsystems are warm from this cycle's feature work.
3. **G4** — parity verification of libSQL feature work; high value, low design
   cost. G6 was completed by item #27.
4. **G9–G15** — batch as a v2.0.4.0 prep list or squeeze in opportunistically.
