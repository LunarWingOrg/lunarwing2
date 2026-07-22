# LunarWing MT Onboard — Web Edition

A localhost, browser-based front end for the LunarWing multi-tenant onboarding
tool. It provides five operations — **provision**, **secrets**, **upgrade**,
**export**, and **import** — behind a LunarWing-styled UI with live-streamed
logs, moon-phase progress, and an animated bat mascot.

It does **not** re-implement any provisioning logic: it imports and drives the
existing `lunarwing_mt_onboard` package (`provisioner.provision`,
`upgrade.run_upgrade`, `export.run_export`, `import_tenant.*`,
`secrets_ops.*`), which in turn calls `ic/scripts/lunarwing-mt-admin.sh` and the
Kawarimi import/export scripts.

## Quick start (demo — no sudo, no system changes)

```bash
./lunarwing_mt_onboard_web/run.sh --demo
```

Then open the printed URL, e.g. `http://127.0.0.1:1969/?token=…`.

Demo mode swaps the active `LUNARWING_*` script paths for bundled fake scripts
that emit realistic phased output with pauses, so you can exercise the entire
UI (all five flows, progress, logs, mascot, moon) safely. Tip: to make a demo
build fail (and see the angry bat), use a tenant name containing `fail`.

## Real provisioning (needs root)

```bash
sudo ./lunarwing_mt_onboard_web/run.sh
```

Real mode drives `lunarwing-mt-admin.sh`, which creates OS users, containers,
and services — so it requires `sudo`. `build-tenant` can take 20–40 minutes;
the UI streams logs and shows progress throughout.

Kawarimi import is dry-run and stage-only by default. Applying an import does
not start the restored tenant unless **Start restored tenant** is also checked;
unattended start requires the operator to confirm that the old host is stopped.
Encrypted `.7z` imports require the bundle passphrase even for dry-run because
the metadata must be decrypted before validation. A real export requires a
confirmed passphrase of at least 12 characters before it can stop services.

The upgrade form drives `lunarwing-mt-admin.sh upgrade-tenant`, accepts a branch,
tag, or commit such as `v2.0.2.0`, and applies immediately after explicit
confirmation. Backup and service-unit rendering remain enabled by default.

## Options

| Flag | Default | Meaning |
|------|---------|---------|
| `--port N` | `1969` (or `$LUNARWING_ONBOARD_WEB_PORT`) | Bind port |
| `--host H` | `127.0.0.1` | Bind host (loopback only is strongly recommended) |
| `--demo` | off | Use bundled fake scripts; no sudo, no system changes |
| `--log-dir DIR` | `/var/log/lunarwing-mt-onboard-web` or package `logs/` | Audit log location |
| `--no-token` | off | Disable the session token (not recommended) |
| `--allow-insecure-remote` | off | Permit real mode outside loopback without TLS (dangerous; prefer an SSH tunnel) |

## Security

- Binds `127.0.0.1` only.
- A random **session token** is generated at startup and embedded in the launch
  URL; every REST and WebSocket call must present it. Because this app can
  create OS users and run privileged scripts, do not expose it beyond loopback.
- Real mode refuses non-loopback binds unless the operator supplies the explicit
  `--allow-insecure-remote` override. The application does not terminate TLS.
- Kawarimi passphrases are sent only in POST bodies, held per job, passed to the
  shell through an anonymous descriptor, and fed to `7z` over stdin. They are
  excluded from model serialization, subprocess argv, streamed output, and logs.
- Only one privileged job may run at a time. A concurrent request receives HTTP
  `409` instead of racing tenant lifecycle or port-registry mutations.

## Audit logs

Every underlying invocation is recorded to a per-run log file (command line,
every output line, phase results, duration). Secret values — master keys,
passwords, API keys, secret payloads — are redacted and never written to disk.
Audit files are created and enforced as mode `0600`.

## Architecture

- **Backend:** FastAPI + Uvicorn. `models.py` maps request bodies to the reused
  dataclasses; `runner.py` drives each phase via the reused arg-builders,
  streaming events; `jobs.py` runs the blocking work on a thread and bridges to
  an async WebSocket; `audit.py` handles redacted logging; `demo.py` wires the
  simulation.
- **Frontend:** vanilla HTML/CSS/JS (no build step), palette lifted from the
  gateway UI. `moon.js` renders the waxing SVG moon; `bat.js` is the mascot
  state machine; `progress.js` is the run panel; `wizard.js` builds the forms.

## Provisioning defaults

The provision wizard enables the following by default:

- SSH harness
- Host health pipeline
- WeeChat relay bootstrap (automatic relay configuration)

Unchecking **Automatically configure WeeChat relay** sends
`no_weechat_bootstrap: true`, which forwards `--no-weechat-bootstrap` to
`lunarwing-mt-admin.sh add-tenant`. This skips WeeChat command execution and
`relay.conf` generation but still writes the minimal `weechat.env` and renders
the WeeChat service units — so the services can start without relay
configuration. Use `configure-weechat-relay <tenant>` later to enable the
relay without re-running provisioning.

Import (Kawarimi) forms do not expose a WeeChat opt-out control.

The provision form also exposes current controls for the daemon LLM base URL
and Nanocode/OpenCode model and base-URL
overrides. The XMPP control customizes the identity; leaving it off uses the
default `<tenant>@xmpp.localhost` bridge rather than disabling XMPP.

## Requirements

Python 3.10+ and a venv with `fastapi`, Pydantic v2, `uvicorn`, `websockets`
(`run.sh` creates it automatically). Real `secrets` mode also needs
`cryptography` and `psycopg2-binary`, installed on demand.
The test suite additionally uses `httpx` through FastAPI's `TestClient`; it is
included in `requirements.txt`.
