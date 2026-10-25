//! LunarWing WASM tool for self-hosted GitLab.
//!
//! API credentials are injected by LunarWing at HTTP boundary. Guest code can
//! check token presence but never read token value.

mod api;
mod gitlab;
mod params;
mod types;

use types::GitlabAction;

wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../../wit/tool.wit",
});

struct GitlabTool;

impl exports::lunarwing::agent::tool::Guest for GitlabTool {
    fn execute(
        request: exports::lunarwing::agent::tool::Request,
    ) -> exports::lunarwing::agent::tool::Response {
        match execute_inner(&request.params) {
            Ok(output) => exports::lunarwing::agent::tool::Response {
                output: Some(output),
                error: None,
            },
            Err(error) => exports::lunarwing::agent::tool::Response {
                output: None,
                error: Some(error),
            },
        }
    }

    fn schema() -> String {
        serde_json::to_string(&schemars::schema_for!(GitlabAction))
            .unwrap_or_else(|_| r#"{"type":"object"}"#.to_string())
    }

    fn description() -> String {
        "Manage a self-hosted GitLab instance through REST API v4. Supports projects, \
         classic issues and comments, unified Work Items (Issues, Tasks, and hierarchy), \
         merge requests and approvals, branches, repository files, search, pipelines, \
         jobs, and current-user lookup. Use project numeric ID or group/project path; \
         use project-local iid values. Token is injected by LunarWing and never exposed \
         to WASM. List responses carry explicit pagination metadata. Use list_issues \
         with all_pages=true for classic issues, or list_work_items with all_pages=true \
         for a bounded, single-approval inventory across Issue and Task types. \
         search_blobs takes optional ref (branch, tag, or commit SHA) and searches \
         only the project's default branch when it is omitted, so branch-only files \
         need ref. Bad parameter names are rejected with the action's valid set. \
         Calls require approval unless you choose to always allow this tool."
            .to_string()
    }
}

fn execute_inner(params: &str) -> Result<String, String> {
    // One approval covers parameter errors as well as HTTP calls, so a
    // rejection must teach the caller how to retry in a single message.
    // Field names are checked before serde: its strict-schema rejection
    // names only the offending field, not the names that would work.
    params::validate(params).map_err(|error| {
        lunarwing::agent::host::log(
            lunarwing::agent::host::LogLevel::Warn,
            "GitLab tool parameter validation failed",
        );
        format!("Invalid GitLab parameters: {error}")
    })?;
    let action = parse_action(params).map_err(|error| {
        lunarwing::agent::host::log(
            lunarwing::agent::host::LogLevel::Warn,
            "GitLab tool parameter validation failed",
        );
        format!(
            "Invalid GitLab parameters: {error}. Expected {{\"action\":\"<name>\",...}}; call tool_info for schema."
        )
    })?;
    lunarwing::agent::host::log(
        lunarwing::agent::host::LogLevel::Info,
        &format!("GitLab action dispatched: {}", action_name(&action)),
    );
    let client = gitlab::GitlabClient::load()?;

    let result = match action {
        GitlabAction::CurrentUser {} => api::current_user(&client)?,
        GitlabAction::ListProjects {
            per_page,
            page,
            search,
            membership,
        } => api::list_projects(&client, per_page, page, search.as_deref(), membership)?,
        GitlabAction::GetProject { project } => api::get_project(&client, &project)?,
        GitlabAction::CreateProject {
            name,
            path,
            description,
            visibility,
            initialize_with_readme,
        } => api::create_project(
            &client,
            &name,
            path.as_deref(),
            description.as_deref(),
            &visibility,
            initialize_with_readme,
        )?,
        GitlabAction::ListIssues {
            project,
            state,
            per_page,
            page,
            all_pages,
        } => api::list_issues(&client, &project, &state, per_page, page, all_pages)?,
        GitlabAction::GetIssue { project, iid } => api::get_issue(&client, &project, iid)?,
        GitlabAction::GetWorkItem { project, iid } => api::get_work_item(&client, &project, iid)?,
        GitlabAction::ListWorkItems {
            project,
            state,
            per_page,
            cursor,
            all_pages,
        } => api::list_work_items(
            &client,
            &project,
            &state,
            per_page,
            cursor.as_deref(),
            all_pages,
        )?,
        GitlabAction::CreateIssue {
            project,
            title,
            description,
            labels,
            assignee_ids,
        } => api::create_issue(
            &client,
            &project,
            &title,
            description.as_deref(),
            &labels,
            &assignee_ids,
        )?,
        GitlabAction::UpdateIssue {
            project,
            iid,
            title,
            description,
            state_event,
            labels,
        } => api::update_issue(
            &client,
            &project,
            iid,
            title.as_deref(),
            description.as_deref(),
            state_event.as_deref(),
            labels.as_deref(),
        )?,
        GitlabAction::ListIssueNotes {
            project,
            iid,
            per_page,
            page,
        } => api::list_issue_notes(&client, &project, iid, per_page, page)?,
        GitlabAction::CreateIssueNote { project, iid, body } => {
            api::create_issue_note(&client, &project, iid, &body)?
        }
        GitlabAction::ListMergeRequests {
            project,
            state,
            per_page,
            page,
        } => api::list_merge_requests(&client, &project, &state, per_page, page)?,
        GitlabAction::GetMergeRequest { project, iid } => {
            api::get_merge_request(&client, &project, iid)?
        }
        GitlabAction::CreateMergeRequest {
            project,
            source_branch,
            target_branch,
            title,
            description,
            remove_source_branch,
            squash,
        } => api::create_merge_request(
            &client,
            &project,
            api::CreateMergeRequest {
                source_branch: &source_branch,
                target_branch: &target_branch,
                title: &title,
                description: description.as_deref(),
                remove_source_branch,
                squash,
            },
        )?,
        GitlabAction::UpdateMergeRequest {
            project,
            iid,
            title,
            description,
            state_event,
            target_branch,
        } => api::update_merge_request(
            &client,
            &project,
            iid,
            title.as_deref(),
            description.as_deref(),
            state_event.as_deref(),
            target_branch.as_deref(),
        )?,
        GitlabAction::ListMrNotes {
            project,
            iid,
            per_page,
            page,
        } => api::list_mr_notes(&client, &project, iid, per_page, page)?,
        GitlabAction::CreateMrNote { project, iid, body } => {
            api::create_mr_note(&client, &project, iid, &body)?
        }
        GitlabAction::ApproveMergeRequest { project, iid, sha } => {
            api::approve_merge_request(&client, &project, iid, &sha)?
        }
        GitlabAction::MergeMergeRequest {
            project,
            iid,
            sha,
            merge_commit_message,
            squash,
            should_remove_source_branch,
        } => api::merge_merge_request(
            &client,
            &project,
            iid,
            &sha,
            merge_commit_message.as_deref(),
            squash,
            should_remove_source_branch,
        )?,
        GitlabAction::ListBranches {
            project,
            search,
            per_page,
            page,
        } => api::list_branches(&client, &project, search.as_deref(), per_page, page)?,
        GitlabAction::GetBranch { project, branch } => api::get_branch(&client, &project, &branch)?,
        GitlabAction::CreateBranch {
            project,
            branch,
            ref_,
        } => api::create_branch(&client, &project, &branch, &ref_)?,
        GitlabAction::DeleteBranch { project, branch } => {
            api::delete_branch(&client, &project, &branch)?
        }
        GitlabAction::GetFileContent {
            project,
            path,
            ref_,
        } => api::get_file_content(&client, &project, &path, &ref_)?,
        GitlabAction::CreateOrUpdateFile {
            project,
            path,
            branch,
            content,
            commit_message,
            encoding,
            author_email,
            author_name,
            last_commit_id,
        } => api::create_or_update_file(
            &client,
            &project,
            &path,
            api::FileUpdate {
                branch: &branch,
                content: &content,
                commit_message: &commit_message,
                encoding: encoding.as_deref(),
                author_email: author_email.as_deref(),
                author_name: author_name.as_deref(),
                last_commit_id: last_commit_id.as_deref(),
            },
        )?,
        GitlabAction::DeleteFile {
            project,
            path,
            branch,
            commit_message,
            last_commit_id,
        } => api::delete_file(
            &client,
            &project,
            &path,
            &branch,
            &commit_message,
            &last_commit_id,
        )?,
        GitlabAction::SearchProjects {
            query,
            per_page,
            page,
        } => api::search_projects(&client, &query, per_page, page)?,
        GitlabAction::SearchIssues {
            project,
            query,
            per_page,
            page,
        } => api::search_issues(&client, &project, &query, per_page, page)?,
        GitlabAction::SearchBlobs {
            project,
            query,
            ref_,
            per_page,
            page,
        } => api::search_blobs(&client, &project, &query, ref_.as_deref(), per_page, page)?,
        GitlabAction::ListPipelines {
            project,
            status,
            ref_,
            per_page,
            page,
        } => api::list_pipelines(
            &client,
            &project,
            status.as_deref(),
            ref_.as_deref(),
            per_page,
            page,
        )?,
        GitlabAction::GetPipeline {
            project,
            pipeline_id,
        } => api::get_pipeline(&client, &project, pipeline_id)?,
        GitlabAction::ListJobs {
            project,
            pipeline_id,
            per_page,
            page,
        } => api::list_jobs(&client, &project, pipeline_id, per_page, page)?,
    };

    serde_json::to_string(&result).map_err(|error| format!("Failed to encode result: {error}"))
}

fn parse_action(params: &str) -> Result<GitlabAction, String> {
    let normalized = resolve_description_alias(params)?;
    let mut unknown = Vec::new();
    let mut deserializer = serde_json::Deserializer::from_str(&normalized);
    let action = serde_ignored::deserialize(&mut deserializer, |path| {
        unknown.push(path.to_string());
    })
    .map_err(|error| error.to_string())?;
    if unknown.is_empty() {
        Ok(action)
    } else {
        Err(format!("unknown field(s): {}", unknown.join(", ")))
    }
}

/// `body` is an accepted alias for the `description` field of issue write
/// actions (WI#266: callers habitually reuse the note-action parameter name).
/// Serde counts the alias and the canonical spelling as the same field, so
/// both appearing — even as a null placeholder — would fail deep inside
/// deserialization with an opaque "duplicate field" error. Normalize the raw
/// params instead: both spelled with a value is a caller error, a null twin
/// is dropped, and everything else passes through untouched.
fn resolve_description_alias(params: &str) -> Result<String, String> {
    let mut value: serde_json::Value = serde_json::from_str(params).map_err(|e| e.to_string())?;
    let is_issue_write = matches!(
        value.get("action").and_then(serde_json::Value::as_str),
        Some("create_issue") | Some("update_issue")
    );
    if !is_issue_write {
        return Ok(params.to_string());
    }
    let body_present = value.get("body").is_some_and(|v| !v.is_null());
    let description_present = value.get("description").is_some_and(|v| !v.is_null());
    if body_present && description_present {
        return Err(
            "body and description are the same field; provide only one of them".to_string(),
        );
    }
    let redundant_null = if body_present {
        "description"
    } else if description_present {
        "body"
    } else {
        return Ok(params.to_string());
    };
    if let Some(map) = value.as_object_mut() {
        map.remove(redundant_null);
    }
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

fn action_name(action: &GitlabAction) -> &'static str {
    match action {
        GitlabAction::CurrentUser {} => "current_user",
        GitlabAction::ListProjects { .. } => "list_projects",
        GitlabAction::GetProject { .. } => "get_project",
        GitlabAction::CreateProject { .. } => "create_project",
        GitlabAction::ListIssues { .. } => "list_issues",
        GitlabAction::GetIssue { .. } => "get_issue",
        GitlabAction::GetWorkItem { .. } => "get_work_item",
        GitlabAction::ListWorkItems { .. } => "list_work_items",
        GitlabAction::CreateIssue { .. } => "create_issue",
        GitlabAction::UpdateIssue { .. } => "update_issue",
        GitlabAction::ListIssueNotes { .. } => "list_issue_notes",
        GitlabAction::CreateIssueNote { .. } => "create_issue_note",
        GitlabAction::ListMergeRequests { .. } => "list_merge_requests",
        GitlabAction::GetMergeRequest { .. } => "get_merge_request",
        GitlabAction::CreateMergeRequest { .. } => "create_merge_request",
        GitlabAction::UpdateMergeRequest { .. } => "update_merge_request",
        GitlabAction::ListMrNotes { .. } => "list_mr_notes",
        GitlabAction::CreateMrNote { .. } => "create_mr_note",
        GitlabAction::ApproveMergeRequest { .. } => "approve_merge_request",
        GitlabAction::MergeMergeRequest { .. } => "merge_merge_request",
        GitlabAction::ListBranches { .. } => "list_branches",
        GitlabAction::GetBranch { .. } => "get_branch",
        GitlabAction::CreateBranch { .. } => "create_branch",
        GitlabAction::DeleteBranch { .. } => "delete_branch",
        GitlabAction::GetFileContent { .. } => "get_file_content",
        GitlabAction::CreateOrUpdateFile { .. } => "create_or_update_file",
        GitlabAction::DeleteFile { .. } => "delete_file",
        GitlabAction::SearchProjects { .. } => "search_projects",
        GitlabAction::SearchIssues { .. } => "search_issues",
        GitlabAction::SearchBlobs { .. } => "search_blobs",
        GitlabAction::ListPipelines { .. } => "list_pipelines",
        GitlabAction::GetPipeline { .. } => "get_pipeline",
        GitlabAction::ListJobs { .. } => "list_jobs",
    }
}

export!(GitlabTool);

#[cfg(test)]
mod tests {
    use schemars::schema_for;

    use super::parse_action;
    use super::types::GitlabAction;

    #[test]
    fn generated_schema_lists_action_discriminator() {
        let schema = serde_json::to_value(schema_for!(GitlabAction)).unwrap();
        let rendered = schema.to_string();
        assert!(rendered.contains("current_user"));
        assert!(rendered.contains("create_or_update_file"));
        assert!(rendered.contains("merge_merge_request"));
    }

    #[test]
    fn action_parser_rejects_unknown_fields() {
        let input = r#"{"action":"current_user","token":"must-not-be-accepted"}"#;
        assert!(parse_action(input).is_err());
    }

    #[test]
    fn search_blobs_accepts_optional_ref() {
        let action = parse_action(
            r#"{"action":"search_blobs","project":"group/repo","query":"TODO","ref":"feature/wi-315"}"#,
        )
        .unwrap_or_else(|error| panic!("ref must be accepted: {error}"));
        let GitlabAction::SearchBlobs {
            project,
            query,
            ref_,
            ..
        } = action
        else {
            panic!("wrong variant");
        };
        assert_eq!(project, "group/repo");
        assert_eq!(query, "TODO");
        assert_eq!(ref_.as_deref(), Some("feature/wi-315"));

        let action = parse_action(r#"{"action":"search_blobs","project":35,"query":"TODO"}"#)
            .unwrap_or_else(|error| panic!("default branch call must stay valid: {error}"));
        let GitlabAction::SearchBlobs { ref_, .. } = action else {
            panic!("wrong variant");
        };
        assert!(ref_.is_none());
    }

    #[test]
    fn project_accepts_numeric_id_string_id_and_project_id_alias() {
        for input in [
            r#"{"action":"list_issues","project":35}"#,
            r#"{"action":"list_issues","project":"35"}"#,
            r#"{"action":"list_issues","project":"group/repo"}"#,
            r#"{"action":"list_issues","project_id":35}"#,
            r#"{"action":"list_issues","project_id":"group/repo"}"#,
        ] {
            let action = parse_action(input).unwrap_or_else(|e| panic!("rejected {input}: {e}"));
            let GitlabAction::ListIssues { project, .. } = action else {
                panic!("wrong variant for {input}");
            };
            assert!(!project.is_empty());
        }

        // Negative IDs are not valid project references.
        assert!(parse_action(r#"{"action":"list_issues","project":-1}"#).is_err());
    }

    #[test]
    fn list_issues_accepts_all_pages_flag() {
        let action =
            parse_action(r#"{"action":"list_issues","project":35,"state":"all","all_pages":true}"#)
                .unwrap_or_else(|e| panic!("rejected all_pages: {e}"));
        let GitlabAction::ListIssues {
            all_pages, state, ..
        } = action
        else {
            panic!("wrong variant");
        };
        assert!(all_pages);
        assert_eq!(state, "all");

        // Default is single-page behavior.
        let action = parse_action(r#"{"action":"list_issues","project":35}"#).unwrap();
        let GitlabAction::ListIssues { all_pages, .. } = action else {
            panic!("wrong variant");
        };
        assert!(!all_pages);
    }

    #[test]
    fn work_items_actions_accept_tasks_and_keyset_cursor() {
        let action = parse_action(
            r#"{"action":"list_work_items","project":35,"state":"all","per_page":100,"cursor":"opaque/cursor","all_pages":true}"#,
        )
        .unwrap();
        let GitlabAction::ListWorkItems {
            project,
            state,
            per_page,
            cursor,
            all_pages,
        } = action
        else {
            panic!("wrong variant");
        };
        assert_eq!(project, "35");
        assert_eq!(state, "all");
        assert_eq!(per_page, 100);
        assert_eq!(cursor.as_deref(), Some("opaque/cursor"));
        assert!(all_pages);

        let action =
            parse_action(r#"{"action":"get_work_item","project":"group/repo","iid":45}"#).unwrap();
        assert!(matches!(
            action,
            GitlabAction::GetWorkItem { project, iid }
                if project == "group/repo" && iid == 45
        ));
    }

    #[test]
    fn destructive_actions_require_concurrency_guards() {
        for input in [
            r#"{"action":"approve_merge_request","project":"group/repo","iid":7}"#,
            r#"{"action":"merge_merge_request","project":"group/repo","iid":7}"#,
            r#"{"action":"delete_file","project":"group/repo","path":"a.txt","branch":"main","commit_message":"delete"}"#,
        ] {
            assert!(parse_action(input).is_err(), "accepted {input}");
        }
    }

    /// WI#266: `body` and `description` name the same GitLab issue field.
    /// Either spelling populates the description; both at once is rejected.
    #[test]
    fn issue_description_accepts_body_alias() {
        let via_description = parse_action(
            r#"{"action":"create_issue","project":"group/repo","title":"t","description":"from description"}"#,
        )
        .unwrap();
        let via_body = parse_action(
            r#"{"action":"create_issue","project":"group/repo","title":"t","body":"from body"}"#,
        )
        .unwrap();
        for action in [via_description, via_body] {
            let GitlabAction::CreateIssue { description, .. } = action else {
                panic!("wrong variant");
            };
            assert!(description.is_some());
        }

        let updated = parse_action(
            r#"{"action":"update_issue","project":"group/repo","iid":7,"body":"replaced"}"#,
        )
        .unwrap();
        let GitlabAction::UpdateIssue { description, .. } = updated else {
            panic!("wrong variant for update_issue");
        };
        assert_eq!(description.as_deref(), Some("replaced"));

        for conflicting in [
            r#"{"action":"create_issue","project":"group/repo","title":"t","body":"a","description":"b"}"#,
            r#"{"action":"update_issue","project":"group/repo","iid":7,"body":"a","description":"b"}"#,
        ] {
            let error = parse_action(conflicting)
                .err()
                .unwrap_or_else(|| panic!("accepted {conflicting}"));
            assert!(
                error.contains("body and description"),
                "unexpected error: {error}"
            );
        }

        // Explicit nulls do not conflict with a real value.
        let null_body = parse_action(
            r#"{"action":"create_issue","project":"group/repo","title":"t","body":null,"description":"kept"}"#,
        )
        .unwrap();
        let GitlabAction::CreateIssue { description, .. } = null_body else {
            panic!("wrong variant for null body");
        };
        assert_eq!(description.as_deref(), Some("kept"));
    }

    /// `body` stays a note-only field everywhere else: unrelated actions
    /// must not quietly start accepting it through the alias.
    #[test]
    fn body_alias_is_scoped_to_issue_write_actions() {
        assert!(parse_action(
            r#"{"action":"create_merge_request","project":"group/repo","source_branch":"a","target_branch":"b","title":"t","body":"not an issue"}"#
        )
        .is_err());
        assert!(
            parse_action(r#"{"action":"get_issue","project":"group/repo","iid":7,"body":"x"}"#)
                .is_err()
        );
        // Note actions keep `body` as their real, required field name.
        let note = parse_action(
            r#"{"action":"create_issue_note","project":"group/repo","iid":7,"body":"note text"}"#,
        )
        .unwrap();
        assert!(matches!(
            note,
            GitlabAction::CreateIssueNote { body, .. } if body == "note text"
        ));
    }
}
