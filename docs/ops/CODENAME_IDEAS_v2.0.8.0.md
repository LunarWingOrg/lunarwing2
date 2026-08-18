# LunarWing v2.0.8.0 Codename Ideas

LunarWing v2.0.8.0's work since the `v2.0.7.0` Mihari release has one
consistent shape. Where Mihari posted watchmen over the machinery, this
cycle makes the machinery **bounded, signed, and self-explaining**. Every
failure path now tells the model or operator what would unlock it: bounded
recovery turns for provider errors, a text-only final turn past the
iteration limit, typed terminal failures with next steps, denied approvals
that resume the loop with per-call denial results instead of ending the
turn, hook rejections that name the unlock path, and missions that report
the precise reason they did not fire. Budgets are no longer invisible:
mission daily thread limits are configurable at creation, exposed on list
surfaces, and logged at warn when exhausted.

The supporting changes draw hard edges around everything that runs.
OpenRC hosts get automated pre-delegated cgroup subtrees with bounded
tenant builds — `memory.high` throttling, `memory.max` OOM enforcement,
login-hook session placement, and a root-side rendezvous so bounding works
on every host. The MCP marketplace becomes a signed supply chain: ed25519
publisher signatures, signed lockfiles pinning exact version and digest,
and signed revocation lists with monotonic sequences, now wired into
config and activation. The bundled gateway gains an enable-able
control-plane service and a hardened streaming data plane: SSE
passthrough without a cumulative lifetime cap, separate transient and
stream permit pools so notification streams cannot starve tool calls,
request-body deadlines, upstream userinfo rejection, and sanitized error
surfaces. Sandbox profiles can be made mandatory; bearer tokens are shape
validated; external runtime descriptors are reachable.

The rest of the cycle is couriers and housekeeping. Cross-tenant knowledge
sharing lets one tenant's durable finding travel the coordination pipeline
and land in every watching tenant's study-notes as untrusted,
needs-verification input, behind publisher allowlists and per-sender plus
daemon-wide ingest budgets. The opencode worker is pinned, baked, and
self-reporting at `v1.18.18`. A fresh crate security audit clears the
advisory board, ten manifests and six lockfiles move to `2.0.8`, CI test
fixtures are templated to the crate version forever, and a recorded repo
hygiene sweep archives the clutter.

Taken together, this is a release about **a system that posts a guidepost
at every dead end, checks every traveler's papers at every gate, and sends
couriers between its villages**. The candidates therefore favor
guideposts, checkpoints, passes, seals, ledgers, reins, couriers, and the
invisible stagehands who keep the play moving. They follow LunarWing's
established Japanese word/name convention and do not repeat a previous
release codename or a previous cycle's candidate.

## Candidate Shortlist

### 1. Michishirube (道標)

**English context:** A guidepost or signpost; a marker that shows the way.

Michishirube is the strongest overall metaphor. This release's defining
work is that no path ends opaquely anymore: provider failures return with
a bounded recovery turn, the iteration limit yields a text-only final
turn, denied approvals resume the loop with the denial recorded per call,
hook rejections and disabled capabilities name exactly what would unlock
them, missions report the precise not-fired reason, and exhausted budgets
announce themselves at warn instead of debug. Every dead end now carries a
signpost pointing at the way forward.

### 2. Sekisho (関所)

**English context:** The Edo-period checkpoint or barrier station where
officials inspected travelers' papers before letting them pass.

Sekisho names the release's gate-keeping work. MCP marketplace packages
are inspected for publisher signatures and checked against signed
revocation lists before activation; the gateway control plane demands
constant-time bearer credentials; coordination KNOW records are stopped at
a publisher allowlist; sandbox profiles become mandatory papers; cgroup
subtrees bound every build; and shape-validated tokens, permit pools, and
body deadlines check every request at the gate. Nothing crosses without
inspection.

### 3. Hikyaku (飛脚)

**English context:** The licensed courier runners of the Edo period who
relayed letters and goods between post towns on fixed routes.

Hikyaku fits the release's courier work. Cross-tenant KNOW records carry
one durable finding from the publishing tenant to every watching tenant's
own study-notes, arriving stamped with provenance and marked untrusted —
exactly how relay runners delivered sealed mail between towns. The gateway
SSE passthrough is the same figure in protocol form: a runner who carries
the stream end to end without stopping to rest in a buffer.

### 4. Tegata (手形)

**English context:** A pass or permit; the wooden passport shown at
checkpoints to prove the bearer's right to travel.

Tegata highlights the permits this release issues and honors. Publisher
allowlists decide who may emit knowledge records; bearer admin tokens gate
the gateway control plane; transient and stream permits ration gateway
capacity so one stream cannot crowd out tool calls; mandatory sandbox
profiles become the standing pass every MCP server must carry; and
external-worker placement is a validated, fail-closed credential rather
than a hopeful string.

### 5. Shiori (栞)

**English context:** A bookmark; also a travel guidebook or itinerary
handed to a traveler.

Shiori covers the release's documentary and guidance work. Recovery
instructions are small guidebooks handed to the model at the moment it
gets lost; audit documents — the crate security audit, the repo hygiene
ledger, the MCP review ledgers — record what was examined and where it
stands; and the operator-facing surfaces (mission budgets, `opencode_ref`
on worker health, precise not-fired reasons) leave a bookmark at every
place someone once had to guess.

### 6. Tazuna (手綱)

**English context:** Reins; the straps used to direct and check a horse.

Tazuna names the release's deliberate control. Mission thread budgets rein
in daily spend per rider, cgroup subtrees rein in tenant build memory with
hard `memory.max` limits, ingest budgets rein in cross-tenant knowledge
flow, permit pools rein in gateway concurrency, and token-shape and
body-size limits rein in what the control plane will accept. The system
gallops no faster than its operator holds the straps.

### 7. Inkan (印鑑)

**English context:** The registered personal seal stamped in place of a
signature to authenticate a document.

Inkan focuses on the supply-chain trust work. MCP marketplace manifests
are stamped with ed25519 publisher seals, lockfiles seal exact versions
and digests, and revocation lists carry their own signed seal with a
monotonic sequence that refuses rollback — a document stamped by the wrong
seal, or by a seal since revoked, fails closed. Provenance digest
verification and untrusted-wrapping of coordination notes extend the seal
to everything the system accepts from outside.

### 8. Kakebo (家計簿)

**English context:** A household account book or ledger where every
income and expense is written down.

Kakebo fits a release that writes everything down. Mission budgets are
ledger entries rendered visible (`threads_today`, `max_threads_per_day`,
`budget_exhausted`); the crate audit balances the advisory ledger against
the live dependency graph; the repo hygiene sweep keeps a line-by-line
ledger of every move; and lockfile refreshes plus registry patch pins keep
the book reconciled so tomorrow's build matches today's entry.

### 9. Kuroko (黒子)

**English context:** The black-clad stagehands of kabuki theater who move
scenery and props in plain sight yet are treated as invisible.

Kuroko suits the release's invisible machinery. Pre-delegated cgroup
subtrees, PAM login hooks, and the root-side rendezvous place every
session and build in its correct container without the tenant ever seeing
a hand move; detached ingest tasks repair stranded writes behind the
scenes; stale containers and drifted contracts are recreated before they
can fail loudly. The play runs smoothly because unseen hands keep moving
the set.

### 10. Kaidō (街道)

**English context:** The great highways of the Edo period — fixed routes
linking the post towns, traveled by couriers, pilgrims, and officials.

Kaidō names the roads this release paves between tenants and services.
The coordination pipeline becomes a maintained highway with rules of the
road: KNOW records travel it one finding at a time, gated at both ends;
the bundled gateway turns proxied MCP traffic into a streamed road with
separate lanes for transient POSTs and long-lived SSE; and the ports
registry, control plane, and worker health surfaces serve as the posted
distance markers along the way.

## Top Picks

### 1. Michishirube

Best overall. The release's deepest change is behavioral: every
termination path — iteration limit, provider error, denied approval,
disabled capability, hook rejection, unfired mission — now explains itself
and points at what would unlock it. Budgets, audits, and health metadata
extend the same courtesy to operators. A guidepost at every dead end is
the one image that covers the whole cycle.

```markdown
## Release Codename:  道標
## English Context:   Michishirube — the guidepost; every stopped path now tells you the way forward
```

### 2. Sekisho

Best security alternative. Signed marketplace manifests, revocation
checks, publisher allowlists, mandatory sandbox profiles, constant-time
bearer gates, cgroup bounds, and validated tokens are all inspections
performed before anything is allowed to pass. The release staffs a
checkpoint on every road into the system.

```markdown
## Release Codename:  関所
## English Context:   Sekisho — the checkpoint; no traveler, package, or token passes without inspection
```

### 3. Hikyaku

Best narrative alternative. Cross-tenant KNOW records are sealed letters
relayed between hidden villages with provenance intact and contents marked
untrusted on arrival, and the gateway SSE passthrough is the runner who
carries the stream without resting. The cycle's most distinctive new
capability is the courier route between tenants.

```markdown
## Release Codename:  飛脚
## English Context:   Hikyaku — the courier; sealed findings relayed between villages, streams carried end to end
```

## Recommendation

Use **Michishirube (道標)** for LunarWing v2.0.8.0. The release is not
merely a collection of bounds and signatures. Its defining achievement is
that the system stopped failing silently: it recovers with guidance,
refuses with reasons, budgets with visibility, and reports exactly why a
thing did not run. Mihari taught LunarWing to watch itself; Michishirube
teaches it to tell you the way.
