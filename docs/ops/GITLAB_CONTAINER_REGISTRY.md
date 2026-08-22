# GitLab Container Registry — setup and usage

Goal: enable GitLab's built-in container registry for `lunarwing/lunarwing2`
and publish supported external-worker images (`worker-pebble`,
`worker-opencode`, and `worker-kimi`) from CI, so MT hosts can pull prebuilt images instead of
building locally on every upgrade.

The active CI definition lives in `.gitlab/ci/containers.yml`; the original
implementation branch was `c0ntainers/mcr/v2.0.3.0`, but operators should use
the merged file in their target commit. The server side is a one-time GitLab
host operation for an administrator.

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
- **502 on Kaniko `PATCH .../blobs/uploads`:** not a missing backend — HTTP/2
  blob streaming through Traefik/nginx. CI forces `GODEBUG=http2client=0`
  and `--push-retry=5`. See section 4.
- **499 Client Closed Request on Kaniko `PATCH .../blobs/uploads`:** a proxy
  timeout firing mid-upload on multi-GB blobs (observed on the opencode/pebble
  worker images). Needs Traefik/omnibus timeout changes on this host — full
  diagnosis and exact config in `REGISTRY_PUSH_499.md`.
- **Wrong token realm / scheme mismatch on `docker login`:** the
  `registry_nginx['proxy_set_headers']` block is missing.

### 1f. Large-image notes for Traefik

Worker images can be multi-GB (especially opencode with toolchains). Traefik
streams request bodies by default, which is fine; large layer pushes that die
mid-`PATCH` with `499 Client Closed Request` mean a responding/forwarding
timeout fired — raise `respondingTimeouts` on the `websecure` entrypoint and
omnibus `registry_nginx` body/proxy timeouts per `REGISTRY_PUSH_499.md`.
Leave request buffering off.

## 2. CI: build and push worker images

`.gitlab/ci/containers.yml` adds supported worker-image jobs to the `containers` stage:

| Job | Context | Image pushed |
|---|---|---|
| `worker-pebble-image` | repo root (Dockerfile in `pebble4lunarwing/`) | `$CI_REGISTRY_IMAGE/worker-pebble:{v2.0.8.0,latest,<short-sha>}` |
| `worker-opencode-image` | `opencode4lunarwing/` | `$CI_REGISTRY_IMAGE/worker-opencode:{v2.0.8.0,latest,<short-sha>}` |
| `worker-kimi-image` | `kimicode4lunarwing/` | `$CI_REGISTRY_IMAGE/worker-kimi:{v2.0.8.0,latest,<short-sha>}` |

Details:

- `$CI_REGISTRY` / `$CI_REGISTRY_IMAGE` resolve automatically from
  `registry_external_url` once section 1 is live — no CI variable changes.
- Builds use **Kaniko** (`gcr.io/kaniko-project/executor`, pinned by sha256
  digest per repo convention) — unprivileged, works on any docker-executor
  runner, produces docker-format images so the Dockerfiles' `HEALTHCHECK`s
  survive (matches the mt-admin `--format docker` convention).
- Auth uses the automatic `$CI_REGISTRY_USER` / `$CI_REGISTRY_PASSWORD`
  per-job credentials — nothing to configure.
- All three image jobs are **manual-only on every pipeline** and
  `allow_failure: true`; multi-GB worker builds never start automatically. Run
  the pipeline and explicitly trigger only the image jobs needed for the
  release.
- Variable: `WITH_TOOLCHAINS` for both OpenCode and Kimi jobs (default
  `false`; set `true` on the selected manual job for the full toolchain image —
  mirrors mt-admin's `--with-toolchains`). Kimi also exposes
  `KIMI_CODE_VERSION` (currently `0.37.2`).
- Tags: a moving `WORKER_IMAGE_TAG` (default `v2.0.8.0`) plus `:latest`, plus
  an immutable `$CI_COMMIT_SHORT_SHA` tag. The moving tags silently retarget
  between builds — consumers that need a stable reference should pull the SHA
  tag (or pin the digest it resolves to). SHA tags accumulate, so the
  server-side cleanup policy (section 4) should be enabled to prune them; this
  registry's tag-delete UI has 502ed while trying to clear tags manually.
  When playing a job, override `WORKER_IMAGE_TAG` and/or `WORKER_IMAGE_NAME`
  to retarget a wedged tag or repository (e.g. `WORKER_IMAGE_NAME=worker-pebble-2080`).
- Kaniko jobs set `GODEBUG=http2client=0` and `--push-retry=5`. HTTP/2 blob
  PATCH through Traefik/nginx in front of GitLab's registry is a known 502
  source; forcing HTTP/1.1 is GitLab's documented workaround.
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
  policy for container repositories — e.g. keep 10 tags per repo, remove tags
  older than 30 days matching `.*` except `latest` and `v2.0.8.0`. CI pushes a
  new immutable short-SHA tag per build (for pinning), so this policy is what
  keeps them from accumulating — enable it if it is not already on. The
  tag-delete UI has 502ed on manual clears, so do not rely on it.
- Registry-side garbage collection runs via omnibus defaults; no action
  needed unless storage grows.
- **`502 Bad Gateway` on `PATCH .../blobs/uploads/...`:** Kaniko hitting
  Traefik/registry over HTTP/2. Confirm the job has `GODEBUG=http2client=0`.
  If a specific tag/repo is wedged after a failed delete, replay the manual
  job with `WORKER_IMAGE_TAG` and/or `WORKER_IMAGE_NAME` set to a fresh
  value rather than retrying the same destination.
- **`499 Client Closed Request` on `PATCH .../blobs/uploads/...`:** proxy
  timeout on large blobs, not fixable from CI — see `REGISTRY_PUSH_499.md`
  for the GitLab-host Traefik/omnibus changes.
