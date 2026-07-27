#!/usr/bin/env python3
"""Validate lw2studymcp and merge read-only OpenCode configuration."""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import tempfile
from pathlib import Path
from typing import Any


SERVER_NAME = "study_notes"


def validate_home(raw_home: Path) -> Path:
    if not raw_home.is_absolute():
        raise ValueError("LW2STUDYMCP_HOME must be an absolute path")
    home = raw_home.resolve(strict=True)
    required_files = (
        home / "study_notes_mcp.py",
        home / "memorydb.py",
        home / "agent-notes.sqlite3",
        home / "skills" / "study-notes" / "SKILL.md",
    )
    for path in required_files:
        if not path.is_file():
            raise ValueError(f"Study Notes file missing: {path}")
    notes = home / "agent-notes-by-agent"
    if not notes.is_dir():
        raise ValueError(f"Study Notes directory missing: {notes}")

    database = home / "agent-notes.sqlite3"
    connection = sqlite3.connect(f"{database.as_uri()}?mode=ro", uri=True)
    try:
        schema = connection.execute(
            "SELECT value FROM metadata WHERE key = 'schema_version'"
        ).fetchone()
        fts = connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sections_fts'"
        ).fetchone()
        quick_check = connection.execute("PRAGMA quick_check").fetchone()
    finally:
        connection.close()
    if schema is None or fts is None:
        raise ValueError(f"Study Notes database has unexpected schema: {database}")
    if quick_check != ("ok",):
        raise ValueError(f"Study Notes database integrity check failed: {database}")
    return home


def merge_config(config: dict[str, Any], home: Path) -> dict[str, Any]:
    skills = config.setdefault("skills", {})
    if not isinstance(skills, dict):
        raise ValueError("OpenCode config skills must be an object")
    paths = skills.setdefault("paths", [])
    if not isinstance(paths, list) or not all(isinstance(path, str) for path in paths):
        raise ValueError("OpenCode config skills.paths must be a string array")
    skill_path = str(home / "skills")
    if skill_path not in paths:
        paths.append(skill_path)

    mcp = config.setdefault("mcp", {})
    if not isinstance(mcp, dict):
        raise ValueError("OpenCode config mcp must be an object")
    mcp[SERVER_NAME] = {
        "type": "local",
        "command": [
            "python3",
            str(home / "study_notes_mcp.py"),
            "--db",
            str(home / "agent-notes.sqlite3"),
            "--notes",
            str(home / "agent-notes-by-agent"),
        ],
        "cwd": str(home),
        "enabled": True,
        "timeout": 10000,
    }
    return config


def write_atomic(path: Path, config: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(config, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        temporary.replace(path)
    except Exception:
        temporary.unlink(missing_ok=True)
        raise


def configure(config_path: Path, raw_home: Path) -> None:
    home = validate_home(raw_home)
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"Unable to read OpenCode config {config_path}: {error}") from error
    if not isinstance(config, dict):
        raise ValueError("OpenCode config must contain a JSON object")
    write_atomic(config_path, merge_config(config, home))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--home", required=True, type=Path)
    args = parser.parse_args()
    try:
        configure(args.config, args.home)
    except (OSError, sqlite3.Error, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
