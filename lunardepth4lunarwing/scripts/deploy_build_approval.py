#!/usr/bin/env python3
"""Temporarily admit the reviewed workspace postinstall at its deploy identity."""

import argparse
from pathlib import Path

PACKAGE = "@lunardepth/dsh-subprocess-local"
PROJECT = "packages/subprocess/subprocess-local"
RELATIVE = f"  '{PACKAGE}@file:{PROJECT}': true\n"


def deploy_approval(source_root: Path) -> str:
    uri = (source_root / PROJECT).resolve().as_uri()
    return f"  '{PACKAGE}@{uri}': true\n"


def add(workspace: Path, source_root: Path) -> None:
    source = workspace.read_text(encoding="utf-8")
    approval = deploy_approval(source_root)
    if source.count(RELATIVE) != 1 or approval in source:
        raise ValueError(f"reviewed build approval is absent or already deployed: {workspace}")
    workspace.write_text(source.replace(RELATIVE, RELATIVE + approval), encoding="utf-8")


def remove(workspace: Path, source_root: Path) -> None:
    source = workspace.read_text(encoding="utf-8")
    approval = deploy_approval(source_root)
    if source.count(RELATIVE + approval) != 1:
        raise ValueError(f"deployed build approval is absent: {workspace}")
    workspace.write_text(source.replace(RELATIVE + approval, RELATIVE), encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("add", "remove"))
    parser.add_argument("workspace", type=Path)
    parser.add_argument("source_root", type=Path)
    args = parser.parse_args()
    if args.action == "add":
        add(args.workspace, args.source_root)
    else:
        remove(args.workspace, args.source_root)


if __name__ == "__main__":
    main()
