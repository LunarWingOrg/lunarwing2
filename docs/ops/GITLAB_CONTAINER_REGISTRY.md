# GitLab Container Registry — setup and usage

Goal: enable GitLab's built-in container registry for `lunarwing/lunarwing2`
and publish the external-worker images (`worker-nanocode`, `worker-pebble`,
`worker-opencode`) from CI, so MT hosts can pull prebuilt images instead of
building locally on every upgrade.

The CI side lives in `.gitlab/ci/containers.yml` (branch
`c0ntainers/mcr/v2.0.3.0`). The server side must be done once on the GitLab
host by someone with access.

**Environment (confirmed 2026-07-29):** GitLab runs as a Docker container
behind **Traefik** (`proxy` network, `websecure` entrypoint, `letsencrypt`
certresolver). Traefik terminates TLS — the GitLab container itself holds no
certs. Registry URL: **`https://registry.git.lunarwing.org`** (subdomain
through Traefik; the port-5050-direct alternative doesn't apply here because
omnibus has no certificates of its own).

## 1. Server-side: enable the registry (run on the GitLab host)

Three parts: DNS, Traefik route, omnibus config.

### 1a. DNS

Add an A record so the subdomain actually resolves (the previously sketched
`regisitry.sobe.world` was both typo'd and unregistered — discard it):

```
registry.git.lunarwing.org.  A  104.36.84.181   ; same IP as git.lunarwing.org
```

Traefik's `letsencrypt` certresolver will issue the cert automatically once
DNS resolves and the route exists (HTTP-01 via the existing `websecure`
entrypoint — nothing else to configure).

### 1b. Traefik route

Routes live in Traefik's **file provider** (`traefik-dynamic.yaml`) — the
`traefik.*` labels on the GitLab container are inert (`traefik.enable=false`).
Add to `traefik-dynamic.yaml`:

Under `http.routers` (sibling to the existing `gitlab:` router):

```yaml
  gitlab-registry:
    rule: "Host(`registry.git.lunarwing.org`)"
    entryPoints:
      - websecure
    tls:
      certResolver: letsencrypt
    service: gitlab-registry
```

Under `http.services` (sibling to the existing `gitlab:` service; plain HTTP
backend, same as the main `gitlab:8181` service):

```yaml
  gitlab-registry:
    loadBalancer:
      servers:
        - url: "http://gitlab:5050"
```

The file provider hot-reloads on save — no Traefik restart needed. The
`letsencrypt` certresolver issues the cert automatically once DNS resolves.

No host port publish is needed: Traefik reaches the container's 5050 over the
shared `proxy` network.

### 1c. GitLab omnibus config

Add to the container's `GITLAB_OMNIBUS_CONFIG` environment (docker run `-e`
or compose `environment:`):

```ruby
registry_external_url 'https://registry.git.lunarwing.org'
registry_nginx['listen_port'] = 5050
registry_nginx['listen_https'] = false   # Traefik terminates TLS
registry_nginx['proxy_set_headers'] = {  # same headers as the main nginx vhost
  "X-Forwarded-Proto" => "https",
  "X-Forwarded-Ssl" => "on"
}
```

Setting `registry_external_url` auto-enables the registry in gitlab-rails.
The `registry_nginx` lines keep the in-container registry listener on plain
HTTP :5050, matching the Traefik service above. The `proxy_set_headers` block
mirrors the main `nginx['proxy_set_headers']` — without it the registry can
emit `http://` token/redirect URLs and `docker login`/push fails on the
scheme mismatch. (`Host` needs nothing: Traefik's `passHostHeader` defaults
to true.)

Recreate/restart the container (`docker compose up -d`); omnibus reconfigures
from the env config on boot.

### 1d. Verify (from anywhere)

```bash
curl -i https://registry.git.lunarwing.org/v2/
# expect: HTTP/2 401 with a Docker-Distribution-Api-Version header — registry up

docker login registry.git.lunarwing.org   # GitLab username + PAT with read_registry/write_registry
```

In the GitLab UI: project → Deploy → Container Registry should now list an
(empty) registry for `lunarwing/lunarwing2`.

(Verified working 2026-07-29: LE cert issued for `registry.git.lunarwing.org`,
`/v2/` returns 401 + `docker-distribution-api-version: registry/2.0`, token
realm `https://git.lunarwing.org/jwt/auth`.)

### 1e. Troubleshooting (from the actual bring-up)

- **`curl: (60) self-signed certificate` / `CN=TRAEFIK DEFAULT CERT`:**
  Traefik has no router for the host and is serving its fallback cert. Check
  `docker logs traefik | grep -iE 'error|acme'` — a YAML error in
  `traefik-dynamic.yaml` makes Traefik reject the **entire file** (all routes,
  not just the new one → every site 404s). Fix indentation: routers under
  `http.routers`, services under `http.services`.
- **502 "Waiting for GitLab to boot" page:** that's GitLab's own boot page,
  not a proxy problem — Rails is still starting after the recreate. Wait a
  few minutes.
- **502 Bad Gateway from Traefik (no GitLab page):** the router matched but
  the backend is down — check `docker exec gitlab ss -ltnp | grep 5050` and
  `docker exec gitlab grep '^registry' /etc/gitlab/gitlab.rb`. If the
  registry lines are missing, the `GITLAB_OMNIBUS_CONFIG` env didn't render —
  it must be a block scalar (`|`) in compose, not list syntax.
- **Wrong token realm / scheme mismatch on `docker login`:** the
  `registry_nginx['proxy_set_headers']` block is missing.

### 1f. Large-image notes for Traefik

Worker images can be multi-GB (especially opencode with toolchains). Traefik
streams request bodies by default, which is fine; if very large layer pushes
ever stall, raise `forwardingTimeouts` on the entrypoint or on the
`gitlab-registry` router (`serversTransport`). Leave request buffering off.

## 2. CI: build and push worker images

`.gitlab/ci/containers.yml` adds three jobs to a new `containers` stage:

| Job | Context | Image pushed |
|---|---|---|
| `worker-nanocode-image` | `lunarcode4lunarwing/` | `$CI_REGISTRY_IMAGE/worker-nanocode:{sha,latest}` |
| `worker-pebble-image` | repo root (Dockerfile in `pebble4lunarwing/`) | `$CI_REGISTRY_IMAGE/worker-pebble:{sha,latest}` |
| `worker-opencode-image` | `opencode4lunarwing/` | `$CI_REGISTRY_IMAGE/worker-opencode:{sha,latest}` |

Details:

- `$CI_REGISTRY` / `$CI_REGISTRY_IMAGE` resolve automatically from
  `registry_external_url` once section 1 is live — no CI variable changes.
- Builds use **Kaniko** (`gcr.io/kaniko-project/executor`, pinned by sha256
  digest per repo convention) — unprivileged, works on any docker-executor
  runner, produces docker-format images so the Dockerfiles' `HEALTHCHECK`s
  survive (matches the mt-admin `--format docker` convention).
- Auth uses the automatic `$CI_REGISTRY_USER` / `$CI_REGISTRY_PASSWORD`
  per-job credentials — nothing to configure.
- Jobs run **automatically on the default branch** when the relevant worker
  directory changes, and are **manual** on every other pipeline (run the
  pipeline, click the job).
- Variables: `NANOCODE_REF` (default `v1.2.28`), `WITH_TOOLCHAINS` for
  opencode (default `false`; set `true` on a manual pipeline run for the fat
  toolchain image — mirrors mt-admin's `--with-toolchains`).
- If gcr.io is unreachable from the runners, pull the executor image
  elsewhere, push it into our registry once section 1 is done, and update
  `.container-job`'s `image:name`.

## 3. Using the images on MT hosts

Images are named `registry.git.lunarwing.org/lunarwing/lunarwing2/worker-<name>`.

One-time login on the host (rootful runtime that mt-admin builds with):

```bash
podman login registry.git.lunarwing.org    # or docker login; use a read_registry PAT
```

Then, instead of building locally during upgrades:

```bash
podman pull registry.git.lunarwing.org/lunarwing/lunarwing2/worker-opencode:latest
podman tag  registry.git.lunarwing.org/lunarwing/lunarwing2/worker-opencode:latest \
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
