# FirstMate-led LunarWing/Hermes fleet: feasibility and draft requirements

**Status:** requirements draft; source-reviewed; exploratory check #2 (approval
vertical slice) live-validated 2026-09-17 — see
[`FIRSTMATE_SLICE_RESULTS.md`](FIRSTMATE_SLICE_RESULTS.md).
**Date:** 2026-09-17; updated 2026-09-19.
**Branch:** `experiment/feasibility`, based on LunarWing master `decbdff7e`.

## 1. Confirmed user requirements

- General-purpose work: coding, research, infrastructure operations, recurring
  maintenance, and other tasks. A repository or pull request is not a required
  task input or output.
- Tool approvals must be resolved **before 30 minutes elapse**, with a desired
  **maximum response time of five minutes** from request creation (§5).
- Start fairly autonomously with **at least five existing LunarWing test agents**.
- Eventually support **over a dozen agents across LunarWing and Hermes**.
- Easy initial setup and enrollment of additional agents are primary product
  requirements. Avoid a large Node.js application-server deployment.
- **Manual onboarding only:** the operator explicitly adds existing agents.
  Fleet integration concerns interaction with those agents; creating temporary
  agents or automating tenant provisioning is outside this experiment.
- Reconnecting clients and recovery of pending approvals need investigation (§6).
- This is open-ended experimental research, **with no project deadline**.
  FirstMate is one candidate; the user is also considering other options.

The supervisor interaction surface and initial host count are still open.
The requirement for autonomous supervision does not itself select a runtime's
execution-mode enum or authorize permanent tool grants.

## 2. Feasibility conclusion

A FirstMate-led experiment is plausible using LunarWing's existing authenticated
agent gateway plus a small fleet connector. Since the first draft, the approval
vertical slice passed live against a real tenant on stock gateway APIs
([`FIRSTMATE_SLICE_RESULTS.md`](FIRSTMATE_SLICE_RESULTS.md)): gate discovery via
the durable `state/pending-gates.json` file, request-scoped approval round
trip, and a confirmed terminal turn — pull-based, no SSE replay journal, and
well inside the five-minute target. The largest risks that remain are reconnect
correctness across missed events, ambiguous task submission (still no
client-keyed idempotency), and FirstMate's own setup/enrollment burden, which
has not been trialled at all.

The user accepts the proposed supervisor-plus-connector direction. FirstMate's
conversational supervisor and local-state approach make it a candidate worth
exploring within that design. Its actual setup burden is an evaluation criterion,
and comparison with other candidates remains open.

Paperclip already includes local and gateway Hermes adapters and supports
external adapter packages. Its application-server deployment is a weaker fit
for the user's stated priority, so it remains comparison evidence rather than
the proposed initial deployment.

## 3. Source-verified integration facts

### FirstMate

Reviewed snapshot: `kunchenguid/firstmate` at
`3eb5b6334a80e06083e3837f0032a5cec39b8e52`.

- Existing worker control is harness-specific. LunarWing and Hermes are absent
  from the verified harness list. A raw launch command is an experimental
  entry point, not a complete supervision/control integration.
- External `process-event-adapter/1` bindings can report events to the supervisor;
  they do not define worker launch/control or an automatic approval sink.
- The reference watcher defaults to `FM_POLL=15` seconds. This is one component
  of latency, not an end-to-end approval-response guarantee.
- Remote secondmates require a complete remote FirstMate home and the Herdr
  backend for the remote supervisor. The proposed connector below would instead
  address existing agent gateways; that is new integration work.
- Stock bootstrap requires `node`, `git`, `gh`, `no-mistakes`, `gh-axi`,
  `chrome-devtools-axi`, `tasks-axi`, and `quota-axi`, plus the selected backend's
  dependencies. It does not require a large web application server, but its
  dependency footprint is significant. A reduced external-fleet profile would
  need explicit upstream support or a maintained patch.

Sources: [control adapters][fm-control], [dispatch][fm-spawn],
[event extension boundary][fm-extensions], [watcher][fm-watch],
[remote homes][fm-remote], [bootstrap toolchain][fm-bootstrap].

### LunarWing

- The routed `POST /api/chat/send` requires an explicit `thread_id` and returns
  HTTP 202 after enqueueing input (`ic/src/channels/web/server.rs:1338-1428`).
  It provides neither a task-completion receipt nor client-keyed idempotent
  submission. A lost acceptance response must not trigger a blind resend.
- `POST /api/chat/approval` accepts `request_id`, `thread_id`, and an action
  (`approve`, `deny`, or `always`). Its HTTP 202 is also only an enqueue receipt
  (`ic/src/channels/web/server.rs:1430-1505`).
- Engine V2 resolves approvals using request identity, user/thread scope,
  channel authorization, and expiry checks. `gate_resolved` identifies the
  outcome; approval resolution precedes execution of the approved action
  (`ic/src/bridge/router.rs:2317-2506`, `ic/src/gate/store.rs:235-299`).
- A single pending approval can emit both `gate_required` and `approval_needed`.
  These must be deduplicated by request identity
  (`ic/src/bridge/router.rs:290-326`).
- Internal pending gates have `created_at` and `expires_at`, but neither the
  public gate view nor the approval/gate event DTOs exposes those timestamps
  (`ic/src/gate/pending.rs:54-57,82-107`,
  `ic/src/channels/web/types.rs:198-208,319-330`).
- The configured pending-gate expiry defaults to 1800 seconds, with a valid
  range of 30 seconds to 30 days. The background sweep runs every 60 seconds;
  resolution itself checks expiry. This is distinct from the supervisor's
  proposed response target (`ic/src/config/agent.rs:40-65`).
- SSE is an in-memory broadcast stream with gap reporting, not a replay journal
  (`ic/src/channels/web/sse.rs:44-53,176-233`). The routed history handler's
  pending approval reads legacy session state; it is not a complete Engine V2
  pending-gate snapshot (`ic/src/channels/web/server.rs:1770-1930`).
- The gateway's OpenAI-compatible completions endpoint proxies the LLM provider;
  use the chat/agent path for fleet work
  (`ic/src/channels/web/openai_compat.rs:478-626`).
- `ExecutionMode::Autonomous` denies `Always`-approval tools rather than asking
  a supervisor. Select an approval-capable execution path for delegated review
  (`ic/src/gate/approval.rs:69-149`).
- **Live slice findings (tenant `antelope`, 2026-09-17):** the daemon-persisted
  `state/pending-gates.json` held every field the approval POST needs (request
  ID, tool name, parameters, description, thread identity, resume kind) and
  served as the discovery snapshot for the whole flow. **Multi-gate turns are
  real:** the first approval immediately re-armed a second `shell` call under a
  new request ID, so one approval never means one completed turn. A missing or
  mismatched `thread_id` on the approval POST can silently land the decision on
  the wrong ("Idle") thread — always pass the explicit thread ID
  ([`FIRSTMATE_SLICE_RESULTS.md`](FIRSTMATE_SLICE_RESULTS.md)).

### Hermes

Reviewed snapshot: `NousResearch/hermes-agent` at
`98f758ae7e8db83c2bb9214c3b35adf41df15f03`.

The current Runs API implements submission, status, events, approval, steer,
and stop endpoints. Idempotency capability reports whether storage is durable;
keyed runs can recover status after restart, classifying a vanished execution
owner as interrupted. These are useful provider-adapter contracts, but approval
timing and deployed-version compatibility still require a live trial.

Sources: [Runs API implementation][hermes-runs], [API guide][hermes-api].

## 4. Proposed minimal architecture

```text
Operator
   |
FirstMate supervisor (supported primary harness)
   | task, status, steer, approve/deny, cancel tools
Fleet connector CLI + event listener + local durable records
   |-- LunarWing adapter --> existing tenant gateways
   `-- Hermes adapter ----> existing Hermes Runs APIs
```

Target one supervisor installation and one connector installation for the fleet.
The connector can expose a small CLI and a supervised event-listener process;
an additional application server is not required by this design. Language and
local storage format are implementation decisions, not prerequisites.

FirstMate's backlog remains the work inventory. Connector records map each task
attempt to the agent instance, runtime conversation/run, event observations,
and approvals. The connector handles transport, deduplication, deadlines, and
reconciliation; the supervisor handles planning, delegation, review, and follow-up.
Adapting FirstMate's task inventory and general-purpose task semantics is part
of the integration, not an existing feature of its event extension.

Agents retain their existing identity, memory, tools, and service management.
LunarWing tenant lifecycle remains with `lunarwing-mt-admin.sh` and its
systemd-user/OpenRC paths. The connector interacts with the agents' gateways.

### Manual enrollment workflow

1. Operator supplies an agent name, runtime type, existing gateway endpoint,
   and credential reference.
2. A connection check confirms authentication, agent identity, and supported
   task/approval interfaces; show actionable diagnostics for missing capabilities.
3. Save the registration locally and begin observing the selected agent.
4. Provide manual edit, reconnect, and remove operations for that registration.
   Removing a registration retains its task history and leaves the agent running.

Capabilities can be queried after registration, but fleet membership is always
operator-selected. Credentials stay outside model-visible task metadata. A new
agent should require one short registration flow through the shared connector.

## 5. Delegated approval contract

### Confirmed timing

- **Desired maximum:** runtime-confirmed resolution within **300 seconds** of
  the pending request's creation.
- **Outer limit:** resolution must occur **before 1800 seconds** have elapsed,
  subject to any earlier expiry enforced by the agent.
- These are per-request timing requirements, separate from the open-ended
  exploration schedule. They concern resolving a pending tool call, not granting
  tool access for a five- or thirty-minute period.

The five-minute target does not shorten the agent's thirty-minute expiry.
Crossing five minutes is a target miss; expiry is a separate outcome. Reconnecting
does not reset either clock. Verify each existing agent's configured expiry
during onboarding rather than assuming the source default matches deployment.

### Proposed per-request flow

1. Capture the runtime's approval event and durably identify it by agent
   instance, runtime user/thread, and request ID. Deduplicate compatibility
   events and re-delivery.
2. Present the supervisor with the task mandate, tool name, display-safe
   arguments, and the actual request creation/expiry times. Keep runtime
   conversation identity and engine thread identity distinct where necessary.
3. Schedule approval review ahead of ordinary reporting. A supervisor occupied
   with a long task must not silently starve approvals from the other agents.
4. Submit an explicit request-scoped decision through the native approval API.
5. Confirm the matching runtime resolution. Record `decision_submitted`,
   `approved`/`denied`/`expired`, and subsequent tool outcome separately.

Steps 1–5 were validated live by the slice. One approval resolves exactly one
request, and turns can immediately re-arm with a fresh request under a new ID —
the connector must therefore loop: re-read the pending snapshot after every
decision and continue until none remain, with a connector-side cap against
degenerate re-gating (the browser smoke path uses the same pattern).

A response-time target is an operational objective under a stated workload.
Model/provider delays prevent an unconditional guarantee of an LLM decision by
a wall-clock deadline. Code can enforce deadline handling and expose a missed
decision rather than reporting success.

Use `created_at + 300s` as the target resolution time. The hard cutoff is the
earlier of `created_at + 1800s` and the runtime's `expires_at`. Submission must
leave a margin for gateway queueing, transport, and clock uncertainty; the
appropriate margin needs measurement. Record runtime-confirmed resolution
latency as well as local submission latency.

The runtime must enforce the hard cutoff when resolving the request. An HTTP
timeout cannot retract an approval already queued by the gateway. LunarWing's
native expiry checks provide this boundary when configured consistently; agents
with a longer expiry need explicit handling before claiming the thirty-minute
limit is enforced.

Handling after a five-minute target miss remains a policy question for the
experiment: whether to keep reviewing while the request is valid, notify the
operator, or apply another agreed fallback. Expired approvals must be reported
as expired, and a timeout must not be recorded as an approval.

### LunarWing support: live-validated vs still open

Validated by the slice run (no LunarWing changes needed for a v0 connector):

- Pending-gate persistence (`state/pending-gates.json` per tenant) is a usable
  durable snapshot holding every field `POST /api/chat/approval` needs.
- The request-scoped approval round trip resolves atomically and resumes the
  turn; observed timing (~15s send→gate armed, ~5s approve→re-gate, <8s to
  completion) sits far inside the five-minute target — the dominant cost is the
  agent's own reasoning, not the approval mechanics.

Still open:

- An authenticated HTTP snapshot of pending gates (including `created_at` and
  `expires_at`) so a remote connector need not read tenant-local files. Polling
  the file over SSH works for the experiment but is not a real API contract.
- Recoverable resolution evidence for approvals that close while the connector
  is down: a gate leaving the pending file proves closure, not *how* it closed
  (approved, denied, or expired).
- Preserve existing atomic request matching, authorization, and expiry checks
  through any new endpoint.
- Exercise the selected FirstMate primary harness while it is busy: wakeup,
  decision scheduling, and delivery must be demonstrated rather than inferred
  from watcher liveness.

## 6. Reconnecting-client investigation

Investigate reconnect behaviour for the fleet connector, the FirstMate session,
and any other client observing the same agent. A restored socket alone is not
proof that the client has an accurate view of pending work.

The intended recovery sequence is:

1. Reconnect to the manually registered endpoint and verify its identity.
2. Subscribe to live events and obtain an authoritative snapshot of active work
   and pending approvals. Deduplicate events overlapping the snapshot.
3. Reconcile requests against saved request IDs and runtime resolution evidence.
   Distinguish still pending, already resolved, expired, and outcome unknown.
4. Prioritize pending approvals by their original creation and expiry times.
   A request first recovered after five minutes is already overdue; reconnection
   must not give it a fresh five- or thirty-minute window.
5. Resume ordinary monitoring after reconciliation. A reported stream gap
   triggers the same recovery path.

The slice retired the lowest-level piece of this: `state/pending-gates.json`
served as the authoritative pending snapshot across the whole approval flow, no
SSE subscription required. What remains to exercise: a disconnect before an
approval event arrives, after decision submission but before resolution is
observed, and while another client resolves the same request (the pending file
then answers "still pending?" authoritatively but not "was it approved, denied,
or expired?"). Also exercise connector/supervisor restart, reconnect after
expiry, and original-deadline recovery (§5 clocks never reset on reconnect).
Avoid blind task resubmission; never infer approval from a missing pending
request alone.

## 7. Acceptance requirements

| ID | Required behaviour |
| --- | --- |
| F1 | Interact with at least five existing LunarWing test agents concurrently; the adapter contract also accommodates a mixed fleet of more than twelve agents. |
| F2 | Accept general-purpose tasks, with optional repository/workspace bindings and report/artifact outputs. |
| F3 | Supervisor can decompose work, assign agents, monitor progress, follow up, and consolidate outcomes with the agreed test-fleet autonomy. |
| F4 | Distinguish task receipt, running, waiting for approval, other blockers, stopping, completion, failure, and interruption. Connectivity is separate from task state. |
| F5 | Target confirmed approval resolution within five minutes under both single-request and five-agent burst tests, with a hard cutoff before thirty minutes or earlier runtime expiry; record target misses and expiry separately. |
| F6 | Restart/reconnect without losing the work inventory, pending-decision visibility, or known outcomes. Ambiguous submission does not produce an automatic duplicate attempt. |
| F7 | Preserve reports and attempt history when an agent is disconnected or manually removed from the fleet; reconnect against its registered identity. |
| F8 | Manually enroll existing agents through a short name/runtime/endpoint/credential-reference flow with a connection check. Membership is explicitly operator-managed. |
| F9 | Use a single documented supervisor/connector setup and a repeatable health check. Measure clean installation and incremental enrollment effort, including FirstMate's actual dependencies. |
| F10 | Support systemd-user and OpenRC deployment through the established lifecycle boundary. |
| F11 | Scope credentials and approval decisions to the selected fleet/agent instances; keep credentials out of prompts and task reports. |
| F12 | Bound concurrent task attempts and reconnect/retry activity, with cancellation outcomes confirmed against runtime state. |

F8/F9 are target behaviour to implement and measure, not current FirstMate
capabilities. The user has not selected numerical setup-time targets. F5 has
partial evidence: the single-agent slice resolved every approval well inside
five minutes; the five-agent burst remains untested.

## 8. Suggested exploratory checks

These checks have no calendar deadline and can also inform comparisons with
other tools the user is evaluating.

1. **Setup and manual enrollment:** inventory the required FirstMate tools and
   install steps. Manually register an existing test agent and assess the
   simplicity of both initial setup and adding another registration.
2. **Approval vertical slice** — **PASSED 2026-09-17** (tenant `antelope`;
   [`FIRSTMATE_SLICE_RESULTS.md`](FIRSTMATE_SLICE_RESULTS.md)): gate-arming
   probe, file-based discovery, request-scoped approve round trip, and a
   confirmed `Completed` turn, all pull-based with bearer auth. Not covered yet
   inside this check: denied, expired, duplicate-request, and intentionally
   disconnected cases, plus a FirstMate-driven (rather than script-driven)
   decision.
3. **Five-agent trial:** manually register at least five existing LunarWing test
   agents and exercise concurrent tasks with different outputs and a simultaneous
   approval burst. Include a busy supervisor and provider delay; measure the
   five-minute target and distinguish it from thirty-minute expiry.
4. **Reconnect recovery:** exercise the interruption points in §6, including
   decisions resolved by another client and an expiry during a disconnection.
   Check retained results, original deadlines, and unresolved-submission reporting.
5. **Hermes compatibility:** manually add an existing Hermes agent through the same fleet tools;
   verify native run/approval mapping before scaling to the final mixed fleet.

Use the observed approval behaviour, reconnect correctness, and manual enrollment
effort to assess this approach. A large FirstMate fork or a return to cumbersome
application-server setup would count against it in the ongoing comparison.

## 9. Open exploration questions

- Desired handling of a five-minute target miss while the request is still valid.
- Primary supervisor harness and whether routine tool categories may use
  explicit standing permissions rather than individual model reviews.
- Initial host layout and the acceptable measured setup/enrollment effort.
- Other candidate tools and how their enrollment, approval, and reconnect
  behaviour compares under the same requirements.
- Connector-side cap for multi-gate loops (a hard number, and what happens when
  it trips — hold for operator or fail the task).
- Whether to expose pending gates (with timestamps) as an authenticated HTTP
  endpoint now, or defer until remote connectors prove file-polling inadequate.

[fm-control]: https://github.com/kunchenguid/firstmate/blob/3eb5b6334a80e06083e3837f0032a5cec39b8e52/bin/fm-control-lib.sh#L61-L115
[fm-spawn]: https://github.com/kunchenguid/firstmate/blob/3eb5b6334a80e06083e3837f0032a5cec39b8e52/bin/fm-spawn.sh#L130-L146
[fm-extensions]: https://github.com/kunchenguid/firstmate/blob/3eb5b6334a80e06083e3837f0032a5cec39b8e52/docs/extension-bindings.md#scope-and-design
[fm-watch]: https://github.com/kunchenguid/firstmate/blob/3eb5b6334a80e06083e3837f0032a5cec39b8e52/bin/fm-watch.sh#L211-L223
[fm-remote]: https://github.com/kunchenguid/firstmate/blob/3eb5b6334a80e06083e3837f0032a5cec39b8e52/docs/remote-secondmates.md
[fm-bootstrap]: https://github.com/kunchenguid/firstmate/blob/3eb5b6334a80e06083e3837f0032a5cec39b8e52/bin/fm-bootstrap.sh#L909
[hermes-runs]: https://github.com/NousResearch/hermes-agent/blob/98f758ae7e8db83c2bb9214c3b35adf41df15f03/gateway/platforms/api_server_runs.py#L137-L150
[hermes-api]: https://github.com/NousResearch/hermes-agent/blob/98f758ae7e8db83c2bb9214c3b35adf41df15f03/website/docs/user-guide/features/api-server.md
