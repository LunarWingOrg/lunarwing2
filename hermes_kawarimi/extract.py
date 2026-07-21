"""Read a Hermes agent's on-disk footprint into a ``HermesAgentSnapshot``.

Pure, read-only. Never writes to the source. Tolerant of missing files and
schema drift in ``state.db`` (older Hermes versions have fewer columns), so a
partial agent still imports what it has.

Hermes layout (all under ``$HERMES_HOME``, confirmed against hermes-agent):

    SOUL.md                 persona / identity          (prompt_builder.py:1888)
    memories/MEMORY.md      agent notes                 (tools/memory_tool.py:198)
    memories/USER.md        user profile                (tools/memory_tool.py:199)
    memories/*.md           any other memory docs
    state.db                sqlite sessions + messages  (hermes_state.py:153)
    config.yaml             all settings                (hermes_constants.py:1192)
    .env                    credentials                 (hermes_constants.py:1203)
    auth.json               oauth tokens / grants
"""

from __future__ import annotations

import json
import os
import sqlite3
from pathlib import Path

from hermes_kawarimi.model import (
    HermesAgentSnapshot,
    HermesMessage,
    HermesSession,
)


def resolve_source(source: str | None) -> Path:
    """Resolve the Hermes home: explicit arg > ``HERMES_HOME`` env > ``~/.hermes``.

    Mirrors Hermes' own resolution order (``hermes_constants.get_hermes_home``).
    """
    if source and source.strip():
        return Path(source).expanduser()
    env_home = os.environ.get("HERMES_HOME", "").strip()
    if env_home:
        return Path(env_home).expanduser()
    return Path.home() / ".hermes"


def extract(source: str | None = None) -> HermesAgentSnapshot:
    """Read the agent at *source* into a snapshot (read-only)."""
    home = resolve_source(source)
    snap = HermesAgentSnapshot(source_path=str(home))

    if not home.is_dir():
        snap.warnings.append(f"Hermes home not found or not a directory: {home}")
        return snap

    _read_markdown(home, snap)
    _read_state_db(home / "state.db", snap)
    _read_config(home / "config.yaml", snap)
    snap.env = _parse_env_file(home / ".env", snap)
    _read_auth(home / "auth.json", snap)
    return snap


# --------------------------------------------------------------------------- #
# Markdown
# --------------------------------------------------------------------------- #


def _read_markdown(home: Path, snap: HermesAgentSnapshot) -> None:
    # Persona at the root.
    soul = _read_text(home / "SOUL.md")
    if soul is not None:
        snap.markdown_docs["SOUL.md"] = soul

    mem_dir = home / "memories"
    if not mem_dir.is_dir():
        return

    # The two canonical memory files land at the workspace root (MEMORY.md /
    # USER.md); everything else keeps a memories/ prefix so nothing collides.
    for canonical in ("MEMORY.md", "USER.md"):
        text = _read_text(mem_dir / canonical)
        if text is not None:
            snap.markdown_docs[canonical] = text

    for md in sorted(mem_dir.rglob("*.md")):
        if not md.is_file():
            continue
        rel = md.relative_to(mem_dir).as_posix()
        if rel in ("MEMORY.md", "USER.md"):
            continue  # already handled at root
        text = _read_text(md)
        if text is not None:
            snap.markdown_docs[f"memories/{rel}"] = text


# --------------------------------------------------------------------------- #
# state.db (sessions + messages)
# --------------------------------------------------------------------------- #


def _read_state_db(db_path: Path, snap: HermesAgentSnapshot) -> None:
    if not db_path.is_file():
        return
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    except sqlite3.Error as exc:
        snap.warnings.append(f"could not open state.db read-only: {exc}")
        return
    conn.row_factory = sqlite3.Row
    try:
        try:
            session_rows = conn.execute("SELECT * FROM sessions").fetchall()
        except sqlite3.Error as exc:
            snap.warnings.append(f"state.db has no readable sessions table: {exc}")
            return

        for row in session_rows:
            d = _row_dict(row)
            session = HermesSession(
                id=str(d.get("id")),
                source=str(d.get("source") or "cli"),
                started_at=_as_float(d.get("started_at")),
                user_id=_opt_str(d.get("user_id")),
                chat_id=_opt_str(d.get("chat_id")),
                thread_id=_opt_str(d.get("thread_id")),
                title=_opt_str(d.get("title")),
                model=_opt_str(d.get("model")),
                profile_name=_opt_str(d.get("profile_name")),
                ended_at=_opt_float(d.get("ended_at")),
                archived=bool(d.get("archived") or 0),
            )
            session.messages = _read_messages(conn, session.id, snap)
            snap.sessions.append(session)
    finally:
        conn.close()


def _read_messages(
    conn: sqlite3.Connection, session_id: str, snap: HermesAgentSnapshot
) -> list[HermesMessage]:
    try:
        rows = conn.execute(
            "SELECT * FROM messages WHERE session_id = ? "
            "ORDER BY timestamp ASC, id ASC",
            (session_id,),
        ).fetchall()
    except sqlite3.Error as exc:
        snap.warnings.append(
            f"could not read messages for session {session_id}: {exc}"
        )
        return []

    out: list[HermesMessage] = []
    for row in rows:
        d = _row_dict(row)
        out.append(
            HermesMessage(
                id=_as_int(d.get("id")),
                role=str(d.get("role") or "user"),
                content=_opt_str(d.get("content")),
                timestamp=_as_float(d.get("timestamp")),
                tool_calls=_opt_str(d.get("tool_calls")),
                tool_name=_opt_str(d.get("tool_name")),
            )
        )
    return out


# --------------------------------------------------------------------------- #
# config.yaml / .env / auth.json
# --------------------------------------------------------------------------- #


def _read_config(path: Path, snap: HermesAgentSnapshot) -> None:
    if not path.is_file():
        return
    try:
        import yaml
    except ImportError:
        snap.warnings.append("pyyaml not installed — config.yaml not parsed")
        return
    try:
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, yaml.YAMLError) as exc:
        snap.warnings.append(f"could not parse config.yaml: {exc}")
        return
    if isinstance(data, dict):
        snap.config = data
    elif data is not None:
        snap.warnings.append("config.yaml is not a mapping — ignored")


def _read_auth(path: Path, snap: HermesAgentSnapshot) -> None:
    if not path.is_file():
        return
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        snap.warnings.append(f"could not parse auth.json: {exc}")
        return
    if isinstance(data, dict):
        snap.auth = data
    else:
        snap.warnings.append("auth.json is not an object — ignored")


def _parse_env_file(path: Path, snap: HermesAgentSnapshot) -> dict[str, str]:
    """Parse a ``KEY=VALUE`` env file, stripping matched surrounding quotes."""
    result: dict[str, str] = {}
    if not path.is_file():
        return result
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        snap.warnings.append(f"could not read .env: {exc}")
        return result
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
            value = value[1:-1]
        if key:
            result[key] = value
    return result


# --------------------------------------------------------------------------- #
# small coercion helpers
# --------------------------------------------------------------------------- #


def _read_text(path: Path) -> str | None:
    """Read a UTF-8 text file, or ``None`` if absent/unreadable."""
    if not path.is_file():
        return None
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None


def _row_dict(row: sqlite3.Row) -> dict[str, object]:
    return {k: row[k] for k in row.keys()}


def _as_int(value: object) -> int:
    try:
        return int(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return 0


def _opt_str(value: object) -> str | None:
    if value is None:
        return None
    s = str(value)
    return s if s != "" else None


def _as_float(value: object) -> float:
    try:
        return float(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return 0.0


def _opt_float(value: object) -> float | None:
    if value is None:
        return None
    try:
        return float(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return None
