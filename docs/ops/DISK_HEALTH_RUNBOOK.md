# Disk Health Operator Runbook

The `disk` health component watches block and inode capacity on the host and on
tenant container stores. It is **monitor-only**: self-heal sends the normal
Gotify escalation as `disk@<mount>`, but never prunes images, removes files, or
restarts a service for this component.

## What is monitored

`health-disk.sh` watches `/`, `/tmp`, `/var/tmp`, the LunarWing base directory,
the configured root container store, every registry tenant's
`~/.local/share/containers/storage`, and paths in `HEALTH_DISK_EXTRA_PATHS`.
Tenant homes come from the ports registry's `user` field followed by `getent`;
no init-specific or `podman info` call is made. Paths on the same filesystem are
reported together once.

The defaults are:

- degraded at 80% blocks or inodes used, or below 5 GiB free;
- critical at 90% blocks or inodes used, or below 2 GiB free.

Override them with `HEALTH_DISK_DEGRADED_USED_PCT`,
`HEALTH_DISK_CRITICAL_USED_PCT`, `HEALTH_DISK_DEGRADED_FREE_GIB`, and
`HEALTH_DISK_CRITICAL_FREE_GIB` in `/etc/lunarwing/health.env`. Each `df` and
`stat` probe is bounded by `HEALTH_DISK_TIMEOUT` (3 seconds by default). An
unreadable path, a malformed probe result, or a timeout is degraded rather than
silently healthy.

For a colocated GitLab runner, add its rootless store to
`HEALTH_DISK_EXTRA_PATHS`, for example
`HEALTH_DISK_EXTRA_PATHS=/home/gitlab-runner/.local/share/containers/storage`.
Use `HEALTH_DISK_DOCKER_ROOT` when Docker's data root is not `/var/lib/docker`.

## Triage

1. Read the alert's mount and the component's `issues[]` and
   `metrics.filesystems[]`. The entry lists all watched paths on that
   filesystem, its free bytes, and its block and inode percentages.
2. Confirm both kinds of capacity from the host:

   ```bash
   df -hP /path/from/the/report
   df -iP /path/from/the/report
   ```

3. Check whether the affected path belongs to root, a tenant, or another
   operator-managed account such as `gitlab-runner`. Perform every cleanup as
   that account and only after checking for active builds and containers.
4. Re-run `ic-infrastructure-health-check/health-disk.sh` after cleanup. Its
   non-zero exit is the health verdict, not a command failure.

Inode exhaustion usually means too many small files or overlay layers even
when `df -h` still shows free blocks. Clearing space on a different filesystem
does not clear the alert.

## Manual cleanup choices

These are operator actions, not steps for the health pipeline to automate.

### Container storage

Inspect usage first with `podman system df` and `podman images` as the affected
user. For a rootless tenant, run commands through `sudo -u <tenant-user>` with
that user's normal rootless environment. After identifying unused containers,
images, and build cache, consider:

```bash
sudo -u <tenant-user> podman system prune
sudo -u <tenant-user> podman image prune
```

Review the prompts. Do not add `--all`, `--volumes`, or `--force` unless the
operator has separately confirmed that the additional deletion is acceptable.
For rootful Podman, use the host's normal root administration path instead of a
tenant account. Docker hosts require the equivalent Docker cleanup as the
Docker-owning account.

A GitLab runner is not managed by this repository. If it shares the host, its
operator can schedule a separate pruning policy, such as a daily
`podman system prune --filter until=72h` as `gitlab-runner`, after validating
that the retention period preserves needed build cache.

### Stale Cargo target directories

Identify old tenant or CI worktrees whose builds have stopped. As the directory
owner, prefer `cargo clean --manifest-path /path/to/Cargo.toml` for a stale
worktree, or remove its confirmed-stale `target/` directory through the normal
tenant cleanup process. Never clean a target directory while a build, test, or
tenant upgrade may be using it.

### Buildah leftovers in `/var/tmp`

Look for stale `buildah*` temporary directories on the alerted filesystem and
verify their owner and age. Remove only leftovers associated with finished or
failed builds, as that owner. Do not delete a directory belonging to an active
Buildah/Podman operation, and do not recursively clear `/var/tmp`.

If space cannot be reclaimed safely, stop nonessential writers according to the
normal tenant/runbook procedure and escalate to the host operator. The disk
health component itself deliberately takes none of these actions.
