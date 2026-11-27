# WI #920: Dependency migration scope at Rust 1.96.1

[WI #920](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/920)
follows the [v2.2.3.0 sweep](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/911).
The decisions below use the locks at `c58fcfb26` and stable, non-yanked releases
from the crates.io sparse index. They select candidates for separate reviews,
not dependency upgrades. Keep the Rust 1.96.1 floor and the current pins until
an implementation passes its own checks.

| Dependency | Locked in both graphs | Decision |
|---|---|---|
| `wasmtime` and `wasmtime-wasi` | 36.0.23 | Target **48.0.10 LTS** (Rust 1.95.0). No toolchain increase or intermediate major hop required by the declared minimum. |
| `rig-core` | 0.40.0, with the WI #292 vendor patch | Target **0.44.0** (Rust 1.95.0), conditional on finish-reason parity. Keep the current patch until that is proven. |
| `html-to-markdown-rs` | 3.19.1 | **Defer the direct upgrade** until an approved Rust 1.99 floor or a release with a maintained parser that works on 1.96.1. |

## 1. Wasmtime runtime and cache

The [Wasmtime](https://index.crates.io/wa/sm/wasmtime) and
[WASI](https://index.crates.io/wa/sm/wasmtime-wasi) index records agree on the
floors. Lines 37–47 require Rust 1.87–1.94 but are outside their regular support
windows. 49.0.7 requires 1.96.0 and fits, but its regular support window has also
ended. 50.0.3 and 51.0.0 both require 1.97.0. By the
[upstream release policy](https://docs.wasmtime.dev/stability-release.html),
36 and 48 are supported LTS lines with 24 months of security backports. Stay
on 36.0.23 until the 48 LTS migration is ready rather than stepping through
unsupported releases.

- **Cache gate:** Move the two runtime crates together and rebase the
  [local cache patch](../../ic/patches/wasmtime-internal-cache/LUNARWING-PATCH.md)
  in the same review. Wasmtime 48.0.10 requires `wasmtime-internal-cache`
  **`=48.0.10`**, and the [upstream cache](https://index.crates.io/wa/sm/wasmtime-internal-cache)
  still requests `zstd ^0.13.0` on 48 LTS. Keep the local `zstd = "0.14.3"`
  requirement. Both `ic/Cargo.lock` and `ic/bridges/xmpp-bridge/Cargo.lock`
  currently select the source-less (path) cache at 36.0.23 with `zstd` 0.14.5
  and `zstd-safe` 8.4.0. At the new pin, verify the same path-source invariant
  and the cache's own `zstd` edge in **both** graphs; a stale patch version
  can silently select the registry crate. Run `WasmtimeSecurityFloorTests`
  in [the policy guards](../../.gitlab/tests/test_deny_toml_guards.py) and
  the WASM compilation-cache round-trip test. Do not reuse serialized
  components from a different Wasmtime version.
- **Runtime gate:** Review the component bindings and WASI linker in the
  [tool](../../ic/src/tools/wasm/wrapper.rs),
  [channel](../../ic/src/channels/wasm/wrapper.rs) and
  [MCP](../../ic/src/tools/mcp/wasm_transport.rs) hosts. Run the focused WASM
  tests for fuel, epoch deadlines and memory limits; recheck that filesystem,
  network and host capability defaults remain restricted. Instantiate an
  existing tool and channel component and exercise a tool call plus channel
  startup, message delivery and shutdown. The cache check does not replace
  those behavioral tests.

## 2. rig-core and finish-reason parity

The [rig-core index](https://index.crates.io/ri/g-/rig-core) does not declare a
Rust floor for 0.41.0 or 0.42.0. Both 0.43.0 and 0.44.0 declare 1.95.0; use the
newer one. The floor rises to 1.97.1 at 0.45.0 and stays there through 0.47.1.

The published 0.44.0 [OpenAI-compatible parser](https://docs.rs/crate/rig-core/0.44.0/source/src/providers/openai/wire/chat.rs)
retains reported finish reasons and skips empty ones; its
[completion API](https://docs.rs/crate/rig-core/0.44.0/source/src/completion/request.rs)
exposes an optional reason. This is not proof that the
[WI #292 patch](../../ic/vendor/rig-core/LUNARWING-PATCH.md) can be removed:
0.44.0 reconciles normalized reasons with the response and its `stop()`
classification can turn a content filter or unfamiliar reason into an error.
Use the reported reason, not that classification, when adapting
[`rig_adapter.rs`](../../ic/src/llm/rig_adapter.rs). Preserve the injected HTTP
client and environment-only proxy behavior as the transport APIs change.

- **Parity gate, before changing the vendor pin:** Compare the candidate
  adapter against 0.40.0 with the existing TensorZero and blocking fixtures
  plus Copilot chat coverage. Check blocking (with and without tools) and
  stream completion for `stop`, `length`, `tool_calls`, `content_filter`, and
  missing/empty reasons, plus an unfamiliar provider reason where applicable.
  Distinguish a silent tool-less reply (`unknown`) from explicit `stop`;
  preserve the existing tool-call fallback and terminal usage. Include the
  stream case where a usage-only frame follows the finish reason.
- **Vendor gate:** If upstream plus the adapter passes the matrix, retire the
  patch and the refresh-script marker contract together. Otherwise port the
  missing behavior and update the patch provenance, checksums and marker
  lists per the patch README. Verify the
  [vendor refresh guard](../../ic/scripts/tests/test-refresh-vendor-rigcore-guard.sh)
  independently of reply behavior. Keep 0.40.0 if parity is not proven.

## 3. HTML converter and advisory removal

The [converter index](https://index.crates.io/ht/ml/html-to-markdown-rs) shows
3.19.1 at Rust 1.88 and every newer non-yanked release (3.20.0 through 3.23.5,
including the work item's 3.23.4) at **1.99**. The switch to
[`astral-html =0.0.4`](https://index.crates.io/as/tr/astral-html) starts at
3.20.0; that parser itself requires Rust 1.97. No intermediate drop-in upgrade
clears the notice on 1.96.1. Do not bypass the parser or parent MSRV or raise
LunarWing's floor for this migration scope.

- **Advisory gate:** Both current locked graphs reach `astral-tl` 0.8.1 via
  `lunarwing` → `html-to-markdown-rs` 3.19.1. The XMPP bridge keeps LunarWing's
  default features, including `html-to-markdown`.
  [RUSTSEC-2026-0444](https://rustsec.org/advisories/RUSTSEC-2026-0444.html) is
  an unmaintained notice, not a vulnerability or an `ic/deny.toml` ignore.
  Once unblocked, require no `astral-tl` entries in either lock or in a
  successful `cargo tree --locked --all-features --target all` for each root.
  Confirm the notice is absent in both lockfile advisory reports without any
  new ignores; an advisory gate passing by itself does not prove removal.
- **Conversion gate:** Run [`html_to_markdown`](../../ic/tests/html_to_markdown.rs)
  fixture tests and the [converter unit tests](../../ic/src/tools/builtin/html_converter.rs)
  for article extraction, error handling and feature-disabled passthrough. Do
  not disable the default feature to claim the advisory has cleared.

For each approved migration, renew only the affected pins, resolve and check
both lock roots at the agreed Rust floor, and regenerate sources with
[`refresh-vendor.sh`](../../ic/scripts/refresh-vendor.sh). Keep its vendor and
secret-history checks and the existing advisory policy intact. The two candidate
MSRVs above are declarations, not claims of a successful LunarWing build.
