use serde::Deserialize;
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

pub struct ListMergeRequests<'a> {
    pub state: &'a str,
    pub source_branch: Option<&'a str>,
    pub target_branch: Option<&'a str>,
    pub per_page: u32,
    pub page_number: u32,
    pub all_pages: bool,
}

pub fn list_merge_requests(
    client: &GitlabClient,
    project: &str,
    request: ListMergeRequests<'_>,
) -> Result<Value, String> {
    let endpoint = merge_requests_endpoint(
        project,
        request.state,
        request.source_branch,
        request.target_branch,
        request.per_page,
        request.page_number,
        request.all_pages,
    )?;
    if request.all_pages {
        client.request_all_pages(&endpoint)
    } else {
        client.request_json("GET", &endpoint, None)
    }
}

fn merge_requests_endpoint(
    project: &str,
    state: &str,
    source_branch: Option<&str>,
    target_branch: Option<&str>,
    per_page: u32,
    page_number: u32,
    all_pages: bool,
) -> Result<String, String> {
    validate_choice(
        "state",
        state,
        &["opened", "closed", "locked", "merged", "all"],
    )?;
    let (per_page, page_number) = page(per_page, page_number);
    let per_page = if all_pages { 100 } else { per_page };
    let mut endpoint = format!(
        "/projects/{}/merge_requests?state={}&per_page={per_page}",
        project_path(project)?,
        url_encode(state)
    );
    if !all_pages {
        endpoint.push_str(&format!("&page={page_number}"));
    }
    for (field, branch) in [
        ("source_branch", source_branch),
        ("target_branch", target_branch),
    ] {
        if let Some(branch) = branch {
            require_nonempty(field, branch)?;
            endpoint.push_str(&format!("&{field}={}", url_encode(branch)));
        }
    }
    Ok(endpoint)
}

pub fn get_merge_request(client: &GitlabClient, project: &str, iid: u64) -> Result<Value, String> {
    let endpoint = format!("/projects/{}/merge_requests/{iid}", project_path(project)?);
    client.request_json("GET", &endpoint, None)
}

pub fn get_mr_changes(
    client: &GitlabClient,
    project: &str,
    iid: u64,
    access_raw_diffs: bool,
) -> Result<Value, String> {
    let query = if access_raw_diffs {
        "?access_raw_diffs=true"
    } else {
        ""
    };
    let endpoint = format!(
        "/projects/{}/merge_requests/{iid}/changes{query}",
        project_path(project)?
    );
    annotate_mr_changes(client.request_json("GET", &endpoint, None)?)
}

fn annotate_mr_changes(mut response: Value) -> Result<Value, String> {
    let mr = response
        .as_object_mut()
        .ok_or("expected a GitLab merge request changes object")?;
    // GitLab can cap the file list (e.g. changes_count="404+") without setting
    // overflow, even with raw diffs. Only an exact, matching count is evidence.
    let count = mr
        .get("changes_count")
        .and_then(Value::as_str)
        .and_then(|value| {
            if value.bytes().all(|byte| byte.is_ascii_digit()) {
                value.parse::<usize>().ok()
            } else {
                None
            }
        });
    let complete = mr.get("overflow").and_then(Value::as_bool) == Some(false)
        && mr
            .get("changes")
            .and_then(Value::as_array)
            .is_some_and(|files| {
                count == Some(files.len())
                    && files.iter().all(|file| {
                        // Empty text can also be a rename or a binary file; the
                        // response alone cannot establish that its contents were seen.
                        file.get("diff")
                            .and_then(Value::as_str)
                            .is_some_and(|diff| !diff.is_empty())
                            && ["too_large", "collapsed"].into_iter().all(|flag| {
                                file.get(flag)
                                    .is_none_or(|value| value.as_bool() == Some(false))
                            })
                    })
            });
    mr.insert("diff_complete".into(), json!(complete));
    Ok(response)
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

pub fn unapprove_merge_request(
    client: &GitlabClient,
    project: &str,
    iid: u64,
) -> Result<Value, String> {
    let endpoint = format!(
        "/projects/{}/merge_requests/{iid}/unapprove",
        project_path(project)?
    );
    client.request_json("POST", &endpoint, Some(&json!({})))
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

pub fn search_issues(
    client: &GitlabClient,
    project: &str,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    project_search(client, project, "issues", query, per_page, page_number)
}

/// Run a project-scoped search through the typed, per-page-capped path so a
/// broad term degrades to a smaller page instead of exhausting guest memory.
/// A zero-match query is a normal empty array, never an error.
fn project_search(
    client: &GitlabClient,
    project: &str,
    scope: &str,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    require_nonempty("query", query)?;
    let endpoint = project_search_endpoint(project, scope, query, per_page, page_number)?;
    client.request_typed_page::<SearchHit>(&endpoint)
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
    client.request_typed_page::<SearchHit>(&endpoint)
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
    let mut endpoint = project_search_endpoint(project, "blobs", query, per_page, page_number)?;
    if let Some(ref_) = ref_ {
        endpoint.push_str("&ref=");
        endpoint.push_str(&url_encode(ref_));
    }
    Ok(endpoint)
}

/// Search URL shared by every project search scope. Split out so the
/// `blobs` branch filter can be pinned without a live GitLab instance.
///
/// The page size is capped here, not at the call sites, so every search
/// scope inherits the WI#891 bound.
fn project_search_endpoint(
    project: &str,
    scope: &str,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<String, String> {
    let (per_page, page_number) = search_page(per_page, page_number);
    Ok(format!(
        "/projects/{}/search?scope={scope}&search={}&per_page={per_page}&page={page_number}",
        project_path(project)?,
        url_encode(query)
    ))
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

/// Per-page clamp for `list_jobs` (WI#525). Even with typed deserialization,
/// fewer jobs per page means fewer retained objects and a smaller raw body to
/// buffer in typical responses. This limits job count, not response bytes or
/// peak guest memory; the host call still buffers the entire body.
const LIST_JOBS_MAX_PER_PAGE: u32 = 50;

/// The flat investigation surface of one GitLab CI job (WI#525).
///
/// GitLab's full job object nests heavy `pipeline`, `commit`, `runner`,
/// `user`, `artifacts`, and `project` sub-objects. Deserializing straight into
/// this type makes `serde` **skip** every unmodelled field while streaming
/// tokens, avoiding a full parsed job tree in addition to the buffered raw
/// body. This reduces parse-time allocations, not the raw-body allocation.
/// Everything dropped here is one `web_url` hop away when present.
#[derive(Debug, serde::Serialize, Deserialize)]
struct JobSummary {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default, rename = "ref")]
    ref_: Option<String>,
    #[serde(default)]
    tag: Option<bool>,
    #[serde(default)]
    allow_failure: Option<bool>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    finished_at: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    queued_duration: Option<f64>,
    #[serde(default)]
    web_url: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

pub fn list_jobs(
    client: &GitlabClient,
    project: &str,
    pipeline_id: u64,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    let endpoint = jobs_endpoint(project, pipeline_id, per_page, page_number)?;
    client.request_typed_page::<JobSummary>(&endpoint)
}

/// Per-page clamp for the search actions (WI#891).
///
/// Search hits carry the full issue/MR `description`, which dominates the
/// page: a 100-hit issue search on `lunarwing/lunarwing2` is ~555 KB of JSON.
/// Parsing that into a `serde_json::Value` tree in the guest's 10 MB sandbox
/// is several times the raw size, and a refused `memory.grow` aborts the
/// module as an `unreachable` trap. A smaller page keeps a broad term inside
/// the budget; `pagination.next_page` continues the scan.
const SEARCH_MAX_PER_PAGE: u32 = 20;

/// One search hit, reduced to the fields an agent triages by (WI#891).
///
/// GitLab's search payload embeds whole issue/MR objects: `description`
/// alone is ~75% of a full page, plus `author`, `assignees`, `milestone`,
/// `time_stats`, and `task_completion_status`. Deserializing straight into
/// this type makes `serde` skip those subtrees while streaming tokens instead
/// of building their `Value` trees. `web_url` is one hop to the full object.
#[derive(Debug, serde::Serialize, Deserialize)]
struct SearchHit {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    iid: Option<u64>,
    #[serde(default)]
    project_id: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    /// Project hits carry a name and namespace path instead of a title.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    name_with_namespace: Option<String>,
    #[serde(default)]
    path_with_namespace: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    state: Option<String>,
    /// Echoed so the agent can see the visibility the server authorized.
    #[serde(default)]
    confidential: Option<bool>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    web_url: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    basename: Option<String>,
    /// Blob hits carry the branch/tag the match came from.
    #[serde(default, rename = "ref")]
    ref_: Option<String>,
    #[serde(default)]
    startline: Option<u64>,
}

pub fn search_projects(
    client: &GitlabClient,
    query: &str,
    per_page: u32,
    page_number: u32,
) -> Result<Value, String> {
    require_nonempty("query", query)?;
    let (per_page, page_number) = search_page(per_page, page_number);
    let endpoint = format!(
        "/search?scope=projects&search={}&per_page={per_page}&page={page_number}",
        url_encode(query)
    );
    client.request_typed_page::<SearchHit>(&endpoint)
}

/// Clamp a search page like [`page`] and then apply the tighter search cap.
fn search_page(per_page: u32, page_number: u32) -> (u32, u32) {
    let (per_page, page_number) = page(per_page, page_number);
    (per_page.min(SEARCH_MAX_PER_PAGE), page_number)
}

/// Build the `list_jobs` endpoint with the global `page()` clamp applied and
/// then the tighter WI#525 per-page clamp on top.
fn jobs_endpoint(
    project: &str,
    pipeline_id: u64,
    per_page: u32,
    page_number: u32,
) -> Result<String, String> {
    let (per_page, page_number) = page(per_page, page_number);
    let per_page = per_page.min(LIST_JOBS_MAX_PER_PAGE);
    Ok(format!(
        "/projects/{}/pipelines/{pipeline_id}/jobs?per_page={per_page}&page={page_number}",
        project_path(project)?
    ))
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
    use serde_json::{Map, json};

    use super::{
        SEARCH_MAX_PER_PAGE, SearchHit, annotate_mr_changes, merge_requests_endpoint,
        project_search_endpoint, require_changes, search_blobs_endpoint, search_page,
        validate_choice, validate_file_path, work_items_endpoint,
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
    fn capped_mr_file_count_is_incomplete_even_with_all_returned_patches() {
        let patch = json!({"new_path": "file", "diff": "@@ -1 +1 @@\n-old\n+new", "collapsed": false, "too_large": false});
        let mut response =
            json!({"overflow": false, "changes_count": "404+", "changes": vec![patch; 404]});
        let annotated = annotate_mr_changes(response.clone()).unwrap();
        response["diff_complete"] = json!(false);
        assert_eq!(annotated, response);
    }

    #[test]
    fn mr_diff_completeness_requires_matching_count_and_available_patches() {
        let patch = json!({"diff": "patch"});
        let complete = json!({"overflow": false, "changes_count": "1", "changes": [patch]});
        assert_eq!(
            annotate_mr_changes(complete.clone()).unwrap()["diff_complete"],
            true
        );
        for count in [json!(""), json!(null), json!("2"), json!("+1")] {
            let mut incomplete = complete.clone();
            incomplete["changes_count"] = count;
            assert_eq!(
                annotate_mr_changes(incomplete).unwrap()["diff_complete"],
                false
            );
        }
        for (key, value) in [
            ("collapsed", json!(true)),
            ("too_large", json!(true)),
            ("diff", json!("")),
        ] {
            let mut incomplete = complete.clone();
            incomplete["changes"][0][key] = value;
            assert_eq!(
                annotate_mr_changes(incomplete).unwrap()["diff_complete"],
                false
            );
        }
        let mut overflow = complete;
        overflow["overflow"] = json!(true);
        assert_eq!(
            annotate_mr_changes(overflow).unwrap()["diff_complete"],
            false
        );
    }

    #[test]
    fn merge_request_filters_reach_request_urls() {
        let single = merge_requests_endpoint(
            "group/project",
            "opened",
            Some("feature/wi-279"),
            Some("integration branch"),
            20,
            2,
            false,
        )
        .unwrap();
        assert_eq!(
            single,
            "/projects/group%2Fproject/merge_requests?state=opened&per_page=20&page=2&source_branch=feature%2Fwi-279&target_branch=integration%20branch"
        );

        let all =
            merge_requests_endpoint("35", "merged", None, Some("main"), 500, 1, true).unwrap();
        assert_eq!(
            all,
            "/projects/35/merge_requests?state=merged&per_page=100&target_branch=main"
        );
        assert!(merge_requests_endpoint("35", "opened", Some(" "), None, 20, 1, false).is_err());
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
            let endpoint = search_blobs_endpoint("35", "search_blobs", Some(ref_), 20, 1).unwrap();
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
        let endpoint =
            project_search_endpoint("group/project", "blobs", "def search_blobs(", 20, 1).unwrap();
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

    /// WI#525: a full GitLab job object deserializes into `JobSummary` while
    /// every heavy nested field is skipped without allocation; the retained
    /// surface carries the CI-triage fields intact.
    #[test]
    fn job_summary_skips_heavy_nested_fields_but_keeps_triage_surface() {
        let full_job = serde_json::json!({
            "id": 103070,
            "name": "cargo-deny",
            "stage": "test",
            "status": "failed",
            "failure_reason": "script_failure",
            "ref": "main",
            "tag": false,
            "allow_failure": false,
            "started_at": "2026-10-30T14:00:00.000Z",
            "finished_at": "2026-10-30T14:05:00.000Z",
            "duration": 300.5,
            "queued_duration": 1.25,
            "web_url": "https://git.lunarwing.org/lunarwing/lunarwing2/-/jobs/103070",
            "created_at": "2026-10-30T13:59:00.000Z",
            "pipeline": {"id": 6843, "project_id": 35, "sha": "d8bcc5262", "web_url": "https://..."},
            "commit": {"id": "d8bcc5262", "message": "very long message", "author_name": "a"},
            "runner": {"id": 1, "description": "shared", "tag_list": ["docker"]},
            "user": {"id": 4, "username": "kageho", "name": "kageho"},
            "artifacts": [{"file_type": "trace", "size": 123456, "filename": "trace.log"}],
            "artifacts_file": {"filename": "trace.log", "size": 123456},
            "runner_manager": {"id": 9, "system_id": "s1"},
            "project": {"ci_job_token_scope_enabled": false}
        });
        let summary: super::JobSummary = serde_json::from_value(full_job).unwrap();
        assert_eq!(summary.id, Some(103070));
        assert_eq!(summary.name.as_deref(), Some("cargo-deny"));
        assert_eq!(summary.status.as_deref(), Some("failed"));
        assert_eq!(summary.failure_reason.as_deref(), Some("script_failure"));
        assert_eq!(summary.ref_.as_deref(), Some("main"));
        assert_eq!(summary.tag, Some(false));
        assert_eq!(summary.allow_failure, Some(false));
        assert_eq!(summary.duration, Some(300.5));
        // Re-serialising yields only the flat surface — the heavy nested
        // objects are gone, bounding the retained response.
        let rendered = serde_json::to_value(&summary).unwrap();
        for key in [
            "pipeline",
            "commit",
            "runner",
            "user",
            "artifacts",
            "project",
        ] {
            assert!(rendered.get(key).is_none(), "heavy field {key} retained");
        }
        assert_eq!(
            rendered["web_url"],
            "https://git.lunarwing.org/lunarwing/lunarwing2/-/jobs/103070"
        );
    }

    /// Missing optional triage fields default to `None`.
    #[test]
    fn job_summary_tolerates_a_minimal_object() {
        let summary: super::JobSummary = serde_json::from_value(json!({ "id": 7 })).unwrap();
        assert_eq!(summary.id, Some(7));
        assert!(summary.name.is_none());
        assert!(summary.failure_reason.is_none());
    }

    /// SO-2: an omitted or null ID must not reject an otherwise usable job.
    #[test]
    fn job_summary_tolerates_missing_or_null_id() {
        for body in [r#"{"name":"partial"}"#, r#"{"id":null,"name":"partial"}"#] {
            let summary: super::JobSummary = serde_json::from_str(body).unwrap();
            assert!(summary.id.is_none());
            assert_eq!(summary.name.as_deref(), Some("partial"));
            let rendered = serde_json::to_value(summary).unwrap();
            assert_eq!(rendered.get("id"), Some(&serde_json::Value::Null));
            assert_eq!(rendered["name"], "partial");
        }
        // Optional does not mean accepting an ID with an invalid JSON type.
        assert!(serde_json::from_str::<super::JobSummary>(r#"{"id":"not-an-id"}"#).is_err());
    }

    /// A partial job in the middle must preserve the whole page and its order.
    #[test]
    fn job_summary_keeps_mixed_page_with_missing_ids() {
        let body = br#"[
            {"id":1,"name":"first","status":"success"},
            {"name":"missing-id","pipeline":{"id":42},"commit":{"message":"ignored"}},
            {"id":null,"name":"null-id"},
            {},
            {"id":5,"name":"last","status":"failed","failure_reason":"script_failure"}
        ]"#;
        let summaries: Vec<super::JobSummary> = serde_json::from_slice(body).unwrap();
        let items = serde_json::to_value(summaries).unwrap();
        let items = items.as_array().unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(
            items
                .iter()
                .map(|job| job["id"].clone())
                .collect::<Vec<_>>(),
            vec![json!(1), json!(null), json!(null), json!(null), json!(5)]
        );
        assert_eq!(items[0]["name"], "first");
        assert_eq!(items[1]["name"], "missing-id");
        assert_eq!(items[2]["name"], "null-id");
        assert!(items[3]["name"].is_null());
        assert_eq!(items[4]["name"], "last");
        assert_eq!(items[4]["status"], "failed");
        assert_eq!(items[4]["failure_reason"], "script_failure");
        assert!(items[1].get("pipeline").is_none());
        assert!(items[1].get("commit").is_none());
    }

    /// WI#525: `list_jobs` applies a tighter per-page clamp than the global
    /// `page()` ceiling, and still encodes the project and rejects page 0.
    #[test]
    fn jobs_endpoint_clamps_per_page_and_encodes_project() {
        let clamped = super::jobs_endpoint("group/project", 6843, 100, 1).unwrap();
        assert_eq!(
            clamped,
            "/projects/group%2Fproject/pipelines/6843/jobs?per_page=50&page=1"
        );
        let numeric = super::jobs_endpoint("35", 6842, 20, 3).unwrap();
        assert_eq!(
            numeric,
            "/projects/35/pipelines/6842/jobs?per_page=20&page=3"
        );
        // page 0 normalises to page 1.
        let normalised = super::jobs_endpoint("35", 6842, 0, 0).unwrap();
        assert_eq!(
            normalised,
            "/projects/35/pipelines/6842/jobs?per_page=1&page=1"
        );
    }

    /// WI#891: a broad term must not ask for a page larger than the guest can
    /// parse. The cap sits below the global `page()` ceiling and below the
    /// 100-hit page that carried ~555 KB of JSON.
    #[test]
    fn search_page_clamps_below_the_global_ceiling() {
        assert_eq!(search_page(100, 1), (SEARCH_MAX_PER_PAGE, 1));
        assert_eq!(search_page(50, 2), (SEARCH_MAX_PER_PAGE, 2));
        // A caller asking for less keeps their page size.
        assert_eq!(search_page(5, 3), (5, 3));
        // The global clamp still applies underneath.
        assert_eq!(search_page(0, 0), (1, 1));
        const { assert!(SEARCH_MAX_PER_PAGE < 100) };
    }

    /// WI#891: every search action routes through the capped page size, so the
    /// failing 100-hit request cannot be reissued by a future edit.
    #[test]
    fn search_endpoints_apply_the_page_cap() {
        let issues = project_search_endpoint("35", "issues", "PostgreSQL", 100, 1).unwrap();
        assert_eq!(
            issues,
            "/projects/35/search?scope=issues&search=PostgreSQL&per_page=20&page=1"
        );

        let blobs = search_blobs_endpoint("35", "PostgreSQL", None, 100, 1).unwrap();
        assert_eq!(
            blobs,
            "/projects/35/search?scope=blobs&search=PostgreSQL&per_page=20&page=1"
        );

        // A caller asking for fewer hits keeps their page size.
        let small = project_search_endpoint("35", "issues", "q", 5, 2).unwrap();
        assert!(small.contains("per_page=5"), "{small}");
    }

    /// WI#891: a hit carrying the full issue object keeps the fields an agent
    /// triages by while dropping the heavy subtrees that dominated the page.
    #[test]
    fn search_hit_keeps_triage_fields_and_skips_heavy_subtrees() {
        let hit = json!({
            "id": 993,
            "iid": 891,
            "project_id": 35,
            "title": "search_issues traps on a matching term",
            "description": "x".repeat(16_000),
            "state": "opened",
            "web_url": "https://git.example.test/lunarwing/lunarwing2/-/work_items/891",
            "author": { "id": 4, "username": "kageho" },
            "assignees": [{ "id": 18, "username": "applejack" }],
            "time_stats": { "time_estimate": 0 },
            "task_completion_status": { "count": 0 },
        });
        let summary: SearchHit = serde_json::from_value(hit).unwrap();
        let rendered = serde_json::to_value(&summary).unwrap();

        assert_eq!(rendered["iid"], 891);
        assert_eq!(rendered["project_id"], 35);
        assert_eq!(rendered["title"], "search_issues traps on a matching term");
        assert_eq!(rendered["state"], "opened");
        assert!(rendered["web_url"].is_string());
        for heavy in ["description", "author", "assignees", "time_stats"] {
            assert!(
                rendered.get(heavy).is_none(),
                "{heavy} must be skipped, not carried"
            );
        }
        assert!(
            serde_json::to_string(&summary).unwrap().len() < 1_000,
            "the retained summary must stay small"
        );
    }

    /// WI#891: blob hits keep the location fields the agent needs to open the
    /// file, including the branch the match came from.
    #[test]
    fn search_hit_keeps_blob_location_fields() {
        let hit = json!({
            "basename": "wrapper.rs",
            "filename": "ic/src/tools/wasm/wrapper.rs",
            "path": "ic/src/tools/wasm/wrapper.rs",
            "data": "fn execute() { ... }",
            "ref": "main",
            "startline": 1731,
            "project_id": 35,
        });
        let summary: SearchHit = serde_json::from_value(hit).unwrap();
        let rendered = serde_json::to_value(&summary).unwrap();
        assert_eq!(rendered["path"], "ic/src/tools/wasm/wrapper.rs");
        assert_eq!(rendered["basename"], "wrapper.rs");
        assert_eq!(rendered["ref"], "main");
        assert_eq!(rendered["startline"], 1731);
        assert!(rendered.get("data").is_none());
    }

    /// WI#891: project hits have no `title`/`iid` — they identify a project
    /// by name and namespace path. Dropping those would make a project search
    /// return empty objects.
    #[test]
    fn search_hit_keeps_project_identity_fields() {
        let hit = json!({
            "id": 35,
            "name": "lunarwing2",
            "name_with_namespace": "lunarwing / lunarwing2",
            "path_with_namespace": "lunarwing/lunarwing2",
            "visibility": "private",
            "description": "x".repeat(4_000),
            "namespace": { "id": 12, "full_path": "lunarwing" },
        });
        let summary: SearchHit = serde_json::from_value(hit).unwrap();
        let rendered = serde_json::to_value(&summary).unwrap();
        assert_eq!(rendered["name"], "lunarwing2");
        assert_eq!(rendered["name_with_namespace"], "lunarwing / lunarwing2");
        assert_eq!(rendered["path_with_namespace"], "lunarwing/lunarwing2");
        assert_eq!(rendered["visibility"], "private");
        assert!(rendered.get("description").is_none());
        assert!(rendered.get("namespace").is_none());
    }

    /// WI#891: a zero-match search is an ordinary empty array — it must parse
    /// as an empty hit list, not as a shape error, so "no matches" stays
    /// distinguishable from a backend failure.
    #[test]
    fn zero_match_search_parses_as_an_empty_page() {
        let hits: Vec<SearchHit> = serde_json::from_str("[]").unwrap();
        assert!(hits.is_empty());

        let rendered = serde_json::to_value(hits).unwrap();
        assert_eq!(rendered, json!([]));
    }

    /// WI#891: confidentiality is server-enforced — the search scope must not
    /// add a parameter that could widen it (there is no `confidential` or
    /// `all` flag here), and hits that do carry the flag must keep it intact.
    #[test]
    fn search_scope_keeps_server_side_confidentiality_and_echoes_the_flag() {
        let issues = project_search_endpoint("35", "issues", "security", 20, 1).unwrap();
        for widening in ["confidential", "all=true", "scope=all"] {
            assert!(
                !issues.contains(widening),
                "search must not widen visibility with {widening}: {issues}"
            );
        }
        assert!(issues.contains("scope=issues"), "{issues}");

        // A confidential hit keeps its flag so the agent can see the state
        // the server authorized it to see.
        let confidential = json!({
            "iid": 890,
            "title": "command guard finding",
            "confidential": true,
        });
        let summary: SearchHit = serde_json::from_value(confidential).unwrap();
        let rendered = serde_json::to_value(&summary).unwrap();
        assert_eq!(rendered["confidential"], true);
    }
}
