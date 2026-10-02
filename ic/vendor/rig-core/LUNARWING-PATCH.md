# LunarWing local patch — rig-core 0.40.0

This vendored copy of rig-core carries a **local LunarWing patch** on top of
pristine crates.io `rig-core 0.40.0`. `cargo vendor` regenerates this directory
from the registry tarball and **silently drops the patch**;
`ic/scripts/refresh-vendor.sh` contains a tripwire that fails the refresh if
the patch marker is gone. If that tripwire fires, re-apply the patch below
(see git history for the exact diff) and re-run the refresh.

## WI#292 — finish_reason pass-through

Marker string in patched sources: `LUNARWING_WI292_FINISH_REASON_PATCH`.

Problem: the provider's real `finish_reason` was destroyed on both paths —
the OpenAI-compatible streaming aggregator collapsed `stop`/`length`/
`content_filter` into a generic `Other`, and downstream consumers re-guessed
the reason from content shape.

Patched files:

- `src/providers/internal/openai_chat_completions_compatible.rs` —
  `CompatibleFinishReason` widened with `Stop`/`Length`/`ContentFilter`;
  the stream aggregator records the last real finish reason and passes it to
  `CompatibleStreamProfile::build_final_response` (new parameter).
- `src/providers/openai/completion/streaming.rs` — the chunk mapping keeps
  the canonical reasons; `StreamingCompletionResponse` gains a
  `finish_reason: Option<FinishReason>` field; its `GetTokenUsage` impl
  reports the canonical spelling.
- `src/providers/openai/completion/mod.rs` — blocking `Choice.finish_reason`
  is `#[serde(default)]` so gateways that omit it no longer fail
  deserialization (empty string = "not reported").
- `src/providers/copilot/mod.rs` — same mapping widening;
  `CopilotStreamingResponse` forwards the finish reason for the chat route.
- `src/completion/request.rs` — `GetTokenUsage` gains a defaulted
  `finish_reason()` method (`None` = provider did not say), so consumers can
  read the real reason without new trait bounds.
- `src/test_utils/internal_streaming_profiles.rs` — test profiles updated for
  the new `build_final_response` signature.

Upstream candidates: widen rig's streaming finish-reason surface so this
patch can be dropped on the next rig upgrade.
