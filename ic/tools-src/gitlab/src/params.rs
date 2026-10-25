//! Action parameter validation with actionable rejections (WI#315).
//!
//! `GitlabAction` is a strict-schema (`deny_unknown_fields`) internally
//! tagged enum, so serde's own rejection names only the field it tripped
//! over. That is accurate but not actionable: an agent that remembers a
//! looser spelling from the older GitLab tool learns nothing about which
//! names this action actually accepts. Validate the raw params first and
//! answer with the action's exact parameter set, plus a rename hint when a
//! near-miss is obvious.

use serde_json::Value;

/// Accepted parameter names for every action, in `GitlabAction` declaration
/// order. The generated schema is the source of truth; the
/// `table_matches_generated_schema` test fails if this table drifts.
pub(crate) const ACTION_PARAMS: &[(&str, &[&str])] = &[
    ("current_user", &[]),
    (
        "list_projects",
        &["per_page", "page", "search", "membership"],
    ),
    ("get_project", &["project"]),
    (
        "create_project",
        &[
            "name",
            "path",
            "description",
            "visibility",
            "initialize_with_readme",
        ],
    ),
    (
        "list_issues",
        &["project", "state", "per_page", "page", "all_pages"],
    ),
    ("get_issue", &["project", "iid"]),
    ("get_work_item", &["project", "iid"]),
    (
        "list_work_items",
        &["project", "state", "per_page", "cursor", "all_pages"],
    ),
    (
        "create_issue",
        &["project", "title", "description", "labels", "assignee_ids"],
    ),
    (
        "update_issue",
        &[
            "project",
            "iid",
            "title",
            "description",
            "state_event",
            "labels",
        ],
    ),
    ("list_issue_notes", &["project", "iid", "per_page", "page"]),
    ("create_issue_note", &["project", "iid", "body"]),
    (
        "list_merge_requests",
        &["project", "state", "per_page", "page"],
    ),
    ("get_merge_request", &["project", "iid"]),
    (
        "create_merge_request",
        &[
            "project",
            "source_branch",
            "target_branch",
            "title",
            "description",
            "remove_source_branch",
            "squash",
        ],
    ),
    (
        "update_merge_request",
        &[
            "project",
            "iid",
            "title",
            "description",
            "state_event",
            "target_branch",
        ],
    ),
    ("list_mr_notes", &["project", "iid", "per_page", "page"]),
    ("create_mr_note", &["project", "iid", "body"]),
    ("approve_merge_request", &["project", "iid", "sha"]),
    (
        "merge_merge_request",
        &[
            "project",
            "iid",
            "sha",
            "merge_commit_message",
            "squash",
            "should_remove_source_branch",
        ],
    ),
    ("list_branches", &["project", "search", "per_page", "page"]),
    ("get_branch", &["project", "branch"]),
    ("create_branch", &["project", "branch", "ref"]),
    ("delete_branch", &["project", "branch"]),
    ("get_file_content", &["project", "path", "ref"]),
    (
        "create_or_update_file",
        &[
            "project",
            "path",
            "branch",
            "content",
            "commit_message",
            "encoding",
            "author_email",
            "author_name",
            "last_commit_id",
        ],
    ),
    (
        "delete_file",
        &[
            "project",
            "path",
            "branch",
            "commit_message",
            "last_commit_id",
        ],
    ),
    ("search_projects", &["query", "per_page", "page"]),
    ("search_issues", &["project", "query", "per_page", "page"]),
    (
        "search_blobs",
        &["project", "query", "ref", "per_page", "page"],
    ),
    (
        "list_pipelines",
        &["project", "status", "ref", "per_page", "page"],
    ),
    ("get_pipeline", &["project", "pipeline_id"]),
    ("list_jobs", &["project", "pipeline_id", "per_page", "page"]),
];

/// Serde aliases that hold for every action carrying the canonical field.
const GLOBAL_ALIASES: &[(&str, &str)] = &[("project_id", "project")];

/// Echoed-name and report bounds (run-214 NX-5): the rejection message is
/// built from caller-controlled bytes that land verbatim in the model
/// context, so names are escaped, head-truncated, count-capped, and the
/// `Valid` list is printed once instead of per name.
const MAX_ECHOED_NAME_CHARS: usize = 64;
const MAX_REPORTED_NAMES: usize = 10;

/// Serde aliases scoped to one action, because the alias name is a real
/// field of other actions: `body` is the note text everywhere except in the
/// two issue-write actions, where it spells `description`.
const SCOPED_ALIASES: &[(&str, &str, &str)] = &[
    ("create_issue", "body", "description"),
    ("update_issue", "body", "description"),
];

/// Habitual misspellings, as (spelling, canonical field). A hint is emitted
/// only when the canonical field is valid for the action and the spelling is
/// not, so the table serves both directions (`ref`/`branch`,
/// `query`/`search`).
const SYNONYMS: &[(&str, &str)] = &[
    ("branch", "ref"),
    ("ref", "branch"),
    ("ref_name", "ref"),
    ("branch_name", "branch"),
    ("sha", "ref"),
    ("query", "search"),
    ("search", "query"),
    ("q", "query"),
    ("page_size", "per_page"),
    ("limit", "per_page"),
    ("filename", "path"),
    ("file_path", "path"),
    ("note", "body"),
    ("body", "description"),
    ("mr_iid", "iid"),
    ("merge_request_iid", "iid"),
];

/// Validate the raw parameter object's field names before serde sees them.
///
/// Returns every offending name at once: a retry that fixes one field only
/// to trip over the next wastes an approval prompt per attempt.
pub(crate) fn validate(params: &str) -> Result<(), String> {
    let value: Value = serde_json::from_str(params)
        .map_err(|error| format!("params must be a JSON object: {error}"))?;
    let Some(object) = value.as_object() else {
        return Err("params must be a JSON object carrying an \"action\" field".to_string());
    };
    let Some(action) = object.get("action").and_then(Value::as_str) else {
        return Err(format!(
            "action is required and must be a string. Valid actions: [{}]",
            action_names()
        ));
    };
    let Some(valid) = valid_params(action) else {
        return Err(unknown_action(action));
    };

    let mut errors = Vec::new();
    let mut offending = 0usize;
    for field in object.keys() {
        if field == "action" || accepts(action, valid, field) {
            continue;
        }
        offending += 1;
        if errors.len() < MAX_REPORTED_NAMES {
            errors.push(invalid_field(action, field, valid));
        }
    }
    if offending == 0 {
        Ok(())
    } else {
        if offending > MAX_REPORTED_NAMES {
            errors.push(format!("(+{} more)", offending - MAX_REPORTED_NAMES));
        }
        Err(format!(
            "{}. Valid: [{}]",
            errors.join("; "),
            valid.join(", ")
        ))
    }
}

pub(crate) fn valid_params(action: &str) -> Option<&'static [&'static str]> {
    ACTION_PARAMS
        .iter()
        .find(|(name, _)| *name == action)
        .map(|(_, params)| *params)
}

fn accepts(action: &str, valid: &[&str], field: &str) -> bool {
    valid.contains(&field)
        || GLOBAL_ALIASES
            .iter()
            .any(|(alias, canonical)| *alias == field && valid.contains(canonical))
        || SCOPED_ALIASES.iter().any(|(scope, alias, canonical)| {
            *scope == action && *alias == field && valid.contains(canonical)
        })
}

fn invalid_field(action: &str, field: &str, valid: &[&str]) -> String {
    let mut message = format!("{} not valid for {action}", echo_name(field));
    if let Some(hint) = hint_for(field, valid) {
        message.push_str("; ");
        message.push_str(&hint);
    }
    message
}

/// Echo a caller-supplied field name safely (run-214 NX-5): names are
/// attacker-controlled bytes that would otherwise land verbatim in the
/// model context. Control characters and quotes are `escape_debug`-escaped
/// and over-long names are head-truncated with the dropped char count.
fn echo_name(field: &str) -> String {
    let escaped: String = field.escape_debug().collect();
    let total = escaped.chars().count();
    if total > MAX_ECHOED_NAME_CHARS {
        let head: String = escaped.chars().take(MAX_ECHOED_NAME_CHARS).collect();
        format!("{head}…(+{} chars)", total - MAX_ECHOED_NAME_CHARS)
    } else {
        escaped
    }
}

/// Turn a rejected name into a rename instruction when one is defensible.
fn hint_for(field: &str, valid: &[&str]) -> Option<String> {
    if field == "all_pages" && valid.contains(&"page") {
        // Only actions that page can hear the paging explanation: a
        // `page`-capable action rejects all_pages because it returns one
        // page and pages with page/per_page. Actions without page/per_page
        // at all (get_issue, create_issue, get_file_content, …) fall
        // through to the normal hints (run-214 NX-8).
        return Some("this action returns a single page; use page/per_page".to_string());
    }
    // Long names skip the did-you-mean entirely: they are a near-miss of
    // nothing here, and `edit_distance` on unbounded input is the
    // amplification NX-5 closes.
    if field.chars().count() > MAX_ECHOED_NAME_CHARS {
        return None;
    }
    if let Some((_, candidate)) = SYNONYMS
        .iter()
        .find(|(wrong, right)| *wrong == field && valid.contains(right))
    {
        return Some(format!("use {candidate}"));
    }
    closest(field, valid).map(|candidate| format!("did you mean {candidate}?"))
}

fn unknown_action(action: &str) -> String {
    let names: Vec<&str> = ACTION_PARAMS.iter().map(|(name, _)| *name).collect();
    // Bound the caller-supplied action echo too (NX-5): `{:?}` escapes
    // control characters, and an over-long name is head-truncated.
    let total = action.chars().count();
    let mut message = if total > MAX_ECHOED_NAME_CHARS {
        let head: String = action.chars().take(MAX_ECHOED_NAME_CHARS).collect();
        format!(
            "unknown action {head:?} (+{} chars)",
            total - MAX_ECHOED_NAME_CHARS
        )
    } else {
        format!("unknown action {action:?}")
    };
    if total <= MAX_ECHOED_NAME_CHARS
        && let Some(candidate) = closest(action, &names)
    {
        message.push_str(&format!("; did you mean {candidate}?"));
    }
    message.push_str(&format!(". Valid actions: [{}]", names.join(", ")));
    message
}

fn action_names() -> String {
    ACTION_PARAMS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Closest candidate within a small edit distance, preferring the earliest
/// declaration when several tie.
fn closest<'a>(field: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(usize, &'a str)> = None;
    for candidate in candidates {
        let distance = edit_distance(field, candidate);
        if distance > max_suggestion_distance(candidate) {
            continue;
        }
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

fn max_suggestion_distance(candidate: &str) -> usize {
    if candidate.chars().count() <= 4 { 1 } else { 2 }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().flat_map(char::to_lowercase).collect();
    let right: Vec<char> = right.chars().flat_map(char::to_lowercase).collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use schemars::schema_for;
    use serde_json::{Value, json};

    use super::{ACTION_PARAMS, MAX_ECHOED_NAME_CHARS, valid_params, validate};
    use crate::types::GitlabAction;

    /// Every action's parameter set as the generated schema describes it.
    fn schema_params() -> BTreeMap<String, BTreeSet<String>> {
        let schema = serde_json::to_value(schema_for!(GitlabAction))
            .unwrap_or_else(|error| panic!("schema did not serialize: {error}"));
        let variants = schema
            .get("oneOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("schema has no oneOf variants: {schema}"));
        let mut actions = BTreeMap::new();
        for variant in variants {
            let properties = variant
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("variant without properties: {variant}"));
            let action = properties
                .get("action")
                .and_then(|action| action.get("const"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("variant without action const: {variant}"));
            let fields = properties
                .keys()
                .filter(|key| key.as_str() != "action")
                .cloned()
                .collect();
            actions.insert(action.to_string(), fields);
        }
        actions
    }

    #[test]
    fn table_matches_generated_schema() {
        let schema = schema_params();
        let table: BTreeMap<String, BTreeSet<String>> = ACTION_PARAMS
            .iter()
            .map(|(action, params)| {
                (
                    (*action).to_string(),
                    params.iter().map(|param| (*param).to_string()).collect(),
                )
            })
            .collect();
        assert_eq!(
            table.keys().collect::<Vec<_>>(),
            schema.keys().collect::<Vec<_>>(),
            "ACTION_PARAMS and GitlabAction disagree on the action set"
        );
        for (action, schema_fields) in &schema {
            let table_fields = table
                .get(action)
                .unwrap_or_else(|| panic!("{action} missing from ACTION_PARAMS"));
            assert_eq!(
                table_fields, schema_fields,
                "ACTION_PARAMS drifted from the schema for {action}"
            );
        }
    }

    #[test]
    fn validator_never_rejects_a_schema_field() {
        for (action, params) in ACTION_PARAMS {
            let mut object = serde_json::Map::new();
            object.insert("action".to_string(), json!(action));
            for param in params.iter() {
                object.insert((*param).to_string(), Value::Null);
            }
            let params = Value::Object(object).to_string();
            validate(&params)
                .unwrap_or_else(|error| panic!("{action} rejected its own field: {error}"));
        }
    }

    #[test]
    fn project_id_alias_is_accepted_where_project_is() {
        for (action, params) in ACTION_PARAMS {
            if !params.contains(&"project") {
                continue;
            }
            let params = json!({ "action": action, "project_id": "lunarwing/lunarwing2" });
            validate(&params.to_string())
                .unwrap_or_else(|error| panic!("{action} rejected project_id: {error}"));
            assert!(
                validate(&json!({ "action": action, "project": "x" }).to_string()).is_ok(),
                "{action} rejected project"
            );
        }
    }

    #[test]
    fn wrong_field_names_name_the_valid_set() {
        let error = validate(
            r#"{"action":"get_file_content","project":"g/p","path":"a.txt","branch":"feature"}"#,
        )
        .expect_err("branch must be rejected");
        assert_eq!(
            error,
            "branch not valid for get_file_content; use ref. Valid: [project, path, ref]"
        );

        let error = validate(r#"{"action":"list_projects","query":"lunarwing"}"#)
            .expect_err("query must be rejected");
        assert!(
            error.contains("query not valid for list_projects"),
            "{error}"
        );
        assert!(error.contains("use search"), "{error}");
        assert!(
            error.contains("Valid: [per_page, page, search, membership]"),
            "{error}"
        );
    }

    #[test]
    fn near_miss_names_get_a_did_you_mean() {
        let error = validate(r#"{"action":"get_file_content","project":"g/p","pth":"a.txt"}"#)
            .expect_err("pth must be rejected");
        assert!(error.contains("did you mean path?"), "{error}");

        let error = validate(r#"{"action":"get_work_item","project":"g/p","iId":3}"#)
            .expect_err("iId must be rejected");
        assert!(error.contains("did you mean iid?"), "{error}");

        let error = validate(r#"{"action":"current_usre"}"#).expect_err("typo action");
        assert!(error.contains("did you mean current_user?"), "{error}");
    }

    #[test]
    fn all_pages_rejection_explains_paging() {
        let error =
            validate(r#"{"action":"list_merge_requests","project":"g/p","all_pages":true}"#)
                .expect_err("all_pages must be rejected here");
        assert!(
            error.contains("this action returns a single page; use page/per_page"),
            "{error}"
        );

        // The two actions that do support it keep accepting it.
        for action in ["list_issues", "list_work_items"] {
            let params = json!({ "action": action, "project": "g/p", "all_pages": true });
            assert!(
                validate(&params.to_string()).is_ok(),
                "{action} rejected all_pages"
            );
        }
    }

    #[test]
    fn body_alias_stays_scoped_to_issue_writes() {
        for action in ["create_issue", "update_issue"] {
            let params = json!({ "action": action, "project": "g/p", "title": "t", "body": "x" });
            validate(&params.to_string())
                .unwrap_or_else(|error| panic!("{action} rejected body: {error}"));
        }
        let error = validate(
            r#"{"action":"create_merge_request","project":"g/p","source_branch":"a","target_branch":"b","title":"t","body":"x"}"#,
        )
        .expect_err("body must not alias the MR description");
        assert!(
            error.contains("body not valid for create_merge_request"),
            "{error}"
        );
        assert!(error.contains("use description"), "{error}");
    }

    #[test]
    fn every_invalid_field_is_reported_at_once() {
        let error =
            validate(r#"{"action":"get_issue","project":"g/p","iid":1,"branch":"a","q":"b"}"#)
                .expect_err("two bad fields must be rejected");
        assert!(error.contains("branch not valid for get_issue"), "{error}");
        assert!(error.contains("q not valid for get_issue"), "{error}");
    }

    #[test]
    fn missing_or_unknown_action_is_actionable() {
        let error = validate(r#"{"project":"g/p"}"#).expect_err("action is required");
        assert!(error.starts_with("action is required"), "{error}");
        assert!(error.contains("get_file_content"), "{error}");

        let error = validate(r#"{"action":"open_an_issue"}"#).expect_err("unknown action");
        assert!(
            error.contains("unknown action \"open_an_issue\""),
            "{error}"
        );
        assert!(error.contains("Valid actions: [current_user"), "{error}");

        let error = validate(r#"["not","an","object"]"#).expect_err("non-object params");
        assert!(error.contains("JSON object"), "{error}");
    }

    #[test]
    fn validator_accepts_a_realistic_call_per_action() {
        // Name-level acceptance is the whole contract: values are serde's job.
        for (action, params) in ACTION_PARAMS {
            assert!(valid_params(action).is_some(), "{action} lookup failed");
            let object: serde_json::Map<String, Value> =
                std::iter::once(("action".to_string(), json!(action)))
                    .chain(
                        params
                            .iter()
                            .map(|param| ((*param).to_string(), json!("x"))),
                    )
                    .collect();
            validate(&Value::Object(object).to_string())
                .unwrap_or_else(|error| panic!("{action} rejected a full call: {error}"));
        }
    }

    /// NX-5: the rejection message is bounded and safe — field names are
    /// caller-controlled bytes echoed into the model context.
    #[test]
    fn rejection_message_is_bounded_escaped_and_prints_valid_once() {
        // A hundred short junk keys stay a bounded message.
        let mut object = serde_json::Map::new();
        object.insert("action".to_string(), json!("create_or_update_file"));
        for index in 0..100 {
            object.insert(format!("junk{index:03}"), Value::Null);
        }
        let error =
            validate(&Value::Object(object).to_string()).expect_err("junk keys must be rejected");
        assert!(
            error.len() < 4_096,
            "rejection message amplified to {} bytes: {error}",
            error.len()
        );
        assert!(error.contains("junk000 not valid"), "{error}");
        assert!(error.contains("(+90 more)"), "{error}");
        // The Valid list is printed exactly once, not per name.
        assert_eq!(error.matches("Valid: [").count(), 1, "{error}");

        // Control characters in a name are escaped, never echoed raw.
        let error = validate(r#"{"action":"get_issue","project":"g/p","iid":1,"a\u001b[31mb":1}"#)
            .expect_err("control-byte name must be rejected");
        assert!(error.contains("a\\u{1b}"), "{error}");
        assert!(!error.contains("a\u{1b}"), "{error}");

        // An over-long name is head-truncated with the dropped count and
        // skips the did-you-mean entirely.
        let mut long_call = serde_json::Map::new();
        long_call.insert("action".to_string(), json!("get_issue"));
        long_call.insert("project".to_string(), json!("g/p"));
        long_call.insert("iid".to_string(), json!(1));
        long_call.insert("x".repeat(300), json!(1));
        let error = validate(&Value::Object(long_call).to_string())
            .expect_err("the long junk name must be rejected");
        assert!(error.contains("…(+236 chars)"), "{error}");
        assert!(
            !error.contains(&"x".repeat(MAX_ECHOED_NAME_CHARS + 1)),
            "the echoed name was not truncated: {error}"
        );
        assert!(!error.contains("did you mean"), "{error}");
    }

    /// NX-8: actions that do not page at all get the normal rejection, not
    /// the paging explanation.
    #[test]
    fn all_pages_hint_only_on_paging_actions() {
        for input in [
            r#"{"action":"get_issue","project":"g/p","iid":1,"all_pages":true}"#,
            r#"{"action":"create_issue","project":"g/p","title":"t","all_pages":true}"#,
            r#"{"action":"get_file_content","project":"g/p","path":"a.txt","all_pages":true}"#,
        ] {
            let error = validate(input).expect_err("all_pages must stay rejected");
            assert!(
                error.contains("all_pages not valid for"),
                "{input}: {error}"
            );
            assert!(
                !error.contains("single page"),
                "a page-less action must not hear the paging hint: {input}: {error}"
            );
        }
    }

    /// NX-6: `project_id` aliases `project` only where `project` exists —
    /// a project-less action must reject it.
    #[test]
    fn project_id_alias_is_rejected_on_project_less_actions() {
        let error = validate(r#"{"action":"search_projects","query":"x","project_id":35}"#)
            .expect_err("project_id must be rejected on a project-less action");
        assert!(
            error.contains("project_id not valid for search_projects"),
            "{error}"
        );
        assert!(error.contains("Valid: [query, per_page, page]"), "{error}");
    }

    /// NX-6: the suggestion budget tightens for short candidates — a
    /// two-character slip from a four-or-fewer-character field is noise,
    /// not a hint.
    #[test]
    fn short_candidates_keep_the_tight_suggestion_budget() {
        let error = validate(r#"{"action":"get_issue","project":"g/p","iid":1,"ixe":2}"#)
            .expect_err("ixe must be rejected");
        assert!(!error.contains("did you mean"), "{error}");
    }
}
