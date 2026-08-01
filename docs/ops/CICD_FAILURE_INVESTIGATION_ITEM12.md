# CI/CD Failure Investigation — item #12 (v2.0.4.0 goals)

Date: 2026-08-01. Branch: `aj/kimi-12-20260801-0304` (HEAD `f1c02265`).
Investigated jobs: `python-syntax`, `registry-json`, `shell-syntax`,
`version-policy`, `wasm-channel-test-artifacts`.

## Verdict (short)

The suspicion in item #12 is correct: with one nuance, these were **not test
defects**.

- `python-syntax`, `registry-json`, `shell-syntax`, and
  `wasm-channel-test-artifacts` only ever failed as **`runner_system_failure`**
  — the connected docker-executor runner could not pull the pinned docker.io
  images (Docker Hub unauthenticated pull rate limit). This is the same error
  quoted in item #14.
- `version-policy` additionally had real **`script_failure`** runs — but the
  script was working as designed: it caught feature branches that changed
  `channels-src/`/`tools-src/` without bumping the matching registry JSON
  versions. Those bumps have since landed.

## Evidence

### 1. All four lint/validate scripts pass locally on `f1c02265`

```
python3 .gitlab/scripts/python_syntax.py    -> Validated Python syntax in 97 tracked files. (exit 0)
python3 .gitlab/scripts/registry_json.py    -> Validated 12 registry JSON files. (exit 0)
bash    .gitlab/scripts/shell-syntax.sh     -> Validated Bash syntax in 156 scripts and 3 hooks. (exit 0)
bash    .gitlab/scripts/version-policy.sh   -> All version checks passed. (exit 0)
```

### 2. All five jobs are green on pipeline 755 — the exact commit investigated

Pipeline **755** (`ruffles/issue12f/v2.0.4.0`, sha `f1c02265`, 2026-07-31) ran
every named job successfully: `shell-syntax` 14089, `python-syntax` 14091,
`registry-json` 14092, `version-policy` 14093, `wasm-channel-test-artifacts`
14098. (That pipeline failed only in `secret-detection` — outside this item's
scope.)

### 3. Transient failures: Docker Hub rate limit (`runner_system_failure`)

Job traces (e.g. 14047 pipeline 752, 12933 pipeline 693) all end the same way:

```
Using effective pull policy of [always] for container docker.io/library/python:3.12-slim-bookworm@sha256:d50fb76...
ERROR: Job failed: failed to pull image ... toomanyrequests: You have reached
your unauthenticated pull rate limit. (manager.go:343:124s)
```

Affected runs found (2026-07-30/31), runner `cablemanagement`
(gitlab-runner 19.2.1, docker executor):

| Pipeline | Job(s) hit |
|---|---|
| 752 | `python-syntax`, `registry-json`, `version-policy` (python image) |
| 694 | `version-policy` |
| 693 | `wasm-channel-test-artifacts` (rust image) |

Contributing factors:

- Both images are pinned in `.gitlab/ci/defaults.yml` and pulled from
  `docker.io` anonymously (100 pulls / 6 h / IP limit).
- The runner's effective pull policy is **`always`**, so every job contacts
  Docker Hub even when the image is already cached locally. Note the digest
  pinned in `defaults.yml:37` is byte-identical to the one in item #14's error.
- `defaults.yml` retries `runner_system_failure` once, but the rate-limit
  window outlasts the retry, so the retry fails the same way.

### 4. Real `version-policy` script failures — policy worked as designed

Pipelines 572 (`runc/mcr/v2.0.3.0`), 658/660 (MRs 171/173), 662/674
(`aragents/kimil00p/v2.0.4.0`), 666 (`fixvc/aragents/kimil00p/v2.0.4.0`) failed
`version-policy` with genuine missing-bump violations, e.g.:

```
ERROR: registry/channels/multica.json version was not bumped (0.1.1 -> 0.1.1).
ERROR: registry/channels/xmpp.json version was not bumped (0.1.2 -> 0.1.2).
ERROR: registry/tools/vision-analyze.json version was not bumped (0.1.0 -> 0.1.0).
```

These were defects **on those branches**, not in the check. Verified fixed in
the current tree: `multica.json` 0.1.2, `xmpp.json` 0.1.3, `vision-analyze.json`
0.1.1.

`shell-syntax` never failed in any pipeline scanned (755 back to 409) — its only
non-success states were `canceled` (downstream of other failures).

## Recommendations (runner/infra — overlaps item #14, not done here)

1. Set `pull_policy = "if-not-present"` in the runner's `config.toml` (images
   are digest-pinned, so freshness guarantees are unaffected).
2. Or configure Docker Hub credentials / a registry mirror on the runner host.
3. Or mirror the two pinned images into the instance registry
   (`registry.git.lunarwing.org`, see `docs/ops/GITLAB_CONTAINER_REGISTRY.md`)
   and point `.lint-job`/`.rust-job` at the mirror.

No repo-side test changes are warranted by this investigation.
