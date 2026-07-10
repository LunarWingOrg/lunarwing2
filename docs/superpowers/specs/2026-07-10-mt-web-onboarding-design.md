# Design: LunarWing MT Web Onboarding

**Date:** 2026-07-10
**Status:** Draft
**Author:** Christopher (via brainstorming with Sisyphus)

## Problem

The interactive Python CLI (`lunarwing_mt_onboard`) provides a guided provisioning experience but is terminal-only. Operators who prefer a visual interface have no alternative to the raw `mt-admin.sh` script. A web-based onboarding wizard would make tenant provisioning accessible to a broader audience while preserving all the safety guarantees and logic of the CLI.

## Solution

A self-contained Flask web application (`lunarwing_mt_web/`) that mirrors the full interactive onboarding flow through a browser-based wizard. Served on `127.0.0.1:7424`, launched via a shell script that opens the browser automatically.

## Architecture

### Package structure

```
lunarwing_mt_web/
├── launch.sh              # Entry point: checks deps, starts Flask, opens browser
├── __init__.py            # Package marker
├── server.py              # Flask app: routes, API endpoints, SSE streaming
├── bridge.py              # Imports lunarwing_mt_onboard modules, wraps provisioner
├── templates/
│   └── index.html         # Single-page wizard (all steps in one HTML file)
├── static/
│   ├── style.css          # LunarWing design system (ported from gateway CSS)
│   ├── app.js             # Wizard logic, animations, SSE handling, mascot cycling
│   ├── logo.svg           # Copied from ic/src/channels/web/static/logo.svg
│   ├── favicon.svg        # Copied from ic/src/channels/web/static/favicon.svg
│   └── bat/               # SVG bat mascot sprites
│       ├── content.svg
│       ├── angry.svg
│       ├── sleeping.svg
│       └── excited.svg
└── requirements.txt       # flask
```

### Tech stack

- **Backend**: Python + Flask (lightweight, one pip dep)
- **Frontend**: Vanilla HTML/CSS/JS (no build step, no Node.js)
- **Communication**: REST for config submission, Server-Sent Events (SSE) for streaming provisioning output
- **Launcher**: Shell script (`launch.sh`) — checks for Flask, installs if missing, starts server, prints URL, attempts to open browser via `xdg-open`

### Bridge to existing onboarding logic

`bridge.py` imports directly from `lunarwing_mt_onboard`:
- `lunarwing_mt_onboard.config.TenantConfig` — for config validation and serialization
- `lunarwing_mt_onboard.provisioner.build_add_tenant_args()` — constructs mt-admin.sh argv
- `lunarwing_mt_onboard.provisioner.build_build_tenant_args()` — build phase argv
- `lunarwing_mt_onboard.provisioner._inject_secrets()` — secret injection into lunarwing.env
- `lunarwing_mt_onboard.provisioner.ensure_mt_admin()` — locate the shell script
- `lunarwing_mt_onboard.verify.verify_tenant()` — post-start health checks

No provisioning logic is duplicated. The web app is a presentation and transport layer over the existing CLI's operations.

### Launch script (`launch.sh`)

```bash
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

# Ensure Flask is available
python3 -c "import flask" 2>/dev/null || pip3 install flask

PORT="${LUNARWING_WEB_PORT:-7424}"

echo "LunarWing MT Web Onboarding"
echo "   Starting on http://127.0.0.1:${PORT}"
echo "   Press Ctrl+C to stop"

# Try to open browser (non-blocking)
(command -v xdg-open &>/dev/null && xdg-open "http://127.0.0.1:${PORT}") 2>/dev/null &

exec python3 -m lunarwing_mt_web.server --port "$PORT"
```

Port 7424 is chosen for being memorable (L-U-N-W phonetic mapping) and well above the privileged range.

## Wizard flow (9 screens)

Each screen is a `<section>` in a single HTML page, shown/hidden via JavaScript with fade + slide transitions (300ms, `ease-out-expo`). The right sidebar always shows the bat mascot and current moon phase indicator.

### Screen 1: Welcome

- **Content**: LunarWing logo (SVG, centered), app title, brief description
- **Action**: "Begin Onboarding" button
- **Mascot**: Content (smiling)
- **Moon**: New moon (empty circle)

### Screen 2: Identity

- **Fields**:
  - Tenant name (text input, live regex validation: `^[a-z0-9][a-z0-9-]*$`)
  - Reserved prefix check: rejects `pg-`, `proxy-`, `nanocode-`, `pebble-`, `opencode-`, `weechat-`
  - Real-time visual feedback: green border when valid, red border + error message when invalid
- **Mascot**: Content (curious)
- **Moon**: Waxing crescent (thin sliver)

### Screen 3: Network

- **Fields**:
  - Gateway bind host/IP (text input, default: `127.0.0.1`)
  - Add to docker/podman group (toggle switch, default: ON)
- **Mascot**: Content
- **Moon**: First quarter (half full)

### Screen 4: Channels

- **Fields**:
  - DarkIRC (toggle, default: OFF)
  - XMPP bridge (toggle, default: OFF)
    - When ON, expands sub-section:
      - XMPP domain (text, default: `xmpp.localhost`)
      - XMPP JID (text, default: `{name}@{domain}`, auto-derived)
      - XMPP password (password, empty = auto-generate)
      - Extra allowed DM senders (text, comma-separated)
  - Gotify notifications (toggle, default: OFF)
    - When ON, expands sub-section:
      - Gotify URL (url input)
      - Gotify title override (text, default: tenant name)
- **Mascot**: Excited when XMPP or Gotify enabled, content otherwise
- **Moon**: Waxing gibbous (3/4 full)

### Screen 5: Workers

- **Fields**:
  - Worker multi-select (checkbox cards):
    - nanocode (NanoGPT)
    - pebble (Rust harness)
    - opencode (sst/opencode)
  - Include Rust/Go/C++ toolchains (toggle, default: OFF, only shown when workers selected)
  - Warning badge: "+~5GB to image size" when toolchains toggled ON
- **Mascot**: Sleeping (warning about long build ahead)
- **Moon**: Waxing gibbous (3/4 full)

### Screen 6: LLM

- **Fields**:
  - TensorZero upstream URL (url input, default: `http://192.168.1.157:3000/openai/v1`)
  - LLM model (select dropdown):
    - `tensorzero::function_name::FrontierCODE`
    - `tensorzero::function_name::lunarwing` (default selected)
    - `Custom...` (reveals custom model text input)
  - LLM API key (password input, empty = not set)
- **Mascot**: Content
- **Moon**: Waxing gibbous (nearly full)

### Screen 7: Security

- **Fields**:
  - Secrets master key (password input, empty = auto-generate)
    - "Generate" button that creates a random 64-char hex key and fills the field
    - Validation: if non-empty, must be 64-char hex
  - Provision SSH harness (toggle, default: ON)
  - Enable host health pipeline (toggle, default: ON)
- **Mascot**: Angry (guarding secrets, red eyes, bared fangs)
- **Moon**: Almost full

### Screen 8: Review

- **Content**: Summary card listing all configured fields, secrets masked (showing last 4 chars only)
- **Action**: "Confirm and Provision" button (also "Back" to edit)
- **Mascot**: Excited (ready to go)
- **Moon**: Full moon (complete)

### Screen 9: Provisioning

- **Content**: 
  - Phase progress indicator (vertical stepper): add-tenant -> inject-secrets -> build-tenant -> (build-darkirc) -> start-tenant -> verification
  - Animated progress bar per phase
  - Terminal-style log output box (monospace, dark background, streaming lines via SSE)
  - Current phase label with animated ellipsis
- **States**:
  - In-progress: blue accent, pulsing animation, bat cycling emotions
  - Success per phase: green checkmark, phase label dimmed
  - Failure: red X, error details shown, "Retry" or "Abort" buttons
  - All phases complete: full success panel, bat excited, verification results table
- **Mascot**: Cycles through all emotions based on phase state:
  - Sleeping during build-tenant (long operation)
  - Angry on error
  - Excited on phase completion
  - Content during idle/in-between
- **Moon**: Full moon with subtle glow pulse

## Backend API

### Endpoints

| Endpoint | Method | Purpose |
|---|---|---|
| `/` | GET | Serve `templates/index.html` |
| `/static/<path>` | GET | Static assets (CSS, JS, SVGs) |
| `/api/validate/name` | POST | Validate tenant name, return `{valid: bool, error: str\|null}` |
| `/api/provision` | POST | Accept full `TenantConfig` JSON, start provisioning in background thread, return `{session_id: str}` |
| `/api/provision/<session_id>/stream` | GET | SSE stream: line-by-line output from provisioning subprocesses |
| `/api/provision/<session_id>/status` | GET | Current state: `{phase: str, status: "running"\|"ok"\|"fail", phases: [...]}` |
| `/api/verify/<tenant>` | GET | Run `verify_tenant()`, return results JSON |

### Provisioning flow (server-side)

1. `POST /api/provision` receives `TenantConfig` JSON
2. `bridge.py` constructs a `TenantConfig` object via `TenantConfig.from_dict()`
3. Validates the config
4. Spawns a background thread that:
   a. Calls `build_add_tenant_args()` -> runs `mt-admin.sh add-tenant` as subprocess
   b. Calls `_inject_secrets()` to write secret values to `lunarwing.env`
   c. Calls `build_build_tenant_args()` -> runs `mt-admin.sh build-tenant`
   d. If darkirc: runs `mt-admin.sh build-darkirc --tenant <name>`
   e. Runs `mt-admin.sh start-tenant <name>`
   f. Calls `verify_tenant()` for post-start checks
5. Each subprocess streams stdout/stderr line-by-line into a queue
6. SSE endpoint reads from the queue and pushes to connected clients
7. Session state tracked in a dict: `{session_id: {phase, status, phases: [], queue: Queue}}`

### Error handling

- Phase failure halts provisioning (same as CLI behavior)
- Error message surfaced in the SSE stream and `/status` endpoint
- Frontend shows error state with the failing phase highlighted in red
- User can go back to the review screen, adjust config, and retry

## Activity logging

Every `mt-admin.sh` invocation is logged to `/var/log/lunarwing-mt-web.log`:

```
2026-07-10T12:34:56Z tenant=ruffles action=add-tenant exit=0 duration=45s
2026-07-10T12:35:41Z tenant=ruffles action=build-tenant exit=0 duration=312s
2026-07-10T12:40:53Z tenant=ruffles action=start-tenant exit=0 duration=12s
```

Log entries include: ISO timestamp, tenant name, action, exit code, duration in seconds. Errors also log stderr excerpt (first 500 chars).

## Visual design

### Color system

Direct port of the web gateway CSS custom properties. Dark theme only (onboarding is a server-side operation, no need for light theme):

**Backgrounds (3-tier depth)**:
- `--bg`: `#09090b` (deepest, app body)
- `--bg-secondary`: `#0f0f11` (cards, panels)
- `--bg-tertiary`: `#1a1a1e` (hover states)

**Accent (blue family)**:
- `--accent`: `#60a5fa`
- `--accent-hover`: `#3b82f6`
- `--accent-brand`: `#2563eb`

**Semantic**:
- `--success`: `#34d399`
- `--warning`: `#F5A623`
- `--danger`: `#E64C4C`

**Text**:
- `--text`: `#fafafa` (primary)
- `--text-secondary`: `#a1a1aa` (labels, descriptions)
- `--text-muted`: `#888`

**Borders**:
- `--border`: `rgba(255, 255, 255, 0.08)`
- `--border-hover`: `rgba(255, 255, 255, 0.15)`

### Typography

- **UI font**: DM Sans (400/500/600/700), loaded from Google Fonts
- **Monospace**: IBM Plex Mono (400/500/600), for terminal output and code
- **Size scale**: 11px (xs), 13px (sm), 14px (base), 16px (lg), 20px (xl), 24px (2xl), 36px (3xl)

### Component patterns

Ported from the web gateway:

- **Buttons**: Primary (accent fill, dark text), secondary (bg-tertiary + border), outline (semantic colors). Spring hover (`translateY(-1px)`), scale on click (`scale(0.97)`).
- **Cards**: `bg-secondary`, 12px radius, `1px solid border`, subtle shadow. Left-border accent (3px) for emphasis.
- **Inputs**: Darker than card (`bg`), 8px radius, accent focus ring (`0 0 0 3px rgba(96,165,250,0.1)`).
- **Toggle switches**: 44x24px, accent fill when ON.
- **Step indicator**: Vertical or horizontal stepper with numbered circles, filled/dimmed based on progress.

### Logo

Use `logo.svg` (vector, from web gateway) as the primary logo. Copied into `static/logo.svg`. Favicon from `favicon.svg`.

### Bat mascot (4 SVG sprites)

Four hand-crafted SVG bat faces in the LunarWing blue palette, ~120px display size, positioned in a fixed sidebar on the right:

1. **Content** — Small smile, relaxed round eyes, gentle wing position. Default state.
2. **Angry** — Red glowing eyes (CSS `filter: drop-shadow(0 0 4px #E64C4C)`), bared fangs (white triangles), wings raised. Shown during security step and on errors.
3. **Sleeping** — Closed eyes (curved lines), small "z z z" text floating above (CSS animation), drooped wings. Shown during long build operations.
4. **Excited** — Wide eyes (large circles), big grin, wings spread upward. Shown on successful steps and completions.

**Animation behaviors**:
- Idle: gentle floating motion (CSS keyframe: `translateY(0 → -4px → 0)`, 3s loop)
- Emotion swap: 200ms crossfade between sprites
- Excited: wings flap (CSS keyframe: `rotate(-5deg → 5deg)`, 0.5s loop)
- Sleeping: slow breathing motion (scale: `1.0 → 1.03 → 1.0`, 4s loop)
- Angry: subtle shake (translate: `0 → 1px → 0 → -1px → 0`, 0.3s loop)
- Auto-cycle: mascot changes emotion every ~15 seconds when idle (content -> sleeping -> content -> excited -> content)

**Triggered emotion changes**:
- Tenant name validated: content -> excited -> content (quick flash)
- XMPP/Gotify enabled: content -> excited
- Build phase starts: sleeping
- Phase succeeds: excited (2s) -> content
- Error occurs: angry (until resolved)
- Security step active: angry
- Final success: excited (persistent, wings flapping)

### Moon phase indicator

CSS-drawn circle representing the moon, positioned above the mascot in the sidebar. Progresses through phases as the wizard advances:

| Wizard step | Moon phase | Visual |
|---|---|---|
| Welcome | New moon | Empty dark circle with thin outline |
| Identity | Waxing crescent | Thin blue sliver on right edge |
| Network | First quarter | Right half illuminated |
| Channels | Waxing gibbous | 3/4 illuminated |
| Workers | Waxing gibbous | 3/4 illuminated (same) |
| LLM | Almost full | 90% illuminated |
| Security | Almost full | 90% illuminated (slight shadow) |
| Review | Full moon | Fully illuminated, subtle glow |
| Provisioning | Full moon | Glow pulse animation synchronized with build progress |

Implementation: CSS `box-shadow inset` or `clip-path` to create the crescent shape. Blue gradient fill (`#B3E5FC` -> `#4FC3F7`) matching the logo's moon gradient.

### Terminal output box

During provisioning, a monospace log box styled like a terminal:
- Background: `#111113` (from gateway `--code-bg`)
- Font: IBM Plex Mono, 13px
- Border: subtle accent border when active
- Auto-scroll to bottom
- Color-coded lines: stdout (default text), stderr (warning/danger color), phase markers (accent color, bold)
- Max height with scrollbar

## Security considerations

- **localhost-only**: Flask binds to `127.0.0.1` — not accessible from external networks
- **Secrets never in argv**: Same guarantee as the CLI — `xmpp_password`, `llm_api_key`, and `secrets_master_key` are injected into `lunarwing.env` via file write, never passed as subprocess arguments
- **Secrets masked in review**: Summary screen shows `****<last4>` for all secret values
- **Root required**: `launch.sh` must be run as root (same as mt-admin.sh). The Flask process inherits root privileges. This is acceptable for a localhost-only tool.
- **No CSRF token**: Acceptable for localhost-only, single-user tool. If this changes, add Flask-WTF.
- **Session isolation**: Each provisioning run gets a unique session ID (UUID). Only one provisioning session at a time (concurrent builds would conflict on cargo lock anyway).

## Testing

- **Unit tests**: `bridge.py` config construction — verify `build_add_tenant_args()` and `build_build_tenant_args()` produce correct argv from web-submitted JSON configs
- **Integration smoke test**: Start Flask app, `GET /` returns 200 with expected HTML, `GET /static/style.css` returns 200
- **Manual test**: Full provisioning flow against a test tenant, verifying SSE streaming and phase progression

## Out of scope

- Upgrade subcommand (can be added later)
- Export subcommand (can be added later)
- Secrets subcommand (can be added later)
- Multi-user authentication (localhost-only, single operator)
- Light theme (dark-only for now)
- Progress rollback / undo (mt-admin.sh doesn't support it)
- Concurrent provisioning sessions (serialized by cargo flock anyway)
