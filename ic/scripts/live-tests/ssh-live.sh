#!/usr/bin/env bash
set -euo pipefail
umask 077

IC_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR=""
SSHD_PIDS=()

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  local pid
  for pid in "${SSHD_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  for pid in "${SSHD_PIDS[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
  if [[ -n "$WORK_DIR" && "${LUNARWING_SSH_LIVE_KEEP:-0}" == 1 ]]; then
    printf 'Retained disposable artifacts at %s\n' "$WORK_DIR" >&2
  elif [[ -n "$WORK_DIR" ]]; then
    rm -rf -- "$WORK_DIR"
  fi
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

[[ "${EUID:-$(id -u)}" -ne 0 ]] || fail "run as an unprivileged user"
for tool in cargo git python3 ssh ssh-add ssh-keygen sshd taskset; do
  command -v "$tool" >/dev/null 2>&1 || fail "missing required tool: $tool"
done

SSH_USER="$(id -un)"
[[ "$SSH_USER" =~ ^[a-zA-Z0-9._-]+$ ]] || fail "unsupported SSH username"
SSHD="$(command -v sshd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lunarwing-ssh-live.XXXXXX")"

read -r SSH_PORT CERT_PORT < <(python3 - <<'PY'
import socket

sockets = []
for _ in range(2):
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    sockets.append(sock)
print(*(sock.getsockname()[1] for sock in sockets))
for sock in sockets:
    sock.close()
PY
)
[[ "$SSH_PORT" =~ ^[0-9]+$ && "$CERT_PORT" =~ ^[0-9]+$ ]] \
  || fail "could not allocate loopback ports"
[[ "$SSH_PORT" != "$CERT_PORT" ]] || fail "listener ports must differ"

ssh-keygen -q -t ed25519 -N '' -C lunarwing-live-client -f "$WORK_DIR/client_key"
ssh-keygen -q -t ed25519 -N '' -C lunarwing-live-host -f "$WORK_DIR/host_key"
ssh-keygen -q -t ed25519 -N '' -C lunarwing-live-wrong -f "$WORK_DIR/wrong_host_key"
ssh-keygen -q -t ed25519 -N '' -C lunarwing-live-ca -f "$WORK_DIR/ca_key"
ssh-keygen -q -s "$WORK_DIR/ca_key" -I lunarwing-live -h \
  -n 127.0.0.1,localhost -V -5m:+1h "$WORK_DIR/host_key.pub"
cp -- "$WORK_DIR/client_key.pub" "$WORK_DIR/authorized_keys"
chmod 0600 "$WORK_DIR/authorized_keys"

write_sshd_config() {
  local config="$1" port="$2" pid_file="$3" certificate_only="$4"
  {
    printf 'Port %s\n' "$port"
    printf 'ListenAddress 127.0.0.1\n'
    printf 'HostKey %s/host_key\n' "$WORK_DIR"
    if [[ "$certificate_only" == yes ]]; then
      printf 'HostCertificate %s/host_key-cert.pub\n' "$WORK_DIR"
      printf 'HostKeyAlgorithms ssh-ed25519-cert-v01@openssh.com\n'
    fi
    printf 'PidFile %s\n' "$pid_file"
    printf 'AuthorizedKeysFile %s/authorized_keys\n' "$WORK_DIR"
    printf 'AuthorizedKeysCommand none\n'
    printf 'StrictModes no\n'
    printf 'PubkeyAuthentication yes\n'
    printf 'AuthenticationMethods publickey\n'
    printf 'PasswordAuthentication no\n'
    printf 'KbdInteractiveAuthentication no\n'
    printf 'ChallengeResponseAuthentication no\n'
    printf 'UsePAM no\n'
    printf 'PermitRootLogin no\n'
    printf 'AllowUsers %s\n' "$SSH_USER"
    printf 'AllowTcpForwarding no\n'
    printf 'AllowAgentForwarding no\n'
    printf 'X11Forwarding no\n'
    printf 'PermitTunnel no\n'
    printf 'PermitTTY no\n'
    printf 'PermitUserEnvironment no\n'
    printf 'PermitUserRC no\n'
    printf 'PrintMotd no\n'
    printf 'PrintLastLog no\n'
    printf 'UseDNS no\n'
    printf 'SetEnv HOME=%s/home BASH_ENV=/dev/null ENV=/dev/null\n' "$WORK_DIR"
    printf 'LogLevel VERBOSE\n'
  } >"$config"
}

wait_for_port() {
  local port="$1" pid="$2" log="$3"
  if ! python3 - "$port" "$pid" <<'PY'
import os
import socket
import sys
import time

port = int(sys.argv[1])
pid = int(sys.argv[2])
for _ in range(100):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        break
    try:
        connection = socket.create_connection(("127.0.0.1", port), timeout=0.1)
    except OSError:
        time.sleep(0.05)
    else:
        connection.close()
        sys.exit(0)
sys.exit(1)
PY
  then
    tail -n 30 "$log" >&2 || true
    fail "sshd did not start on loopback port $port"
  fi
}

mkdir -p -- "$WORK_DIR/home"
write_sshd_config "$WORK_DIR/plain-sshd_config" "$SSH_PORT" "$WORK_DIR/plain.pid" no
write_sshd_config "$WORK_DIR/cert-sshd_config" "$CERT_PORT" "$WORK_DIR/cert.pid" yes
"$SSHD" -t -f "$WORK_DIR/plain-sshd_config"
"$SSHD" -t -f "$WORK_DIR/cert-sshd_config"

"$SSHD" -D -e -f "$WORK_DIR/plain-sshd_config" >"$WORK_DIR/plain-sshd.log" 2>&1 &
SSHD_PIDS+=("$!")
wait_for_port "$SSH_PORT" "${SSHD_PIDS[0]}" "$WORK_DIR/plain-sshd.log"
"$SSHD" -D -e -f "$WORK_DIR/cert-sshd_config" >"$WORK_DIR/cert-sshd.log" 2>&1 &
SSHD_PIDS+=("$!")
wait_for_port "$CERT_PORT" "${SSHD_PIDS[1]}" "$WORK_DIR/cert-sshd.log"

git init -q --bare "$WORK_DIR/repo.git"
git init -q "$WORK_DIR/seed"
git -C "$WORK_DIR/seed" config user.name 'LunarWing SSH live test'
git -C "$WORK_DIR/seed" config user.email 'ssh-live@example.invalid'
git -C "$WORK_DIR/seed" checkout -q -b main
printf 'disposable SSH live test repository\n' >"$WORK_DIR/seed/README.md"
git -C "$WORK_DIR/seed" add README.md
git -C "$WORK_DIR/seed" commit -q -m 'initial live test commit'
git -C "$WORK_DIR/seed" remote add origin "$WORK_DIR/repo.git"
git -C "$WORK_DIR/seed" push -q origin main
git --git-dir="$WORK_DIR/repo.git" symbolic-ref HEAD refs/heads/main

python3 - "$WORK_DIR/config.json" "$WORK_DIR" "$SSH_USER" "$SSH_PORT" "$CERT_PORT" <<'PY'
import json
from pathlib import Path
import sys

config_path, work_dir, user, port, certificate_port = sys.argv[1:]
work = Path(work_dir)

def public_key(name):
    parts = (work / name).read_text().split()
    return " ".join(parts[:2])

config = {
    "user": user,
    "port": int(port),
    "certificate_port": int(certificate_port),
    "host_key": public_key("host_key.pub"),
    "wrong_host_key": public_key("wrong_host_key.pub"),
    "client_key": str(work / "client_key"),
    "bare_repo": str(work / "repo.git"),
    "seed_repo": str(work / "seed"),
    "work_dir": str(work),
}
Path(config_path).write_text(json.dumps(config))
PY
chmod 0600 "$WORK_DIR/config.json"

cd "$IC_DIR"
printf 'Running live SSH handshake test on 127.0.0.1 (plain and certificate-only listeners)\n'
LUNARWING_SSH_LIVE_CONFIG="$WORK_DIR/config.json" \
  taskset -c 0-17 cargo test -j18 --lib \
  bridge::ssh::ssh_live_tests::live_ssh_tool_end_to_end \
  -- --exact --ignored --nocapture
printf 'PASS: ssh command, host-key refusals, agent signing, and ssh_git clone/fetch\n'
