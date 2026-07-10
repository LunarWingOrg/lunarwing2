# LunarWing MT Web Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Flask web application that mirrors the lunarwing_mt_onboard interactive CLI through a browser-based wizard with LunarWing visual branding, animated bat mascot, moon phase indicator, and SSE-streamed provisioning output.

**Architecture:** Flask backend serves a single-page HTML/CSS/JS frontend. Backend imports lunarwing_mt_onboard modules directly for config validation and provisioning logic. Provisioning runs in a background thread with SSE streaming output to the browser.

**Tech Stack:** Python 3.10+, Flask, vanilla HTML/CSS/JS (no build step), DM Sans + IBM Plex Mono fonts, SVG bat mascot sprites.

## Global Constraints

- Python 3.10+ required
- Flask is the only new pip dependency (`flask`)
- No Node.js, no build step, no JS framework
- Dark theme only — port CSS variables from `ic/src/channels/web/static/style.css`
- Fonts: DM Sans (400/500/600/700) + IBM Plex Mono (400/500/600) via Google Fonts
- Must import from `lunarwing_mt_onboard` package — no provisioning logic duplication
- Root/sudo required at runtime (mt-admin.sh needs root)
- Port: 7424 (override via `LUNARWING_WEB_PORT` env var)
- Secrets never passed on subprocess argv — injected via file write

---

## File Structure

```
lunarwing_mt_web/
├── __init__.py              # Package marker
├── server.py                # Flask app, routes, API endpoints, SSE
├── bridge.py                # Wraps lunarwing_mt_onboard provisioner
├── launch.sh                # Entry point script
├── requirements.txt         # flask
├── templates/
│   └── index.html           # Single-page wizard (all 9 screens)
├── static/
│   ├── style.css            # LunarWing design system
│   ├── app.js               # Wizard logic, animations, SSE, mascot
│   ├── logo.svg             # Copied from web gateway
│   ├── favicon.svg          # Copied from web gateway
│   └── bat/
│       ├── content.svg      # 4 bat mascot emotions
│       ├── angry.svg
│       ├── sleeping.svg
│       └── excited.svg
└── tests.py                 # Unit tests
```

---

### Task 1: Package scaffold + static assets

**Files:**
- Create: `lunarwing_mt_web/__init__.py`
- Create: `lunarwing_mt_web/requirements.txt`
- Create: `lunarwing_mt_web/launch.sh`
- Create: `lunarwing_mt_web/static/logo.svg`
- Create: `lunarwing_mt_web/static/favicon.svg`
- Create: `lunarwing_mt_web/templates/.gitkeep`
- Create: `lunarwing_mt_web/static/bat/.gitkeep`

**Interfaces:**
- Produces: `lunarwing_mt_web` Python package, launchable via `launch.sh`

- [ ] **Step 1: Create package scaffold**

Create `lunarwing_mt_web/__init__.py`:
```python
"""Web-based multi-tenant onboarding wizard for LunarWing.

Run with::

    sudo bash lunarwing_mt_web/launch.sh
"""

__version__ = "0.1.0"
```

Create `lunarwing_mt_web/requirements.txt`:
```
flask>=3.0
```

- [ ] **Step 2: Copy logo and favicon**

Copy `ic/src/channels/web/static/logo.svg` to `lunarwing_mt_web/static/logo.svg`.
Copy `ic/src/channels/web/static/favicon.svg` to `lunarwing_mt_web/static/favicon.svg`.

- [ ] **Step 3: Create launch.sh**

Create `lunarwing_mt_web/launch.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

# Ensure Flask is available
python3 -c "import flask" 2>/dev/null || pip3 install flask

PORT="${LUNARWING_WEB_PORT:-7424}"

echo ""
echo "  LunarWing MT Web Onboarding"
echo "  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~"
echo "  Starting on http://127.0.0.1:${PORT}"
echo "  Press Ctrl+C to stop"
echo ""

# Try to open browser (non-blocking)
(command -v xdg-open &>/dev/null && xdg-open "http://127.0.0.1:${PORT}") 2>/dev/null &

exec python3 -m lunarwing_mt_web.server --port "$PORT"
```

Make it executable: `chmod +x lunarwing_mt_web/launch.sh`

- [ ] **Step 4: Create placeholder files**

Create empty `.gitkeep` files in `templates/` and `static/bat/` so directories are tracked.

- [ ] **Step 5: Commit**

```bash
git add lunarwing_mt_web/
git commit -m "Add lunarwing_mt_web package scaffold and static assets"
```

---

### Task 2: Bat mascot SVG sprites

**Files:**
- Create: `lunarwing_mt_web/static/bat/content.svg`
- Create: `lunarwing_mt_web/static/bat/angry.svg`
- Create: `lunarwing_mt_web/static/bat/sleeping.svg`
- Create: `lunarwing_mt_web/static/bat/excited.svg`

**Interfaces:**
- Produces: 4 SVG files, each ~100x100 viewBox, using LunarWing blue palette (#4FC3F7, #1565C0, #0D47A1)

- [ ] **Step 1: Create content bat**

Default state. Round body (dark blue #0D47A1 fill), small smile, two round eyes (#4FC3F7), simplified wings on sides. Gentle and approachable. ViewBox 0 0 100 100.

- [ ] **Step 2: Create angry bat**

Red glowing eyes (#E64C4C with red filter glow), bared fangs (white triangles below mouth), eyebrows angled down, wings raised aggressively. Same body shape as content.

- [ ] **Step 3: Create sleeping bat**

Closed eyes (curved downward lines), small "z z z" text floating above at varying opacities, wings drooped lower, peaceful expression.

- [ ] **Step 4: Create excited bat**

Wide eyes (large circles with highlight), big open grin, wings spread upward, small motion lines near wing tips. Joyful.

- [ ] **Step 5: Commit**

```bash
git add lunarwing_mt_web/static/bat/
git commit -m "Add bat mascot SVG sprites (content, angry, sleeping, excited)"
```

---

### Task 3: Stylesheet — LunarWing design system

**Files:**
- Create: `lunarwing_mt_web/static/style.css`

**Interfaces:**
- Produces: Complete CSS with `:root` variables ported from gateway, layout classes, wizard step styling, mascot/moon animations, terminal output box, responsive layout

- [ ] **Step 1: Write the CSS design system**

Port the `:root` CSS variables from `ic/src/channels/web/static/style.css`. Include:

**Variables block** (dark theme only):
```css
:root {
  --bg: #09090b;
  --bg-secondary: #0f0f11;
  --bg-tertiary: #1a1a1e;
  --border: rgba(255, 255, 255, 0.08);
  --border-hover: rgba(255, 255, 255, 0.15);
  --text: #fafafa;
  --text-secondary: #a1a1aa;
  --text-muted: #888;
  --accent: #60a5fa;
  --accent-hover: #3b82f6;
  --accent-brand: #2563eb;
  --text-on-accent: #09090b;
  --accent-subtle: rgba(96, 165, 250, 0.15);
  --accent-border-subtle: rgba(96, 165, 250, 0.3);
  --focus-ring: rgba(96, 165, 250, 0.1);
  --success: #34d399;
  --warning: #F5A623;
  --danger: #E64C4C;
  --code-bg: #111113;
  --radius: 8px;
  --radius-lg: 12px;
  --shadow-card: 0 4px 24px rgba(0, 0, 0, 0.4);
  --ease-out-expo: cubic-bezier(0.16, 1, 0.3, 1);
  --ease-spring: cubic-bezier(0.34, 1.56, 0.64, 1);
}
```

**Layout**: Two-column layout on desktop. Left: wizard content (max-width 640px, centered). Right: fixed sidebar (mascot + moon phase). Below 900px width: sidebar moves to bottom.

**Components to style**:
- `.wizard-section` — each screen section, hidden by default, `.active` class shows it
- `.form-card` — card container for form fields (bg-secondary, border, radius-lg, padding)
- `.form-group` — label + input wrapper
- `.btn-primary` — accent fill button
- `.btn-secondary` — ghost/secondary button
- `.toggle-switch` — CSS-only toggle (44x24px)
- `.input` — text/password/url input (bg, border, radius, focus ring)
- `.select` — dropdown styled like input
- `.checkbox-card` — clickable card for worker selection
- `.moon-indicator` — CSS-drawn moon (circle with box-shadow inset)
- `.bat-container` — fixed position mascot area
- `.bat-sprite` — SVG sprite container with float animation
- `.terminal-output` — monospace log box (code-bg, IBM Plex Mono, auto-scroll)
- `.progress-bar` — animated progress bar for provisioning
- `.phase-stepper` — vertical stepper for provisioning phases
- `.step-dots` — horizontal step indicator at top of wizard

**Animations** (CSS keyframes):
- `@keyframes fadeInSlide` — opacity 0→1, translateY 10px→0, 300ms ease-out-expo
- `@keyframes batFloat` — translateY 0→-4px→0, 3s infinite
- `@keyframes batFlap` — rotate -5deg→5deg, 0.5s infinite (excited state)
- `@keyframes batBreathe` — scale 1.0→1.03→1.0, 4s infinite (sleeping state)
- `@keyframes batShake` — translate 0→1px→0→-1px→0, 0.3s infinite (angry state)
- `@keyframes moonGlow` — box-shadow pulse, 3s infinite (full moon during provisioning)
- `@keyframes ellipsis` — dots animation for "working..." text
- `@keyframes progressFill` — width 0→100%, transition-based

- [ ] **Step 2: Commit**

```bash
git add lunarwing_mt_web/static/style.css
git commit -m "Add LunarWing design system CSS for web onboarding wizard"
```

---

### Task 4: Backend — Flask server and bridge

**Files:**
- Create: `lunarwing_mt_web/server.py`
- Create: `lunarwing_mt_web/bridge.py`

**Interfaces:**
- Consumes: `lunarwing_mt_onboard.config.TenantConfig`, `lunarwing_mt_onboard.provisioner.*`, `lunarwing_mt_onboard.verify.verify_tenant`
- Produces: Flask app with routes: `GET /`, `POST /api/validate/name`, `POST /api/provision`, `GET /api/provision/<id>/stream` (SSE), `GET /api/provision/<id>/status`, `GET /api/verify/<tenant>`

- [ ] **Step 1: Write bridge.py**

```python
"""Bridge between the web layer and lunarwing_mt_onboard internals."""

from __future__ import annotations

import logging
import os
import queue
import threading
import uuid
from datetime import datetime, timezone
from dataclasses import dataclass, field
from typing import Any

from lunarwing_mt_onboard.config import TenantConfig
from lunarwing_mt_onboard.provisioner import (
    build_add_tenant_args,
    build_build_tenant_args,
    build_darkirc_args,
    build_start_tenant_args,
    ensure_mt_admin,
    provision,
)
from lunarwing_mt_onboard.verify import verify_tenant

LOG_FILE = "/var/log/lunarwing-mt-web.log"

logger = logging.getLogger("lunarwing_mt_web")


def _log_activity(tenant: str, action: str, exit_code: int, duration: float) -> None:
    """Append a line to the activity log."""
    ts = datetime.now(timezone.utc).isoformat()
    line = f"{ts} tenant={tenant} action={action} exit={exit_code} duration={duration:.0f}s\n"
    try:
        with open(LOG_FILE, "a") as f:
            f.write(line)
    except OSError:
        pass


@dataclass
class SessionState:
    """Tracks a single provisioning session."""

    session_id: str
    tenant: str
    status: str = "running"  # running, ok, fail
    phases: list[dict[str, Any]] = field(default_factory=list)
    output_queue: queue.Queue[str] = field(default_factory=queue.Queue)
    current_phase: str = ""
    error: str = ""

    def push(self, line: str) -> None:
        self.output_queue.put(line)

    def to_dict(self) -> dict[str, Any]:
        return {
            "session_id": self.session_id,
            "tenant": self.tenant,
            "status": self.status,
            "current_phase": self.current_phase,
            "phases": list(self.phases),
            "error": self.error,
        }


# Module-level session registry (one at a time)
_active_session: SessionState | None = None
_session_lock = threading.Lock()


def get_session(session_id: str) -> SessionState | None:
    with _session_lock:
        global _active_session
        if _active_session and _active_session.session_id == session_id:
            return _active_session
        return None


def start_provisioning(config_dict: dict[str, Any]) -> SessionState:
    """Start provisioning in a background thread. Returns the session state."""
    import time

    with _session_lock:
        global _active_session
        if _active_session and _active_session.status == "running":
            raise RuntimeError("A provisioning session is already running")

    config = TenantConfig.from_dict(config_dict)
    err = TenantConfig.validate_name(config.name)
    if err:
        raise ValueError(err)

    if not config.secrets_master_key:
        from lunarwing_mt_onboard.secrets import generate_master_key
        config.secrets_master_key = generate_master_key()

    session = SessionState(session_id=str(uuid.uuid4()), tenant=config.name)
    with _session_lock:
        _active_session = session

    def _on_output(line: str) -> None:
        session.push(line)

    def _worker() -> None:
        try:
            ensure_mt_admin()
        except FileNotFoundError as exc:
            session.push(f"[ERROR] {exc}")
            session.status = "fail"
            session.error = str(exc)
            return

        phases_meta = [
            ("add-tenant", "Creating OS user and allocating ports"),
            ("inject-secrets", "Injecting secrets into tenant env"),
            ("build-tenant", "Building release binary + WASM tools"),
            ("build-darkirc", "Building DarkIRC daemon"),
            ("start-tenant", "Starting tenant services"),
            ("verify", "Post-start health verification"),
        ]
        if not config.enable_darkirc:
            phases_meta = [p for p in phases_meta if p[0] != "build-darkirc"]

        session.current_phase = "add-tenant"
        session.push(f"[PHASE] Starting: add-tenant")

        t0 = time.time()
        result = provision(
            config,
            on_output=_on_output,
        )

        for pr in result.phases:
            session.phases.append({
                "name": pr.name,
                "ok": pr.ok,
                "returncode": pr.returncode,
            })
            _log_activity(config.name, pr.name, pr.returncode, time.time() - t0)
            session.push(f"[{'OK' if pr.ok else 'FAIL'}] {pr.name} (exit {pr.returncode})")

        if result.ok:
            session.current_phase = "verify"
            session.push("[PHASE] Starting: post-start verification")
            results = verify_tenant(config.name, config.gateway_host, config.gateway_port)
            for r in results:
                session.phases.append({
                    "name": f"verify:{r.label}",
                    "ok": r.ok,
                    "returncode": 0 if r.ok else 1,
                })
                session.push(f"[{'OK' if r.ok else 'FAIL'}] verify:{r.label}")
            session.status = "ok"
            session.current_phase = "complete"
        else:
            session.status = "fail"
            session.current_phase = "failed"

        session.push("[DONE]")

    thread = threading.Thread(target=_worker, daemon=True)
    thread.start()
    return session


def verify_existing(tenant: str, host: str = "127.0.0.1", port: int = 0) -> list[dict[str, Any]]:
    """Run verify_tenant and return JSON-serializable results."""
    results = verify_tenant(tenant, host, port)
    return [{"label": r.label, "ok": r.ok} for r in results]
```

- [ ] **Step 2: Write server.py**

```python
"""Flask server for the LunarWing MT Web Onboarding wizard."""

from __future__ import annotations

import argparse
import json
import time

from flask import Flask, Response, render_template, request, jsonify, stream_with_context

from lunarwing_mt_web.bridge import (
    SessionState,
    get_session,
    start_provisioning,
    verify_existing,
)
from lunarwing_mt_onboard.config import TenantConfig

app = Flask(
    __name__,
    template_folder="templates",
    static_folder="static",
)


@app.route("/")
def index():
    return render_template("index.html")


@app.route("/api/validate/name", methods=["POST"])
def validate_name():
    data = request.get_json(force=True)
    name = data.get("name", "")
    err = TenantConfig.validate_name(name)
    return jsonify({"valid": err is None, "error": err})


@app.route("/api/provision", methods=["POST"])
def provision():
    data = request.get_json(force=True)
    try:
        session = start_provisioning(data)
    except (ValueError, RuntimeError) as exc:
        return jsonify({"error": str(exc)}), 400
    return jsonify({"session_id": session.session_id})


@app.route("/api/provision/<session_id>/stream")
def provision_stream(session_id: str):
    session = get_session(session_id)
    if not session:
        return jsonify({"error": "session not found"}), 404

    def generate():
        while True:
            try:
                line = session.output_queue.get(timeout=30)
                yield f"data: {json.dumps(line)}\n\n"
                if line == "[DONE]":
                    break
            except Exception:
                # Heartbeat keepalive
                yield f"data: {json.dumps('[HEARTBEAT]')}\n\n"

    return Response(
        stream_with_context(generate()),
        mimetype="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


@app.route("/api/provision/<session_id>/status")
def provision_status(session_id: str):
    session = get_session(session_id)
    if not session:
        return jsonify({"error": "session not found"}), 404
    return jsonify(session.to_dict())


@app.route("/api/verify/<tenant>")
def verify(tenant: str):
    host = request.args.get("host", "127.0.0.1")
    port = int(request.args.get("port", 0))
    results = verify_existing(tenant, host, port)
    return jsonify({"results": results})


def main():
    parser = argparse.ArgumentParser(description="LunarWing MT Web Onboarding")
    parser.add_argument("--port", type=int, default=7424)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()
    app.run(host=args.host, port=args.port, debug=False, threaded=True)


if __name__ == "__main__":
    main()
```

- [ ] **Step 3: Test imports resolve**

```bash
python3 -c "from lunarwing_mt_web.server import app; print('OK')"
python3 -c "from lunarwing_mt_web.bridge import start_provisioning; print('OK')"
```

Expected: both print `OK` with no import errors.

- [ ] **Step 4: Commit**

```bash
git add lunarwing_mt_web/server.py lunarwing_mt_web/bridge.py
git commit -m "Add Flask server and provisioner bridge for web onboarding"
```

---

### Task 5: Frontend — HTML wizard page

**Files:**
- Create: `lunarwing_mt_web/templates/index.html`

**Interfaces:**
- Consumes: `/api/validate/name`, `/api/provision`, `/api/provision/<id>/stream`, `/api/provision/<id>/status`
- Produces: Single-page wizard with 9 sections (welcome, identity, network, channels, workers, llm, security, review, provisioning)

- [ ] **Step 1: Write the HTML structure**

Single-page app with:
- `<head>`: Meta tags, Google Fonts (DM Sans + IBM Plex Mono), stylesheet link, favicon link
- `<body class="dark">`: 
  - `.layout` — two-column grid (wizard + sidebar)
  - `.wizard` — left column with all 9 `<section class="wizard-section">` elements
  - `.sidebar` — right column with moon indicator + bat mascot
  - Step dots indicator at top

**Section details** (all must be implemented):

1. **Welcome**: Logo SVG (centered, max-width 200px), "LunarWing" title, subtitle "Multi-Tenant Onboarding Wizard", "Begin Onboarding" button
2. **Identity**: Tenant name text input with `oninput` validation call, error message div, "Continue" button, "Back" button
3. **Network**: Gateway host text input (default 127.0.0.1), docker group toggle, nav buttons
4. **Channels**: DarkIRC toggle, XMPP toggle (expandable sub-section with domain/JID/password/allow-from), Gotify toggle (expandable sub-section with URL/title), nav buttons
5. **Workers**: Three checkbox-cards (nanocode/pebble/opencode), toolchains toggle (conditionally visible), toolchains warning badge, nav buttons
6. **LLM**: TensorZero URL input, model select dropdown (FrontierCODE/lunarwing/Custom), custom model text input (conditionally visible), API key password input, nav buttons
7. **Security**: Master key password input with "Generate" button, SSH toggle, health toggle, nav buttons
8. **Review**: Summary table populated by JS from collected config (all fields, secrets masked), "Confirm and Provision" button, "Back" button
9. **Provisioning**: Phase stepper, progress bar, terminal output box, bat status area. Success/error states at bottom.

Each section has `id="step-<name>"` and `class="wizard-section"`. Only `.active` section is visible.

- [ ] **Step 2: Commit**

```bash
git add lunarwing_mt_web/templates/index.html
git commit -m "Add single-page HTML wizard for web onboarding"
```

---

### Task 6: Frontend — JavaScript wizard logic

**Files:**
- Create: `lunarwing_mt_web/static/app.js`

**Interfaces:**
- Consumes: All HTML element IDs from Task 5, all API endpoints from Task 4
- Produces: `app.js` loaded by `index.html`, handles navigation, validation, config collection, SSE streaming, mascot/moon animation

- [ ] **Step 1: Write the JavaScript**

Implement these modules in a single file:

**Navigation**:
- `showStep(stepName)` — hide all `.wizard-section`, show `#step-{stepName}`, update step dots, update moon phase, update bat emotion based on step
- `nextStep()` / `prevStep()` — advance/retreat based on step order array
- Step order: `['welcome', 'identity', 'network', 'channels', 'workers', 'llm', 'security', 'review', 'provisioning']`

**Validation**:
- `validateTenantName(name)` — POST to `/api/validate/name`, update UI (green/red border, error text), return boolean
- Debounced on input (300ms delay)

**Config collection**:
- `collectConfig()` — reads all form inputs into a plain object matching `TenantConfig.to_dict()` shape:
```javascript
{
  name: string,
  gateway_host: string,
  docker_group: boolean,
  enable_darkirc: boolean,
  xmpp_enabled: boolean,
  xmpp_jid: string,
  xmpp_password: string,
  xmpp_allow_from: string[],
  gotify_enabled: boolean,
  gotify_url: string,
  gotify_title: string,
  workers: string[],  // "nanocode", "pebble", "opencode"
  toolchains: boolean,
  tensorzero_url: string,
  llm_model: string,
  llm_api_key: string,
  secrets_master_key: string,
  no_ssh: boolean,
  no_health: boolean
}
```

**Review population**:
- `populateReview()` — fills the review summary table from collected config, masks secrets (show last 4 chars only, rest as asterisks)

**Provisioning**:
- `startProvisioning()` — POST config to `/api/provision`, get session_id, connect to SSE stream
- `connectSSE(sessionId)` — `new EventSource('/api/provision/'+sessionId+'/stream')`, on message:
  - Lines starting with `[PHASE]` → update phase stepper, set progress bar to that phase position
  - Lines starting with `[OK]` → mark phase green in stepper
  - Lines starting with `[FAIL]` → mark phase red, show error state
  - Lines starting with `[DONE]` → show success panel or error panel
  - `[HEARTBEAT]` → ignore
  - All other lines → append to terminal output box, auto-scroll
- `setBatMood(emotion)` — swap bat sprite to content/angry/sleeping/excited, add/remove animation class

**Mascot cycling**:
- `startMascotCycle()` — every 15 seconds when idle, cycle content→sleeping→content→excited→content
- Stop cycling during provisioning (mood is controlled by phase state)

**Moon phase**:
- `updateMoon(stepIndex)` — update `.moon-indicator` fill level based on step index (0=0%, 1=12.5%, ..., 7=100%, 8=100%)

**Secret generation**:
- `generateMasterKey()` — fetch `/api/generate-key` or generate client-side: `crypto.getRandomValues(new Uint8Array(32))` → hex string

**Helper functions**:
- `showError(msg)` / `clearError()` — toggle error display
- `maskSecret(value)` — return `****<last4>`
- `toggleSection(id)` — show/hide conditional sub-sections (XMPP, Gotify fields)

- [ ] **Step 2: Commit**

```bash
git add lunarwing_mt_web/static/app.js
git commit -m "Add JavaScript wizard logic with SSE streaming and mascot animation"
```

---

### Task 7: Tests

**Files:**
- Create: `lunarwing_mt_web/tests.py`

**Interfaces:**
- Consumes: `lunarwing_mt_web.bridge`, `lunarwing_mt_web.server.app`
- Produces: Unit tests runnable via `python3 -m lunarwing_mt_web.tests`

- [ ] **Step 1: Write tests**

Test cases:

```python
"""Unit tests for lunarwing_mt_web."""

from __future__ import annotations

import json
import unittest

from lunarwing_mt_web.bridge import SessionState, _log_activity
from lunarwing_mt_web.server import app


class TestSessionState(unittest.TestCase):
    def test_session_state_creation(self):
        s = SessionState(session_id="abc", tenant="test")
        self.assertEqual(s.status, "running")
        self.assertEqual(s.tenant, "test")

    def test_session_push_and_to_dict(self):
        s = SessionState(session_id="abc", tenant="test")
        s.push("hello")
        self.assertEqual(s.to_dict()["tenant"], "test")
        self.assertEqual(s.to_dict()["status"], "running")

    def test_session_phase_tracking(self):
        s = SessionState(session_id="abc", tenant="test")
        s.phases.append({"name": "add-tenant", "ok": True, "returncode": 0})
        self.assertEqual(len(s.to_dict()["phases"]), 1)


class TestFlaskRoutes(unittest.TestCase):
    def setUp(self):
        self.client = app.test_client()

    def test_index_returns_html(self):
        resp = self.client.get("/")
        self.assertEqual(resp.status_code, 200)
        self.assertIn(b"text/html", resp.content_type.encode())

    def test_validate_name_empty_rejected(self):
        resp = self.client.post(
            "/api/validate/name",
            json={"name": ""},
        )
        data = resp.get_json()
        self.assertFalse(data["valid"])
        self.assertIsNotNone(data["error"])

    def test_validate_name_valid_accepted(self):
        resp = self.client.post(
            "/api/validate/name",
            json={"name": "ruffles"},
        )
        data = resp.get_json()
        self.assertTrue(data["valid"])

    def test_validate_name_reserved_rejected(self):
        resp = self.client.post(
            "/api/validate/name",
            json={"name": "pg-test"},
        )
        data = resp.get_json()
        self.assertFalse(data["valid"])

    def test_provision_missing_name_rejected(self):
        resp = self.client.post(
            "/api/provision",
            json={"name": ""},
        )
        self.assertEqual(resp.status_code, 400)

    def test_status_not_found(self):
        resp = self.client.get("/api/provision/nonexistent/status")
        self.assertEqual(resp.status_code, 404)


class TestActivityLog(unittest.TestCase):
    def test_log_activity_does_not_raise(self):
        # Should not raise even if log file is not writable
        _log_activity("test", "add-tenant", 0, 42.0)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run tests**

```bash
PYTHONPATH=. python3 -m pytest lunarwing_mt_web/tests.py -v
# or
PYTHONPATH=. python3 -c "
import unittest
from lunarwing_mt_web import tests
unittest.TextTestRunner(verbosity=2).run(
    unittest.TestLoader().loadTestsFromModule(tests)
)
"
```

Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add lunarwing_mt_web/tests.py
git commit -m "Add unit tests for web onboarding server and bridge"
```

---

### Task 8: README and smoke test script

**Files:**
- Create: `lunarwing_mt_web/README.md`
- Create: `lunarwing_mt_web/smoke_test.sh`

- [ ] **Step 1: Write README.md**

Document:
- What it is (web-based version of the interactive onboarding CLI)
- Requirements (Python 3.10+, Flask, root/sudo)
- Quick start: `sudo bash lunarwing_mt_web/launch.sh`
- Port info (7424 default, `LUNARWING_WEB_PORT` override)
- Browser access: `http://127.0.0.1:7424`
- What it does (mirrors CLI flow: 7 config steps → provisioning → verification)
- Activity log location (`/var/log/lunarwing-mt-web.log`)
- Module layout
- Testing instructions

- [ ] **Step 2: Write smoke_test.sh**

```bash
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
export PYTHONPATH="$REPO_ROOT"

echo "── Unit tests ──"
python3 -c "
import unittest
from lunarwing_mt_web import tests
unittest.TextTestRunner(verbosity=2).run(
    unittest.TestLoader().loadTestsFromModule(tests)
)
"

echo ""
echo "── Import check ──"
python3 -c "
from lunarwing_mt_web.server import app
from lunarwing_mt_web.bridge import start_provisioning, SessionState
print('All modules imported successfully')
"

echo ""
echo "── Flask test client ──"
python3 -c "
from lunarwing_mt_web.server import app
c = app.test_client()
r = c.get('/')
assert r.status_code == 200, f'Expected 200, got {r.status_code}'
print('GET / ->', r.status_code, 'OK')
r = c.post('/api/validate/name', json={'name': 'test'})
assert r.status_code == 200
print('POST /api/validate/name ->', r.status_code, 'OK')
"

echo ""
echo "✓ Smoke test passed"
```

- [ ] **Step 3: Commit**

```bash
git add lunarwing_mt_web/README.md lunarwing_mt_web/smoke_test.sh
chmod +x lunarwing_mt_web/smoke_test.sh
git update-index --chmod=+x lunarwing_mt_web/smoke_test.sh 2>/dev/null || true
git commit -m "Add README and smoke test script for web onboarding"
```
