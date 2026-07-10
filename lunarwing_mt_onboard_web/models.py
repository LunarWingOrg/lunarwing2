"""Pydantic request models that map onto the existing onboarding dataclasses.

The web layer never re-implements provisioning logic — it only translates JSON
request bodies into the same ``TenantConfig`` / ``UpgradeConfig`` /
``ExportConfig`` objects the CLI builds, then hands them to the reused runners.
"""

from __future__ import annotations

from pydantic import BaseModel, Field

from lunarwing_mt_onboard.config import TenantConfig, WorkerType
from lunarwing_mt_onboard.export import DEFAULT_OUT_DIR, ExportConfig
from lunarwing_mt_onboard.upgrade import UpgradeConfig

_VALID_WORKERS = {w.value for w in WorkerType}


class ProvisionRequest(BaseModel):
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
    tensorzero_url: str = "http://192.168.1.157:3000/openai/v1"
    llm_model: str = "tensorzero::function_name::lunarwing"
    llm_api_key: str = ""
    secrets_master_key: str = ""
    no_ssh: bool = False
    no_health: bool = False
    skip_build: bool = False
    skip_start: bool = False

    def to_tenant_config(self) -> TenantConfig:
        workers = [WorkerType(w) for w in self.workers if w in _VALID_WORKERS]
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
            tensorzero_url=self.tensorzero_url.strip(),
            llm_model=self.llm_model.strip(),
            llm_api_key=self.llm_api_key,
            secrets_master_key=self.secrets_master_key.strip(),
            no_ssh=self.no_ssh,
            no_health=self.no_health,
        )


class UpgradeRequest(BaseModel):
    """In-place tenant upgrade form (mirrors upgrade_cli)."""

    tenant: str = ""
    target: str = ""
    apply: bool = False
    auto_yes: bool = False
    force: bool = False
    source_version_override: str = ""
    run_preflight: bool = True

    def to_upgrade_config(self) -> UpgradeConfig:
        return UpgradeConfig(
            tenant=self.tenant.strip(),
            target=self.target.strip(),
            apply=self.apply,
            auto_yes=self.auto_yes,
            force=self.force,
            source_version_override=self.source_version_override.strip(),
            run_preflight=self.run_preflight,
        )


class ExportRequest(BaseModel):
    """Kawarimi export form (mirrors export_cli)."""

    tenant: str = ""
    out_dir: str = DEFAULT_OUT_DIR
    apply: bool = False
    no_quiesce: bool = False

    def to_export_config(self) -> ExportConfig:
        return ExportConfig(
            tenant=self.tenant.strip(),
            out_dir=self.out_dir.strip() or DEFAULT_OUT_DIR,
            apply=self.apply,
            no_quiesce=self.no_quiesce,
        )


class SecretRequest(BaseModel):
    """A single secret to insert into a tenant's encrypted store."""

    tenant: str = ""
    name: str = ""
    value: str = ""
