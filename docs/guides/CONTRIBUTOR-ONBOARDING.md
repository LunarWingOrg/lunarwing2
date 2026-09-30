# LunarWing Contributor Onboarding

So you found LunarWing and want to contribute. Welcome. This document is the shortest
path from "I cloned the repo" to "my MR got merged" — and, just as importantly, it tells
you what *not* to touch right now.

This is an **onboarding** document. It sits below the canonical policies, not above them:

- **Policy of record:** [`CODE-CONTRIBUTION-POLICY.md`](CODE-CONTRIBUTION-POLICY.md) — style, review, security, and change discipline.
- **AI-generated code rules:** [`AI-CODE-CONTRIBUTION-POLICY.md`](AI-CODE-CONTRIBUTION-POLICY.md) — **read this before having any model write code for you.** Closed-weight model output is not accepted for source code. Effective 2026-09-15. No exceptions beyond those listed in the policy.
- **Agent-facing map:** [`AGENTS.md`](../../AGENTS.md) — where things live, build/test/lint commands, invariants. Human contributors should skim it too; it's the best architecture map in the repo.

If this document and the policy disagree, the policy wins and this document is stale. Please
file an issue.

---

## 1. The shape of the project (60 seconds)

- `ic/` is the core daemon — the product is **LunarWing**, the engine sources live here.
  Most of what the policy calls "high-risk" (engine, runtime, DB, secrets, gateway) is in here.
- WASM tools and channels are standalone sandboxes. They've historically been good first
  targets because they're isolated by design.
- Scripts, harnesses, and docs live at the repo root and under `docs/`.

When in doubt about where a change belongs, read [`AGENTS.md`](../../AGENTS.md) — specifically
"Where to Work" — before asking in IRC. It's almost certainly in there.

## 2. What's hot lava right now (freeze status)

**Current state (updated 2026-09-27, target release 2.1.3.0):**

- v2.1.2.0 (Yoroi) shipped 2026-09-25: the generic podman-first agent-shell
  sandbox, worker-contract hardening, health system parity, and
  confabulation hardening have landed. Normal review tiers apply to engine
  internals again — but `ic/src/agent/`, `ic/src/db/`, `ic/src/secrets/`,
  gateway, sandboxing, and approvals are still Tier 3 by default (§3).
- The v2.1.4.0 pre-release checklist is being worked through
  (`docs/ops/AGENT_GOALS_2.1.4.0.md`). Areas under active change include the
  crate/pin bumps (all LunarWing crates now 2.1.3, pending the 2.1.4 bump in
  checklist item #1; kimi worker 2.1.1; opencode2 v2.0.20), the WI#239
  new-tenant env defaults, and the open
  work-item audit. Check the checklist before opening MRs there.
- **Kawarimi export testing under OpenRC** is being re-worked due to an open-write detection
  difference vs systemd. OpenRC contributors *with kawarimi export expertise* are welcome →
  ask in `#lunarwing`.

We'll keep this updated — if the text above looks old, assume it is.

## 3. Good first contributions (the fast lane)

**Tier 1 — nearly always mergeable on a sane diff:**

- Documentation fixes and clarifications
- Test coverage improvements (especially around existing behavior)
- Shell/Python script hardening (`shellcheck` clean)
- Typo and consistency cleanups in non-code assets

**Tier 2 — valuable, needs a design conversation first:**

- New WASM tools or channels (sandboxed by construction)
- Additional integration/E2E coverage on existing flows
- Observability improvements that don't touch hot paths

**Tier 3 — the slow lane (on purpose):**

- Anything in `ic/src/agent/`, `ic/src/db/`, `ic/src/secrets/`, `ic/src/safety/`, gateway,
  sandboxing, approvals
- DB schema changes (must dual-implement PostgreSQL + libSQL)
- Anything that weakens an invariant listed in §10 of the policy

If you're not sure which tier your idea is: it's Tier 3 until a maintainer says otherwise.
That's not gatekeeping, it's how we keep the blast radius small.

## 4. What a mergeable MR looks like

- **Scope discipline.** One concern per MR. If your diff touches two unrelated files, it's
  two MRs. Review latency scales superlinearly with diff size.
- **Targeted tests.** Narrowest thing that validates the change. Don't refactor the world to
  add one unit test; don't skip tests because "it's obvious."
- **Policy compliance.** `cargo fmt --all -- --check`, full clippy zero-warnings,
  `cargo deny check`. All in CI, all blocking. Run them locally first.
- **Disclosure** of AI involvement is appreciated (not required for open-weight models —
  but it helps us calibrate review intensity). Closed-weight code generation is rejected
  per the AI policy. We mean this.

## 5. The review stack (set expectations)

The core review team is small. Reviews are done by **humans plus orchestrator agents**
(Ruffles and friends — yes, really). For day-one contributors:

- **Tier 1 diffs** go fast. Hours to a day.
- **Tier 2** goes through at least one design pass.
- **Tier 3** requires 2+ approvals and may sit for days while larger MRs
  clear the queue. **That's the correct behavior.** Please don't bump MRs.

During pre-release checklist crunches, even Tier 1 reviews may be slower than usual. Land
small, land patient.

## 6. Contributing as (or with) an agent

This is a project where agents are first-class contributors. Some realities if you're
driving one here:

- The AI policy applies to agent-generated code **exactly as** it applies to human-generated
  code. Closed-weight codegen is rejected, no matter how good the agent is.
- Agents should read [`AGENTS.md`](../../AGENTS.md) **before** doing anything else. It is the
  project contract.
- Don't open MRs from an agent without a human in the loop on *your* side. We do the same
  thing on our side; respect the symmetry.
- Agent-driven MRs that fail CI and sit untouched for 7 days will be closed. Reopen when
  you've iterated.

## 7. Privacy stance (translates to code)

LunarWing is privacy-first. In contributed code, that means:

- **No telemetry.** Not "anonymized," not opt-out, not "just crash pings." None.
- **No surprise network calls.** If a tool reaches out, the user asked for it explicitly.
- **No secret exfiltration pathways.** Ever. Full stop. Treat anything that moves secrets as
  a Tier 3 change requiring 2+ approvals.
- **Self-hosting is not optional.** Don't add hard dependencies on SaaS APIs when a
  self-hostable alternative exists.

Code that violates these won't be merged no matter how elegant it is.

## 8. Getting help

- IRC: `#lunarwing` on `irc.libera.chat:6697` (TLS)
- Architecture specs: `docs/architecture/`
- Subsystem specs: `src/*/CLAUDE.md` (inside `ic/`)
- Testing guide: `docs/guides/TESTING_GUIDE.md`

Ask before you build. We would rather tell you "bad idea" in a 30-second IRC reply than
have you find out after 6 hours of work.

---

*Last updated: 2026-09-27 (v2.1.3.0 freeze status). Update §2 whenever freeze status changes.*
