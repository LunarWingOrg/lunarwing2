# GitLab CI artifact retention (WI#791)

Job artifacts were 99.3% of this project's storage: 32.57 GiB of 32.8 GiB on
2026-11-20, from 60,138 artifact entries. They never expired in practice
because the project setting **Keep artifacts from most recent successful jobs**
was on, which keeps the latest artifact set of every ref regardless of
`expire_in`, and agents open one branch per work item. The largest entries were
the ~250-280 MiB worker rootfs tars from the `worker-*-candidate` jobs. A manual
cleanup on 2026-11-20 deleted non-log artifacts older than two days and took the
project to 9.66 GiB; it also deleted the scan and promotion artifacts of every
release pipeline older than two days, and promotion never rebuilds, so that
evidence cannot be made again. This document records the retention rules that
now keep storage down, and the cleanup rule below supersedes that one-off.

## Rules

Audit of every `artifacts:` block before this change, and the value it has now
(`.gitlab-ci.yml` declares none; `security.yml` uses `artifacts: false` in
`needs` only):

| Declaration | Jobs covered | Artifacts | Before | Now | Why |
| --- | --- | --- | --- | --- | --- |
| `.worker-candidate` | pebble, opencode, kimi candidates | dotenv, image metadata, inputs hash, `opencode2-rootfs-export` | 30 days | **2 days** | Read only by the scan, the out-of-band exposure verification, and promotion in the same pipeline. opencode2 and lunardepth declare their own blocks. |
| `worker-opencode2-candidate` | opencode2 candidate | metadata plus `opencode2-rootfs.tar` | 30 days | **2 days** | Rootfs tar, ~250-280 MiB; pipeline-scoped. |
| `worker-lunardepth-candidate` | lunardepth candidate | metadata plus `lunardepth-rootfs.tar` | 30 days | **2 days** | Rootfs tar; pipeline-scoped. |
| `.worker-scan` | all five `worker-*-scan` jobs (pebble, opencode, opencode2, lunardepth, kimi) | `worker-sbom.cdx.json`, `worker-vulnerabilities.json`, container-scanning report | 30 days | **30 days** | Audit evidence for an image a tenant may still be running. 30 days is the platform ceiling, not a shortening: see below. |
| `.worker-promotion` | all five `worker-*-promote` jobs | `worker-image-evidence.json` | 1 year | **30 days** | The only durable record of what was published, and it lives exactly as long as its pipeline. |
| `python-unit`, `shell-harnesses` | test reports | JUnit XML | 7 days | **7 days** | Small test reports. |
| `wasm-channel-test-artifacts` | wasm channel binaries | 2 `.wasm` files | 1 day | **1 day** | Build output only. |
| `default` | no job in practice | — | none | **7 days** | Backstop only; it does not reach a job with its own `artifacts:` block (see below). |

GitLab applies `expire_in` per **job**, not per path: everything a job uploads
shares one expiry. That is why the candidate jobs keep their small metadata
with the tar for 2 days instead of splitting it out; anything that must outlive
the candidate job is re-uploaded by the scan (SBOM) or promotion (evidence)
job, which are cheap and small.

Evidence is kept **30 days at most**, whatever `expire_in` says. The project
setting **Delete pipelines after** is 30 days, and when GitLab deletes a
pipeline it deletes its jobs, logs and artifacts with it. The scan and
promotion jobs declare 30 days because that is the ceiling the platform
delivers; a longer value would promise time that does not exist. Measured on
2026-11-20, the oldest pipeline of the project was exactly 30 days old.

Operational consequence: a manual `worker-*-promote` job downloads its
candidate's dotenv and metadata. Play it from the pipeline that produced the
candidate, within two days. If it has already expired, re-run the candidate and
then the scan; the promotion never rebuilds, so that is the only recovery.

`default:artifacts:expire_in: 7 days` in `.gitlab/ci/defaults.yml` is a
backstop, but a narrow one. GitLab does not merge `default` into a job that
defines the same keyword: a job with its own `artifacts:` block and no
`expire_in` gets the instance expiry, not this value. The default reaches only
jobs that have no `artifacts:` block at all, and those upload nothing. The
control that matters is the gate below.

## Enforcement

`ci-config` runs `.gitlab/scripts/validate_config.py`, whose
`validate_artifact_retention` step fails the pipeline when:

- a job or hidden template declares `artifacts:` without `expire_in`, or with
  one that is not a bounded duration (`never`, empty, misspelled);
- any job keeps artifacts longer than **1 year**;
- a job uploads a path containing `rootfs` and keeps it longer than **2 days**;
- a job uploads `worker-image-evidence.json`, `worker-sbom.cdx.json`, or
  `worker-vulnerabilities.json` for less than **30 days**, the longest a
  pipeline lives;
- `.gitlab/ci/defaults.yml` loses the `default:artifacts:expire_in` backstop.

Values are resolved through `extends`, so an inherited expiry counts only when
GitLab would actually apply it. The gate is covered by
`.gitlab/tests/test_artifact_retention.py`. A deliberate retention change edits
the YAML and the expected values in that test together.

## Project settings

Changed on the GitLab host on 2026-11-20 for WI#791:

- **Keep artifacts from most recent successful jobs**: **off**
  (`keep_latest_artifact: false`). Leaving it on overrides every `expire_in`
  for the newest job on each of the many per-work-item branches.

Checked, left unchanged:

- **Delete pipelines after**: 30 days (`ci_delete_pipelines_in_seconds =
  2592000`). This bounds everything: job logs, job artifacts and the jobs
  themselves. Nothing outlives its pipeline, whatever `expire_in` says.

New expiries apply only to jobs created after the change lands. Artifacts that
already exist keep the expiry they were created with, so storage falls
gradually rather than on merge.

## Job logs: left alone

Decision: keep job logs, do not add a scheduled cleanup.

- GitLab exposes no per-job trace expiry in `.gitlab-ci.yml`; traces are
  deleted with their pipeline, which the project already bounds at 30 days.
- A repo-side scheduled cleanup would need a Maintainer API token stored as a
  CI variable, which is a standing privilege escalation for a storage chore.
- If logs ever dominate again, the operator lever is the instance-level
  pipeline deletion / trace archiving settings on the GitLab host, not this
  repository.

## Ceiling and the 7-day check

Ceiling: **10 GiB** of job artifacts for project 35, the value suggested in
WI#791. The API's `job_artifacts_size` counts job logs too, so this single
number covers both surfaces the retention rules below control.

Measurement on 2026-11-20, after this change was written and before it merged:

```console
$ curl -sS -H "PRIVATE-TOKEN: <maintainer>" \
    "https://git.lunarwing.org/api/v4/projects/35?statistics=true" \
  | jq '.statistics.job_artifacts_size'
10971986940        # 10.22 GiB
```

That is **above** the ceiling, and the reason matters: the manual cleanup left
9.66 GiB, pipelines have added about 0.5 GiB since, and **every artifact created
before this change keeps the expiry it was created with** (30 days for the
candidate jobs). Those pre-change artifacts keep growing and do not start
expiring until 30 days after they were made. The new 2-day value only governs
what is produced from here on.

So the 7-day check has two legitimate outcomes:

1. **Record the number and the reason.** Storage stays above 10 GiB until the
   pre-change 30-day artifacts age out, and the check cannot pass however good
   the gate is. Record that on WI#805 rather than loosening any `expire_in`.
2. **Delete only artifacts whose own expiry has passed.** A cleanup must not
   delete "non-log artifacts older than two days": that removes the scan and
   promotion evidence of every release pipeline older than two days, and
   promotion never rebuilds, so the evidence cannot be made again. A cleanup
   must spare the files of every `worker-*-scan` and `worker-*-promote` job
   (`worker-sbom.cdx.json`, `worker-vulnerabilities.json`,
   `worker-image-evidence.json`) — in practice, delete nothing whose expiry has
   not passed.

On or shortly after **2026-11-27**, record the number on WI#805 with:

- UI: **Settings > Usage Quotas > Storage** (Maintainer), which splits
  `Job artifacts` from `Job logs`.
- API: the command above; `statistics.job_artifacts_size` is the total.

The same `GET /api/v4/projects/35` response reports `keep_latest_artifact` and
`ci_delete_pipelines_in_seconds`. Both are part of this contract, so a flip back
to `keep_latest_artifact: true` is a regression, not a preference.

If the check lands above 10 GiB with the artifact gate green and the pre-change
artifacts already cleaned, the remaining gap is **job logs**: lower the pipeline
deletion window or archive traces at the instance level and record that decision
on WI#805 rather than loosening artifact expiries. The post-cleanup split was
about 5.26 GiB of artifacts and 4.39 GiB of logs.
