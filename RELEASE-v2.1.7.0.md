# Release Notes for LunarWing v2.1.7.0

## Codename: Shiranui (不知火)

**DRAFT — refreshed 2026-10-28 against tree `74e87f1bb`; not a release or deployment sign-off.**

**Release date:** To be set when tagged

> Lights on dark water,<br>
> no fire, yet the way opens,<br>
> we steer by their glow. 不知火

*Lights on the water, with no flame behind them.*

Tsuno reached beyond the old outline. **Shiranui makes the passage trustworthy.** This release concentrates on work beneath the visible surface: dependencies and vendored sources refreshed, security findings re-checked, network boundaries tightened, and mission and worker behavior made more truthful.

These are lights to steer by: mostly invisible from the shore, but essential before crossing into what comes next.

## Highlights

- **Supply-chain maintenance, end to end.** All eleven LunarWing-owned release packages move to `2.1.7`; registry packages, external crates, lockfiles, and vendored sources advance together. The post-sweep audit re-ran advisory, ban, license, and source gates. Wasmtime's cache uses a local patch selecting the fixed zstd chain; Pebble's source and image-override locks now pin `pulldown-cmark 0.13.5` for the wikilink memory-exhaustion vulnerability (RUSTSEC-2026-0414). Internal licenses were normalized to AGPL, preserving Cavepony's upstream MIT terms, and the unpinned `tui/node_modules` tree was removed (WI#488).
- **Stronger WASM HTTP boundaries (WI#429, WI#430, WI#432).** Tools and channels share tighter private-target and credential-redaction guards. Loopback exemptions are exact-origin and channel-specific, direct requests pin vetted DNS answers, encoded and cross-field credential echoes are scrubbed, and ambiguous transfer-coding shapes are refused. Empty-body HEAD, 204, and 304 responses no longer fail the opaque-coding guard.
- **Worker egress controls land, with rollout limits (WI#424).** mt-admin provisions per-tenant networks and nftables allowlists with static host mappings, checks rules around managed starts, and recreates stale containers. Host health repeats the read-only audit; OpenCode 2's smoke exercises the real entrypoint as the runtime user. IP-only allowlists no longer make launch-argument generation report failure after success. Fresh-rootless startup and backend coverage still need the fixes listed below.
- **Engine V2 history and gates behave consistently (WI#442, WI#472, WI#473, WI#481).** A bounded, one-time import brings eligible v1 history into newly created Engine V2 surfaces without backfilling existing surfaces or reviving `/clear`. Thread-less approval requests resolve their owning gates server-side, terminal turns reap turn-bound gates, and expired gateway-gate replies reach the owning thread.
- **Mission and turn lifecycle becomes clearer (WI#321, WI#323, WI#394).** Duplicate default projects resolve deterministically, dropped boot records produce bounded warnings, and mission schedulers emit liveness, fire, and overdue signals without user payloads. Boot recovery also heals a settled-stop dedup window without replaying the lost run.
- **External workers report progress honestly (WI#386, WI#475, WI#476, WI#477).** Kimi and Pebble expose request waits, elapsed time, retry notices, and per-attempt LLM deadlines. Self-repair uses a separate external-worker idle threshold and heartbeat baseline, stops claiming no-op recovery, and emits manual-intervention alerts once per job per boot. OpenCode failed-attempt text cannot masquerade as successful output. External-job cancellation synchronizes shared context, and job details expose `stuck` and `repair_attempts` when available.
- **Worker credentials and images fail closed (WI#425, WI#440, WI#474, WI#495).** OpenCode lanes reject unsupported credential grants before secret lookup or dispatch. Image verification checks exported filesystem metadata and runtime configuration, including non-writable, root-owned OpenCode 2 dependencies. BuildKit and its policy parser move to `v0.34.1`. Token generation requires valid OS randomness. OpenCode 2 smoke state stays in its scratch tree, with runtime-user process cleanup; the clean-rebuild gate remains a separate requirement.
- **Redaction and completion backstops improve (WI#355, WI#383).** Secret-safe truncation walks token boundaries and rescans retained text. Harness write-back scrubs staged paths and promotion summaries, refuses duplicate completion side effects, and reports a dedicated Finalizing refusal. These are targeted fixes, not closure of the remaining harness-redaction and lifecycle backlog.
- **Jobs and operator workflows are easier to manage.** The Jobs table shows mode; `GET /api/jobs` reports lane attribution with case-insensitive filtering (WI#479). GitLab blob search accepts branch, tag, or SHA refs (WI#315). Workspace writes reject control-bearing paths (WI#493), Pebble rejects malformed pasted configuration (WI#471), and mount protection recognizes structural tenant layouts (WI#441). The inline v8 port migration now backs up existing registries and refuses collisions or parse errors before replacement (WI#379).

See the [v2.1.6.0 release notes](docs/releases/RELEASE-v2.1.6.0.md) for the previous compatibility baseline. Historical notes remain a record of that release, not a current work-item status list.

## Compatibility

| Area | v2.1.7.0 |
|---|---|
| Owned release packages | Eleven packages at `2.1.7` |
| Rust / database / ports | Rust 2024, MSRV `1.96.1`; SQL migrations remain at V28; multi-tenant port registry remains v17 |
| Channel registry | darkirc `0.1.13`, multica `0.1.16`, weechat `0.2.14`, xmpp `0.1.19` |
| Tool registry | cavepony-tool `0.4.8`, gitlab `0.1.11`, gotify `0.1.4`, llm-context `0.1.6`, multica-bridge `0.1.7`, ssh `0.1.5`, vision-analyze `0.1.7`, voice-transcribe `0.1.5`, web-search `0.2.7` |
| External workers | Kimi Code `2.3.0`, OpenCode `v1.18.25`, OpenCode 2 `v2.1.2`, Pebble ref `2d0f3786…`; upstream versions remain unchanged from v2.1.6.0, but worker code, patches, and locks advance |
| Worker build policy | BuildKit `v0.34.1-rootless` and matching `v0.34.1` Dockerfile parser module; Go `1.26.10` guard toolchain |

The registry bumps require rebuilt WASM artifacts; they do not by themselves change channel protocols. Pebble's image-override lock digest is now `19f4e96c…`, independently of its unchanged upstream source ref. Proposed worker-version upgrades are not shipped pins until they land.

## Upgrade Notes

1. **Back up and deploy components together.** Preserve the database, tenant state, port registry, workspaces, and channel state. Rebuild and restart the daemon and XMPP bridge, then rebuild/reinstall all four channel and nine tool registry extensions. No SQL or port-registry migration is introduced by this release.
2. **Gate worker rollout on verified candidates.** Rebuild images to deploy executor, bridge, dependency, timeout, and image-policy changes; require their protected build, scan, and applicable exposure-verification gates before promotion. The October 29 item-95 follow-up moves all four Debian snapshots to `20261024T011300Z` for patched Brotli OS packages (CVE-2026-111497) and pins account shadow dates to the source epoch, preventing UTC-midnight rebuild drift; WI#514 completes the Brotli fix by pinning Node `22.23.4` (kimi) and Bun `1.4.4` (both OpenCode lanes) for the bundled decoders, so existing containers need rebuilding and recreation. Protected candidate/scan acceptance remains pending, and neither change nor OpenCode 2's smoke fixes certify whole-image reproducibility (WI#510). Preserve its database before changing image versions.
3. **Review egress before restarting tenants.** Use [mt-admin's worker runbook](docs/ops/WORKER-CONTAINERS.md) and check `worker-egress <tenant>` after rollout; old running containers are not retroactively filtered by a source update. Pebble's backend needs an explicit `configure-worker-egress <tenant> --allow <backend-host>:443` grant and restart for static host mappings (WI#506). Do not bypass the fail-closed startup errors below.
4. **Review credentials and idle thresholds.** OpenCode/OpenCode 2 jobs must omit `credentials` and non-empty `context.environment`; managed entries gain `supports_credentials = false`, while unmanaged or renamed entries need that setting explicitly, followed by a daemon restart. `AGENT_EXTERNAL_STUCK_THRESHOLD_SECS` defaults to 1800 seconds, separately from the 300-second local threshold; it is an idle detector, not an LLM deadline or automatic task restart.
5. **Expect bounded Engine V2 history import.** The first message on a newly created Engine V2 surface may import eligible pre-switch history once. Existing surfaces are not backfilled; failed imports are best-effort. Engine V2 routing remains opt-in for XMPP, DarkIRC, and WeeChat, not a replacement of every v1 channel loop.
6. **Review dated security evidence.** The [v2.1.7.0 cargo-deny audit](docs/ops/v2.1.7.0_CARGO_DENY_AUDIT.md) includes the post-sweep results and the October 28 Pebble fix. Accepted advisories and graph exclusions still apply; neither this draft nor an older green audit certifies a running binary. Rebuild and repeat release gates before tagging.

## Remaining Limitations

Tracker states were checked on 2026-10-28; landed changes above describe tree `74e87f1bb`, not pending branches.

- **Worker isolation is unfinished.** The full bridge/task-container split remains WI#454, and Pebble's non-root image acceptance remains open under WI#425. Environment scrubbing, grant rejection, and naive-use exposure guards are not substitutes for that boundary or enforced egress.
- **Egress rollout has known blockers.** A fresh rootless Podman tenant can load and verify rules in different namespaces and refuse its first start (WI#504). An OpenRC daemon can inherit the lifecycle-lock descriptor, leaving later commands blocked after an abort (WI#505). Pebble's backend is not automatically covered (WI#506). The [mistyfly report](docs/ops/ITEM99_TEST_TENANT_MISTYFLY.md) records these separately from the fixed IP-only launch-status bug.
- **Worker availability and provenance still need work.** OpenCode v1's WI#333 startup failure is not claimed fixed; the stale-image loop can abort later lanes (WI#469), and boot-time worker liveness diagnostics remain open (WI#464/WI#512). Pebble image builds still use the operator checkout rather than the tenant source root (WI#507). OpenCode 2's binary reproducibility remains open under WI#510; do not promote a digest mismatch.
- **Stuck detection is not recovery.** WI#475 is closed, but the daemon still cannot retry an external task; a heartbeating executor can remain alive while its LLM is wedged, leaving the outer job timeout as the backstop. The cancellation fix still excludes container-sandbox context and the delivered-cancel/database-update race (WI#476).
- **Security follow-ups remain.** Engine V2 still needs an ownership guard for caller-supplied thread UUIDs (WI#502). A configured HTTP proxy bypasses direct-request vetted-DNS pinning (WI#503). Reserved workspace paths remain incomplete (WI#455/WI#492), and helper-path execution and truncation residuals remain WI#487/WI#491.
- **Harness fixes are scoped.** WI#383 remains open for upstream-truncation and structured-field redaction gaps, orphan-directory cleanup, and unbounded Finalizing promotion. Duplicate default-project resolution does not prevent the concurrent creation race (WI#490).
- The [v2.1.6.0 security and deployment boundaries](docs/releases/RELEASE-v2.1.6.0.md#security-deployment-and-release-limitations) remain relevant where not superseded above. Worker tool grants remain empty by default and must be assigned deliberately.

---

> Beyond charted lines,<br>
> small lights show the open sea,<br>
> we sail into dawn.
