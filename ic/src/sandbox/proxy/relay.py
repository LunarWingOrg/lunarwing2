"""In-container relay for proxy-backed modes; launched with Python isolated mode.

The enforced container has no external network interface. Legacy Podman keeps
its normal advisory network. Both forward proxy traffic only to the mounted
Unix socket, whose host endpoint reaches the validating HTTP proxy. The command
is a separate argv value, never interpolated into this program.

WI#641: the host maps stderr containing "lunarwing-proxy-relay:" to a
structured proxy error and discards the command's own exit status, so only the
startup failure may use that exact prefix. A bare errno cannot distinguish a
missing mount from a directory the container's UID cannot traverse from an
SELinux denial, so the startup failure carries the mount's nodes, the
container's IDs, and the container's MAC state.
"""

import errno
import os
import select
import socket
import socketserver
import subprocess
import sys
import threading
import time

SOCKET = "/run/lunarwing-proxy/proxy.sock"
MOUNT = "/run/lunarwing-proxy"
# A just-started container can race the host-side relabel of the mount; a
# short retry turns that into a success instead of a dead execution.
START_ATTEMPTS = 3
START_RETRY_SECONDS = 0.2
# Client disconnects are routine; only genuine transport failures are logged.
BENIGN = frozenset((errno.EPIPE, errno.ECONNRESET, errno.ENOTCONN, errno.EBADF))


def _node(path):
    """Mode and ownership of one mount node, or the lstat error."""
    try:
        info = os.lstat(path)
    except OSError as error:
        return f"{path}: {error.strerror}"
    return f"{path}: mode={oct(info.st_mode & 0o7777)} uid={info.st_uid} gid={info.st_gid}"


def _security_state():
    """Facts that separate a DAC refusal from a MAC denial."""
    try:
        with open("/sys/fs/selinux/enforce", encoding="ascii") as handle:
            enforce = handle.read().strip()
    except OSError:
        return "selinux=not-mounted"
    try:
        with open("/proc/self/attr/current", encoding="ascii") as handle:
            label = handle.read().strip()
    except OSError:
        label = "unknown"
    return f"selinux=enforce:{enforce} container-label={label}"


def _probe():
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
        probe.settimeout(5)
        probe.connect(SOCKET)


def _startup_error(error):
    return (
        f"lunarwing-proxy-relay: {error}; euid={os.geteuid()} egid={os.getegid()} "
        f"{_node(MOUNT)} {_node(SOCKET)} {_security_state()}"
    )


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as upstream:
            upstream.settimeout(30)
            self.request.settimeout(30)
            try:
                upstream.connect(SOCKET)
            except OSError as error:
                self._report(error)
                return
            peers = {self.request: upstream, upstream: self.request}
            try:
                while peers:
                    ready, _, _ = select.select(list(peers), [], [], 30)
                    if not ready:
                        break
                    for source in ready:
                        data = source.recv(65536)
                        if data:
                            peers[source].sendall(data)
                        else:
                            peers.pop(source).shutdown(socket.SHUT_WR)
            except OSError as error:
                self._report(error)

    def _report(self, error):
        if error.errno in BENIGN:
            return
        # Never the startup prefix: the host would report a proxy failure and
        # hide the command's real exit status.
        print(f"lunarwing-proxy-relay transport: {error}", file=sys.stderr)


class Relay(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    slots = threading.BoundedSemaphore(32)

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()


def _connect_probe():
    """Probe the transport, retrying the mount/relabel race at container start."""
    error = None
    for attempt in range(START_ATTEMPTS):
        try:
            _probe()
            return None
        except OSError as current:
            error = current
            if attempt + 1 < START_ATTEMPTS:
                time.sleep(START_RETRY_SECONDS)
    return error


def main():
    # Establish transport before starting the untrusted command. A missing
    # socket, wrong image, or denied mount prevents the command from running.
    error = _connect_probe()
    if error is not None:
        print(_startup_error(error), file=sys.stderr)
        return 125
    try:
        relay = Relay(("127.0.0.1", 3128), Handler)
    except OSError as error:
        print(f"lunarwing-proxy-relay: {error}", file=sys.stderr)
        return 125
    with relay:
        threading.Thread(target=relay.serve_forever, daemon=True).start()
        result = subprocess.run(["/bin/sh", "-c", sys.argv[1]], check=False)
        return result.returncode if result.returncode >= 0 else 128 - result.returncode


if __name__ == "__main__":
    sys.exit(main())
