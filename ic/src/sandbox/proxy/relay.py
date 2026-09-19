"""In-container relay for proxy-enforced; launched with Python isolated mode.

This process has no external network interface and forwards only to the mounted
Unix socket. The host endpoint always forwards to the validating HTTP proxy.
The command is a separate argv value, never interpolated into this program.
"""

import select
import socket
import socketserver
import subprocess
import sys
import threading

SOCKET = "/run/lunarwing-proxy/proxy.sock"


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as upstream:
            upstream.settimeout(30)
            self.request.settimeout(30)
            try:
                upstream.connect(SOCKET)
                peers = {self.request: upstream, upstream: self.request}
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
            except OSError:
                pass


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
    try:
        # Establish transport before starting the untrusted command. A missing
        # socket, wrong image, or denied mount never degrades to advisory mode.
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
            probe.settimeout(5)
            probe.connect(SOCKET)
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
