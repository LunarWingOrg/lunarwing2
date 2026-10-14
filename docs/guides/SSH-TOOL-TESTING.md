# SSH Delivery Testing Guide

Use these checks to validate the SSH paths described in [`../architecture/SSH_DELIVERY_MECHANISMS.md`](../architecture/SSH_DELIVERY_MECHANISMS.md). Setup and recovery procedures live in [`../ops/SSH-HARNESS-SETUP.md`](../ops/SSH-HARNESS-SETUP.md).

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), and 2026-10-14 (v2.1.6.0).** The four delivery paths
(built-in `ssh`, `ssh_git`, WASM `ssh`, worker SSH), the `[[ssh.hosts]]`
allowlist, and the WASM `ssh.allowed_hosts` capability were re-checked against
the current tree; no changes were needed.

All SSH tools accept a host from the configured `[[ssh.hosts]]` allowlist. Replace `<ssh-host>` below with that exact configured host value; an arbitrary IP or hostname is rejected even when it is reachable.

## Prerequisites

- The tenant was provisioned without `--no-ssh`, or the harness was configured manually.
- `config.toml` contains at least one `[[ssh.hosts]]` entry.
- The encrypted secrets store contains an Ed25519 or ECDSA private key for the host. RSA is not supported by the in-process client.
- The daemon was restarted after a key upload so the signing agent loaded the key.
- The configured host-key policy is satisfied. Prefer `Strict` with a pinned `known_host_key` for sensitive systems.
- For a multi-tenant host, `sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>` reports the tenant healthy.

If the SSH harness HTTP API is enabled, its `/agent/status` response should report `running: true` and at least one loaded key. Treat the response as operational metadata and do not publish it unchanged.

## 1. Built-in `ssh`

Request a harmless command:

```text
Use the ssh tool with host <ssh-host> to run "hostname && whoami && date". Show stdout, stderr, and the exit code.
```

Expected behavior:

- the first action requires approval and offers **Always**;
- the reported host matches the configured allowlist entry;
- the command exits successfully with bounded stdout and stderr;
- the remote username and hostname match the configured target.

Selecting **Always** after reviewing a harmless command suppresses later
built-in `ssh` prompts for that user, across all configured aliases. Verify a
second harmless call runs without a prompt, restart LunarWing, then verify the
grant still applies. Set `tool_permissions.ssh` back to `ask_each_time` and
verify the next call prompts again. This grant does not apply to `ssh_git`,
WASM SSH, autonomous jobs, or supervised mode.

The built-in tool runs the SSH client in the LunarWing process and verifies the server key through the configured host-key policy.

For an isolated AcceptFirst validation, use a disposable SSH server whose host
key you can rotate. Make one approved connection, restart LunarWing, and repeat
the harmless command: it must succeed without relearning trust. Then stop the
server, replace only its host key, and retry the same configured host/port. The
built-in `ssh` and WASM `ssh` paths must report a host-key mismatch. `ssh_git`
must fail its host-key check before Git authentication or repository mutation.
Restore the original server key after the test. Do not rotate a production
server merely to exercise this case.

## 2. Built-in `ssh_git`

Use a disposable repository that the configured SSH identity can read. Prefer a read-only clone test before testing writes:

```text
Use ssh_git with operation clone, host <ssh-host>, repo <repo-relative-to-ssh-home>, path ssh-test-clone, ref main, and depth 1. Report the result without modifying or pushing the repository.
```

Expected behavior:

- the action requires approval;
- `path` is resolved below `<base_dir>/ssh-git/`;
- absolute paths, `..`, symlink escapes, and option-injection inputs are rejected;
- authentication uses the harness agent socket and the clone completes without an interactive prompt;
- on the first unpinned AcceptFirst call, logs show a russh host-key preflight
  before Git; subsequent calls and calls after a daemon restart reuse the
  encrypted pin, while OpenSSH always runs with strict host-key checking.

Only test `push` against an explicitly disposable remote. Force-push refs are rejected, and `pull` is fast-forward only.

## 3. WASM `ssh`

Build and install the registry `ssh` extension, then confirm its capability sidecar allows `<ssh-host>`. MT admin patches the installed allowlist from the tenant's `[[ssh.hosts]]` entries.

```text
Use the WASM ssh tool with host <ssh-host> to run "uname -a && id". Show stdout, stderr, and the exit code.
```

Expected behavior:

- the action requires approval;
- both the global SSH host allowlist and the WASM tool's `ssh.allowed_hosts` capability permit the host;
- key material remains host-side;
- the one-shot command succeeds without PTY or interactive input.

The WASM guest is sandboxed, but the actual SSH operation runs in the host function inside the LunarWing process. Do not claim that WASM isolates the network operation itself.

## 4. Worker SSH

Create a disposable Pebble, OpenCode, or Kimi worker job that runs a read-only SSH command using the injected `SSH_AUTH_SOCK`. Confirm the worker performs normal OpenSSH host-key verification; the in-process `HostKeyVerifier` is not used in this path.

Use worker mode for multi-step jobs where container isolation matters. A successful built-in `ssh` test does not prove the worker container has a correct `known_hosts` configuration.

## Cleanup and Evidence

Remove disposable clones and repositories through the same tenant context that created them. Record the LunarWing commit, configured host alias, mechanism tested, approval result, exit status, and a short redacted log window. Authentication log locations vary by distribution and init system; use the host's supported journal or auth log instead of assuming `/var/log/auth.log` exists.
