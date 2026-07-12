#!/usr/bin/env bash
# Verify OpenRC loads tenant environment data only after command_user drops
# privileges, using literal parsing rather than root-side shell sourcing.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
ENV_EXEC="$SCRIPT_DIR/../lunarwing-openrc-env-exec.sh"
TMP_ROOT="$(mktemp -d)"
foreign_parent_env=""
trap 'rm -rf "$TMP_ROOT"; [[ -z "$foreign_parent_env" ]] || rm -f "$foreign_parent_env"' EXIT

[[ -f "$ENV_EXEC" ]] || {
  printf 'OpenRC tenant env launcher is missing\n' >&2
  exit 1
}

marker="$TMP_ROOT/command-substitution-ran"
env_file="$TMP_ROOT/darkirc-adapter.env"
literal="\$(touch $marker)"
printf 'DARKIRC_ADAPTER_SECRET=%s\n' "$literal" >"$env_file"
chmod 0600 "$env_file"

value="$(bash "$ENV_EXEC" --env-file "$env_file" -- \
  /bin/sh -c 'printf %s "$DARKIRC_ADAPTER_SECRET"')"
[[ "$value" == "$literal" ]] || {
  printf 'OpenRC env launcher did not preserve the literal secret value\n' >&2
  exit 1
}
[[ ! -e "$marker" ]] || {
  printf 'OpenRC env launcher evaluated tenant shell syntax\n' >&2
  exit 1
}

if command -v unshare >/dev/null 2>&1 && unshare -Ur true >/dev/null 2>&1; then
  if unshare -Ur bash "$ENV_EXEC" --env-file "$env_file" -- /bin/true \
    >/dev/null 2>&1; then
    printf 'OpenRC env launcher accepted execution as root\n' >&2
    exit 1
  fi
fi

printf 'DARKIRC_REALNAME="LunarWing DarkIRC Bridge"\n' >"$env_file"
value="$(bash "$ENV_EXEC" --env-file "$env_file" -- \
  /bin/sh -c 'printf %s "$DARKIRC_REALNAME"')"
[[ "$value" == 'LunarWing DarkIRC Bridge' ]] || {
  printf 'OpenRC env launcher did not preserve quoted dotenv semantics\n' >&2
  exit 1
}

printf 'INVALID-NAME=value\n' >"$env_file"
if bash "$ENV_EXEC" --env-file "$env_file" -- /bin/true >/dev/null 2>&1; then
  printf 'OpenRC env launcher accepted an invalid variable name\n' >&2
  exit 1
fi

printf 'SAFE=value\n' >"$env_file"
chmod 0400 "$env_file"
if bash "$ENV_EXEC" --env-file "$env_file" -- /bin/true >/dev/null 2>&1; then
  printf 'OpenRC env launcher accepted a non-0600 env file\n' >&2
  exit 1
fi

chmod 0644 "$env_file"
if bash "$ENV_EXEC" --env-file "$env_file" -- /bin/true >/dev/null 2>&1; then
  printf 'OpenRC env launcher accepted a group/world-readable env file\n' >&2
  exit 1
fi

unsafe_parent="$TMP_ROOT/unsafe-parent"
mkdir "$unsafe_parent"
chmod 0755 "$unsafe_parent"
printf 'SAFE=value\n' >"$unsafe_parent/service.env"
chmod 0600 "$unsafe_parent/service.env"
if bash "$ENV_EXEC" --env-file "$unsafe_parent/service.env" -- /bin/true \
  >/dev/null 2>&1; then
  printf 'OpenRC env launcher accepted an accessible parent directory\n' >&2
  exit 1
fi

real_parent="$TMP_ROOT/real-parent"
symlink_parent="$TMP_ROOT/symlink-parent"
mkdir "$real_parent"
chmod 0700 "$real_parent"
printf 'SAFE=value\n' >"$real_parent/service.env"
chmod 0600 "$real_parent/service.env"
ln -s "$real_parent" "$symlink_parent"
if bash "$ENV_EXEC" --env-file "$symlink_parent/service.env" -- /bin/true \
  >/dev/null 2>&1; then
  printf 'OpenRC env launcher followed a symlinked parent directory\n' >&2
  exit 1
fi

if [[ "$EUID" -ne 0 ]]; then
  foreign_parent_env="$(mktemp /tmp/lunarwing-openrc-env.XXXXXX)"
  printf 'SAFE=value\n' >"$foreign_parent_env"
  chmod 0600 "$foreign_parent_env"
  if bash "$ENV_EXEC" --env-file "$foreign_parent_env" -- /bin/true \
    >/dev/null 2>&1; then
    printf 'OpenRC env launcher accepted a parent owned by another user\n' >&2
    exit 1
  fi
fi

# shellcheck source=../lunarwing-mt-admin.sh
# shellcheck disable=SC1091
source "$ADMIN_SCRIPT"
render_body="$(declare -f render_tenant_openrc_units)"
grep -Fq 'lunarwing_openrc_env_exec' <<<"$render_body" || {
  printf 'OpenRC services do not use the post-drop env launcher\n' >&2
  exit 1
}
if grep -Eq '\.[[:space:]]+"?\$\{[^}]*env_file' <<<"$render_body"; then
  printf 'OpenRC render still sources a tenant env file in a root hook\n' >&2
  exit 1
fi

printf 'ALL TESTS PASSED\n'
