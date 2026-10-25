#!/usr/bin/env python3
"""Tests for runtime_smoke.py (WI#424 control 6).

Every check is driven over a fixture transport so the suite's own logic is
pinned without needing a worker image. The mutation set is deliberate: each
failure mode the control exists to catch gets its own case, so a check that
silently stops inspecting (or starts always passing) fails here.
"""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import Iterable, Sequence

import runtime_smoke as smoke


class FixtureTransport(smoke.Transport):
    def __init__(
        self,
        files: dict[str, str] | None = None,
        dirs: Iterable[str] = (),
        runner: "callable | None" = None,
        tcp: str = "",
        homes: Iterable[str] = ("/root", "/home/opencode2"),
    ) -> None:
        self.files = dict(files or {})
        self.dirs = set(dirs)
        self.runner = runner
        self.tcp = tcp
        self.homes = tuple(homes)
        self.calls: list[tuple[Sequence[str], dict[str, str]]] = []

    def read_text(self, path: str) -> str | None:
        return self.files.get(path)

    def exists(self, path: str) -> bool:
        return path in self.files or path in self.dirs

    def is_dir(self, path: str) -> bool:
        return path in self.dirs

    def run(self, argv: Sequence[str], env: dict[str, str]) -> tuple[int, str]:
        self.calls.append((list(argv), dict(env)))
        if self.runner is not None:
            return self.runner(argv, env)
        return (127, "no runner")

    def listening_sockets(self) -> str:
        return self.tcp

    def home_dirs(self) -> Iterable[str]:
        return self.homes


def _env(**overrides: str) -> dict[str, str]:
    env = {
        "HOME": "/root",
        "XDG_CONFIG_HOME": "/root/.config",
        "XDG_DATA_HOME": "/root/.local/share",
        "XDG_STATE_HOME": "/root/.local/state",
    }
    env.update(overrides)
    return env


def _tcp_row(local: str, port: int, state: str = "0A") -> str:
    return (
        "  0: 00000000:0000 00000000:0000 "
        f"{state} 00000000:00000000 00:00000000 00000000  1000        0 "
        f"0 1 0000000000000000 100 0 0 10 0"
    )


def _ipv4_hex(address: str) -> str:
    octets = [int(part) for part in address.split(".")]
    return bytes(reversed(octets)).hex().upper()


def _row(address: str, port: int, state: str = "0A") -> str:
    hex_address = _ipv4_hex(address).rjust(8, "0")
    return (
        f"  0: {hex_address}:{port:04X} 00000000:0000 "
        f"{state} 00000000:00000000 00:00000000 00000000  1000        0 "
        "0 1 0000000000000000 100 0 0 10 0"
    )


class AddressDecodingTests(unittest.TestCase):
    def test_ipv4_is_little_endian(self) -> None:
        self.assertEqual(smoke._decode_address("0100007F"), "127.0.0.1")
        self.assertEqual(smoke._decode_address("00000000"), "0.0.0.0")

    def test_ipv6_loopback(self) -> None:
        self.assertEqual(
            smoke._decode_address("00000000000000000000000001000000"), "::1"
        )

    def test_ipv6_unspecified(self) -> None:
        self.assertEqual(smoke._decode_address("0" * 32), "::")

    def test_loopback_classification(self) -> None:
        self.assertTrue(smoke.is_loopback("127.0.0.1"))
        self.assertTrue(smoke.is_loopback("127.5.5.5"))
        self.assertTrue(smoke.is_loopback("::1"))
        self.assertFalse(smoke.is_loopback("0.0.0.0"))
        self.assertFalse(smoke.is_loopback("192.168.1.10"))
        self.assertFalse(smoke.is_loopback("not-an-address"))


class ListenerParsingTests(unittest.TestCase):
    def test_only_listen_rows_are_returned(self) -> None:
        table = "\n".join(
            [
                "  sl  local_address rem_address   st",
                _row("127.0.0.1", 8443),
                _row("0.0.0.0", 4096),
                _row("127.0.0.1", 1234, state="01"),
            ]
        )
        self.assertEqual(
            smoke.parse_listeners(table), [("127.0.0.1", 8443), ("0.0.0.0", 4096)]
        )

    def test_malformed_rows_are_skipped(self) -> None:
        table = "\n".join(["garbage", "  1: zzzz:yy 0:0 0A 0", _row("127.0.0.1", 80)])
        self.assertEqual(smoke.parse_listeners(table), [("127.0.0.1", 80)])


class ServiceConfigTests(unittest.TestCase):
    def test_absent_is_the_private_boot_default(self) -> None:
        result = smoke.check_service_config(
            FixtureTransport(), _env(OPENCODE_CONFIG_DIR="/run/oc")
        )
        self.assertTrue(result.ok)
        self.assertIn("/run/oc/service.json", result.detail)

    def test_disabled_passes(self) -> None:
        transport = FixtureTransport(
            files={"/root/.config/opencode/service.json": '{"disabled": true}'}
        )
        self.assertTrue(smoke.check_service_config(transport, _env()).ok)

    def test_remote_true_fails(self) -> None:
        transport = FixtureTransport(
            files={
                "/root/.config/opencode/service.json": '{"disabled": true, "remote": true}'
            }
        )
        result = smoke.check_service_config(transport, _env())
        self.assertFalse(result.ok)

    def test_remote_false_still_disabled_passes(self) -> None:
        transport = FixtureTransport(
            files={
                "/root/.config/opencode/service.json": '{"disabled": true, "remote": false}'
            }
        )
        self.assertTrue(smoke.check_service_config(transport, _env()).ok)

    def test_not_disabled_fails(self) -> None:
        transport = FixtureTransport(
            files={"/root/.config/opencode/service.json": "{}"}
        )
        self.assertFalse(smoke.check_service_config(transport, _env()).ok)

    def test_invalid_json_fails(self) -> None:
        transport = FixtureTransport(
            files={"/root/.config/opencode/service.json": "not json"}
        )
        self.assertFalse(smoke.check_service_config(transport, _env()).ok)

    def test_xdg_config_home_precedence(self) -> None:
        env = _env(XDG_CONFIG_HOME="/srv/xdg")
        self.assertEqual(
            smoke.service_config_path(env), "/srv/xdg/opencode/service.json"
        )
        self.assertEqual(
            smoke.service_config_path(_env(OPENCODE_CONFIG_DIR="/srv/oc")),
            "/srv/oc/service.json",
        )


class OpentunnelIdentityTests(unittest.TestCase):
    def test_clean_tree_passes(self) -> None:
        self.assertTrue(
            smoke.check_opentunnel_identity(FixtureTransport(), _env()).ok
        )

    def test_state_dir_under_root_is_caught(self) -> None:
        transport = FixtureTransport(dirs={"/root/.local/state/opentunnel"})
        self.assertFalse(smoke.check_opentunnel_identity(transport, _env()).ok)

    def test_xdg_data_home_is_caught(self) -> None:
        transport = FixtureTransport(
            dirs={"/srv/data/opentunnel"},
            homes=("/srv/home",),
        )
        env = _env(XDG_DATA_HOME="/srv/data", XDG_STATE_HOME="")
        self.assertFalse(smoke.check_opentunnel_identity(transport, env).ok)

    def test_worker_home_is_caught(self) -> None:
        transport = FixtureTransport(dirs={"/home/opencode2/.local/share/opentunnel"})
        self.assertFalse(smoke.check_opentunnel_identity(transport, _env()).ok)


class ListenerCheckTests(unittest.TestCase):
    def test_loopback_and_health_pass(self) -> None:
        table = "\n".join([_row("127.0.0.1", 4096), _row("0.0.0.0", 8443)])
        transport = FixtureTransport(tcp=table)
        self.assertTrue(smoke.check_listeners(transport, 8443).ok)

    def test_public_listener_fails(self) -> None:
        transport = FixtureTransport(tcp=_row("0.0.0.0", 4096))
        result = smoke.check_listeners(transport, 8443)
        self.assertFalse(result.ok)
        self.assertIn("0.0.0.0:4096", result.detail)

    def test_health_port_on_non_loopback_is_allowed_but_others_are_not(self) -> None:
        table = "\n".join([_row("0.0.0.0", 8443), _row("0.0.0.0", 9000)])
        result = smoke.check_listeners(FixtureTransport(tcp=table), 8443)
        self.assertFalse(result.ok)
        self.assertIn("0.0.0.0:9000", result.detail)
        self.assertNotIn("0.0.0.0:8443", result.detail)


class ShareTests(unittest.TestCase):
    def test_disabled_passes(self) -> None:
        transport = FixtureTransport(
            files={"/app/config/opencode2.json": '{"share": "disabled"}'}
        )
        self.assertTrue(
            smoke.check_share_disabled(transport, "/app/config/opencode2.json").ok
        )

    def test_enabled_fails(self) -> None:
        transport = FixtureTransport(
            files={"/app/config/opencode2.json": '{"share": "public"}'}
        )
        self.assertFalse(
            smoke.check_share_disabled(transport, "/app/config/opencode2.json").ok
        )

    def test_missing_config_fails(self) -> None:
        self.assertFalse(
            smoke.check_share_disabled(FixtureTransport(), "/app/config/opencode2.json").ok
        )


class RemoteRefusedTests(unittest.TestCase):
    def test_refusal_passes_and_is_sandboxed(self) -> None:
        transport = FixtureTransport(runner=lambda argv, env: (1, "refused"))
        result = smoke.check_remote_refused(transport, "/usr/local/bin/opencode")
        self.assertTrue(result.ok)
        argv, env = transport.calls[0]
        self.assertEqual(
            argv, ["/usr/local/bin/opencode", "service", "set", "remote", "true"]
        )
        self.assertNotEqual(env["HOME"], "/root")
        self.assertFalse(Path(env["HOME"]).exists())

    def test_acceptance_fails(self) -> None:
        transport = FixtureTransport(runner=lambda argv, env: (0, "enabled"))
        self.assertFalse(
            smoke.check_remote_refused(transport, "/usr/local/bin/opencode").ok
        )

    def test_refusal_that_still_writes_remote_fails(self) -> None:
        def runner(argv: Sequence[str], env: dict[str, str]) -> tuple[int, str]:
            planted = Path(env["XDG_CONFIG_HOME"]) / "opencode"
            planted.mkdir(parents=True, exist_ok=True)
            (planted / "service.json").write_text('{"remote": true}')
            return (1, "refused")

        transport = FixtureTransport(runner=runner)
        self.assertFalse(
            smoke.check_remote_refused(transport, "/usr/local/bin/opencode").ok
        )


class RunChecksTests(unittest.TestCase):
    def test_all_five_checks_are_reported(self) -> None:
        transport = FixtureTransport(
            files={
                "/app/config/opencode2.json": '{"share": "disabled"}',
                "/root/.config/opencode/service.json": '{"disabled": true}',
            },
            runner=lambda argv, env: (1, "refused"),
        )
        results = smoke.run_checks(
            transport, 8443, "/app/config/opencode2.json", "/usr/local/bin/opencode", _env()
        )
        self.assertEqual(len(results), 5)
        self.assertEqual({result.name for result in results}, {
            "service_config",
            "opentunnel_identity",
            "listeners",
            "share_disabled",
            "remote_refused",
        })


class CliWiringTests(unittest.TestCase):
    def _fixture_root(self, directory: str) -> str:
        root = Path(directory)
        (root / "app/config").mkdir(parents=True)
        (root / "app/config/opencode2.json").write_text('{"share": "disabled"}')
        (root / "proc/net").mkdir(parents=True)
        (root / "proc/net/tcp").write_text(_row("127.0.0.1", 4096) + "\n")
        (root / "proc/net/tcp6").write_text("")
        return directory

    def test_clean_fixture_root_exits_zero(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self._fixture_root(directory)
            rc = smoke.main(
                [
                    "--profile",
                    "build",
                    "--root",
                    root,
                    "--config",
                    "/app/config/opencode2.json",
                    "--wrapper",
                    "/usr/local/bin/opencode",
                ]
            )
            self.assertEqual(rc, 0)

    def test_public_listener_in_fixture_root_exits_one(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self._fixture_root(directory)
            (Path(root) / "proc/net/tcp").write_text(_row("0.0.0.0", 4096) + "\n")
            rc = smoke.main(["--profile", "build", "--root", root])
            self.assertEqual(rc, 1)

    def test_remote_profile_requires_a_name(self) -> None:
        self.assertEqual(smoke.main(["--profile", "remote"]), 2)

    def test_container_profile_is_local(self) -> None:
        # `--profile container` runs inside the worker: the same fixture root
        # checks apply, so the clean tree passes without a runtime binary.
        with tempfile.TemporaryDirectory() as directory:
            root = self._fixture_root(directory)
            rc = smoke.main(["--profile", "container", "--root", root])
            self.assertEqual(rc, 0)

    def test_json_output_is_parseable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self._fixture_root(directory)
            import contextlib
            import io

            buffer = io.StringIO()
            with contextlib.redirect_stdout(buffer):
                smoke.main(["--profile", "build", "--root", root, "--json"])
            payload = json.loads(buffer.getvalue())
            self.assertEqual(payload["failed"], 0)
            self.assertEqual(len(payload["results"]), 5)


if __name__ == "__main__":
    unittest.main()
