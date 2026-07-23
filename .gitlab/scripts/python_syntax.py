#!/usr/bin/env python3

import ast
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]
EXCLUDED = {
    "tensorzero-proxy-configurations/experimental-ironclaw-proxy-version-with-enhancements.py",
    "tensorzero-proxy-configurations/experimental-ironclaw-proxy-version-with-enhancements-working-copy.py",
}


def main() -> int:
    output = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", "*.py"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    tracked = {
        path for path in output.splitlines() if (ROOT / path).is_file()
    }
    stale_exclusions = EXCLUDED - tracked
    if stale_exclusions:
        print(f"ERROR: stale Python syntax exclusions: {sorted(stale_exclusions)}", file=sys.stderr)
        return 1

    failures: list[str] = []
    checked = 0
    for relative_path in sorted(tracked - EXCLUDED):
        path = ROOT / relative_path
        try:
            ast.parse(path.read_text(encoding="utf-8"), filename=relative_path)
            checked += 1
        except (SyntaxError, UnicodeError) as error:
            failures.append(f"{relative_path}: {error}")

    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1

    print(f"Validated Python syntax in {checked} tracked files.")
    print(f"Excluded {len(EXCLUDED)} documented experimental working copies.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
