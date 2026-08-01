#!/usr/bin/env bash
# Regression coverage for Kawarimi import/export orchestration flags.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
IC_DIR="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
IMPORT_SRC="$IC_DIR/scripts/import-tenant.sh"
EXPORT_SRC="$IC_DIR/scripts/export-tenant.sh"

failures=0

fail() {
  printf '  FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

pass() {
  printf '  PASS: %s\n' "$1"
}

assert_contains() {
  local label="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    pass "$label"
  else
    fail "$label"
    printf '        missing: %s\n' "$needle"
  fi
}

assert_file_contains() {
  local label="$1" file="$2" needle="$3"
  if grep -qF -- "$needle" "$file"; then
    pass "$label"
  else
    fail "$label"
    printf '        %s missing: %s\n' "$file" "$needle"
  fi
}

assert_file_not_contains() {
  local label="$1" file="$2" needle="$3"
  if grep -qF -- "$needle" "$file"; then
    fail "$label"
    printf '        %s unexpectedly contains: %s\n' "$file" "$needle"
  else
    pass "$label"
  fi
}

make_bundle() {
  local work="$1"
  mkdir -p "$work/bundle-src"
  cat >"$work/bundle-src/meta.txt" <<'META'
tenant=kawarimi
source_version=test
db_backend=postgres
META
  printf 'not-a-real-pgdump-for-dry-run\n' >"$work/bundle-src/db.dump"
  cat >"$work/bundle-src/manifest-lunarwing.env" <<'ENV'
SECRETS_MASTER_KEY="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
XMPP_JID=kawarimi@example.test
GATEWAY_HOST=0.0.0.0
NANOCODE_MODEL=test-nanocode-model
NANOCODE_BASE_URL=https://nanocode.example.test/openai/v1
KIMI_API_KEY=kimi-manifest-secret
ENV
  cat >"$work/bundle-src/manifest-bridge.env" <<'ENV'
XMPP_JID=kawarimi@example.test
ENV
  cat >"$work/bundle-src/manifest-vision.env" <<'ENV'
VL_URL=http://vision.example.test:8080
VL_MODEL=qwen3-vl
LUNARWING_AUTH_TOKEN=vision-token
ENV
  tar cf "$work/kawarimi.tar" -C "$work/bundle-src" .
}

make_test_scripts() {
  local work="$1" fixture_uid fixture_gid
  fixture_uid="$(id -u)"
  fixture_gid="$(id -g)"
  mkdir -p "$work/scripts" "$work/bin" "$work/home/kawarimi/lunarwing/env" \
    "$work/home/kawarimi/lunarwing/state"
  chmod 0700 "$work/home/kawarimi/lunarwing/env"
  cp "$IMPORT_SRC" "$work/scripts/import-tenant.sh"
  cp "$EXPORT_SRC" "$work/scripts/export-tenant.sh"

  cat >"$work/scripts/lunarwing-mt-admin.sh" <<'MT'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${KAWARIMI_TEST_WORK:?}/mt-admin.log"
if [[ "${1:-}" == darkirc-contact && "${2:-}" == validate-migration ]]; then
  jq -c '{status:"valid",scope_id:(.scope_id // empty)}'
  exit 0
fi
case "${1:-}" in
  stop-writers) exit 0 ;;
  writers-active)
    case "${KAWARIMI_WRITER_MODE:-inactive}" in
      active) exit 0 ;;
      inactive) exit 1 ;;
      *) exit 2 ;;
    esac
    ;;
  owner-scopes)
    if [[ -f "$KAWARIMI_TEST_WORK/scope-migrated" ]]; then
      printf 'kawarimi\t12\n'
    else
      printf 'default\t12\n'
    fi
    ;;
  migrate-owner-scope)
    touch "$KAWARIMI_TEST_WORK/scope-migrated"
    exit 0
    ;;
  restore-tenant) exit 0 ;;
  *) exit 0 ;;
esac
MT
  chmod +x "$work/scripts/lunarwing-mt-admin.sh"

  cat >"$work/scripts/lunarwing-weechat-preflight.sh" <<'PREFLIGHT'
#!/usr/bin/env bash
printf 'preflight %s\n' "${1:-}" >> "${KAWARIMI_TEST_WORK:?}/preflight.log"
PREFLIGHT
  chmod +x "$work/scripts/import-tenant.sh" "$work/scripts/lunarwing-weechat-preflight.sh"

  cat >"$work/bin/id" <<ID
#!/usr/bin/env bash
if [[ "\${1:-}" == "-u" && \$# -eq 1 ]]; then
  if [[ "\${KAWARIMI_FAKE_TENANT:-}" == 1 ]]; then
    printf '%s\n' "$fixture_uid"
  else
    printf '0\n'
  fi
elif [[ "\${1:-}" == "-u" && "\${2:-}" == "kawarimi" ]]; then
  printf '%s\n' "$fixture_uid"
else
  exec /usr/bin/id "\$@"
fi
ID
  chmod +x "$work/bin/id"

  cat >"$work/bin/getent" <<GETENT
#!/usr/bin/env bash
if [[ "\${1:-}" == "passwd" && "\${2:-}" == "kawarimi" ]]; then
  printf 'kawarimi:x:$fixture_uid:$fixture_gid::%s/home/kawarimi:/bin/bash\n' "$work"
else
  exit 2
fi
GETENT
  cat >"$work/bin/install" <<'INSTALL'
#!/usr/bin/env bash
target="${!#}"
if [[ "${1:-}" == "-d" && "$target" == "${LUNARWING_MT_LIFECYCLE_LOCK_DIR:?}" ]]; then
  mkdir -p -- "$target"
  chmod 0700 -- "$target"
else
  exec /usr/bin/install "$@"
fi
INSTALL
  cat >"$work/bin/stat" <<'STAT'
#!/usr/bin/env bash
target="${!#}"
if [[ "${1:-}" == "-c" && "${2:-}" == "%u:%a" \
  && "$target" == "${LUNARWING_MT_LIFECYCLE_LOCK_DIR:?}" ]]; then
  printf '0:700\n'
else
  exec /usr/bin/stat "$@"
fi
STAT
  cat >"$work/bin/sudo" <<'SUDO'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" ]]; then shift 2; fi
export KAWARIMI_FAKE_TENANT=1
exec "$@"
SUDO
  chmod +x "$work/bin/getent" "$work/bin/install" "$work/bin/stat" "$work/bin/sudo"
}

echo "=== import-tenant dry-run flag passthrough ==="
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export LUNARWING_MT_LIFECYCLE_LOCK_DIR="$tmp/lifecycle-locks"
make_bundle "$tmp"
make_test_scripts "$tmp"
printf '{"tenants":{}}\n' >"$tmp/ports.json"

output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/kawarimi.tar" \
    --dry-run --yes --start --old-stopped \
    --docker-group \
    --with-vision \
    2>&1
)" || {
  status=$?
  printf '%s\n' "$output"
  fail "import dry-run exited successfully"
  exit "$status"
}

assert_contains "passes --docker-group to add-tenant" "$output" "--docker-group"
assert_contains "preserves nanocode model override" "$output" "--nanocode-model test-nanocode-model"
assert_contains "preserves nanocode base URL override" "$output" "--nanocode-base-url https://nanocode.example.test/openai/v1"
assert_contains "builds vision sidecar when requested" "$output" "build-vision-sidecar"
assert_contains "injects vision manifest" "$output" "manifest-vision.env ->"
assert_contains "runs WeeChat preflight before start" "$output" "lunarwing-weechat-preflight.sh kawarimi"

echo "=== structured DarkIRC migration wiring ==="
darkirc_tmp="$(mktemp -d)"
make_bundle "$darkirc_tmp"
printf 'darkirc_scope_id=fedcba9876543210fedcba9876543210\n' >>"$darkirc_tmp/bundle-src/meta.txt"
printf '{}\n' >"$darkirc_tmp/bundle-src/darkirc-contacts-v1.json"
tar cf "$darkirc_tmp/kawarimi.tar" -C "$darkirc_tmp/bundle-src" .
make_test_scripts "$darkirc_tmp"
printf '{"tenants":{}}\n' >"$darkirc_tmp/ports.json"
darkirc_output="$(
  PATH="$darkirc_tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$darkirc_tmp/ports.json" \
  KAWARIMI_TEST_WORK="$darkirc_tmp" \
  bash "$darkirc_tmp/scripts/import-tenant.sh" "$darkirc_tmp/kawarimi.tar" \
    --dry-run --yes 2>&1
)" || {
  status=$?
  printf '%s\n' "$darkirc_output"
  fail "structured DarkIRC import dry-run exited successfully"
  exit "$status"
}
assert_contains "DarkIRC import enables target service" "$darkirc_output" "--enable-darkirc"
assert_contains "DarkIRC import preserves stable scope" "$darkirc_output" \
  "--darkirc-scope-id fedcba9876543210fedcba9876543210"
assert_contains "DarkIRC import stages structured manifest" "$darkirc_output" \
  "would stage and import structured DarkIRC contacts"
assert_contains "DarkIRC import plans typed preflight" "$darkirc_output" \
  "would typed-validate darkirc-contacts-v1.json before provisioning"
assert_file_not_contains "DarkIRC preflight binds target compatibility" "$IMPORT_SRC" \
  "validate-migration --in - --json --unbound"
rm -rf "$darkirc_tmp"

echo "=== import-tenant worker selection reaches BOTH add-tenant and build-tenant ==="
# Regression guard for per-tenant worker gating: start-tenant now gates workers on
# the registry flag persisted at add-tenant, so import must forward --with-* to
# add-tenant too (not only build-tenant) — otherwise a --start import would build
# workers but never start them. See docs/proposals/PER_TENANT_WORKER_GATING.md.
worker_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/kawarimi.tar" \
    --dry-run --yes \
    --with-nanocode --with-opencode \
    2>&1
)" || {
  status=$?
  printf '%s\n' "$worker_output"
  fail "worker-selection import dry-run exited successfully"
  exit "$status"
}
# Under --dry-run the import prints (does not exec) each planned command via
# run()'s `[dry-run] <cmd>` line, so assert against stdout. Isolate the two
# planned invocations so a flag on one can't satisfy an assertion on the other.
add_line="$(printf '%s\n' "$worker_output" | grep -m1 '\[dry-run\].* add-tenant ' || true)"
build_line="$(printf '%s\n' "$worker_output" | grep -m1 '\[dry-run\].* build-tenant ' || true)"
assert_contains "add-tenant carries --with-nanocode" "$add_line" "--with-nanocode"
assert_contains "add-tenant carries --with-opencode" "$add_line" "--with-opencode"
assert_contains "build-tenant carries --with-nanocode" "$build_line" "--with-nanocode"
assert_contains "build-tenant carries --with-opencode" "$build_line" "--with-opencode"
# Unselected worker must NOT appear on either planned call.
if [[ "$add_line" == *"--with-pebble"* ]]; then
  fail "add-tenant leaked --with-pebble (not selected)"
else
  pass "add-tenant omits unselected --with-pebble"
fi
if [[ "$build_line" == *"--with-pebble"* ]]; then
  fail "build-tenant leaked --with-pebble (not selected)"
else
  pass "build-tenant omits unselected --with-pebble"
fi

echo "=== import-tenant explicit owner-scope dry-run ==="
explicit_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/kawarimi.tar" \
    --dry-run --yes --owner-scope legacy-scope \
    2>&1
)" || {
  status=$?
  printf '%s\n' "$explicit_output"
  fail "explicit owner-scope dry-run exited successfully"
  exit "$status"
}
assert_contains "plans explicit owner-scope rekey" "$explicit_output" "migrate-owner-scope kawarimi --from legacy-scope"

echo "=== import-tenant auto owner-scope migration ==="
rm -f "$tmp/mt-admin.log" "$tmp/scope-migrated"
cat >"$tmp/home/kawarimi/lunarwing/env/lunarwing.env" <<'ENV'
SECRETS_MASTER_KEY=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
VISION_AUTH_TOKEN=target-vision-token
ENV
cat >"$tmp/home/kawarimi/lunarwing/env/xmpp-bridge.env" <<'ENV'
XMPP_JID=throwaway@example.test
ENV
cat >"$tmp/home/kawarimi/lunarwing/env/vision.env" <<'ENV'
VL_MODEL=throwaway
LUNARWING_AUTH_TOKEN=target-vision-token
ENV
chmod 0600 "$tmp/home/kawarimi/lunarwing/env/"*.env
auto_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/kawarimi.tar" --yes \
    2>&1
)" || {
  status=$?
  printf '%s\n' "$auto_output"
  fail "auto owner-scope import exited successfully"
  exit "$status"
}
assert_file_contains "auto-migrates default owner scope" "$tmp/mt-admin.log" "migrate-owner-scope kawarimi --from default"
assert_file_contains "import keeps target sidecar token" "$tmp/home/kawarimi/lunarwing/env/vision.env" "LUNARWING_AUTH_TOKEN=target-vision-token"
assert_file_contains "import keeps matching daemon token" "$tmp/home/kawarimi/lunarwing/env/lunarwing.env" "VISION_AUTH_TOKEN=target-vision-token"
assert_file_not_contains "import ignores carried sidecar token" "$tmp/home/kawarimi/lunarwing/env/vision.env" "LUNARWING_AUTH_TOKEN=vision-token"
assert_file_contains "import injects Kimi key after provisioning" "$tmp/home/kawarimi/lunarwing/env/lunarwing.env" "KIMI_API_KEY=kimi-manifest-secret"
assert_file_not_contains "import keeps Kimi key out of mt-admin argv" "$tmp/mt-admin.log" "kimi-manifest-secret"
if [[ "$auto_output" == *"kimi-manifest-secret"* ]]; then
  fail "import keeps Kimi key out of logs"
else
  pass "import keeps Kimi key out of logs"
fi

echo "=== export-tenant vision manifest coverage ==="
assert_file_contains "export writes a vision manifest" "$EXPORT_SRC" "manifest-vision.env"
assert_file_contains "export carries VL_URL" "$EXPORT_SRC" "VL_URL"
assert_file_contains "export carries VL_MODEL" "$EXPORT_SRC" "VL_MODEL"
assert_file_not_contains "export does not carry host-local vision auth token" "$EXPORT_SRC" "VISION_KEYS=(VL_URL VL_MODEL LUNARWING_AUTH_TOKEN)"

echo "=== encrypted bundle passphrase safety ==="
passphrase="correct-horse-battery-staple"
encrypted_bundle="$tmp/kawarimi.7z"
passphrase_fd_file="$tmp/passphrase-no-newline"
printf '%s' "$passphrase" >"$passphrase_fd_file"
(
  cd "$tmp/bundle-src"
  { printf '%s\n%s\n' "$passphrase" "$passphrase"; } \
    | 7z a -t7z -mhe=on -p "$encrypted_bundle" ./* >/dev/null
)
encrypted_output="$(
  exec 9<"$passphrase_fd_file"
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  KAWARIMI_PASS_FD=9 \
  bash "$tmp/scripts/import-tenant.sh" "$encrypted_bundle" --dry-run --yes \
    2>&1
)" || {
  status=$?
  printf '%s\n' "$encrypted_output"
  fail "encrypted import dry-run accepts the correct passphrase"
  exit "$status"
}
assert_contains "encrypted import reaches dry-run plan" "$encrypted_output" "DRY RUN"
if [[ "$encrypted_output" == *"$passphrase"* ]]; then
  fail "encrypted import output does not expose passphrase"
else
  pass "encrypted import output does not expose passphrase"
fi

set +e
wrong_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  KAWARIMI_PASS="definitely-the-wrong-passphrase" \
  bash "$tmp/scripts/import-tenant.sh" "$encrypted_bundle" --dry-run --yes \
    2>&1
)"
wrong_status=$?
set -e
if [[ "$wrong_status" -ne 0 && "$wrong_output" == *"wrong passphrase or corrupted archive"* ]]; then
  pass "encrypted import rejects a wrong passphrase"
else
  fail "encrypted import rejects a wrong passphrase"
fi

echo "=== bundle layout and manifest validation ==="
mkdir -p "$tmp/unsafe-manifest"
cp -a "$tmp/bundle-src/." "$tmp/unsafe-manifest/"
printf 'DATABASE_URL=postgres://attacker.invalid/override\n' >>"$tmp/unsafe-manifest/manifest-lunarwing.env"
tar cf "$tmp/unsafe-manifest.tar" -C "$tmp/unsafe-manifest" .
set +e
unsafe_manifest_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/unsafe-manifest.tar" --dry-run --yes \
    2>&1
)"
unsafe_manifest_status=$?
set -e
if [[ "$unsafe_manifest_status" -ne 0 && "$unsafe_manifest_output" == *"unsupported key 'DATABASE_URL'"* ]]; then
  pass "import rejects unsupported manifest keys"
else
  fail "import rejects unsupported manifest keys"
fi

mkdir -p "$tmp/unsafe-link"
cp "$tmp/bundle-src/meta.txt" "$tmp/bundle-src/manifest-lunarwing.env" "$tmp/unsafe-link/"
ln -s /etc/passwd "$tmp/unsafe-link/db.dump"
tar cf "$tmp/unsafe-link.tar" -C "$tmp/unsafe-link" .
set +e
unsafe_link_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/unsafe-link.tar" --dry-run --yes \
    2>&1
)"
unsafe_link_status=$?
set -e
if [[ "$unsafe_link_status" -ne 0 && "$unsafe_link_output" == *"link or special file"* ]]; then
  pass "import rejects symbolic links in bundle layout"
else
  fail "import rejects symbolic links in bundle layout"
fi

mkdir -p "$tmp/unsafe-state/state"
ln -s /etc/passwd "$tmp/unsafe-state/state/outside"
tar czf "$tmp/unsafe-state.tar.gz" -C "$tmp/unsafe-state" state
mkdir -p "$tmp/unsafe-state-bundle"
cp "$tmp/bundle-src/meta.txt" "$tmp/bundle-src/db.dump" \
  "$tmp/bundle-src/manifest-lunarwing.env" "$tmp/unsafe-state-bundle/"
cp "$tmp/unsafe-state.tar.gz" "$tmp/unsafe-state-bundle/state.tar.gz"
tar cf "$tmp/unsafe-state-bundle.tar" -C "$tmp/unsafe-state-bundle" .
set +e
unsafe_state_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/unsafe-state-bundle.tar" --dry-run --yes \
    2>&1
)"
unsafe_state_status=$?
set -e
if [[ "$unsafe_state_status" -ne 0 && "$unsafe_state_output" == *"state archive contains a link or special file"* ]]; then
  pass "import rejects links and special files in nested state"
else
  fail "import rejects links and special files in nested state"
fi

mkdir -p "$tmp/managed-darkirc/state/darkirc/key-exchange"
printf '{}\n' >"$tmp/managed-darkirc/state/darkirc/key-exchange/ledger.json"
tar czf "$tmp/managed-darkirc-state.tar.gz" -C "$tmp/managed-darkirc" state
mkdir -p "$tmp/managed-darkirc-bundle"
cp "$tmp/bundle-src/meta.txt" "$tmp/bundle-src/db.dump" \
  "$tmp/bundle-src/manifest-lunarwing.env" "$tmp/managed-darkirc-bundle/"
cp "$tmp/managed-darkirc-state.tar.gz" "$tmp/managed-darkirc-bundle/state.tar.gz"
tar cf "$tmp/managed-darkirc-bundle.tar" -C "$tmp/managed-darkirc-bundle" .
set +e
managed_darkirc_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/managed-darkirc-bundle.tar" --dry-run --yes \
    2>&1
)"
managed_darkirc_status=$?
set -e
if [[ "$managed_darkirc_status" -ne 0 && "$managed_darkirc_output" == *"structured contact import"* ]]; then
  pass "import rejects generic managed DarkIRC state"
else
  fail "import rejects generic managed DarkIRC state"
fi

tar czf "$tmp/managed-darkirc-alias-state.tar.gz" \
  --transform='s#^state/darkirc#state/./darkirc#' -C "$tmp/managed-darkirc" state
cp "$tmp/managed-darkirc-alias-state.tar.gz" "$tmp/managed-darkirc-bundle/state.tar.gz"
tar cf "$tmp/managed-darkirc-alias-bundle.tar" -C "$tmp/managed-darkirc-bundle" .
set +e
managed_alias_output="$(
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/managed-darkirc-alias-bundle.tar" --dry-run --yes \
    2>&1
)"
managed_alias_status=$?
set -e
if [[ "$managed_alias_status" -ne 0 && "$managed_alias_output" == *"structured contact import"* ]]; then
  pass "import normalizes managed DarkIRC path aliases"
else
  fail "import normalizes managed DarkIRC path aliases"
fi

set +e
limit_output="$(
  PATH="$tmp/bin:$PATH" \
  KAWARIMI_MAX_BUNDLE_BYTES=1 \
  bash "$tmp/scripts/import-tenant.sh" "$tmp/kawarimi.tar" --dry-run --yes \
    2>&1
)"
limit_status=$?
set -e
if [[ "$limit_status" -ne 0 && "$limit_output" == *"exceeds KAWARIMI_MAX_BUNDLE_BYTES"* ]]; then
  pass "import enforces archive resource limits before extraction"
else
  fail "import enforces archive resource limits before extraction"
fi

invalid_key_tmp="$(mktemp -d)"
make_bundle "$invalid_key_tmp"
sed -i 's/^SECRETS_MASTER_KEY=.*/SECRETS_MASTER_KEY=not-a-master-key/' \
  "$invalid_key_tmp/bundle-src/manifest-lunarwing.env"
tar cf "$invalid_key_tmp/kawarimi.tar" -C "$invalid_key_tmp/bundle-src" .
make_test_scripts "$invalid_key_tmp"
printf '{"tenants":{}}\n' >"$invalid_key_tmp/ports.json"
set +e
invalid_key_output="$(
  PATH="$invalid_key_tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$invalid_key_tmp/ports.json" \
  KAWARIMI_TEST_WORK="$invalid_key_tmp" \
  bash "$invalid_key_tmp/scripts/import-tenant.sh" "$invalid_key_tmp/kawarimi.tar" \
    --dry-run --yes 2>&1
)"
invalid_key_rc=$?
set -e
[[ "$invalid_key_rc" -ne 0 ]] || fail "import accepts an invalid SECRETS_MASTER_KEY"
assert_contains "import rejects invalid master key before provisioning" "$invalid_key_output" "missing or invalid SECRETS_MASTER_KEY"
rm -rf "$invalid_key_tmp"

assert_file_not_contains "export keeps passphrase out of 7z argv" "$EXPORT_SRC" "-p\"\$KAWARIMI"
assert_file_not_contains "import keeps passphrase out of 7z argv" "$IMPORT_SRC" "-p\"\$KAWARIMI"

echo "=== export rejects missing passphrase before quiescence ==="
mkdir -p "$tmp/export-home/alpha/lunarwing/env" "$tmp/export-bin" "$tmp/export-out"
cat >"$tmp/export-home/alpha/lunarwing/env/lunarwing.env" <<'ENV'
DATABASE_BACKEND=postgres
DATABASE_URL=postgres://lunarwing:test@127.0.0.1/lunarwing
SECRETS_MASTER_KEY=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
ENV
cat >"$tmp/export-bin/id" <<'ID'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" ]]; then
  if [[ $# -eq 1 ]]; then printf '0\n'; else printf '1000\n'; fi
  exit 0
fi
[[ "${1:-}" == "alpha" ]] && exit 0
exec /usr/bin/id "$@"
ID
cat >"$tmp/export-bin/getent" <<GETENT
#!/usr/bin/env bash
printf 'alpha:x:1000:1000::%s/export-home/alpha:/bin/bash\n' "$tmp"
GETENT
cat >"$tmp/export-bin/docker" <<'DOCKER'
#!/usr/bin/env bash
if [[ "${1:-}" == "inspect" ]]; then
  [[ "${2:-}" == "-f" ]] && printf 'true\n'
  exit 0
fi
if [[ "${1:-}" == "exec" ]]; then
  printf 'PGDMP-test-export\n'
  exit 0
fi
exit 0
DOCKER
cat >"$tmp/export-bin/sudo" <<'SUDO'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" ]]; then shift 2; fi
if [[ "${1:-}" == *=* ]]; then exec env "$@"; fi
exec "$@"
SUDO
cat >"$tmp/export-bin/git" <<'GIT'
#!/usr/bin/env bash
printf 'v2.0.2.0-test\n'
GIT
cat >"$tmp/export-bin/systemctl" <<'SYSTEMCTL'
#!/usr/bin/env bash
printf 'called\n' >>"${KAWARIMI_TEST_WORK:?}/systemctl-called"
if [[ "${KAWARIMI_SYSTEMCTL_MODE:-}" == unavailable && " $* " == *" list-unit-files "* ]]; then
  exit 1
fi
if [[ "${KAWARIMI_SYSTEMCTL_MODE:-}" == active && " $* " == *" is-active "* ]]; then
  printf 'active\n'
  exit 0
fi
if [[ "${KAWARIMI_SYSTEMCTL_MODE:-}" == inactive && " $* " == *" is-active "* ]]; then
  printf 'inactive\n'
  exit 3
fi
exit 0
SYSTEMCTL
cat >"$tmp/export-bin/stat" <<'STAT'
#!/usr/bin/env bash
if [[ "${1:-}" == "-c" && "${2:-}" == "%u" ]]; then
  printf '0\n'
elif [[ "${1:-}" == "-c" && "${2:-}" == "%u:%a" ]]; then
  printf '0:%s\n' "$(/usr/bin/stat -c '%a' "${!#}")"
elif [[ "${1:-}" == "-c" && "${2:-}" == "%a" && -d "${!#}" \
  && ( "${!#}" == /tmp || "${!#}" == "${KAWARIMI_TEST_WORK:?}"* ) ]]; then
  printf '700\n'
else
  exec /usr/bin/stat "$@"
fi
STAT
cat >"$tmp/export-bin/install" <<'INSTALL'
#!/usr/bin/env bash
if [[ "${1:-}" == "-d" ]]; then
  target="${!#}"
  mkdir -p -- "$target"
  chmod 0700 -- "$target"
else
  exec /usr/bin/install "$@"
fi
INSTALL
chmod +x "$tmp/export-bin/"*

set +e
missing_output="$(
  PATH="$tmp/export-bin:$PATH" \
  KAWARIMI_TEST_WORK="$tmp" \
  KAWARIMI_PASS='' KAWARIMI_PASS_FILE='' KAWARIMI_PASS_FD='' \
  bash "$tmp/scripts/export-tenant.sh" alpha --out-dir "$tmp/export-out" </dev/null 2>&1
)"
missing_status=$?
set -e
if [[ "$missing_status" -ne 0 && "$missing_output" == *"encryption passphrase required"* ]]; then
  pass "missing passphrase fails before export mutation"
else
  fail "missing passphrase fails before export mutation"
  printf '%s\n' "$missing_output"
fi
if [[ -e "$tmp/systemctl-called" ]]; then
  fail "missing passphrase does not stop tenant services"
else
  pass "missing passphrase does not stop tenant services"
fi

echo "=== export validates source secrets before quiescence ==="
printf 'XMPP_PASSWORD=daemon-password\n' >>"$tmp/export-home/alpha/lunarwing/env/lunarwing.env"
cat >"$tmp/export-home/alpha/lunarwing/env/xmpp-bridge.env" <<'ENV'
XMPP_PASSWORD=bridge-password
ENV
set +e
mismatch_output="$(
  PATH="$tmp/export-bin:$PATH" \
  KAWARIMI_TEST_WORK="$tmp" \
  KAWARIMI_PASS='valid-export-passphrase' \
  bash "$tmp/scripts/export-tenant.sh" alpha --out-dir "$tmp/export-out" </dev/null 2>&1
)"
mismatch_status=$?
set -e
if [[ "$mismatch_status" -ne 0 && "$mismatch_output" == *"XMPP_PASSWORD differs"* ]]; then
  pass "export rejects mismatched XMPP credentials"
else
  fail "export rejects mismatched XMPP credentials"
fi
if [[ -e "$tmp/systemctl-called" ]]; then
  fail "XMPP mismatch does not stop tenant services"
else
  pass "XMPP mismatch does not stop tenant services"
fi
sed -i '/^XMPP_PASSWORD=/d' "$tmp/export-home/alpha/lunarwing/env/lunarwing.env"
rm -f "$tmp/export-home/alpha/lunarwing/env/xmpp-bridge.env"

echo "=== export requires structured DarkIRC tooling before quiescence ==="
mkdir -p "$tmp/export-home/alpha/lunarwing/state/darkirc/key-exchange"
printf '{}\n' >"$tmp/export-home/alpha/lunarwing/state/darkirc/key-exchange/ledger.json"
rm -f "$tmp/systemctl-called"
set +e
managed_export_output="$(
  PATH="$tmp/export-bin:$PATH" \
  KAWARIMI_TEST_WORK="$tmp" \
  LUNARWING_MT_ADMIN="$tmp/missing-mt-admin" \
  KAWARIMI_PASS='valid-export-passphrase' \
  bash "$EXPORT_SRC" alpha --out-dir "$tmp/export-out" </dev/null 2>&1
)"
managed_export_status=$?
set -e
if [[ "$managed_export_status" -ne 0 && "$managed_export_output" == *"trusted mt-admin not found"* ]]; then
  pass "export requires structured DarkIRC tooling"
else
  fail "export requires structured DarkIRC tooling"
fi
if [[ -e "$tmp/systemctl-called" ]]; then
  fail "missing DarkIRC tooling does not stop tenant services"
else
  pass "missing DarkIRC tooling does not stop tenant services"
fi
rm -rf "$tmp/export-home/alpha/lunarwing/state/darkirc/key-exchange"

echo "=== export --no-quiesce fails closed ==="
for writer_mode in active unavailable; do
  rm -f "$tmp/systemctl-called"
  set +e
  no_quiesce_output="$(
    PATH="$tmp/export-bin:$PATH" \
    KAWARIMI_TEST_WORK="$tmp" \
    KAWARIMI_WRITER_MODE="$writer_mode" \
    bash "$tmp/scripts/export-tenant.sh" alpha --out-dir "$tmp/export-out" --no-quiesce --no-encrypt \
      </dev/null 2>&1
  )"
  no_quiesce_status=$?
  set -e
  if [[ "$writer_mode" == active ]]; then
    expected_no_quiesce="active writers"
  else
    expected_no_quiesce="could not verify"
  fi
  if [[ "$no_quiesce_status" -ne 0 && "$no_quiesce_output" == *"$expected_no_quiesce"* \
    && "$no_quiesce_output" != *"Database (pg_dump"* ]]; then
    pass "--no-quiesce rejects $writer_mode writer state before snapshot"
  else
    fail "--no-quiesce rejects $writer_mode writer state before snapshot"
  fi
done

echo "=== production export -> encrypted import dry-run ==="
rm -f "$tmp/systemctl-called"
full_passphrase="full-roundtrip-passphrase"
printf '%s' "$full_passphrase" >"$tmp/full-passphrase"
cat >>"$tmp/export-home/alpha/lunarwing/env/lunarwing.env" <<'ENV'
LLM_BASE_URL=https://localhost:7443/openai/v1
NANOCODE_MODEL=roundtrip-nanocode
NANOCODE_BASE_URL=https://127.0.0.2:7443/openai/v1
OPENCODE_MODEL=roundtrip-opencode
OPENCODE_BASE_URL=https://[::1]:7443/openai/v1
KIMI_API_KEY=encrypted-roundtrip-kimi-secret
ENV
full_export_output="$(
  exec 8<"$tmp/full-passphrase"
  PATH="$tmp/export-bin:$PATH" \
  KAWARIMI_TEST_WORK="$tmp" \
  KAWARIMI_PASS_FD=8 \
  bash "$tmp/scripts/export-tenant.sh" alpha --out-dir "$tmp/export-out" 2>&1
)" || {
  status=$?
  printf '%s\n' "$full_export_output"
  fail "production export writes an encrypted archive"
  exit "$status"
}
exported_bundle="$(printf '%s\n' "$tmp/export-out/"*.7z)"
if [[ -f "$exported_bundle" && "$(stat -c '%a' "$exported_bundle")" == "600" ]]; then
  pass "production export writes a mode-0600 .7z bundle"
else
  fail "production export writes a mode-0600 .7z bundle"
fi

roundtrip_output="$(
  exec 9<"$tmp/full-passphrase"
  PATH="$tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
  KAWARIMI_TEST_WORK="$tmp" \
  KAWARIMI_PASS_FD=9 \
  bash "$tmp/scripts/import-tenant.sh" "$exported_bundle" --dry-run --yes \
    2>&1
)" || {
  status=$?
  printf '%s\n' "$roundtrip_output"
  fail "production encrypted export can be imported"
  exit "$status"
}
assert_contains "production encrypted export can be imported" "$roundtrip_output" "DRY RUN"
assert_contains "production export preserves Nanocode model" "$roundtrip_output" "--nanocode-model roundtrip-nanocode"
assert_contains "production export preserves OpenCode model" "$roundtrip_output" "--opencode-model roundtrip-opencode"
if [[ "$full_export_output$roundtrip_output" == *"encrypted-roundtrip-kimi-secret"* ]]; then
  fail "encrypted Kimi round-trip does not log plaintext key"
else
  pass "encrypted Kimi round-trip does not log plaintext key"
fi
mkdir -p "$tmp/roundtrip-extract"
{
  printf '%s\n' "$full_passphrase"
} | 7z x -o"$tmp/roundtrip-extract" "$exported_bundle" -y >/dev/null 2>&1
manifest_path="$(find "$tmp/roundtrip-extract" -name manifest-lunarwing.env -print -quit)"
assert_file_contains "encrypted manifest preserves Kimi API key" "$manifest_path" "KIMI_API_KEY=encrypted-roundtrip-kimi-secret"
for source_local_url in \
  'https://localhost:7443/openai/v1' \
  'https://127.0.0.2:7443/openai/v1' \
  'https://[::1]:7443/openai/v1'; do
  if [[ "$roundtrip_output" == *"$source_local_url"* ]]; then
    fail "production export drops host-local endpoint $source_local_url"
  else
    pass "production export drops host-local endpoint $source_local_url"
  fi
done

echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
