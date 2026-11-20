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

| Job / template | Artifacts | `expire_in` | Why |
| --- | --- | --- | --- |
| `.worker-candidate` (pebble, opencode, kimi, opencode2 base) | dotenv, image metadata, inputs hash, `opencode2-rootfs-export` | **2 days** | Read only by the scan, the out-of-band exposure verification, and promotion in the same pipeline. |
| `worker-opencode2-candidate` | metadata plus `opencode2-rootfs.tar` | **2 days** | Rootfs tar ~250-280 MiB; pipeline-scoped. |
| `worker-lunardepth-candidate` | metadata plus `lunardepth-rootfs.tar` | **2 days** | Rootfs tar; pipeline-scoped. |
| `.worker-scan` | `worker-sbom.cdx.json`, `worker-vulnerabilities.json`, container-scanning report | **90 days** | Audit evidence for an image that a tenant may still be running. |
| `.worker-promotion` | `worker-image-evidence.json` | **1 year** | Release evidence: the durable record of what was published. |
| `python-unit`, `shell-harnesses` | JUnit reports | 7 days | Small test reports. |
| `wasm-channel-test-artifacts` | wasm channel binaries | 1 day | Build output only. |
| every other job | — | **7 days** via `default:artifacts:expire_in` | Backstop only. The validator still requires an explicit per-job value. |

GitLab applies `expire_in` per **job**, not per path: everything a job uploads
shares one expiry. That is why the candidate jobs keep their small metadata
with the tar for 2 days instead of splitting it out; anything that must outlive
the pipeline is re-uploaded by the scan (SBOM) or promotion (evidence) job,
which are cheap and small.

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

Agreed ceiling: **10 GiB** of job artifacts and job logs for project 35.

On or shortly after **2026-11-27**, record the current usage in WI#791:

- UI: **Settings > Usage Quotas > Storage** (Maintainer) shows `Job artifacts`
  and `Job logs` separately.
- API, with a Maintainer token:
  `curl -sS -H "PRIVATE-TOKEN: <maintainer>" "https://git.lunarwing.org/api/v4/projects/35?statistics=true"`
  and read `statistics.job_artifacts_size`.
- The project settings object (`GET /api/v4/projects/35`) also reports
  `keep_latest_artifact` and `ci_delete_pipelines_in_seconds`; both are part of
  this contract, so a flip back to `true` is a regression, not a preference.

The post-cleanup baseline was 9.66 GiB: about 5.26 GiB of artifacts from the
last two days plus 4.39 GiB of job logs. If the check lands above 10 GiB while
the artifact gate is green, the remaining gap is job logs; lower the pipeline
deletion window or archive traces at the instance level and record that
decision in WI#791 rather than loosening artifact expiries.
