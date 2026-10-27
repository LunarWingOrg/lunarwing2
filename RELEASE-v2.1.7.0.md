# Release Notes for LunarWing v2.1.7.0

## Codename: Shiranui (不知火)

**Release date:** 2026-10-27

> Lights on dark water,  
> no fire, yet the way opens,  
> we steer by their glow. 不知火

*Lights on the water, with no flame behind them.*

Tsuno reached beyond the old outline. **Shiranui makes the passage trustworthy.** This release concentrates on work beneath the visible surface: dependencies and vendored sources refreshed, security findings re-checked, network boundaries tightened, and mission and worker behavior made more truthful.

These are lights to steer by: mostly invisible from the shore, but essential before crossing into what comes next.

## Highlights

- **Supply-chain maintenance, end to end.** All eleven LunarWing-owned release packages move to `2.1.7`; channel and tool registry packages advance together; the external-crate sweep, lockfiles, and committed vendor tree were refreshed and checked across feature combinations. A post-sweep audit re-ran the advisory, ban, license, and source gates. Internal crate license metadata was also normalized to AGPL, with Cavepony's upstream MIT terms preserved through its dual-license declaration.
- **Stronger WASM HTTP boundaries (WI#429, WI#430, WI#432).** Tools and channels now share tighter private-target and credential-redaction guards. Loopback exemptions are exact-origin and channel-specific, vetted DNS answers are pinned for the request, encoded and cross-field credential echoes are scrubbed, and ambiguous transfer-coding shapes are refused. Empty-body HEAD, 204, and 304 responses no longer fail the opaque-coding guard.
- **Engine V2 inherits the right history (WI#442).** A bounded, one-time import brings eligible v1 user/assistant history into newly created Engine V2 surfaces. Ownership checks prevent cross-user imports, existing surfaces are not backfilled, and `/clear` stays cleared. Non-opted channels remain on their existing path.
- **Mission and turn lifecycle becomes clearer (WI#321, WI#323, WI#394, WI#473).** Duplicate default projects resolve deterministically, dropped boot records produce bounded warnings, and mission schedulers emit liveness, fire, and overdue signals without user payloads. Boot recovery heals a settled-stop dedup window, while terminal turns immediately reap their pending gates without disturbing paused or authentication-fallback gates.
- **External workers report progress honestly (WI#386, WI#476, WI#477).** Kimi and Pebble expose indexed request waits, elapsed time, retry notices, and configurable per-attempt LLM deadlines. OpenCode diagnostics and failed-attempt text can no longer masquerade as successful output. External-job cancellation now updates the shared context before reporting success, so stuck-job detection and the Jobs view agree with the persisted state.
- **Worker credentials and images fail closed (WI#425, WI#474, WI#495).** OpenCode lanes explicitly reject unsupported credential grants before secret lookup or dispatch instead of accepting them as a no-op. Worker-image verification checks exported filesystem metadata and runtime configuration, including root-owned, non-writable OpenCode 2 dependencies. Protected builds and the Dockerfile policy parser move to BuildKit `v0.34.1`.
- **Jobs are easier to attribute (WI#479).** `GET /api/jobs` can report `job_mode` and `worker_name`, with case-insensitive `?mode=` filtering for worker, harness, and external lanes. The web Jobs table gains a Mode column without adding a per-row database lookup or weakening user scoping.
- **Operator-facing polish.** The GitLab tool supports branch-, tag-, and SHA-scoped blob search and gives actionable parameter errors (WI#315). Workspace writes reject control-bearing paths while legacy stored names remain recoverable through escaped displays (WI#493). Pebble configuration rejects pasted assignments, surrounding quotes, padding, and line breaks before touching tenant state (WI#471). Sandbox mount protection recognizes structural multi-tenant layouts and warns once when protection degrades (WI#441).

See the [v2.1.6.0 release notes](docs/releases/RELEASE-v2.1.6.0.md) for the previous compatibility baseline.

## Compatibility

| Area | v2.1.7.0 |
|---|---|
| Owned release packages | Eleven packages at `2.1.7` |
| Rust / database / ports | Rust 2024, MSRV `1.96.1`; SQL migrations remain at V28; multi-tenant port registry remains v17 |
| Channel registry | darkirc `0.1.13`, multica `0.1.16`, weechat `0.2.14`, xmpp `0.1.19` |
| Tool registry | cavepony-tool `0.4.8`, gitlab `0.1.11`, gotify `0.1.4`, llm-context `0.1.6`, multica-bridge `0.1.7`, ssh `0.1.5`, vision-analyze `0.1.7`, voice-transcribe `0.1.5`, web-search `0.2.7` |
| External workers | Kimi Code `2.3.0`, OpenCode `v1.18.25`, OpenCode 2 `v2.1.2`, Pebble ref `2d0f3786…`; upstream pins are unchanged from v2.1.6.0, while worker-side fixes and instrumentation advance |
| Worker build policy | BuildKit `v0.34.1-rootless` and matching `v0.34.1` Dockerfile parser module; Go `1.26.10` guard toolchain |

The registry version bumps require rebuilt WASM artifacts; they do not by themselves change channel protocols.

## Upgrade Notes

1. **Back up and deploy components together.** Preserve the database, tenant state, port registry, workspaces, and channel state. Rebuild and restart the daemon and XMPP bridge, then rebuild/reinstall all four channel and nine tool registry extensions. No SQL or port-registry migration is introduced by this release.
2. **Rebuild worker images through the normal promotion path.** Build and scan fresh candidates, promote them, and restart tenants so the executor, bridge, timeout, heartbeat, and image-policy changes take effect. Preserve OpenCode 2 state before changing image versions because upstream may migrate its database.
3. **Review OpenCode credential use.** Do not submit `credentials` or a non-empty `context.environment` to OpenCode or OpenCode 2 jobs; omit the grants or use Kimi/Pebble. Managed configurations gain `supports_credentials = false`; restart the daemon after the managed migration. Unmanaged or renamed entries need the explicit setting and a restart.
4. **Expect bounded Engine V2 history import.** The first message on a newly created Engine V2 surface may import eligible pre-switch history once. Existing surfaces are not backfilled, and a failed import is best-effort rather than a reason to lose the new turn.
5. **Review the security record.** The [v2.1.7.0 cargo-deny audit](docs/ops/v2.1.7.0_CARGO_DENY_AUDIT.md) records the post-sweep evidence and the remaining accepted advisories. Rebuilding is required to deploy dependency fixes; a green source audit alone does not update a running binary.

## Remaining Limitations

- The full non-root bridge/task-container split remains open under WI#454. Environment scrubbing and task credential rejection are not a substitute for that isolation boundary.
- OpenCode v1 remains at `v1.18.25`; the known WI#333 startup failure is not claimed fixed by this release's output-accounting changes.
- The carried-forward deployment and security boundaries in the [v2.1.6.0 limitations](docs/releases/RELEASE-v2.1.6.0.md#security-deployment-and-release-limitations) still apply. Worker tool grants remain empty by default and must be assigned deliberately.

---

> Beyond charted lines,  
> small lights show the open sea,  
> we sail into dawn.
