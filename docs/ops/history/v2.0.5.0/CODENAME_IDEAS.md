# LunarWing v2.0.5.0 Codename Ideas

LunarWing v2.0.5.0 is an AJ release: an even-numbered cycle whose completed
work centers on security hardening, self-repair, and dependency hygiene. The
headline items are the DarkIRC secure staged key exchange Stage A acceptance
(source-audit hardening with the H1/H2, M1-M4, and L1-L2 findings fixed and
all local gates green), the self-healing watchdog v2.1.0 expansion (escalation
cooldowns, retryable delivery, flap history, NetBird readiness, LunarVision
container remediation, persistent OpenRC worker stop intent), and a full
dependency refresh that cleared RUSTSEC-2026-0187 and pruned stale `deny.toml`
advisory ignores. The strongest codenames therefore describe keys and trust,
healing and recovery, or audit-forged hardening. The candidates below follow
the Japanese word/name convention established by `Kawarimi`, `Takamaru`,
`Tatara`, `Kiyome`, `Togishi`, `Dōkutsu`, `Sashiko`, and `Odoshi`, without
repeating any previous release codename.

## Candidates

### 1. Kagi (鍵)

**English context:** A key; also the act of locking.

Kagi is the most literal fit for the release's centerpiece: DarkIRC's secure
staged key exchange Stage A, hardened through source audit until every H, M,
and L finding was closed and all 111 core and helper tests passed. The name
also covers the quieter work of locking the supply chain down — refreshed
dependencies, a cleared RUSTSEC advisory, and pruned `deny.toml` ignores.

### 2. Chiyu (治癒)

**English context:** Healing; curing; recovery from injury or illness.

Chiyu names the release's largest functional change: self-healing watchdog
v2.1.0. Escalation cooldowns with collision-resistant sidecars, retryable
failed delivery, state-preserving dry-run, flap restart history, and container
remediation all describe a system that treats its own wounds instead of
waiting for an operator.

### 3. Hagane (鋼)

**English context:** Steel; hardened metal.

Hagane continues the craft-metallurgy line opened by v1.1.8's `Tatara`, the
furnace that smelts tamahagane steel. Where Tatara produced the raw metal,
v2.0.5.0 tempers it: audit-forged fixes to the key-exchange implementation,
cleared advisories, and renewed dependencies leave the codebase harder than
the last cycle found it.

### 4. Mamori (守り)

**English context:** Protection; a guard or defense; as *omamori*, a small
protective charm kept close.

Mamori captures the release's layered protection: the watchdog guarding
tenant services, staged key exchange guarding DarkIRC channels, and
`cargo deny` advisories guarding the dependency graph. Like the charm, each
safeguard is small, deliberate, and placed exactly where harm is expected.

### 5. Jōbu (丈夫)

**English context:** Robust, sturdy, healthy; able to withstand use without
breaking.

Jōbu describes the state the release leaves behind rather than any single
feature: a fleet whose health checks, escalation paths, worker stop intent,
and dependencies have all been made sturdier. It pairs naturally with the
health-check pipeline that gained 435 passing tests across seven suites.

### 6. Kaifuku (回復)

**English context:** Recovery; restoration to a former good state.

Kaifuku emphasizes the recovery half of self-healing: retryable escalation
delivery, restart history that survives recovery, remediated LunarVision
containers, and OpenRC workers that stay stopped when an operator paused
them. The name also fits the Stage A acceptance path, which recovered the
key-exchange work from its audit findings.

### 7. Tate (盾)

**English context:** A shield.

Tate is the plainest defensive metaphor in the list. The release raises
shields on three fronts at once: cryptographic (hardened staged key
exchange), operational (watchdog remediation and escalation controls), and
supply-chain (dependency refresh with the active RUSTSEC advisory cleared).

### 8. Yakishime (焼締)

**English context:** High-fired, unglazed ceramic ware hardened entirely by
the kiln.

Yakishime gains its strength from process rather than coating, which mirrors
how v2.0.5.0 was hardened: not by surface polish but by running the work
through the fire of source audit, analyzer gates, and full harness suites
until the findings stopped coming.

### 9. Kagami (鏡)

**English context:** A mirror; also one of the Three Imperial Regalia,
associated with wisdom and honesty.

Kagami suits a release whose watchdog now watches itself: opt-in NetBird
readiness probes, systemd-scope-matched base-unit checks, dry-runs that
preserve state, and flap history that remembers what recovery tried. A
system that can see itself clearly is the precondition for healing itself.

### 10. Kintsugi (金継ぎ)

**English context:** "Joining with gold"; repairing broken ceramics with a
visible gold-finished seam.

Carried forward from the v2.0.3.0 shortlist because it fits this cycle even
better: the self-heal expansion makes repairs visible rather than hidden —
flap restart history survives recovery, escalation delivery is retried and
accounted for, and paused workers stay visibly paused instead of being
silently resurrected.

## Top picks

### 1. Kagi

Best overall: the release's defining work is secure staged key exchange, and
the name is short, distinctive, and unmistakable in what it points at.

```markdown
## Release Codename:  鍵
## English Context:   Kagi — key; the act of locking
```

### 2. Chiyu

Best functional metaphor: self-healing watchdog v2.1.0 is the largest shipped
capability, and healing is exactly what it does.

```markdown
## Release Codename:  治癒
## English Context:   Chiyu — healing; recovery
```

### 3. Hagane

Best lineage metaphor: audit-tempered steel continuing the metallurgy thread
that `Tatara` began in v1.1.8.

```markdown
## Release Codename:  鋼
## English Context:   Hagane — hardened steel
```

## Summary

These candidates follow LunarWing's established convention of a single
Japanese word in romaji with native script, drawn from craft, warrior-era, or
everyday culture and tied directly to the release's real work. For this
even-numbered AJ cycle, the list favors keys and trust, healing and recovery,
and audit-forged hardening; **Kagi (鍵)** is the top pick because the DarkIRC
secure staged key exchange Stage A acceptance is the release's centerpiece,
and the same "locking things down properly" instinct runs through the
watchdog and dependency work beside it.
