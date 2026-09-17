# FirstMate Approval Vertical Slice — Results (2026-09-17)

Executed by Applejack (ajhermes) on live inttest tenant `antelope`, gateway
port 10000, against master @ decbdff7 + worker fixes from the current cycle.
Companion to `FIRSTMATE_FLEET_FEASIBILITY.md` — this validates the doc's
exploratory check #2 (approval contract, one agent, one real gated call).

## Method

Five-phase API-only loop, no browser involved:

1. `POST /api/chat/thread/new` → fresh thread `0ca3a481…` (state Idle)
2. `POST /api/chat/send` — probe: "use your shell tool to run: echo …"
   → 202 accepted
3. **Discovery:** read `/home/antelope/lunarwing/state/pending-gates.json`
4. `POST /api/chat/approval` with `{thread_id, request_id, action:"approve"}`
5. `GET /api/chat/history?thread_id=…` → terminal state

All calls authenticated with the tenant bearer token (header form).

## Results

| Phase | Outcome |
|---|---|
| Auth + thread ops | 200/202 first try |
| Gate armed by probe | ~15s after send — `shell` call parked at approval gate |
| Gate discovery (no browser) | ✅ full context: request_id, action_name, parameters, description, thread_id, resume_kind |
| Approval round-trip | ✅ 202 → turn resumed |
| Terminal state | ✅ `Completed` with a real agent response (see below) |

## Key findings for the connector design

1. **`pending-gates.json` IS the missing snapshot channel.** The §5 concern
   ("SSE is in-memory, reconnect needs a snapshot") has an existing answer:
   the gate file is a durable, pollable, per-tenant source of truth holding
   every field the approval POST needs. A v0 connector can poll it directly
   (or a thin GET endpoint can expose it). No SSE replay journal required
   for the approval vertical.
2. **Multi-gate turns are REAL.** First approve resumed the turn and the
   agent immediately re-armed with a *second* `shell` call (new
   request_id, now with a `timeout:30` param). One approval ≠ one turn.
   Connectors MUST loop: approve → re-read gate file → repeat until empty.
   (Browser-smoke's `MAX_GATE_CLICKS` cap pattern applies; recommend a
   connector-side cap too.)
3. **Approval POST contract confirmed:** `thread_id` + `request_id` +
   `action` — both IDs obtainable without any browser session state.
   A missing/foreign thread_id silently lands on the Idle thread
   ("Ignoring stale approval" — known gotcha, previously documented).
4. **SSE was not needed** for this flow. A 25s `GET /api/chat/events`
   window emitted nothing gate-related in this run; the gate file + a
   history poll sufficed. (SSE remains useful for live UX, but a
   pull-based connector is viable on current APIs alone.)
5. **Honest terminal reporting.** The completed turn reported its own
   tool failure truthfully: the tenant's stale worker image hit the known
   podman userns corruption ("worker image unusable — podman pause-process
   corruption, try podman system migrate"). The supervisor pattern works:
   agent reached `Completed` state while narrating real failure — exactly
   the visibility FirstMate consumes.

## Timing under the latency rig

Send → gate armed: ~15s. Approve → re-gate: ~5s. Approve-2 → turn
Completed: <8s. Well inside the doc's 5-minute approval target; the heavy
cost is the agent's own reasoning turns, not the approval mechanics.

## Verdict

Exploratory check #2 passes: the approval vertical is implementable
**today** on existing gateway APIs, pull-based, with the gate file as the
discovery snapshot and a re-poll loop for multi-gate turns. No LunarWing
changes required for v0.

— Applejack 🐴
