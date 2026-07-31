#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$TEST_DIR/../lunarwing-mt-admin.sh"
BUILD_SCRIPT="$TEST_DIR/../build-wasm-extensions.sh"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

mkdir -p \
  "$fixture/build/ic/scripts" \
  "$fixture/build/ic/registry/tools" \
  "$fixture/build/ic/registry/channels" \
  "$fixture/build/ic/tools-src/demo" \
  "$fixture/bin"
cp "$BUILD_SCRIPT" "$fixture/build/ic/scripts/build-wasm-extensions.sh"
cat >"$fixture/build/ic/registry/tools/demo-tool.json" <<'JSON'
{
  "source": {
    "dir": "tools-src/demo",
    "crate_name": "demo-tool"
  }
}
JSON
printf '[package]\nname = "demo-tool"\nversion = "0.1.0"\n' \
  >"$fixture/build/ic/tools-src/demo/Cargo.toml"

cat >"$fixture/bin/rustup" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == "target list --installed" ]]; then
  printf 'wasm32-wasip2 (installed)\n'
  exit 0
fi
exit 0
SH

cat >"$fixture/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$CARGO_CALLS"
[[ "${FAIL_BUILD:-false}" != "true" ]] || exit 1
manifest=""
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "--manifest-path" ]]; then
    manifest="$2"
    break
  fi
  shift
done
source_dir="${manifest%/Cargo.toml}"
mkdir -p "$source_dir/target/wasm32-wasip2/release"
printf 'fresh component\n' >"$source_dir/target/wasm32-wasip2/release/demo_tool.wasm"
SH
chmod +x "$fixture/bin/rustup" "$fixture/bin/cargo"

export CARGO_CALLS="$fixture/cargo-calls"
if PATH="$fixture/bin:$PATH" bash "$fixture/build/ic/scripts/build-wasm-extensions.sh" --tools >/dev/null; then
  pass 'WASM builder succeeds with direct Preview 2 artifact'
else
  fail 'WASM builder should succeed with expected artifact'
fi
if grep -Fq 'build --release --target wasm32-wasip2 --manifest-path tools-src/demo/Cargo.toml' "$CARGO_CALLS" \
  && ! grep -Fq 'component build' "$CARGO_CALLS"; then
  pass 'WASM builder uses cargo build for wasm32-wasip2'
else
  fail 'WASM builder used wrong Cargo command'
fi

artifact="$fixture/build/ic/tools-src/demo/target/wasm32-wasip2/release/demo_tool.wasm"
printf 'stale component\n' >"$artifact"
if FAIL_BUILD=true PATH="$fixture/bin:$PATH" \
  bash "$fixture/build/ic/scripts/build-wasm-extensions.sh" --tools >/dev/null 2>&1; then
  fail 'WASM builder should fail when Cargo fails'
else
  pass 'WASM builder propagates Cargo failure'
fi
if [[ -e "$artifact" ]]; then
  fail 'failed WASM build left stale source artifact installable'
else
  pass 'failed WASM build removes stale source artifact'
fi

mv "$fixture/build/ic/tools-src/demo" "$fixture/build/ic/tools-src/demo-missing"
if PATH="$fixture/bin:$PATH" \
  bash "$fixture/build/ic/scripts/build-wasm-extensions.sh" --tools >/dev/null 2>&1; then
  fail 'WASM builder should fail when registered source is missing'
else
  pass 'WASM builder rejects missing registered source'
fi
mv "$fixture/build/ic/tools-src/demo-missing" "$fixture/build/ic/tools-src/demo"

# shellcheck disable=SC1090
source "$ADMIN_SCRIPT"
mkdir -p "$fixture/tenant/repo/bridges/xmpp-bridge" "$fixture/tenant/env"
# shellcheck disable=SC2034
BUILD_LOCK="$fixture/build.lock"
tenant_repo() { printf '%s/tenant/repo\n' "$fixture"; }
tenant_env_dir() { printf '%s/tenant/env\n' "$fixture"; }
ensure_tenant_wasm_toolchain() { return 0; }
install_wasm_tenant() { : >"$fixture/installed"; }
flock() { return 0; }
say() { :; }
sudo() {
  if [[ "$*" == *'build-wasm-extensions.sh'* ]]; then
    return 1
  fi
  return 0
}

if output="$(build_tenant fixture true false false false false true 2>&1)"; then
  fail 'strict tenant build should fail after WASM build error'
else
  pass 'strict tenant build fails after WASM build error'
fi
if [[ "$output" == *'refusing to install stale artifacts'* ]] && [[ ! -e "$fixture/installed" ]]; then
  pass 'strict tenant build blocks stale WASM install'
else
  fail 'strict tenant build did not block stale WASM install'
fi
if build_tenant fixture true >/dev/null 2>&1 && [[ ! -e "$fixture/installed" ]]; then
  pass 'non-strict tenant build preserves installed WASM after build error'
else
  fail 'non-strict tenant build installed stale WASM after build error'
fi
if output="$(build_tenant fixture false false false false false true 2>&1)"; then
  fail 'strict WASM mode should require --with-wasm'
elif [[ "$output" == *'--require-wasm requires --with-wasm'* ]]; then
  pass 'strict WASM mode rejects missing --with-wasm'
else
  fail 'strict WASM mode returned the wrong validation error'
fi
# shellcheck disable=SC2016
if grep -Fq 'build_tenant "$name" "true" "false" "$with_opencode" "$with_kimi" "false" "true"' "$ADMIN_SCRIPT"; then
  pass 'upgrade-tenant requires successful WASM build'
else
  fail 'upgrade-tenant does not enable strict WASM build mode'
fi
# shellcheck disable=SC2016
if grep -Fq 'tenant_worker_enabled "$name" opencode' "$ADMIN_SCRIPT"; then
  pass 'upgrade-tenant rebuilds selected OpenCode worker'
else
  fail 'upgrade-tenant ignores persisted OpenCode worker selection'
fi
# shellcheck disable=SC2016
if grep -Fq -- '--require-wasm)' "$ADMIN_SCRIPT" \
  && grep -Fq '"$with_toolchains" "$require_wasm"' "$ADMIN_SCRIPT"; then
  pass 'build-tenant CLI forwards strict WASM mode'
else
  fail 'build-tenant CLI does not forward strict WASM mode'
fi

for wrapper in \
  upgrade-tenant.sh \
  upgrade-tenant-version.sh \
  upgrade-tenant-kageho.sh \
  upgrade-tenant-starforce.sh \
  upgrade-tenant-sunburst.sh; do
  if grep -Eq 'build-tenant.*--with-wasm.*--require-wasm' "$TEST_DIR/../$wrapper"; then
    pass "$wrapper requires a fresh WASM build"
  else
    fail "$wrapper allows stale WASM artifacts"
  fi
done

if grep -q -- '--with-nanocode' "$TEST_DIR/../upgrade-tenant.sh.old"; then
  fail 'executable old upgrade script still exposes retired Nanocode option'
else
  pass 'executable old upgrade script omits retired Nanocode option'
fi

if output="$(bash "$TEST_DIR/../upgrade-tenant.sh" fixture --skip-build 2>&1)"; then
  fail 'live upgrade should reject --skip-build'
elif [[ "$output" == *'--skip-build is unsafe for live upgrades'* ]]; then
  pass 'live upgrade cannot bypass fresh WASM build'
else
  fail 'live --skip-build returned the wrong validation error'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all WASM upgrade build tests passed\n'
