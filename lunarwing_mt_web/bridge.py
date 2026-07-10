"""Bridge between the web layer and lunarwing_mt_onboard internals."""

from __future__ import annotations

import logging
import queue
import threading
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
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
        t0 = time.time()
        try:
            ensure_mt_admin()
        except FileNotFoundError as exc:
            session.push(f"[ERROR] {exc}")
            session.status = "fail"
            session.error = str(exc)
            return

        session.current_phase = "add-tenant"
        session.push("[PHASE] Starting: add-tenant")

        result = provision(
            config,
            on_output=_on_output,
        )

        for pr in result.phases:
            session.phases.append(
                {
                    "name": pr.name,
                    "ok": pr.ok,
                    "returncode": pr.returncode,
                }
            )
            _log_activity(config.name, pr.name, pr.returncode, time.time() - t0)
            status_tag = "OK" if pr.ok else "FAIL"
            session.push(f"[{status_tag}] {pr.name} (exit {pr.returncode})")

        if result.ok:
            session.current_phase = "verify"
            session.push("[PHASE] Starting: post-start verification")
            results = verify_tenant(
                config.name, config.gateway_host, config.gateway_port
            )
            for r in results:
                session.phases.append(
                    {
                        "name": f"verify:{r.label}",
                        "ok": r.ok,
                        "returncode": 0 if r.ok else 1,
                    }
                )
                status_tag = "OK" if r.ok else "FAIL"
                session.push(f"[{status_tag}] verify:{r.label}")
            session.status = "ok"
            session.current_phase = "complete"
        else:
            session.status = "fail"
            session.current_phase = "failed"

        session.push("[DONE]")

    thread = threading.Thread(target=_worker, daemon=True)
    thread.start()
    return session


def verify_existing(
    tenant: str, host: str = "127.0.0.1", port: int = 0
) -> list[dict[str, Any]]:
    """Run verify_tenant and return JSON-serializable results."""
    results = verify_tenant(tenant, host, port)
    return [{"label": r.label, "ok": r.ok} for r in results]
