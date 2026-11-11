#!/bin/bash
set -euo pipefail

refuse() {
  printf 'dsh exposure guard: %s is forbidden in the LunarWing worker (exit 77)\n' "$1" >&2
  exit 77
}

expect_value=
for token in "$@"; do
  if [[ -n "$expect_value" ]]; then
    case "$token" in
      127.0.0.1|localhost|::1) ;;
      *) refuse "$expect_value $token" ;;
    esac
    expect_value=
    continue
  fi
  lowered="${token,,}"
  case "$lowered" in
    web|tunnel|relay|pair|pairing|share|service|update|upgrade|self-update)
      refuse "$token"
      ;;
    --host|--hostname|--bind)
      expect_value="$token"
      ;;
    --remote|--public|--share|--tunnel|--relay|--pair|--service|--update|--upgrade|--enable-web|--web)
      refuse "$token"
      ;;
    --host=*|--hostname=*|--bind=*)
      value="${token#*=}"
      case "$value" in
        127.0.0.1|localhost|::1) ;;
        *) refuse "$token" ;;
      esac
      ;;
  esac
done

exec /usr/local/bin/node /opt/lunardepth/node_modules/@lunardepth/dsh/lib/bin.js "$@"
