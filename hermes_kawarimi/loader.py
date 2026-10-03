"""The ``--apply`` path: provision a fresh tenant and populate it.

Sequence (race-free, mirrors the export model where the daemon is stopped but
PostgreSQL stays up):

    1. add-tenant --no-health  + build-tenant + install-wasm   (empty tenant)
    2. start-tenant            -> daemon runs migrations + onboard = SCHEMA
    3. stop-writers            -> verified writer quiescence (WI#133/#134)
    4. stop-tenant             -> daemon down (mt-admin stop-tenant also stops
       start-pg                PostgreSQL, so bring PG back up PG-only; the
                                  daemon stays down)
    5. populate                -> INSERT rows + encrypt secrets, no daemon racing
    6. start-tenant (if --start) else leave STAGED (daemon stopped, data in
       place; PG left running, mirroring import-tenant.sh's staged state)

Why stop-writers before stop-tenant: on OpenRC a container with a restart
policy (or a -sup babysitter still registered) can be respawned by the runtime
after stop-tenant's stop calls. stop-writers verifies quiescence with
tenant_writers_active and fails if any writer survived, so the populate step
can never run against a tenant whose sidecars/workers are still writing.
stop-tenant performs the same verification itself (WI#133/#134); since WI#137
both paths poll the check over a bounded window because supervise-daemon
teardown is asynchronous.

Why start-then-stop for the schema: LunarWing creates its schema via the
daemon's refinery runner at first boot (``ic/src/app.rs`` run_migrations +
auto-onboard). ``V1__initial.sql`` uses bare ``CREATE TABLE`` (not
``IF NOT EXISTS``), so hand-applying SQL would collide with refinery's own run.
Letting the daemon create the schema is the only safe path.

Every row is written under the tenant's ``LUNARWING_OWNER_ID`` (read from its
env after provisioning), so owner-scope continuity is inherent — no pg_restore,
no rekey, none of the KAWARIMI-OWNER-SCOPE-CONTINUITY silent-empty hazard.

Live writes only happen here (behind ``--apply``); the CLI renders a dry-run
report without calling this module.
"""

from __future__ import annotations

import os
import time
from dataclasses import dataclass, field

from hermes_kawarimi.config import ImportPlan
from hermes_kawarimi.model import MappedAgent
from lunarwing_mt_onboard import secrets_ops
from lunarwing_mt_onboard.provisioner import (
    PhaseResult,
    ensure_mt_admin,
    run_command,
)

OutputCb = "object"  # callable[[str], None] | None; kept loose to avoid import churn

_SCHEMA_SENTINEL_TABLE = "memory_documents"
_SCHEMA_POLL_INTERVAL = 2.0


def _schema_wait_seconds() -> int:
    """Configurable via KAWARIMI_SCHEMA_WAIT_SECONDS env (default 90)."""
    raw = os.environ.get("KAWARIMI_SCHEMA_WAIT_SECONDS", "").strip()
    if raw:
        try:
            val = int(raw)
            if val > 0:
                return val
        except ValueError:
            pass
    return 90


@dataclass
class ImportResult:
    phases: list[PhaseResult] = field(default_factory=list)
    counts: dict[str, int] = field(default_factory=dict)
    warnings: list[str] = field(default_factory=list)
    staged: bool = True  # True => tenant left stopped (not started)
    error: str | None = None

    @property
    def ok(self) -> bool:
        return self.error is None and all(p.ok for p in self.phases)


class ImportError_(Exception):
    """Raised for a precondition failure before any tenant is provisioned."""


# --------------------------------------------------------------------------- #
# Preconditions
# --------------------------------------------------------------------------- #


def preflight(plan: ImportPlan) -> None:
    """Fail fast (before provisioning) on anything we can check up front."""
    if os.geteuid() != 0:
        raise ImportError_(
            "must run as root (sudo): provisioning + tenant env/DB need root"
        )
    if not secrets_ops.ensure_dependencies():
        raise ImportError_(
            "missing dependencies: pip install cryptography psycopg2-binary"
        )
    err = plan.validate()
    if err:
        raise ImportError_(err)
    if plan.tenant in secrets_ops.list_tenants() and not plan.force:
        raise ImportError_(
            f"tenant '{plan.tenant}' already exists — pass --force to reuse it"
        )
    if plan.with_vision:
        raise ImportError_(
            "--with-vision is not supported by mt-admin add-tenant/build-tenant; "
            "vision provisioning must be done manually after import"
        )


# --------------------------------------------------------------------------- #
# Orchestration
# --------------------------------------------------------------------------- #


def run_import(
    plan: ImportPlan,
    mapped: MappedAgent,
    *,
    on_output=None,
) -> ImportResult:
    """Provision + populate. Assumes ``preflight`` already passed."""
    result = ImportResult(warnings=list(mapped.warnings))
    mt = ensure_mt_admin()

    def phase(args: list[str]) -> bool:
        pr = run_command(args, on_output=on_output)
        result.phases.append(pr)
        return pr.ok

    # 1. provision an empty tenant
    if not phase(_add_tenant_args(mt, plan)):
        result.error = "add-tenant failed"
        return result
    if not phase([mt, "build-tenant", plan.tenant, "--with-wasm", *_worker_flags(plan), *_build_flags(plan)]):
        result.error = "build-tenant failed"
        return result
    # Required, mirroring import-tenant.sh (set -e aborts there on failure):
    # a tenant missing its WASM artifacts boots with silently absent channels.
    if not phase([mt, "install-wasm", plan.tenant]):
        result.error = "install-wasm failed"
        return result

    # Env (DATABASE_URL / SECRETS_MASTER_KEY / LUNARWING_OWNER_ID) exists now.
    try:
        env = secrets_ops.parse_tenant_env(plan.tenant)
    except OSError as exc:
        result.error = f"could not read tenant env after add-tenant: {exc}"
        return result
    backend = (env.get("DATABASE_BACKEND") or "postgres").strip().lower()
    if backend != "postgres":
        result.error = (
            f"DATABASE_BACKEND={backend}: v1 supports postgres only "
            "(libsql import is future work) — leaving tenant provisioned but unpopulated"
        )
        return result
    for key in ("DATABASE_URL", "SECRETS_MASTER_KEY", "LUNARWING_OWNER_ID"):
        if not env.get(key):
            result.error = f"tenant env missing {key} after provisioning"
            return result
    db_url = env["DATABASE_URL"]
    master_key = env["SECRETS_MASTER_KEY"]
    owner_id = env["LUNARWING_OWNER_ID"]

    # 2. create the schema by booting the daemon once, then 3. quiesce writers
    # and 4. stop the tenant. stop-writers verifies writer quiescence
    # (tenant_writers_active) and fails if any writer survived — without it,
    # OpenRC restart policies / -sup babysitters can respawn writer containers
    # during the PG-only populate window (WI#133/#134). stop-tenant performs
    # the same verification itself; since WI#137 both paths poll over a
    # bounded window (supervise-daemon teardown is asynchronous).
    if not phase([mt, "start-tenant", plan.tenant]):
        result.error = "start-tenant failed (needed to create the DB schema)"
        return result
    try:
        _wait_for_schema(db_url)
    except TimeoutError as exc:
        result.error = str(exc)
        return result
    if not phase([mt, "stop-writers", plan.tenant]):
        result.error = "stop-writers failed (writers must be quiesced before populate)"
        return result
    if not phase([mt, "stop-tenant", plan.tenant]):
        result.error = "stop-tenant failed (before populate)"
        return result
    # mt-admin's stop-tenant also stops PostgreSQL. Bring PG back up PG-only
    # (daemon stays down) so populate can connect with nothing racing.
    if not phase([mt, "start-pg", plan.tenant]):
        result.error = "start-pg failed (populate needs PostgreSQL up, daemon down)"
        return result

    # This import owns the PG-only start until a successful result deliberately
    # leaves the tenant staged or transfers control to start-tenant.
    postgres_owned = True
    try:
        # 4. populate (daemon down, PostgreSQL still up)
        try:
            counts = _populate(db_url, owner_id, mapped)
            result.counts = counts  # set before _insert_secrets adds counts["secrets"]
            _insert_secrets(db_url, master_key, owner_id, mapped, result)
        except Exception as exc:  # psycopg2 / crypto errors
            result.error = f"populate failed: {exc}"
            return result

        # 5. cut over or stage
        if plan.start:
            if not phase([mt, "start-tenant", plan.tenant]):
                result.error = "start-tenant failed (after populate)"
                return result
            result.staged = False
        else:
            result.staged = True
        postgres_owned = False
        return result
    finally:
        if postgres_owned:
            _stop_owned_postgres(mt, plan.tenant, result, on_output)


def _stop_owned_postgres(
    mt: str,
    tenant: str,
    result: ImportResult,
    on_output=None,
) -> None:
    """Best-effort cleanup without replacing the original import failure."""
    try:
        cleanup = run_command([mt, "stop-pg", tenant], on_output=on_output)
        result.phases.append(cleanup)
        if not cleanup.ok:
            result.warnings.append(
                "stop-pg cleanup failed after import failure; "
                "PostgreSQL may still be running"
            )
    except Exception as exc:
        result.warnings.append(
            f"stop-pg cleanup raised after import failure: {exc}; "
            "PostgreSQL may still be running"
        )


def _add_tenant_args(mt: str, plan: ImportPlan) -> list[str]:
    args = [mt, "add-tenant", plan.tenant, "--no-health"]
    if plan.docker_group:
        args.append("--docker-group")
    if plan.tensormoon_url:
        args.extend(["--tensormoon-url", plan.tensormoon_url])
    if plan.llm_model:
        args.extend(["--llm-model", plan.llm_model])
    args.extend(_worker_flags(plan))
    return args


def _worker_flags(plan: ImportPlan) -> list[str]:
    flags: list[str] = []
    if plan.with_pebble:
        flags.append("--with-pebble")
    if plan.with_opencode:
        flags.append("--with-opencode")
    if plan.with_kimi:
        flags.append("--with-kimi")
    return flags


def _build_flags(plan: ImportPlan) -> list[str]:
    """Flags that build-tenant accepts but add-tenant does not."""
    flags: list[str] = []
    if plan.with_toolchains:
        flags.append("--with-toolchains")
    return flags


# --------------------------------------------------------------------------- #
# Schema wait + populate
# --------------------------------------------------------------------------- #


def _wait_for_schema(db_url: str) -> None:
    """Poll until the core schema exists (daemon migrations finished)."""
    import psycopg2

    wait_seconds = _schema_wait_seconds()
    deadline = time.monotonic() + wait_seconds
    last_exc: Exception | None = None
    while time.monotonic() < deadline:
        try:
            conn = psycopg2.connect(db_url)
            try:
                cur = conn.cursor()
                cur.execute("SELECT to_regclass(%s)", (f"public.{_SCHEMA_SENTINEL_TABLE}",))
                row = cur.fetchone()
                if row and row[0] is not None:
                    return
            finally:
                conn.close()
        except Exception as exc:  # DB not ready yet
            last_exc = exc
        time.sleep(_SCHEMA_POLL_INTERVAL)
    raise TimeoutError(
        f"schema table '{_SCHEMA_SENTINEL_TABLE}' did not appear within "
        f"{wait_seconds}s after start-tenant"
        + (f" (last error: {last_exc})" if last_exc else "")
    )


def _populate(db_url: str, owner_id: str, mapped: MappedAgent) -> dict[str, int]:
    """Write memory docs, conversations, and settings in one transaction."""
    import psycopg2
    from psycopg2.extras import Json

    conn = psycopg2.connect(db_url)
    try:
        conn.autocommit = False
        cur = conn.cursor()

        for doc in mapped.memory_docs:
            # UNIQUE(user_id, agent_id, path) treats NULL as distinct in
            # Postgres, so ON CONFLICT won't fire for our agent_id=NULL rows.
            # Manual UPDATE-then-INSERT keeps re-imports idempotent. If
            # LunarWing ever imports docs under a non-null agent_id, those
            # would need their own path (not the kawarimi use case).
            cur.execute(
                "UPDATE memory_documents SET content = %s, metadata = %s, "
                "updated_at = NOW() WHERE user_id = %s AND agent_id IS NULL AND path = %s",
                (doc.content, Json(doc.metadata), owner_id, doc.path),
            )
            if cur.rowcount == 0:
                cur.execute(
                    "INSERT INTO memory_documents (user_id, agent_id, path, content, metadata) "
                    "VALUES (%s, NULL, %s, %s, %s)",
                    (owner_id, doc.path, doc.content, Json(doc.metadata)),
                )

        for conv in mapped.conversations:
            cur.execute(
                "INSERT INTO conversations (id, channel, user_id, thread_id, "
                "started_at, last_activity, metadata) "
                "VALUES (%s, %s, %s, %s, %s, %s, %s) "
                "ON CONFLICT (id) DO UPDATE SET channel = EXCLUDED.channel, "
                "user_id = EXCLUDED.user_id, thread_id = EXCLUDED.thread_id, "
                "started_at = EXCLUDED.started_at, last_activity = EXCLUDED.last_activity, "
                "metadata = EXCLUDED.metadata",
                (
                    conv.id,
                    conv.channel,
                    owner_id,
                    conv.thread_id,
                    conv.started_at,
                    conv.last_activity,
                    Json(conv.metadata),
                ),
            )
            for msg in conv.messages:
                cur.execute(
                    "INSERT INTO conversation_messages (id, conversation_id, role, "
                    "content, created_at) VALUES (%s, %s, %s, %s, %s) "
                    "ON CONFLICT (id) DO UPDATE SET role = EXCLUDED.role, "
                    "content = EXCLUDED.content, created_at = EXCLUDED.created_at, "
                    "conversation_id = EXCLUDED.conversation_id",
                    (msg.id, msg.conversation_id, msg.role, msg.content, msg.created_at),
                )

        for setting in mapped.settings:
            cur.execute(
                "INSERT INTO settings (user_id, key, value) VALUES (%s, %s, %s) "
                "ON CONFLICT (user_id, key) DO UPDATE SET value = EXCLUDED.value, "
                "updated_at = NOW()",
                (owner_id, setting.key, Json(setting.value)),
            )

        conn.commit()
    except Exception:
        conn.rollback()
        raise
    finally:
        conn.close()

    return {
        "memory_docs": len(mapped.memory_docs),
        "conversations": len(mapped.conversations),
        "messages": mapped.message_count,
        "settings": len(mapped.settings),
    }


def _insert_secrets(
    db_url: str,
    master_key: str,
    owner_id: str,
    mapped: MappedAgent,
    result: ImportResult,
) -> None:
    """Encrypt each secret into the secrets table (reuses secrets_ops crypto).

    secrets_ops.insert_secret is an upsert (ON CONFLICT (user_id, name)
    DO UPDATE), so re-imports update rather than duplicate.
    """
    inserted = 0
    for secret in mapped.secrets:
        try:
            secrets_ops.insert_secret(
                db_url, master_key, owner_id, secret.name, secret.value
            )
            inserted += 1
        except Exception as exc:
            result.warnings.append(f"secret '{secret.name}' not stored: {exc}")
    result.counts["secrets"] = inserted
