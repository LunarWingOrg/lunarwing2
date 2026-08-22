# Registry push failures: `499 Client Closed Request` on blob PATCH

Status: **diagnosed 2026-08-22; server-side fix must be applied on the GitLab
host — it cannot be done from the dev box / this repo.** CI-side mitigations
are already in `.gitlab/ci/containers.yml`.

## Symptom

Manual worker image jobs (`worker-opencode-image`, `worker-pebble-image`)
fail during the Kaniko push phase:

```
WARN[0205] Retrying operation after 2s due to PATCH
https://registry.git.lunarwing.org/v2/lunarwing/lunarwing2/worker-pebble/blobs/uploads/<uuid>?_state=...:
unexpected status code 499 : Client Closed Request
```

Kaniko retries (`--push-retry=5`), each retry restarts the whole blob upload,
and the job eventually fails. Only the **opencode** and **pebble** images are
affected — those carry the largest layers. `worker-kimi` (slim) pushes fine.

## What is confirmed (checked from the dev box, 2026-08-22)

- The registry itself is healthy: `https://registry.git.lunarwing.org/v2/`
  answers `401` with `docker-distribution-api-version: registry/2.0` in ~0.2 s.
- The request chain is: `kaniko → Traefik (websecure, TLS termination) →
  omnibus registry_nginx (gitlab:5050) → GitLab registry`. The `server: nginx`
  header on `/v2/` responses confirms responses pass through the omnibus
  registry nginx.
- **This box cannot apply the fix**: it has no docker daemon, and the GitLab
  host (`git.lunarwing.org` / 104.36.84.181) is a separate machine. SSH to
  `git@git.lunarwing.org` lands in GitLab-shell (`Welcome to GitLab`), not a
  host shell. Traefik and omnibus config changes must be made on that host.

## Root cause

`499` is nginx's own status code meaning *client closed request*: the peer in
front of a hop abandoned the request before the response completed. For a
multi-GB blob `PATCH` that streams for minutes, the abandonment is a **timeout
firing somewhere in the proxy chain**, not a registry fault — the registry
stays up and small pushes succeed.

The previous CI mitigation (`GODEBUG=http2client=0`, commit `18e91b03`) fixed
the earlier HTTP/2 `502 Bad Gateway` failures, and `--push-retry=5` covers
transient errors — but a deterministic timeout is not transient: every retry
re-uploads the same oversized blob into the same too-short window and dies the
same way. That is why the warnings repeat and the job still fails.

Two timeout layers are the realistic suspects, in order:

1. **Traefik `websecure` entrypoint `respondingTimeouts`** — `readTimeout`
   bounds the *total* time to read the client request body. If set (some
   Traefik packages/distributions ship non-zero defaults), a long blob upload
   is aborted mid-stream. The omnibus nginx in front of the registry then logs
   `499` because its client (Traefik) went away.
2. **omnibus `registry_nginx`** — `client_body_timeout` (nginx default **60
   s** between successive body reads) and `proxy_read_timeout`/`proxy_send_timeout`.
   A slow runner uplink stretching a multi-GB layer read past one of these
   aborts the upload at the nginx → registry leg.

## Fix — apply on the GitLab host

### 1. Traefik: raise/disable responding timeouts for `websecure`

Static config (traefik.yaml or CLI flags on the container):

```yaml
entryPoints:
  websecure:
    transport:
      respondingTimeouts:
        readTimeout: "0s"     # no total-request deadline; or a generous 30m
        writeTimeout: "0s"
        idleTimeout: "180s"   # inactivity timeout only — keep
```

Caveat: this entrypoint serves every site behind this Traefik, so the change
is instance-wide. `readTimeout: 0` / `writeTimeout: 0` mean "no timeout";
`idleTimeout` still protects against dead connections. Traefik must be
restarted/recreated for static config to take effect.

Optionally, route-scoped hardening in `traefik-dynamic.yaml` (file provider,
hot-reloads): attach a `serversTransport` with generous forwarding timeouts to
the `gitlab-registry` service:

```yaml
http:
  serversTransports:
    registry-long-running:
      forwardingTimeouts:
        dialTimeout: "30s"
        responseHeaderTimeout: "900s"
        idleConnTimeout: "900s"
  services:
    gitlab-registry:
      loadBalancer:
        serversTransport: registry-long-running
        servers:
          - url: "http://gitlab:5050"
```

(Field names above are Traefik v3; check against the deployed version with
`traefik version` + the v2/v3 static/dynamic config reference if a key is
rejected — an invalid key makes Traefik reject the whole dynamic file.)

### 2. Omnibus: raise registry_nginx body/proxy timeouts

In the GitLab container's `GITLAB_OMNIBUS_CONFIG` (same place
`registry_external_url` lives, see `GITLAB_CONTAINER_REGISTRY.md` §1c):

```ruby
registry_nginx['client_body_timeout'] = 3600
registry_nginx['proxy_read_timeout'] = 3600
registry_nginx['proxy_send_timeout'] = 3600
```

Recreate/restart the container so omnibus regenerates the registry nginx vhost.

### 3. Inspect before/after (on the GitLab host)

```bash
# current effective registry nginx timeouts
docker exec gitlab grep -rE 'client_body_timeout|proxy_read_timeout|proxy_send_timeout' \
  /var/opt/gitlab/nginx/conf/

# Traefik static config in effect
docker inspect traefik --format '{{json .Args}}'    # or read its static yaml

# during a failing push, watch which hop records the 499
docker logs -f traefik                               # access log status codes
docker exec gitlab tail -f /var/log/gitlab/nginx/gitlab_registry_access.log
```

Whichever component logs the 499 first is where the upstream peer disappeared;
the timeout to raise belongs to the component *in front of* it.

## Verification

1. Re-run the manual `worker-opencode-image` job (largest image).
   Expect: no `499` retries in the push phase.
2. If pushes still fail after both layers are raised, replay the manual job
   with `WORKER_IMAGE_NAME=worker-opencode-2080` (fresh repository) to rule
   out a wedged upload session/tag, per the registry doc's housekeeping notes.

## Rollback

Both changes are config-only: remove the added lines and restart/recreate the
respective container. No data or registry content is touched.

## CI-side state (this repo)

- `GODEBUG=http2client=0` (forces HTTP/1.1 — GitLab's documented workaround
  for the earlier HTTP/2 502s) and `--push-retry=5` remain in place.
- Worker images now push an immutable `$CI_COMMIT_SHORT_SHA` tag next to the
  moving `v2.0.8.0`/`:latest` tags so consumers can pin (see
  `GITLAB_CONTAINER_REGISTRY.md` §2). Enable the server-side cleanup policy
  (§4) so SHA tags get pruned automatically.
- Nothing else client-side can cure a deterministic server timeout — do not
  keep bumping `--push-retry`; it re-uploads whole blobs each time.
