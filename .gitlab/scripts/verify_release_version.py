#!/usr/bin/env python3

import argparse
import os
import pathlib
import re
import sys
import tempfile
import tomllib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
TAG_PATTERN = re.compile(
    r"^v(?P<major>\d+)\.(?P<minor>\d+)\.(?P<patch>\d+)\.(?P<release>\d+)"
    r"(?:-rc(?P<rc>[1-9]\d*))?$"
)
MANIFESTS = [
    pathlib.Path("ic/Cargo.toml"),
    pathlib.Path("ic/crates/lunarwing_common/Cargo.toml"),
    pathlib.Path("ic/crates/lunarwing_engine/Cargo.toml"),
    pathlib.Path("ic/crates/lunarwing_health_metrics/Cargo.toml"),
    pathlib.Path("ic/crates/lunarwing_safety/Cargo.toml"),
    pathlib.Path("ic/crates/lunarwing_skills/Cargo.toml"),
]


def validate_release(tag: str, protected: bool, root: pathlib.Path) -> list[str]:
    errors: list[str] = []
    match = TAG_PATTERN.fullmatch(tag)
    if not match:
        return [f"unsupported release tag: {tag!r}"]
    if not protected:
        errors.append(f"release tag {tag} is not protected")

    parts = match.groupdict()
    release_version = ".".join(
        (parts["major"], parts["minor"], parts["patch"], parts["release"])
    )
    crate_version = ".".join((parts["major"], parts["minor"], parts["patch"]))

    for manifest in MANIFESTS:
        path = root / manifest
        if not path.is_file():
            errors.append(f"missing Cargo manifest: {manifest}")
            continue
        with path.open("rb") as stream:
            version = tomllib.load(stream).get("package", {}).get("version")
        if version != crate_version:
            errors.append(
                f"{manifest} has version {version!r}; expected {crate_version!r}"
            )

    notes = root / f"RELEASE-v{release_version}.md"
    if not notes.is_file():
        errors.append(f"missing release notes: {notes.name}")
    else:
        content = notes.read_text(encoding="utf-8")
        if release_version not in content:
            errors.append(f"{notes.name} does not contain {release_version}")
        codename = next(
            (line for line in content.splitlines() if "codename" in line.lower()),
            "",
        )
        if not codename or "unknown" in codename.lower():
            errors.append(f"{notes.name} must name a non-UNKNOWN codename")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ReleaseVersionTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    tag = os.environ.get("CI_COMMIT_TAG", "")
    protected = os.environ.get("CI_COMMIT_REF_PROTECTED") == "true"
    errors = validate_release(tag, protected, ROOT)

    if errors:
        print("\n".join(f"ERROR: {error}" for error in errors), file=sys.stderr)
        return 1

    print(
        f"Validated protected release tag {tag} against crate manifests and release notes."
    )
    return 0


class ReleaseVersionTests(unittest.TestCase):
    def make_release_tree(
        self,
        root: pathlib.Path,
        crate_version: str = "2.0.1",
        codename: str = "Togishi",
    ) -> None:
        for manifest in MANIFESTS:
            path = root / manifest
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                f'[package]\nname = "fixture"\nversion = "{crate_version}"\n',
                encoding="utf-8",
            )
        (root / "RELEASE-v2.0.1.0.md").write_text(
            f"# LunarWing v2.0.1.0 - Codename `{codename}`\n",
            encoding="utf-8",
        )

    def test_final_and_rc_tags_pass_complete_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.make_release_tree(root)

            self.assertEqual(validate_release("v2.0.1.0", True, root), [])
            self.assertEqual(validate_release("v2.0.1.0-rc1", True, root), [])

    def test_malformed_release_tags_fail(self) -> None:
        for tag in ("v2.0.1", "v2.0.1.0-rc0", "v2.0.1.0-rc01", "version-2"):
            with self.subTest(tag=tag):
                self.assertIn("unsupported release tag", validate_release(tag, True, ROOT)[0])

    def test_unprotected_tag_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.make_release_tree(root)

            errors = validate_release("v2.0.1.0", False, root)

        self.assertIn("not protected", errors[0])

    def test_manifest_mismatch_and_unknown_codename_fail(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.make_release_tree(root, crate_version="2.0.0", codename="UNKNOWN")

            errors = validate_release("v2.0.1.0", True, root)

        self.assertTrue(any("expected '2.0.1'" in error for error in errors))
        self.assertTrue(any("non-UNKNOWN codename" in error for error in errors))


if __name__ == "__main__":
    raise SystemExit(main())
