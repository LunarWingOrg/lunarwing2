"""Tenant configuration dataclass and JSON serialization.

``TenantConfig`` captures every choice the interactive CLI collects and can
serialize to/from JSON so that a provisioning session can be saved and
resumed later via ``--resume <file>``.
"""

from __future__ import annotations

import json
import os
import re
import warnings
from dataclasses import dataclass, field, asdict
from enum import Enum
from pathlib import Path
from typing import Any

TENANT_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")

RESERVED_PREFIXES = (
    "pg-",
    "proxy-",
    "nanocode-",
    "pebble-",
    "opencode-",
    "kimi-",
    "opencode2-",
    "weechat-",
)


class WorkerType(str, Enum):
    """Selectable external worker container types."""

    PEBBLE = "pebble"
    # WI#333 retired the opencode v1 lane in v2.1.8.0. The member stays so an
    # older saved plan still parses; from_dict drops it with a notice.
    OPENCODE = "opencode"
    KIMI = "kimi"
    OPENCODE2 = "opencode2"


# WI#333: one wording for every caller that drops the retired v1 selection.
RETIRED_OPENCODE_NOTICE = (
    "opencode v1 worker lane is retired (WI#333); dropping the opencode "
    "selection — use opencode2 for the supported lane"
)


def warn_retired_opencode() -> None:
    """Announce that a retired opencode v1 selection is being dropped."""
    warnings.warn(RETIRED_OPENCODE_NOTICE, RuntimeWarning, stacklevel=3)


def without_retired_opencode(workers: list[WorkerType]) -> list[WorkerType]:
    """Return *workers* minus the retired opencode v1 lane (WI#333)."""
    return [w for w in workers if w is not WorkerType.OPENCODE]


@dataclass
class TenantConfig:
    """All parameters collected during interactive provisioning."""

    name: str = ""
    gateway_host: str = "127.0.0.1"
    gateway_port: int = 0
    # WI#389: rootless podman (the MT default runtime) needs no group
    # membership, so opting in must be a deliberate choice, never the default.
    docker_group: bool = False
    enable_darkirc: bool = False
    xmpp_enabled: bool = False
    xmpp_jid: str = ""
    xmpp_password: str = ""
    xmpp_allow_from: list[str] = field(default_factory=list)
    gotify_enabled: bool = False
    gotify_url: str = ""
    gotify_title: str = ""
    workers: list[WorkerType] = field(default_factory=list)
    toolchains: bool = False
    llm_base_url: str = ""
    llm_model: str = "tensorzero::function_name::lunarwing"
    opencode_model: str = ""
    opencode_base_url: str = ""
    opencode2_model: str = ""
    opencode2_base_url: str = ""
    kimi_model: str = ""
    kimi_base_url: str = ""
    kimi_api_key: str = ""
    llm_api_key: str = ""
    secrets_master_key: str = ""
    no_ssh: bool = False
    no_health: bool = False
    no_weechat_bootstrap: bool = False

    # ------------------------------------------------------------------
    # Validation
    # ------------------------------------------------------------------

    @staticmethod
    def validate_name(name: str) -> str | None:
        """Return an error message if *name* is invalid, ``None`` if OK."""
        if not name:
            return "tenant name is required"
        if not TENANT_NAME_RE.match(name):
            return "tenant name must be lowercase alphanumeric with hyphens"
        if any(name.startswith(p) for p in RESERVED_PREFIXES):
            return (
                "tenant name collides with a reserved service-unit prefix "
                f"({', '.join(RESERVED_PREFIXES)})"
            )
        return None

    # ------------------------------------------------------------------
    # Serialization
    # ------------------------------------------------------------------

    def to_dict(self) -> dict[str, Any]:
        """Return a plain ``dict`` suitable for JSON dumps."""
        d = asdict(self)
        d["workers"] = [w.value for w in self.workers]
        return d

    def to_json(self, path: str | Path) -> None:
        """Write this config to *path* as JSON with 0600 at creation time."""
        p = Path(path)
        fd = os.open(str(p), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            f.write(json.dumps(self.to_dict(), indent=2) + "\n")

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "TenantConfig":
        """Reconstruct a config from a deserialized dict."""
        workers_raw = data.get("workers", [])
        if "nanocode" in workers_raw:
            raise ValueError("Nanocode worker is retired; remove it from the saved plan")
        workers = [WorkerType(w) for w in workers_raw]
        if WorkerType.OPENCODE in workers:
            # WI#333: a plan saved before the retirement keeps loading, minus the
            # retired lane, so a resumed provisioning run does not die later.
            warn_retired_opencode()
            workers = without_retired_opencode(workers)
        return cls(
            name=data.get("name", ""),
            gateway_host=data.get("gateway_host", "127.0.0.1"),
            gateway_port=data.get("gateway_port", 0),
            docker_group=data.get("docker_group", False),
            enable_darkirc=data.get("enable_darkirc", False),
            xmpp_enabled=data.get("xmpp_enabled", False),
            xmpp_jid=data.get("xmpp_jid", ""),
            xmpp_password=data.get("xmpp_password", ""),
            xmpp_allow_from=list(data.get("xmpp_allow_from", [])),
            gotify_enabled=data.get("gotify_enabled", False),
            gotify_url=data.get("gotify_url", ""),
            gotify_title=data.get("gotify_title", ""),
            workers=workers,
            toolchains=data.get("toolchains", False),
            llm_base_url=data.get("llm_base_url", ""),
            llm_model=data.get(
                "llm_model", "tensorzero::function_name::lunarwing"
            ),
            opencode_model=data.get("opencode_model", ""),
            opencode_base_url=data.get("opencode_base_url", ""),
            opencode2_model=data.get("opencode2_model", ""),
            opencode2_base_url=data.get("opencode2_base_url", ""),
            kimi_model=data.get("kimi_model", ""),
            kimi_base_url=data.get("kimi_base_url", ""),
            kimi_api_key=data.get("kimi_api_key", ""),
            llm_api_key=data.get("llm_api_key", ""),
            secrets_master_key=data.get("secrets_master_key", ""),
            no_ssh=data.get("no_ssh", False),
            no_health=data.get("no_health", False),
            no_weechat_bootstrap=data.get("no_weechat_bootstrap", False),
        )

    @classmethod
    def from_json(cls, path: str | Path) -> "TenantConfig":
        """Load a config previously written by :meth:`to_json`."""
        data = json.loads(Path(path).read_text())
        return cls.from_dict(data)
