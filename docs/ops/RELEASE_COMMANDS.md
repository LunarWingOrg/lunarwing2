# Release Commands (GitLab)

Step-by-step git / **GitLab** commands to cut a release for this remote:

```text
https://git.lunarwing.org/LunarWing/LunarWing_v2
```

Self-hosted GitLab CE (v19.2.0). Replace `<version>` (e.g. `1.1.9`) and
`<codename>` with the target release values. This guide uses the current
`master` → `release/v<version>` branching flow and the GitLab origin used by
`LunarWing_v2`.

---

## Prerequisites

```bash
# origin should be your self-hosted GitLab
git remote -v
# origin  https://git.lunarwing.org/LunarWing/LunarWing_v2.git (fetch/push)

# Auth for API release creation (pick one):
#  1) User Settings → Access Tokens → scope: api (or the narrower
#     "Create Releases" permission if using a Project Access Token)
#  2) Project Settings → Access Tokens → project-scoped, role: Developer+
#     (preferred over a personal token for automation — matches the spirit
#     of the old write:repository scope)
export GITLAB_TOKEN='…'          # do not commit this

# curl + jq for the API path below
command -v curl jq
```

GitLab's official CLI is [`glab`](https://gitlab.com/gitlab-org/cli) — the
`gh`/`tea` equivalent. This doc uses **curl** so no extra tool is required,
but `glab` is covered as an option below.

---

## Full flow

```bash
# ── 0. Preflight: up-to-date master with the release notes committed ───────────
git checkout master
git pull --ff-only origin master
git status                       # should be clean before tagging

# If RELEASE-v<version>.md (and any create-tenant-*.sh, etc.) are still uncommitted,
# commit them to master first so the tag captures them:
git add RELEASE-v<version>.md
# or, if notes already live under docs/:
# git add docs/releases/RELEASE-v<version>.md
git commit -m "Finalize release notes for v<version>"
git push origin master

# ── 1. Create the release branch from master ───────────────────────────────────
git checkout -b release/v<version> master
git push -u origin release/v<version>

# ── 2. Create + push the annotated tag ─────────────────────────────────────────
git tag -a v<version> -m "LunarWing v<version> - Codename <codename>"
git push origin v<version>

# ── 3. Create the GitLab release (Releases API) ────────────────────────────────
# Notes file: use the path that exists for this cut (root or docs/releases/).
NOTES_FILE=RELEASE-v<version>.md
# NOTES_FILE=docs/releases/RELEASE-v<version>.md

# Project path, URL-encoded (avoids needing the numeric project ID)
PROJECT=35
TAG=v<version>
TITLE="v<version> - Codename <codename>"

# Tag already exists (pushed in step 2), so "ref" is omitted — GitLab only
# needs it if it has to create the tag itself as part of the release.
jq -n \
  --arg tag "$TAG" \
  --arg title "$TITLE" \
  --arg body "$(cat "$NOTES_FILE")" \
  '{
    tag_name: $tag,
    name: $title,
    description: $body
  }' \
| curl -sS -X POST \
    -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
    -H "Content-Type: application/json" \
    -d @- \
    "https://git.lunarwing.org/api/v4/projects/${PROJECT}/releases"

# ── 4. Verify ────────────────────────────────────────────────────────────────
curl -sS -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
  "https://git.lunarwing.org/api/v4/projects/${PROJECT}/releases/${TAG}" \
  | jq '{tag: .tag_name, name: .name, url: ._links.self}'

# Browser
echo "https://git.lunarwing.org/LunarWing/LunarWing_v2/-/releases/${TAG}"
```

---

## tl;dr (example: v1.1.5 / Kawarimi on GitLab)

```bash
cd /path/to/LunarWing_v2

git checkout master
git pull --ff-only origin master
git checkout -b release/v1.1.5
git push -u origin release/v1.1.5

git tag -a v1.1.5 -m "LunarWing v1.1.5 - Codename Kawarimi"
git push origin v1.1.5

# GITLAB_TOKEN must already be set (api scope, or project token w/ release perms)
jq -n \
  --arg tag "v1.1.5" \
  --arg title "v1.1.5 - Codename Kawarimi" \
  --arg body "$(cat RELEASE-v1.1.5.md)" \
  '{
    tag_name: $tag,
    name: $title,
    description: $body
  }' \
| curl -sS -X POST \
    -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
    -H "Content-Type: application/json" \
    -d @- \
    "https://git.lunarwing.org/api/v4/projects/LunarWing%2FLunarWing_v2/releases"

# Verify
curl -sS -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
  "https://git.lunarwing.org/api/v4/projects/LunarWing%2FLunarWing_v2/releases/v1.1.5" \
  | jq '{tag: .tag_name, name: .name}'

# Open in browser:
# https://git.lunarwing.org/LunarWing/LunarWing_v2/-/releases/v1.1.5
```

**No `draft`/`prerelease` flags** — GitLab's Releases API doesn't have an
equivalent to Forgejo's `draft`/`prerelease` booleans. Conventions instead:

- **RC/prerelease**: tag as `v1.1.5-rc1`, note it plainly in the title/description.
- **Upcoming/scheduled release**: set `released_at` to a future ISO 8601
  timestamp; GitLab shows it as an "Upcoming Release" until that time passes.

---

## Optional: `glab` CLI

If you install [`glab`](https://gitlab.com/gitlab-org/cli) and authenticate
against your self-hosted instance:

```bash
glab auth login --hostname git.lunarwing.org --token "$GITLAB_TOKEN"

glab release create v<version> \
  --repo LunarWing/LunarWing_v2 \
  --name "v<version> - Codename <codename>" \
  --notes-file RELEASE-v<version>.md \
  --ref release/v<version>
```

(`glab` flag names shift between versions — `glab release create --help`.)

---

## Optional: upload release assets

Two approaches on GitLab, neither is a raw binary POST like Forgejo's:

**A) Link an existing artifact/URL** (simplest — points at a CI job artifact,
an object storage URL, etc.):

```bash
curl -sS -X POST \
  -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
  -H "Content-Type: application/json" \
  --data '{
    "name": "lunarwing-linux-x86_64",
    "url": "https://git.lunarwing.org/LunarWing/LunarWing_v2/-/jobs/artifacts/'"${TAG}"'/raw/target/release/lunarwing?job=build",
    "link_type": "other"
  }' \
  "https://git.lunarwing.org/api/v4/projects/${PROJECT}/releases/${TAG}/assets/links"
```

**B) Push to the Generic Package Registry first, then link it** (better for
actual binaries you want GitLab to store, rather than referencing a CI job
artifact that may expire):

```bash
# Upload
curl -sS -X PUT \
  -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
  --upload-file "path/to/artifact.tar.gz" \
  "https://git.lunarwing.org/api/v4/projects/${PROJECT}/packages/generic/lunarwing/${TAG#v}/artifact.tar.gz"

# Link it to the release
curl -sS -X POST \
  -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
  -H "Content-Type: application/json" \
  --data '{
    "name": "artifact.tar.gz",
    "url": "https://git.lunarwing.org/api/v4/projects/'"${PROJECT}"'/packages/generic/lunarwing/'"${TAG#v}"'/artifact.tar.gz",
    "link_type": "package"
  }' \
  "https://git.lunarwing.org/api/v4/projects/${PROJECT}/releases/${TAG}/assets/links"
```

This is also exactly what the CI-driven `release-cli` job from earlier does
automatically — worth switching to that instead of the manual curl steps once
you've got a build pipeline in place.

---

## Notes / differences from Codeberg (Forgejo API)

| Codeberg (Forgejo) | GitLab (this doc) |
|---|---|
| `POST /api/v1/repos/{owner}/{repo}/releases` | `POST /api/v4/projects/{id}/releases` |
| `Authorization: token <TOKEN>` | `PRIVATE-TOKEN: <TOKEN>` |
| `target_commitish` field | `ref` field (only needed if tag doesn't exist yet) |
| `draft` / `prerelease` booleans | no direct equivalent — use tag naming (`-rc1`) or `released_at` for "upcoming" |
| Binary asset upload: `POST .../releases/{id}/assets` | Generic Package Registry upload, then link via Release Links API |
| `tea` CLI | `glab` CLI |
| repo path: `owner/repo` | project path (URL-encoded) or numeric project ID |

Do **not** put `GITLAB_TOKEN` in the repo, shell history intentionally shared
with others, or release notes.

---

## Related

- [`RELEASE_CADENCE.md`](RELEASE_CADENCE.md) — odd/even cadence notes
- [`../releases/`](../releases/) — archived release notes