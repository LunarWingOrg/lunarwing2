# Registry push failures: `499 Client Closed Request` on blob PATCH

Status: **diagnosed 2026-08-22, extended 2026-11-19 (WI#706); the server-side
fix must be applied on the GitLab host — it cannot be done from the dev box /
this repo.** CI-side retry mitigations are already in
`.gitlab/ci/containers.yml`. The interim runner mitigation for WI#706 is
applied (see “Artifact uploads” below).

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

## Artifact uploads: the same 60-second cut (WI#706, 2026-11-19)

The limit also cuts a runner's **job artifact** upload, not just registry blob
`PATCH`es. `worker-opencode2-candidate` uploads a 279 MiB
`opencode2-rootfs.tar`; from runner 17 (`laptop`, host `rainova43`, GitLab
Runner 19.5.1) the upload takes longer than 60 seconds and is cut on every
attempt:

| Job | Runner | Attempts (elapsed) | Result |
|---|---|---|---|
| 179088 | 17 | 60 s, 61 s, 60 s, 63 s | reset, 504, reset, 499 |
| 179011 | 17 | 60 s, 61 s, 62 s | 499, 499, 499 |
| 178891 | 17 | 60 s, 61 s, 60 s | 499, 499, 499 |

These are from the job traces (`POST /api/v4/jobs/<id>/artifacts`); an excerpt
from job 179088's attempts:

```
2026-11-15T00:01:11Z ERROR: Uploading artifacts as "archive" to coordinator...
write tcp 172.17.0.9:53410->104.36.84.181:443: write: connection reset by peer
2026-11-15T00:04:16Z WARNING: Uploading artifacts as "archive" to coordinator...
499 status code 499
```

The same archive passes from runners 13, 15 and 16 in 12–18 s. This is a time
limit, not a size limit: 279 MiB in >60 s is under 5 MB/s, while the other
runners reach 16–23 MB/s.

The 60-second cut matches **Traefik `respondingTimeouts.readTimeout`**, which
has defaulted to 60 seconds since Traefik 2.11.2 and 3.0. It bounds the whole
request, body included, and it is the component common to both
`git.lunarwing.org` (API/artifacts) and `registry.git.lunarwing.org` (blob
pushes). The WI asks first for one cut line from the edge access log naming
the proxy and its duration; that host is still unreachable from the dev box,
so the log evidence remains an administrator action.

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
   bounds the *total* time to read the client request body, and its default
   has been **60 seconds** since Traefik 2.11.2 and 3.0. A long blob or
   artifact upload is aborted mid-stream at that boundary. The omnibus nginx
   in front of the registry then logs `499` because its client (Traefik) went
   away, and the runner sees `499`, `504` or a connection reset.
2. **omnibus `registry_nginx`** — `client_body_timeout` (nginx default **60
   s** between successive body reads) and `proxy_read_timeout`/`proxy_send_timeout`.
   A slow runner uplink stretching a multi-GB layer read past one of these
   aborts the upload at the nginx → registry leg.

## Fix — apply on the GitLab host

### 1. Traefik: raise the `websecure` read timeout to a bounded value

Static config (traefik.yaml or CLI flags on the container):

```yaml
entryPoints:
  websecure:
    transport:
      respondingTimeouts:
        readTimeout: "30m"    # bounded — do NOT set 0
        writeTimeout: "0s"    # response writes are short; not the upload bound
        idleTimeout: "180s"   # inactivity timeout only — keep
```

`readTimeout` bounds reading the whole request including the body. `30m` is
well above the worst observed upload (66 s) and is still finite. **Do not set
it to `0`**: the timeout is also the protection against a client that sends
its body slowly. `idleTimeout` still protects against dead connections.

This entry point serves every site behind this Traefik, so the change is
instance-wide. Traefik sets the timeout per entry point, not per router. If
the public entry point must keep 60 seconds, add a dedicated CI entry point
with `readTimeout: "30m"` and an address allowlist for the runners. Traefik
must be restarted/recreated for static config to take effect.

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

## Interim mitigation — runner 17 (applied 2026-11-19)

Until the edge change is live, shared runner 17 no longer carries the
`container-build` tag. The tag was removed with the GitLab admin API:

```bash
curl -X PUT 'https://git.lunarwing.org/api/v4/runners/17' \
  -H "PRIVATE-TOKEN: <admin token>" \
  --data-urlencode 'tag_list=lint,rust-x86_64,general,docker'
```

- Before: `lint, rust-x86_64, container-build, general, docker`
- After: `lint, rust-x86_64, general, docker` (verified with `GET
  /api/v4/runners/17`)

Runner 17 keeps its other tags and still takes untagged jobs. The
`.worker-candidate` jobs in `.gitlab/ci/containers.yml` require
`container-build`, so runner 17 can no longer take the large-upload job;
runners 13, 15 and 16 still carry that tag (7 and 8 do as well). Re-add
`container-build` to runner 17 only after a rate-limited upload that takes
more than 60 seconds succeeds from it.

The edge host remains unreachable from the dev box: `ssh root@104.36.84.181`
returns `Permission denied (publickey)`, and SSH to `git.lunarwing.org` lands
in GitLab Shell. The Traefik config change, the access-log line, and the live
>60 s upload test therefore stay with the host administrator.

## Verification

1. Rate-limited upload through the edge: send a body that takes more than
   60 seconds (for example `curl --limit-rate` against a throwaway project's
   upload API or a scratch job artifact) and confirm it completes with `201`
   instead of `499`/`504`/reset. Record the edge access-log line with its
   duration and status.
2. Re-run the manual `worker-opencode-image` job (largest image).
   Expect: no `499` retries in the push phase.
3. Re-run `worker-opencode2-candidate` and confirm it can pass on runner 17
   again — only after step 1 succeeds. Otherwise leave runner 17 off
   `container-build`.
4. If pushes still fail after both layers are raised, replay the manual job
   with `WORKER_IMAGE_NAME=worker-opencode-2080` (fresh repository) to rule
   out a wedged upload session/tag, per the registry doc's housekeeping notes.

## Rollback

The Traefik/omnibus changes are config-only: remove the added lines and
restart/recreate the respective container. No data or registry content is
touched. The runner 17 mitigation is also reversible: add `container-build`
back to its tag list with the same `PUT /api/v4/runners/17` call.

## CI-side state (this repo)

- `GODEBUG=http2client=0` (forces HTTP/1.1 — GitLab's documented workaround
  for the earlier HTTP/2 502s) and `--push-retry=5` remain in place.
- Worker images now push an immutable `$CI_COMMIT_SHORT_SHA` tag next to the
  moving `v2.0.8.0`/`:latest` tags so consumers can pin (see
  `GITLAB_CONTAINER_REGISTRY.md` §2). Enable the server-side cleanup policy
  (§4) so SHA tags get pruned automatically.
- Nothing else client-side can cure a deterministic server timeout — do not
  keep bumping `--push-retry`; it re-uploads whole blobs each time.
