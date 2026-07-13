# E2E test bugs and retired fixtures

> **Overall status: PARTIALLY-FIXED / one item UNVERIFIED (verified against
> HEAD 2026-07-12).** The bootstrap and tool-execution failures are resolved in
> the current test/code paths. The old Gmail OAuth fixture was retired. The
> clipboard test is still collected, but its former permission diagnosis is not
> supported by the current test and has not been reproduced here.

This consolidates the four `BUG-e2e-*` reports. Status is per section so a
resolved test does not get reported as an active production failure.

## 1. Bootstrap greeting tests

**Status: FIXED (static verification; test execution not run).**

### Former failure

`bootstrap_greeting_fires` and
`bootstrap_onboarding_clears_bootstrap` previously waited on a channel response
that was not observed. The old report pointed at
`tests/e2e_advanced_traces.rs:828/862`, `BOOTSTRAP.md`, and a greeting containing
"chief of staff"; those paths and text are stale.

The observed assertion messages were `bootstrap greeting should produce a
response` and `bootstrap greeting should arrive`. The original root-cause
hypotheses were a DB/SSE-versus-channel path mismatch or a subscription-ordering
race; the current explicit gateway broadcast resolves that divergence.

### Current path and evidence

- `Agent::run` takes `Arc<Self>` and persists the greeting before channel start
  (`ic/src/agent/agent_loop.rs:401-430`).
- After startup it registers the gateway thread, constructs an
  `OutgoingResponse`, sets `thread_id`, and broadcasts to the gateway
  (`ic/src/agent/agent_loop.rs:850-871`).
- `TestRigBuilder::with_bootstrap()` preserves the pending flag and installs a
  gateway-named test channel (`ic/tests/support/test_rig.rs:472-475,769-780`).
- The two tests now wait for and assert the static greeting at
  `ic/tests/e2e_advanced_traces.rs:752-776` and `:786-805`; the source text is
  `ic/src/workspace/seeds/GREETING.md`.

The old DB/SSE-only hypothesis and startup-ordering workaround are therefore
historical, not current action items.

## 2. Clipboard copy test

**Status: UNVERIFIED (possibly still environment-sensitive).**

The test remains collected at
`ic/tests/e2e/scenarios/test_chat.py:79-117`. It creates a synthetic `copy`
event with an in-memory `clipboardData` object
(`test_chat.py:95-109`); it does not call `navigator.clipboard` or request
browser clipboard permissions. The production handler only requires a selected
chat range and event clipboard data, then writes `text/plain`
(`ic/src/channels/web/static/app.js:832-846`).

Consequently, the old claim that setup fails because headless Chromium lacks
clipboard permissions is not supported by the current source. No skip marker or
CI-specific permission configuration was found, and Playwright was not run in
this pass. Keep this item visible until a browser run confirms whether the
synthetic event is stable in the supported CI image.

The original proposed remedies remain useful only if a browser failure returns:
grant `clipboard-read`/`clipboard-write`, use `navigator.clipboard` after
explicit permission, or skip only in an environment that demonstrably cannot
dispatch the synthetic event. None is present or justified by current source.

The original report's setup symptom was a Playwright timeout/permission error,
not a production copy failure. Its affected files were `test_chat.py` and
`conftest.py`; those paths remain the ones to inspect if a real browser run
fails.

## 3. Gmail OAuth URL-parameter fixture

**Status: FIXED / RETIRED (test surface removed).**

The six named tests and their `installed_gmail` fixture are absent from the
current tree. The generated `ic/tests/e2e/lunarwing_e2e.egg-info/SOURCES.txt`
still lists them, but that metadata is stale. Release history records removal
of the Gmail-based scenarios (`docs/releases/RELEASE-v1.1.2.md:106-116`).
For provenance, the retired test names were:

- `test_oauth_url_parameters.py::test_oauth_url_has_client_id_not_clientid`
- `test_oauth_url_parameters.py::test_oauth_url_has_required_parameters`
- `test_oauth_url_parameters.py::test_oauth_url_has_extra_params`
- `test_oauth_url_parameters.py::test_oauth_url_is_valid_google_oauth`
- `test_oauth_url_parameters.py::test_oauth_url_state_is_unique`
- `test_oauth_url_parameters.py::test_oauth_url_escaping`

Current OAuth coverage uses mock/MCP and UI fixtures instead:

- `ic/tests/e2e/conftest.py:296-300,500-504` uses a mock OAuth exchange.
- `ic/tests/e2e/scenarios/test_mcp_auth_flow.py` covers the current mocked auth
  flow.
- `ic/tests/e2e/scenarios/test_extensions.py:633-663,743-750,1189-1215`
  covers UI OAuth cards and rejects unsafe URL schemes.

The historical proxy/download failure and missing `wasm-tools` explanation is
preserved here for provenance, but there is no current `test_oauth_url_parameters.py`
failure to reproduce.

The historical setup error was:

```text
AssertionError: Install failed: Primary install failed: Download failed: error sending request
for url (WASM artifact download URL);
fallback install also failed: Installation failed: 'gmail' requires building from source.
```

The fixture could not reach the artifact without a proxy, and the
`wasm-tools`/`cargo-component` source fallback was unavailable. The same setup
root cause affected all 20 `test_wasm_lifecycle.py` cases and
`test_extension_oauth.py::test_oauth_install_gmail`. Its old remediation choices
were proxy injection, prebuilt local artifacts, or a mocked install fixture;
current mock/MCP coverage supersedes them.

The old fixture named six URL assertions (`client_id`, required/extra
parameters, Google validity, state uniqueness, and escaping). Because the
fixture and Gmail bundle were removed, those exact assertions cannot be
reproduced in the current suite.

## 4. Tool execution timeout

**Status: FIXED (static verification; E2E execution not run).**

The named tests remain at
`ic/tests/e2e/scenarios/test_tool_execution.py:76-115`. The mock LLM still
recognizes `echo` and `time` (`ic/tests/e2e/mock_llm.py:26-37`) and returns a
follow-up summary for a tool result (`mock_llm.py:187-206`). The contaminating
approval scenario now denies its pending approval during cleanup
(`ic/tests/e2e/scenarios/test_tool_approval.py:190-195`), which releases the
agent loop for subsequent tests. The old blank "Verify fix" section and its
older line ranges are removed from the canonical record.

The former diagnosis considered a missing mock-LLM follow-up, slow SSE delivery,
or an insufficient 30-second timeout. Current `mock_llm.py` explicitly converts
a tool result into a second text response. Those hypotheses are historical; the
pending-approval leak was the documented resolution.

The original symptom was a 30-second Playwright `wait_for_function` timeout for
`test_builtin_echo_tool` and `test_builtin_time_tool`, while the non-tool test
passed. The current helper still waits for a new assistant element and the mock
follow-up path now supplies the expected fragment.

## Verification record

Static source inspection was supplemented by a collection-only Pytest attempt
for the current chat/MCP/extension/tool scenarios. Pytest enumerated 64 tests,
but collection reported `ModuleNotFoundError: No module named 'playwright'` for
`test_tool_execution.py`; no test body, browser, daemon, build, or Cargo
command ran. The clipboard status remains unverified because the environment
does not provide a Playwright runtime here.
