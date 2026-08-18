# Self-hosted GitLab WASM Tool

Sandboxed LunarWing tool for GitLab REST API v4. Ported from NearAI IronHub's
GitLab tool, adapted for LunarWing WIT 0.3, self-hosted base URLs, host-side PAT
injection, bounded errors, and approval gating.

## Supported Actions

- Projects: `list_projects`, `get_project`, `create_project`
- Issues: `list_issues`, `get_issue`, `create_issue`, `update_issue`,
  `list_issue_notes`, `create_issue_note`
- Unified Work Items: `list_work_items`, `get_work_item` (Issues, Tasks, and
  hierarchy; `get_issue` automatically falls back when the classic endpoint 404s)
- Merge requests: `list_merge_requests`, `get_merge_request`,
  `create_merge_request`, `update_merge_request`, `list_mr_notes`,
  `create_mr_note`, `approve_merge_request`, `merge_merge_request`
- Branches: `list_branches`, `get_branch`, `create_branch`, `delete_branch`
- Files: `get_file_content`, `create_or_update_file`, `delete_file`
- Search: `search_projects`, `search_issues`, `search_blobs`
- CI/CD: `list_pipelines`, `get_pipeline`, `list_jobs`
- Profile: `current_user`

`project` accepts a numeric project ID or `group/project`, as a JSON string or
number, and may also be spelled `project_id`. Issue, Work Item, and merge
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

No wildcard host grant exists. Calls require approval by default; you may
choose "always allow" at the prompt, per-session, or via global auto-approval
config to skip subsequent prompts.

Merge approval and merge actions require current merge-request HEAD `sha`.
Existing-file updates and deletes require current `last_commit_id`; obtain it
with `get_file_content`. Guards prevent acting on code changed since review.
List and search responses include GitLab pagination headers in a `pagination`
object beside `items` when those headers are present; blank boundary values
(GitLab sends an empty `x-next-page` on the last page) are omitted.

`list_issues` accepts `all_pages: true` to walk every classic Issues API page
in one bounded call. Pages are fetched at `per_page=100` and merged; the run
stops at 20 pages, 2000 unique items, or 3 MiB of source JSON, whichever comes
first. The merged `pagination` object reports `fetched_pages`, `returned_items`,
`complete`, and `truncated`; when truncated it also carries `resume_page`, which
can be passed as `page` to a normal paged `list_issues` call.

Use `list_work_items` when Tasks and other Work Item types must be included.
GitLab Work Items use opaque keyset cursors rather than page numbers. A normal
call reports `has_next`, `complete`, `returned_items`, and (when present)
`next_cursor`; pass that cursor back as `cursor`. `all_pages: true` follows the
same-origin `Link: rel="next"` chain within one approval and applies the same
20-page / 2000-item / 3-MiB bounds. It preserves first-seen order and removes
duplicate stable Work Item IDs. GitLab keyset pages do not expose a server total;
once the complete chain is exhausted the tool reports the exact unique returned
count as `total`. Truncated or cycle-detected traversals omit `total` rather than
inventing one.

`get_work_item` retrieves Issue or Task details via the collection's exact
`iids` filter. This intentionally avoids GitLab 19.2 instances that expose Work
Items listing but feature-gate the single-item REST endpoint. `get_issue` keeps
the richer classic Issue response and falls back to the same Work Items lookup
only when the classic endpoint returns 404.

Choosing `always` at an approval prompt persists the preference even if that
specific external API request fails (for example, a 404 or transient outage),
so it no longer behaves like one-time `yes`. SSH remains stricter and only
earns persistence after a successful command.

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
taskset -c 0-11 cargo test -j12 --manifest-path tools-src/gitlab/Cargo.toml
taskset -c 0-11 cargo build --release -j12 --target wasm32-wasip2 \
  --manifest-path tools-src/gitlab/Cargo.toml
```

Artifact:

```text
tools-src/gitlab/target/wasm32-wasip2/release/gitlab_tool.wasm
```
