# Item 101 (v2.1.7.0) — persistent test tenant `highwinds`

Status: provisioning and verification in progress; item 101 is not complete.

This is a new v2.1.7.0 test pass, not the previous silverzoom or mistyfly run.
Rarity is working sequentially on `aj/kimi-101-20261029-0054`, starting from
`a83ea3d8b`, on 2026-10-29. No subagents are used.

## Completion contract

- Create a new tenant named `highwinds` (a non-animal name from the WI#273
  test-tenant naming convention).
- Enable kimi and pebble external workers with full toolchains.
- Assign complex tasks through LunarWing, independently verify the resulting
  artifacts, and record failures as well as successes.
- Leave the tenant running; do not invoke teardown.

## Host and initial diagnostics

The current host is `ajhermes-z`, Linux `6.18.54-2-lts`, with systemd and
Podman, not the Gentoo/OpenRC host used for the mistyfly report. The only
registered tenant at the start is `hermes`, gateway port 10040. Its daemon,
PostgreSQL, and LunarVision services are active; no workers are selected.
The operator checkout is clean. The filesystem has 264 GiB available and
`nproc` reports 12 usable processors.

The standard mt-admin doctor reports 33 passes and 7 failures:

- `newuidmap` and `newgidmap` are not setuid, but carry `cap_setuid=ep` and
  `cap_setgid=ep`, respectively. A real `podman unshare true` as the existing
  tenant succeeds. The doctor's setuid-only check is therefore not sufficient
  evidence of a broken rootless installation on this host. No helper mode or
  host security setting was changed.
- Root's administrative image store is empty (pebble, opencode, kimi, vision,
  and sandbox image checks fail). The existing tenant's separate rootless
  store does contain working PostgreSQL and LunarVision images. New worker
  images and the standard image-preparation path still need verification.

The provisioning path is `ic/scripts/lunarwing-mt-admin.sh` with explicit
`LUNARWING_SERVICE_MANAGER=systemd` and
`LUNARWING_CONTAINER_RUNTIME=podman`. Tenant lifecycle/status will use mt-admin,
not direct system-bus tenant service calls. Credentials are supplied from the
operator's existing environment and must not appear in this report or logs.

## Provisioning record

`add-tenant highwinds` completed with exit 0 using the standard mt-admin path:

```bash
sudo env LUNARWING_SERVICE_MANAGER=systemd \
  LUNARWING_CONTAINER_RUNTIME=podman LUNARWING_MT_BUILD_JOBS=18 \
  taskset -c 0-17 bash ic/scripts/lunarwing-mt-admin.sh add-tenant highwinds \
  --with-kimi --with-pebble --with-study-mcp \
  --llm-base-url https://nano-gpt.com/api/v1 --llm-model openai/gpt-5.2 \
  --llm-api-key '<provided privately>' \
  --kimi-base-url https://nano-gpt.com/api/v1 --kimi-model openai/gpt-5.2 \
  --kimi-api-key-file '<anonymous memory descriptor>'
```

The credential placeholders above are not literal invocation arguments. The
existing `NANOGPT_API_KEY` was passed privately; Kimi's current file-based CLI
received an anonymous `memfd` reference, not an on-disk credential copy. The
initial command using the old `--kimi-api-key` spelling was refused before any
provisioning; the supported `--kimi-api-key-file` spelling succeeded. No
credential file was read or copied by the test harness.

- New OS account: `highwinds`, uid 1010, linger enabled.
- Registry allocation: gateway 10000, PostgreSQL 10003, orchestrator 10006,
  pebble WebSocket 10008, kimi WebSocket 30000; primary block 10000–10009.
- Source: local branch clone from the operator checkout after scope commit
  `05a2cd90f` (before later report-only commits).
- PostgreSQL: ready via tenant user Quadlet, not a system-bus tenant unit.
- WeeChat relay: configured successfully on this host.
- Study MCP: latest checkout configured successfully.
- LunarVision and managed sandbox images: built by `add-tenant`; sandbox
  distributed into the new tenant's rootless store.
- Initial rootless pre-warm warned when run from the operator's private home
  checkout. `podman unshare true` as `highwinds` from `/tmp` succeeds. Subsequent
  administrative commands use `/tmp` as their working directory and absolute
  paths to mt-admin; no permissions or rootless security settings were relaxed.

The next standard sequence is running in tmux `item101-deploy`:
`configure-pebble` → explicit `configure-worker-egress --allow nano-gpt.com:443`
→ `build-tenant --with-wasm --with-pebble --with-kimi --with-toolchains`
→ `start-tenant`. Explicitly allowing the chosen LLM endpoint preserves the
fail-closed egress policy rather than bypassing it. The build is a deployment
release build, not a full debug verification build. Redacted local logs:
`/tmp/lw-item101/add.log` and `/tmp/lw-item101/deploy.log`.

## Planned worker exercise

Six sequential jobs are prepared, each in a separate persistent worker
workspace subdirectory. No job will launch subagents or inspect credentials.

| Lane | Toolchain | Task | Independent oracle |
| --- | --- | --- | --- |
| kimi | Rust | Two-tree Fenwick range addition/range sum with signed i128 values | Seeded naive-array comparison |
| kimi | Go | Concurrent snapshot-isolation transactional key-value store | Independent transaction model and race detector |
| kimi | C++17 | Dinic maximum flow with parallel edges and residual paths | Exhaustive small-graph minimum cuts |
| pebble | Rust | Segment-tree lifetimes and rollback-DSU dynamic multigraph connectivity | Seeded multigraph BFS comparison |
| pebble | Go | External stable CSV sort/merge join with bounded sort chunks | Python CSV Cartesian-product join |
| pebble | C++17 | Rectangular Hungarian minimum-cost assignment | Exhaustive small assignments plus dual certificates |

These are planned checks, not claimed results. Job IDs, actual toolchain
versions, test outputs, independent pass counts, and final service/security
audits will be recorded after execution.
