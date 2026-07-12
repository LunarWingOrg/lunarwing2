"""Demo / simulation mode.

Demo mode points five ``LUNARWING_*`` script-path environment variables at
bundled fake shell scripts that emit realistic ``=== phase ===`` output with
sleeps. This exercises the *real* provisioner/upgrade/export/import code paths
end-to-end with no sudo and zero system impact.

Two things can't be faked purely by swapping scripts, so we patch them:
  * ``provisioner._inject_secrets`` writes to /home/<tenant>/... which doesn't
    exist in demo — replaced with a no-op.
  * secrets mode talks to Postgres directly (not via a script) — the runner
    checks ``is_demo()`` and simulates the insert instead.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parent / "scripts"

_DEMO_ENV = {
    "LUNARWING_MT_ADMIN": SCRIPTS_DIR / "fake-mt-admin.sh",
    "LUNARWING_UPGRADE_SCRIPT": SCRIPTS_DIR / "fake-upgrade.sh",
    "LUNARWING_PREFLIGHT_SCRIPT": SCRIPTS_DIR / "fake-preflight.sh",
    "LUNARWING_EXPORT_SCRIPT": SCRIPTS_DIR / "fake-export.sh",
    "LUNARWING_IMPORT_SCRIPT": SCRIPTS_DIR / "fake-import.sh",
}

# Module globals in the parent package captured LUNARWING_*_SCRIPT at import
# time, so setting env vars alone isn't enough if those modules already loaded.
_PATCH_TARGETS = [
    ("lunarwing_mt_onboard.provisioner", "MT_ADMIN_SCRIPT", "LUNARWING_MT_ADMIN"),
    ("lunarwing_mt_onboard.upgrade", "UPGRADE_SCRIPT", "LUNARWING_UPGRADE_SCRIPT"),
    ("lunarwing_mt_onboard.upgrade", "PREFLIGHT_SCRIPT", "LUNARWING_PREFLIGHT_SCRIPT"),
    ("lunarwing_mt_onboard.export", "EXPORT_SCRIPT", "LUNARWING_EXPORT_SCRIPT"),
    ("lunarwing_mt_onboard.import_tenant", "IMPORT_SCRIPT", "LUNARWING_IMPORT_SCRIPT"),
]

_DEMO_FLAG = "LUNARWING_ONBOARD_WEB_DEMO"

FAKE_TENANTS = ["sphinx", "griffin", "chimera"]


def is_demo() -> bool:
    return os.environ.get(_DEMO_FLAG) == "1"


def enable() -> None:
    """Activate demo mode: set env, patch module globals, stub inject-secrets."""
    os.environ[_DEMO_FLAG] = "1"
    for key, path in _DEMO_ENV.items():
        os.environ[key] = str(path)

    for modname, attr, envkey in _PATCH_TARGETS:
        mod = sys.modules.get(modname)
        if mod is not None:
            setattr(mod, attr, os.environ[envkey])

    # Stub the real env-file mutation, which requires a provisioned /home tree.
    try:
        import lunarwing_mt_onboard.provisioner as pv

        pv._inject_secrets = _demo_noop  # type: ignore[attr-defined]
    except Exception:
        pass


def _demo_noop(*args: object, **kwargs: object) -> None:
    """No-op replacement for provisioner._inject_secrets in demo mode."""
    _ = (args, kwargs)
    return None


def fake_tenant_env(tenant: str) -> dict[str, str]:
    """A believable tenant env for the secrets demo (never hits a real DB)."""
    return {
        "DATABASE_URL": f"postgresql://demo@127.0.0.1:5432/lunarwing_{tenant}",
        "SECRETS_MASTER_KEY": "0" * 64,
        "LUNARWING_OWNER_ID": "default",
    }
