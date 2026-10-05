# OpenCode 2 External Worker — Exploration Plan (v2.1.2.0, goals item #17)

Status: **plan only** — no implementation is authorized by this document. It is the
exploration output for `docs/ops/history/opencode2extworkerideaandissuesandpotentialplan.md`
(the "opencode2 idea/issues/potential plan" note). Audience: whoever picks up the
implementation (Rarity/AJ per the source note).

## 1. Decision being made

Whether LunarWing should support OpenCode **v1 only**, **v2 only**, or **both**, and —
if both — exactly how a second `opencode2` external worker gets added without colliding
with the proven v1 worker.

**Recommendation up front: support both, as two separate worker lanes, with v1 staying
the default until upstream v2 earns trust.** This matches the source note. Rationale and
the concrete collision inventory are below.

### Why not "v2 only, upgrade in place"

Two independent reasons, both already demonstrated in this repo's history:

1. **v2 is beta in practice, not stable.** Upstream's own migration doc says
   "OpenCode 2.0 is in beta. Features may break unintentionally, and the server and
   plugin APIs may continue to change," and the terminal client ships on a `beta`
   dist-tag. The source note calls 2.0.3/2.0.4 "stable in marketing terms only" and
   describes the community doing the debugging release-testing should have caught.
2. **We already got burned by an in-place opencode bump.** The pre-Ma-ai regression
   (worker upgraded, integration broke in testing, worker downgraded/pinned to the
   last-known-good) means the current pinned v1 worker at **v1.18.25** is the only
   proven-good opencode lane. Replacing it with an unproven v2 repeats that mistake in
   production instead of testing. v1.18.29 already shipped two live regressions (model
   metadata shape crash + silent ai-sdk stream death); it was reverted for cause — see
   the pin note in `opencode4lunarwing/Dockerfile:1-9`.

So v1 keeps its pin and its default status. v2 arrives as a *new, separate* worker that
tenants opt into explicitly.

## 2. What a worker actually is in LunarWing (ground truth)

An external worker is a persistent container speaking the `lunarwing-agent-v1`
WebSocket subprotocol. The daemon learns about it from
`[[sandbox.external_workers]]` entries in the tenant `config.toml`:

```
name  = "opencode"                       # ← the slug; matches create_job(mode: "...")
url   = "ws://127.0.0.1:<opencode_wss>/ws/agent"
auth_token = "…"
timeout_ms = …
```

- **Daemon side is fully generic.** `ic/src/settings.rs:884` (`ExternalWorkerSettings`)
  holds any named worker. `ic/src/orchestrator/mod.rs:175` builds an
  `ExternalWorkerManager` from whatever the list contains;
  `ic/src/orchestrator/external_worker.rs` has **no** hardcoded opencode reference
  (only comments noting TS bridges distinguish absent-vs-null `context`). The dispatch
  surface is `create_job(title, description, mode)` in `ic/src/tools/builtin/job.rs`,
  where `mode` is just the worker name string. **A second worker named `opencode2` needs
  zero Rust changes** — it is one more `[[sandbox.external_workers]]` entry with a
  distinct name, URL port, and timeout.
- **mt-admin is where the per-worker specificity lives** (see §3). Adding a worker is,
  per the source note, much cheaper since the ~20K-line script was decomposed into
  `ic/scripts/mt-admin-lib/NN-*.sh` modules — but "add a worker" is still several
  coordinated edits, not a single config entry.
- The opencode worker container = Bun/TypeScript bridge
  (`opencode4lunarwing/scripts/lunarwing_bridge.ts`) translating between the opencode
  headless server (`opencode serve` on internal `127.0.0.1:4096`, via
  `@opencode-ai/sdk` v2) and the `lunarwing-agent-v1` websocket the daemon dials. Three
  modes in `entrypoint.sh`: `websocket` (default), `cli`, `acp`.

## 3. Collision-surface inventory (step 1 of the source note, done)

The source note's first action was "grep the codebase for `opencode` collision
surfaces." That scan is done here. Verdict: **the bare slug `opencode2` is clean** —
no regex matcher, alias, or routing table pattern-matches on `opencode` with a
wildcard/regex; every reference is an exact literal. But the name `opencode` is spread
across many coordinated literals, so each must be mirrored deliberately for `opencode2`.

Daemon (Rust): only comments/example strings —
`ic/src/settings.rs:887`, `ic/src/tools/builtin/job.rs:871` (comment "opencode, kimi,
pebble" + doc/tests). **No code change needed for the daemon.**

| Surface | File / location | What must be mirrored for `opencode2` |
|---|---|---|
| Worker name allow-list (pattern-match guard) | `ic/scripts/mt-admin-lib/03-ports-registry.sh:985,1024` (`pebble\|opencode\|kimi` case arms) and `ic/scripts/mt-admin-lib/09-worker-config.sh:1529` | add `opencode2` to the `pebble|opencode|kimi` acceptance lists |
| Worker enable/iterate loops | `03-ports-registry.sh:609` (`for _w in pebble opencode kimi`), `07-units-systemd.sh:952`, `08-units-openrc.sh:1294`; lifecycle stop loop `04-tenant-lifecycle.sh:1014`-area `stop_tenant_opencode`; `04-tenant-lifecycle.sh:572-574` | add `opencode2` to each loop / add `stop_tenant_opencode2` |
| Ports registry: dedicated slots | `opencode_wss` = `extended_base+7`, `opencode_health` = +8 (`03-ports-registry.sh:690-691`) | allocate `opencode2_wss`/`opencode2_health`, needs a **ports-registry version bump** (next reserved slots; the v11 migration at `03-ports-registry.sh:382` is the template) |
| Per-worker port env var | `OPENCODE_WSS_PORT` written in `06-env-generation.sh:857`, re-added `:2064-2071` | `OPENCODE2_WSS_PORT` |
| Per-tenant LLM overrides | `OPENCODE_MODEL` / `OPENCODE_BASE_URL` env vars (threaded through `04-tenant-lifecycle.sh:493-557`, `06-env-generation.sh:612-929`, quadlet render `07-units-systemd.sh:546-549`) | `OPENCODE2_MODEL` / `OPENCODE2_BASE_URL` |
| Container / unit names | `lunarwing-opencode-<tenant>` (+ `.container` quadlet, `-sup` OpenRC supervisor) in `07-units-systemd.sh:915,944`, `08-units-openrc.sh:1353,1409` | `lunarwing-opencode2-<tenant>` (+ `-sup`) |
| Image name + builder | `lunarwing-worker-opencode:latest`, `build_opencode_worker` (`05-tenant-build.sh:1056`), source dir `opencode4lunarwing` (`00-common.sh`, `shared_worker_source_root` at `05-tenant-build.sh:954`) | `lunarwing-worker-opencode2:latest`, `build_opencode2_worker`, new dir `opencode2.4lunarwing` (or `opencode2-4-lunarwing`) |
| Worker dir / userns constants | `OPENCODE_CONTAINER_UID=10001`, `OPENCODE_USERNS`, `OPENCODE_ROOTLESS_LAUNCH_CONTRACT` (`ic/scripts/lunarwing-mt-admin.sh:150-153`; kimi is 10002 at `:155-158`) | a **distinct** `OPENCODE2_CONTAINER_UID=10003` so two workers on one tenant keep separate userns identities (opencode=10001, kimi=10002 are already distinct) |
| `configure-opencode` admin command | `configure-opencode <name> [--model --base-url]` (`09-worker-config.sh:663`, `:1169`) | `configure-opencode2` |
| `--with-opencode` CLI flags | add-tenant / build-tenant / build-all flags (`ic/scripts/lunarwing-mt-admin.sh` dispatch ~`:542-692`) | `--with-opencode2` |
| Tenant-name reserved prefix | `opencode-*` is a reserved tenant-name prefix (`04-tenant-lifecycle.sh:518-519`) | add `opencode2-*` |
| Agent-facing description text | `create_job` mode description in `ic/src/tools/builtin/job.rs:1297` ("opencode" enum/description text) | optional: mention `opencode2` so agents can route (see §5) |

None of these is a *blocking* collision — there is no regex or alias that would misfire
on `opencode2`. The work is mechanical mirroring plus one ports-registry migration.

## 4. The ACP/bridge question (step 2 of the source note, researched)

The source note asks whether v2's ACP divergence means the v1 websocket bridge
(`lunarwing_bridge.ts` → `opencode serve` + `@opencode-ai/sdk/v2`) is reusable, "or even
necessary."

Findings (upstream docs + ACP migration guide, fetched live 2026-09-15):

1. **The v1 server API the bridge uses is intentionally broken in v2.** OpenCode's
   migrate-v1 doc: "OpenCode 2 has a revised, more ergonomic server API and a new set of
   clients. Integrations that call the V1 server API must migrate to the V2 API… Use
   `@opencode-ai/client`… still being finalized during beta." So the current bridge's
   server-API calls **cannot** be reused unchanged against an opencode2 binary. The
   *shape* of the bridge (ws server → headless local opencode → stream events back) is
   still the right shape; the SDK calls and config format must be re-pointed.

2. **Config format changed too.** V2 replaces the agent/mode/provider config shape
   (`agents` map, renamed provider/endpoint fields, `server` moved to explicit options,
   `logLevel` → `OPENCODE_LOG_LEVEL`, TUI config → `cli.json`). Our
   `config/opencode.json` and the entrypoint override-injection Python
   (`entrypoint.sh:98-128`, the `provider/model` model-ref materialization) would need a
   v2 variant. Note the source note's observation that "v1 now reads supported v2 config
   fields" — that helps mixed configs on the *v1* line, but the v2 binary wants v2
   config, so a separate `opencode2.json` is cleaner than overloading.

3. **ACP is *not* the right hook for this worker.** `entrypoint.sh` already has an `acp`
   mode that just `exec opencode acp` (stdio nd-JSON, per the CLI docs). But LunarWing's
   worker contract is the `lunarwing-agent-v1` **WebSocket** protocol — the daemon dials
   a persistent ws, sends `task_request` envelopes, receives streaming `task_progress`.
   ACP is a stdio, client↔editor protocol (v2 draft adds `state_update`,
   `session/resume`, removes `fs/*`+`terminal/*` client capabilities, gated behind
   `OPENCODE_EXPERIMENTAL_ACP_V2`). Nothing in the daemon speaks ACP, and the opencode2
   ACP-v2 support is explicitly experimental/flag-gated. **Conclusion: keep the
   websocket-bridge architecture; do not build the worker on ACP.** The v2 worker is
   a *new bridge* (`lunarwing_bridge.ts` adapted to `@opencode-ai/client` + v2 config),
   not an ACP client. The bridge stays necessary; ACP is a red herring for this
   integration.

So the bridge is **reusable in architecture, not in code**: copy the TS bridge, swap the
SDK import to `@opencode-ai/client`, re-map session/event handling to the v2 API, and
generate v2 config. Effort is a focused port, not a rewrite.

## 5. Slug decision and agent confusion (steps 3 + the "agent confusion" issue)

**Slug: `opencode2`.** It matches the shell command, and §3 shows it is clean — no
wildcard/regex collision anywhere. `opencode-v2` is only a fallback if a real collision
surfaces later (none did).

The genuine risk the source note flags is **agent routing confusion**, and it's real:
with both configured, the agent sees two `create_job` modes — `opencode` and `opencode2`
— and must pick. The slug alone doesn't carry "which is proven." Mitigations, in order
of preference:

1. **Don't configure both on the same tenant initially.** Cheapest, removes the problem
   for the transition. Default: v2 only goes on scratch/eval tenants.
2. **Version-explicit description text.** Extend the `create_job` mode description in
   `ic/src/tools/builtin/job.rs` so each configured worker is described with its version
   ("opencode (OpenCode v1.18.25, proven)" vs "opencode2 (OpenCode 2.x, beta)"), so the
   agent reasons over an explicit signal rather than the bare slug.
3. **Capability/routing preference (heavier, optional).** If both must coexist
   long-term, add a worker-manifest capability hint so routing prefers v1 during the
   transition. Not required for the first ship; revisit if dual-tenant becomes common.

**Fork-and-rename was rejected** (per the source note): carrying renames across every
upstream version bump is an ongoing maintenance fork, only worth it if the conflict is
real. §3 shows it is not — no collision forces a rename of the tool itself.

## 6. Recommended plan — ordered phases

Phase A (research, this doc): collision scan ✅ done (§3), ACP/bridge answer ✅ done (§4),
slug ✅ `opencode2` decided (§5).

Phase B (worker image + bridge):
1. Create `opencode2.4lunarwing/` (name TBD) as a copy of `opencode4lunarwing/`, pinned
   the same way: `ARG OPENCODE_VERSION=v2.<pin>` + `ARG OPENCODE_REF=<full 40-char
   SHA>`, tag+SHA moved together, frozen Bun lock mandatory, base images pinned — the
   v1 Dockerfile pin discipline (`opencode4lunarwing/Dockerfile:1-42`) carries over
   verbatim. Bake `OPENCODE_IMAGE_REF="v2.x@<sha>"` and surface it as `opencode2_ref` in
   the health payload.
2. Port the TS bridge to `@opencode-ai/client`; keep the `lunarwing-agent-v1` envelope
   byte-identical (shared with pebble/kimi — must not break compat,
   `opencode4lunarwing/AGENTS.md`). Keep internal serve port 4096 unexposed, health on
   8443, ws on 9090. Keep the health server (orchestration depends on it); fix the
   copy-pasted `CODEX_MODE`/`CODEX_VERSION` env names while copying (cosmetic).
3. Generate a v2 `opencode2.json` config and adapt the entrypoint override injection.

Phase C (mt-admin wiring): mirror every §3 surface — `build_opencode2_worker`,
`start/stop_tenant_opencode2`, `configure_opencode2`, `--with-opencode2`,
`opencode2_wss`/`opencode2_health` ports (registry v-bump + migration off the next
reserved slots, modeled on the v11 migration), `OPENCODE2_*` env vars,
`lunarwing-opencode2-<tenant>` units/quadlet/`-sup` (both systemd and OpenRC — **both
first-class**), `opencode2-*` reserved prefix, distinct container UID. Keep
`check-mt-admin.sh` (shellcheck) green.

Phase D (tests — Rarity + AJ per the source note): extend the existing worker harness
tests (`ic/scripts/tests/test-mt-admin-worker-lifecycle.sh`, `test-mt-admin-kimi.sh`
patterns) with `opencode2`; a live create_job(mode:"opencode2") dispatch test; upgrade/
re-run idempotency (the `ensure_external_worker_config` re-run path); both-init unit
render smoke via check-mt-admin.

Phase E (rollout): v1 stays default everywhere. v2 to eval tenants only. Let the 2.0.x
patch stack settle; flip default and sunset v1 only when v2 is genuinely stable — on our
schedule, not upstream's (per the source note).

## 7. Risks / open questions

- **Upstream churn**: v2 server + plugin APIs "may continue to change" (upstream's own
  words) — pin hard, re-verify the SHA on every bump, treat each v2 bump like the
  v1.18.29 incident until proven otherwise.
- **Bridge porting risk**: exact v2 session/event API parity (streaming `task_progress`
  deltas, cancel semantics) needs a spike in Phase B step 2; the v1.18.29 ai-sdk
  stream-path regression is the cautionary pattern.
- **Shared-auth assumption**: `ensure_external_worker_config` reuses `GATEWAY_AUTH_TOKEN`
  as the worker auth token; opencode2 inherits this — confirm that's still desired.
- **Config-field drift**: v1 reading v2 config fields helps mixed setups on v1, but keep
  `opencode.json` (v1) and `opencode2.json` (v2) as separate mounted configs.
- **Sunset date for v1**: deliberately not set; revisit per the source note once 2.0.x
  is stable in practice.
