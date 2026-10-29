# Item 101 (v2.1.7.0) — persistent test tenant `highwinds`

Status: RG-1 implemented, fat image built and task-uid smoke verified. Live
complex-job acceptance is blocked by the managed egress keeper's Podman 6.1.4
compatibility refusal. Item 101 is not complete; the daemon and Pebble remain
running, but Kimi is stopped after its guarded recreation failed.

## Scope and provenance

This continues closed [MR !2040](https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/2040),
head `767896863`, and its run-262 review #12562. That pass provisioned the new
non-animal tenant `highwinds`, built full-toolchain Kimi and Pebble images and
left services running, but did **not** complete the planned worker jobs.
Those commits are Applejack's (`aj/kimi-101-20261029-0054`), not Rarity's as
the old report said. This re-attempt is `aj/kimi-101-20261029-0530`; no subagents
are used. The earlier silverzoom and mistyfly reports are separate test passes.

Completion requires real complex tasks on both workers, independent artifact
verification, and a final running-tenant/security audit. No teardown is allowed.
`highwinds` follows [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
and must not be targeted by animal-name cleanup.

## Previous pass: provisioning and builds

These are historical observations from !2040, not new measurements in this pass.
The host is a Linux systemd VM with rootless Podman, tenant **user-manager**
units, 12 reported usable processors, and 23 GiB memory. Host machine names are
intentionally omitted. All lifecycle operations use mt-admin, not system-bus
commands against tenant units.

- `add-tenant highwinds --with-kimi --with-pebble --with-study-mcp --llm-base-url
  https://nano-gpt.com/api/v1 --llm-model openai/gpt-5.2` succeeded. Credentials
  were supplied privately; Kimi used `--kimi-api-key-file` with an anonymous
  memory descriptor. No credential file was read or copied by the test harness.
- New tenant uid 1010, linger enabled. Primary ports 10000–10009: gateway
  10000, PostgreSQL 10003, orchestrator 10006, Pebble WS 10008. Kimi WS/health
  30000/30001; Pebble health 20004; vision API/health 20005/20006.
- The tenant clone began at `05a2cd90fc34da616c9480b7e7a31ed1da6dc3ef`.
  PostgreSQL, WeeChat, study MCP, LunarVision and sandbox provisioning succeeded.
- `build-tenant --with-wasm --with-pebble --with-kimi --with-toolchains` built
  the daemon release binary (12m16s), XMPP bridge release (10m27s), nine WASM
  tools and four channels. Installation: 12 installed, DarkIRC skipped because
  disabled. No full debug daemon build was used.
- Pebble upstream `2d0f3786cbebb705cbd4f6212b0aaf47d8d8590b`, reviewed lock and
  WI#477 patch verified. The fat image passed both WI#334 toolchain smoke legs:
  actual `/bin/sh -lc` task shell and sanitized `env -i` resolution. Rust/Cargo
  1.96.1, rustup 1.28.2, Go 1.24.3 and native C/C++ support were installed.
  Image ID `168667d144b4fce57985a430a554b2c53e42172f75220ea2a83e56f8a2bfe6e4`,
  size 2,334,932,434 bytes, revision `67a96a9dd4380317dd05badda0f357a20c9085bb`,
  toolchain profile true.
- Initial Kimi fat image: CLI 2.3.0, 178/178 in-image tests, including the real
  CLI contract. Task setup then failed at runtime with `Permission denied`:
  root-owned `/app/entrypoint-task-setup.sh` was 0511, executable but unreadable
  to uid 10002. Commit `5e041ea0f` changed the two entrypoints to 0555, rebuilt
  image `5491cbb1cfe425f7324c06e45035817c65b8e5b91cb6fef082750146e7861ffe`,
  and recovered Kimi readiness. That symptom-only change is superseded by RG-1
  below; root-side readiness did not prove task-side config/hook readability.
- Previous final status: daemon, PostgreSQL, both workers and vision healthy;
  listener audit clean, worker egress exclusive and DNS pinned with only the
  explicit `nano-gpt.com:443` allow. No complex acceptance job had run.

The old report contradicted itself about the rebuilt Kimi revision. Its initial
image used `05a2cd90f`; the rebuilt image was recorded as `5e041ea0f`. Neither is
a claim about the image this re-attempt will build.

## RG-1 — restrictive checkout umask propagated into runtime policy files

The review identified the cause, not just the entrypoint symptom: mt-admin's
private tenant checkout has 0600 tracked non-executable files and 0700
directories. `COPY` preserves these modes. `chmod +x` followed by `chmod a-w`
turns a 0600 script into 0511, while config files become 0400 and their
directories 0500. Bridge root can start while uid 10002 cannot read its policy,
hook, runner or MCP shim. Root-run tests/readiness therefore missed the problem.

Fresh read-only confirmation on 2026-10-29: the tenant's Kimi `config` and
`hooks` directories are both 0700; `config/config.toml` and
`hooks/headless_command_guard.mjs` are both 0600. Their tracked Git modes are
100644. This is a restrictive-context build, not a broken task privilege drop.

The replacement fix in `kimicode4lunarwing/Dockerfile`:

- Normalizes `/app/config`, `/app/hooks` and `/app/scripts` with `a+rX`, the
  health/bind/protocol files with read access, and entrypoints to 0555.
- Removes write access from all those immutable inputs, preserving root
  ownership. Does not relax task identity, WI#206 hooks, authentication or egress.
- In the final image layer, verifies root ownership/no write bits, then uses
  `setpriv --reuid=10002 --regid=10002 --clear-groups` to traverse/read every
  input (including dependency symlink targets) and execute the real task setup.
  Setup uses a disposable home and a documentation endpoint, no credential;
  its config/MCP outputs must exist. Temporary state is removed in the same RUN.
- `.gitlab/tests/test_kimi_image_modes.py` executes the real Dockerfile mode
  block against both normal/restrictive fixtures, compares resulting modes,
  runs setup as uid 10002 on root-capable runners, and confirms unreadable
  nested files/directories are rejected rather than silently skipped.

Local regression evidence so far: all four tests pass under root, including
both genuine dropped-uid cases. The unprivileged run has two passes and two
explicit root-required skips. CI config validation and the worker exposure
policy guard also pass. Full rebuilt-image evidence is still pending.

The same restrictive source context is relevant to the Pebble and OpenCode
image `COPY`/read-only patterns. This change fixes **Kimi**, not all workers;
no cross-worker mode repair or whole-image reproducibility certification is
claimed. Copied permission differences also affect image digests independently
of source revision (WI#425/WI#507 provenance concerns).

## Other findings retained from !2040

**WI#504: first-start rootless namespace lifetime.** The earlier standard
`configure-worker-egress highwinds --allow nano-gpt.com:443` failed because
separate short-lived load/verify sessions saw different namespaces. A temporary
operator-only `podman unshare --rootless-netns sleep 3600` keeper allowed the
standard command to succeed; it was not a firewall bypass and is **not** a
runbook step. In this re-attempt the old `item101-netns` tmux session is absent;
`worker-egress highwinds` freshly verifies both running containers and exact
forward/input rejects. The current tree carries the managed keeper path;
its use will be recorded separately if applied. No new manual keeper is planned.

**WI#507: source-root asymmetry.** `build_pebble_worker` used the operator
checkout (`67a96a9dd...`), while Kimi used the tenant clone (`05a2cd90f...`).
Those image labels/build logs are historical evidence of two source commits
for one tenant; this report does not rewrite them to a later head. The owning
work item remains separate from RG-1.

## Planned worker exercise — not results

Existing prepared tasks are retained, sequentially dispatched into distinct
workspace subdirectories. No subagents, secret inspection or unrelated project
changes are permitted. Artifacts will remain in the persistent tenant:
`/home/highwinds/lunarwing/{kimi,pebble}-workspace`, mounted at `/workspace`.

| Lane | Task | Independent oracle |
| --- | --- | --- |
| Kimi / Rust | Two-tree Fenwick inclusive range add/sum, signed i128 | Seeded naive array |
| Kimi / Go | Concurrent snapshot-isolation transactional store | Transaction model plus race detector |
| Kimi / C++17 | Dinic max flow, parallel edges and residuals | Exhaustive small-graph min cuts |
| Pebble / Rust | Segment-tree lifetimes and rollback DSU connectivity | Seeded multigraph BFS |
| Pebble / Go | External stable CSV sort/merge join | Python CSV Cartesian-product join |
| Pebble / C++17 | Rectangular Hungarian assignment | Exhaustive assignments plus dual certificates |

Prior verifier-host versions were Python 3.11.17, Go 1.27.3, Rust/Cargo 1.96.1
and G++ 16.2.1; these are **not** worker-image version claims. Job IDs, actual
commands, worker versions, independent counts and the final hand-off will be
added only after they are observed. Redacted operational logs and helpers from
the previous pass remain under `/tmp/lw-item101/` on this host.
