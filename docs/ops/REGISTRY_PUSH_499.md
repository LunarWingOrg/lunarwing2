# Registry push failures: `499 Client Closed Request` on blob PATCH

Status: **diagnosed 2026-08-22, extended 2026-11-19 (WI#706); the server-side
fix must be applied on the GitLab host — it cannot be done from the dev box /
this repo.** CI-side retry mitigations are already in
`.gitlab/ci/containers.yml`. The interim runner mitigation for WI#706 is
applied (see “Artifact uploads” below). On 2026-11-19 the 60-second cut was
reproduced on demand from the dev box and the edge's own error page was
fingerprinted (see “Live confirmation” below); only the edge's access-log line
and the timeout change itself still need the host administrator.

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
| 178891 | 17 | 60 s, 61 s, 62 s | 499, 499, 499 |

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

### Live confirmation, 2026-11-19

The cut was reproduced on demand from the dev box with rate-limited uploads to
a throwaway private project's `/uploads` endpoint. No runner is involved, so
the result separates the edge from runner 17's uplink:

| Upload | Sent when cut | Elapsed | Result |
|---|---|---|---|
| 1 KiB control, unthrottled | 1 KiB | 0.82 s | `201 Created` |
| 12 MiB at 100 KiB/s | 6.0 MiB | 61.18 s | `502` |
| 12 MiB at 100 KiB/s, repeated | 6.0 MiB | 61.18 s | `502` |
| 25 MiB at 400 KiB/s | 23.8 MiB | 61.18 s | `502` |

Two different sizes at two different rates stop at the same 61.18 s, from a
third network, with a control upload passing in under a second: the limit is
time, not size, and it is not runner 17.

The response points to the edge. It is `HTTP/2 502`, `content-length: 11`,
body `Bad Gateway`, and **no `server` header**:

```
HTTP/2 502
content-length: 11
date: Thu, 19 Nov 2026 18:08:52 GMT

Bad Gateway
```

Normal responses from omnibus nginx carry `server: nginx` (for example, the
`https://git.lunarwing.org/` front page). The 11-byte `Bad Gateway` without
that header supports an edge-generated `502` for this probe. That fits
`entryPoints.websecure.transport.respondingTimeouts.readTimeout` (60 s default
since Traefik 2.11.2/3.0). The runner's `499`, `504` and resets line up with
the same timeout, but the runner trace alone does not identify which hop
produced a status. Omnibus nginx can log a `499` when Traefik, its client,
abandons the request; that log entry is distinct from what the runner receives.

Only the access-log line remains unseen, because the edge host has no shell
reachable from this box (SSH lands in GitLab-shell). WI#706 acceptance box 1
still needs that line from the host administrator; the pre-fix half of box 2
is now recorded above.

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
   artifact upload can be aborted mid-stream at that boundary. Omnibus nginx
   can then log `499` because Traefik closed the upstream request. The probe
   received an apparent edge `502`; the runner's `499`, `504` and resets have
   not been correlated with the edge and nginx logs for those requests.
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
        writeTimeout: "0s"    # Traefik's default; idleTimeout covers dead connections
        idleTimeout: "180s"   # inactivity timeout only — keep
```

`readTimeout` bounds reading the whole request including the body. `30m` is
the recommended value: it is well above the cuts observed (60 to 66 s; no
completed upload from runner 17 was timed) and is still finite. Nobody with
access to the edge has applied it yet. **Do not set it to `0`**: the timeout
is also the protection against a client that sends its body slowly.
`idleTimeout` still protects against dead connections.

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

# during a failing push, correlate the same request across the two logs
docker logs -f traefik                               # access log status codes
docker exec gitlab tail -f /var/log/gitlab/nginx/gitlab_registry_access.log
```

Compare the timestamps, request path, duration and status for the same upload.
With Traefik JSON access logs, distinguish `DownstreamStatus` (sent to the
client) from `OriginStatus` (outcome of the upstream request). An nginx `499`
means its client disconnected, not that nginx sent `499` to the runner or
that nginx's own timeout fired.

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
`container-build` to runner 17 only after the go/no-go probe in
“Verification” returns `201` through the edge (the probe makes the
`>60 s` test reproducible from any workstation; it does not have to come
from runner 17 itself, because the cut is at the shared edge entry point).

The edge host is not reachable from the dev box (SSH lands in GitLab-shell).
The Traefik config change and the access-log line therefore stay with the
host administrator. The *pre-fix* half of the rate-limited upload test has
been done from the dev box — see “Live confirmation” above.

## Verification

1. **The go/no-go probe** — a rate-limited upload through the edge, run from
   a workstation with Bash and curl 7.55.0 or newer. Use a dedicated, standing
   **private probe project** on this GitLab instance. Use its numeric project
   ID and a short-lived **project access token** with `api` scope and the least
   project role that permits uploads. Do not use an administrator's personal
   token. The probe uploads 12 MiB at 100 KiB/s (about 123 s of body). Run
   this block in a terminal; it prompts without echoing the token:

   ```bash
   (
     set +ax # Do not auto-export or trace the token.
     set -euo pipefail
     unset probe_token
     umask 077
     probe_id='REPLACE_WITH_PRIVATE_PROBE_PROJECT_ID'
     [[ "$probe_id" =~ ^[1-9][0-9]*$ ]] || { echo 'Set the numeric probe project ID.' >&2; exit 1; }

     scratch=$(mktemp -d)
     trap 'rm -f -- "$scratch/probe.bin" "$scratch/probe.out"; rmdir -- "$scratch"' EXIT
     CDPATH='' cd -- "$scratch"
     scratch=$PWD
     head -c 12582912 /dev/urandom > probe.bin

     IFS= read -rs -p 'Probe project access token: ' probe_token </dev/tty
     printf '\n' >&2
     [[ -n "$probe_token" && "$probe_token" != *[[:space:][:cntrl:]]* ]] || exit 1
     builtin printf 'PRIVATE-TOKEN: %s\n' "$probe_token" |
       curl --disable --silent --show-error --limit-rate 100k --max-time 280 \
         --header @- --form 'file=@probe.bin' --output probe.out \
         --write-out 'status=%{http_code} elapsed=%{time_total}s sent=%{size_upload}B\n' \
         "https://git.lunarwing.org/api/v4/projects/$probe_id/uploads"
   )
   ```

   The token goes to curl on standard input, not in its arguments or a file.
   Local scratch files are removed on exit. Successful uploads stay in the
   probe project; have a project Maintainer periodically remove them via the
   [Markdown uploads API](https://docs.gitlab.com/api/project_markdown_uploads/).

   **Before the fix** this ends at about 61 s with `status=502` and `sent`
   well under 12 MiB (measured 2026-11-19: 61.18 s, 6.0 MiB). **After the
   fix** it must trickle the whole 12 MiB, end with `status=201`, and take
   about 125 s. A cut at ~61 s means the timeout is still live; a completed
   upload in under 90 s means the probe never crossed the boundary, so raise
   the file size. This exercises the same `websecure` entry point as artifact
   uploads and registry pushes — Traefik applies `readTimeout` per entry
   point, not per router, so one probe covers both hosts. Record the edge
   access-log line for the same request with its duration and status (WI#706
   acceptance box 1); the pre-fix line is not obtainable from the dev box.
2. Re-run the manual `worker-opencode-image` job (largest image).
   Expect: no `499` retries in the push phase.
3. Re-run `worker-opencode2-candidate` and confirm it can pass on runner 17
   again — only after step 1 succeeds. Otherwise leave runner 17 off
   `container-build`.
4. If pushes still fail after both layers are raised, replay the manual job
   with `WORKER_IMAGE_NAME=worker-opencode-2080` (fresh repository) to rule
   out a wedged upload session/tag, per the registry doc's housekeeping notes.

## Open items (WI#706, as of 2026-11-19)

- **Edge access-log line** for one cut upload (duration ~61 s, status
  `499`/`502`/`504`). Only the edge host has it; the dev box cannot reach a
  shell there.
- **Apply the timeout** on the `websecure` entry point that serves both hosts:
  `readTimeout: "30m"` — static configuration, Traefik restart required. Do
  not use `0`.
- **Record the applied value and its date** beside the other edge settings
  (`GITLAB_CONTAINER_REGISTRY.md` §1f); the value there today is the
  recommendation, not the live setting.
- **Run the go/no-go probe** after the change (expect `201` in ~125 s).
- **Re-add `container-build` to runner 17** only after the probe passes.

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
