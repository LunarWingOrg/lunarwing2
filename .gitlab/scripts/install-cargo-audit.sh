#!/usr/bin/env bash
set -euo pipefail

readonly version="0.23.0"
readonly archive="cargo-audit-x86_64-unknown-linux-musl-v${version}.tgz"
readonly checksum="4f01c74ed84b2dffd0b1d37012a62cdf2a92c96e25bdf8eec7fcc0d4d3eb7811"
readonly url="https://github.com/rustsec/rustsec/releases/download/cargo-audit/v${version}/${archive}"

repo_root=$(git rev-parse --show-toplevel)
install_dir="$repo_root/.ci-bin"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

if [[ $(uname -m) != "x86_64" ]]; then
    echo "ERROR: cargo-audit installer requires x86_64 runner" >&2
    exit 1
fi

# Match install-cargo-deny.sh: retry transient download failures only, and
# verify the pinned archive outside the retry loop so wrong bytes fail at once.
for attempt in 1 2 3; do
    if curl --fail --location --silent --show-error "$url" --output "$temp_dir/$archive"; then
        break
    fi
    if (( attempt == 3 )); then
        echo "ERROR: cargo-audit $version download failed after 3 attempts; check runner DNS/HTTPS access to github.com" >&2
        exit 1
    fi
    echo "Retrying cargo-audit $version download ($attempt/3 failed)" >&2
    sleep "$((attempt * 5))"
done
printf '%s  %s\n' "$checksum" "$temp_dir/$archive" | sha256sum --check
tar -xzf "$temp_dir/$archive" -C "$temp_dir" --strip-components=1
install -d "$install_dir"
install -m 0755 "$temp_dir/cargo-audit" "$install_dir/cargo-audit"
"$install_dir/cargo-audit" --version
