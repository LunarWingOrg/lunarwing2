# Credential Redaction in Tool Output and Approval Events

LunarWing redacts credentials on the paths where they would otherwise be
**displayed, persisted, or handed back to untrusted code** — WebAssembly tool
errors and HTTP responses, approval prompts, `ApprovalRequested` thread
events, `ToolCall` hook payloads, and channel status summaries.

Redaction is not an execution boundary. The call that resumes an approval
still uses the real parameters, and several at-rest surfaces keep the raw
values (see [What is not redacted](#what-is-not-redacted)).

Two mechanisms apply redaction, with different markers:

| Mechanism | Applies to | Marker |
|---|---|---|
| Wasm HTTP host boundary | Any `http_request` made by a WASM tool/channel | `[REDACTED:<placeholder_name>]` for `{PLACEHOLDER}` substitutions, `[REDACTED:host_credential]` for capability/host-resolved credentials |
| Display/event redaction | `ApprovalRequested.parameters`, approval prompts, `ToolCall` hooks, summaries | `[REDACTED]` |

## Default redacted parameter keys

Display redaction classifies a key with
`lunarwing_safety::is_credential_param_key`
(`ic/crates/lunarwing_safety/src/credential_detect.rs`). The key is normalized
first — fullwidth Latin (U+FF01..=U+FF5E) is folded to ASCII, then it is
lowercased and `_`/`-` are stripped — so `apiKey`, `api_key`, `api-key`, and
`ａｐｉ＿ｋｅｙ` all fold to `apikey`.

Substring matches (the default list, compiled in):

```
secret  token  passw  passphrase  passcode  passkey  apikey  auth
credential  privatekey  cookie  bearer  signature  pwd  jwt
sessionid  sessid  hmac  signingkey  sshkey  encryptionkey
accesskey  clientassertion
```

The short names `sid` and `pass` match only as a whole word of the key or at
the end of one (`sid`, `x_sid`, `AccountSid`, `connect.sid`, `db_pass`,
`smtpPass`); trailing ASCII digits are ignored (`pass1`, `sid2`), and the
ordinary words `bypass`, `compass`, `encompass`, `overpass`, `surpass`,
`trespass`, and `underpass` are exempt. Bare `key` and bare `sig` are
deliberately excluded to avoid over-redacting names such as `X-Idempotency-Key`
and `insights`.

Matching recurses into nested objects and arrays, so a positional payload
under `_args[0]` is covered. `null` and empty-string values are left untouched:
redacting them would claim a credential was present when none was.

## HTTP tool credential injection (WASM host boundary)

WASM tools never receive secret values. They reference credentials and the
host substitutes them in `http_request`
(`ic/src/tools/wasm/wrapper.rs`):

- **Placeholders** — a tool writes `{SECRET_NAME}` in the URL, headers, or
  body; the host replaces the placeholder with the stored value before the
  request leaves the sandbox.
- **Capability mappings** — `capabilities.http.credentials` in the tool's
  `*.capabilities.json` names the secret, the injection location (`bearer`,
  `basic`, `header`, `query_param`, `url_path`), and the `host_patterns` the
  credential applies to. These are resolved before the guest runs and become
  entries in the same host-credential list described next.
- **Host credentials** — credentials resolved for the requesting user are
  matched against the request host and merged into the request headers (host
  credentials take precedence over guest headers) and appended as URL query
  parameters when configured, gated by an optional URL prefix.

A credential is injected only when the request host matches the mapping's host
pattern; injected headers overwrite any guest-supplied header of the same name.
An exchange that injected any credential also forces `accept-encoding: identity`
and drops `TE` offers, so the response body stays scrubbable (see below).

## What the WASM HTTP boundary scrubs

`StoreData::redact_http_outcome_for_guest` is the single redaction point for
everything the guest sees (`ic/src/tools/wasm/wrapper.rs`). It covers:

- **Send and transport errors** — reqwest's error display includes the full
  URL, which may carry a query-parameter credential; the URL is stripped
  (`Error::without_url`) before the error chain is formatted, then the text is
  scrubbed.
- **Early validator refusals** — allowlist/private-IP/URL-validation deny
  reasons are built from the already-injected URL and may quote injected
  values.
- **Response headers** — redacted field by field, plus a cross-field pass that
  replaces both complete values when a secret is split across two headers.
- **Response bodies** — a byte-level scrub, so a redirect `Location` or echoed
  request is covered even when the body is not text.

The needle set for a tool execution contains the placeholder-injected secrets
(marker `[REDACTED:<placeholder_name>]`, using the placeholder the tool wrote,
e.g. `{GOOGLE_ACCESS_TOKEN}`) and the resolved capability/host credentials
(marker `[REDACTED:host_credential]`, which covers every
`capabilities.http.credentials` mapping and every host-resolved credential).
Needles include encoded spellings, because an upstream server may echo the
value after decoding or re-encoding:

- the raw value;
- percent-encoded and form-encoded spellings;
- the URL-parser's query and path normalization of the value;
- the `+`-to-space, percent-decoded spelling;
- the JSON-string-escaped inner value;
- lowercase and uppercase hexadecimal.

Values shorter than four bytes are not tracked (a short value would match
ordinary text). Needles are sorted longest-first, so a credential nested inside
another cannot leave a visible remainder, and duplicates are removed.

**Encoded bodies are refused.** When an exchange carried credentials and a
non-empty response declares any content coding other than `identity` — or a
transfer coding other than the single `chunked` layer hyper removes — the
boundary returns an error instead of the body:

```
HTTP response refused: encoded body cannot be inspected for echoed credentials
```

The host forces `identity` on those requests, so an encoded answer means the
peer ignored it; the boundary fails closed rather than handing over bytes that
might contain the credential.

> This section describes the WASM `http_request` host call. The built-in
> `http` and `web_fetch` tools do not use placeholder injection; they require
> approval unless auto-approved when their parameters carry manually supplied
> credentials or the target host has registered credentials, and their display
> copies are redacted as described next.

## `ApprovalRequested` display parameters

The persisted `ApprovalRequested` thread event
(`ic/crates/lunarwing_engine/src/types/event.rs`) stores a **display copy** of
the gated call's parameters in `parameters`, produced by
`redact_event_params`. The event history is an at-rest record, not the resume
path, so it keeps only the redacted copy.

1. If the whole argument set arrived as one JSON-encoded object string, the
   string is parsed, redacted, and re-serialized as a string — a string-encoded
   argument cannot hide a credential from the event.
2. Every credential-keyed value is replaced by `[REDACTED]`, recursively
   (see [Default redacted parameter keys](#default-redacted-parameter-keys)).
3. For `http` and `web_fetch`, the HTTP surface is redacted too:
   - **URL** — userinfo is removed, the fragment is dropped, and
     credential-like query values become `[REDACTED]`. Untouched pairs survive
     byte-for-byte, empty values are left alone, and an unparseable URL keeps
     only the text before `?`/`#`.
   - **Headers** — a value is blanked when the header name looks
     credential-like, when the normalized name ends in `key` (except
     `idempotencykey`), or when the value starts with an auth-scheme prefix
     (`Bearer `, `Basic `, …) even under an innocuous name. String-form
     `headers` are parsed, redacted inside, and re-serialized; unparseable
     string-form headers fail closed to `"[REDACTED]"`. Object and
     `[{name, value}]` array forms are both handled.
   - **Body** — a string body is parsed as JSON, recursively redacted, and
     re-serialized. A body that is not JSON is treated as a form/query string:
     credential-named pairs are redacted, everything else is preserved
     byte-for-byte.

`params_summary` is generated separately by `summarize_params`. For `http` and
`web_fetch` it shows the sanitized URL (query credentials removed, truncated to
80 characters); the generic fallback skips values whose keys look
credential-like; `mission_*` verbs summarize as `name`/`id` only, so a
mis-keyed `hmac_secret` or a positional `_args[0]` payload cannot ride the
summary into a status line.

`redact_display_params` applies the tool's exact-match `sensitive_params` and,
for `http`/`web_fetch`, delegates to `redact_event_params`
(`ic/src/tools/tool.rs`), so the prompt a user sees matches the persisted
event. `http` and `web_fetch` declare no `sensitive_params`; the HTTP-surface
arm is what keeps a raw `Authorization` header or `?api_key=…` URL out of the
prompt.

## Inspecting the sanitized strings

| Surface | How to read it |
|---|---|
| Live pending gates | `GET /api/chat/pending-gates` — each `PendingGateView.parameters` is the redacted display copy, pretty-printed as a JSON string; `tool_name` and `resume_kind` identify the call |
| History envelope | `GET /api/chat/history` — `pending_approval` carries `request_id`, `tool_name`, and the redacted display parameters for a live Engine V2 approval gate |
| Persisted event | The thread's `ApprovalRequested` event — `action_name`, `call_id`, `parameters` (redacted), `params_summary` |
| Hooks | `ToolCall` hook payloads receive the redacted parameter copy |

For Engine V2 gates these surfaces expose only the display copy; the
executable parameters stay on the in-memory gate. To see what an
approval would actually run, read the pending-gate store
(`$LUNARWING_BASE_DIR/pending-gates.json`, mode 0600) or the thread transcript —
both hold the raw copy.

## Configuring and extending redaction

- **WASM tools** declare injected credentials in `capabilities.http.credentials`
  in the tool's `*.capabilities.json` — `secret_name`, `location`, and
  `host_patterns`. This is also what makes the credential value a redaction
  needle for that tool's `http_request` calls.
- **Built-in tools** that accept plaintext secrets as parameters override
  `Tool::sensitive_params()` and return the exact parameter names to blank
  (`ic/src/tools/tool.rs`). This is an exact-match, per-tool contract — it can
  name a parameter (e.g. a plain `value` key) that the shared heuristic cannot
  infer, and it is intentionally distinct from `is_credential_param_key`.
- **The shared key list is compiled in.** There is no runtime or per-tenant
  setting that adds keys to it; extending the list is a source change in
  `lunarwing_safety::credential_detect`.

## What is not redacted

Redaction protects the display/at-rest surfaces listed above. It does not make
the following safe to share:

- the pending-gate store (`pending-gates.json`, mode 0600) and the in-memory
  gate, which need the executable parameters to resume;
- thread transcript action arguments — a pre-existing at-rest class;
- persisted `MessageAdded` content previews;
- daemon logs, which have their own scrubber but are not the same path.

A redacted approval prompt is evidence that the display copy was sanitized,
not that the credential was removed from every persisted surface.
