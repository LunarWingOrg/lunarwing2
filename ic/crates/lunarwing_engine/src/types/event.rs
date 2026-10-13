//! Event sourcing types.
//!
//! Every significant action within a thread is recorded as an event.
//! This enables replay, debugging, reflection, and trace-based testing.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::capability::LeaseId;

/// Generate a short human-readable summary of tool parameters for display.
///
/// For `http`: shows the URL. For `web_search`: shows the query.
/// For other tools: shows the first string argument, truncated.
/// Returns `None` for empty or unrecognizable params.
///
/// The generic fallback skips values whose keys look credential-like
/// ([`lunarwing_safety::is_credential_param_key`]) so a credential (e.g. a
/// `mission_create` `webhook_secret`) cannot ride the summary into a
/// channel status line or a persisted event (WI#352, AQ-5; WI#359, CJ-10).
pub fn summarize_params(action_name: &str, params: &serde_json::Value) -> Option<String> {
    let summary = match action_name {
        "http" | "web_fetch" => params
            .get("url")
            .and_then(|v| v.as_str())
            // WI#359 (CJ-11): the URL can carry `?api_key=…` — sanitize
            // before it reaches params_summary and the channel status.
            .map(|u| truncate(&lunarwing_safety::sanitize_url_credentials(u), 80)),
        "web_search" | "llm_context" => params
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| truncate(q, 60)),
        "memory_search" => params
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| truncate(q, 60)),
        "memory_write" => params
            .get("target")
            .and_then(|v| v.as_str())
            .map(|t| t.to_string()),
        "memory_read" => params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| p.to_string()),
        "shell" => params
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| truncate(c, 60)),
        "message" => params
            .get("content")
            .and_then(|v| v.as_str())
            .map(|c| truncate(c, 40)),
        _ if action_name.starts_with("mission_") => summarize_mission_params(params),
        _ => {
            // Generic: show the first string value whose key is not
            // credential-like (WI#352 AQ-5, WI#359 CJ-10).
            if let Some(obj) = params.as_object() {
                obj.iter()
                    .filter(|(key, _)| !lunarwing_safety::is_credential_param_key(key))
                    .find_map(|(_, v)| v.as_str())
                    .map(|s| truncate(s, 50))
            } else {
                None
            }
        }
    };
    summary.filter(|s| !s.is_empty())
}

/// WI#359 (CJ-10): mission verbs summarize as name/id only — arbitrary
/// kwargs (e.g. a mis-keyed `hmac_secret`) or a positional `_args[0]`
/// payload must never ride the summary into a status line or event.
fn summarize_mission_params(params: &serde_json::Value) -> Option<String> {
    for key in ["name", "id"] {
        if let Some(value) = params.get(key).and_then(|v| v.as_str()) {
            return Some(truncate(value, 50));
        }
    }
    // Positional form: `_args[0]` is the positional name/id — a string
    // summarizes directly; a dict (mission_update({"id": …, …})) yields
    // only its name/id field, never an arbitrary value.
    let first = params
        .get("_args")
        .and_then(|args| args.as_array())
        .and_then(|args| args.first());
    if let Some(first) = first {
        if let Some(value) = first.as_str() {
            return Some(truncate(value, 50));
        }
        for key in ["name", "id"] {
            if let Some(value) = first.get(key).and_then(|v| v.as_str()) {
                return Some(truncate(value, 50));
            }
        }
    }
    None
}

/// Return a display copy of `params` with credential material replaced by
/// `"[REDACTED]"` (WI#352 run-33 AQ-4/AQ-5, WI#359 CJ-10/CJ-11).
///
/// Credential-like keys are matched via the shared
/// [`lunarwing_safety::is_credential_param_key`] heuristic, recursing into
/// nested objects and arrays (a positional `_args[0]` payload included).
/// For `http`/`web_fetch`, credential-like header values and URL query
/// credentials are redacted too — those calls are approval-gated precisely
/// because they can carry manually supplied credentials.
///
/// Used when a gated call's parameters are persisted in an
/// [`EventKind::ApprovalRequested`] event: the event history is an at-rest
/// record, not the resume path, so it keeps only the redacted copy. The
/// pending gate holds the raw parameters for the actual resume.
pub fn redact_event_params(action_name: &str, params: &serde_json::Value) -> serde_json::Value {
    let mut redacted = lunarwing_safety::redact_credential_params(params);
    if matches!(action_name, "http" | "web_fetch") {
        redact_http_surface(&mut redacted);
    }
    redacted
}

/// WI#359 (CJ-11): blank credential-like header values and sanitize the
/// URL in `http`/`web_fetch` params. Null/empty values are left alone —
/// redacting them would falsely claim a credential was present.
fn redact_http_surface(redacted: &mut serde_json::Value) {
    if let Some(url) = redacted.get("url").and_then(|v| v.as_str()) {
        let sanitized = lunarwing_safety::sanitize_url_credentials(url);
        redacted["url"] = serde_json::Value::String(sanitized);
    }
    let Some(headers) = redacted.get_mut("headers") else {
        return;
    };
    if let serde_json::Value::String(raw) = headers {
        // Run-91 FI-1: the http tool also accepts `headers` as a JSON
        // string, and the approval gate runs on the coerced copy — the
        // persisted event would otherwise keep the exact credential that
        // caused the gate. Parse, redact inside, and re-serialise;
        // anything unparseable fails closed.
        let raw = std::mem::take(raw);
        *headers = match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(mut parsed) => {
                redact_header_pairs(&mut parsed);
                serde_json::Value::String(parsed.to_string())
            }
            Err(_) => serde_json::Value::String("[REDACTED]".into()),
        };
    } else {
        redact_header_pairs(headers);
    }
}

/// Whether one header name/value pair carries a credential (run-91
/// FI-2/FI-3): the name check unions the header heuristic with the shared
/// credential-key list (so `X-Goog-Api-Key`, `apikey`, or `api_key` are
/// caught), and a value starting with an auth-scheme prefix (`Bearer `,
/// `Basic `, …) is redacted even under an innocuous name.
fn header_pair_is_credential(name: &str, value: &serde_json::Value) -> bool {
    lunarwing_safety::header_name_carries_credentials(name)
        || lunarwing_safety::is_credential_param_key(name)
        || value
            .as_str()
            .is_some_and(lunarwing_safety::header_value_carries_credentials)
}

/// Blank credential-carrying header values in object or array form.
fn redact_header_pairs(headers: &mut serde_json::Value) {
    let is_present = |value: &serde_json::Value| {
        !value.is_null() && !matches!(value, serde_json::Value::String(s) if s.is_empty())
    };
    match headers {
        serde_json::Value::Object(map) => {
            for (name, value) in map.iter_mut() {
                if header_pair_is_credential(name, value) && is_present(value) {
                    *value = serde_json::Value::String("[REDACTED]".into());
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                let credential = match (
                    item.get("name").and_then(|n| n.as_str()),
                    item.get("value"),
                ) {
                    (Some(name), Some(value)) => header_pair_is_credential(name, value),
                    _ => false,
                };
                if credential
                    && let Some(value) = item.get_mut("value")
                    && is_present(value)
                {
                    *value = serde_json::Value::String("[REDACTED]".into());
                }
            }
        }
        _ => {}
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Find a safe UTF-8 boundary
        let mut end = max.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

pub const ACTION_RESULT_PREVIEW_MAX_BYTES: usize = 1000;

/// Build a bounded display preview from a tool output value.
pub fn preview_from_output(output: &serde_json::Value) -> Option<String> {
    let raw = match output {
        serde_json::Value::Null => return None,
        serde_json::Value::String(value) => value.clone(),
        value => value.to_string(),
    };

    if raw.is_empty() {
        None
    } else {
        Some(truncate(&raw, ACTION_RESULT_PREVIEW_MAX_BYTES))
    }
}
use crate::types::step::{StepId, TokenUsage};
use crate::types::thread::{ThreadId, ThreadState};

/// Strongly-typed event identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

/// A recorded event in a thread's execution history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadEvent {
    pub id: EventId,
    pub thread_id: ThreadId,
    pub timestamp: DateTime<Utc>,
    pub kind: EventKind,
}

impl ThreadEvent {
    pub fn new(thread_id: ThreadId, kind: EventKind) -> Self {
        Self {
            id: EventId::new(),
            thread_id,
            timestamp: Utc::now(),
            kind,
        }
    }
}

/// The specific kind of event that occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    // ── Thread lifecycle ────────────────────────────────────
    StateChanged {
        from: ThreadState,
        to: ThreadState,
        reason: Option<String>,
    },

    // ── Step lifecycle ──────────────────────────────────────
    StepStarted {
        step_id: StepId,
    },
    StepCompleted {
        step_id: StepId,
        tokens: TokenUsage,
    },
    StepFailed {
        step_id: StepId,
        error: String,
    },

    // ── Action execution ────────────────────────────────────
    ActionExecuted {
        step_id: StepId,
        action_name: String,
        call_id: String,
        duration_ms: u64,
        /// Short human-readable summary of parameters (e.g., URL for http tool).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params_summary: Option<String>,
        /// Bounded preview of the post-safety tool output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_preview: Option<String>,
    },
    ActionFailed {
        step_id: StepId,
        action_name: String,
        call_id: String,
        error: String,
        /// Short human-readable summary of parameters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params_summary: Option<String>,
    },

    // ── Capability leases ───────────────────────────────────
    LeaseGranted {
        lease_id: LeaseId,
        capability_name: String,
    },
    LeaseRevoked {
        lease_id: LeaseId,
        reason: String,
    },
    LeaseExpired {
        lease_id: LeaseId,
    },

    // ── Messages ────────────────────────────────────────────
    MessageAdded {
        role: String,
        content_preview: String,
    },
    /// A live, provider-neutral assistant text fragment.
    ///
    /// This event is broadcast for delivery but is not persisted in the
    /// thread's event history.
    ResponseDelta {
        content: String,
    },

    // ── Thread tree ─────────────────────────────────────────
    ChildSpawned {
        child_id: ThreadId,
        goal: String,
    },
    ChildCompleted {
        child_id: ThreadId,
    },

    // ── Approval flow ───────────────────────────────────────
    ApprovalRequested {
        action_name: String,
        call_id: String,
        /// Display copy of the gated call's parameters with credential
        /// values redacted (WI#352 run-33 AQ-4, WI#359 CJ-10/CJ-11 — shared
        /// `lunarwing_safety` predicate, recursive, http header/query
        /// surfaces included): this event is persisted in the thread's event
        /// history, which is an at-rest record, not the resume path. The raw
        /// parameters live on the host's pending gate (unredacted, needed to
        /// resume) and in the thread transcript's action arguments (a
        /// pre-existing at-rest class).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameters: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allow_always: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gate_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params_summary: Option<String>,
    },
    ApprovalReceived {
        call_id: String,
        approved: bool,
    },

    // ── Self-improvement ──────────────────────────────────────
    SelfImprovementStarted,
    SelfImprovementComplete {
        prompt_updated: bool,
        patterns_added: usize,
    },
    SelfImprovementFailed {
        error: String,
    },

    // ── Skill activation ───────────────────────────────────────
    SkillActivated {
        skill_names: Vec<String>,
    },

    // ── Orchestrator versioning ───────────────────────────────
    OrchestratorRollback {
        from_version: u64,
        to_version: u64,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        EventKind, ThreadEvent, preview_from_output, redact_event_params, summarize_params,
    };
    use crate::types::thread::ThreadId;

    #[test]
    fn summarize_params_never_surfaces_sensitive_values() {
        // WI#352 (run-33 AQ-5): a CodeAct mission_create carrying a webhook
        // secret must not put the secret into the channel status summary,
        // even when it is the only (or first) string value.
        let secret_only = serde_json::json!({"webhook_secret": "super-secret-value"});
        assert_eq!(summarize_params("mission_create", &secret_only), None);

        // WI#359 (CJ-10): mission verbs summarize as name/id/`_args[0]`
        // only — arbitrary kwargs (benign or not) never surface, so a
        // mis-keyed credential cannot ride the summary.
        let arbitrary_kwarg = serde_json::json!({
            "webhook_secret": "super-secret-value",
            "zzz": "benign-tail",
        });
        assert_eq!(summarize_params("mission_create", &arbitrary_kwarg), None);

        // ...but the mission's own name/id still summarize.
        let named = serde_json::json!({
            "auth_token": "super-secret-value",
            "name": "n",
        });
        let summary = summarize_params("mission_update", &named).expect("name summarizes");
        assert_eq!(summary, "n");

        // A mis-keyed credential kwarg (hmac_secret is not on any exact
        // list) must not surface either.
        let miskeyed = serde_json::json!({
            "id": "m-1",
            "hmac_secret": "super-secret-value",
        });
        let summary = summarize_params("mission_update", &miskeyed).expect("id summarizes");
        assert_eq!(summary, "m-1");
        assert!(!summary.contains("super-secret-value"));

        // The CodeAct call shape from the work item: `_args` is an array;
        // its first element is the positional name/id and may summarize,
        // but a nested secret never does.
        let codeact = serde_json::json!({
            "_args": ["digest", "report on events"],
            "webhook_secret": "super-secret-value",
        });
        let summary =
            summarize_params("mission_create", &codeact).expect("positional name summarizes");
        assert_eq!(summary, "digest");
        assert!(!summary.contains("super-secret-value"));

        // Positional dict form: mission_update({"id": …, "webhook_secret": …})
        // summarizes the dict's id, never the secret.
        let positional_dict = serde_json::json!({
            "_args": [{"id": "m-9", "webhook_secret": "super-secret-value"}],
        });
        let summary =
            summarize_params("mission_update", &positional_dict).expect("dict id summarizes");
        assert_eq!(summary, "m-9");
        assert!(!summary.contains("super-secret-value"));

        for key in ["api_key", "auth_token", "password", "secret", "token"] {
            let params = serde_json::json!({key: "super-secret-value"});
            assert_eq!(
                summarize_params("some_custom_tool", &params),
                None,
                "{key} must be deny-listed"
            );
        }

        // WI#359 (CJ-10): the generic fallback uses the shared normalized
        // credential-key predicate, not an exact list.
        for key in [
            "hmac_secret",
            "access_token",
            "client_secret",
            "apiKey",
            "x-auth",
        ] {
            let params = serde_json::json!({key: "super-secret-value"});
            assert_eq!(
                summarize_params("some_custom_tool", &params),
                None,
                "{key} must be deny-listed"
            );
        }
    }

    #[test]
    fn summarize_params_http_url_drops_query_credentials() {
        // WI#359 (CJ-11): the http summary puts the URL into params_summary
        // and the channel status — credential query values must not survive.
        let params = serde_json::json!({
            "url": "https://api.example.com/data?api_key=abc123&page=2",
        });
        let summary = summarize_params("http", &params).expect("url summarizes");
        assert!(!summary.contains("abc123"), "api_key leaked: {summary}");
        assert!(summary.contains("page=2"), "safe param dropped: {summary}");
    }

    #[test]
    fn redact_event_params_replaces_only_sensitive_values() {
        let params = serde_json::json!({
            "name": "digest",
            "webhook_secret": "super-secret-value",
            "max_threads_per_day": 5,
        });
        let redacted = redact_event_params("mission_update", &params);
        assert_eq!(redacted["webhook_secret"], "[REDACTED]");
        assert_eq!(redacted["name"], "digest");
        assert_eq!(redacted["max_threads_per_day"], 5);
        assert!(!redacted.to_string().contains("super-secret-value"));
        // The input is untouched.
        assert_eq!(params["webhook_secret"], "super-secret-value");

        // Non-object params pass through unchanged.
        let scalar = serde_json::json!("webhook_secret");
        assert_eq!(redact_event_params("mission_update", &scalar), scalar);
    }

    #[test]
    fn redact_event_params_recurses_into_positional_args() {
        // WI#359 (CJ-10): mission_update({"id":…,"webhook_secret":s}) hides
        // the secret under `_args[0]` — a top-level-only pass misses it.
        let params = serde_json::json!({
            "_args": [{"id": "m-1", "webhook_secret": "super-secret-value"}],
        });
        let redacted = redact_event_params("mission_update", &params);
        assert_eq!(redacted["_args"][0]["webhook_secret"], "[REDACTED]");
        assert_eq!(redacted["_args"][0]["id"], "m-1");
        assert!(!redacted.to_string().contains("super-secret-value"));

        // Mis-keyed kwargs (not on any exact list) are redacted too.
        let miskeyed = serde_json::json!({"id": "m-1", "hmac_secret": "super-secret-value"});
        let redacted = redact_event_params("mission_update", &miskeyed);
        assert_eq!(redacted["hmac_secret"], "[REDACTED]");
        assert_eq!(redacted["id"], "m-1");
    }

    #[test]
    fn redact_event_params_http_surface_blanks_credentials() {
        // WI#359 (CJ-11): http calls are approval-gated precisely because
        // they can carry manually supplied credentials — the persisted
        // event must not keep header values or URL query credentials raw.
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://user:pass@api.example.com/data?api_key=abc123&page=2",
            "headers": [
                {"name": "Authorization", "value": "Bearer raw-token"},
                {"name": "Content-Type", "value": "application/json"},
            ],
        });
        let redacted = redact_event_params("http", &params);
        let url = redacted["url"].as_str().expect("url stays a string");
        assert!(!url.contains("abc123"), "query credential leaked: {url}");
        assert!(!url.contains("user:pass"), "userinfo leaked: {url}");
        assert!(url.contains("page=2"), "safe query param dropped: {url}");
        assert_eq!(redacted["headers"][0]["value"], "[REDACTED]");
        assert_eq!(redacted["headers"][1]["value"], "application/json");
        assert!(!redacted.to_string().contains("raw-token"));

        // Object-form headers are covered too.
        let params = serde_json::json!({
            "url": "https://api.example.com",
            "headers": {"X-Api-Key": "raw-key", "Accept": "text/html"},
        });
        let redacted = redact_event_params("http", &params);
        assert_eq!(redacted["headers"]["X-Api-Key"], "[REDACTED]");
        assert_eq!(redacted["headers"]["Accept"], "text/html");
    }

    #[test]
    fn redact_event_params_http_string_headers_are_redacted_fail_closed() {
        // Run-91 FI-1: the http tool accepts `headers` as a JSON string and
        // the approval gate runs on the coerced copy, so the persisted
        // event must not keep the raw string either.
        let params = serde_json::json!({
            "url": "https://api.example.com",
            "headers": "[{\"name\":\"Authorization\",\"value\":\"Bearer raw-token\"}]",
        });
        let redacted = redact_event_params("http", &params);
        let headers = redacted["headers"].as_str().expect("stays a string");
        assert!(!headers.contains("raw-token"), "credential leaked: {headers}");
        let parsed: serde_json::Value =
            serde_json::from_str(headers).expect("re-serialised headers stay valid JSON");
        assert_eq!(parsed[0]["value"], "[REDACTED]");
        assert_eq!(parsed[0]["name"], "Authorization");

        // Object form inside the string is covered too.
        let params = serde_json::json!({
            "url": "https://api.example.com",
            "headers": "{\"X-Api-Key\":\"raw-key\"}",
        });
        let redacted = redact_event_params("http", &params);
        let headers = redacted["headers"].as_str().expect("stays a string");
        assert!(!headers.contains("raw-key"), "credential leaked: {headers}");

        // An unparseable string fails closed.
        let params = serde_json::json!({
            "url": "https://api.example.com",
            "headers": "Authorization: Bearer raw-token",
        });
        let redacted = redact_event_params("http", &params);
        assert_eq!(redacted["headers"], "[REDACTED]");
    }

    #[test]
    fn redact_event_params_http_value_prefix_credential_is_redacted() {
        // Run-91 FI-2: the approval gate reacts to auth-scheme value
        // prefixes, so redaction must blank them even under an innocuous
        // header name — in both array and object form.
        let params = serde_json::json!({
            "url": "https://api.example.com",
            "headers": [{"name": "X-Custom", "value": "Bearer raw-token"}],
        });
        let redacted = redact_event_params("http", &params);
        assert_eq!(redacted["headers"][0]["value"], "[REDACTED]");

        let params = serde_json::json!({
            "url": "https://api.example.com",
            "headers": {"X-Custom": "Basic dXNlcjpwYXNz"},
        });
        let redacted = redact_event_params("http", &params);
        assert_eq!(redacted["headers"]["X-Custom"], "[REDACTED]");
    }

    #[test]
    fn redact_event_params_http_uses_the_shared_credential_key_list() {
        // Run-91 FI-3: header and query checks union the http heuristic
        // with the shared credential-key list, so the documented schema
        // shape (array form) and provider key styles are covered.
        let params = serde_json::json!({
            "url": "https://api.example.com/data?x-api-key=raw-key&q=hello",
            "headers": [{"name": "X-Goog-Api-Key", "value": "raw-goog-key"}],
        });
        let redacted = redact_event_params("http", &params);
        assert_eq!(redacted["headers"][0]["value"], "[REDACTED]");
        let url = redacted["url"].as_str().expect("url stays a string");
        assert!(!url.contains("raw-key"), "query credential leaked: {url}");
        assert!(url.contains("q=hello"), "safe query param dropped: {url}");

        let params = serde_json::json!({
            "url": "https://api.example.com",
            "headers": {"apikey": "raw-key"},
        });
        let redacted = redact_event_params("http", &params);
        assert_eq!(redacted["headers"]["apikey"], "[REDACTED]");
    }

    #[test]
    fn response_delta_round_trips_through_serde() {
        let event = ThreadEvent::new(
            ThreadId::new(),
            EventKind::ResponseDelta {
                content: "partial response".to_string(),
            },
        );

        let json = serde_json::to_value(&event).expect("response delta should serialize");
        let decoded: ThreadEvent =
            serde_json::from_value(json).expect("response delta should deserialize");

        assert_eq!(decoded.thread_id, event.thread_id);
        assert!(matches!(
            decoded.kind,
            EventKind::ResponseDelta { content } if content == "partial response"
        ));
    }

    #[test]
    fn action_result_preview_handles_strings_json_and_empty_values() {
        assert_eq!(preview_from_output(&serde_json::Value::Null), None);
        assert_eq!(preview_from_output(&serde_json::json!("")), None);
        assert_eq!(
            preview_from_output(&serde_json::json!("hello")).as_deref(),
            Some("hello")
        );
        assert_eq!(
            preview_from_output(&serde_json::json!({"count": 2})).as_deref(),
            Some("{\"count\":2}")
        );
    }

    #[test]
    fn action_result_preview_truncates_at_utf8_boundary() {
        let preview = preview_from_output(&serde_json::json!("你".repeat(400)))
            .expect("non-empty output should produce a preview");

        assert!(preview.len() <= 1003);
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn action_executed_without_preview_round_trips_as_legacy_event() {
        let event = ThreadEvent::new(
            ThreadId::new(),
            EventKind::ActionExecuted {
                step_id: crate::types::step::StepId::new(),
                action_name: "shell".into(),
                call_id: "call-1".into(),
                duration_ms: 1,
                params_summary: None,
                result_preview: None,
            },
        );

        let json = serde_json::to_value(&event).expect("event should serialize");
        assert!(
            json["kind"]["ActionExecuted"]
                .get("result_preview")
                .is_none()
        );
        let decoded: ThreadEvent =
            serde_json::from_value(json).expect("legacy event should deserialize");
        assert!(matches!(
            decoded.kind,
            EventKind::ActionExecuted {
                result_preview: None,
                ..
            }
        ));
    }
}
