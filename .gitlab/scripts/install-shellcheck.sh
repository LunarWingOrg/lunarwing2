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
readonly url="https://github.com/koalaman/shellcheck/releases/download/v${version}/${archive}"

repo_root=$(git rev-parse --show-toplevel)
install_dir="$repo_root/.ci-bin"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

if [[ $(uname -m) != "x86_64" ]]; then
    echo "ERROR: shellcheck installer requires x86_64 runner" >&2
    exit 1
fi

curl --fail --location --silent --show-error "$url" --output "$temp_dir/$archive"
printf '%s  %s\n' "$checksum" "$temp_dir/$archive" | sha256sum --check
tar -xJf "$temp_dir/$archive" -C "$temp_dir"
install -d "$install_dir"
install -m 0755 "$temp_dir/shellcheck-v${version}/shellcheck" "$install_dir/shellcheck"
"$install_dir/shellcheck" --version
