Decision about whether LunarWing should support OpenCode v1, v2, or both, triggered by OpenCode 2.0.3 (2.0.4 now actually) shipping and a prior integration regression.

## Problem

OpenCode 2 is announced as stable (packages `@opencode/plugin` and `@opencode/cli`, version 2.0.3), but in practice it needs multiple community patches — "stable" in marketing terms only. The community is doing the debugging that release testing should have caught.

Separately, the OpenCode version used in LunarWing's external worker had been bumped, and that broke the integration. The regression was caught during testing before the Ma-ai release, not in production, and the worker was downgraded/pinned to the last-known-good version. The actual root cause traced to OpenCode's shipped default config being broken, not LunarWing's glue code. The new version was never shipped.

Current version state:
- Zanshin runs the known-good v1 version, 1.18.25, in its Opencode external worker.
- Beta builds of v2 are still installed on LunarWing core contributers' machines but not in active use. Furthermore, there is no opencode2 support whatsoever in LunarWing.
- V1 now reads supported V2 config fields, so mixed v1/v2 config setups keep working — this appears to be OpenCode's fix for the exact breakage already defended against. Something in the default config LunarWing uses for its opencode external worker breaks something in the recent versions of v1 however.

## Anticipated issues

- Version support scope: whether to maintain v1 only, v2 only, or both — a real fork in the road given v1 is proven and v2 is still patching itself.
- Worker slug/naming: `opencode2` is the least confusing choice because it matches the shell command, but it could collide with existing aliases, regex matchers, unit-name patterns, routing tables, port allocations, env files, and MCP/admin scripts. A codebase scan is needed before committing to a plan here.
- Agent confusion when both external workers are present and acitve: an agent sees `opencode` and `opencode2` and must decide which to route to. The slug is what agents reason over; the version string helps but isn't the whole answer.
- ACP protocol divergence: v2 reportedly changes how its ACP works from v1, so the custom `lunarwingv1 websocket` bridge may not be reusable or even necessary. The v2 worker could actually be simpler. Worth reading v2's ACP docs before assuming the old bridge applies.
- Misleading naming conventions in the existing protocol itself
- Fork-and-rename was considered as a fallback, but it creates an ongoing maintenance fork with upstream (carrying renames across every version bump), so it's only worth it if the conflict is real rather than hypothetical

## Proposed plan

- Support both versions, with an eventual sunset date for v1 (not really anticipated any time soon): keep the v1 lane pinned at 1.18.25 with Zanshin, add a v2 lane, let the v2 patch stack settle and earn trust gradually, then flip the default and retire v1 once 2.0.x is genuinely stable. Avoid being forced by upstream's schedule
- This is low-cost because adding workers is trivial since 2.0.8.0 and the modular mt-admin library — the earlier break-up of a ~20K-line single bash script into ~14 components turned "add a worker" into a config entry. The same approach could also cover Moonshot's Kimicode, Pebble, and other harnesses, with or without OS-level toolchains present.
- Suggested order of operations:
  1. Grep the codebase for `opencode` collision surfaces (roughly an hour)
  2. Read v2's ACP contract to determine whether the v1 websocket bridge applies at all
  3. Decide the slug: `opencode2` if clean, `opencode-v2` if the bare name collides
  4. Ship a worker manifest with explicit version-in-description fields for agent clarity (possibly with capability keys to prefer v1 during the transition).
  5. Have Rarity and AJ write the integration tests
- Longer term: unpin the worker, upgrade, verify, and move on once upstream settles

Alternative mitigations noted for the dual-install confusion: distinct description fields naming versions explicitly, capability keys in the worker manifest so routing prefers v1, or simply not installing both on the same tenant initially
