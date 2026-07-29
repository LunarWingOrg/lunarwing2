# GitLab Container Registry — setup and usage

Goal: enable GitLab's built-in container registry on `git.lunarwing.org` and
publish the external-worker images (`worker-nanocode`, `worker-pebble`,
`worker-opencode`) from CI, so MT hosts can pull prebuilt images instead of
building locally on every upgrade.

The CI side lives in `.gitlab/ci/containers.yml` (branch
`c0ntainers/mcr/v2.0.3.0`). The server side must be done once on the GitLab
host by someone with access (GitLab runs as a Docker container).

## 1. Server-side: enable the registry (run on the GitLab host)

### 1a. Recommended: port 5050 on the existing domain

Using `https://git.lunarwing.org:5050` reuses the **existing TLS certificate**
for `git.lunarwing.org` — no new DNS record and no new certificate. Omnibus
looks up the registry vhost certificate by URL hostname, so
`/etc/gitlab/ssl/git.lunarwing.org.crt` (already present) is used.

Add to the container's `GITLAB_OMNIBUS_CONFIG` environment (docker run `-e` or
docker-compose `environment:`):

```ruby
registry_external_url 'https://git.lunarwing.org:5050'
```

Publish the port (same place you publish 443):

```bash
-p 5050:5050
```

Recreate/restart the container (`docker compose up -d` or `docker restart
gitlab`); omnibus reconfigures automatically from the env config on boot.

### 1b. Alternative: dedicated subdomain

If you prefer `https://registry.git.lunarwing.org`:

- Add a DNS A record for `registry.git.lunarwing.org`.
- Provide `/etc/gitlab/ssl/registry.git.lunarwing.org.crt` (+ `.key`) in the
  container's config volume (or enable omnibus Let's Encrypt for the registry
  vhost).
- `registry_external_url 'https://registry.git.lunarwing.org'` — no port
  publish needed beyond 443 if a reverse proxy routes it; otherwise the
  registry nginx listens on 443 inside the container alongside the main one.

### 1c. If a reverse proxy fronts GitLab

The registry API (`/v2/`) needs large-request support and proper
`X-Forwarded-*` headers. Pointing clients directly at
`git.lunarwing.org:5050` (option 1a) bypasses the proxy entirely and avoids
this class of issues.

### 1d. Verify (from anywhere)

```bash
curl -i https://git.lunarwing.org:5050/v2/
# expect: HTTP/2 401 with a Docker-Distribution-Api-Version header — registry up

docker login git.lunarwing.org:5050   # GitLab username + PAT with read_registry/write_registry
```

In the GitLab UI: project → Deploy → Container Registry should now list an
(empty) registry for `lunarwing/lunarwing2`.

## 2. CI: build and push worker images

`.gitlab/ci/containers.yml` adds three jobs to a new `containers` stage:

| Job | Context | Image pushed |
|---|---|---|
| `worker-nanocode-image` | `lunarcode4lunarwing/` | `$CI_REGISTRY_IMAGE/worker-nanocode:{sha,latest}` |
| `worker-pebble-image` | repo root (Dockerfile in `pebble4lunarwing/`) | `$CI_REGISTRY_IMAGE/worker-pebble:{sha,latest}` |
| `worker-opencode-image` | `opencode4lunarwing/` | `$CI_REGISTRY_IMAGE/worker-opencode:{sha,latest}` |

Details:

- Builds use **Kaniko** (`gcr.io/kaniko-project/executor:v1.23.2-debug`) —
  unprivileged, works on any docker-executor runner, produces docker-format
  images so the Dockerfiles' `HEALTHCHECK`s survive (matches the mt-admin
  `--format docker` convention).
- Auth uses the automatic `$CI_REGISTRY_USER` / `$CI_REGISTRY_PASSWORD`
  per-job credentials — nothing to configure.
- Jobs run **automatically on the default branch** when the relevant worker
  directory changes, and are **manual** on every other pipeline (run the
  pipeline, click the job).
- Variables: `NANOCODE_REF` (default `v1.2.28`), `WITH_TOOLCHAINS` for
  opencode (default `false`; set `true` on a manual pipeline run for the fat
  toolchain image — mirrors mt-admin's `--with-toolchains`).
- The Kaniko executor image is pinned by sha256 digest (repo convention,
  enforced by `validate_config.py`). If gcr.io is unreachable from the
  runners, pull the executor image elsewhere, push it into our registry once
  section 1 is done, and update `.container-job`'s `image:name`.

## 3. Using the images on MT hosts

Images are named `git.lunarwing.org:5050/lunarwing/lunarwing2/worker-<name>`.

One-time login on the host (rootful runtime that mt-admin builds with):

```bash
podman login git.lunarwing.org:5050    # or docker login; use a read_registry PAT
```

Then, instead of building locally during upgrades:

```bash
podman pull git.lunarwing.org:5050/lunarwing/lunarwing2/worker-opencode:latest
podman tag  git.lunarwing.org:5050/lunarwing/lunarwing2/worker-opencode:latest \
            lunarwing-worker-opencode:latest
```

Tenant distribution is unchanged: mt-admin's existing save|load path pushes
the host-local `lunarwing-worker-*:latest` image into each tenant's rootless
store (`_ensure_tenant_image`), so pulling once on the host is enough.

Follow-up idea (not implemented here): teach `lunarwing-mt-admin.sh` a
`--pull-worker-images` mode that pulls from the registry instead of building,
plus an integrity check (digest pin per release).

## 4. Housekeeping

- **Cleanup policy**: project → Settings → Packages and registries → Cleanup
  policy for container repositories — e.g. keep 5 tags per repo, remove tags
  older than 30 days matching `.*` except `latest`. Keeps short-SHA tags from
  accumulating.
- Registry-side garbage collection runs via omnibus defaults; no action
  needed unless storage grows.
