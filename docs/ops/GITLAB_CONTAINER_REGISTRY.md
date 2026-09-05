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
- **502 on `PATCH .../blobs/uploads`:** not a missing backend — inspect the
  Traefik/nginx HTTP/2 path and registry logs. Rootless BuildKit replaced the
  archived Kaniko client, so old Kaniko-specific retry advice no longer applies.
- **499 Client Closed Request on `PATCH .../blobs/uploads`:** a proxy timeout
  firing mid-upload on multi-GB worker images. Needs Traefik/omnibus timeout
  changes on this host — full diagnosis and exact config in
  `REGISTRY_PUSH_499.md`.
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

`.gitlab/ci/containers.yml` defines three jobs per worker:

| Step | Jobs | Contract |
|---|---|---|
| Candidate | `worker-{pebble,opencode,kimi}-candidate` | Rootless BuildKit pushes a unique `<full-sha>-<pipeline>-<job>` tag, then performs one uncached rebuild and requires the same digest |
| Verify | `worker-*-scan` | Trivy scans that exact digest for critical findings and emits a CycloneDX SBOM |
| Promote | `worker-*-promote` | A serialized, non-interruptible job copies the tested manifest to immutable `vX.Y.Z.W[-rcN]-<worker>` without rebuilding |

Details:

- Registry-writing jobs exist only for a protected default-branch push or a
  protected push-created tag matching `vX.Y.Z.W[-rcN]`. They never exist in an
  MR, ordinary branch, web/API/trigger pipeline, schedule, or unprotected tag.
- Candidate and promotion jobs are non-interruptible and have retries disabled,
  so redundant-pipeline cancellation or automatic timeout retries cannot hide a
  partial registry operation.
- The digest-pinned `moby/buildkit:v0.25.1-rootless` image runs without a Docker
  daemon or privileged container. The protected `container-build` runner must
  allow the narrow user-namespace/mount syscall set BuildKit needs; never mount
  a Docker socket or disable seccomp wholesale.
- Worker source revisions, base images, npm/Bun/Cargo locks, Debian snapshots,
  and optional Rust/Go downloads are fixed in source. Build arguments are
  hard-coded in `.gitlab/scripts/build-worker-image.sh`, not pipeline variables.
  `SOURCE_DATE_EPOCH` comes from the source commit and BuildKit rewrites layer
  timestamps. A second `--no-cache` build is pushed under a sibling tag; any
  digest mismatch blocks scanning and promotion.
- `worker-image.env`, both BuildKit metadata records, and
  `worker-inputs.sha256` bind the verification jobs to the candidate. Promotion
  also requires the recorded rebuild digest, re-reads the registry manifest,
  checks its digest and source revision label, validates the health check and
  entrypoint, and refuses to retarget an existing worker-specific release tag
  (`vX.Y.Z.W[-rcN]-pebble`, `-opencode`, or `-kimi`) to another digest.
- The retained `worker-image-evidence.json` records full source SHA, source/input
  hashes, image digest, SBOM/scan hashes, configuration proof, and pipeline URL.
  Tenant configuration should consume `repository@sha256:...`; `latest` and
  short-SHA tags are no longer published.

## 3. Using the images on MT hosts

Images are named `registry.git.lunarwing.org/lunarwing/lunarwing2/worker-<name>`.

One-time login on the host (rootful runtime that mt-admin builds with):

```bash
podman login registry.git.lunarwing.org    # or docker login; use a read_registry PAT
```

Then pull the digest from `worker-image-evidence.json`, not a moving tag:

```bash
podman pull registry.git.lunarwing.org/lunarwing/lunarwing2/worker-opencode@sha256:<digest>
podman tag  registry.git.lunarwing.org/lunarwing/lunarwing2/worker-opencode@sha256:<digest> \
            lunarwing-worker-opencode:latest
```

Tenant distribution is unchanged: mt-admin's existing save|load path pushes
the verified host-local image into each tenant's rootless store
(`_ensure_tenant_image`). `latest` here is only a host-local compatibility name;
CI never publishes it.

Follow-up idea (not implemented here): teach `lunarwing-mt-admin.sh` a
`--pull-worker-images` mode that pulls from the registry instead of building,
plus an integrity check (digest pin per release).

## 4. Housekeeping

- **Cleanup policy**: project → Settings → Packages and registries → Cleanup
  policy for container repositories. Preserve release tags and prune unique
  `<full-sha>-<pipeline>-<job>` candidate tags after retained evidence no longer
  needs them. Never protect `latest`; CI does not create it.
- Registry-side garbage collection runs via omnibus defaults; no action
  needed unless storage grows.
- **`502 Bad Gateway` on `PATCH .../blobs/uploads/...`:** inspect BuildKit and
  proxy logs. Re-running creates a fresh candidate tag, so it cannot mutate the
  candidate already referenced by verification evidence.
- **`499 Client Closed Request` on `PATCH .../blobs/uploads/...`:** proxy
  timeout on large blobs, not fixable from CI — see `REGISTRY_PUSH_499.md`
  for the GitLab-host Traefik/omnibus changes.
