# WeeChat relay `rand_check` stub

> **Status: PARTIALLY-FIXED (verified against `51ae5a8` on 2026-07-20).** The
> stale `HttpEndpointConfig` import warning is gone; `rand_check` still ignores
> its probability argument and always returns `false`.

The former history record `history/BUG-FIXED-WEECHAT-WARNINGS.md` mixed these two
independent findings. It is active again because the second one remains
unresolved.

## Fixed import warning

The relay imports only the channel types it uses at
`lunarwing_weechat_wss/weechat_relay/src/lib.rs:46-50`; there is no unused
`HttpEndpointConfig` import in the current file.

## Open latent behavior bug

`rand_check(_probability: f64) -> bool` at
`lunarwing_weechat_wss/weechat_relay/src/lib.rs:2164-2172` unconditionally
returns `false`. Any future feature that calls it would be silently disabled
regardless of the requested probability. A repository search finds no current
call site, so this is a dead/unreferenced latent stub rather than a currently
observed user-facing failure. Severity remains Low, but the function contract
and implementation disagree.

Possible resolutions are to implement a small deterministic state-based PRNG
with a threshold, or remove the parameter and the now-dead random feature. No
fix is present in reachable history; do not mark this resolved until behavior
or call sites change and a focused test exists.

## Verification record

Status comes from current source inspection and `git log --all`; no Rust build
or test command was run.
