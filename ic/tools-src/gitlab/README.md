# Self-hosted GitLab WASM Tool

Sandboxed LunarWing tool for GitLab REST API v4. Ported from NearAI IronHub's
GitLab tool, adapted for LunarWing WIT 0.3, self-hosted base URLs, host-side PAT
injection, bounded errors, and explicit approval.

## Supported Actions

- Projects: `list_projects`, `get_project`, `create_project`
- Issues: `list_issues`, `get_issue`, `create_issue`, `update_issue`,
  `list_issue_notes`, `create_issue_note`
- Merge requests: `list_merge_requests`, `get_merge_request`,
  `create_merge_request`, `update_merge_request`, `list_mr_notes`,
  `create_mr_note`, `approve_merge_request`, `merge_merge_request`
- Branches: `list_branches`, `get_branch`, `create_branch`, `delete_branch`
- Files: `get_file_content`, `create_or_update_file`, `delete_file`
- Search: `search_projects`, `search_issues`, `search_blobs`
- CI/CD: `list_pipelines`, `get_pipeline`, `list_jobs`
- Profile: `current_user`

`project` accepts a numeric project ID or `group/project`. Issue and merge
request `iid` values are project-local IDs shown by GitLab, not global IDs.
`get_file_content` returns base64 to preserve arbitrary file bytes.

## Authentication

Create a personal access token with `api` scope, then configure extension through
LunarWing setup UI or CLI. Secret name is `gitlab_token`. LunarWing injects token
as `PRIVATE-TOKEN` only at HTTP boundary; WASM guest can only check whether secret
exists.

Bundled capability file grants only:

```text
https://git.lunarwing.org/api/v4/
```

No wildcard host grant exists. Every call requires explicit approval, including
when global or session auto-approval is enabled.

Merge approval and merge actions require current merge-request HEAD `sha`.
Existing-file updates and deletes require current `last_commit_id`; obtain it
with `get_file_content`. Guards prevent acting on code changed since review.
List and search responses include GitLab pagination headers in a `pagination`
object beside `items` when those headers are present.

## Another Self-hosted Host

Static WASM capabilities cannot be parameterized from setup fields. Supporting
another host therefore requires explicit deployment-specific grant:

1. Copy tool source and capability file.
2. Replace `git.lunarwing.org` in both `http.allowlist[].host` and credential
   `host_patterns` with exact GitLab hostname. Never use `*`.
3. Update `auth.setup_url`.
4. Write operator-managed workspace config:

   ```json
   {
     "base_url": "https://gitlab.example.com"
   }
   ```

   Save as `config/gitlab.json`.

5. Rebuild and install resulting WASM plus edited sidecar.

Relative URL installs are supported, for example
`https://example.com/gitlab`; corresponding allowlist path must then be
`/gitlab/api/v4/`. HTTP is rejected. Credentials, query strings, and fragments
in `base_url` are rejected. Private-network hosts also require LunarWing's
explicit `ALLOW_PRIVATE_IPS=1` host policy; prefer a narrow HTTPS gateway rather
than globally relaxing egress. Non-443 ports are rejected because capability
and credential grants are hostname-scoped.

## Build

From `ic/`:

```bash
taskset -c 0-5 cargo test -j6 --manifest-path tools-src/gitlab/Cargo.toml
taskset -c 0-5 cargo build --release -j6 --target wasm32-wasip2 \
  --manifest-path tools-src/gitlab/Cargo.toml
```

Artifact:

```text
tools-src/gitlab/target/wasm32-wasip2/release/gitlab_tool.wasm
```
