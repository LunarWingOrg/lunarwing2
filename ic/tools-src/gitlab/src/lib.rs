//! LunarWing WASM tool for self-hosted GitLab.
//!
//! API credentials are injected by LunarWing at HTTP boundary. Guest code can
//! check token presence but never read token value.

mod api;
mod gitlab;
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
         issues and comments, merge requests and approvals, branches, repository files, \
         search, pipelines, jobs, and current-user lookup. Use project numeric ID or \
         group/project path; use per-project iid for issues and merge requests. Token is \
         injected by LunarWing and never exposed to WASM. Calls require approval unless \
         you choose to always allow this tool."
            .to_string()
    }
}

fn execute_inner(params: &str) -> Result<String, String> {
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
        } => api::list_issues(&client, &project, &state, per_page, page)?,
        GitlabAction::GetIssue { project, iid } => api::get_issue(&client, &project, iid)?,
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
            per_page,
            page,
        } => api::search_blobs(&client, &project, &query, per_page, page)?,
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
    let mut unknown = Vec::new();
    let mut deserializer = serde_json::Deserializer::from_str(params);
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

fn action_name(action: &GitlabAction) -> &'static str {
    match action {
        GitlabAction::CurrentUser {} => "current_user",
        GitlabAction::ListProjects { .. } => "list_projects",
        GitlabAction::GetProject { .. } => "get_project",
        GitlabAction::CreateProject { .. } => "create_project",
        GitlabAction::ListIssues { .. } => "list_issues",
        GitlabAction::GetIssue { .. } => "get_issue",
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
    fn destructive_actions_require_concurrency_guards() {
        for input in [
            r#"{"action":"approve_merge_request","project":"group/repo","iid":7}"#,
            r#"{"action":"merge_merge_request","project":"group/repo","iid":7}"#,
            r#"{"action":"delete_file","project":"group/repo","path":"a.txt","branch":"main","commit_message":"delete"}"#,
        ] {
            assert!(parse_action(input).is_err(), "accepted {input}");
        }
    }
}
