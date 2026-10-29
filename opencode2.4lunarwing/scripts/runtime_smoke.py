#!/usr/bin/env python3
"""WI#424 control 6 — opencode2 worker runtime exposure smoke suite.

WI#357 control 6 said the runtime smoke "extends WI#345", and WI#345 was
closed as a stub with no suite behind it, so nothing asserted the *running*
lane's exposure surface. This suite is that check: it verifies the built
image (``--profile build``, run from the Dockerfile's self-test chain, i.e.
the protected candidate stage) and a live tenant container
(``--profile container --container NAME``) against the same five assertions:

1. no ``service.json`` carrying ``remote`` (absent is the private default);
2. no ``opentunnel`` identity directory under any XDG data/state home;
3. no listener beyond loopback except the health port;
4. the lane config resolves ``share`` to ``disabled``;
5. ``opencode service set remote true`` is refused.

Control 9 (``mt-admin worker-listeners``) covers only the live-listener half
and only for a running container. Everything here is a naive-use guard of the
same class as the wrapper and config pins: the real binary stays executable,
so a compromised agent can still try. Only the egress allowlist (WI#424
control 1) holds against that.

Exit codes: 0 all checks passed, 1 a check failed, 2 usage/transport error.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence

DEFAULT_HEALTH_PORT = 8443
DEFAULT_CONFIG = "/app/config/opencode2.json"
LANE_WRAPPER = "/usr/local/bin/opencode"
LOOPBACK_V4 = "127.0.0.1"
LOOPBACK_V6 = "::1"

# /proc/net/tcp{,6} socket states. 0A is TCP LISTEN.
_TCP_LISTEN = "0A"


@dataclass(frozen=True)
class CheckResult:
    name: str
    ok: bool
    detail: str


class Transport:
    """Filesystem / process access used by every check."""

    def read_text(self, path: str) -> str | None:
        raise NotImplementedError

    def exists(self, path: str) -> bool:
        raise NotImplementedError

    def is_dir(self, path: str) -> bool:
        raise NotImplementedError

    def is_executable(self, path: str) -> bool:
        raise NotImplementedError

    def run(self, argv: Sequence[str], env: dict[str, str]) -> tuple[int, str]:
        raise NotImplementedError

    def listening_sockets(self) -> str:
        raise NotImplementedError

    def socket_owners(self) -> dict[int, int]:
        """Map listening-socket inode -> owning uid. WI#511: lets the
        listeners check ignore host-owned sockets when the inspected
        namespace is shared with the host (image builds)."""
        return {}

    def home_dirs(self) -> Iterable[str]:
        raise NotImplementedError

    def environment(self) -> dict[str, str]:
        raise NotImplementedError


def _safe_read(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except (FileNotFoundError, NotADirectoryError):
        return None


class LocalTransport(Transport):
    def __init__(self, root: str = "/") -> None:
        self.root = Path(root)

    def _resolve(self, path: str) -> Path:
        if self.root == Path("/"):
            return Path(path)
        return self.root / path.lstrip("/")

    def read_text(self, path: str) -> str | None:
        return _safe_read(self._resolve(path))

    def exists(self, path: str) -> bool:
        return self._resolve(path).exists()

    def is_dir(self, path: str) -> bool:
        return self._resolve(path).is_dir()

    def is_executable(self, path: str) -> bool:
        try:
            resolved = self._resolve(path)
            return resolved.is_file() and resolved.stat().st_mode & 0o111 == 0o111
        except OSError:
            return False

    def run(self, argv: Sequence[str], env: dict[str, str]) -> tuple[int, str]:
        if not argv:
            return (127, "empty command")
        command = [str(self._resolve(argv[0])), *argv[1:]]
        try:
            proc = subprocess.run(
                command,
                env=env,
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            return (127, str(exc))
        return (proc.returncode, (proc.stdout or "") + (proc.stderr or ""))

    def listening_sockets(self) -> str:
        parts = [_safe_read(self._resolve("/proc/net/tcp")) or ""]
        parts.append(_safe_read(self._resolve("/proc/net/tcp6")) or "")
        return "\n".join(parts)

    def socket_owners(self) -> dict[int, int]:
        # WI#511: image builds share the HOST netns, so the raw socket table
        # is full of host listeners that are not the worker's. Attribute each
        # listening socket inode to the uid of the process holding it, so the
        # listeners check judges only sockets the runtime user itself owns.
        owners: dict[int, int] = {}
        for status in self._resolve("/proc").glob("[0-9]*/status"):
            fd_dir = status.parent / "fd"
            try:
                uid_line = next(
                    line
                    for line in status.read_text(
                        encoding="utf-8", errors="replace"
                    ).splitlines()
                    if line.startswith("Uid:")
                ).split()[1]
                uid = int(uid_line)
            except (OSError, StopIteration, IndexError, ValueError):
                continue
            try:
                links = os.listdir(fd_dir)
            except OSError:
                continue
            for link in links:
                try:
                    target = os.readlink(fd_dir / link)
                except OSError:
                    continue
                if target.startswith("socket:["):
                    try:
                        owners[int(target[len("socket:[") : -1])] = uid
                    except ValueError:
                        continue
        return owners

    def home_dirs(self) -> Iterable[str]:
        if os.geteuid() == 0:
            yield "/root"
        yield "/home/opencode2"

    def environment(self) -> dict[str, str]:
        return dict(os.environ)


class ContainerTransport(Transport):
    def __init__(self, runtime: str, container: str) -> None:
        self.runtime = runtime
        self.container = container

    def _exec(self, argv: Sequence[str]) -> tuple[int, str]:
        try:
            proc = subprocess.run(
                [self.runtime, "exec", self.container, *argv],
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            return (127, str(exc))
        return (proc.returncode, (proc.stdout or "") + (proc.stderr or ""))

    def read_text(self, path: str) -> str | None:
        rc, out = self._exec(["cat", "--", path])
        if rc == 0:
            return out
        exists_rc, _ = self._exec(["test", "-e", path])
        if exists_rc != 0:
            return None
        raise OSError(f"{path} exists but is unreadable: {out.strip()}")

    def exists(self, path: str) -> bool:
        rc, _ = self._exec(["test", "-e", path])
        return rc == 0

    def is_dir(self, path: str) -> bool:
        rc, _ = self._exec(["test", "-d", path])
        return rc == 0

    def is_executable(self, path: str) -> bool:
        rc, _ = self._exec(["test", "-x", path])
        return rc == 0

    def run(self, argv: Sequence[str], env: dict[str, str]) -> tuple[int, str]:
        assignments = [f"{key}={value}" for key, value in sorted(env.items())]
        return self._exec(["env", *assignments, *argv])

    def listening_sockets(self) -> str:
        rc, out = self._exec(["cat", "/proc/net/tcp", "/proc/net/tcp6"])
        if rc != 0:
            raise OSError(f"cannot read container socket tables: {out.strip()}")
        return out

    def socket_owners(self) -> dict[int, int]:
        # WI#511: a tenant container may share the host netns too (or run
        # beside host listeners the exec'd shell can see). Attribute listening
        # socket inodes to owning uids the same way the build profile does.
        owners: dict[int, int] = {}
        proc_rc, pids = self._exec(["sh", "-c", "ls -d /proc/[0-9]* 2>/dev/null"])
        if proc_rc != 0:
            return owners
        for entry in pids.split():
            pid = entry.rsplit("/", 1)[-1]
            status_rc, status = self._exec(["cat", f"/proc/{pid}/status"])
            uid = -1
            if status_rc == 0:
                for line in status.splitlines():
                    if line.startswith("Uid:"):
                        fields = line.split()
                        if len(fields) >= 2:
                            try:
                                uid = int(fields[1])
                            except ValueError:
                                pass
                        break
            if uid < 0:
                continue
            fds_rc, fds = self._exec(["sh", "-c", f"ls /proc/{pid}/fd 2>/dev/null"])
            if fds_rc != 0:
                continue
            for fd in fds.split():
                rc, out = self._exec(["readlink", f"/proc/{pid}/fd/{fd}"])
                if rc != 0:
                    continue
                target = out.strip()
                if target.startswith("socket:["):
                    try:
                        owners[int(target[len("socket:[") : -1])] = uid
                    except ValueError:
                        continue
        return owners

    def home_dirs(self) -> Iterable[str]:
        return ("/root", "/home/opencode2")

    def environment(self) -> dict[str, str]:
        rc, out = self._exec(["env"])
        if rc != 0:
            raise OSError(f"cannot read container environment: {out.strip()}")
        return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)


def parse_listeners(
    table: str,
) -> list[tuple[str, int]] | list[tuple[str, int, int]]:
    """Return (address, port[, inode]) for every TCP LISTEN row.

    Format (kernel): ``sl local_address rem_address st ...`` where
    ``local_address`` is ``HEXADDR:HEXPORT``. IPv4 addresses are little-endian
    hex; IPv6 is four little-endian 32-bit words.

    The socket inode (field 10 of the kernel row) is included when present so
    the listeners check can attribute sockets to the uid of the process
    holding them (WI#511 — shared host netns during image builds).
    """
    listeners: list[tuple[str, int]] = []
    for line in table.splitlines():
        fields = line.split()
        if len(fields) < 4 or fields[0] == "sl":
            continue
        if fields[3] != _TCP_LISTEN:
            continue
        try:
            address_hex, port_hex = fields[1].split(":")
            port = int(port_hex, 16)
        except ValueError:
            continue
        # Kernel row layout: sl, local, rem, st, tx:rx, tr:tm->when,
        # retrnsmt, uid, timeout, INODE, ... The inode (index 12 in a data
        # row, since field 0 is the slot number) is what /proc/<pid>/fd
        # links name; report it so the listeners check can attribute
        # sockets to owning uids (WI#511).
        inode = 0
        if len(fields) >= 13:
            try:
                inode = int(fields[12])
            except ValueError:
                inode = 0
        listeners.append((_decode_address(address_hex), port, inode))
    return listeners


def _decode_address(address_hex: str) -> str:
    """Decode a /proc/net/tcp{,6} address field into a printable address."""
    try:
        raw = bytes.fromhex(address_hex)
    except ValueError:
        return address_hex
    if len(raw) == 4:
        return str(ipaddress.IPv4Address(bytes(reversed(raw))))
    if len(raw) == 16:
        # Four little-endian 32-bit words, in network order per word.
        host_order = b"".join(
            raw[index : index + 4][::-1] for index in range(0, 16, 4)
        )
        return str(ipaddress.IPv6Address(host_order))
    return address_hex


def is_loopback(address: str) -> bool:
    try:
        return ipaddress.ip_address(address).is_loopback
    except ValueError:
        return address in (LOOPBACK_V4, LOOPBACK_V6)


def service_config_path(env: dict[str, str]) -> str:
    """Upstream's resolution order, mirrored from entrypoint.sh."""
    config_dir = env.get("OPENCODE_CONFIG_DIR")
    if config_dir:
        return f"{config_dir}/service.json"
    xdg = env.get("XDG_CONFIG_HOME") or f"{env.get('HOME', '/root')}/.config"
    return f"{xdg}/opencode/service.json"


def opentunnel_dirs(env: dict[str, str], homes: Iterable[str]) -> list[str]:
    candidates: list[str] = []
    for base_key in ("XDG_DATA_HOME", "XDG_STATE_HOME"):
        base = env.get(base_key)
        if base:
            candidates.append(f"{base}/opentunnel")
    for home in homes:
        candidates.append(f"{home}/.local/share/opentunnel")
        candidates.append(f"{home}/.local/state/opentunnel")
        candidates.append(f"{home}/.opentunnel")
    seen: list[str] = []
    for candidate in candidates:
        if candidate not in seen:
            seen.append(candidate)
    return seen


def check_service_config(transport: Transport, env: dict[str, str]) -> CheckResult:
    path = service_config_path(env)
    try:
        raw = transport.read_text(path)
    except OSError as exc:
        return CheckResult("service_config", False, f"{path} is unreadable: {exc}")
    if raw is None:
        return CheckResult(
            "service_config",
            True,
            f"{path} absent (entrypoint writes {{disabled:true}} at boot)",
        )
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        return CheckResult("service_config", False, f"{path} is not JSON: {exc}")
    if not isinstance(parsed, dict):
        return CheckResult("service_config", False, f"{path} is not a JSON object")
    if parsed.get("remote"):
        return CheckResult(
            "service_config", False, f"{path} carries remote={parsed.get('remote')!r}"
        )
    if parsed.get("disabled") is True:
        return CheckResult("service_config", True, f"{path} is {{disabled:true}}")
    return CheckResult(
        "service_config",
        False,
        f"{path} does not disable service mode: {sorted(parsed)}",
    )


def check_opentunnel_identity(
    transport: Transport, env: dict[str, str]
) -> CheckResult:
    found = [
        path
        for path in opentunnel_dirs(env, transport.home_dirs())
        if transport.exists(path)
    ]
    if found:
        return CheckResult(
            "opentunnel_identity", False, f"opentunnel identity present: {found}"
        )
    return CheckResult(
        "opentunnel_identity", True, "no opentunnel identity directory"
    )


def check_listeners(
    transport: Transport,
    health_port: int,
    ws_port: int | None = None,
    smoke_uid: int | None = None,
) -> CheckResult:
    try:
        listeners = parse_listeners(transport.listening_sockets())
    except OSError as exc:
        return CheckResult("listeners", False, f"socket tables are unreadable: {exc}")
    allowed_ports = {health_port}
    if ws_port is not None:
        allowed_ports.add(ws_port)

    # WI#511: during image builds (and any shared-netns container) the socket
    # table is the HOST's, not the worker's. When the transport can attribute
    # sockets to owning uids, judge only sockets owned by the smoke/runtime
    # user; host listeners are not the worker's exposure surface. Without
    # owner data (or with owner data but an unknown uid), fall back to
    # judging every row — the guard stays fail-closed.
    owners = transport.socket_owners()
    if owners and smoke_uid is not None:
        # WI#511 attribution: sockets owned by OTHER uids are host/peer
        # processes, not the worker's exposure surface — ignore them.
        # Sockets with NO owner entry are unknown provenance: keep judging
        # them (fail-closed; an unreadable fd tree must not open a hole).
        owned = [
            row
            for row in listeners
            if not row[-1] or owners.get(row[-1], smoke_uid) == smoke_uid
        ]
        judged = f"{len(owned)} of {len(listeners)} listener(s) owned by or unattributed to uid {smoke_uid}"
    else:
        owned = list(listeners)
        judged = f"{len(listeners)} listener(s); no uid attribution available"
    offending = [
        f"{address}:{port}"
        for address, port, *_rest in owned
        if not is_loopback(address) and port not in allowed_ports
    ]
    if offending:
        return CheckResult(
            "listeners",
            False,
            f"({judged}) listener beyond loopback (allowed ports {sorted(allowed_ports)}): {offending}",
        )
    return CheckResult(
        "listeners",
        True,
        f"{judged}; only health/WS ports {sorted(allowed_ports)} may be non-loopback",
    )


def check_share_disabled(transport: Transport, config_path: str) -> CheckResult:
    try:
        raw = transport.read_text(config_path)
    except OSError as exc:
        return CheckResult("share_disabled", False, f"{config_path} is unreadable: {exc}")
    if raw is None:
        return CheckResult("share_disabled", False, f"{config_path} is absent")
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        return CheckResult("share_disabled", False, f"{config_path} is not JSON: {exc}")
    if not isinstance(parsed, dict):
        return CheckResult("share_disabled", False, f"{config_path} is not an object")
    share = parsed.get("share")
    if share != "disabled":
        return CheckResult(
            "share_disabled", False, f"{config_path} share={share!r}, expected 'disabled'"
        )
    return CheckResult("share_disabled", True, "share resolves to disabled")


REMOTE_REFUSAL_MARKER = "opencode-exposure-guard: refused 'opencode service'"


def check_remote_refused(transport: Transport, wrapper: str) -> CheckResult:
    """Require the installed wrapper's specific exit-77 exposure refusal."""
    if not transport.is_executable(wrapper):
        return CheckResult(
            "remote_refused", False, f"{wrapper} is missing or not executable"
        )
    sandbox = tempfile.mkdtemp(prefix="opencode2-runtime-smoke-")
    try:
        env = {
            "PATH": f"{os.path.dirname(wrapper)}:/usr/local/bin:/usr/bin:/bin",
            "HOME": sandbox,
            "XDG_CONFIG_HOME": f"{sandbox}/.config",
            "XDG_DATA_HOME": f"{sandbox}/.local/share",
            "XDG_STATE_HOME": f"{sandbox}/.local/state",
            "XDG_CACHE_HOME": f"{sandbox}/.cache",
            "TMPDIR": sandbox,
        }
        rc, output = transport.run([wrapper, "service", "set", "remote", "true"], env)
        if rc != 77 or REMOTE_REFUSAL_MARKER not in output:
            return CheckResult(
                "remote_refused",
                False,
                f"{wrapper} did not produce the guard's exit-77 service refusal "
                f"(rc={rc}): {output.strip()}",
            )
        planted = f"{sandbox}/.config/opencode/service.json"
        try:
            planted_raw = transport.read_text(planted)
        except OSError as exc:
            return CheckResult(
                "remote_refused", False, f"cannot verify {planted}: {exc}"
            )
        if planted_raw is not None:
            try:
                parsed = json.loads(planted_raw)
            except json.JSONDecodeError:
                return CheckResult(
                    "remote_refused",
                    False,
                    f"guard refusal left invalid JSON at {planted}",
                )
            if not isinstance(parsed, dict) or parsed.get("remote"):
                return CheckResult(
                    "remote_refused",
                    False,
                    f"guard refusal wrote remote=true to {planted}",
                )
        return CheckResult("remote_refused", True, "guard refusal rc=77 verified")
    finally:
        shutil.rmtree(sandbox, ignore_errors=True)


def run_checks(
    transport: Transport,
    health_port: int,
    config_path: str,
    wrapper: str,
    env: dict[str, str],
    smoke_uid: int | None = None,
) -> list[CheckResult]:
    try:
        ws_port = int(env.get("WS_PORT", "0"))
    except ValueError:
        ws_port = 0
    return [
        check_service_config(transport, env),
        check_opentunnel_identity(transport, env),
        check_listeners(transport, health_port, ws_port or None, smoke_uid),
        check_share_disabled(transport, config_path),
        check_remote_refused(transport, wrapper),
    ]


def build_env(transport: Transport, health_port: int) -> dict[str, str]:
    env = transport.environment()
    env.setdefault("HOME", "/root")
    env["HEALTH_PORT"] = str(health_port)
    return env


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="opencode2 worker runtime exposure smoke suite (WI#424 control 6)"
    )
    parser.add_argument(
        "--profile",
        choices=("build", "container", "remote"),
        default="build",
        help=(
            "build: inspect this image; container: run inside the worker "
            "container (podman exec); remote: drive a container from the host"
        ),
    )
    parser.add_argument(
        "--root",
        default="/",
        help="build profile only: image root to inspect (default /)",
    )
    parser.add_argument(
        "--container",
        default="",
        help="remote profile: worker container to inspect",
    )
    parser.add_argument(
        "--runtime",
        default=os.environ.get("CONTAINER_RT", "podman"),
        help="container profile: container runtime binary (default podman)",
    )
    parser.add_argument(
        "--config",
        default=DEFAULT_CONFIG,
        help=f"lane config to read share from (default {DEFAULT_CONFIG})",
    )
    parser.add_argument(
        "--wrapper",
        default=LANE_WRAPPER,
        help=f"installed opencode wrapper (default {LANE_WRAPPER})",
    )
    parser.add_argument(
        "--health-port",
        type=int,
        default=DEFAULT_HEALTH_PORT,
        help=f"in-container health port excepted from the listener rule (default {DEFAULT_HEALTH_PORT})",
    )
    parser.add_argument("--json", action="store_true", help="emit machine-readable output")
    parser.add_argument(
        "--smoke-uid",
        type=int,
        default=None,
        help=(
            "runtime-user uid for listener attribution (WI#511); defaults to "
            "the SMOKE_UID environment variable when set"
        ),
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.profile == "remote":
        if not args.container:
            print(
                "runtime-smoke: --container is required for --profile remote",
                file=sys.stderr,
            )
            return 2
        runtime = shutil.which(args.runtime)
        if runtime is None:
            print(f"runtime-smoke: runtime not found: {args.runtime}", file=sys.stderr)
            return 2
        transport: Transport = ContainerTransport(runtime, args.container)
    else:
        transport = LocalTransport(args.root)

    try:
        env = build_env(transport, args.health_port)
    except OSError as exc:
        print(f"runtime-smoke: {exc}", file=sys.stderr)
        return 2
    smoke_uid = args.smoke_uid
    if smoke_uid is None and os.environ.get("SMOKE_UID"):
        try:
            smoke_uid = int(os.environ["SMOKE_UID"])
        except ValueError:
            smoke_uid = None
    results = run_checks(
        transport, args.health_port, args.config, args.wrapper, env, smoke_uid
    )
    failures = [result for result in results if not result.ok]
    if args.json:
        print(
            json.dumps(
                {
                    "profile": args.profile,
                    "results": [result.__dict__ for result in results],
                    "failed": len(failures),
                },
                indent=2,
            )
        )
    else:
        for result in results:
            status = "ok" if result.ok else "FAIL"
            print(f"{status}: {result.name}: {result.detail}")
        print(f"runtime-smoke: {len(results) - len(failures)}/{len(results)} checks passed")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
