from __future__ import annotations

import argparse
from dataclasses import dataclass

from rich.console import Console
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

from lunarwing_mt_onboard.config import TenantConfig
from lunarwing_mt_onboard.provisioner import ProvisionResult, run_command
from lunarwing_mt_onboard.secrets_ops import tenant_gateway_host, tenant_gateway_port
from lunarwing_mt_onboard.upgrade import (
    TenantUpgradeConfig,
    build_mt_admin_upgrade_args,
)
from lunarwing_mt_onboard.verify import CheckResult, verify_tenant

console = Console()


@dataclass(frozen=True, slots=True)
class UpgradeCliArgs:
    tenant: str | None = None
    target: str = ""
    source_repo: str = ""
    no_backup: bool = False
    skip_render: bool = False
    apply: bool = False
    yes: bool = False
    non_interactive: bool = False
    accept_defaults: bool = False
    resume: str | None = None
    save: str | None = None

    @classmethod
    def from_namespace(cls, args: argparse.Namespace) -> UpgradeCliArgs:
        return cls(
            tenant=_optional_str(getattr(args, "tenant", None)),
            target=_str_arg(getattr(args, "target", "")),
            source_repo=_str_arg(getattr(args, "source_repo", "")),
            no_backup=_bool_arg(getattr(args, "no_backup", False)),
            skip_render=_bool_arg(getattr(args, "skip_render", False)),
            apply=_bool_arg(getattr(args, "apply", False)),
            yes=_bool_arg(getattr(args, "yes", False)),
            non_interactive=_bool_arg(getattr(args, "non_interactive", False)),
            accept_defaults=_bool_arg(getattr(args, "accept_defaults", False)),
            resume=_optional_str(getattr(args, "resume", None)),
            save=_optional_str(getattr(args, "save", None)),
        )


def _str_arg(value: str | bool | None) -> str:
    return value if isinstance(value, str) else ""


def _optional_str(value: str | bool | None) -> str | None:
    return value if isinstance(value, str) else None


def _bool_arg(value: str | bool | None) -> bool:
    return value if isinstance(value, bool) else False


def _q_text(label: str, *, default: str = "") -> str:
    try:
        import questionary

        result = questionary.text(label, default=default).ask()
        if result is None:
            raise KeyboardInterrupt
        return str(result)
    except KeyboardInterrupt:
        raise
    except Exception:
        prompt = f"{label}: " if not default else f"{label} [{default}]: "
        raw = input(prompt).strip()
        return raw or default


def _q_confirm(label: str, *, default: bool = False) -> bool:
    try:
        import questionary

        answer = questionary.confirm(label, default=default).ask()
        if answer is None:
            raise KeyboardInterrupt
        return bool(answer)
    except KeyboardInterrupt:
        raise
    except Exception:
        suffix = " [Y/n]" if default else " [y/N]"
        raw = input(f"{label}{suffix}: ").strip().lower()
        if not raw:
            return default
        return raw in ("y", "yes")


def gather_upgrade_config(config: TenantUpgradeConfig) -> TenantUpgradeConfig:
    console.print(
        Panel.fit(
            Text(
                "LunarWing In-Place Tenant Upgrade\n"
                "================================\n"
                "Uses lunarwing-mt-admin.sh upgrade-tenant for the current\n"
                "systemd-user/OpenRC lifecycle. This operation has no dry-run.",
                style="bold",
            ),
            title="lunarwing-mt-upgrade",
            border_style="yellow",
        )
    )

    tenant = _q_text("Tenant name (lowercase, e.g. 'sphinx')", default=config.tenant)
    error = TenantConfig.validate_name(tenant)
    while error:
        console.print(f"[red]{error}[/]")
        tenant = _q_text("Tenant name", default=config.tenant)
        error = TenantConfig.validate_name(tenant)
    config.tenant = tenant

    config.target = _q_text(
        "Target branch, tag, or commit (required)", default=config.target
    )
    config.source_repo = _q_text(
        "Source repository override (optional)", default=config.source_repo
    )
    config.no_backup = not _q_confirm(
        "Create a pre-upgrade PostgreSQL backup?", default=not config.no_backup
    )
    config.skip_render = not _q_confirm(
        "Re-render service units?", default=not config.skip_render
    )
    config.apply = _q_confirm(
        "Apply the upgrade now? This stops and modifies the tenant.", default=False
    )
    return config


def _show_upgrade_summary(config: TenantUpgradeConfig) -> bool:
    table = Table(title=f"Upgrade summary: {config.tenant}", show_lines=True)
    table.add_column("Field", style="bold cyan")
    table.add_column("Value", style="white")
    table.add_row("Tenant", config.tenant)
    table.add_row("Target ref", config.target)
    table.add_row("Source repository", config.source_repo or "current origin")
    table.add_row("PostgreSQL backup", "skip" if config.no_backup else "create")
    table.add_row("Render service units", "skip" if config.skip_render else "yes")
    table.add_row("Mode", "APPLY")
    console.print(table)
    return _q_confirm("Proceed with upgrade?", default=False)


def _display_upgrade_results(result: ProvisionResult) -> None:
    table = Table(title="Upgrade result")
    table.add_column("Phase", style="bold cyan")
    table.add_column("Status", justify="center")
    table.add_column("Code", justify="right")
    for name, ok, code in result.summary():
        status = Text("OK", style="green") if ok else Text("FAIL", style="red")
        table.add_row(name, status, str(code))
    console.print(table)
    console.print(
        "Upgrade command completed." if result.ok else "Upgrade FAILED.",
        style="bold green" if result.ok else "bold red",
    )


def _show_verify(results: list[CheckResult]) -> bool:
    table = Table(title="Post-upgrade verification")
    table.add_column("Check", style="bold cyan")
    table.add_column("Status", justify="center")
    table.add_column("Detail")
    for check in results:
        status = Text("PASS", style="green") if check.ok else Text("FAIL", style="red")
        table.add_row(check.label, status, check.detail)
    console.print(table)
    return bool(results) and all(check.ok for check in results)


def run_upgrade_flow(args: UpgradeCliArgs) -> int:
    config = TenantUpgradeConfig()
    if args.resume:
        try:
            config = TenantUpgradeConfig.from_json(args.resume)
        except Exception as exc:
            console.print(f"[red]Failed to load --resume file:[/] {exc}")
            return 1
    elif not (args.non_interactive or args.accept_defaults):
        try:
            config = gather_upgrade_config(config)
        except KeyboardInterrupt:
            console.print("\nAborted.")
            return 130

    if args.tenant:
        config.tenant = args.tenant
    if args.target:
        config.target = args.target
    if args.source_repo:
        config.source_repo = args.source_repo
    if args.no_backup:
        config.no_backup = True
    if args.skip_render:
        config.skip_render = True
    if args.apply:
        config.apply = True

    error = config.validate()
    if error:
        console.print(f"[red]Invalid config:[/] {error}")
        return 1
    if not config.apply:
        console.print("[red]Upgrade requires explicit --apply confirmation.[/]")
        return 1

    try:
        command = build_mt_admin_upgrade_args(config)
    except FileNotFoundError as exc:
        console.print(f"[red]{exc}[/]")
        return 1

    if args.save:
        config.to_json(args.save)
        console.print(f"Saved config to [cyan]{args.save}[/]")

    should_run = (
        args.non_interactive
        or args.accept_defaults
        or args.yes
        or _show_upgrade_summary(config)
    )
    if not should_run:
        console.print("Aborted.")
        return 130

    phase = run_command(
        command,
        on_output=lambda line: console.print(line, highlight=False),
        phase_name="upgrade",
    )
    result = ProvisionResult(phases=[phase])
    _display_upgrade_results(result)
    if not result.ok:
        return 2

    gateway_port = tenant_gateway_port(config.tenant)
    gateway_host = tenant_gateway_host(config.tenant)
    checks = verify_tenant(config.tenant, host=gateway_host, port=gateway_port)
    if not gateway_port:
        checks.insert(
            0,
            CheckResult(
                "gateway port",
                False,
                "gateway allocation not found in ports registry",
            ),
        )
    return 0 if _show_verify(checks) else 2
