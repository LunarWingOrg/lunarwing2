#!/usr/bin/env bash
# Regression coverage for the shell-side DarkIRC migration boundary.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
IC_DIR="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
EXPORT_SRC="$IC_DIR/scripts/export-tenant.sh"
IMPORT_SRC="$IC_DIR/scripts/import-tenant.sh"

failures=0

fail() {
  printf '  FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

pass() {
  printf '  PASS: %s\n' "$1"
}

assert_file_contains() {
  local label="$1" file="$2" needle="$3"
  if grep -qF -- "$needle" "$file"; then
    pass "$label"
  else
    fail "$label"
    printf '        missing: %s\n' "$needle"
  fi
}

assert_file_not_matches() {
  local label="$1" file="$2" pattern="$3"
  if rg -n --pcre2 "$pattern" "$file" >/dev/null 2>&1; then
    fail "$label"
    rg -n --pcre2 "$pattern" "$file" || true
  else
    pass "$label"
  fi
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

echo "=== shell lifecycle and archive guards ==="
assert_file_not_matches \
  "export delegates lifecycle instead of calling init directly" \
  "$EXPORT_SRC" 'systemctl|rc-service'
assert_file_contains \
  "export uses mt-admin writer quiesce contract" \
  "$EXPORT_SRC" "stop-writers"
assert_file_contains \
  "export excludes DarkIRC from generic state archive" \
  "$EXPORT_SRC" "state/darkirc"
assert_file_contains \
  "import defensively excludes DarkIRC from state restore" \
  "$IMPORT_SRC" "state/darkirc"
assert_file_contains \
  "export uses the structured contact manifest name" \
  "$EXPORT_SRC" "darkirc-contacts-v1.json"
assert_file_contains \
  "import stages the structured contact manifest through mt-admin" \
  "$IMPORT_SRC" "stage-migration"
assert_file_not_matches \
  "export never copies DarkIRC manifest through WORK/tmp" \
  "$EXPORT_SRC" 'WORK[^\n]*darkirc-contacts-v1|cp[^\n]*(/tmp|\$WORK)[^\n]*DARKIRC'
assert_file_contains \
  "export pins the manifest in the protected migration directory" \
  "$EXPORT_SRC" "DARKIRC_PIN_DIR"
assert_file_contains \
  "import streams the manifest from the outer bundle" \
  "$IMPORT_SRC" "tar -xOf \"\$BUNDLE\""
assert_file_contains \
  "export validates the credential output directory" \
  "$EXPORT_SRC" "validate_export_output_dir"
assert_file_contains \
  "import pins the bundle before validation and extraction" \
  "$IMPORT_SRC" "BUNDLE_SOURCE"

echo "=== unsafe archive rejection happens before mt-admin mutation ==="
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/scripts" "$tmp/bin" "$tmp/bundle/state/darkirc"
cp "$IMPORT_SRC" "$tmp/scripts/import-tenant.sh"
chmod +x "$tmp/scripts/import-tenant.sh"

cat >"$tmp/scripts/lunarwing-mt-admin.sh" <<'MT'
#!/usr/bin/env bash
case "${1:-}" in
  restore-tenant) ;;
  owner-scopes) ;;
esac
printf '%s\n' "$*" >> "${DARKIRC_IMPORT_TEST_WORK:?}/mt.log"
exit 99
MT
chmod +x "$tmp/scripts/lunarwing-mt-admin.sh"
cat >"$tmp/scripts/lunarwing-weechat-preflight.sh" <<'PREFLIGHT'
#!/usr/bin/env bash
exit 0
PREFLIGHT
chmod +x "$tmp/scripts/lunarwing-weechat-preflight.sh"

cat >"$tmp/bin/id" <<'ID'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" ]]; then
  printf '0\n'
else
  exec /usr/bin/id "$@"
fi
ID
chmod +x "$tmp/bin/id"

cat >"$tmp/bin/jq" <<'JQ'
#!/usr/bin/env bash
exec /usr/bin/jq "$@"
JQ
chmod +x "$tmp/bin/jq"

cat >"$tmp/bundle/meta.txt" <<'META'
tenant=alpha
source_version=test
db_backend=postgres
darkirc_enabled=true
darkirc_scope_id=00112233445566778899aabbccddeeff
source_quiesced=true
META
printf 'not-a-real-pgdump\n' >"$tmp/bundle/db.dump"
cat >"$tmp/bundle/manifest-lunarwing.env" <<'ENV'
SECRETS_MASTER_KEY=test-master-key
ENV
printf 'secret contact material\n' >"$tmp/bundle/state/darkirc/darkirc_config.toml"
tar cf "$tmp/unsafe.tar" -C "$tmp/bundle" .
printf '{"tenants":{}}\n' >"$tmp/ports.json"

if PATH="$tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
   DARKIRC_IMPORT_TEST_WORK="$tmp" \
   bash "$tmp/scripts/import-tenant.sh" "$tmp/unsafe.tar" --yes >"$tmp/import.out" 2>&1; then
  fail "import rejects archive containing state/darkirc"
else
  pass "import rejects archive containing state/darkirc"
fi
if [[ ! -s "$tmp/mt.log" ]]; then
  pass "unsafe archive rejection occurs before mt-admin mutation"
else
  fail "unsafe archive rejection occurs before mt-admin mutation"
  cat "$tmp/mt.log"
fi

mkdir -p "$tmp/aliased-duplicate-src"
cp "$tmp/bundle/meta.txt" "$tmp/bundle/db.dump" \
  "$tmp/bundle/manifest-lunarwing.env" "$tmp/aliased-duplicate-src/"
tar cf "$tmp/aliased-duplicate.tar" -C "$tmp/aliased-duplicate-src" .
tar --append --file "$tmp/aliased-duplicate.tar" \
  --transform='s#^\./#././#' -C "$tmp/aliased-duplicate-src" ./meta.txt
: >"$tmp/mt.log"
if PATH="$tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
   DARKIRC_IMPORT_TEST_WORK="$tmp" \
   bash "$tmp/scripts/import-tenant.sh" "$tmp/aliased-duplicate.tar" --yes \
     >"$tmp/aliased-duplicate.out" 2>&1; then
  fail "import rejects canonically duplicate bundle paths"
elif ! grep -Fq 'duplicate path' "$tmp/aliased-duplicate.out"; then
  fail "import rejects canonically duplicate bundle paths"
  cat "$tmp/aliased-duplicate.out"
elif [[ -s "$tmp/mt.log" ]]; then
  fail "canonical duplicate rejection occurs before mt-admin mutation"
  cat "$tmp/mt.log"
else
  pass "import rejects canonically duplicate bundle paths"
  pass "canonical duplicate rejection occurs before mt-admin mutation"
fi

make_nested_bundle() {
  local root="$1" archive_source="$2"
  mkdir -p "$root"
  cat >"$root/meta.txt" <<'META'
tenant=alpha
source_version=test
db_backend=postgres
darkirc_enabled=false
darkirc_migration=false
source_quiesced=true
META
  cp "$tmp/bundle/db.dump" "$root/db.dump"
  cp "$tmp/bundle/manifest-lunarwing.env" "$root/manifest-lunarwing.env"
  tar czf "$root/state.tar.gz" -C "$archive_source" .
  tar cf "$root.tar" -C "$root" .
}

assert_import_rejected_before_mutation() {
  local label="$1" bundle="$2" output="$3"
  : >"$tmp/mt.log"
  if PATH="$tmp/bin:$PATH" \
     LUNARWING_PORTS_REGISTRY="$tmp/ports.json" \
     DARKIRC_IMPORT_TEST_WORK="$tmp" \
     bash "$tmp/scripts/import-tenant.sh" "$bundle" --yes >"$output" 2>&1; then
    fail "$label"
  elif [[ -s "$tmp/mt.log" ]]; then
    fail "$label before mt-admin mutation"
    cat "$tmp/mt.log"
  else
    pass "$label"
    pass "$label before mt-admin mutation"
  fi
}

mkdir -p "$tmp/sibling-state/env"
printf 'overwrite\n' >"$tmp/sibling-state/env/lunarwing.env"
make_nested_bundle "$tmp/sibling-bundle" "$tmp/sibling-state"
assert_import_rejected_before_mutation \
  "import rejects state archive members outside state/" \
  "$tmp/sibling-bundle.tar" "$tmp/sibling.out"

mkdir -p "$tmp/special-state/state"
mkfifo "$tmp/special-state/state/blocked.fifo"
make_nested_bundle "$tmp/special-bundle" "$tmp/special-state"
assert_import_rejected_before_mutation \
  "import rejects non-regular state archive members" \
  "$tmp/special-bundle.tar" "$tmp/special.out"

echo "=== dotenv manifest allowlist guards ==="
mkdir -p "$tmp/invalid-manifest"
cat >"$tmp/invalid-manifest/meta.txt" <<'META'
tenant=alpha
source_version=test
db_backend=postgres
darkirc_enabled=false
darkirc_migration=false
source_quiesced=true
META
cp "$tmp/bundle/db.dump" "$tmp/invalid-manifest/db.dump"
cat >"$tmp/invalid-manifest/manifest-lunarwing.env" <<'ENV'
SECRETS_MASTER_KEY=test-master-key
UNEXPECTED_IMPORT_KEY=must-be-rejected
ENV
tar cf "$tmp/invalid-manifest.tar" -C "$tmp/invalid-manifest" .
assert_import_rejected_before_mutation \
  "import rejects dotenv keys outside the carried allowlist" \
  "$tmp/invalid-manifest.tar" "$tmp/invalid-manifest.out"

echo "=== export excludes generic DarkIRC state and blocks unsettled state ==="
export_tmp="$(mktemp -d)"
mkdir -p "$export_tmp/bin" "$export_tmp/scripts" \
  "$export_tmp/home/alpha/lunarwing/env" \
  "$export_tmp/home/alpha/lunarwing/state/darkirc" \
  "$export_tmp/home/alpha/lunarwing/state/xmpp"
cp "$EXPORT_SRC" "$export_tmp/scripts/export-tenant.sh"
chmod +x "$export_tmp/scripts/export-tenant.sh"

cat >"$export_tmp/scripts/lunarwing-mt-admin.sh" <<'MT'
#!/usr/bin/env bash
case "${1:-}" in
  stop-writers) exit 0 ;;
  writers-active) exit 1 ;;
  *) exit 0 ;;
esac
MT
chmod +x "$export_tmp/scripts/lunarwing-mt-admin.sh"
printf '{"tenants":{"alpha":{"enable_darkirc":false}}}\n' >"$export_tmp/ports.json"
cat >"$export_tmp/home/alpha/lunarwing/env/lunarwing.env" <<'ENV'
DATABASE_BACKEND=postgres
DATABASE_URL=postgres://lunarwing@localhost/lunarwing
SECRETS_MASTER_KEY=test-master-key
ENV
chmod 0600 "$export_tmp/home/alpha/lunarwing/env/lunarwing.env"
chmod 0700 "$export_tmp/home/alpha/lunarwing/state/darkirc"
printf 'safe omemo state\n' >"$export_tmp/home/alpha/lunarwing/state/xmpp/session.db"

cat >"$export_tmp/bin/id" <<'ID'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" && $# -eq 1 ]]; then printf '0\n'; exit 0; fi
if [[ "${1:-}" == "-u" && "${2:-}" == alpha ]]; then printf '1000\n'; exit 0; fi
if [[ "${1:-}" == "alpha" ]]; then exit 0; fi
exec /usr/bin/id "$@"
ID
cat >"$export_tmp/bin/getent" <<GETENT
#!/usr/bin/env bash
if [[ "\${1:-}" == passwd && "\${2:-}" == alpha ]]; then
  printf 'alpha:x:1000:1000::%s/home/alpha:/bin/bash\n' "$export_tmp"
  exit 0
fi
exec /usr/bin/getent "$@"
GETENT
cat >"$export_tmp/bin/sudo" <<'SUDO'
#!/usr/bin/env bash
if [[ "${1:-}" == -u ]]; then shift 2; fi
if [[ "${1:-}" == env ]]; then
  shift
  while [[ "${1:-}" == *=* ]]; do shift; done
fi
exec "$@"
SUDO
cat >"$export_tmp/bin/git" <<'GIT'
#!/usr/bin/env bash
printf 'test-source\n'
GIT
cat >"$export_tmp/bin/podman" <<'PODMAN'
#!/usr/bin/env bash
case "${1:-}" in
  inspect)
    if [[ "${2:-}" == -f ]]; then printf 'true\n'; else exit 0; fi
    ;;
  exec) printf 'PGDMP-test-dump\n' ;;
  *) exit 0 ;;
esac
PODMAN
chmod +x "$export_tmp/bin"/*

export_out="$export_tmp/out"
mkdir -p "$export_out"
chmod 0700 "$export_out"

# Root must not follow a tenant-controlled final env symlink while assembling a
# credential archive. Reject it before lifecycle commands can run.
mv "$export_tmp/home/alpha/lunarwing/env/lunarwing.env" \
  "$export_tmp/home/alpha/lunarwing/env/lunarwing.env.real"
ln -s "$export_tmp/home/alpha/lunarwing/env/lunarwing.env.real" \
  "$export_tmp/home/alpha/lunarwing/env/lunarwing.env"
if PATH="$export_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$export_tmp/ports.json" \
   LUNARWING_CONTAINER_RUNTIME=podman \
   LUNARWING_MIGRATE_DIR="$export_out" \
   bash "$export_tmp/scripts/export-tenant.sh" alpha --no-quiesce \
     >"$export_tmp/export-symlink.out" 2>&1; then
  fail "export rejects a symlinked tenant env"
else
  pass "export rejects a symlinked tenant env"
fi
rm "$export_tmp/home/alpha/lunarwing/env/lunarwing.env"
mv "$export_tmp/home/alpha/lunarwing/env/lunarwing.env.real" \
  "$export_tmp/home/alpha/lunarwing/env/lunarwing.env"

mv "$export_out" "$export_out.real"
ln -s "$export_out.real" "$export_out"
if PATH="$export_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$export_tmp/ports.json" \
   LUNARWING_CONTAINER_RUNTIME=podman \
   LUNARWING_MIGRATE_DIR="$export_out" \
   bash "$export_tmp/scripts/export-tenant.sh" alpha --no-quiesce \
     >"$export_tmp/export-output-symlink.out" 2>&1; then
  fail "export rejects a symlinked output directory"
else
  pass "export rejects a symlinked output directory"
fi
rm "$export_out"
mv "$export_out.real" "$export_out"

if PATH="$export_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$export_tmp/ports.json" \
   LUNARWING_CONTAINER_RUNTIME=podman \
   LUNARWING_MIGRATE_DIR="$export_out" \
   bash "$export_tmp/scripts/export-tenant.sh" alpha --no-quiesce >"$export_tmp/export.out" 2>&1; then
  bundle="$(find "$export_out" -type f -name 'alpha-migrate-*.tar' -print -quit)"
  if [[ -n "$bundle" ]] && ! tar -tf "$bundle" | grep -qE '(^|/)state/darkirc(/|$)'; then
    pass "export bundle omits state/darkirc"
  else
    fail "export bundle omits state/darkirc"
  fi
  if [[ -n "$bundle" ]] && tar -xOf "$bundle" state.tar.gz 2>/dev/null | tar -tzf - 2>/dev/null | grep -qE '(^|/)state/darkirc(/|$)'; then
    fail "generic state archive contains state/darkirc"
  else
    pass "generic state archive omits state/darkirc"
  fi
else
  fail "export fixture completed"
  cat "$export_tmp/export.out"
fi

# Recreate the export fixture with a pending transaction. The preflight must
# abort before invoking stop-writers or creating an output bundle.
mkdir -p "$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/pending"
printf 'pending-secret\n' >"$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/pending/offer.json"
rm -f "$export_tmp/mt.log"
if PATH="$export_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$export_tmp/ports.json" \
   LUNARWING_CONTAINER_RUNTIME=podman \
   LUNARWING_MIGRATE_DIR="$export_out" \
   bash "$export_tmp/scripts/export-tenant.sh" alpha --no-quiesce >"$export_tmp/export-pending.out" 2>&1; then
  fail "export rejects pending DarkIRC state"
else
  pass "export rejects pending DarkIRC state"
fi
if [[ ! -e "$export_tmp/mt.log" ]]; then
  pass "pending-state rejection occurs before lifecycle mutation"
else
  fail "pending-state rejection occurs before lifecycle mutation"
fi

rm -rf "$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/pending"
mkdir -p "$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange"
printf 'staged-secret-state\n' >"$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/darkirc-contacts-v1.staged.json"
rm -f "$export_tmp/mt.log"
if PATH="$export_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$export_tmp/ports.json" \
   LUNARWING_CONTAINER_RUNTIME=podman \
   LUNARWING_MIGRATE_DIR="$export_out" \
   bash "$export_tmp/scripts/export-tenant.sh" alpha --no-quiesce >"$export_tmp/export-staged.out" 2>&1; then
  fail "export rejects staged DarkIRC migration state"
else
  pass "export rejects staged DarkIRC migration state"
fi
if [[ ! -e "$export_tmp/mt.log" ]]; then
  pass "staged-state rejection occurs before lifecycle mutation"
else
  fail "staged-state rejection occurs before lifecycle mutation"
fi
rm -f "$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/darkirc-contacts-v1.staged.json"

printf 'interrupted-export\n' \
  >"$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/darkirc-contacts-v1.json.next"
rm -f "$export_tmp/mt.log"
if PATH="$export_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$export_tmp/ports.json" \
   LUNARWING_CONTAINER_RUNTIME=podman \
   LUNARWING_MIGRATE_DIR="$export_out" \
   bash "$export_tmp/scripts/export-tenant.sh" alpha --no-quiesce \
     >"$export_tmp/export-candidate.out" 2>&1; then
  fail "export rejects interrupted DarkIRC migration export"
else
  pass "export rejects interrupted DarkIRC migration export"
fi
if [[ ! -e "$export_tmp/mt.log" ]]; then
  pass "interrupted-export rejection occurs before lifecycle mutation"
else
  fail "interrupted-export rejection occurs before lifecycle mutation"
fi
rm -f "$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/darkirc-contacts-v1.json.next"

cat >"$export_tmp/home/alpha/lunarwing/state/darkirc/key-exchange/ledger.json" <<'LEDGER'
{
  "schema": "lunarwing.darkirc-ledger/v1",
  "scope_id": "00112233445566778899aabbccddeeff",
  "contacts": {"alice": {"state": "VerificationOverdue"}}
}
LEDGER
rm -f "$export_tmp/mt.log"
if PATH="$export_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$export_tmp/ports.json" \
   LUNARWING_CONTAINER_RUNTIME=podman \
   LUNARWING_MIGRATE_DIR="$export_out" \
   bash "$export_tmp/scripts/export-tenant.sh" alpha --no-quiesce >"$export_tmp/export-ledger.out" 2>&1; then
  fail "export rejects unresolved DarkIRC ledger state"
else
  pass "export rejects unresolved DarkIRC ledger state"
fi
if [[ ! -e "$export_tmp/mt.log" ]]; then
  pass "ledger rejection occurs before lifecycle mutation"
else
  fail "ledger rejection occurs before lifecycle mutation"
fi
rm -rf "$export_tmp"

echo "=== structured DarkIRC import is contact-only and same-scope ==="
structured_tmp="$(mktemp -d)"
mkdir -p "$structured_tmp/scripts" "$structured_tmp/bin" "$structured_tmp/home/alpha/lunarwing/env"
cp "$IMPORT_SRC" "$structured_tmp/scripts/import-tenant.sh"
chmod +x "$structured_tmp/scripts/import-tenant.sh"
cat >"$structured_tmp/scripts/lunarwing-mt-admin.sh" <<'MT'
#!/usr/bin/env bash
# Contract markers intentionally mirror the real dispatch names.
case "${1:-}" in
  restore-tenant) ;;
  owner-scopes) ;;
  darkirc-scope-id) ;;
  stage-migration) ;;
  import-migration) ;;
esac
printf '%s\n' "$*" >> "${STRUCTURED_TEST_WORK:?}/mt.log"
if [[ "${1:-}" == darkirc-contact && "${2:-}" == validate-migration ]]; then
  : >"${STRUCTURED_TEST_WORK:?}/helper-install-attempted"
  cat >/dev/null
  printf '{"scope_id":"00112233445566778899aabbccddeeff"}\n'
  exit 0
fi
case "${1:-}" in
  restore-tenant|owner-scopes|darkirc-contact) exit 0 ;;
  *) exit 0 ;;
esac
MT
cat >"$structured_tmp/scripts/lunarwing-weechat-preflight.sh" <<'PREFLIGHT'
#!/usr/bin/env bash
exit 0
PREFLIGHT
chmod +x "$structured_tmp/scripts/lunarwing-mt-admin.sh" "$structured_tmp/scripts/lunarwing-weechat-preflight.sh"
cat >"$structured_tmp/bin/id" <<'ID'
#!/usr/bin/env bash
if [[ "${1:-}" == -u ]]; then printf '0\n'; else exec /usr/bin/id "$@"; fi
ID
chmod +x "$structured_tmp/bin/id"
cat >"$structured_tmp/bin/getent" <<GETENT
#!/usr/bin/env bash
if [[ "\${1:-}" == passwd && "\${2:-}" == alpha ]]; then
  printf 'alpha:x:1000:1000::%s/home/alpha:/bin/bash\n' "$structured_tmp"
else
  exit 2
fi
GETENT
chmod +x "$structured_tmp/bin/getent"
printf '{"tenants":{}}\n' >"$structured_tmp/ports.json"
cat >"$structured_tmp/bundle-src-meta" <<'META'
tenant=alpha
source_version=test
db_backend=postgres
darkirc_enabled=true
darkirc_scope_id=00112233445566778899aabbccddeeff
darkirc_migration=true
source_quiesced=true
META
printf 'PGDMP-test\n' >"$structured_tmp/bundle-src-db.dump"
printf 'SECRETS_MASTER_KEY=test-master-key\n' >"$structured_tmp/bundle-src-env"
cat >"$structured_tmp/bundle-src-manifest" <<'JSON'
{
  "schema": "darkirc-contacts-v1",
  "scope_id": "00112233445566778899aabbccddeeff",
  "generator_profile": "legacy-unmanaged",
  "contacts_toml": "[contact.\"alice\"]\ndm_chacha_public = \"peer\"\nmy_dm_chacha_secret = \"private\"\n",
  "contacts_hash": "sha256:test",
  "ledger": {"schema":"lunarwing.darkirc-ledger/v1","scope_id":"00112233445566778899aabbccddeeff","contacts":{}},
  "ledger_hash": "sha256:test"
}
JSON
chmod 600 "$structured_tmp/bundle-src-manifest"
mkdir -p "$structured_tmp/bundle-src"
mv "$structured_tmp/bundle-src-meta" "$structured_tmp/bundle-src/meta.txt"
mv "$structured_tmp/bundle-src-db.dump" "$structured_tmp/bundle-src/db.dump"
mv "$structured_tmp/bundle-src-env" "$structured_tmp/bundle-src/manifest-lunarwing.env"
mv "$structured_tmp/bundle-src-manifest" "$structured_tmp/bundle-src/darkirc-contacts-v1.json"
tar cf "$structured_tmp/structured.tar" -C "$structured_tmp/bundle-src" .

structured_output="$(
  PATH="$structured_tmp/bin:$PATH" \
  LUNARWING_PORTS_REGISTRY="$structured_tmp/ports.json" \
  STRUCTURED_TEST_WORK="$structured_tmp" \
  bash "$structured_tmp/scripts/import-tenant.sh" "$structured_tmp/structured.tar" --dry-run --yes --old-stopped 2>&1
)" || {
  printf '%s\n' "$structured_output"
  fail "structured DarkIRC import dry-run succeeds"
}
assert_contains "structured import preserves source scope" "$structured_output" "--darkirc-scope-id 00112233445566778899aabbccddeeff"
assert_contains "structured import stages manifest via stdin" "$structured_output" "stage-migration alpha --in - < darkirc-contacts-v1.json"
assert_contains "structured dry-run reports deferred typed validation" "$structured_output" "would typed-validate darkirc-contacts-v1.json"
if [[ -e "$structured_tmp/helper-install-attempted" ]]; then
  fail "structured import dry-run does not install or build the DarkIRC helper"
else
  pass "structured import dry-run does not install or build the DarkIRC helper"
fi

rm -f "$structured_tmp/mt.log"
if PATH="$structured_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$structured_tmp/ports.json" \
   STRUCTURED_TEST_WORK="$structured_tmp" \
   bash "$structured_tmp/scripts/import-tenant.sh" "$structured_tmp/structured.tar" --dry-run --yes >"$structured_tmp/not-stopped.out" 2>&1; then
  fail "DarkIRC scope import requires --old-stopped"
else
  pass "DarkIRC scope import requires --old-stopped"
fi
if [[ ! -e "$structured_tmp/mt.log" ]]; then
  pass "old-source gate occurs before mt-admin mutation"
else
  fail "old-source gate occurs before mt-admin mutation"
fi

rm -f "$structured_tmp/mt.log"
if PATH="$structured_tmp/bin:$PATH" \
   LUNARWING_PORTS_REGISTRY="$structured_tmp/ports.json" \
   STRUCTURED_TEST_WORK="$structured_tmp" \
   bash "$structured_tmp/scripts/import-tenant.sh" "$structured_tmp/structured.tar" --dry-run --yes --name beta >"$structured_tmp/clone.out" 2>&1; then
  fail "structured DarkIRC clone is rejected"
else
  pass "structured DarkIRC clone is rejected"
fi
if [[ ! -e "$structured_tmp/mt.log" ]]; then
  pass "structured DarkIRC clone rejection occurs before mt-admin mutation"
else
  fail "structured DarkIRC clone rejection occurs before mt-admin mutation"
fi
rm -rf "$structured_tmp"

echo "=== result ==="
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
else
  echo "$failures TEST(S) FAILED"
fi
exit "$((failures > 0))"
