# LunarWing v2.0.7.0 Codename Ideas

LunarWing v2.0.7.0 is an AJ release: an even-numbered cycle whose work since
`v2.0.6.0` has one consistent shape. The release teaches the system to watch
itself and to recover without operator intervention. Agent turns and approvals
no longer stall after tool calls: orchestrator v0 retains structured results,
denied approvals resolve their original call, and `missing_tool_output`
analysis pairs call IDs against the authoritative inference transcript.
LunarVision gains supervised, health-gated lifecycles on both systemd and
OpenRC: dedicated operational health listeners, crash-supervisor `-sup` units,
health-aware tenant Quadlets, and registry-aware port discovery with fail-loud
container recreation. MCP Part 2 closes its audited gaps with conservative
protocol negotiation, bounded discovery refresh, generation-safe worker
recovery, atomic pinned stdio execution, and canonical untrusted annotations.

The supporting changes reinforce that theme. Multi-agent coordination becomes
deterministic: cross-daemon responder election with tenant roster persistence,
an auto-loaded managed seven-rule protocol bundle, strike-aware secret
detection, coalescer validation, and a uniform master switch across legacy and
Engine V2 paths. Two self-audits — the crate security audit and the MCP Part 2
gap audit — examine the release from the outside and the inside. Version
bumps, refreshed lockfiles, registry patch pins, and CI secret-detection fixes
keep the whole structure fastened down.

Taken together, this is a release about **a system that posts watchmen over its
own machinery, elects a single voice among many, and repairs its own stalls**.
The candidates therefore favor watchfulness, guardianship, inspection,
firefighting brigades, restored flow, concord, clockwork, appraisal, caution,
and anchored fastenings. They follow LunarWing's established Japanese
word/name convention and do not repeat a previous release codename or a
previous cycle's candidate.

## Candidate Shortlist

### 1. Mihari (見張り)

**English context:** A lookout or watchman; the act of keeping watch.

Mihari is the strongest overall metaphor. This release posts watchmen
everywhere: LunarVision supervisors watch tenant containers on systemd and
OpenRC, health gates watch dedicated listeners before reporting success,
strike-aware hooks watch for leaked secrets, and two audit documents watch the
dependency graph and the MCP runtime itself. A lookout does not act on the
machinery directly — it sees trouble first and raises the alarm, which is
exactly how the new supervision, health, and detection layers behave.

### 2. Shugo (守護)

**English context:** Protection, guardianship, or a guardian.

Shugo names the release's guardian layer. The new `-sup` crash supervisors are
literal guardians that stand beside each LunarVision unit and restart what
dies; generation-safe MCP worker recovery guards tool ownership across
restarts; scoped secrets and canonical untrusted annotations guard the
boundary between trusted and untrusted content. The word also fits the
release's defensive posture: hardening rather than new surface area.

### 3. Metsuke (目付)

**English context:** An inspector or overseer, after the shogunate officials
who examined the conduct of the feudal lords.

Metsuke captures the cycle's two formal self-examinations: the crate security
audit, which inspected the full shipped dependency graph against live RustSec
data, and the MCP Part 2 gap audit, whose three P1 and five P2 findings were
then remediated in the same cycle. Strike-aware secret enforcement and
canonical annotation of untrusted content extend the inspector's role into the
runtime, where conduct is graded rather than merely observed.

### 4. Hikeshi (火消し)

**English context:** The organized fire brigades of the Edo period.

Hikeshi fits a release whose defining work is putting out fires in an
organized way. Stuck agent turns, dropped tool results, false-failing CI
secret detection, stale LunarVision containers, and MCP runtime gaps were each
a small fire that is now extinguished at its source. The Edo brigades were not
lone heroes but coordinated crews with assigned roles — matching the
deterministic responder election that assigns one daemon to answer while the
others stand by.

### 5. Nagare (流れ)

**English context:** A flow, stream, or current.

Nagare highlights the un-sticking work at the heart of the release. Turns that
used to halt after certain tool calls or approval decisions now flow to
completion: denied approvals resolve their call instead of stranding it,
interrupted action batches close instead of hanging, and missing tool outputs
are detected by pairing call IDs rather than guessed. Bounded MCP discovery
refresh and health-gated sidecar startup keep the surrounding currents moving
instead of pooling behind a failed component.

### 6. Wa (和)

**English context:** Harmony, concord, or peaceful agreement.

Wa names the coordination theme. Multiple daemons and tenants now elect a
single responder deterministically, persist that roster, and honor explicit
routing, so a fleet of agents speaks with one voice instead of talking over
each other. The same concord appears in the init layer, where systemd and
OpenRC lifecycles converge on one contract, and in the engine layer, where
legacy and Engine V2 paths accept one uniform set of disables and one master
switch.

### 7. Karakuri (絡繰り)

**English context:** A clockwork mechanism or automaton; an ingenious device
that performs its task on its own.

Karakuri presents LunarWing as self-regulating machinery. Responder election
resolves without human input, the managed seven-rule coordination bundle
auto-loads, supervisors restart dead containers, stale sidecars are recreated
when their image, environment, or port contract drifts, and worker recovery
rebuilds state from snapshots. Each mechanism is small and deterministic, and
together they let the system wind itself back to health.

### 8. Kantei (鑑定)

**English context:** Appraisal, expert examination, or authentication of
genuineness.

Kantei focuses on the release's judgment work. The crate audit appraised every
advisory against the live shipped graph and separated fixable findings from
accepted ones; the MCP gap audit classified findings by severity and then
closed them; canonical annotations now mark untrusted tool content so later
stages appraise it correctly; and strike-aware secret detection grades
suspicious output instead of reacting blindly to entropy. The release
repeatedly asks what a thing really is before deciding what to do with it.

### 9. Ishibashi (石橋)

**English context:** A stone bridge; from the proverb about tapping a stone
bridge before crossing — caution even where crossing seems safe.

Ishibashi fits the release's deliberate conservatism. MCP protocol negotiation
now prefers the safest mutually supported version, stdio servers execute from
atomically pinned artifacts instead of mutable paths, LunarVision refuses to
report success before its health endpoint answers, mt-admin recreates rather
than trusts stale container contracts, and denied approvals fail closed into
an explicit result. Every crossing is tapped first.

### 10. Netsuke (根付)

**English context:** The carved toggle that anchors a cord so what hangs from
it cannot slip away.

Netsuke names the release's fastening work. Lockfiles are refreshed and
registry patch versions pinned so builds cannot drift, MCP stdio execution is
anchored to immutable artifacts, elected responder rosters are persisted so
coordination survives restarts, and recovered workers keep their generation
identity instead of silently becoming someone else. Small toggles, placed
exactly where a cord would otherwise slip.

## Top Picks

### 1. Mihari

Best overall. Every major thread of the release — supervision, health gating,
secret detection, auditing, and recovery — is an act of keeping watch over
machinery that previously ran unwatched. It covers the release's breadth
without reducing it to one subsystem, and the watchman image explains itself
in a single sentence of release notes.

```markdown
## Release Codename:  見張り
## English Context:   Mihari — the lookout; watchmen posted over the system's own machinery
```

### 2. Karakuri

Best mechanical alternative. Deterministic election, auto-loaded coordination
rules, crash supervisors, stale-container recreation, and snapshot-based worker
recovery are clockwork: small, self-acting mechanisms that return the system
to a healthy state without operator hands.

```markdown
## Release Codename:  絡繰り
## English Context:   Karakuri — clockwork that winds itself back to health
```

### 3. Hikeshi

Best narrative alternative. The cycle reads as a series of fires found and
extinguished — stuck turns, false CI failures, stale containers, MCP gaps —
by coordinated crews with assigned roles rather than by lone effort, which is
precisely the responder-election model the release formalizes.

```markdown
## Release Codename:  火消し
## English Context:   Hikeshi — the organized brigade that finds the fire and puts it out
```

## Recommendation

Use **Mihari (見張り)** for LunarWing v2.0.7.0. The release is not merely a
collection of fixes. Its defining achievement is that the system now posts
watchmen over every layer that previously failed silently: supervisors over
containers, health gates over listeners, election over competing daemons,
strikes over secret leaks, and audits over dependencies and protocol gaps.
That single act of deliberate watchfulness, extended across the whole product,
is what `Mihari` means.
