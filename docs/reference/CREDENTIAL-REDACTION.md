# Credential Redaction in Tool Output and Approval Events

LunarWing redacts credentials on the paths where they would otherwise be
**displayed, persisted, or handed back to untrusted code** — WebAssembly tool
errors and HTTP responses, approval prompts, `ApprovalRequested` thread
events, `ToolCall` hook payloads, and channel status summaries.

Redaction is not an execution boundary. The call that resumes an approval
still uses the real parameters, and several at-rest surfaces keep the raw
values (see [What is not redacted](#what-is-not-redacted)).

How much is redacted differs per surface. The persisted event gets the shared
key list for every action; approval prompts, hook payloads, and failure
statuses get it only for some actions. See
[Which surface gets which redaction](#which-surface-gets-which-redaction)
before relying on any of them.

Two mechanisms apply redaction, with different markers:

| Mechanism | Applies to | Marker |
|---|---|---|
| Wasm HTTP host boundary | Any `http_request` made by a WASM tool/channel | `[REDACTED:<placeholder_name>]` for `{PLACEHOLDER}` substitutions, `[REDACTED:host_credential]` for capability/host-resolved credentials |
| Display/event redaction | `ApprovalRequested.parameters` for every action; approval prompts, `ToolCall` hooks, and failure statuses to the extent listed [per surface](#which-surface-gets-which-redaction); summaries | `[REDACTED]` |

## Default redacted parameter keys

The shared key list is `lunarwing_safety::is_credential_param_key`
(`ic/crates/lunarwing_safety/src/credential_detect.rs`). It is applied to the
persisted `ApprovalRequested` event for every action, and to the other display
surfaces only for the actions named
[per surface](#which-surface-gets-which-redaction). The key is normalized
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

WASM tools and channels never receive secret values. The host adds
credentials to their `http_request` calls
(`ic/src/tools/wasm/wrapper.rs`, `ic/src/channels/wasm/wrapper.rs`):

- **Placeholders** — the guest writes `{SECRET_NAME}` in the URL or in a
  header value; the host replaces the placeholder with the stored value before
  the request leaves the sandbox. The request body is not substituted. The
  placeholder map is filled for WASM channels. For a WASM tool it is empty in
  production, so a tool gets its credentials through capability mappings.
- **Capability mappings** — `capabilities.http.credentials` in the tool's
  `*.capabilities.json` names the secret, the injection location (`bearer`,
  `basic`, `header`, `query_param`, `url_path`), and the `host_patterns` the
  credential applies to. These are resolved before the guest runs and become
  entries in the same host-credential list described next. The exception is
  `url_path`: such a mapping is left to placeholder substitution and never
  joins that list, so for a tool it injects nothing today. No bundled tool
  declares one.
- **Host credentials** — credentials resolved for the requesting user are
  matched against the request host and merged into the request headers (a
  host credential replaces a guest header of the identical name) and appended as URL query
  parameters when configured, gated by an optional URL prefix.

A capability or host credential is injected only when the request host matches
the mapping's host pattern. A placeholder is substituted wherever the guest
wrote it: no host pattern binds it, and the request passes only the general
checks (the HTTP allowlist and private-address vetting). An injected
header replaces a guest-supplied header only when the name is spelled the same,
case included. With another spelling (a guest `authorization` next to the
injected `Authorization`) both are sent; the guest still never sees the
injected value.
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

The needle set for an execution contains every secret in the placeholder map
(marker `[REDACTED:<placeholder_name>]`, e.g. `{GOOGLE_ACCESS_TOKEN}`; the map
is empty for a tool today) and the resolved capability/host credentials
(marker `[REDACTED:host_credential]`, which covers every
`capabilities.http.credentials` mapping except `url_path` ones, and every
host-resolved credential).
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
> `http` tool does not use placeholder injection; it requires approval unless
> auto-approved when its parameters carry manually supplied credentials or the
> target host has registered credentials, and its display copies are redacted
> as described next. There is no built-in `web_fetch` tool. The name is on the
> registry's protected list, but that list only stops an extension from
> replacing a built-in that is registered, so by itself it does not reserve
> the name. Where this page says "`http` and `web_fetch`" it means that
> redaction treats an action of either name alike.

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
`web_fetch` it shows the sanitized URL (query credentials removed, cut after
80 bytes and followed by `...`); the generic fallback skips values whose keys look
credential-like; `mission_*` verbs summarize as `name`/`id` only, so a
mis-keyed `hmac_secret` or a positional `_args[0]` payload cannot ride the
summary into a status line.

## Which surface gets which redaction

Engine V2 and the v1 chat loop do not share every surface. The persisted
`ApprovalRequested` event exists only on Engine V2, and it is the only surface
that gets the shared key list for every action. The failed-tool status that
carries parameters exists only on v1.

Approval prompts, the Engine V2 hook payload of a registered tool, and the v1
failure status go through `redact_display_params` (`ic/src/tools/tool.rs`): it
applies the tool's exact-match `sensitive_params` and, for an action named
`http` or `web_fetch` only, delegates to `redact_event_params`. Mission verbs
take the shared key list directly. The hook payloads of the v1 chat loop, the
background job worker, and the external-worker tool proxy get
`sensitive_params` only. So for an `http` call the prompt a user sees matches
the persisted event; for any tool other than `http` and the mission verbs, the
prompt is not passed through the key list. `http` declares no
`sensitive_params`; the HTTP-surface arm is what keeps a raw `Authorization`
header or `?api_key=…` URL out of the prompt.

| Surface | Shared key list | HTTP surface (URL, headers, body) | Tool's `sensitive_params` |
|---|---|---|---|
| Persisted `ApprovalRequested.parameters` (Engine V2) | every action | `http`, `web_fetch` | not applied |
| Engine V2 approval prompt, and the copies of it served by `GET /api/chat/pending-gates` and the history envelope | `http`, `web_fetch`, and `mission_*` actions | `http`, `web_fetch`; on the first prompt also when no tool of that name is registered | applied when the tool is registered |
| v1 approval prompt, and its copy in the history envelope | `http`, `web_fetch` | `http`, `web_fetch` | applied |
| v1 failed-tool channel status | `http`, `web_fetch` | `http`, `web_fetch` | applied when the tool is known |
| `ToolCall` hook payload, Engine V2 | `http`, and `mission_*` actions; `web_fetch` only if a tool of that name is registered | `http`; `web_fetch` only if a tool of that name is registered | applied; **an action that is neither a mission verb nor a registered tool passes its parameters on unredacted** |
| `ToolCall` hook payload: v1 chat loop, background job worker, external-worker tool proxy | not applied | not applied | applied |

A gate that Engine V2 raises again after an approval shows `null` in place of
the parameters when its tool is no longer registered.

No built-in tool declares `sensitive_params` today, and a WASM or MCP tool
cannot declare any, so the last column changes nothing at present.

What this means in practice: take a WASM or MCP tool called with
`{"api_key": "…"}`.

- On Engine V2 the persisted event shows `[REDACTED]`; the approval prompt and
  the `ToolCall` hook payload show the value.
- On v1, which has no `ApprovalRequested` event, the approval prompt, the hook
  payload, and the failed-tool status show it.

Do not send hook payloads or approval prompts to an external consumer on the
assumption that the shared key list was applied to them.

## Inspecting the sanitized strings

| Surface | How to read it |
|---|---|
| Live pending gates (Engine V2) | `GET /api/chat/pending-gates` — each `PendingGateView.parameters` is the approval prompt's display copy, pretty-printed as a JSON string; `tool_name` and `resume_kind` identify the call |
| History envelope | `GET /api/chat/history` — `pending_approval` carries `request_id`, `tool_name`, and the approval prompt's display parameters for a live approval (an Engine V2 gate or a v1 approval) |
| Persisted event | The thread's `ApprovalRequested` event — `action_name`, `call_id`, `parameters` (redacted with the shared key list), `params_summary` |
| Hooks | `ToolCall` hook payloads receive the hook copy, which is redacted the same as the persisted event or less |

The prompt and hook copies are redacted as listed
[per surface](#which-surface-gets-which-redaction), not as the persisted event
is.

For Engine V2 gates these surfaces expose only the display copy; the
executable parameters stay on the in-memory gate. To see what an
approval would actually run, read the pending-gate store
(`$LUNARWING_BASE_DIR/pending-gates.json`, mode 0600) or the thread transcript —
both hold the raw copy.

## Configuring and extending redaction

- **WASM tools** declare injected credentials in `capabilities.http.credentials`
  in the tool's `*.capabilities.json` — `secret_name`, `location`, and
  `host_patterns`. This is also what makes the credential value a redaction
  needle for that tool's `http_request` calls (except for a `url_path`
  mapping, which is never resolved for a tool).
- **A Rust tool compiled into the daemon** that accepts plaintext secrets as parameters can override
  `Tool::sensitive_params()` and return the exact parameter names to blank
  (`ic/src/tools/tool.rs`). This is an exact-match, per-tool contract — it can
  name a parameter (e.g. a plain `value` key) that the shared heuristic cannot
  infer, and it is intentionally distinct from `is_credential_param_key`. No
  built-in tool overrides it today; the default is an empty list. A WASM or
  MCP tool cannot declare any.
- **The shared key list is compiled in.** There is no runtime or per-tenant
  setting that adds keys to it; extending the list is a source change in
  `lunarwing_safety::credential_detect`.

## What is not redacted

Redaction protects the display/at-rest surfaces listed above. It does not make
the following safe to share:

- the pending-gate store (`pending-gates.json`, mode 0600) and the in-memory
  gate, which need the executable parameters to resume;
- thread transcript action arguments — a pre-existing at-rest class;
- job `tool_use` events, which are persisted and sent to the job's event
  stream: the in-process job worker records the first 500 bytes of the
  parameters as they are, and the external-worker tool proxy applies
  `sensitive_params` and its own leak scan but not the shared key list;
- persisted `MessageAdded` content previews;
- daemon logs, which have their own scrubber but are not the same path.

A redacted approval prompt is evidence that the display copy was sanitized,
not that the credential was removed from every persisted surface.
