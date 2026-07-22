"""Pydantic request models that map onto the existing onboarding dataclasses.

The web layer never re-implements provisioning logic — it only translates JSON
request bodies into the same ``TenantConfig`` / ``UpgradeConfig`` /
``ExportConfig`` / ``ImportConfig`` objects the CLI builds, then hands them to
the reused runners.
"""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field, SecretStr, model_validator

from lunarwing_mt_onboard.config import TenantConfig, WorkerType
from lunarwing_mt_onboard.export import DEFAULT_OUT_DIR, ExportConfig
from lunarwing_mt_onboard.import_tenant import ImportConfig
from lunarwing_mt_onboard.kawarimi_secret import validate_passphrase
from lunarwing_mt_onboard.secrets import is_valid_master_key
from lunarwing_mt_onboard.upgrade import TenantUpgradeConfig

_VALID_WORKERS = {w.value for w in WorkerType}


class RequestModel(BaseModel):
    """Base request policy that keeps invalid input out of error strings."""

    model_config = ConfigDict(hide_input_in_errors=True, extra="forbid")


class ProvisionRequest(RequestModel):
    """New-tenant provisioning form (mirrors cli.gather_config)."""

    name: str = ""
    gateway_host: str = "127.0.0.1"
    docker_group: bool = True
    enable_darkirc: bool = False
    xmpp_enabled: bool = False
    xmpp_jid: str = ""
    xmpp_password: str = ""
    xmpp_allow_from: list[str] = Field(default_factory=list)
    gotify_enabled: bool = False
    gotify_url: str = ""
    gotify_title: str = ""
    workers: list[str] = Field(default_factory=list)
    toolchains: bool = False
    llm_base_url: str = ""
    llm_model: str = "tensorzero::function_name::lunarwing"
    nanocode_model: str = ""
    nanocode_base_url: str = ""
    opencode_model: str = ""
    opencode_base_url: str = ""
    llm_api_key: str = ""
    secrets_master_key: str = ""
    no_ssh: bool = False
    no_health: bool = False
    no_weechat_bootstrap: bool = False
    skip_build: bool = False
    skip_start: bool = False

    @model_validator(mode="after")
    def validate_request(self) -> "ProvisionRequest":
        master_key = self.secrets_master_key.strip()
        if master_key and not is_valid_master_key(master_key):
            raise ValueError("secrets master key must be exactly 64 hexadecimal characters")
        if self.skip_build and not self.skip_start:
            raise ValueError("skip start-tenant when build-tenant is skipped")
        unknown_workers = sorted(set(self.workers) - _VALID_WORKERS)
        if unknown_workers:
            raise ValueError(f"unknown workers: {', '.join(unknown_workers)}")
        return self

    def to_tenant_config(self) -> TenantConfig:
        workers = [WorkerType(w) for w in self.workers]
        return TenantConfig(
            name=self.name.strip(),
            gateway_host=self.gateway_host.strip() or "127.0.0.1",
            docker_group=self.docker_group,
            enable_darkirc=self.enable_darkirc,
            xmpp_enabled=self.xmpp_enabled,
            xmpp_jid=self.xmpp_jid.strip(),
            xmpp_password=self.xmpp_password,
            xmpp_allow_from=[j.strip() for j in self.xmpp_allow_from if j.strip()],
            gotify_enabled=self.gotify_enabled,
            gotify_url=self.gotify_url.strip(),
            gotify_title=self.gotify_title.strip(),
            workers=workers,
            toolchains=self.toolchains,
            llm_base_url=self.llm_base_url.strip(),
            llm_model=self.llm_model.strip(),
            nanocode_model=(
                self.nanocode_model.strip()
                if WorkerType.NANOCODE in workers
                else ""
            ),
            nanocode_base_url=(
                self.nanocode_base_url.strip()
                if WorkerType.NANOCODE in workers
                else ""
            ),
            opencode_model=(
                self.opencode_model.strip()
                if WorkerType.OPENCODE in workers
                else ""
            ),
            opencode_base_url=(
                self.opencode_base_url.strip()
                if WorkerType.OPENCODE in workers
                else ""
            ),
            llm_api_key=self.llm_api_key,
            secrets_master_key=self.secrets_master_key.strip(),
            no_ssh=self.no_ssh,
            no_health=self.no_health,
            no_weechat_bootstrap=self.no_weechat_bootstrap,
        )


class UpgradeRequest(RequestModel):
    """Current init-agnostic in-place tenant upgrade form."""

    tenant: str = ""
    target: str = ""
    source_repo: str = ""
    no_backup: bool = False
    skip_render: bool = False
    apply: bool = False

    @model_validator(mode="after")
    def validate_request(self) -> "UpgradeRequest":
        config = self.to_upgrade_config()
        error = config.validate()
        if error:
            raise ValueError(error)
        if not self.apply:
            raise ValueError("explicit upgrade confirmation is required")
        return self

    def to_upgrade_config(self) -> TenantUpgradeConfig:
        return TenantUpgradeConfig(
            tenant=self.tenant.strip(),
            target=self.target.strip(),
            source_repo=self.source_repo.strip(),
            no_backup=self.no_backup,
            skip_render=self.skip_render,
            apply=self.apply,
        )


class ExportRequest(RequestModel):
    """Kawarimi export form (mirrors export_cli)."""

    tenant: str = ""
    out_dir: str = DEFAULT_OUT_DIR
    apply: bool = False
    no_quiesce: bool = False
    passphrase: SecretStr = Field(
        default_factory=lambda: SecretStr(""), exclude=True, repr=False
    )
    passphrase_confirm: SecretStr = Field(
        default_factory=lambda: SecretStr(""), exclude=True, repr=False
    )

    @model_validator(mode="after")
    def validate_passphrase(self) -> "ExportRequest":
        passphrase = self.passphrase.get_secret_value()
        confirmation = self.passphrase_confirm.get_secret_value()
        if self.apply:
            error = validate_passphrase(passphrase, min_length=12)
            if error:
                raise ValueError(error)
            if passphrase != confirmation:
                raise ValueError("passphrase confirmation does not match")
        return self

    def to_export_config(self) -> ExportConfig:
        return ExportConfig(
            tenant=self.tenant.strip(),
            out_dir=self.out_dir.strip() or DEFAULT_OUT_DIR,
            apply=self.apply,
            no_quiesce=self.no_quiesce,
            passphrase=self.passphrase.get_secret_value(),
        )


class ImportRequest(RequestModel):
    """Kawarimi import form."""

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
    passphrase: SecretStr = Field(
        default_factory=lambda: SecretStr(""), exclude=True, repr=False
    )

    @model_validator(mode="after")
    def validate_passphrase(self) -> "ImportRequest":
        if self.start and not self.old_stopped:
            raise ValueError(
                "starting an imported tenant requires confirmation that the old host is stopped"
            )
        if self.bundle.strip().lower().endswith(".7z"):
            error = validate_passphrase(self.passphrase.get_secret_value())
            if error:
                raise ValueError(error)
        return self

    def to_import_config(self) -> ImportConfig:
        return ImportConfig(
            bundle=self.bundle.strip(),
            name=self.name.strip(),
            start=self.start,
            old_stopped=self.old_stopped,
            with_nanocode=self.with_nanocode,
            with_pebble=self.with_pebble,
            with_opencode=self.with_opencode,
            with_toolchains=self.with_toolchains,
            with_vision=self.with_vision,
            docker_group=self.docker_group,
            owner_scope=self.owner_scope.strip(),
            apply=self.apply,
            force=self.force,
            auto_yes=True,
            passphrase=self.passphrase.get_secret_value(),
        )


class SecretRequest(RequestModel):
    """A single secret to insert into a tenant's encrypted store."""

    tenant: str = ""
    name: str = ""
    value: str = ""
