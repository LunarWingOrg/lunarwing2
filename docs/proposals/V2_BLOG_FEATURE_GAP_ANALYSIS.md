# v2 Blog Feature Gap Analysis

> Compiled 2026-07-12 (item #5, AGENT_GOALS_2.0.0.0).
>
> Cross-references every feature mentioned in the LunarWing v2 announcement
> blog post (`blog.lunarwing.org/2026/07/12/lunarwingv2-the-next-frontier-of-private-self-hosted-ai-agents/`)
> against `docs/ops/AGENT_GOALS_2.0.0.0.md` and `docs/ops/ROADMAP_2026.md`.
>
> **Goal:** identify features the blog promises that are NOT yet tracked in
> either document, so they can be planned and accounted for before release.

---

## Summary

| Blog Feature | In AGENT_GOALS? | In ROADMAP? | Status |
|---|---|---|---|
| lunarwing-websocket-1 protocol | Partial (legacy drop only, item #6) | No | **Gap** |
| MCP Overhaul | Yes (item #18) | Yes (v2.0.2/0.4/0.6) | Covered |
| LunarVoice — Runtime Speech | No | Yes (v2.0.4/0.6/0.7) | **Gap in GOALS** |
| Lorebook Support | No | Yes (v2.0.2) | Covered |
| Agent & User Profile Switching | No | Yes (v2.0.2) | Covered |
| Agent Profile Enhancements | No | Yes (v2.0.2) | Covered |
| Workspace Seeding Improvements | No | Yes (v2.0.2) | Covered |
| Preseeded Memory Routines | No | No | **Gap** |
| Interactive Secret Management | Yes (item #8, MT web) | No | Shipped (1.1.9) |
| DarkIRC Key Exchange | Yes (item #15) | No | Covered |
| Reflex Compiler v2 | No | Partial (v2.0.5 "polishing") | **Gap (scope mismatch)** |
| Self-Healing Expansion | No | Yes (v2.0.2) | Covered |
| ONNX Support | No | Yes (v2.0.4) | Covered |
| Web UI Overhaul | Yes (item #7, done) | No | Shipped |
| Localhost Web-Based Onboarding | Yes (item #8) | No | Shipped |
| Crate Updates | Yes (items #3, #10) | Yes (v2.0.2) | Covered |
| Instinct Over Turns paradigm | No | Vague (v2.0.4 Hermes subset) | **Gap** |

**5 gaps identified.** Details below.

---

## Gaps

### 1. lunarwing-websocket-1 Protocol (Migration & Naming)

**Blog says:** The legacy `ironclaw-websocket-1` protocol is retired. v2
introduces `lunarwing-websocket-1`. The database secret has been renamed with
migrations seeded in 1.1.9.

**Current tracking:** AGENT_GOALS item #6 tracks *dropping* the legacy
`ironclaw-agent-v1` subprotocol offer and removing compat symlinks. ROADMAP
v2.0.1 tracks dropping legacy acceptance from external workers. Neither
document tracks the **new protocol name**, its migration path, or verification
that the renamed secret is functioning end-to-end.

**What's missing:**
- Explicit tracking of the `lunarwing-websocket-1` protocol name adoption.
- Verification that existing tenants' migrated secrets work with the new name.
- Documentation update for the worker protocol spec (pebble, opencode,
  nanocode containers all need to negotiate the new protocol name).

**Recommendation:** Add an item to AGENT_GOALS verifying the protocol rename
is complete across the daemon, all worker containers, and documentation.
ROADMAP v2.0.1's legacy-drop item should reference the new name explicitly.

---

### 2. LunarVoice — Missing from AGENT_GOALS

**Blog says:** Agents can speak. Runtime speech support is a headline v2
feature.

**Current tracking:** ROADMAP_2026.md tracks LunarVoice across three releases:
- v2.0.4: ONNX Runtime speech-to-text model support
- v2.0.6: Two-way voice communication
- v2.0.7: Additional polishing

However, **AGENT_GOALS_2.0.0.0.md has zero mention of LunarVoice.** Since
AGENT_GOALS is the pre-release checklist for 2.0.0.0, and LunarVoice ONNX
support targets v2.0.4 (post-2.0.0.0), this may be intentional — LunarVoice
may not land in the initial 2.0.0.0 release. But the blog positions it as a
headline v2 feature, so its absence from the release checklist should at
least be acknowledged with a "deferred to v2.0.4+" note.

**Recommendation:** Add a note to AGENT_GOALS confirming LunarVoice is tracked
in ROADMAP and intentionally deferred past 2.0.0.0.

---

### 3. Preseeded Memory Routines

**Blog says:** "Routines that come pre-configured. Agents start with useful
behaviors and schedules, not just potential."

**Current tracking:** **Not tracked in either AGENT_GOALS or ROADMAP.** No
existing proposal covers this either. The existing routine system
(`src/agent/routines/`) supports user-created routines, but the concept of
shipping pre-built, opinionated default routines is new.

**What's missing:**
- Design: which routines ship by default? (memory consolidation? daily
  summary? skill review? stale-pattern eviction?)
- Implementation: how are default routines registered (hardcoded vs.
  workspace-seeded vs. DB-seeded)?
- User control: can users disable/modify preseeded routines?

**Recommendation:** Create a proposal document for preseeded routine design.
Add a ROADMAP entry (likely v2.0.2 or v2.0.3, alongside workspace seeding
improvements).

---

### 4. Reflex Compiler v2 — Scope Mismatch

**Blog says:** "The next version of the reflex compiler. Tweaks,
improvements, and partial rewrites where needed. Not a full rewrite — an
evolution." The blog frames this as a significant version bump.

**Current tracking:** ROADMAP v2.0.5 says only "Reflex Compiler polishing
and improvements." This significantly understates the scope described in the
blog ("partial rewrites where needed" implies structural changes to the
compiler, not just polish).

**What's missing:**
- A breakdown of what specifically gets rewritten vs. polished.
- Whether the reflex pattern format changes (breaking change for existing
  reflex patterns).
- Performance/correctness targets for the v2 compiler.

**Recommendation:** Expand the ROADMAP entry or create a proposal detailing
the reflex compiler v2 scope. If the scope is genuinely just polish, update
the blog framing. If partial rewrites are planned, the ROADMAP entry should
reflect that.

---

### 5. "Instinct Over Turns" / Persistent Internal Drives

**Blog says:** "The most exciting shift in v2 isn't any single feature. It's
a fundamental rethinking of what an agent *is*." The blog describes agents
with persistent internal drives (inspired by Nous Research's work on
biological instinctiveness), self-improvement checks that fire constantly
within the agent loop (Hermes model), and positions this as **"the major
focus of v2."**

**Current tracking:**
- ROADMAP v2.0.4: "Feature set of subset of agent features adopted from Hermes
  Agent (Human Delay mode already landed); comprehensive documentation to
  accompany each new feature."
- AGENT_GOALS item #16: "memory_impl: implement third party memory cleaning,
  de-duping, correction routines" — related but tangential.
- `docs/proposals/COOL_THINGS_THAT_HERMES_AGENT_HAS.md` — catalog of Hermes
  features, but doesn't constitute a design for instinct-driven agents.

**What's missing:** The blog calls this the *major focus of v2*, yet neither
tracking document reflects that priority. There is no:
- Architecture proposal for persistent internal drives
- Design for self-improvement checks integrated into the agent loop
- ROADMAP milestone dedicated to the instinct paradigm (it's folded into a
  generic "Hermes subset" line at v2.0.4)
- Success criteria or scope definition for what "instinct over turns" means
  in concrete implementation terms

This is the largest gap by far. The blog promises a paradigm shift; the
tracking documents barely acknowledge it.

**Recommendation:** This needs its own design proposal and dedicated ROADMAP
milestones. The existing Hermes feature catalog
(`COOL_THINGS_THAT_HERMES_AGENT_HAS.md`) is a good starting point but needs
to be evolved into a concrete v2 architecture plan. Suggest:
1. Create `docs/proposals/INSTINCT_PARADIGM.md` with architecture design.
2. Add ROADMAP entries spanning multiple releases (this can't be one
   release's work).
3. Define which existing LunarWing systems are foundational (human-delay
   mode, routine system, safe sandbox) and what new pieces are needed
   (internal drive loop, self-improvement checks, surfacing mechanism).

---

## Items Verified as Covered

For completeness, these blog features are already adequately tracked:

| Feature | Where Tracked |
|---|---|
| MCP Overhaul | AGENT_GOALS #18, ROADMAP v2.0.2/0.4/0.6 |
| Lorebook Support | ROADMAP v2.0.2, proposals/LOREBOOKS.md |
| Profile Switching | ROADMAP v2.0.2, proposals/Profiles.md |
| Workspace Seeding | ROADMAP v2.0.2 |
| DarkIRC Key Exchange | AGENT_GOALS #15, proposals/DARKIRC_SECURE_KEY_EXCHANGE.md |
| Self-Healing Expansion | ROADMAP v2.0.2 |
| ONNX Support | ROADMAP v2.0.4 |
| Web UI Overhaul | AGENT_GOALS #7 (done) |
| Web-Based Onboarding | AGENT_GOALS #8 |
| Crate Updates | AGENT_GOALS #3, #10; ROADMAP v2.0.2 |
| Interactive Secret Mgmt | Shipped in 1.1.9 MT onboarding |

---

## Cross-Reference: Features in AGENT_GOALS/ROADMAP Not Mentioned in Blog

These items appear in the tracking documents but were not highlighted in the
blog announcement. This is informational, not a gap — the blog is a marketing
surface, not an exhaustive feature list.

- Multi-arch CI/CD pipeline (ROADMAP v2.0.1)
- In-place Upgrade Harness v3/v4 (ROADMAP v2.0.1)
- Nanocode external worker deprecation (ROADMAP v2.0.3)
- Input-validation security improvements (ROADMAP v2.0.4)
- Lunartica UI reskin (ROADMAP v2.0.3)
- Various doc consolidation tasks (AGENT_GOALS #19–#23)

---

## Recommended Actions

1. **Add `lunarwing-websocket-1` protocol verification** to AGENT_GOALS or
   link it to the existing item #6 / ROADMAP v2.0.1 legacy-drop entry.

2. **Acknowledge LunarVoice deferral** in AGENT_GOALS with a note pointing to
   ROADMAP v2.0.4+.

3. **Create `docs/proposals/PRESEEDED_MEMORY_ROUTINES.md`** and add a ROADMAP
   entry.

4. **Expand the Reflex Compiler ROADMAP entry** to reflect the actual scope
   (partial rewrites, not just polishing), or create a proposal.

5. **Create `docs/proposals/INSTINCT_PARADIGM.md`** — the single highest-
   priority gap. The blog positions persistent internal drives as the major
   focus of v2, but there is no design document, no architecture proposal,
   and no dedicated ROADMAP milestones for it. The existing
   `COOL_THINGS_THAT_HERMES_AGENT_HAS.md` is a feature catalog, not an
   architecture plan.

---

*Authored by Rarity — 2026-07-12*
