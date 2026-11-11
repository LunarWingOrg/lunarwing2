#!/usr/bin/env python3
"""Select pnpm's shared-lockfile deploy in the reviewed pinned build script."""

import argparse
from pathlib import Path

LEGACY_DEPLOY = """      'deploy',
      '--legacy',
      '--prod',
      // Production deployment omits workspace tooling such as Electron's patched signer.
      '--config.allow-unused-patches=true',
      '--config.node-linker=hoisted',
      '--config.auto-install-peers=false',
      '--config.link-workspace-packages=true',
"""

LOCKFILE_DEPLOY = """      'deploy',
      '--prod',
      '--frozen-lockfile',
      '--config.inject-workspace-packages=true',
      '--config.minimumReleaseAgeStrict=true',
      // Production deployment omits workspace tooling such as Electron's patched signer.
      '--config.allow-unused-patches=true',
      '--config.node-linker=hoisted',
      '--config.link-workspace-packages=true',
"""


def prepare(path: Path) -> None:
    source = path.read_text(encoding="utf-8")
    if source.count(LEGACY_DEPLOY) != 1 or LOCKFILE_DEPLOY in source:
        raise ValueError(f"pinned deploy block is absent or already transformed: {path}")
    path.write_text(source.replace(LEGACY_DEPLOY, LOCKFILE_DEPLOY), encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("build_script", type=Path)
    args = parser.parse_args()
    prepare(args.build_script)


if __name__ == "__main__":
    main()
