"""Command-line interface for the Hermes -> LunarWing Kawarimi adapter.

Two subcommands:

    inspect   read a Hermes agent and report what would migrate (read-only)
    import    same report, then import — DRY-RUN by default; --apply to write

Dry-run needs neither root nor a database. ``import --apply`` provisions a
tenant and writes to it, so it needs root on a LunarWing host.
"""

from __future__ import annotations

import argparse
import re

from hermes_kawarimi import __version__
from hermes_kawarimi.config import ImportPlan
from hermes_kawarimi.extract import extract
from hermes_kawarimi.mapper import map_agent
from hermes_kawarimi.model import MappedAgent

# --------------------------------------------------------------------------- #
# Output (rich if available, plain otherwise — dry-run must work anywhere)
# --------------------------------------------------------------------------- #

try:
    from rich.console import Console

    _console = Console()

    def out(msg: str = "") -> None:
        _console.print(msg)

except Exception:  # rich not installed
    _MARKUP = re.compile(r"\[/?[a-zA-Z0-9 _#=]+\]")

    def out(msg: str = "") -> None:
        print(_MARKUP.sub("", str(msg)))


# --------------------------------------------------------------------------- #
# Reporting (shared by inspect and dry-run import)
# --------------------------------------------------------------------------- #


def _render_report(source: str, mapped: MappedAgent) -> None:
    counts = mapped.counts()
    out(f"[bold]Hermes agent:[/bold] {source}")
    out("")
    out("[bold]Would migrate:[/bold]")
    out(f"  memory documents : {counts['memory_docs']}")
    out(f"  conversations    : {counts['conversations']}")
    out(f"  messages         : {counts['messages']}")
    out(f"  secrets          : {counts['secrets']}")
    out(f"  settings         : {counts['settings']}")

    if mapped.memory_docs:
        out("")
        out("[bold]Memory documents[/bold] (path -> memory_documents):")
        for doc in mapped.memory_docs[:12]:
            out(f"  - {doc.path} ({len(doc.content)} chars)")
        if len(mapped.memory_docs) > 12:
            out(f"  … and {len(mapped.memory_docs) - 12} more")

    if mapped.secrets:
        out("")
        out("[bold]Secrets[/bold] (names only — values are never shown):")
        for secret in mapped.secrets:
            tag = f" [{secret.provider}]" if secret.provider else ""
            out(f"  - {secret.name}{tag}")

    if mapped.warnings:
        out("")
        out("[bold yellow]Warnings:[/bold yellow]")
        for w in mapped.warnings[:20]:
            out(f"  ! {w}")
        if len(mapped.warnings) > 20:
            out(f"  … and {len(mapped.warnings) - 20} more")


def _extract_and_map(source: str | None) -> tuple[str, MappedAgent]:
    snap = extract(source)
    mapped = map_agent(snap)
    return snap.source_path, mapped


# --------------------------------------------------------------------------- #
# Subcommands
# --------------------------------------------------------------------------- #


def _cmd_inspect(args: argparse.Namespace) -> int:
    source_path, mapped = _extract_and_map(args.source)
    _render_report(source_path, mapped)
    return 0


def _cmd_import(args: argparse.Namespace) -> int:
    plan = _plan_from_args(args)
    err = plan.validate()
    if err:
        out(f"[red]error:[/red] {err}")
        return 2

    source_path, mapped = _extract_and_map(plan.source or None)
    _render_report(source_path, mapped)
    out("")
    out(f"[bold]Target tenant:[/bold] {plan.tenant}  (owner scope = tenant)")

    if args.save:
        plan.to_json(args.save)
        out(f"[dim]saved import plan -> {args.save}[/dim]")

    if not plan.apply:
        out("")
        out("[bold cyan]DRY RUN[/bold cyan] — no changes made. Planned steps on --apply:")
        for step in _planned_steps(plan):
            out(f"  {step}")
        out("")
        out("Re-run with [bold]--apply[/bold] (as root on a LunarWing host) to execute.")
        return 0

    # Live path — import lazily so dry-run never needs psycopg2/root.
    from hermes_kawarimi import loader

    try:
        loader.preflight(plan)
    except loader.ImportError_ as exc:
        out(f"[red]error:[/red] {exc}")
        return 1

    out("")
    out("[bold]Applying…[/bold]")
    result = loader.run_import(plan, mapped, on_output=lambda line: out(f"  [dim]{line}[/dim]"))

    out("")
    if result.ok:
        state = "STAGED (stopped)" if result.staged else "STARTED"
        out(f"[bold green]Import complete[/bold green] — tenant '{plan.tenant}' is {state}.")
        out(f"  wrote: {result.counts}")
        if result.staged:
            out(f"  start it with: sudo lunarwing-mt-admin.sh start-tenant {plan.tenant}")
    else:
        out(f"[red]Import failed:[/red] {result.error or 'see phase output above'}")
    for w in result.warnings:
        out(f"  [yellow]! {w}[/yellow]")
    if not result.ok:
        out("")
        out(f"[dim]Rollback: sudo lunarwing-mt-admin.sh remove-tenant {plan.tenant}[/dim]")
    return 0 if result.ok else 1


def _planned_steps(plan: ImportPlan) -> list[str]:
    final = f"start-tenant {plan.tenant}" if plan.start else "leave STAGED (stopped)"
    return [
        f"1. add-tenant {plan.tenant} --no-health (+ workers) + build-tenant + install-wasm",
        "2. start-tenant (create DB schema via migrations) then stop-tenant",
        "3. populate memory_documents / conversations / settings + encrypt secrets",
        f"4. {final}",
    ]


def _plan_from_args(args: argparse.Namespace) -> ImportPlan:
    if args.resume:
        plan = ImportPlan.from_json(args.resume)
    else:
        plan = ImportPlan()
    if args.source:
        plan.source = args.source
    if args.tenant:
        plan.tenant = args.tenant
    plan.apply = plan.apply or bool(args.apply)
    plan.start = plan.start or bool(args.start)
    plan.force = plan.force or bool(args.force)
    plan.docker_group = plan.docker_group or bool(args.docker_group)
    plan.with_nanocode = plan.with_nanocode or bool(args.with_nanocode)
    plan.with_pebble = plan.with_pebble or bool(args.with_pebble)
    plan.with_opencode = plan.with_opencode or bool(args.with_opencode)
    plan.with_toolchains = plan.with_toolchains or bool(args.with_toolchains)
    plan.with_vision = plan.with_vision or bool(args.with_vision)
    if args.tensorzero_url:
        plan.tensorzero_url = args.tensorzero_url
    if args.llm_model:
        plan.llm_model = args.llm_model
    return plan


# --------------------------------------------------------------------------- #
# argparse wiring
# --------------------------------------------------------------------------- #


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="hermes_kawarimi",
        description="Import a Hermes agent into a LunarWing tenant (Kawarimi adapter).",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    sub = parser.add_subparsers(dest="command", required=True)

    insp = sub.add_parser("inspect", help="report what would migrate (read-only)")
    insp.add_argument("--source", help="Hermes home dir (default: $HERMES_HOME or ~/.hermes)")
    insp.set_defaults(func=_cmd_inspect)

    imp = sub.add_parser("import", help="import a Hermes agent (dry-run unless --apply)")
    imp.add_argument("--source", help="Hermes home dir (default: $HERMES_HOME or ~/.hermes)")
    imp.add_argument("--tenant", help="target LunarWing tenant name (also the owner scope)")
    imp.add_argument("--apply", action="store_true", help="execute (default: dry-run)")
    imp.add_argument("--start", action="store_true", help="start the tenant after import")
    imp.add_argument("--force", action="store_true", help="reuse an existing tenant name")
    imp.add_argument("--docker-group", action="store_true", help="pass --docker-group to add-tenant")
    imp.add_argument("--with-nanocode", action="store_true")
    imp.add_argument("--with-pebble", action="store_true")
    imp.add_argument("--with-opencode", action="store_true")
    imp.add_argument("--with-toolchains", action="store_true", help="pass --with-toolchains to build-tenant")
    imp.add_argument("--with-vision", action="store_true", help="unsupported by mt-admin — will fail in preflight")
    imp.add_argument("--tensorzero-url", default="")
    imp.add_argument("--llm-model", default="")
    imp.add_argument("--save", help="write the resolved import plan to this JSON file")
    imp.add_argument("--resume", help="load an import plan from this JSON file")
    imp.set_defaults(func=_cmd_import)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.func(args))
