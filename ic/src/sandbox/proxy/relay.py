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

The container's own label is the *source* context of an SELinux
`unix_stream_socket connectto` denial, and container runtimes normally do not
mount selinuxfs. It is therefore read from /proc unconditionally, never gated
on /sys/fs/selinux being visible.
"""

import errno
import os
import select
import socket
import socketserver
import subprocess
import sys
import threading

SOCKET = "/run/lunarwing-proxy/proxy.sock"
MOUNT = "/run/lunarwing-proxy"
# Client disconnects are routine; only genuine transport failures are logged.
BENIGN = frozenset((errno.EPIPE, errno.ECONNRESET, errno.ENOTCONN, errno.EBADF))


def _node(path):
    """Mode, ownership and SELinux type of one mount node, or the lstat error."""
    try:
        info = os.lstat(path)
    except OSError as error:
        return f"{path}: {error.strerror}"
    node = f"{path}: mode={oct(info.st_mode & 0o7777)} uid={info.st_uid} gid={info.st_gid}"
    try:
        label = os.getxattr(path, b"security.selinux").decode("ascii", "replace").rstrip("\0")
    except OSError:
        return node
    return f"{node} selinux={label}"


def _self_label():
    """The container's own MAC label, readable with or without selinuxfs.

    This is the source context of an SELinux `connectto` denial. The
    container runtime normally does not mount /sys/fs/selinux, so the label
    must not be gated on the enforce node being visible.
    """
    for path in ("/proc/thread-self/attr/current", "/proc/self/attr/current"):
        try:
            with open(path, encoding="ascii") as handle:
                return handle.read().strip() or "unknown"
        except OSError:
            continue
    return "unreadable"


def _security_state():
    """Facts that separate a DAC refusal from a MAC denial."""
    try:
        with open("/sys/fs/selinux/enforce", encoding="ascii") as handle:
            enforce = f"enforce:{handle.read().strip()}"
    except OSError:
        enforce = "not-mounted"
    return f"selinux={enforce} self-label={_self_label()}"


def _probe():
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
        probe.settimeout(5)
        probe.connect(SOCKET)


def _startup_error(error):
    # EACCES on this connect has three possible causes, and the errno cannot
    # tell them apart: the SELinux `unix_stream_socket connectto` check against
    # the *listening socket's* own context (not the mount's :z inode label),
    # the SELinux `sock_file` check on the socket inode, or a channel
    # directory this UID cannot traverse. Name all three, so the host-side
    # operator reads the audit log before changing permissions or labels.
    hint = ""
    if error.errno == errno.EACCES:
        hint = (
            " (EACCES has three possible causes: SELinux `unix_stream_socket"
            " connectto` against the listener's own context, SELinux"
            " `sock_file` access to the socket inode, or a channel directory"
            " this uid cannot traverse; collect the denial with"
            " ic/scripts/lunarwing-sandbox-proxy-diag.sh on the host)"
        )
    return (
        f"lunarwing-proxy-relay: {error}; euid={os.geteuid()} egid={os.getegid()} "
        f"{_node(MOUNT)} {_node(SOCKET)} {_security_state()}{hint}"
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


def main():
    # Establish transport before starting the untrusted command. A missing
    # socket, wrong image, or denied mount prevents the command from running.
    try:
        _probe()
    except OSError as error:
        print(_startup_error(error), file=sys.stderr)
        return 125
    try:
        relay = Relay(("127.0.0.1", 3128), Handler)
    except OSError as error:
        print(f"lunarwing-proxy-relay: {error}", file=sys.stderr)
        return 125
    with relay:
        threading.Thread(target=relay.serve_forever, daemon=True).start()
        result = subprocess.run(["sh", "-c", sys.argv[1]], check=False)
        return result.returncode if result.returncode >= 0 else 128 - result.returncode


if __name__ == "__main__":
    sys.exit(main())
