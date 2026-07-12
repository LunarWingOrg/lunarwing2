# Release Commands (Codeberg)

## This is unverified rn.

Step-by-step git / **Codeberg** commands to cut a release for this remote:

```text
https://codeberg.org/LunarWing/LunarWing_v2
```

Codeberg is **Forgejo** (Gitea-compatible API). Replace `<version>` (e.g. `1.1.9`)
and `<codename>` with the target release values. Branching strategy
(`staging` → `release/v<version>`) is documented in the root `CLAUDE.md` where
applicable.

> **Sibling doc:** GitHub-oriented steps remain in
> [`RELEASE-COMMANDS.md`](RELEASE-COMMANDS.md) for historical mirrors. Prefer
> **this** file for the Codeberg origin used by `LunarWing_v2`.

---

## Prerequisites

```bash
# origin should be Codeberg
git remote -v
# origin  https://codeberg.org/LunarWing/LunarWing_v2.git (fetch/push)

# Auth for API release creation (pick one):
#  1) Codeberg Settings → Applications → Generate Token
#     scopes: write:repository (enough for releases)
export CODEBERG_TOKEN='…'          # do not commit this
#  2) or put the same token in a local untracked file and source it

# curl + jq for the API path below
command -v curl jq
```

Optional CLI: [tea](https://gitea.com/gitea/tea) works against Codeberg if
configured with the Codeberg host; this doc uses **curl** so no extra tool is
required.

---

## Full flow

```bash
# ── 0. Preflight: up-to-date staging with the release notes committed ──────────
git checkout staging
git pull --ff-only origin staging
git status                       # should be clean before tagging

# If RELEASE-v<version>.md (and any create-tenant-*.sh, etc.) are still uncommitted,
# commit them to staging first so the tag captures them:
git add RELEASE-v<version>.md
# or, if notes already live under docs/:
# git add docs/releases/RELEASE-v<version>.md
git commit -m "Finalize release notes for v<version>"
git push origin staging

# ── 1. Create the release branch from staging ──────────────────────────────────
git checkout -b release/v<version> staging
git push -u origin release/v<version>

# ── 2. Create + push the annotated tag ─────────────────────────────────────────
git tag -a v<version> -m "LunarWing v<version> - Codename <codename>"
git push origin v<version>

# ── 3. Create the Codeberg release (Forgejo API) ───────────────────────────────
# Notes file: use the path that exists for this cut (root or docs/releases/).
NOTES_FILE=RELEASE-v<version>.md
# NOTES_FILE=docs/releases/RELEASE-v<version>.md

OWNER=LunarWing
REPO=LunarWing_v2
TAG=v<version>
TITLE="v<version> - Codename <codename>"

# Build JSON safely from the notes file (preserves newlines).
jq -n \
  --arg tag "$TAG" \
  --arg title "$TITLE" \
  --arg body "$(cat "$NOTES_FILE")" \
  '{
    tag_name: $tag,
    target_commitish: ("release/" + ($tag | ltrimstr("v"))),
    name: $title,
    body: $body,
    draft: false,
    prerelease: false
  }' \
| curl -sS -X POST \
    -H "Authorization: token ${CODEBERG_TOKEN}" \
    -H "Content-Type: application/json" \
    -d @- \
    "https://codeberg.org/api/v1/repos/${OWNER}/${REPO}/releases"

# target_commitish above is release/v<version> (strip leading "v" from tag).
# If you tagged an exact SHA instead, set target_commitish to that SHA.

# ── 4. Verify ─────────────────────────────────────────────────────────────────
# List release
curl -sS -H "Authorization: token ${CODEBERG_TOKEN}" \
  "https://codeberg.org/api/v1/repos/${OWNER}/${REPO}/releases/tags/${TAG}" \
  | jq '{tag: .tag_name, name: .name, html: .html_url, prerelease: .prerelease}'

# Browser
echo "https://codeberg.org/${OWNER}/${REPO}/releases/tag/${TAG}"
```

> Release notes are often drafted as `RELEASE-v<version>.md` at the repo root
> (so the tag captures them) and archived under `docs/releases/` or
> `docs/ops/` after the cut — follow whatever path this release already uses.

---

## tl;dr (example: v1.1.5 / Kawarimi on Codeberg)

```bash
cd /path/to/LunarWing_v2

git checkout staging
git pull --ff-only origin staging
git checkout -b release/v1.1.5
git push -u origin release/v1.1.5

git tag -a v1.1.5 -m "LunarWing v1.1.5 - Codename Kawarimi"
git push origin v1.1.5

# CODEBERG_TOKEN must already be set (write:repository)
jq -n \
  --arg tag "v1.1.5" \
  --arg title "v1.1.5 - Codename Kawarimi" \
  --arg body "$(cat RELEASE-v1.1.5.md)" \
  '{
    tag_name: $tag,
    target_commitish: "release/v1.1.5",
    name: $title,
    body: $body,
    draft: false,
    prerelease: false
  }' \
| curl -sS -X POST \
    -H "Authorization: token ${CODEBERG_TOKEN}" \
    -H "Content-Type: application/json" \
    -d @- \
    "https://codeberg.org/api/v1/repos/LunarWing/LunarWing_v2/releases"

# Verify
curl -sS -H "Authorization: token ${CODEBERG_TOKEN}" \
  "https://codeberg.org/api/v1/repos/LunarWing/LunarWing_v2/releases/tags/v1.1.5" \
  | jq '{tag: .tag_name, name: .name, html: .html_url}'

# Open in browser:
# https://codeberg.org/LunarWing/LunarWing_v2/releases/tag/v1.1.5
```

Matches a normal (non-prerelease) “latest” style cut: `draft: false`,
`prerelease: false`. For RCs set `"prerelease": true`.

---

## Optional: `tea` CLI

If you install [tea](https://gitea.com/gitea/tea) and log into Codeberg:

```bash
tea login add --name codeberg --url https://codeberg.org --token "$CODEBERG_TOKEN"
tea releases create \
  --repo LunarWing/LunarWing_v2 \
  --tag v<version> \
  --target release/v<version> \
  --title "v<version> - Codename <codename>" \
  --note-file RELEASE-v<version>.md
```

(`tea` flag names vary slightly by version — `tea releases create -h`.)

---

## Optional: upload release assets

After the release exists, attach binaries/tarballs:

```bash
# Resolve release id
REL_ID=$(curl -sS -H "Authorization: token ${CODEBERG_TOKEN}" \
  "https://codeberg.org/api/v1/repos/${OWNER}/${REPO}/releases/tags/${TAG}" \
  | jq .id)

curl -sS -X POST \
  -H "Authorization: token ${CODEBERG_TOKEN}" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @"path/to/artifact.tar.gz" \
  "https://codeberg.org/api/v1/repos/${OWNER}/${REPO}/releases/${REL_ID}/assets?name=artifact.tar.gz"
```

---

## Notes / differences from GitHub (`gh`)

| GitHub (`gh`) | Codeberg (this doc) |
|---------------|---------------------|
| `gh release create … --notes-file …` | Forgejo `POST /api/v1/repos/{owner}/{repo}/releases` |
| `gh release view … --web` | open `https://codeberg.org/…/releases/tag/v…` or GET releases API |
| `gh repo set-default org/repo` | `git remote set-url origin https://codeberg.org/LunarWing/LunarWing_v2.git` |
| GitHub “latest” flag | no identical flag; use non-prerelease + newest tag, or set latest in UI |

Do **not** put `CODEBERG_TOKEN` in the repo, shell history intentionally shared
with others, or release notes.

---

## Related

- [`RELEASE-COMMANDS.md`](RELEASE-COMMANDS.md) — original GitHub / `gh` variant
- [`RELEASE_CADENCE.md`](RELEASE_CADENCE.md) — odd/even cadence notes
- `docs/releases/` — archived release notes (where present)
