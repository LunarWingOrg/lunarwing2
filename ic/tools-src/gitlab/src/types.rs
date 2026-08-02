use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitlabAction {
    CurrentUser {},
    ListProjects {
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
        #[serde(default)]
        search: Option<String>,
        #[serde(default = "default_membership")]
        membership: bool,
    },
    GetProject {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
    },
    CreateProject {
        name: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default = "default_visibility")]
        visibility: String,
        #[serde(default = "default_true")]
        initialize_with_readme: bool,
    },
    ListIssues {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        #[serde(default = "default_issue_state")]
        state: String,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    GetIssue {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
    },
    CreateIssue {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        title: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        labels: Vec<String>,
        #[serde(default)]
        assignee_ids: Vec<u64>,
    },
    UpdateIssue {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        state_event: Option<String>,
        #[serde(default)]
        labels: Option<Vec<String>>,
    },
    ListIssueNotes {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    CreateIssueNote {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        body: String,
    },
    ListMergeRequests {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        #[serde(default = "default_mr_state")]
        state: String,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    GetMergeRequest {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
    },
    CreateMergeRequest {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        source_branch: String,
        target_branch: String,
        title: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        remove_source_branch: bool,
        #[serde(default)]
        squash: bool,
    },
    UpdateMergeRequest {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        state_event: Option<String>,
        #[serde(default)]
        target_branch: Option<String>,
    },
    ListMrNotes {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    CreateMrNote {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        body: String,
    },
    ApproveMergeRequest {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        sha: String,
    },
    MergeMergeRequest {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        iid: u64,
        sha: String,
        #[serde(default)]
        merge_commit_message: Option<String>,
        #[serde(default)]
        squash: bool,
        #[serde(default)]
        should_remove_source_branch: bool,
    },
    ListBranches {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        #[serde(default)]
        search: Option<String>,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    GetBranch {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        branch: String,
    },
    CreateBranch {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        branch: String,
        #[serde(rename = "ref")]
        ref_: String,
    },
    DeleteBranch {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        branch: String,
    },
    GetFileContent {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        path: String,
        #[serde(rename = "ref")]
        ref_: String,
    },
    CreateOrUpdateFile {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        path: String,
        branch: String,
        content: String,
        commit_message: String,
        #[serde(default)]
        encoding: Option<String>,
        #[serde(default)]
        author_email: Option<String>,
        #[serde(default)]
        author_name: Option<String>,
        #[serde(default)]
        last_commit_id: Option<String>,
    },
    DeleteFile {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        path: String,
        branch: String,
        commit_message: String,
        last_commit_id: String,
    },
    SearchProjects {
        query: String,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    SearchIssues {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        query: String,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    SearchBlobs {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        query: String,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    ListPipelines {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(rename = "ref", default)]
        ref_: Option<String>,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
    GetPipeline {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        pipeline_id: u64,
    },
    ListJobs {
        #[serde(alias = "project_id", deserialize_with = "deserialize_project")]
        project: String,
        pipeline_id: u64,
        #[serde(default = "default_per_page")]
        per_page: u32,
        #[serde(default = "default_page")]
        page: u32,
    },
}

fn default_per_page() -> u32 {
    20
}

fn default_page() -> u32 {
    1
}

fn default_membership() -> bool {
    true
}

fn default_visibility() -> String {
    "private".to_string()
}

fn default_issue_state() -> String {
    "opened".to_string()
}

fn default_mr_state() -> String {
    "opened".to_string()
}

fn default_true() -> bool {
    true
}

/// Accept a GitLab project reference as either a numeric ID (`35`) or a
/// string (`"35"` / `"group/project"`). LLM callers routinely emit the
/// numeric JSON form for integer IDs; rejecting it only forces a retry.
/// Also reachable via the `project_id` alias, matching GitLab API naming.
fn deserialize_project<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct ProjectVisitor;

    impl Visitor<'_> for ProjectVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a project ID (integer) or path (string)")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<String, E> {
            u64::try_from(value)
                .map(|id| id.to_string())
                .map_err(|_| E::custom("project ID must not be negative"))
        }
    }

    deserializer.deserialize_any(ProjectVisitor)
}
