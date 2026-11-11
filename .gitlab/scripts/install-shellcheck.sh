#!/usr/bin/env bash
set -euo pipefail

# ShellCheck 0.11.0 pinned installer (MR !988 feedback). Debian bookworm's apt
# carries 0.9.0, whose SC2218 (function use-before-define) false-positives on
# shell test harnesses that source library functions before use — 0.11.0
# rewrote that check ("SC2218 about function use-before-define is now more
# accurate"). The project floor is 0.11.0 from here on: anything below is not
# acceptable, so CI installs this checksum-pinned build instead of the distro
# package.

readonly version="0.11.0"
readonly archive="shellcheck-v${version}.linux.x86_64.tar.xz"
readonly checksum="8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198"
readonly url="${SHELLCHECK_URL:-https://github.com/koalaman/shellcheck/releases/download/v${version}/${archive}}"
if [[ -n "${SHELLCHECK_REGISTRY_URL:-}" ]]; then
    registry_url="$SHELLCHECK_REGISTRY_URL"
else
    registry_url="${CI_API_V4_URL:-https://git.lunarwing.org/api/v4}/projects/${CI_PROJECT_ID:-35}/packages/generic/lunarwing-ci-tools/${version}/${archive}"
fi
readonly registry_url

repo_root=$(git rev-parse --show-toplevel)
install_dir="$repo_root/.ci-bin"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

if [[ $(uname -m) != "x86_64" ]]; then
    echo "ERROR: shellcheck installer requires x86_64 runner" >&2
    exit 1
fi

for tool in curl sha256sum xz; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "ERROR: shellcheck installer requires '$tool' (CI: xz-utils via bootstrap-shell.sh)" >&2
        exit 1
    fi
done

download() {
    local source_url="$1"
    rm -f -- "$temp_dir/$archive"
    # Bound every attempt so a source that drops packets fails over quickly
    # (see bootstrap-sccache.sh).
    curl --fail --location --silent --show-error \
        --connect-timeout 15 --max-time 300 \
        --retry 5 --retry-delay 5 --retry-connrefused \
        --output "$temp_dir/$archive" "$source_url"
}

download_source="$registry_url"
if ! download "$registry_url"; then
    echo "WARNING: ShellCheck project registry download failed; trying GitHub fallback" >&2
    download_source="$url"
    if ! download "$url"; then
        echo "ERROR: ShellCheck $version is mandatory; downloads failed from project registry ($registry_url) and GitHub ($url)" >&2
        exit 1
    fi
fi
if ! printf '%s  %s\n' "$checksum" "$temp_dir/$archive" | sha256sum --check --status; then
    echo "ERROR: ShellCheck archive checksum mismatch from $download_source" >&2
    exit 1
fi
tar -xJf "$temp_dir/$archive" -C "$temp_dir"
install -d "$install_dir"
install -m 0755 "$temp_dir/shellcheck-v${version}/shellcheck" "$install_dir/shellcheck"
"$install_dir/shellcheck" --version
