# lunarwing_mt_web

Web-based multi-tenant onboarding wizard for LunarWing. A browser-based alternative to the interactive `lunarwing_mt_onboard` CLI.

## Overview

Provides a visual step-by-step wizard for provisioning new LunarWing tenants through a web browser. Mirrors the full interactive CLI flow: tenant identity, network, channels (XMPP, Gotify, DarkIRC), worker selection, LLM configuration, secrets, and provisioning with live streaming output.

Under the hood it imports `lunarwing_mt_onboard` modules directly — no provisioning logic is duplicated.

## Features

- 9-screen wizard with LunarWing dark blue visual design
- Animated bat mascot with 4 emotions (content, angry, sleeping, excited)
- Moon phase indicator that progresses with wizard steps
- Live SSE-streamed build/provision output in a terminal-style log box
- Activity logging to `/var/log/lunarwing-mt-web.log`
- Real-time tenant name validation

## Requirements

- Python 3.10+
- `flask` (auto-installed by `launch.sh` if missing)
- Root/sudo (the underlying `mt-admin.sh` requires root)
- `lunarwing_mt_onboard` package (part of this repo)

## Quick start

```bash
sudo bash lunarwing_mt_web/launch.sh
```

Then open `http://127.0.0.1:7424` in your browser.

`launch.sh` checks for Flask (installs if missing), starts the server, and tries to open the browser via `xdg-open`.

## Configuration

| Environment Variable | Default | Description |
|---|---|---|
| `LUNARWING_WEB_PORT` | `7424` | Port for the web server |
| `LUNARWING_MT_ADMIN` | (auto-detected) | Path to `lunarwing-mt-admin.sh` |

## Module layout

```
lunarwing_mt_web/
├── __init__.py        # package marker
├── server.py          # Flask app: routes, API endpoints, SSE streaming
├── bridge.py          # Wraps lunarwing_mt_onboard provisioner + verification
├── launch.sh          # Entry point: checks deps, starts Flask, opens browser
├── tests.py           # unit tests
├── smoke_test.sh      # smoke test script
├── requirements.txt   # flask
├── templates/
│   └── index.html     # single-page wizard (9 screens)
└── static/
    ├── style.css      # LunarWing design system (dark theme)
    ├── app.js         # wizard logic, animations, SSE, mascot cycling
    ├── logo.svg       # LunarWing logo
    ├── favicon.svg    # browser tab icon
    └── bat/           # animated bat mascot sprites
        ├── content.svg
        ├── angry.svg
        ├── sleeping.svg
        └── excited.svg
```

## API endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/` | GET | Serve the wizard HTML page |
| `/api/validate/name` | POST | Validate a tenant name |
| `/api/provision` | POST | Start provisioning (returns session ID) |
| `/api/provision/<id>/stream` | GET | SSE stream of provisioning output |
| `/api/provision/<id>/status` | GET | Current provisioning status |
| `/api/verify/<tenant>` | GET | Run post-start verification |

## Testing

```bash
bash lunarwing_mt_web/smoke_test.sh
```

## Activity log

All `mt-admin.sh` invocations are logged to `/var/log/lunarwing-mt-web.log`:

```
2026-07-10T12:34:56Z tenant=ruffles action=add-tenant exit=0 duration=45s
2026-07-10T12:35:41Z tenant=ruffles action=build-tenant exit=0 duration=312s
```
