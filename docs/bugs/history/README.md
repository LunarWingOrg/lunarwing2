# Archived bug docs

Resolved reports and compatibility pointers kept for provenance. The active index is
[`../README.md`](../README.md); unresolved current work should have a canonical
document directly under `docs/bugs/`. The two pointer rows marked below remain
here only so old paths continue to resolve.

Current-source cross-checks were refreshed against `51ae5a8` on 2026-07-20.

| Document | Current status / resolution |
|---|---|
| [BUG-FIXED-LAPSE.md](BUG-FIXED-LAPSE.md) | FIXED: XML tool-call dialect recovery |
| [BUG-FIXED-engine-and-test-harness.md](BUG-FIXED-engine-and-test-harness.md) | FIXED: engine assertions and `Arc<Agent>::run` harness ownership |
| [BUG-FIXED-kawarimi-import-opencode.md](BUG-FIXED-kawarimi-import-opencode.md) | FIXED named `--with-opencode` import/build forwarding; separate parity bug is active |
| [BUG-FIXED-subagent-worker-hang.md](BUG-FIXED-subagent-worker-hang.md) | FIXED: fire-and-forget completion state transition |
| [BUG-FIXED-WEECHAT-WARNINGS.md](BUG-FIXED-WEECHAT-WARNINGS.md) | COMPATIBILITY POINTER: PARTIAL; `rand_check` is active in `../BUG-weechat-relay-rand-check.md` |
| [BUG-FIXED-wasm-tools-not-found-on-build.md](BUG-FIXED-wasm-tools-not-found-on-build.md) | FIXED: tenant-local `wasm-tools` lookup and raw-component fallback wording |
| [BUG-FIXED-workspace-concurrency-fixes-v1.1.0.md](BUG-FIXED-workspace-concurrency-fixes-v1.1.0.md) | FIXED: atomic workspace writes and indexing; expected pool lag retained as not-a-bug |
| [OPENRC-MT-1.1.4-ISSUES.md](OPENRC-MT-1.1.4-ISSUES.md) | Historical OpenRC pass; O1/O2/O4/O5 fixed and O3 documented |
| [SYSTEMD-MT-1.1.4-ISSUES.md](SYSTEMD-MT-1.1.4-ISSUES.md) | Historical systemd pass; F11 was promoted to active `BUG-mt-nanocode-image-size.md` |
| [WEECHAT-NO-SECRET-ACCESS.md](WEECHAT-NO-SECRET-ACCESS.md) | FIXED: owner credential scope for WASM channel messages |
| [XMPP-OMEMO-BUG-TO-DO.md](XMPP-OMEMO-BUG-TO-DO.md) | COMPATIBILITY POINTER: UNVERIFIED fallback-spam; active report is `../BUG-xmpp-omemo-warmup-and-processing.md` |

The pointer files above are intentionally not counted as resolved reports in
the active index; their primary dispositions appear in its original-file table.
