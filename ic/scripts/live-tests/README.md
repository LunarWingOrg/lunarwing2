# DarkIRC Stage A H3 Live Validation

`darkirc-stage-a-h3.sh` is destructive host validation, not CI test. It restarts two existing DarkIRC-enabled tenants and installs one deferred Stage A contact on each tenant.

## Run

Use real multi-tenant host booted with systemd user managers or OpenRC. Both tenants must already exist, use distinct Unix users/groups, have active DarkIRC daemon and adapter services, and have compatible pinned release.

```bash
sudo ic/scripts/live-tests/darkirc-stage-a-h3.sh \
  --i-understand-this-restarts-live-tenants TENANT_A TENANT_B
```

Alternate mt-admin path:

```bash
sudo ic/scripts/live-tests/darkirc-stage-a-h3.sh \
  --i-understand-this-restarts-live-tenants \
  --mt-admin /path/to/lunarwing-mt-admin.sh TENANT_A TENANT_B
```

## Safety Contract

- Fails before restart unless root, explicit acknowledgement, tools, accounts, release attestation, init system, user managers/services, and peer isolation checks pass.
- Creates root-owned mode `0700` captures under `/run/lunarwing-darkirc-h3/` with `umask 077`.
- Creates separate tenant-owned mode `0700` exchange directories. Root analyzer copies only validated public JSON between them using no-follow opens and atomic destination writes.
- Captures every mt-admin command stdout/stderr, descendant `/proc` argv/environment bytes, and post-restart systemd user journals or OpenRC daemon/adapter logs. `strace` delays each descendant `execve` long enough for sampling; the run fails unless mt-admin, helper, and measured daemon markers are present in `/proc` captures.
- Extracts private-key canaries from tenant state only inside root-only analyzer. Reports surface, pass/fail, file count, and match count; never prints canary values.
- Deletes raw captures and temporary exchange directories on every normal exit or trapped signal. Do not redirect, archive, or publish harness internals as CI artifacts.

Known limits: exact-value canary scanning detects full private-key disclosure, not transformed, truncated, encoded, hashed, kernel-level, network, or external logger disclosure. Non-exec helper processes outside the required coverage markers may be shorter than the polling interval. OpenRC captures configured daemon/adapter files; external syslog sinks remain outside capture scope. Harness leaves intentionally installed Stage A contacts and associated durable ledger/tombstone state.
