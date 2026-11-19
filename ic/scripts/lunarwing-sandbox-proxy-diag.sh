#!/usr/bin/env bash
# Host diagnosis for the sandbox proxy relay (WI#641).
#
# Collects SELinux mode, the tenant's sandbox network mode and daemon context,
# relay mount metadata, and candidate AVC denials. Does not change policy,
# start services, or print arbitrary tenant environment values. Its only write
# is one temporary file (mktemp) that holds the matched AVC lines for
# audit2why; it is removed on exit.
#
# Run as root on the tenant host (ausearch needs the audit log) while the
# sandbox is idle, then trigger one failing sandbox shell call and run again
# with --watch to catch the denial.
set -euo pipefail
export LC_ALL=C

usage() {
  cat <<'EOF'
Usage:
  sudo ic/scripts/lunarwing-sandbox-proxy-diag.sh <tenant> [--watch]

Environment:
  SANDBOX_NETWORK    overrides the diagnostic reading of SANDBOX_NETWORK
                     from /home/<tenant>/lunarwing/env/lunarwing.env

Options:
  --watch            after the snapshot, stream SELinux denials for 60s so a
                     failing sandbox shell call can be caught in the act

Exit:
  0  snapshot printed (a denied connect is reported, not treated as an error)
  1  invalid arguments or unknown tenant account
EOF
}

say() { printf '%s\n' "$*"; }

tenant_network() (
  # Reuse mt-admin's canonical path and per-key reader. Sourcing it in a
  # subshell loads the helpers without running its dispatcher. The reader
  # opens the file as the tenant, never sources it, and refuses unsafe paths.
  # shellcheck source=ic/scripts/lunarwing-mt-admin.sh
  source "$(dirname -- "${BASH_SOURCE[0]}")/lunarwing-mt-admin.sh" || exit 1
  read_tenant_env_value "$1" "$(tenant_env_dir "$1")/lunarwing.env" SANDBOX_NETWORK
)

report_network() {
  local network="${SANDBOX_NETWORK:-}"
  if [[ -z "$network" ]] && ! network="$(tenant_network "$1")"; then
    say "SANDBOX_NETWORK=unavailable (could not safely read tenant env)"
    return
  fi
  # Only print known modes, not arbitrary values from a tenant-writable file.
  case "${network,,}" in
    "")
      say "SANDBOX_NETWORK=unset (Linux default is proxy-enforced unless settings override it)"
      ;;
    proxy-enforced | proxy_enforced)
      say "SANDBOX_NETWORK=proxy-enforced (Unix relay; --network none)"
      ;;
    proxy | allowlist)
      say "SANDBOX_NETWORK=proxy (legacy; Unix relay on Podman only)"
      ;;
    none | off)
      say "SANDBOX_NETWORK=none (no relay)"
      ;;
    tunnel)
      say "SANDBOX_NETWORK=tunnel (no proxy relay)"
      ;;
    *)
      say "SANDBOX_NETWORK=unrecognized (value omitted)"
      ;;
  esac
}

relay_denials() {
  # An audit event can also include syscall, PATH, and command line records.
  # Keep only AVC lines. connectto may not have a path, so those are host-wide
  # candidates and must be correlated by time and contexts, not assumed ours.
  grep --line-buffered -E '^(node=[^ ]+ )?type=(AVC|USER_AVC) .*avc:[[:space:]]+denied' \
    | grep --line-buffered -E 'lw-proxy-|proxy\.sock|\{[^}]*\bconnectto\b[^}]*\}.*\btclass=unix_stream_socket\b'
}

interpret_denials() {
  # The denied permission alone does not say what to change: a missing allow
  # rule and a policy constraint (`mlsconstrain`, e.g. a level comparison) are
  # refused identically but need different fixes. audit2why names which one.
  if [[ ! -s "$1" ]]; then
    say "no matching AVC returned (log unavailable or no match); this does not determine the cause."
    return
  fi
  if command -v audit2why >/dev/null 2>&1; then
    say "audit2why:"
    audit2why <"$1" 2>&1 | sed 's/^/  /' || say "  (audit2why could not interpret the record)"
  else
    say "audit2why not installed; read the denied permission and the two contexts above."
  fi
}

tenant=""
watch=false
for arg in "$@"; do
  case "$arg" in
    -h | --help)
      usage
      exit 0
      ;;
    --watch) watch=true ;;
    -*)
      say "error: unknown option" >&2
      exit 1
      ;;
    *)
      if [[ -n "$tenant" ]]; then
        say "error: one tenant at a time" >&2
        exit 1
      fi
      tenant="$arg"
      ;;
  esac
done

if [[ ! "$tenant" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]]; then
  say "error: expected a lowercase mt-admin tenant name (letters, digits, internal hyphens)" >&2
  exit 1
fi

if ! tenant_uid="$(id -u -- "$tenant" 2>/dev/null)" || [[ ! "$tenant_uid" =~ ^[1-9][0-9]*$ ]]; then
  say "error: no non-root account for tenant '$tenant'" >&2
  exit 1
fi

say "== 1. SELinux mode"
if mode="$(getenforce 2>/dev/null)"; then
  say "getenforce: $mode"
  if [[ "$mode" == "Permissive" || "$mode" == "Disabled" ]]; then
    say "  -> SELinux is not enforcing; check DAC and other security controls."
  fi
elif [[ -r /sys/fs/selinux/enforce ]]; then
  say "selinuxfs enforce=$(cat /sys/fs/selinux/enforce)"
else
  say "SELinux mode unavailable; check AppArmor separately (aa-status)."
fi

say ""
say "== 2. Configured sandbox network mode (not the running process environment)"
report_network "$tenant"

say ""
say "== 3. Tenant daemon SELinux context (PID, context, command name)"
if daemons="$(ps -u "$tenant_uid" -o pid=,label=,comm= 2>/dev/null | awk '$NF == "lunarwing"')" \
  && [[ -n "$daemons" ]]; then
  say "$daemons"
else
  say "no lunarwing process context available for uid $tenant_uid."
fi

say ""
runtime_dir="/run/user/$tenant_uid"
say "== 4. Relay sockets under $runtime_dir"
found=false
for dir in "$runtime_dir"/lw-proxy-*; do
  [[ ! -L "$dir" && -d "$dir" ]] || continue
  found=true
  ls -ldZ -- "$dir" 2>/dev/null || true
  channel="$dir/channel"
  socket="$channel/proxy.sock"
  if [[ ! -L "$channel" && -d "$channel" ]]; then
    ls -ldZ -- "$channel" 2>/dev/null || true
    if [[ ! -L "$socket" && -S "$socket" ]]; then
      ls -ldZ -- "$socket" 2>/dev/null || true
      continue
    fi
  fi
  say "  -> no relay socket visible in this channel."
done
if [[ "$found" == false ]]; then
  say "none visible in the standard runtime dir (the relay may be idle or using a different temp dir)."
fi
say "  -> inode labels do not establish permission to connect to the listening socket."

say ""
say "== 5. Recent candidate SELinux denials (match time and contexts to the failing call)"
denials="$(mktemp)"
trap 'rm -f -- "$denials"' EXIT
if command -v ausearch >/dev/null 2>&1; then
  ausearch -m AVC,USER_AVC -ts recent --raw 2>/dev/null | relay_denials >"$denials" || true
  cat "$denials" 2>/dev/null || true
  interpret_denials "$denials"
else
  say "ausearch not installed; skipped."
fi

if [[ "$watch" == true ]]; then
  say ""
  say "== 6. Watching for candidate denials for 60s; trigger one failing sandbox shell call now"
  if [[ ! -r /var/log/audit/audit.log ]]; then
    say "cannot read /var/log/audit/audit.log; run as root to watch."
  else
    : >"$denials"
    timeout 60 tail -n 0 -F /var/log/audit/audit.log 2>/dev/null | relay_denials | tee "$denials" || true
    say ""
    interpret_denials "$denials"
    say "watch finished."
  fi
fi
