#!/usr/bin/env python3
"""Repair the photon-node patch's hunk-10 context lines before ``bun install``.

The pinned upstream opencode ref (v1.18.25, ``cb7d8b2f``) carries a
``patches/@silvia-odwyer%2Fphoton-node@0.3.4.patch`` whose hunk 10 context
lines carry one stray leading space. Bun <= 1.4.2 tolerated it; Bun 1.4.4
(like ``git apply``) rejects the patch with
``hunk #10 does not apply to photon_rs.js``. Upstream commit ``61845b0``
("fix: repair photon-node patch context for Bun 1.4.4") trims the stray
spaces; the patched ``photon_rs.js`` is byte-identical under 1.3.14,
1.4.2 and 1.4.4.

This script applies that same whitespace repair at image build time, after
the upstream checkout and before ``bun install --frozen-lockfile``. It is a
deterministic, byte-level rewrite of three context lines in the patch file:
no semantic patch content is added or removed. If the upstream ref ever
lands the fix itself, the script's exact-match guard fails the build so the
now-redundant in-tree repair can be dropped.
"""

import pathlib
import sys

PATCH_REL = "patches/@silvia-odwyer%2Fphoton-node@0.3.4.patch"

# Lines as they live in the pinned upstream patch (one stray leading space
# on each context line of hunk 10). The upstream fix trims the stray space.
BUGGY_CONTEXT = (
    "      ;\n",
    "  };\n",
    "  \n",
)
FIXED_CONTEXT = (
    "    ;\n",
    "};\n",
    "\n",
)


def repair(patch_text: str) -> str:
    if patch_text.count("".join(BUGGY_CONTEXT)) != 1:
        if "    ;\n};\n\n" in patch_text:
            raise SystemExit(
                f"{PATCH_REL}: hunk-10 context already carries the fixed "
                "whitespace; drop this in-tree repair (upstream landed 61845b0)"
            )
        raise SystemExit(
            f"{PATCH_REL}: could not locate the buggy hunk-10 context block; "
            "the pinned upstream patch may have drifted — re-review the fix"
        )
    return patch_text.replace(
        "".join(BUGGY_CONTEXT), "".join(FIXED_CONTEXT), 1,
    )


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(
            "usage: fix_photon_patch_context.py <upstream-checkout-root>"
        )
    root = pathlib.Path(sys.argv[1])
    patch_path = root / PATCH_REL
    if not patch_path.is_file():
        raise SystemExit(f"{PATCH_REL}: not found under {root}")
    patch_text = patch_path.read_text(encoding="utf-8")
    patch_path.write_text(repair(patch_text), encoding="utf-8")


if __name__ == "__main__":
    main()
