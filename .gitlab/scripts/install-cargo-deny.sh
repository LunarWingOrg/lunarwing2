#!/usr/bin/env bash
set -euo pipefail

readonly version="0.20.2"
readonly archive="cargo-deny-${version}-x86_64-unknown-linux-musl.tar.gz"
readonly checksum="9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"
readonly url="https://github.com/EmbarkStudios/cargo-deny/releases/download/${version}/${archive}"

repo_root=$(git rev-parse --show-toplevel)
install_dir="$repo_root/.ci-bin"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

if [[ $(uname -m) != "x86_64" ]]; then
    echo "ERROR: cargo-deny installer requires x86_64 runner" >&2
    exit 1
fi

curl --fail --location --silent --show-error "$url" --output "$temp_dir/$archive"
printf '%s  %s\n' "$checksum" "$temp_dir/$archive" | sha256sum --check
tar -xzf "$temp_dir/$archive" -C "$temp_dir" --strip-components=1
install -d "$install_dir"
install -m 0755 "$temp_dir/cargo-deny" "$install_dir/cargo-deny"
"$install_dir/cargo-deny" --version
