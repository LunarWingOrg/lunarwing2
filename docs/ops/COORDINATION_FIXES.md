# Coordination fixes

## Update this file when new issues with coordination arise

## Issue #1: untagged inbound traffic disabled receiving agent

Observed on XMPP coordination room: short untagged messages reached model, model
returned normal persona prose, and `coordination.tag_schema` assigned all three
outbound failures to local agent. Local agent then auto-disabled for 24 hours.
Tag matcher itself was correct; it requires case-insensitive `TAG:` at start.

Root causes:

- tag schema ran only at `transformResponse`, not `beforeInbound`;
- installable XMPP bridge collapsed MUC sender and room target into `user_id`;
- model received no coordination-specific response contract, so persona prose
  commonly preceded required tag.

Fix:

- validate inbound schema before model execution at priority 14;
- attribute inbound violations to `sender_id`, while model-output violations
  remain attributed once to local agent at `transformResponse`;
- preserve bridge `sender_id` with backward-compatible payload decoding;
- inject exact room-scoped system instruction requiring one-line tagged output.

Operator recovery after deploying fix (run with tenant environment loaded; never
print token):

```bash
curl -fsS -X POST \
  -H "Authorization: Bearer ${COORD_ADMIN_TOKEN:?}" \
  "http://127.0.0.1:${COORD_ADMIN_PORT:?}/coord/clear/alpha"
```

Then test with valid input such as `QUERY: @derpy report status`.

## Issue #2: empty aliases silently disabled engagement

Tenant provisioning wrote an empty `COORD_AGENT_ALIASES`. Once coordination was
turned on, valid messages addressed with the configured `COORD_AGENT_ID` still
failed engagement unless an operator separately populated aliases.

Fix: the runtime now derives lowercase full-ID, `@ID`, and JID local-part aliases
from every valid `COORD_AGENT_ID`, while preserving and deduplicating explicit
aliases. Matching observes identifier boundaries, so a short alias such as `aj`
no longer triggers on words such as `adjust`.

## Issue #3: Engine V2 lacked the response contract

The legacy dispatcher injected the room-scoped tagged-output instruction, but
Engine V2 only received generic channel budgets. Its untagged final output was
then rejected by the shared transform and could auto-disable the local agent.

Fix: Engine V2 composes the same coordination instruction into its per-thread
channel context without replacing transport budgets.

## Issue #4: first-responder behavior did not coordinate daemons

The original claim map was process-local, used an implementation-defined hash,
and accepted repeat claims by the same daemon. Normal multi-tenant deployment
could therefore emit duplicate responses, while transport replay could wake one
daemon repeatedly.

Fix:

- `COORD_RESPONDER_ROSTER` enables rendezvous-hash election for unaddressed
  `QUERY:` messages; identical rosters independently choose exactly one daemon;
- stable SHA-256 correlation covers normalized room, sender, and content;
- explicit mentions retain direct routing;
- a process-local TTL claim suppresses repeated delivery to the elected daemon,
  including same-agent replay;
- mt-admin provisioning and upgrade backfill preserve the roster setting.

## Issue #5: auto-disable and master switch were incomplete

Three-strike enforcement stopped Engine V2 effects, but legacy chat, reflex, and
approval-resume paths could still execute. Separately, configuring
`COORD_CHANNELS` could activate the coalescer while `COORD_ENABLED=false`.

Fix: the common inbound boundary now silently suppresses external activity from
a disabled local agent before any execution path while allowing trusted internal
notifications through. Engine V2 retains its effect-time defense in depth.
Coalescer creation now additionally requires actionable coordination config, so
`COORD_ENABLED` is the real master switch.

## Issue #6: Phase 1 rules were not deployed automatically

The canonical secret/newline/length bundle lived under `ic/config/hooks/`, but
startup loaded only workspace hook files. No provisioning path copied it into a
tenant workspace, so an enabled coordination deployment could silently lack all
seven Phase 1 rules. The Rust entropy hook was supplementary, making the missing
bundle a real security gap.

Fix: `bootstrap_hooks()` now embeds, scopes, and registers the canonical bundle
whenever coordination config is actionable. Legacy workspace copies are ignored
to avoid duplicate enforcement. Tests verify disabled no-op, seven-rule
registration, in-scope secret rejection, and out-of-scope pass-through.

## Issue #7: malformed coalescer settings could crash or disable behavior

`COALESCE_WINDOW_SEC=NaN`/infinity/overflow reached
`Duration::from_secs_f64`, which panics, while `COALESCE_MAX_MERGES=0` produced
nonsensical immediate-flush semantics. The parser now accepts only finite,
representable windows (minimum 100 ms) and positive merge counts, otherwise
retaining safe defaults.

## Issue #8: secret filters disagreed on valid tokens and strike accounting

The declarative priority-13 regex rejected every 40-character alphanumeric token,
including exact git SHAs that the Rust entropy hook explicitly allowed.
Conversely, the Rust helper treated any slash-containing token as a path, so a
high-entropy secret with an embedded slash bypassed entropy. Declarative secret
rejections also did not update the three-strike store.

Fix: the stateful priority-5 hook is now the strike-aware authority for explicit
credential assignments, private-key markers, JWTs, and entropy across inbound,
outbound, and transform events. Inbound detections strike the remote sender;
local output strikes `COORD_AGENT_ID` once. Entropy allowlisting is limited to
exact 40-hex SHAs, explicit path prefixes, and absolute HTTP(S) URLs. The
conflicting blanket base64 rule was removed from both canonical and reference
bundles.
