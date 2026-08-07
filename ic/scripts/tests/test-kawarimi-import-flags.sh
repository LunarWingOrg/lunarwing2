#!/usr/bin/env bash
# Regression coverage for Kawarimi import/export orchestration flags.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
IC_DIR="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
IMPORT_SRC="$IC_DIR/scripts/import-tenant.sh"
EXPORT_SRC="$IC_DIR/scripts/export-tenant.sh"
# Stage under HOME, not the checkout: import-tenant.sh rejects any
# group/world-writable stage ancestor, and CI build-dir ancestors are.
TEST_STAGE_PARENT="${HOME:?HOME must identify a secure test-stage ancestor}"
mkdir -p "$TEST_STAGE_PARENT"
export KAWARIMI_IMPORT_STAGE_ROOT="$TEST_STAGE_PARENT/kawarimi-import-stage.$$"
SUDO_UID="$(id -u)"
SUDO_USER="$(id -un)"
export SUDO_UID SUDO_USER

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

assert_not_contains() {
  local label="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label"
    printf '        unexpectedly contains: %s\n' "$needle"
  else
    pass "$label"
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
  chmod 0600 "$work/kawarimi.tar"
}

make_test_scripts() {
  local work="$1" fixture_uid fixture_gid
  fixture_uid="$(id -u)"
  fixture_gid="$(id -g)"
  mkdir -p "$work/scripts" "$work/bin" "$work/home/kawarimi/lunarwing/env" \
    "$work/home/kawarimi/lunarwing/state"
  chmod 0700 "$work/home/kawarimi/lunarwing/env" "$work/home/kawarimi/lunarwing/state"
  cp "$IMPORT_SRC" "$work/scripts/import-tenant.sh"

  cat >"$work/scripts/lunarwing-mt-admin.sh" <<'MT'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${KAWARIMI_TEST_WORK:?}/mt-admin.log"
case "${1:-}" in
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
if [[ "\${1:-}" == "-u" && \$# -eq 1 && "\${KAWARIMI_FAKE_TENANT:-}" == 1 ]]; then
  printf '%s\\n' "$fixture_uid"
elif [[ "\${1:-}" == "-u" && \$# -eq 1 ]]; then
  printf '0\\n'
elif [[ "\${1:-}" == "-u" && "\${2:-}" == "kawarimi" ]]; then
  printf '%s\\n' "$fixture_uid"
else
  exec /usr/bin/id "\$@"
fi
ID
  chmod +x "$work/bin/id"

  cat >"$work/bin/getent" <<GETENT
#!/usr/bin/env bash
if [[ "\${1:-}" == "passwd" && "\${2:-}" == "kawarimi" ]]; then
  printf 'kawarimi:x:%s:%s::%s/home/kawarimi:/bin/bash\n' "$fixture_uid" "$fixture_gid" "$work"
else
  exit 2
fi
GETENT
  cat >"$work/bin/sudo" <<'SUDO'
#!/usr/bin/env bash
if [[ "${1:-}" == -u ]]; then
  export KAWARIMI_FAKE_TENANT=1
  shift 2
fi
if [[ "${1:-}" == env ]]; then
  shift
  [[ "${1:-}" == -i ]] && shift
  while [[ "${1:-}" == *=* ]]; do export "$1"; shift; done
fi
exec "$@"
SUDO
  chmod +x "$work/bin/getent" "$work/bin/sudo"
}

echo "=== import-tenant dry-run flag passthrough ==="
# Fixture root lives under HOME, not /tmp: as root (CI), export-tenant.sh
# rejects output paths with sticky/setuid/group/world-writable ancestors
# (/tmp is 1777), and import-tenant.sh likewise distrusts /tmp staging.
tmp="$(mktemp -d "${HOME:?HOME must identify a secure fixture ancestor}/kawarimi-import-flags.XXXXXX")"
trap 'rm -rf "$tmp" "$KAWARIMI_IMPORT_STAGE_ROOT"' EXIT
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
assert_not_contains "drops retired nanocode model override" "$output" "--nanocode-model"
assert_not_contains "drops retired nanocode base URL override" "$output" "--nanocode-base-url"
assert_contains "builds vision sidecar when requested" "$output" "build-vision-sidecar"
assert_contains "injects vision manifest" "$output" "manifest-vision.env ->"
assert_contains "runs WeeChat preflight before start" "$output" "lunarwing-weechat-preflight.sh kawarimi"

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
    --with-opencode \
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
assert_not_contains "add-tenant omits retired --with-nanocode" "$add_line" "--with-nanocode"
assert_contains "add-tenant carries --with-opencode" "$add_line" "--with-opencode"
assert_not_contains "build-tenant omits retired --with-nanocode" "$build_line" "--with-nanocode"
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

set +e
retired_output="$(bash "$tmp/scripts/import-tenant.sh" "$tmp/kawarimi.tar" --with-nanocode 2>&1)"
retired_status=$?
set -e
if [[ "$retired_status" -eq 2 && "$retired_output" == *"Nanocode worker is retired"* ]]; then
  pass "import rejects retired --with-nanocode explicitly"
else
  fail "import rejects retired --with-nanocode explicitly"
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

echo "=== study-notes (knowledge store) travel coverage ==="
# Study notes are ordinary rows in memory_documents (provenance 'study-note'),
# so they move with the tenant's full-database dump and the owner-scope rekey.
# These assertions pin the contract: the dump must stay whole-database (no
# table exclusion) and the rekey must keep covering memory_documents, or
# study notes would be left behind on the source host.
assert_file_contains "export dumps the whole tenant database (memory_documents included)" "$EXPORT_SRC" "pg_dump -U \"\$PG_ROLE\" -Fc \"\$PG_DB\""
assert_file_not_contains "export does not exclude memory_documents from the dump" "$EXPORT_SRC" "--exclude-table=memory_documents"
assert_file_not_contains "export does not exclude memory_chunks from the dump" "$EXPORT_SRC" "--exclude-table=memory_chunks"
assert_file_contains "owner-scope rekey still covers memory_documents (study notes rekeyed)" "$IC_DIR/scripts/lunarwing-mt-admin.sh" "memory_documents)"
assert_file_contains "migration doc records that study notes travel" "$IC_DIR/../docs/ops/MT-MACHINE-MIGRATION.md" "study notes"

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
chmod 0600 "$encrypted_bundle"
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
chmod 0600 "$tmp/unsafe-manifest.tar"
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
chmod 0600 "$tmp/unsafe-link.tar"
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
chmod 0600 "$tmp/unsafe-state-bundle.tar"
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
chmod 0600 "$invalid_key_tmp/kawarimi.tar"
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
export_fixture_uid="$(id -u)"
export_fixture_gid="$(id -g)"
mkdir -p "$tmp/export-home/alpha/lunarwing/env" "$tmp/export-home/alpha/lunarwing/state" \
  "$tmp/export-bin" "$tmp/export-out"
chmod 0700 "$tmp/export-home/alpha" "$tmp/export-home/alpha/lunarwing" \
  "$tmp/export-home/alpha/lunarwing/env" "$tmp/export-home/alpha/lunarwing/state" \
  "$tmp/export-out"
cat >"$tmp/export-home/alpha/lunarwing/env/lunarwing.env" <<'ENV'
DATABASE_BACKEND=postgres
DATABASE_URL=postgres://lunarwing:test@127.0.0.1/lunarwing
SECRETS_MASTER_KEY=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
ENV
chmod 0600 "$tmp/export-home/alpha/lunarwing/env/lunarwing.env"
printf '{"tenants":{"alpha":{"enable_darkirc":false}}}\n' >"$tmp/export-ports.json"
cp "$EXPORT_SRC" "$tmp/export-tenant.sh"
chmod +x "$tmp/export-tenant.sh"
cat >"$tmp/export-mt-admin.sh" <<'MT'
#!/usr/bin/env bash
# Dispatch markers required by export-tenant compatibility checks.
validate_boundary() {
  tenant="$1"
  fd="${LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD:-}"
  lock_file="${LUNARWING_MT_LIFECYCLE_LOCK_DIR:?}/${tenant}.lock"
  [[ "$fd" =~ ^[0-9]+$ && -e "/proc/self/fd/$fd" \
    && "$(stat -Lc '%d:%i' "/proc/self/fd/$fd")" == "$(stat -c '%d:%i' "$lock_file")" ]] \
    || exit 90
  exec 8>"$lock_file"
  ! flock -x -n 8 || exit 91
}
case "${1:-}" in
  migration-export-boundary)
    tenant="$2"
    shift 2
    [[ ! -v KAWARIMI_PASS ]] || : >"${KAWARIMI_TEST_WORK:?}/passphrase-env-leak"
    lock_dir="$(dirname "$0")/export-lifecycle-locks"
    mkdir -p "$lock_dir"
    exec 9>"$lock_dir/${tenant}.lock"
    flock -x 9
    exec env LUNARWING_MT_EXPORT_BOUNDARY_TENANT="$tenant" \
      LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD=9 \
      LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT="$tenant" \
      LUNARWING_MT_LIFECYCLE_LOCK_DIR="$lock_dir" \
      "$(dirname "$0")/export-tenant.sh" "$tenant" "$@"
    ;;
  validate-migration-export-boundary) validate_boundary "$2"; exit 0 ;;
  stop-writers)
    : >"${KAWARIMI_TEST_WORK:?}/systemctl-called"
    exit 0
    ;;
  writers-active) exit 1 ;;
esac
exit 0
MT
chmod +x "$tmp/export-mt-admin.sh"
cat >"$tmp/export-bin/id" <<ID
#!/usr/bin/env bash
if [[ "\${1:-}" == "-u" && \$# -eq 1 ]]; then
  printf '0\\n'
elif [[ "\${1:-}" == "-u" && "\${2:-}" == alpha ]]; then
  printf '%s\\n' "$export_fixture_uid"
elif [[ "\${1:-}" == alpha ]]; then
  exit 0
else
  exec /usr/bin/id "\$@"
fi
ID
cat >"$tmp/export-bin/getent" <<GETENT
#!/usr/bin/env bash
printf 'alpha:x:%s:%s::%s/export-home/alpha:/bin/bash\n' \
  "$export_fixture_uid" "$export_fixture_gid" "$tmp"
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
exit 0
SYSTEMCTL
chmod +x "$tmp/export-bin/"*

set +e
missing_output="$(
  PATH="$tmp/export-bin:$PATH" \
  KAWARIMI_TEST_WORK="$tmp" \
  LUNARWING_MT_ADMIN="$tmp/export-mt-admin.sh" \
  LUNARWING_PORTS_REGISTRY="$tmp/export-ports.json" \
  KAWARIMI_PASS='' KAWARIMI_PASS_FILE='' KAWARIMI_PASS_FD='' \
  bash "$EXPORT_SRC" alpha --out-dir "$tmp/export-out" </dev/null 2>&1
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
chmod 0600 "$tmp/export-home/alpha/lunarwing/env/xmpp-bridge.env"
set +e
mismatch_output="$(
  PATH="$tmp/export-bin:$PATH" \
  KAWARIMI_TEST_WORK="$tmp" \
  LUNARWING_MT_ADMIN="$tmp/export-mt-admin.sh" \
  LUNARWING_PORTS_REGISTRY="$tmp/export-ports.json" \
  KAWARIMI_PASS='valid-export-passphrase' \
  bash "$EXPORT_SRC" alpha --out-dir "$tmp/export-out" </dev/null 2>&1
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
chmod 0600 "$tmp/export-home/alpha/lunarwing/env/lunarwing.env"
full_export_output="$(
  PATH="$tmp/export-bin:$PATH" \
  KAWARIMI_TEST_WORK="$tmp" \
  LUNARWING_MT_ADMIN="$tmp/export-mt-admin.sh" \
  LUNARWING_PORTS_REGISTRY="$tmp/export-ports.json" \
  KAWARIMI_PASS="$full_passphrase" \
  bash "$EXPORT_SRC" alpha --out-dir "$tmp/export-out" 2>&1
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
if [[ -e "$tmp/passphrase-env-leak" ]]; then
  fail "legacy KAWARIMI_PASS is removed before mt-admin boundary"
else
  pass "legacy KAWARIMI_PASS is removed before mt-admin boundary"
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
assert_not_contains "production export omits retired Nanocode model" "$roundtrip_output" "--nanocode-model"
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
assert_file_not_contains "encrypted manifest omits retired Nanocode model" "$manifest_path" "NANOCODE_MODEL="
assert_file_not_contains "encrypted manifest omits retired Nanocode base URL" "$manifest_path" "NANOCODE_BASE_URL="
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
