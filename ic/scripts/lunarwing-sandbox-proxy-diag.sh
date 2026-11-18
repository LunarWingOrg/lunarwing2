#!/usr/bin/env bash
# Read-only host diagnosis for the sandbox proxy relay (WI#641).
#
# Reports only the facts that distinguish the failure modes of
# `lunarwing-proxy-relay: [Errno 13] Permission denied` inside the sandbox:
# SELinux mode, whether the tenant's sandbox network mode uses the relay at
# all, the relay socket's DAC mode/owner and SELinux type, and any recent
# denial that names the socket. It changes nothing and prints no tenant
# environment beyond the two values named below.
#
# Run as root on the tenant host (ausearch needs the audit log) while the
# sandbox is idle, then trigger one failing sandbox shell call and run again
# with --watch to catch the denial.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/lunarwing-sandbox-proxy-diag.sh <tenant>
  scripts/lunarwing-sandbox-proxy-diag.sh <tenant> --watch

Environment:
  LUNARWING_ETC      default: /etc/lunarwing
  SANDBOX_NETWORK    overrides the value read from the tenant env

Options:
  --watch            after the snapshot, stream SELinux denials for 60s so a
                     failing sandbox shell call can be caught in the act

Exit:
  0  snapshot printed (a denied connect is reported, not treated as an error)
  1  the tenant or its env file is missing
EOF
}

say() { printf '%s\n' "$*"; }

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
      say "error: unknown option $arg" >&2
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

if [[ -z "$tenant" ]]; then
  usage >&2
  exit 1
fi

ETC="${LUNARWING_ETC:-/etc/lunarwing}"
env_file="$ETC/tenants/$tenant/lunarwing.env"
if [[ ! -r "$env_file" ]]; then
  say "error: cannot read $env_file" >&2
  exit 1
fi

say "== 1. SELinux mode"
if command -v getenforce >/dev/null 2>&1; then
  mode="$(getenforce)"
  say "getenforce: $mode"
  if [[ "$mode" == "Permissive" || "$mode" == "Disabled" ]]; then
    say "  -> SELinux attribution is ruled out for a connect() EACCES."
  fi
elif [[ -r /sys/fs/selinux/enforce ]]; then
  say "selinuxfs enforce=$(cat /sys/fs/selinux/enforce)"
else
  say "SELinux not mounted; check AppArmor separately (aa-status)."
fi

say ""
say "== 2. Sandbox network mode (decides whether the relay is in the path)"
network="${SANDBOX_NETWORK:-}"
if [[ -z "$network" ]]; then
  while IFS='=' read -r key value; do
    [[ "$key" == "SANDBOX_NETWORK" ]] && network="$value"
  done <"$env_file"
fi
network="$(printf '%s' "$network" | tr -d "\"'")"
network="${network:-unset}"
say "SANDBOX_NETWORK=${network:-unset}"
case "$network" in
  unset | "" | proxy-enforced | proxy)
    say "  -> the per-execution Unix relay is used; the socket below matters."
    ;;
  none)
    say "  -> no network and no relay; a relay error cannot come from this tenant."
    ;;
  tunnel)
    say "  -> tunnel mode; relay errors are not expected here."
    ;;
  *)
    say "  -> unrecognised value; check SANDBOX_NETWORK parsing."
    ;;
esac

say ""
say "== 3. Relay sockets currently on disk"
runtime_dirs=()
for dir in /run/user/*/lw-proxy-*; do
  [[ -d "$dir" ]] && runtime_dirs+=("$dir")
done
if [[ ${#runtime_dirs[@]} -eq 0 ]]; then
  say "no /run/user/*/lw-proxy-* directory: no execution is in flight right now."
else
  for dir in "${runtime_dirs[@]}"; do
    channel="$dir/channel"
    socket="$channel/proxy.sock"
    ls -ldZ "$dir" "$channel" 2>/dev/null || true
    if [[ -e "$socket" ]]; then
      ls -lZ "$socket"
      say "  -> the channel must be mode 0711 and typed container_file_t for"
      say "     the container's uid to traverse it and connect; a type of"
      say "     tmp_t/user_tmp_t is the WI#641 denial."
    else
      say "  -> channel has no proxy.sock (relay already exited)."
    fi
  done
fi

say ""
say "== 4. Recent denials naming the socket"
if command -v ausearch >/dev/null 2>&1; then
  found=false
  while IFS= read -r line; do
    found=true
    say "$line"
  done < <(ausearch -m AVC,USER_AVC -ts recent 2>/dev/null \
    | grep -E 'connectto|lw-proxy-|proxy\.sock' || true)
  if [[ "$found" == false ]]; then
    say "none in the retained audit window."
    say "If a denial is expected but nothing appears, dontaudit rules may hide"
    say "it: run 'semodule -DB', repeat the failing call, then 'semodule -B'."
  fi
else
  say "ausearch not installed (policycoreutils); skipped."
fi

if [[ "$watch" == true ]]; then
  say ""
  say "== 5. Watching for 60s; trigger one failing sandbox shell call now"
  if [[ ! -r /var/log/audit/audit.log ]]; then
    say "cannot read /var/log/audit/audit.log; run as root to watch."
  else
    timeout 60 tail -n 0 -F /var/log/audit/audit.log 2>/dev/null \
      | grep --line-buffered -E 'connectto|lw-proxy-|proxy\.sock' || true
    say "watch finished."
  fi
fi
