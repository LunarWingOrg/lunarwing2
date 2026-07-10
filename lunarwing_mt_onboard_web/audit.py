"""Audit logging for every underlying invocation made through the web app.

Each job gets its own timestamped log file recording the redacted command
line, every streamed output line, and the phase/exit results. Secret values
(master keys, passwords, API keys, secret payloads) are never written to disk.
"""

from __future__ import annotations

import os
import re
import time
from datetime import datetime, timezone
from pathlib import Path

# key=value / key: value where the key name hints at a secret.
_KV_RE = re.compile(
    r"(?i)([A-Za-z0-9_]*(?:PASSWORD|SECRET|MASTER_KEY|API[_-]?KEY|TOKEN)[A-Za-z0-9_]*)"
    r"(\s*[=:]\s*)(['\"]?)([^\s'\"]+)"
)
# Bare 64-char hex (a secrets master key) appearing anywhere.
_HEX64_RE = re.compile(r"\b[0-9a-fA-F]{64}\b")
# argv flags whose following token is a secret value.
_SECRET_FLAGS = {"--xmpp-password", "--llm-api-key", "--secret", "--secretvalue", "--password"}

_REDACTED = "***REDACTED***"


def redact_text(text: str) -> str:
    """Return *text* with secret-looking values masked."""
    text = _KV_RE.sub(lambda m: f"{m.group(1)}{m.group(2)}{m.group(3)}{_REDACTED}", text)
    text = _HEX64_RE.sub(_REDACTED, text)
    return text


def redact_argv(argv: list[str]) -> list[str]:
    """Return a copy of *argv* with values after secret flags masked."""
    out: list[str] = []
    mask_next = False
    for tok in argv:
        if mask_next:
            out.append(_REDACTED)
            mask_next = False
            continue
        if tok in _SECRET_FLAGS:
            out.append(tok)
            mask_next = True
            continue
        out.append(redact_text(tok))
    return out


def default_log_dir() -> Path:
    """Preferred audit log directory: env override, then /var/log, then local."""
    env = os.environ.get("LUNARWING_ONBOARD_WEB_LOG_DIR")
    if env:
        p = Path(env)
        p.mkdir(parents=True, exist_ok=True)
        return p
    system = Path("/var/log/lunarwing-mt-onboard-web")
    try:
        system.mkdir(parents=True, exist_ok=True)
        probe = system / ".write-test"
        probe.write_text("")
        probe.unlink()
        return system
    except OSError:
        local = Path(__file__).resolve().parent / "logs"
        local.mkdir(parents=True, exist_ok=True)
        return local


class AuditLogger:
    """Append-only, secret-redacting log file for a single job."""

    def __init__(
        self,
        mode: str,
        tenant: str,
        job_id: str,
        *,
        log_dir: str | Path | None = None,
        demo: bool = False,
    ) -> None:
        self.dir = Path(log_dir) if log_dir else default_log_dir()
        self.dir.mkdir(parents=True, exist_ok=True)
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        safe_tenant = re.sub(r"[^A-Za-z0-9_-]", "_", tenant or "none")
        self.path = self.dir / f"{ts}-{mode}-{safe_tenant}-{job_id}.log"
        self._fh = self.path.open("a", encoding="utf-8")
        self._start = time.monotonic()
        tag = "DEMO" if demo else "REAL"
        self._raw(f"# lunarwing-mt-onboard-web audit log ({tag})")
        self._raw(f"# mode={mode} tenant={safe_tenant} job={job_id} started={ts}")

    def _raw(self, text: str) -> None:
        self._fh.write(text + "\n")
        self._fh.flush()

    def _stamp(self) -> str:
        return datetime.now(timezone.utc).strftime("%H:%M:%S")

    def command(self, argv: list[str]) -> None:
        self._raw(f"[{self._stamp()}] $ {' '.join(redact_argv(argv))}")

    def note(self, text: str) -> None:
        self._raw(f"[{self._stamp()}] # {redact_text(text)}")

    def line(self, text: str) -> None:
        self._raw(f"[{self._stamp()}] {redact_text(text)}")

    def phase_result(self, name: str, returncode: int) -> None:
        status = "OK" if returncode == 0 else "FAIL"
        self._raw(f"[{self._stamp()}] === phase '{name}' -> {status} (rc={returncode}) ===")

    def finish(self, ok: bool) -> None:
        dur = time.monotonic() - self._start
        verdict = "SUCCESS" if ok else "FAILURE"
        self._raw(f"# finished={verdict} duration={dur:.1f}s")
        self.close()

    def close(self) -> None:
        try:
            self._fh.close()
        except OSError:
            pass
