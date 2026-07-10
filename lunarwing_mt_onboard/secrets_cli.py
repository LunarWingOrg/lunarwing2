from __future__ import annotations

import argparse
import getpass
from dataclasses import dataclass

from rich.console import Console
from rich.panel import Panel
from rich.text import Text

from lunarwing_mt_onboard import secrets_ops

console = Console()


@dataclass(frozen=True, slots=True)
class SecretsCliArgs:
    tenant: str | None = None
    secretname: str = ""
    secretvalue: str = ""
    non_interactive: bool = False
    yes: bool = False

    @classmethod
    def from_namespace(cls, args: argparse.Namespace) -> SecretsCliArgs:
        return cls(
            tenant=_optional_str(getattr(args, "tenant", None)),
            secretname=_str_arg(getattr(args, "secretname", "")),
            secretvalue=_str_arg(getattr(args, "secretvalue", "")),
            non_interactive=_bool_arg(getattr(args, "non_interactive", False)),
            yes=_bool_arg(getattr(args, "yes", False)),
        )


# ---------------------------------------------------------------------------
# Namespace coercion helpers
# ---------------------------------------------------------------------------


def _str_arg(value: str | bool | None) -> str:
    return value if isinstance(value, str) else ""


def _optional_str(value: str | bool | None) -> str | None:
    return value if isinstance(value, str) else None


def _bool_arg(value: str | bool | None) -> bool:
    return value if isinstance(value, bool) else False


# ---------------------------------------------------------------------------
# Interactive prompt helpers (questionary with input()/getpass fallback)
# ---------------------------------------------------------------------------


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


def _q_password(label: str) -> str:
    try:
        import questionary

        result = questionary.password(label).ask()
        if result is None:
            raise KeyboardInterrupt
        return str(result)
    except KeyboardInterrupt:
        raise
    except Exception:
        return getpass.getpass(f"{label}: ")


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


def _q_select(label: str, choices: list[str], *, default: int = 0) -> int:
    try:
        import questionary

        answer = questionary.select(label, choices=choices).ask()
        if answer is None:
            raise KeyboardInterrupt
        return choices.index(answer) if answer in choices else default
    except KeyboardInterrupt:
        raise
    except Exception:
        for i, c in enumerate(choices, 1):
            marker = "*" if i - 1 == default else " "
            console.print(f"  {marker} {i}) {c}")
        raw = input(f"{label} [{default + 1}]: ").strip()
        if not raw.isdigit():
            return default
        idx = int(raw) - 1
        return idx if 0 <= idx < len(choices) else default


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------


def run_secrets_flow(args: SecretsCliArgs) -> int:
    """Main entry point for the secrets subcommand.

    Returns exit code: 0 success, 1 error, 130 aborted.
    """
    # --- 1. Dependency check ------------------------------------------------
    if not secrets_ops.ensure_dependencies():
        if args.non_interactive:
            console.print(
                "[red]Missing required dependencies for secrets operations.[/]"
            )
            return 1
        console.print(
            Panel.fit(
                Text(
                    "Some dependencies required for secrets operations are missing.",
                    style="bold yellow",
                ),
                title="dependencies",
                border_style="yellow",
            )
        )
        if not _q_confirm("Install missing dependencies now?", default=True):
            console.print("Aborted.")
            return 130
        try:
            secrets_ops.install_dependencies()
        except Exception as exc:
            console.print(f"[red]Failed to install dependencies:[/] {exc}")
            return 1
        if not secrets_ops.ensure_dependencies():
            console.print(
                "[red]Dependencies still missing after install attempt. Aborting.[/]"
            )
            return 1

    # --- 2. Tenant selection ------------------------------------------------
    tenant: str | None = args.tenant
    if not tenant:
        if args.non_interactive:
            console.print("[red]Error:[/] --tenant is required in non-interactive mode.")
            return 1
        tenants = secrets_ops.list_tenants()
        if not tenants:
            console.print("[red]No tenants found on this host.[/]")
            return 1
        idx = _q_select("Select tenant", tenants, default=0)
        tenant = tenants[idx]

    # --- 3. Env resolution --------------------------------------------------
    try:
        env = secrets_ops.parse_tenant_env(tenant)
    except Exception as exc:
        console.print(f"[red]Failed to parse tenant environment:[/] {exc}")
        return 1

    required_keys = ("DATABASE_URL", "SECRETS_MASTER_KEY", "LUNARWING_OWNER_ID")
    for key in required_keys:
        val = env.get(key)
        if not val:
            env_path = env.get("_env_file", "<tenant .env>")
            console.print(
                f"[red]Missing required key '{key}' in tenant environment ({env_path}).[/]"
            )
            return 1

    db_url = env["DATABASE_URL"]
    master_key = env["SECRETS_MASTER_KEY"]
    owner_id = env["LUNARWING_OWNER_ID"]

    # --- 4. Branch ----------------------------------------------------------
    if args.non_interactive:
        if not args.secretname:
            console.print("[red]Error:[/] --secretname is required in non-interactive mode.")
            return 1
        if not args.secretvalue:
            console.print("[red]Error:[/] --secretvalue is required in non-interactive mode.")
            return 1
        try:
            secrets_ops.insert_secret(db_url, master_key, owner_id, args.secretname, args.secretvalue)
        except Exception as exc:
            console.print(f"[red]Failed to insert secret:[/] {exc}")
            return 1
        console.print(
            Panel.fit(
                Text(
                    f"OK: secret '{args.secretname}' stored for tenant '{tenant}'",
                    style="bold green",
                ),
                title="secrets",
                border_style="green",
            )
        )
        return 0

    try:
        _interactive_secret_loop(tenant, db_url, master_key, owner_id)
    except KeyboardInterrupt:
        console.print("\nAborted.")
        return 130
    return 0


# ---------------------------------------------------------------------------
# Interactive loop
# ---------------------------------------------------------------------------


def _interactive_secret_loop(
    tenant: str,
    db_url: str,
    master_key: str,
    owner_id: str,
) -> None:
    """Interactive prompt loop for adding secrets one at a time."""
    while True:
        # 1. Secret name
        name = _q_text("Secret name:")
        while True:
            err = secrets_ops.validate_secret_name(name)
            if err is None:
                break
            console.print(f"[red]{err}[/]")
            name = _q_text("Secret name:")

        # 2 + 3. Value with confirmation
        while True:
            value = _q_password("Secret value:")
            confirm = _q_password("Confirm value:")
            if value == confirm:
                break
            console.print("[red]Values do not match. Please re-enter.[/]")

        # 4. Insert
        try:
            secrets_ops.insert_secret(db_url, master_key, owner_id, name, value)
        except Exception as exc:
            console.print(f"[red]Failed to store secret:[/] {exc}")
        else:
            # 5. Success panel
            console.print(
                Panel.fit(
                    Text(
                        f"OK: secret '{name}' stored for tenant '{tenant}'",
                        style="bold green",
                    ),
                    title="secrets",
                    border_style="green",
                )
            )

        # 6. Continue?
        if not _q_confirm("Add another secret?", default=False):
            return
