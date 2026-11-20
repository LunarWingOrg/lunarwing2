# GitLab CI artifact retention (WI#791)

Job artifacts were 99.3% of this project's storage: 32.57 GiB of 32.8 GiB on
2026-11-20, from 60,138 artifact entries. They never expired in practice
because the project setting **Keep artifacts from most recent successful jobs**
was on, which keeps the latest artifact set of every ref regardless of
`expire_in`, and agents open one branch per work item. The largest entries were
the ~250-280 MiB worker rootfs tars from the `worker-*-candidate` jobs. A manual
cleanup removed artifacts older than two days on 2026-11-20 and took the project
to 9.66 GiB. This document records the retention rules that now keep it there.

## Rules

Audit of every `artifacts:` block before this change, and the value it has now
(`.gitlab-ci.yml` declares none; `security.yml` uses `artifacts: false` in
`needs` only):

| Declaration | Jobs covered | Artifacts | Before | Now | Why |
| --- | --- | --- | --- | --- | --- |
| `.worker-candidate` | pebble, opencode, opencode2, kimi candidates | dotenv, image metadata, inputs hash, `opencode2-rootfs-export` | 30 days | **2 days** | Read only by the scan, the out-of-band exposure verification, and promotion in the same pipeline. |
| `worker-opencode2-candidate` | opencode2 candidate | metadata plus `opencode2-rootfs.tar` | 30 days | **2 days** | Rootfs tar, ~250-280 MiB; pipeline-scoped. |
| `worker-lunardepth-candidate` | lunardepth candidate | metadata plus `lunardepth-rootfs.tar` | 30 days | **2 days** | Rootfs tar; pipeline-scoped. |
| `.worker-scan` | all four `worker-*-scan` jobs | `worker-sbom.cdx.json`, `worker-vulnerabilities.json`, container-scanning report | 30 days | **90 days** | Audit evidence for an image a tenant may still be running. |
| `.worker-promotion` | all four `worker-*-promote` jobs | `worker-image-evidence.json` | 1 year | **1 year** | The only durable record of what was published. Comment added; value unchanged. |
| `python-unit`, `shell-harnesses` | test reports | JUnit XML | 7 days | **7 days** | Small test reports. |
| `wasm-channel-test-artifacts` | wasm channel binaries | 2 `.wasm` files | 1 day | **1 day** | Build output only. |
| `default` | backstop for any job without its own value | — | none | **7 days** | New. Not a substitute for a per-job value. |

GitLab applies `expire_in` per **job**, not per path: everything a job uploads
shares one expiry. That is why the candidate jobs keep their small metadata
with the tar for 2 days instead of splitting it out; anything that must outlive
the pipeline is re-uploaded by the scan (SBOM) or promotion (evidence) job,
which are cheap and small.

Operational consequence: a manual `worker-*-promote` job downloads its
candidate's dotenv and metadata. Play it from the pipeline that produced the
candidate, within two days. If it has already expired, re-run the candidate and
then the scan; the promotion never rebuilds, so that is the only recovery.

`default:artifacts:expire_in: 7 days` in `.gitlab/ci/defaults.yml` is a
backstop for a job that somehow ships without a value. It is not a substitute
for one.

## Enforcement

`ci-config` runs `.gitlab/scripts/validate_config.py`, whose
`validate_artifact_retention` step fails the pipeline when:

- a job or hidden template declares `artifacts:` without `expire_in`, or with
  one that is not a bounded duration (`never`, empty, misspelled);
- any job keeps artifacts longer than **1 year**;
- a job uploads a path containing `rootfs` and keeps it longer than **2 days**;
- a job uploads `worker-image-evidence.json`, `worker-sbom.cdx.json`, or
  `worker-vulnerabilities.json` for less than **90 days**;
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
  2592000`). This is the only thing that bounds job logs.

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

Ceiling: **10 GiB** of job artifacts for project 35 (the value suggested in
WI#791; artifacts are the surface this change controls, and `job_artifacts_size`
in the API includes job logs).

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

1. **Re-run the one-off cleanup once this is live.** Delete non-log artifacts
   older than two days again, as was done on 2026-11-20. Everything left is then
   post-change-shaped, the 2-day expiries hold the line, and the check passes.
   This is the intended path.
2. **Record the number and the reason.** If nobody re-runs it, storage stays
   above 10 GiB until the pre-change 30-day artefacts age out, and the check
   cannot pass however good the gate is. Record that in WI#791 rather than
   loosening any `expire_in`.

On or shortly after **2026-11-27**, record the number in WI#791 with:

- UI: **Settings > Usage Quotas > Storage** (Maintainer), which splits
  `Job artifacts` from `Job logs`.
- API: the command above; `statistics.job_artifacts_size` is the total.

The same `GET /api/v4/projects/35` response reports `keep_latest_artifact` and
`ci_delete_pipelines_in_seconds`. Both are part of this contract, so a flip back
to `keep_latest_artifact: true` is a regression, not a preference.

If the check lands above 10 GiB with the artifact gate green and the pre-change
artifacts already cleaned, the remaining gap is **job logs**: lower the pipeline
deletion window or archive traces at the instance level and record that decision
in WI#791 rather than loosening artifact expiries. The post-cleanup split was
about 5.26 GiB of artifacts and 4.39 GiB of logs.
