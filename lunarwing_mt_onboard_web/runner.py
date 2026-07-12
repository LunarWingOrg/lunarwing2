"""Mode runners: translate a request into reused provisioner/upgrade/export/import
calls, streaming per-phase events to the job queue and auditing everything.

These mirror the sequences in ``lunarwing_mt_onboard.provisioner.provision`` /
``upgrade.run_upgrade`` / ``export.run_export`` / ``import_tenant.run_import``
but drive each phase explicitly so the UI gets accurate phase boundaries (for
the moon + progress). All the real logic — argv construction, secret injection,
DB crypto, verification — comes from the reused parent package; nothing is
re-implemented here.
"""

from __future__ import annotations

import json
import os
import subprocess
import time

import lunarwing_mt_onboard.export as export
import lunarwing_mt_onboard.import_tenant as tenant_import
import lunarwing_mt_onboard.provisioner as provisioner
import lunarwing_mt_onboard.upgrade as upgrade
from lunarwing_mt_onboard import secrets as lw_secrets
from lunarwing_mt_onboard import secrets_ops
from lunarwing_mt_onboard.config import TenantConfig
from lunarwing_mt_onboard.verify import verify_tenant

from . import demo as demo_mod
from .audit import AuditLogger
from .jobs import Job, terminate_process_tree
from .models import (
    ExportRequest,
    ImportRequest,
    ProvisionRequest,
    SecretRequest,
    UpgradeRequest,
)


def list_tenants(demo: bool) -> list[str]:
    """Tenant names for the pickers (fake list in demo, ports.json otherwise)."""
    if demo:
        return list(demo_mod.FAKE_TENANTS)
    return secrets_ops.list_tenants()


# --------------------------------------------------------------------------- #
# Shared subprocess streamer (adds cancellation + audit + emit vs the reused
# provisioner._run, which exposes neither a process handle nor a stop check).
# --------------------------------------------------------------------------- #


def _run_phase(job: Job, argv: list[str], phase_name: str, audit: AuditLogger) -> int:
    audit.command(argv)
    if job.cancelled():
        return 130
    with subprocess.Popen(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        env=os.environ.copy(),
        start_new_session=os.name == "posix",
    ) as proc:
        job.proc = proc
        try:
            if job.cancelled():
                terminate_process_tree(proc)
            assert proc.stdout is not None
            for line in proc.stdout:
                line = line.rstrip("\n")
                audit.line(line)
                job.emit(type="log", line=line)
                if job.cancelled():
                    terminate_process_tree(proc)
                    break
            proc.wait()
        finally:
            job.proc = None
    rc = proc.returncode if proc.returncode is not None else 1
    audit.phase_result(phase_name, rc)
    return rc


def _halt(job: Job, rc: int) -> bool:
    return job.cancelled() or rc != 0


def _finish(job: Job, audit: AuditLogger, summary: list[dict]) -> None:
    cancelled = job.cancelled()
    ok = (not cancelled) and len(summary) > 0 and all(s["ok"] for s in summary)
    job.ok = ok
    if cancelled:
        job.status = "cancelled"
    job.emit(type="done", ok=ok, cancelled=cancelled, phases=summary)
    audit.finish(ok)


def _read_gateway_auth_token(tenant: str, *, demo: bool) -> str | None:
    """Return GATEWAY_AUTH_TOKEN for *tenant* after add-tenant, or None.

    Demo mode never writes a real env file, so a stable fake token is returned
    so the UI can still exercise the credentials reveal path.
    """
    if demo:
        return f"demo-gateway-token-{tenant}"
    try:
        env = secrets_ops.parse_tenant_env(tenant)
    except (FileNotFoundError, OSError, ValueError):
        return None
    token = (env.get("GATEWAY_AUTH_TOKEN") or "").strip()
    return token or None


def _read_gateway_port(tenant: str, *, demo: bool, fallback: int = 0) -> int:
    """Resolve the tenant gateway port from ports.json (or a demo stand-in)."""
    if demo:
        return int(fallback) if fallback else 10000
    if fallback:
        return int(fallback)
    try:
        with open("/etc/lunarwing/ports.json", encoding="utf-8") as fh:
            data = json.load(fh)
        ports = (data.get("tenants") or {}).get(tenant, {}).get("ports") or {}
        port = ports.get("gateway")
        return int(port) if port else 0
    except (FileNotFoundError, OSError, ValueError, TypeError, json.JSONDecodeError):
        return 0


def _emit_gateway_auth_token(
    job: Job, audit: AuditLogger, tenant: str, *, host: str, port: int
) -> None:
    """Surface the gateway Web UI bearer token once the env file exists."""
    token = _read_gateway_auth_token(tenant, demo=job.demo)
    if not token:
        job.emit(
            type="log",
            line="gateway auth token unavailable (not found in tenant env)",
        )
        audit.note("gateway auth token unavailable after add-tenant")
        return
    resolved_port = _read_gateway_port(tenant, demo=job.demo, fallback=port)
    job.emit(
        type="gateway_auth_token",
        token=token,
        host=host or "127.0.0.1",
        port=resolved_port,
    )
    audit.note("emitted gateway auth token to UI (value redacted)")


# --------------------------------------------------------------------------- #
# Provision
# --------------------------------------------------------------------------- #


def run_provision_job(job: Job, req: ProvisionRequest, *, log_dir: str | None = None) -> None:
    cfg = req.to_tenant_config()
    err = TenantConfig.validate_name(cfg.name)
    if err:
        job.emit(type="error", message=f"Invalid tenant name: {err}")
        job.ok = False
        return

    generated_key: str | None = None
    if not cfg.secrets_master_key:
        cfg.secrets_master_key = lw_secrets.generate_master_key()
        generated_key = cfg.secrets_master_key

    audit = AuditLogger("provision", cfg.name, job.id, log_dir=log_dir, demo=job.demo)
    summary: list[dict] = []
    try:
        try:
            provisioner.ensure_mt_admin()
        except FileNotFoundError as exc:
            job.emit(type="error", message=str(exc))
            job.ok = False
            audit.finish(False)
            return

        plan = ["add-tenant", "inject-secrets"]
        if not req.skip_build:
            plan.append("build-tenant")
            if cfg.enable_darkirc:
                plan.append("build-darkirc")
        if not req.skip_start:
            plan.extend(["start-tenant", "verify"])
        total = len(plan)
        counter = {"i": 0}

        def announce(name: str, label: str) -> None:
            counter["i"] += 1
            job.emit(type="phase", name=name, label=label, index=counter["i"], total=total)

        if generated_key:
            job.emit(type="master_key", key=generated_key)
            audit.note("generated new secrets master key (value redacted)")

        # add-tenant
        announce("add-tenant", f"Creating tenant '{cfg.name}'")
        rc = _run_phase(job, provisioner.build_add_tenant_args(cfg), "add-tenant", audit)
        summary.append({"name": "add-tenant", "ok": rc == 0, "code": rc})
        if _halt(job, rc):
            return _finish(job, audit, summary)

        # inject-secrets (real writes to the tenant env file; no-op in demo)
        announce("inject-secrets", "Injecting secrets into tenant environment")
        try:
            provisioner._inject_secrets(cfg)
            audit.note("secrets injected into tenant env (values redacted)")
            summary.append({"name": "inject-secrets", "ok": True, "code": 0})
        except Exception as exc:
            job.emit(type="log", line=f"inject-secrets error: {exc}")
            summary.append({"name": "inject-secrets", "ok": False, "code": 1})
            return _finish(job, audit, summary)

        # GATEWAY_AUTH_TOKEN is minted by add-tenant into lunarwing.env — show it
        # once the env is in place so operators can open the gateway Web UI.
        _emit_gateway_auth_token(
            job,
            audit,
            cfg.name,
            host=cfg.gateway_host,
            port=cfg.gateway_port,
        )

        # build-tenant (+ darkirc)
        if not req.skip_build:
            announce("build-tenant", "Building tenant — cargo + worker images (can take 20-40 min)")
            rc = _run_phase(job, provisioner.build_build_tenant_args(cfg), "build-tenant", audit)
            summary.append({"name": "build-tenant", "ok": rc == 0, "code": rc})
            if _halt(job, rc):
                return _finish(job, audit, summary)
            if cfg.enable_darkirc:
                announce("build-darkirc", "Building DarkIRC daemon")
                rc = _run_phase(job, provisioner.build_darkirc_args(cfg), "build-darkirc", audit)
                summary.append({"name": "build-darkirc", "ok": rc == 0, "code": rc})
                if _halt(job, rc):
                    return _finish(job, audit, summary)

        # start-tenant + verify
        if not req.skip_start:
            announce("start-tenant", f"Starting '{cfg.name}'")
            rc = _run_phase(job, provisioner.build_start_tenant_args(cfg), "start-tenant", audit)
            summary.append({"name": "start-tenant", "ok": rc == 0, "code": rc})
            if _halt(job, rc):
                return _finish(job, audit, summary)

            announce("verify", "Verifying tenant health")
            checks = _verify(job, cfg)
            job.emit(type="verify", checks=checks)
            for c in checks:
                audit.note(f"verify {c['label']}: {'PASS' if c['ok'] else 'FAIL'}")

        return _finish(job, audit, summary)
    finally:
        audit.close()


def _verify(job: Job, cfg: TenantConfig) -> list[dict]:
    if job.demo:
        time.sleep(1.0)  # let the "verifying" state show
        return [
            {"label": "gateway port", "ok": True, "detail": "reachable"},
            {"label": f"service lunarwing-{cfg.name}", "ok": True, "detail": "active"},
        ]
    results = verify_tenant(cfg.name, cfg.gateway_host, cfg.gateway_port)
    return [{"label": r.label, "ok": r.ok, "detail": r.detail} for r in results]


# --------------------------------------------------------------------------- #
# Upgrade
# --------------------------------------------------------------------------- #


def run_upgrade_job(job: Job, req: UpgradeRequest, *, log_dir: str | None = None) -> None:
    cfg = req.to_upgrade_config()
    err = cfg.validate()
    if err:
        job.emit(type="error", message=f"Invalid upgrade config: {err}")
        job.ok = False
        return

    audit = AuditLogger("upgrade", cfg.tenant, job.id, log_dir=log_dir, demo=job.demo)
    summary: list[dict] = []
    try:
        plan = (["preflight"] if cfg.run_preflight else []) + ["upgrade"]
        total = len(plan)
        counter = {"i": 0}

        def announce(name: str, label: str) -> None:
            counter["i"] += 1
            job.emit(type="phase", name=name, label=label, index=counter["i"], total=total)

        if cfg.run_preflight:
            announce("preflight", f"Preflight checks for '{cfg.tenant}'")
            try:
                args = upgrade.build_preflight_args(cfg)
            except FileNotFoundError as exc:
                job.emit(type="error", message=str(exc))
                job.ok = False
                audit.finish(False)
                return
            rc = _run_phase(job, args, "preflight", audit)
            summary.append({"name": "preflight", "ok": rc == 0, "code": rc})
            if job.cancelled():
                return _finish(job, audit, summary)
            if rc != 0 and not cfg.force:
                return _finish(job, audit, summary)

        mode = "APPLY" if cfg.apply else "dry-run"
        announce("upgrade", f"Upgrading '{cfg.tenant}' to {cfg.target or 'default'} [{mode}]")
        try:
            args = upgrade.build_upgrade_args(cfg)
        except FileNotFoundError as exc:
            job.emit(type="error", message=str(exc))
            job.ok = False
            audit.finish(False)
            return
        rc = _run_phase(job, args, "upgrade", audit)
        summary.append({"name": "upgrade", "ok": rc == 0, "code": rc})
        return _finish(job, audit, summary)
    finally:
        audit.close()


# --------------------------------------------------------------------------- #
# Export
# --------------------------------------------------------------------------- #


def run_export_job(job: Job, req: ExportRequest, *, log_dir: str | None = None) -> None:
    cfg = req.to_export_config()
    err = cfg.validate()
    if err:
        job.emit(type="error", message=f"Invalid export config: {err}")
        job.ok = False
        return

    audit = AuditLogger("export", cfg.tenant, job.id, log_dir=log_dir, demo=job.demo)
    summary: list[dict] = []
    try:
        mode = "APPLY" if cfg.apply else "dry-run"
        job.emit(
            type="phase",
            name="export",
            label=f"Exporting '{cfg.tenant}' -> {cfg.out_dir} [{mode}]",
            index=1,
            total=1,
        )
        try:
            args = export.build_export_args(cfg)
        except FileNotFoundError as exc:
            job.emit(type="error", message=str(exc))
            job.ok = False
            audit.finish(False)
            return
        rc = _run_phase(job, args, "export", audit)
        summary.append({"name": "export", "ok": rc == 0, "code": rc})
        return _finish(job, audit, summary)
    finally:
        audit.close()


# --------------------------------------------------------------------------- #
# Import
# --------------------------------------------------------------------------- #


def run_import_job(job: Job, req: ImportRequest, *, log_dir: str | None = None) -> None:
    cfg = req.to_import_config()
    err = cfg.validate()
    if err:
        job.emit(type="error", message=f"Invalid import config: {err}")
        job.ok = False
        return

    audit_name = cfg.name or os.path.basename(cfg.bundle)
    audit = AuditLogger("import", audit_name, job.id, log_dir=log_dir, demo=job.demo)
    summary: list[dict] = []
    try:
        mode = "APPLY" if cfg.apply else "dry-run"
        lifecycle = "start" if cfg.start else "stage-only"
        rename = f" as '{cfg.name}'" if cfg.name else ""
        job.emit(
            type="phase",
            name="import",
            label=f"Importing '{cfg.bundle}'{rename} [{mode}; {lifecycle}]",
            index=1,
            total=1,
        )
        try:
            args = tenant_import.build_import_args(cfg)
        except FileNotFoundError as exc:
            job.emit(type="error", message=str(exc))
            job.ok = False
            audit.finish(False)
            return
        rc = _run_phase(job, args, "import", audit)
        summary.append({"name": "import", "ok": rc == 0, "code": rc})
        return _finish(job, audit, summary)
    finally:
        audit.close()


# --------------------------------------------------------------------------- #
# Secrets
# --------------------------------------------------------------------------- #


def run_secret_job(job: Job, req: SecretRequest, *, log_dir: str | None = None) -> None:
    if not req.tenant.strip():
        job.emit(type="error", message="tenant is required")
        job.ok = False
        return
    name_err = secrets_ops.validate_secret_name(req.name)
    if name_err:
        job.emit(type="error", message=name_err)
        job.ok = False
        return
    if not req.value:
        job.emit(type="error", message="secret value is required")
        job.ok = False
        return

    audit = AuditLogger("secrets", req.tenant, job.id, log_dir=log_dir, demo=job.demo)
    try:
        job.emit(
            type="phase",
            name="secret",
            label=f"Storing secret '{req.name}' for '{req.tenant}'",
            index=1,
            total=1,
        )
        audit.note(f"insert secret name='{req.name}' tenant='{req.tenant}' (value redacted)")

        if job.demo:
            job.emit(type="log", line="deriving AES-256-GCM key (HKDF-SHA256)...")
            time.sleep(0.6)
            job.emit(type="log", line="encrypting value...")
            time.sleep(0.6)
            job.emit(type="log", line=f"stored secret '{req.name}' (simulated - no DB write in demo)")
            job.ok = True
            job.emit(type="secret_stored", name=req.name)
            job.emit(type="done", ok=True, phases=[{"name": "secret", "ok": True, "code": 0}])
            audit.finish(True)
            return

        if not secrets_ops.ensure_dependencies():
            job.emit(type="log", line="installing crypto dependencies (cryptography, psycopg2-binary)...")
            secrets_ops.install_dependencies()
            if not secrets_ops.ensure_dependencies():
                job.emit(type="error", message="missing dependencies for secrets operations")
                job.ok = False
                audit.finish(False)
                return

        try:
            env = secrets_ops.parse_tenant_env(req.tenant)
        except Exception as exc:
            job.emit(type="error", message=f"failed to read tenant env: {exc}")
            job.ok = False
            audit.finish(False)
            return

        missing = [k for k in ("DATABASE_URL", "SECRETS_MASTER_KEY", "LUNARWING_OWNER_ID") if not env.get(k)]
        if missing:
            job.emit(type="error", message=f"tenant env missing keys: {', '.join(missing)}")
            job.ok = False
            audit.finish(False)
            return

        try:
            secrets_ops.insert_secret(
                env["DATABASE_URL"],
                env["SECRETS_MASTER_KEY"],
                env["LUNARWING_OWNER_ID"],
                req.name,
                req.value,
            )
        except Exception as exc:
            job.emit(type="error", message=f"failed to store secret: {exc}")
            job.ok = False
            audit.finish(False)
            return

        job.emit(type="log", line=f"stored secret '{req.name}' for tenant '{req.tenant}'")
        job.ok = True
        job.emit(type="secret_stored", name=req.name)
        job.emit(type="done", ok=True, phases=[{"name": "secret", "ok": True, "code": 0}])
        audit.finish(True)
    finally:
        audit.close()
