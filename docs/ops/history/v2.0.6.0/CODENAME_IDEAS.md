# LunarWing v2.0.6.0 Codename Ideas

LunarWing v2.0.6.0 is an AJ release: an even-numbered cycle whose work since
`v2.0.5.0` is unusually broad, but has one consistent shape. Previously separate
capabilities are now joined through explicit, protected, and recoverable paths.
The dominant change is MCP Part 2: host sandboxing, a shared runtime-adapter
boundary, MCP-over-WASM, dedicated and shared gateways, worker-local placement,
scoped credential grants, package provenance, and post-start restart
supervision. The release also adds a native Study Notes knowledge layer, local
LunarVoice ONNX transcription with Whisper and Parakeet support, and default
Engine V2 channels for XMPP, WeeChat, and DarkIRC.

The supporting changes reinforce that theme. Model failures gain bounded
recovery guidance, worker and host MCP activation recover without losing owner
or tool boundaries, WeeChat restoration works in the tenant's OpenRC context,
and mission goals no longer pollute conversation insights. Lorebooks become
opt-in, ambiguous high-entropy hex warnings become quieter without weakening
blocking or redaction, Multica accepts guarded API subtrees under base paths,
and dependency, CI, documentation, and tenant operations receive another
maintenance pass.

Taken together, this is a release about **many distinct strands becoming one
resilient system without erasing the boundaries between them**. The candidates
therefore favor braiding, joining, confluence, bridges, circuits, voice, and
memory. They follow LunarWing's established Japanese word/name convention and
do not repeat a previous release codename.

## Candidate Shortlist

### 1. Kumihimo (組紐)

**English context:** A traditional Japanese braided cord made by interlacing
multiple strands.

Kumihimo is the strongest overall metaphor. HTTP, stdio, Unix, WASM, gateway,
and worker-local MCP runtimes remain distinct strands, but the runtime adapter
and lifecycle machinery braid them into one operator-facing system. Voice,
knowledge, and default messaging channels add more strands. A braid also grows
stronger from its construction, matching sandboxing, scoped grants, provenance,
restart supervision, bounded recovery, and init-system parity.

### 2. Musubi (結び)

**English context:** A tie, knot, joining, or connection.

Musubi emphasizes what the release accomplishes rather than how. It joins MCP
transports and placements, connects local voice processing to LunarWing through
a sandboxed tool, and turns Study Notes into a native knowledge capability. The
knot image also implies a connection that is intentionally secured: owner scope,
credential grants, package pins, sandbox policy, and tool-limit checks keep each
join from becoming an unguarded opening.

### 3. Gōryū (合流)

**English context:** A confluence; separate streams joining into one flow.

Gōryū fits the adapter and gateway architecture particularly well. Different
MCP transports converge on one validated factory path, host and worker
placements enter the same extension lifecycle, and XMPP, WeeChat, and DarkIRC
become the default Engine V2 channel flow for new tenants. LunarVoice and native
knowledge are additional input streams that now reach the same agent runtime.

### 4. Kakehashi (架け橋)

**English context:** A bridge, especially a figurative bridge between separate
people, places, or systems.

Kakehashi names the release's boundary-crossing work: MCP gateways bridge
remote clients, runtime adapters bridge transport implementations, worker-local
placement bridges the orchestrator and job container, and the transcription
tool bridges a local ONNX sidecar into the agent. The bridge is controlled rather
than open-ended, reflecting authentication, provenance, sandboxing, and bounded
resource policies.

### 5. Kairo (回路)

**English context:** A circuit or routed pathway.

Kairo presents LunarWing as a set of deliberate paths through which tools,
messages, voice, and knowledge can travel. MCP Part 2 adds new routes while
keeping activation on one validated circuit; Engine V2 enables its principal
channel routes by default. Restart supervision, capped model recovery, doctor
output, and circuit-open behavior make failures visible and stop unstable paths
from looping forever.

### 6. Hibiki (響き)

**English context:** Sound, resonance, reverberation, or the effect something
continues to have.

Hibiki is the best name for the release's human-facing additions. LunarVoice now
hears local Whisper and Parakeet models, while three Engine V2 messaging
channels are enabled for new tenants. The resonance metaphor extends to native
knowledge that preserves useful findings and to supervised MCP workers that can
resume service after an unexpected exit instead of letting one failure silence
the capability.

### 7. Amime (網目)

**English context:** The mesh or pattern of a net.

Amime captures the expanded topology: host-local processes, WASM actors,
gateways, job workers, external channels, a voice sidecar, and knowledge stores
form a practical mesh. The release does not treat that mesh as implicit trust.
Sandbox limits, exact credential grants, provenance records, placement rules,
owner routing, and endpoint allowlists define which adjacent points may
communicate.

### 8. Kioku (記憶)

**English context:** Memory or recollection.

Kioku highlights the new native Study Notes layer: PostgreSQL and libSQL
storage, built-in knowledge tools, provenance, lifecycle status, and stale-note
sweeps make durable findings part of LunarWing rather than an external-only
workflow. It also fits the release's recovery work: worker MCP snapshots support
reconstruction, restart state is retained for a job, and WeeChat can again save
and restore tenant-owned buffers correctly on OpenRC.

### 9. Shingō (信号)

**English context:** A signal, indication, or traffic signal.

Shingō unifies the communication and control-plane changes. Voice becomes an
agent input signal; XMPP, WeeChat, and DarkIRC carry message signals by default;
and MCP adapters carry JSON-RPC across several runtimes. Health probes, doctor
status, activation errors, and bounded recovery provide the corresponding
control signals so operators can distinguish a healthy route from an isolated
or failed one.

### 10. Kaname (要)

**English context:** A pivot, linchpin, or essential point that holds a larger
structure together.

Kaname focuses on the shared boundaries introduced in this cycle. The MCP
runtime registry and validated factory become a linchpin for diverse transports
and placements, while native knowledge and local transcription make the core
agent useful across more inputs. Smaller fixes around owner identity, OAuth
activation, mission insights, tenant rendering, secret warnings, and dependency
hygiene keep those central paths dependable.

## Top Picks

### 1. Kumihimo

Best overall. It accounts for the breadth of the release without reducing it to
one feature: many runtimes, placements, channels, voice and knowledge are woven
together, while their security and ownership boundaries remain visible. The
traditional craft image also fits the naming lineage established by `Tatara`,
`Sashiko`, and `Odoshi`.

```markdown
## Release Codename:  組紐
## English Context:   Kumihimo — braided cord; distinct strands made stronger together
```

### 2. Musubi

Best concise alternative. It directly conveys connection, integration, and a
secured join, making it easy to explain in release notes and easy to remember.

```markdown
## Release Codename:  結び
## English Context:   Musubi — a tie, knot, joining, or connection
```

### 3. Gōryū

Best architectural alternative. Its image of multiple streams converging most
precisely matches the unified MCP adapter/factory path and the newly connected
voice, knowledge, worker, and channel flows.

```markdown
## Release Codename:  合流
## English Context:   Gōryū — confluence; separate streams joining one flow
```

## Recommendation

Use **Kumihimo (組紐)** for LunarWing v2.0.6.0. The release is not merely a
larger network or a collection of new integrations. Its defining achievement is
that distinct execution environments and information sources are deliberately
interlaced, then strengthened with sandboxing, provenance, scoped credentials,
recovery, and operator controls. That combination of connection, retained
structure, and added resilience is exactly what a braided cord represents.
