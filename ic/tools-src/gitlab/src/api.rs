use serde_json::{Map, Value, json};

use crate::gitlab::{GitlabClient, page, project_path, require_nonempty, url_encode};

pub fn current_user(client: &GitlabClient) -> Result<Value, String> {
    client.request_json("GET", "/user", None)
}

pub fn list_projects(
    client: &GitlabClient,
    per_page: u32,
    page_number: u32,
    search: Option<&str>,
    membership: bool,
) -> Result<Value, String> {
    let (per_page, page_number) = page(per_page, page_number);
    let mut endpoint =
        format!("/projects?per_page={per_page}&page={page_number}&membership={membership}");
    if let Some(search) = nonempty_optional(search) {
        endpoint.push_str("&search=");
        endpoint.push_str(&url_encode(search));
    }
    client.request_json("GET", &endpoint, None)
}

pub fn get_project(client: &GitlabClient, project: &str) -> Result<Value, String> {
    client.request_json(
        "GET",
        &format!("/projects/{}", project_path(project)?),
        None,
    )
}

pub fn create_project(
    client: &GitlabClient,
    name: &str,
    path: Option<&str>,
    description: Option<&str>,
    visibility: &str,
    initialize_with_readme: bool,
) -> Result<Value, String> {
    require_nonempty("name", name)?;
    validate_choice("visibility", visibility, &["private", "internal", "public"])?;
    let mut payload = json!({
        "name": name,
        "visibility": visibility,
        "initialize_with_readme": initialize_with_readme,
    });
    insert_optional(&mut payload, "path", path);
    insert_optional(&mut payload, "description", description);
    client.request_json("POST", "/projects", Some(&payload))
}

pub fn list_issues(
    client: &GitlabClient,
    project: &str,
    state: &str,
    per_page: u32,
    page_number: u32,
    all_pages: bool,
) -> Result<Value, String> {
    validate_choice("state", state, &["opened", "closed", "all"])?;
    let project = project_path(project)?;
    if all_pages {
        // One tool call walks the whole list, so "list all work items" needs
        // a single approval instead of one approval per page.
        let endpoint = format!(
            "/projects/{project}/issues?state={}&per_page=100",
            url_encode(state)
        );
        return client.request_all_pages(&endpoint);
    }
    let (per_page, page_number) = page(per_page, page_number);
    let endpoint = format!(
        "/projects/{project}/issues?state={}&per_page={per_page}&page={page_number}",
        url_encode(state)
    );
    client.request_json("GET", &endpoint, None)
}

pub fn get_issue(client: &GitlabClient, project: &str, iid: u64) -> Result<Value, String> {
    let project = project_path(project)?;
    let endpoint = format!("/projects/{project}/issues/{iid}");
    match client.request_json_allow_404("GET", &endpoint, None)? {
        Some(issue) => Ok(issue),
        None => get_work_item_by_path(client, &project, iid),
    }
}

/// Retrieve any GitLab Work Item type. The collection `iids` filter is used
/// instead of the single-item REST endpoint because GitLab 19.2 may expose
/// the collection while feature-gating `/-/work_items/:iid` for API tokens.
pub fn get_work_item(client: &GitlabClient, project: &str, iid: u64) -> Result<Value, String> {
    get_work_item_by_path(client, &project_path(project)?, iid)
}

fn get_work_item_by_path(
    client: &GitlabClient,
    encoded_project: &str,
    iid: u64,
) -> Result<Value, String> {
    let endpoint = format!(
        "/projects/{encoded_project}/-/work_items?iids={iid}&per_page=2&fields={}&features={}",
        url_encode(
            "state,confidential,work_item_type,archived,created_at,updated_at,closed_at,author,web_url"
        ),
        url_encode("description,labels,assignees,hierarchy")
    );
    let value = client.request_json("GET", &endpoint, None)?;
    let mut items = match value {
        Value::Array(items) => items,
        Value::Object(mut object) => object
            .remove("items")
            .and_then(|items| items.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    match items.len() {
        1 => {
            let mut item = items.remove(0);
            if let Some(object) = item.as_object_mut() {
                object.insert("retrieved_via".into(), Value::String("work_items".into()));
            }
            Ok(item)
        }
        0 => Err(format!(
            "GitLab Work Item #{iid} was not found in project '{encoded_project}'"
        )),
        count => Err(format!(
            "GitLab Work Items API returned {count} records for IID {iid}; expected exactly one"
        )),
    }
}

pub fn list_work_items(
    client: &GitlabClient,
    project: &str,
    state: &str,
    per_page: u32,
    cursor: Option<&str>,
    all_pages: bool,
) -> Result<Value, String> {
    let endpoint = work_items_endpoint(project, state, per_page, cursor, all_pages)?;
    if all_pages {
        client.request_all_keyset_pages(&endpoint)
    } else {
        client.request_keyset_page(&endpoint)
    }
}

fn work_items_endpoint(
    project: &str,
    state: &str,
    per_page: u32,
    cursor: Option<&str>,
    all_pages: bool,
) -> Result<String, String> {
    validate_choice("state", state, &["opened", "closed", "all"])?;
    let (requested_per_page, _) = page(per_page, 1);
    let per_page = if all_pages { 100 } else { requested_per_page };
    let mut endpoint = format!(
        "/projects/{}/-/work_items?per_page={per_page}&fields={}",
        project_path(project)?,
        url_encode("state,confidential,work_item_type")
    );
    if state != "all" {
        endpoint.push_str("&state=");
        endpoint.push_str(&url_encode(state));
    }
    if let Some(raw_cursor) = cursor {
        if raw_cursor.len() > 2_048 || raw_cursor.chars().any(char::is_control) {
            return Err("cursor must be at most 2048 characters without control characters".into());
        }
        let cursor = raw_cursor.trim();
        if !cursor.is_empty() {
            endpoint.push_str("&cursor=");
            endpoint.push_str(&url_encode(cursor));
        }
    }
    Ok(endpoint)
}

pub fn create_issue(
    client: &GitlabClient,
    project: &str,
    title: &str,
    description: Option<&str>,
    labels: &[String],
    assignee_ids: &[u64],
) -> Result<Value, String> {
    require_nonempty("title", title)?;
    let mut payload = json!({ "title": title });
    insert_optional(&mut payload, "description", description);
    if !labels.is_empty() {
        payload["labels"] = Value::String(labels.join(","));
    }
    if !assignee_ids.is_empty() {
        payload["assignee_ids"] = assignee_ids.iter().copied().map(Value::from).collect();
    }
    let endpoint = format!("/projects/{}/issues", project_path(project)?);
    client.request_json("POST", &endpoint, Some(&payload))
}

pub fn update_issue(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    title: Option<&str>,
    description: Option<&str>,
    state_event: Option<&str>,
    labels: Option<&[String]>,
) -> Result<Value, String> {
    if let Some(event) = state_event {
        validate_choice("state_event", event, &["close", "reopen"])?;
    }
    let mut payload = Map::new();
    map_optional(&mut payload, "title", title);
    map_optional_allow_empty(&mut payload, "description", description);
    map_optional(&mut payload, "state_event", state_event);
    if let Some(labels) = labels {
        payload.insert("labels".into(), Value::String(labels.join(",")));
    }
    require_changes("update_issue", &payload)?;
    let endpoint = format!("/projects/{}/issues/{iid}", project_path(project)?);
    client.request_json("PUT", &endpoint, Some(&Value::Object(payload)))
}

pub fn list_issue_notes(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    let (per_page, page_number) = page(per_page, page_number);
    let endpoint = format!(
        "/projects/{}/issues/{iid}/notes?per_page={per_page}&page={page_number}",
        project_path(project)?
    );
    client.request_json("GET", &endpoint, None)
}

pub fn create_issue_note(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    body: &str,
) -> Result<Value, String> {
    require_nonempty("body", body)?;
    let endpoint = format!("/projects/{}/issues/{iid}/notes", project_path(project)?);
    client.request_json("POST", &endpoint, Some(&json!({ "body": body })))
}

pub fn list_merge_requests(
    client: &GitlabClient,
    project: &str,
    state: &str,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    validate_choice(
        "state",
        state,
        &["opened", "closed", "locked", "merged", "all"],
    )?;
    let (per_page, page_number) = page(per_page, page_number);
    let endpoint = format!(
        "/projects/{}/merge_requests?state={}&per_page={per_page}&page={page_number}",
        project_path(project)?,
        url_encode(state)
    );
    client.request_json("GET", &endpoint, None)
}

pub fn get_merge_request(client: &GitlabClient, project: &str, iid: u64) -> Result<Value, String> {
    let endpoint = format!("/projects/{}/merge_requests/{iid}", project_path(project)?);
    client.request_json("GET", &endpoint, None)
}

pub struct CreateMergeRequest<'a> {
    pub source_branch: &'a str,
    pub target_branch: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub remove_source_branch: bool,
    pub squash: bool,
}

pub fn create_merge_request(
    client: &GitlabClient,
    project: &str,
    request: CreateMergeRequest<'_>,
) -> Result<Value, String> {
    require_nonempty("source_branch", request.source_branch)?;
    require_nonempty("target_branch", request.target_branch)?;
    require_nonempty("title", request.title)?;
    let mut payload = json!({
        "source_branch": request.source_branch,
        "target_branch": request.target_branch,
        "title": request.title,
        "remove_source_branch": request.remove_source_branch,
        "squash": request.squash,
    });
    insert_optional(&mut payload, "description", request.description);
    let endpoint = format!("/projects/{}/merge_requests", project_path(project)?);
    client.request_json("POST", &endpoint, Some(&payload))
}

pub fn update_merge_request(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    title: Option<&str>,
    description: Option<&str>,
    state_event: Option<&str>,
    target_branch: Option<&str>,
) -> Result<Value, String> {
    if let Some(event) = state_event {
        validate_choice("state_event", event, &["close", "reopen"])?;
    }
    let mut payload = Map::new();
    map_optional(&mut payload, "title", title);
    map_optional_allow_empty(&mut payload, "description", description);
    map_optional(&mut payload, "state_event", state_event);
    map_optional(&mut payload, "target_branch", target_branch);
    require_changes("update_merge_request", &payload)?;
    let endpoint = format!("/projects/{}/merge_requests/{iid}", project_path(project)?);
    client.request_json("PUT", &endpoint, Some(&Value::Object(payload)))
}

pub fn list_mr_notes(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    let (per_page, page_number) = page(per_page, page_number);
    let endpoint = format!(
        "/projects/{}/merge_requests/{iid}/notes?per_page={per_page}&page={page_number}",
        project_path(project)?
    );
    client.request_json("GET", &endpoint, None)
}

pub fn create_mr_note(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    body: &str,
) -> Result<Value, String> {
    require_nonempty("body", body)?;
    let endpoint = format!(
        "/projects/{}/merge_requests/{iid}/notes",
        project_path(project)?
    );
    client.request_json("POST", &endpoint, Some(&json!({ "body": body })))
}

pub fn approve_merge_request(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    sha: &str,
) -> Result<Value, String> {
    require_nonempty("sha", sha)?;
    let endpoint = format!(
        "/projects/{}/merge_requests/{iid}/approve",
        project_path(project)?
    );
    client.request_json("POST", &endpoint, Some(&json!({ "sha": sha })))
}

pub fn merge_merge_request(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    sha: &str,
    merge_commit_message: Option<&str>,
    squash: bool,
    should_remove_source_branch: bool,
) -> Result<Value, String> {
    require_nonempty("sha", sha)?;
    let mut payload = json!({
        "sha": sha,
        "squash": squash,
        "should_remove_source_branch": should_remove_source_branch,
    });
    insert_optional(&mut payload, "merge_commit_message", merge_commit_message);
    let endpoint = format!(
        "/projects/{}/merge_requests/{iid}/merge",
        project_path(project)?
    );
    client.request_json("PUT", &endpoint, Some(&payload))
}

pub fn list_branches(
    client: &GitlabClient,
    project: &str,
    search: Option<&str>,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    let (per_page, page_number) = page(per_page, page_number);
    let mut endpoint = format!(
        "/projects/{}/repository/branches?per_page={per_page}&page={page_number}",
        project_path(project)?
    );
    if let Some(search) = nonempty_optional(search) {
        endpoint.push_str("&search=");
        endpoint.push_str(&url_encode(search));
    }
    client.request_json("GET", &endpoint, None)
}

pub fn get_branch(client: &GitlabClient, project: &str, branch: &str) -> Result<Value, String> {
    require_nonempty("branch", branch)?;
    let endpoint = format!(
        "/projects/{}/repository/branches/{}",
        project_path(project)?,
        url_encode(branch)
    );
    client.request_json("GET", &endpoint, None)
}

pub fn create_branch(
    client: &GitlabClient,
    project: &str,
    branch: &str,
    ref_: &str,
) -> Result<Value, String> {
    require_nonempty("branch", branch)?;
    require_nonempty("ref", ref_)?;
    let endpoint = format!(
        "/projects/{}/repository/branches?branch={}&ref={}",
        project_path(project)?,
        url_encode(branch),
        url_encode(ref_)
    );
    client.request_json("POST", &endpoint, Some(&json!({})))
}

pub fn delete_branch(client: &GitlabClient, project: &str, branch: &str) -> Result<Value, String> {
    require_nonempty("branch", branch)?;
    let endpoint = format!(
        "/projects/{}/repository/branches/{}",
        project_path(project)?,
        url_encode(branch)
    );
    client.request_json("DELETE", &endpoint, None)?;
    Ok(json!({ "deleted": branch }))
}

pub fn get_file_content(
    client: &GitlabClient,
    project: &str,
    path: &str,
    ref_: &str,
) -> Result<Value, String> {
    validate_file_path(path)?;
    require_nonempty("ref", ref_)?;
    let endpoint = format!(
        "/projects/{}/repository/files/{}?ref={}",
        project_path(project)?,
        url_encode(path),
        url_encode(ref_)
    );
    client.request_json("GET", &endpoint, None)
}

pub struct FileUpdate<'a> {
    pub branch: &'a str,
    pub content: &'a str,
    pub commit_message: &'a str,
    pub encoding: Option<&'a str>,
    pub author_email: Option<&'a str>,
    pub author_name: Option<&'a str>,
    pub last_commit_id: Option<&'a str>,
}

pub fn create_or_update_file(
    client: &GitlabClient,
    project: &str,
    path: &str,
    update: FileUpdate<'_>,
) -> Result<Value, String> {
    validate_file_path(path)?;
    require_nonempty("branch", update.branch)?;
    require_nonempty("commit_message", update.commit_message)?;
    if let Some(last_commit_id) = update.last_commit_id {
        require_nonempty("last_commit_id", last_commit_id)?;
    }
    if let Some(encoding) = update.encoding {
        validate_choice("encoding", encoding, &["text", "base64"])?;
    }
    let file_endpoint = format!(
        "/projects/{}/repository/files/{}",
        project_path(project)?,
        url_encode(path)
    );
    let lookup = format!("{file_endpoint}?ref={}", url_encode(update.branch));
    let exists = client.resource_exists(&lookup)?;
    if exists && update.last_commit_id.is_none() {
        return Err(
            "last_commit_id is required when updating an existing file; fetch current file metadata first"
                .to_string(),
        );
    }
    let mut payload = json!({
        "branch": update.branch,
        "content": update.content,
        "commit_message": update.commit_message,
    });
    insert_optional(&mut payload, "encoding", update.encoding);
    insert_optional(&mut payload, "author_email", update.author_email);
    insert_optional(&mut payload, "author_name", update.author_name);
    insert_optional(&mut payload, "last_commit_id", update.last_commit_id);
    let method = if exists { "PUT" } else { "POST" };
    client.request_json(method, &file_endpoint, Some(&payload))
}

pub fn delete_file(
    client: &GitlabClient,
    project: &str,
    path: &str,
    branch: &str,
    commit_message: &str,
    last_commit_id: &str,
) -> Result<Value, String> {
    validate_file_path(path)?;
    require_nonempty("branch", branch)?;
    require_nonempty("commit_message", commit_message)?;
    require_nonempty("last_commit_id", last_commit_id)?;
    let endpoint = format!(
        "/projects/{}/repository/files/{}",
        project_path(project)?,
        url_encode(path)
    );
    let payload = json!({
        "branch": branch,
        "commit_message": commit_message,
        "last_commit_id": last_commit_id,
    });
    client.request_json("DELETE", &endpoint, Some(&payload))?;
    Ok(json!({ "deleted": path, "branch": branch }))
}

pub fn search_projects(
    client: &GitlabClient,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    require_nonempty("query", query)?;
    let (per_page, page_number) = page(per_page, page_number);
    let endpoint = format!(
        "/search?scope=projects&search={}&per_page={per_page}&page={page_number}",
        url_encode(query)
    );
    client.request_json("GET", &endpoint, None)
}

pub fn search_issues(
    client: &GitlabClient,
    project: &str,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    project_search(client, project, "issues", query, per_page, page_number)
}

/// GitLab's project search supports `ref` only for the repository scopes
/// (`blobs`, `commits`, `wiki_blobs`); `search_issues` has no branch filter.
/// A branch/tag/SHA is accepted rather than a branch name only: GitLab
/// passes `ref` straight through to the repository search backend.
pub fn search_blobs(
    client: &GitlabClient,
    project: &str,
    query: &str,
    ref_: Option<&str>,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    let endpoint = search_blobs_endpoint(project, query, ref_, per_page, page_number)?;
    client.request_json("GET", &endpoint, None)
}

fn search_blobs_endpoint(
    project: &str,
    query: &str,
    ref_: Option<&str>,
    per_page: u32,
    page_number: u32,
) -> Result<String, String> {
    require_nonempty("query", query)?;
    if let Some(ref_) = ref_ {
        require_nonempty("ref", ref_)?;
    }
    let (per_page, page_number) = page(per_page, page_number);
    let mut endpoint = project_search_endpoint(project, "blobs", query, per_page, page_number)?;
    if let Some(ref_) = ref_ {
        endpoint.push_str("&ref=");
        endpoint.push_str(&url_encode(ref_));
    }
    Ok(endpoint)
}

/// Search URL shared by every project search scope. Split out so the
/// `blobs` branch filter can be pinned without a live GitLab instance.
fn project_search_endpoint(
    project: &str,
    scope: &str,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<String, String> {
    Ok(format!(
        "/projects/{}/search?scope={scope}&search={}&per_page={per_page}&page={page_number}",
        project_path(project)?,
        url_encode(query)
    ))
}

fn project_search(
    client: &GitlabClient,
    project: &str,
    scope: &str,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    require_nonempty("query", query)?;
    let (per_page, page_number) = page(per_page, page_number);
    let endpoint = format!(
        "/projects/{}/search?scope={scope}&search={}&per_page={per_page}&page={page_number}",
        project_path(project)?,
        url_encode(query)
    );
    client.request_json("GET", &endpoint, None)
}

pub fn list_pipelines(
    client: &GitlabClient,
    project: &str,
    status: Option<&str>,
    ref_: Option<&str>,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    let (per_page, page_number) = page(per_page, page_number);
    let mut endpoint = format!(
        "/projects/{}/pipelines?per_page={per_page}&page={page_number}",
        project_path(project)?
    );
    if let Some(status) = nonempty_optional(status) {
        validate_choice(
            "status",
            status,
            &[
                "created",
                "waiting_for_resource",
                "preparing",
                "pending",
                "running",
                "success",
                "failed",
                "canceled",
                "skipped",
                "manual",
                "scheduled",
            ],
        )?;
        endpoint.push_str("&status=");
        endpoint.push_str(&url_encode(status));
    }
    if let Some(ref_) = nonempty_optional(ref_) {
        endpoint.push_str("&ref=");
        endpoint.push_str(&url_encode(ref_));
    }
    client.request_json("GET", &endpoint, None)
}

pub fn get_pipeline(
    client: &GitlabClient,
    project: &str,
    pipeline_id: u64,
) -> Result<Value, String> {
    let endpoint = format!(
        "/projects/{}/pipelines/{pipeline_id}",
        project_path(project)?
    );
    client.request_json("GET", &endpoint, None)
}

pub fn list_jobs(
    client: &GitlabClient,
    project: &str,
    pipeline_id: u64,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    let (per_page, page_number) = page(per_page, page_number);
    let endpoint = format!(
        "/projects/{}/pipelines/{pipeline_id}/jobs?per_page={per_page}&page={page_number}",
        project_path(project)?
    );
    client.request_json("GET", &endpoint, None)
}

fn validate_choice(field: &str, value: &str, allowed: &[&str]) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "Invalid {field}; expected one of {}",
            allowed.join(", ")
        ))
    }
}

fn validate_file_path(path: &str) -> Result<(), String> {
    require_nonempty("path", path)?;
    if path.starts_with('/') || path.chars().any(char::is_control) {
        return Err("path must be repository-relative and contain no control characters".into());
    }
    if path
        .split('/')
        .any(|component| component == "." || component == "..")
    {
        return Err("path must not contain dot segments".into());
    }
    Ok(())
}

fn require_changes(action: &str, payload: &Map<String, Value>) -> Result<(), String> {
    if payload.is_empty() {
        Err(format!("{action} requires at least one changed field"))
    } else {
        Ok(())
    }
}

fn insert_optional(payload: &mut Value, key: &str, value: Option<&str>) {
    if let Some(value) = nonempty_optional(value) {
        payload[key] = Value::String(value.to_string());
    }
}

fn map_optional(payload: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = nonempty_optional(value) {
        payload.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn map_optional_allow_empty(payload: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        payload.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn nonempty_optional(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::{
        project_search_endpoint, require_changes, search_blobs_endpoint, validate_choice,
        validate_file_path, work_items_endpoint,
    };

    #[test]
    fn validates_enums() {
        assert!(validate_choice("state", "opened", &["opened", "closed"]).is_ok());
        assert!(validate_choice("state", "destroy", &["opened", "closed"]).is_err());
    }

    #[test]
    fn rejects_unsafe_repository_paths() {
        for path in [
            "",
            "/etc/passwd",
            "../secret",
            "src/../secret",
            "src/./lib.rs",
        ] {
            assert!(validate_file_path(path).is_err(), "accepted {path}");
        }
        assert!(validate_file_path("src/lib.rs").is_ok());
    }

    #[test]
    fn work_items_endpoint_clamps_pages_and_bounds_cursors() {
        let single = work_items_endpoint("35", "opened", 500, Some("abc/123"), false).unwrap();
        assert!(single.contains("per_page=100"));
        assert!(single.contains("state=opened"));
        assert!(single.contains("cursor=abc%2F123"));

        let all = work_items_endpoint("group/project", "all", 1, None, true).unwrap();
        assert!(all.contains("/projects/group%2Fproject/-/work_items?per_page=100"));
        assert!(!all.contains("&state="));

        assert!(work_items_endpoint("35", "opened", 20, Some("\n"), false).is_err());
        assert!(work_items_endpoint("35", "opened", 20, Some(&"x".repeat(2_049)), false).is_err());
    }

    #[test]
    fn update_requires_a_change() {
        assert!(require_changes("update_issue", &Map::new()).is_err());
    }

    /// WI#315: the branch/tag filter must reach the request URL, and stay
    /// absent when the caller does not name a ref.
    #[test]
    fn search_blobs_ref_reaches_the_request_url() {
        let default_branch = search_blobs_endpoint("35", "search_blobs", None, 20, 1).unwrap();
        assert_eq!(
            default_branch,
            "/projects/35/search?scope=blobs&search=search_blobs&per_page=20&page=1"
        );
        assert!(!default_branch.contains("ref="));

        for (ref_, encoded) in [
            ("feature/wi-315", "feature%2Fwi-315"),
            ("v2.1.7.0", "v2.1.7.0"),
            ("0f4b2a1c", "0f4b2a1c"),
            ("release candidate", "release%20candidate"),
        ] {
            let endpoint =
                search_blobs_endpoint("35", "search_blobs", Some(ref_), 20, 1).unwrap();
            assert!(
                endpoint.ends_with(&format!("&ref={encoded}")),
                "ref {ref_} not encoded into {endpoint}"
            );
        }

        for blank in ["", "   "] {
            assert!(
                search_blobs_endpoint("35", "q", Some(blank), 20, 1).is_err(),
                "accepted a blank ref {blank:?}"
            );
        }
        // An empty query is still rejected with or without a ref.
        assert!(search_blobs_endpoint("35", "", Some("main"), 20, 1).is_err());
    }

    #[test]
    fn project_search_endpoint_encodes_project_and_query() {
        let endpoint = project_search_endpoint(
            "group/project",
            "blobs",
            "def search_blobs(",
            20,
            1,
        )
        .unwrap();
        assert_eq!(
            endpoint,
            "/projects/group%2Fproject/search?scope=blobs&search=def%20search_blobs%28&per_page=20&page=1"
        );

        // Only the repository scopes take ref; issues never gains one.
        let issues = project_search_endpoint("35", "issues", "refactor", 20, 2).unwrap();
        assert_eq!(
            issues,
            "/projects/35/search?scope=issues&search=refactor&per_page=20&page=2"
        );
        assert!(!issues.contains("ref="));
    }
}
