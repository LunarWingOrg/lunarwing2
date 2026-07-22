"""``ImportPlan``: every choice for one Hermes -> LunarWing import.

Serializes to/from JSON so an import can be saved and resumed, mirroring
``lunarwing_mt_onboard.import_tenant.ImportConfig``. Defaults are safe: no
writes (``apply=False``) and no service start (``start=False``).
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

PlanValue = str | bool


class ImportPlanFormatError(Exception):
    pass


@dataclass
class ImportPlan:
    """Parameters for a single Hermes agent import."""

    source: str = ""  # Hermes home dir (empty => HERMES_HOME env / ~/.hermes)
    tenant: str = ""  # target LunarWing tenant name (also the owner scope)
    apply: bool = False
    start: bool = False
    old_stopped: bool = False
    force: bool = False
    docker_group: bool = False
    with_nanocode: bool = False
    with_pebble: bool = False
    with_opencode: bool = False
    with_toolchains: bool = False
    with_vision: bool = False
    tensorzero_url: str = ""
    llm_model: str = ""

    def validate(self) -> str | None:
        if not self.tenant.strip():
            return "tenant name is required"
        # Reuse the mt-admin-compatible name rules; imported lazily so this
        # module stays importable without the sibling package present.
        try:
            from lunarwing_mt_onboard.config import TenantConfig

            return TenantConfig.validate_name(self.tenant)
        except ImportError:
            return None

    def to_dict(self) -> dict[str, PlanValue]:
        return {
            "source": self.source,
            "tenant": self.tenant,
            "apply": self.apply,
            "start": self.start,
            "old_stopped": self.old_stopped,
            "force": self.force,
            "docker_group": self.docker_group,
            "with_nanocode": self.with_nanocode,
            "with_pebble": self.with_pebble,
            "with_opencode": self.with_opencode,
            "with_toolchains": self.with_toolchains,
            "with_vision": self.with_vision,
            "tensorzero_url": self.tensorzero_url,
            "llm_model": self.llm_model,
        }

    def to_json(self, path: str | Path) -> None:
        p = Path(path)
        fd = os.open(str(p), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            _ = f.write(json.dumps(self.to_dict(), indent=2) + "\n")

    @classmethod
    def from_dict(cls, data: dict[str, PlanValue]) -> ImportPlan:
        return cls(
            source=_str(data.get("source", "")),
            tenant=_str(data.get("tenant", "")),
            apply=_bool(data.get("apply", False)),
            start=_bool(data.get("start", False)),
            old_stopped=_bool(data.get("old_stopped", False)),
            force=_bool(data.get("force", False)),
            docker_group=_bool(data.get("docker_group", False)),
            with_nanocode=_bool(data.get("with_nanocode", False)),
            with_pebble=_bool(data.get("with_pebble", False)),
            with_opencode=_bool(data.get("with_opencode", False)),
            with_toolchains=_bool(data.get("with_toolchains", False)),
            with_vision=_bool(data.get("with_vision", False)),
            tensorzero_url=_str(data.get("tensorzero_url", "")),
            llm_model=_str(data.get("llm_model", "")),
        )

    @classmethod
    def from_json(cls, path: str | Path) -> ImportPlan:
        raw = json.loads(Path(path).read_text())
        if not isinstance(raw, dict):
            raise ImportPlanFormatError("import plan JSON must be an object")
        data: dict[str, PlanValue] = {}
        for key, value in raw.items():
            if isinstance(key, str) and isinstance(value, (str, bool)):
                data[key] = value
        return cls.from_dict(data)


def _str(value: object) -> str:
    return value if isinstance(value, str) else ""


def _bool(value: object) -> bool:
    return value if isinstance(value, bool) else False
