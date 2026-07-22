"""Kawarimi import orchestration: wrapper around ``import-tenant.sh``.

``ImportConfig`` captures every choice for restoring a tenant migration bundle
on a new host. It serializes to/from JSON so an import session can be saved and
resumed.

Defaults to a non-interactive, stage-only dry-run. Operators must opt in with
``apply=True`` for a real import and ``start=True`` to start the restored tenant.
"""

from __future__ import annotations

import json
import os
import shutil
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

from lunarwing_mt_onboard.config import TenantConfig
from lunarwing_mt_onboard.kawarimi_secret import (
    passphrase_transport,
    validate_passphrase,
)
from lunarwing_mt_onboard.provisioner import ProvisionResult, run_command

IMPORT_SCRIPT = os.environ.get(
    "LUNARWING_IMPORT_SCRIPT",
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "ic",
        "scripts",
        "import-tenant.sh",
    ),
)

ImportValue = str | bool


class ImportConfigFormatError(Exception):
    pass


@dataclass
class ImportConfig:
    """All parameters for a Kawarimi tenant import."""

    bundle: str = ""
    name: str = ""
    start: bool = False
    old_stopped: bool = False
    with_nanocode: bool = False
    with_pebble: bool = False
    with_opencode: bool = False
    with_toolchains: bool = False
    with_vision: bool = False
    docker_group: bool = False
    owner_scope: str = ""
    apply: bool = False
    force: bool = False
    auto_yes: bool = True
    passphrase: str = field(default="", repr=False, compare=False)

    def validate(self) -> str | None:
        if not self.bundle.strip():
            return "bundle path is required"
        if self.name:
            error = TenantConfig.validate_name(self.name)
            if error:
                return error
        if self.passphrase:
            return validate_passphrase(self.passphrase)
        return None

    def to_dict(self) -> dict[str, ImportValue]:
        return {
            "bundle": self.bundle,
            "name": self.name,
            "start": self.start,
            "old_stopped": self.old_stopped,
            "with_nanocode": self.with_nanocode,
            "with_pebble": self.with_pebble,
            "with_opencode": self.with_opencode,
            "with_toolchains": self.with_toolchains,
            "with_vision": self.with_vision,
            "docker_group": self.docker_group,
            "owner_scope": self.owner_scope,
            "apply": self.apply,
            "force": self.force,
            "auto_yes": self.auto_yes,
        }

    def to_json(self, path: str | Path) -> None:
        p = Path(path)
        fd = os.open(str(p), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            _ = f.write(json.dumps(self.to_dict(), indent=2) + "\n")

    @classmethod
    def from_dict(cls, data: dict[str, ImportValue]) -> ImportConfig:
        return cls(
            bundle=_str_value(data.get("bundle", "")),
            name=_str_value(data.get("name", "")),
            start=_bool_value(data.get("start", False)),
            old_stopped=_bool_value(data.get("old_stopped", False)),
            with_nanocode=_bool_value(data.get("with_nanocode", False)),
            with_pebble=_bool_value(data.get("with_pebble", False)),
            with_opencode=_bool_value(data.get("with_opencode", False)),
            with_toolchains=_bool_value(data.get("with_toolchains", False)),
            with_vision=_bool_value(data.get("with_vision", False)),
            docker_group=_bool_value(data.get("docker_group", False)),
            owner_scope=_str_value(data.get("owner_scope", "")),
            apply=_bool_value(data.get("apply", False)),
            force=_bool_value(data.get("force", False)),
            auto_yes=_bool_value(data.get("auto_yes", True)),
        )

    @classmethod
    def from_json(cls, path: str | Path) -> ImportConfig:
        raw: ImportJson = json.loads(Path(path).read_text())
        if not isinstance(raw, dict):
            raise ImportConfigFormatError("import config JSON must be an object")
        data: dict[str, ImportValue] = {}
        for key, value in raw.items():
            if isinstance(key, str) and isinstance(value, (str, bool)):
                data[key] = value
        return cls.from_dict(data)


def _str_value(value: ImportValue) -> str:
    return value if isinstance(value, str) else ""


def _bool_value(value: ImportValue) -> bool:
    return value if isinstance(value, bool) else False


def ensure_import_script() -> str:
    if os.path.isfile(IMPORT_SCRIPT) and os.access(IMPORT_SCRIPT, os.X_OK):
        return IMPORT_SCRIPT
    found = shutil.which("import-tenant.sh")
    if found:
        return found
    raise FileNotFoundError(
        f"import-tenant.sh not found at {IMPORT_SCRIPT}. Set LUNARWING_IMPORT_SCRIPT env var."
    )


def build_import_args(cfg: ImportConfig) -> list[str]:
    script = ensure_import_script()
    args: list[str] = [script, cfg.bundle]
    if cfg.name:
        args.extend(["--name", cfg.name])
    if cfg.start:
        args.append("--start")
    if cfg.old_stopped:
        args.append("--old-stopped")
    if cfg.with_nanocode:
        args.append("--with-nanocode")
    if cfg.with_pebble:
        args.append("--with-pebble")
    if cfg.with_opencode:
        args.append("--with-opencode")
    if cfg.with_toolchains:
        args.append("--with-toolchains")
    if cfg.with_vision:
        args.append("--with-vision")
    if cfg.docker_group:
        args.append("--docker-group")
    if cfg.owner_scope:
        args.extend(["--owner-scope", cfg.owner_scope])
    if not cfg.apply:
        args.append("--dry-run")
    if cfg.auto_yes:
        args.append("--yes")
    if cfg.force:
        args.append("--force")
    return args


def run_import(
    cfg: ImportConfig,
    *,
    on_output: Callable[[str], None] | None = None,
) -> ProvisionResult:
    result = ProvisionResult()
    with passphrase_transport(cfg.passphrase) as secret:
        import_result = run_command(
            build_import_args(cfg),
            env=secret.env,
            pass_fds=secret.pass_fds,
            on_output=on_output,
            phase_name="import",
        )
    result.phases.append(import_result)
    return result


ImportJson = dict[str, str | bool] | list[str | bool] | str | bool | int | float | None
