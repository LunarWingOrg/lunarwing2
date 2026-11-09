#!/usr/bin/env bash
# Hermetic WI#594 coverage for registry order, fallback, and outage behavior.
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$TEST_DIR/../../.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
check() {
    local label="$1"
    shift
    if "$@"; then pass "$label"; else fail "$label"; fi
}
output_has() { [[ "$output" == *"$1"* ]]; }
status_is() { [[ "$status" -eq "$1" ]]; }
status_nonzero() { [[ "$status" -ne 0 ]]; }

project="$fixture/project"
repo="$fixture/repo"
calls="$fixture/calls"
mkdir -p "$project" "$repo" "$fixture/bin" "$fixture/sccache-v0.17.0-x86_64-unknown-linux-musl" "$fixture/shellcheck-v0.11.0"

cat >"$fixture/sccache-v0.17.0-x86_64-unknown-linux-musl/sccache" <<'SH'
#!/usr/bin/env bash
printf 'sccache 0.17.0\n'
SH
cat >"$fixture/shellcheck-v0.11.0/shellcheck" <<'SH'
#!/usr/bin/env bash
printf 'ShellCheck 0.11.0\n'
SH
chmod +x "$fixture/sccache-v0.17.0-x86_64-unknown-linux-musl/sccache" "$fixture/shellcheck-v0.11.0/shellcheck"
tar -czf "$fixture/sccache.tar.gz" -C "$fixture" sccache-v0.17.0-x86_64-unknown-linux-musl
tar -cJf "$fixture/shellcheck.tar.xz" -C "$fixture" shellcheck-v0.11.0

cp "$REPO_ROOT/.gitlab/scripts/bootstrap-sccache.sh" "$fixture/bootstrap-sccache.sh"
cp "$REPO_ROOT/.gitlab/scripts/install-shellcheck.sh" "$fixture/install-shellcheck.sh"
sccache_archive_sum="$(sha256sum "$fixture/sccache.tar.gz" | cut -d' ' -f1)"
sccache_binary_sum="$(sha256sum "$fixture/sccache-v0.17.0-x86_64-unknown-linux-musl/sccache" | cut -d' ' -f1)"
shellcheck_archive_sum="$(sha256sum "$fixture/shellcheck.tar.xz" | cut -d' ' -f1)"
sed -i \
    -e "s/^SCCACHE_SHA256=\"[0-9a-f]\{64\}\"$/SCCACHE_SHA256=\"$sccache_archive_sum\"/" \
    -e "s/^SCCACHE_BIN_SHA256=\"[0-9a-f]\{64\}\"$/SCCACHE_BIN_SHA256=\"$sccache_binary_sum\"/" \
    "$fixture/bootstrap-sccache.sh"
sed -i "s/^readonly checksum=\"[0-9a-f]\{64\}\"$/readonly checksum=\"$shellcheck_archive_sum\"/" \
    "$fixture/install-shellcheck.sh"

cat >"$fixture/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
url="${!#}"
printf '%s\n' "$url" >>"$CALL_LOG"
if [[ "${FAIL_REGISTRY:-0}" == 1 && "$url" == *registry.invalid* ]]; then exit 22; fi
if [[ "${FAIL_GITHUB:-0}" == 1 && "$url" == *github.invalid* ]]; then exit 22; fi
output=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o|--output)
            output="$2"
            shift 2
            ;;
        *) shift ;;
    esac
done
[[ -n "$output" ]] || exit 2
if [[ "${POISON_REGISTRY:-0}" == 1 && "$url" == *registry.invalid* ]]; then
    printf 'corrupt download\n' >"$output"
elif [[ "$url" == *sccache* ]]; then
    cp "$FIXTURE_ROOT/sccache.tar.gz" "$output"
elif [[ "$url" == *shellcheck* ]]; then
    cp "$FIXTURE_ROOT/shellcheck.tar.xz" "$output"
else
    exit 2
fi
SH
cat >"$fixture/bin/git" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == 'rev-parse --show-toplevel' ]]; then
    printf '%s\n' "$FIXTURE_REPO"
    exit 0
fi
exit 2
SH
chmod +x "$fixture/bin/curl" "$fixture/bin/git"

export PATH="$fixture/bin:$PATH"
export FIXTURE_ROOT="$fixture" FIXTURE_REPO="$repo" CALL_LOG="$calls"
export CI_PROJECT_DIR="$project"
export SCCACHE_REGISTRY_URL="https://registry.invalid/sccache.tar.gz"
export SCCACHE_URL="https://github.invalid/sccache.tar.gz"
export SHELLCHECK_REGISTRY_URL="https://registry.invalid/shellcheck.tar.xz"
export SHELLCHECK_URL="https://github.invalid/shellcheck.tar.xz"
unset CI_JOB_TOKEN

reset_fixture() {
    rm -rf "$project" "$repo/.ci-bin"
    mkdir -p "$project"
    : >"$calls"
    unset FAIL_REGISTRY FAIL_GITHUB POISON_REGISTRY
    output=""
    status=0
}
run_script() {
    local script="$1"
    if output="$(bash "$script" 2>&1)"; then
        status=0
    else
        status=$?
    fi
}
check_calls() {
    local label="$1"
    local expected="$2"
    check "$label" [ "$(cat "$calls")" = "$expected" ]
}

reset_fixture
run_script "$fixture/bootstrap-sccache.sh"
check 'sccache registry install succeeds' status_is 0
check_calls 'sccache registry is the only source on success' "$SCCACHE_REGISTRY_URL"
check 'sccache registry binary is installed' test -x "$project/.ci-cache/bin/sccache"

reset_fixture
export FAIL_REGISTRY=1
run_script "$fixture/bootstrap-sccache.sh"
check 'sccache GitHub fallback succeeds' status_is 0
check_calls 'sccache tries registry then GitHub' "$SCCACHE_REGISTRY_URL
$SCCACHE_URL"
check 'sccache fallback reports the source change' output_has 'trying GitHub fallback'

reset_fixture
export FAIL_REGISTRY=1 FAIL_GITHUB=1
run_script "$fixture/bootstrap-sccache.sh"
check 'sccache total download outage is rejected by the installer' status_nonzero
check 'sccache outage leaves no compiler wrapper' test ! -e "$project/.ci-cache/bin/sccache"
export RUSTC_WRAPPER="$project/.ci-cache/bin/sccache" SCCACHE_DIR="$project/.ci-cache/sccache"
export SCCACHE_CACHE_SIZE=10G SCCACHE_TEST_ONLY=1
fallback_output="$(
    { bash "$fixture/bootstrap-sccache.sh" 2>/dev/null || source "$REPO_ROOT/.gitlab/scripts/disable-sccache.sh"; } 2>&1
    printf 'wrapper=%s dir=%s size=%s custom=%s\n' \
        "${RUSTC_WRAPPER-unset}" "${SCCACHE_DIR-unset}" "${SCCACHE_CACHE_SIZE-unset}" "${SCCACHE_TEST_ONLY-unset}"
)"
output="$fallback_output"
check 'Rust fallback clears the wrapper and every sccache variable' \
    output_has 'wrapper=unset dir=unset size=unset custom=unset'
check 'Rust fallback prints a loud warning' output_has 'WARNING: sccache is unavailable'

reset_fixture
export POISON_REGISTRY=1
run_script "$fixture/bootstrap-sccache.sh"
check 'sccache registry checksum failure is rejected' status_nonzero
check_calls 'sccache checksum failure does not hide behind another source' "$SCCACHE_REGISTRY_URL"
check 'sccache checksum failure leaves no executable' test ! -e "$project/.ci-cache/bin/sccache"

reset_fixture
run_script "$fixture/install-shellcheck.sh"
check 'ShellCheck registry install succeeds' status_is 0
check_calls 'ShellCheck registry is the only source on success' "$SHELLCHECK_REGISTRY_URL"
check 'ShellCheck registry binary is installed' test -x "$repo/.ci-bin/shellcheck"

reset_fixture
export FAIL_REGISTRY=1
run_script "$fixture/install-shellcheck.sh"
check 'ShellCheck GitHub fallback succeeds' status_is 0
check_calls 'ShellCheck tries registry then GitHub' "$SHELLCHECK_REGISTRY_URL
$SHELLCHECK_URL"

reset_fixture
export FAIL_REGISTRY=1 FAIL_GITHUB=1
run_script "$fixture/install-shellcheck.sh"
check 'ShellCheck total download outage remains fatal' status_nonzero
check 'ShellCheck outage names the registry source' output_has "$SHELLCHECK_REGISTRY_URL"
check 'ShellCheck outage says the lint tool is mandatory' output_has 'mandatory'
check 'ShellCheck outage never installs a distro substitute' test ! -e "$repo/.ci-bin/shellcheck"

printf 'CI tool download tests: %s failure(s)\n' "$failures"
[[ "$failures" -eq 0 ]]
