#!/usr/bin/env python3
"""End-to-end tests for opencode2 out-of-band image verification (WI#425)."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
VERIFY = ROOT / ".gitlab" / "scripts" / "verify-opencode2-exposure-out-of-band.py"
LANE = ROOT / "opencode2.4lunarwing"
ASSERT = LANE / "scripts" / "exposure_policy_assert.py"
WRAPPER = LANE / "scripts" / "opencode-exposure-guard.sh"


def load_verify_module():
    spec = importlib.util.spec_from_file_location("verify_oob", VERIFY)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def clean_config() -> dict:
    return {
        "config": {
            "Env": [
                "PATH=/home/opencode2/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:"
                "/home/opencode2/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:"
                "/sbin:/bin:/usr/local/bun-node-fallback-bin",
                "BUN_RUNTIME_TRANSPILER_CACHE_PATH=0",
                "BUN_INSTALL_BIN=/usr/local/bin",
                "OPENCODE_IMAGE_REF=v2.1.2@" + "a" * 40,
                "OPENCODE_DISABLE_AUTOUPDATE=1",
                "RUSTUP_HOME=/usr/local/rustup",
                "GOROOT=/usr/local/go",
                "GOPATH=/home/opencode2/go",
                "CARGO_HOME=/home/opencode2/.cargo",
            ],
            "Entrypoint": ["/usr/bin/tini", "--", "/app/entrypoint.sh"],
            "Cmd": ["--mode", "websocket"],
            "User": "10003:10003",
            "WorkingDir": "/app",
            "Healthcheck": {
                "Test": ["CMD-SHELL", "curl -sf http://127.0.0.1:8443/health || exit 1"],
                "Interval": 30_000_000_000,
                "Timeout": 5_000_000_000,
                "StartPeriod": 30_000_000_000,
                "Retries": 3,
            },
            "ExposedPorts": {"8443/tcp": {}, "9090/tcp": {}},
            "StopSignal": "SIGTERM",
            "Shell": None,
        }
    }


def image_environment() -> dict[str, str]:
    return dict(entry.split("=", 1) for entry in clean_config()["config"]["Env"])


class ConfigHalfTests(unittest.TestCase):
    def setUp(self):
        self.module = load_verify_module()

    def assert_refused(self, config: dict, text: str):
        with contextlib.redirect_stderr(io.StringIO()) as stderr, \
                self.assertRaises(SystemExit) as caught:
            self.module.check_config_half(config)
        self.assertEqual(caught.exception.code, 1)
        self.assertIn(text, stderr.getvalue())

    def test_clean_config_passes(self):
        self.module.check_config_half(clean_config())

    def test_adversarial_runtime_shapes_are_refused(self):
        cases = {
            "entrypoint": ("Entrypoint", ["/usr/local/lib/opencode2/opencode"]),
            "cmd": ("Cmd", ["serve", "--hostname", "0.0.0.0"]),
            "user": ("User", "0:0"),
            "workdir": ("WorkingDir", "/"),
            "healthcheck": ("Healthcheck", {"Test": ["CMD-SHELL", "/usr/local/lib/opencode2/opencode serve --hostname 0.0.0.0 & exit 0"]}),
            "onbuild": ("OnBuild", ["RUN /usr/local/lib/opencode2/opencode serve"]),
            "ports": ("ExposedPorts", {"4096/tcp": {}}),
            "volumes": ("Volumes", {"/workspace": {}}),
            "stop-signal": ("StopSignal", "SIGKILL"),
            "shell": ("Shell", ["/bin/false"]),
        }
        for name, (key, value) in cases.items():
            with self.subTest(name=name):
                config = clean_config()
                config["config"][key] = value
                self.assert_refused(config, key)

    def test_path_order_and_loader_drift_are_refused(self):
        for entry in ("PATH=/tmp:/usr/local/bin", "LD_PRELOAD=/tmp/evil.so"):
            with self.subTest(entry=entry):
                config = clean_config()
                config["config"]["Env"].append(entry)
                self.assert_refused(config, entry.split("=", 1)[0])

    def test_binary_retargets_and_missing_kill_switch_are_refused(self):
        for entry in (
            "OPENCODE_BIN=/usr/local/lib/opencode2/opencode",
            "LUNARWING_REAL_OPENCODE=/tmp/evil",
        ):
            with self.subTest(entry=entry):
                config = clean_config()
                config["config"]["Env"].append(entry)
                self.assert_refused(config, "retargets")
        config = clean_config()
        config["config"]["Env"] = [
            entry for entry in config["config"]["Env"]
            if not entry.startswith("OPENCODE_DISABLE_AUTOUPDATE=")
        ]
        self.assert_refused(config, "OPENCODE_DISABLE_AUTOUPDATE")


class LocalImageInspectTests(unittest.TestCase):
    def setUp(self):
        self.module = load_verify_module()

    def assert_config_accepted(self, payload: object):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "inspect.json"
            path.write_text(json.dumps(payload), encoding="utf-8")
            config = self.module._local_config(path)
        self.module.check_config_half(config)
        self.assertEqual(self.module._environment(config), image_environment())

    def test_docker_inspect_uses_config_healthcheck(self):
        self.assert_config_accepted([{"Config": clean_config()["config"]}])

    def test_podman_inspect_merges_top_level_healthcheck(self):
        section = clean_config()["config"]
        healthcheck = section.pop("Healthcheck")
        self.assert_config_accepted([{"Config": section, "Healthcheck": healthcheck}])

    def test_runtime_inspect_requires_exactly_one_image(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "inspect.json"
            path.write_text("[]", encoding="utf-8")
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                self.module._local_config(path)


class TarFixture:
    def __init__(self, lane: pathlib.Path):
        self.entries: dict[str, dict] = {}
        self.add_dir("")
        for name in (
            "app", "app/node_modules", "usr", "usr/bin", "usr/local", "usr/local/bin",
            "usr/local/lib", "usr/local/lib/opencode2",
        ):
            self.add_dir(name)
        self.add_dir("app/scripts", mode=0o555)
        self.add_dir("app/config", mode=0o555)
        for name in ("entrypoint.sh", "health_server.py", "agent_comm_protocol.json"):
            self.add_file(f"app/{name}", (lane / name).read_bytes(), mode=0o444)
        for directory in ("scripts", "config"):
            for path in (lane / directory).rglob("*"):
                name = f"app/{path.relative_to(lane).as_posix()}"
                if path.is_dir():
                    self.add_dir(name, mode=0o555)
                else:
                    self.add_file(name, path.read_bytes(), mode=0o444)
        self.add_file("usr/local/bin/opencode", WRAPPER.read_bytes(), mode=0o755)
        self.add_file(
            "usr/local/lib/opencode2/opencode", b"\x7fELF clean pinned build", mode=0o755,
        )
        self.add_file("usr/bin/tini", b"\x7fELF pinned tini", mode=0o755)

    def _add_parents(self, name: str):
        for parent in reversed(list(pathlib.PurePosixPath(name).parents)):
            key = str(parent)
            if key not in ("", ".") and key not in self.entries:
                self.add_dir(key)

    def add_dir(self, name: str, mode: int = 0o755, uid: int = 0):
        self._add_parents(name)
        self.entries[name] = {"kind": "dir", "mode": mode, "uid": uid, "gid": 0}

    def add_file(self, name: str, data: bytes, mode: int = 0o444, uid: int = 0):
        self._add_parents(name)
        self.entries[name] = {
            "kind": "file", "mode": mode, "uid": uid, "gid": 0, "data": data,
        }

    def add_symlink(self, name: str, target: str):
        self._add_parents(name)
        self.entries[name] = {
            "kind": "symlink", "mode": 0o777, "uid": 0, "gid": 0, "target": target,
        }

    def write(self, archive_path: pathlib.Path):
        with tarfile.open(archive_path, "w") as archive:
            for name, entry in self.entries.items():
                member = tarfile.TarInfo(name or ".")
                member.mode = entry["mode"]
                member.uid = entry["uid"]
                member.gid = entry["gid"]
                member.pax_headers.update(entry.get("pax", {}))
                if entry["kind"] == "dir":
                    member.type = tarfile.DIRTYPE
                    archive.addfile(member)
                elif entry["kind"] == "symlink":
                    member.type = tarfile.SYMTYPE
                    member.linkname = entry["target"]
                    archive.addfile(member)
                else:
                    data = entry["data"]
                    member.type = tarfile.REGTYPE
                    member.size = len(data)
                    archive.addfile(member, io.BytesIO(data))


class FilesystemExportTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.lane = self.root / "lane"
        shutil.copytree(
            LANE,
            self.lane,
            ignore=shutil.ignore_patterns("node_modules", "__pycache__", "*.pyc"),
        )
        self.module = load_verify_module()
        self.module.REPO_ROOT = self.root
        self.module.LANE = self.lane
        self.module.ASSERT = ASSERT
        self.fixture = TarFixture(self.lane)
        self.archive = self.root / "rootfs.tar"
        self.fixture.write(self.archive)

    def verify(self) -> tuple[int, str, str]:
        stdout, stderr = io.StringIO(), io.StringIO()
        code = 0
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            try:
                self.module.check_filesystem_half(self.archive, image_environment())
            except SystemExit as error:
                code = error.code
        return code, stdout.getvalue(), stderr.getvalue()

    def assert_refused(self, text: str):
        code, _, stderr = self.verify()
        self.assertEqual(code, 1, stderr)
        self.assertIn(text, stderr)

    def test_legitimate_export_passes_end_to_end(self):
        code, stdout, stderr = self.verify()
        self.assertEqual(code, 0, stderr)
        self.assertIn("all checks passed", stdout)
        self.assertIn("tar metadata and checkout-owned files clean", stdout)

    def test_buildkits_missing_root_header_is_a_structural_boundary(self):
        del self.fixture.entries[""]
        self.fixture.write(self.archive)
        code, _, stderr = self.verify()
        self.assertEqual(code, 0, stderr)

    def test_ci_job_environment_is_not_used_as_image_environment(self):
        with mock.patch.dict(os.environ, {"OPENCODE_DISABLE_AUTOUPDATE": ""}):
            code, _, stderr = self.verify()
        self.assertEqual(code, 0, stderr)

    def test_tampered_installed_wrapper_is_refused(self):
        self.fixture.add_file("usr/local/bin/opencode", b"#!/bin/sh\n# tampered\n", mode=0o755)
        self.fixture.write(self.archive)
        self.assert_refused("out-of-band filesystem verification FAILED")

    def test_tampered_checkout_owned_entrypoint_is_refused(self):
        path = self.lane / "entrypoint.sh"
        self.fixture.add_file("app/entrypoint.sh", path.read_bytes() + b"# tampered\n")
        self.fixture.write(self.archive)
        self.assert_refused("exported app/entrypoint.sh differs from the checkout")

    def test_unexpected_app_entry_is_refused(self):
        self.fixture.add_file("app/unexpected.sh", b"#!/bin/sh\n")
        self.fixture.write(self.archive)
        self.assert_refused("unexpected /app entry app/unexpected.sh")

    def test_tar_uid_not_host_uid_controls_wrapper_ownership(self):
        self.fixture.add_file("usr/local/bin/opencode", WRAPPER.read_bytes(), mode=0o755, uid=10003)
        self.fixture.write(self.archive)
        self.assert_refused("owned by uid 0")

    def test_tar_uid_controls_checkout_file_ownership(self):
        self.fixture.add_file("app/entrypoint.sh", (self.lane / "entrypoint.sh").read_bytes(), uid=10003)
        self.fixture.write(self.archive)
        self.assert_refused("root-owned and read-only")

    def test_writable_parent_from_tar_header_is_refused(self):
        self.fixture.add_dir("usr/local", mode=0o777)
        self.fixture.write(self.archive)
        self.assert_refused("parent")
        self.assertIn("writable", self.verify()[2])

    def test_owner_only_executable_wrapper_is_refused(self):
        self.fixture.add_file("usr/local/bin/opencode", WRAPPER.read_bytes(), mode=0o744)
        self.fixture.write(self.archive)
        self.assert_refused("GX-3")

    def test_symlinked_wrapper_never_gets_followed(self):
        self.fixture.entries["usr/local/bin/opencode"] = {
            "kind": "symlink", "mode": 0o777, "uid": 0, "gid": 0, "target": str(WRAPPER),
        }
        self.fixture.write(self.archive)
        self.assert_refused("must be a regular file, not a link")

    def test_tunnel_marker_in_real_binary_is_refused(self):
        self.fixture.add_file(
            "usr/local/lib/opencode2/opencode", b"binary with https://opentunnel.xyz", mode=0o755,
        )
        self.fixture.write(self.archive)
        self.assert_refused("opentunnel.xyz")

    def test_missing_binary_and_archive_name_real_causes(self):
        del self.fixture.entries["usr/local/lib/opencode2/opencode"]
        self.fixture.write(self.archive)
        self.assert_refused("installed file usr/local/lib/opencode2/opencode")
        with contextlib.redirect_stderr(io.StringIO()) as stderr, \
                self.assertRaises(SystemExit):
            self.module.check_filesystem_half(self.root / "missing.tar", image_environment())
        self.assertIn("missing or unreadable", stderr.getvalue())


    def test_earlier_path_entries_of_any_type_are_refused(self):
        cases = (
            ("usr/local/sbin/opencode", "file"),
            ("usr/local/go/bin/opencode", "symlink"),
            ("home/opencode2/.cargo/bin/opencode", "file"),
            ("home/opencode2/.cargo/bin", "symlinked-directory"),
        )
        for name, kind in cases:
            with self.subTest(name=name, kind=kind):
                self.fixture = TarFixture(self.lane)
                if kind == "file":
                    self.fixture.add_file(name, b"#!/bin/sh\n", mode=0o755)
                elif kind == "symlink":
                    self.fixture.add_symlink(name, "/usr/local/lib/opencode2/opencode")
                else:
                    self.fixture.add_symlink(name, "/usr/local/lib/opencode2")
                self.fixture.write(self.archive)
                self.assert_refused("PATH shadow")

    def test_node_modules_owner_mode_links_and_capabilities_are_checked(self):
        name = "app/node_modules/@opencode/client/package.json"
        cases = (
            ("owner", {"uid": 10003}, "root:root"),
            ("group-write", {"mode": 0o664}, "privilege or write bits"),
            ("other-write", {"mode": 0o646}, "privilege or write bits"),
            ("setuid", {"mode": 0o4644}, "privilege or write bits"),
        )
        for label, overrides, expected in cases:
            with self.subTest(label=label):
                self.fixture = TarFixture(self.lane)
                values = {"data": b"{}\n", "mode": 0o444, "uid": 0, **overrides}
                self.fixture.add_file(name, **values)
                self.fixture.write(self.archive)
                self.assert_refused(expected)
        for label, capability in (("link", False), ("capability", True)):
            with self.subTest(label=label):
                self.fixture = TarFixture(self.lane)
                if capability:
                    self.fixture.add_file(name, b"{}\n")
                    self.fixture.entries[name]["pax"] = {
                        "SCHILY.xattr.security.capability": "AQAAAoAAAAAA",
                    }
                else:
                    self.fixture.add_symlink(name, "/bin/sh")
                self.fixture.write(self.archive)
                self.assert_refused("protected path")

    def test_real_binary_privilege_bits_and_capabilities_are_refused(self):
        for mode in (0o4755, 0o2755):
            with self.subTest(mode=oct(mode)):
                self.fixture = TarFixture(self.lane)
                self.fixture.add_file(
                    "usr/local/lib/opencode2/opencode", b"clean", mode=mode,
                )
                self.fixture.write(self.archive)
                self.assert_refused("setuid/setgid")
        self.fixture = TarFixture(self.lane)
        self.fixture.entries["usr/local/lib/opencode2/opencode"]["pax"] = {
            "security.capability": "AQAAAoAAAAAA",
        }
        self.fixture.write(self.archive)
        self.assert_refused("file capabilities")

    def test_loader_preload_and_missing_or_linked_tini_are_refused(self):
        self.fixture.add_file("etc/ld.so.preload", b"/tmp/evil.so\n")
        self.fixture.write(self.archive)
        self.assert_refused("ld.so.preload")
        for linked in (False, True):
            with self.subTest(linked=linked):
                self.fixture = TarFixture(self.lane)
                del self.fixture.entries["usr/bin/tini"]
                if linked:
                    self.fixture.add_symlink("usr/bin/tini", "/bin/true")
                self.fixture.write(self.archive)
                self.assert_refused("installed file usr/bin/tini")

    def test_whiteouts_duplicate_and_unsafe_names_are_refused(self):
        self.fixture.add_file("app/.wh.planted", b"")
        self.fixture.write(self.archive)
        self.assert_refused("whiteout")
        self.fixture = TarFixture(self.lane)
        self.fixture.add_file("app//entrypoint.sh", b"duplicate", mode=0o444)
        self.fixture.write(self.archive)
        self.assert_refused("duplicate member")
        self.fixture = TarFixture(self.lane)
        self.fixture.entries["../escape"] = {
            "kind": "file", "mode": 0o444, "uid": 0, "gid": 0, "data": b"x",
        }
        self.fixture.write(self.archive)
        self.assert_refused("unsafe member name")

    def test_extra_file_in_source_directory_is_refused(self):
        self.fixture.add_file("app/scripts/planted.py", b"print('planted')\n")
        self.fixture.write(self.archive)
        self.assert_refused("unexpected=['app/scripts/planted.py']")

    def test_node_modules_tree_is_required(self):
        del self.fixture.entries["app/node_modules"]
        self.fixture.write(self.archive)
        self.assert_refused("directory app/node_modules")


    def test_local_archive_config_mode_uses_the_shared_verifier(self):
        config = self.root / "image-config.json"
        config.write_text(json.dumps(clean_config()), encoding="utf-8")
        workspace = self.root / "nobody-work"
        workspace.mkdir()
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            result = self.module.main([
                "--archive", str(self.archive), "--config", str(config),
                "--workspace-dir", str(workspace),
            ])
        self.assertEqual(result, 0)
        self.assertIn("local export", stdout.getvalue())


class MainWiringTests(unittest.TestCase):
    def setUp(self):
        self.module = load_verify_module()
        self.digest = "sha256:" + "b" * 64
        self.environment = {
            "WORKER_IMAGE_REF": "registry.example/group/worker-opencode2:candidate",
            "WORKER_IMAGE_DIGEST": self.digest,
            "OPENCODE_DISABLE_AUTOUPDATE": "",
        }

    def test_main_runs_both_halves_with_the_fetched_image_environment(self):
        config = clean_config()
        calls = []
        with mock.patch.dict(os.environ, self.environment), \
                mock.patch.object(self.module, "_fetch_candidate_config", return_value=config), \
                mock.patch.object(
                    self.module, "check_config_half", side_effect=lambda value: calls.append(("config", value)),
                ), \
                mock.patch.object(
                    self.module, "check_filesystem_half",
                    side_effect=lambda path, env: calls.append(("filesystem", path, env)),
                ):
            self.assertEqual(self.module.main([]), 0)
        self.assertEqual(calls[0], ("config", config))
        self.assertEqual(calls[1][0:2], ("filesystem", self.module.REPO_ROOT / "opencode2-rootfs.tar"))
        self.assertEqual(calls[1][2]["OPENCODE_DISABLE_AUTOUPDATE"], "1")

    def test_main_config_refusal_skips_the_filesystem_half(self):
        config = clean_config()
        config["config"]["User"] = "0:0"
        with mock.patch.dict(os.environ, self.environment), \
                mock.patch.object(self.module, "_fetch_candidate_config", return_value=config), \
                mock.patch.object(self.module, "check_filesystem_half") as filesystem, \
                contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                self.module.main([])
        filesystem.assert_not_called()

    def test_main_rejects_a_malformed_digest_before_fetching(self):
        self.environment["WORKER_IMAGE_DIGEST"] = "latest"
        with mock.patch.dict(os.environ, self.environment), \
                mock.patch.object(self.module, "_fetch_candidate_config") as fetch, \
                contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                self.module.main([])
        fetch.assert_not_called()


class BuildScriptWiringTests(unittest.TestCase):
    def setUp(self):
        self.build_script = (ROOT / ".gitlab" / "scripts" / "build-worker-image.sh").read_text()
        self.containers = (ROOT / ".gitlab" / "ci" / "containers.yml").read_text()

    def test_candidate_exports_tar_output(self):
        self.assertIn('build_image_extra_output="type=tar,dest=$export_file"', self.build_script)
        self.assertIn('export_file="$CI_PROJECT_DIR/opencode2-rootfs.tar"', self.build_script)
        self.assertIn("[ ! -s \"$export_file\" ]", self.build_script)

    def test_rebuild_does_not_re_export(self):
        reset_index = self.build_script.index('build_image_extra_output=""\n')
        rebuild_index = self.build_script.index('build_image_with_retry "$rebuild_ref"')
        self.assertLess(reset_index, rebuild_index)

    def test_build_image_appends_extra_output(self):
        self.assertIn(
            '[ -n "${build_image_extra_output:-}" ] && set -- "$@" --output "$build_image_extra_output"',
            self.build_script,
        )

    def test_verify_job_and_artifact_are_wired(self):
        self.assertIn("      - opencode2-rootfs.tar\n", self.containers)
        self.assertIn("worker-opencode2-exposure-verify:", self.containers)
        self.assertIn("    - python3 .gitlab/scripts/verify-opencode2-exposure-out-of-band.py", self.containers)


class GoParserPolicyTests(unittest.TestCase):
    PARSER_DIR = ROOT / ".gitlab" / "go" / "dockerfile-parse"

    def test_module_is_pinned_to_the_candidate_buildkit(self):
        go_mod = (self.PARSER_DIR / "go.mod").read_text(encoding="utf-8")
        self.assertIn("github.com/moby/buildkit v0.34.1", go_mod)
        go_sum = (self.PARSER_DIR / "go.sum").read_text(encoding="utf-8")
        self.assertIn("github.com/moby/buildkit v0.34.1", go_sum)

    def test_policy_source_applies_the_parser(self):
        main_go = (self.PARSER_DIR / "main.go").read_text(encoding="utf-8")
        self.assertIn('"github.com/moby/buildkit/frontend/dockerfile/parser"', main_go)
        self.assertIn("parser.Parse", main_go)

    @unittest.skipUnless(shutil.which("go") is not None, "go toolchain not available")
    def test_go_tests_pass_on_the_real_tree(self):
        result = subprocess.run(
            ["go", "test", "./..."], cwd=self.PARSER_DIR,
            capture_output=True, text=True, timeout=300,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ok", result.stdout)

    @unittest.skipUnless(shutil.which("go") is not None, "go toolchain not available")
    def test_parser_policy_accepts_the_real_dockerfile(self):
        result = subprocess.run(
            ["go", "run", ".", str(LANE / "Dockerfile")], cwd=self.PARSER_DIR,
            capture_output=True, text=True, timeout=300,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("BuildKit parser): ok", result.stdout)


if __name__ == "__main__":
    unittest.main()
