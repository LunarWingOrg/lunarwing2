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

# Transient release-asset download flakes (GitHub egress on the runner) get
# the same three-attempt treatment as the rustup component installs in
# ensure-rust-components.sh. The checksum stays outside the retry loop: a
# served-wrong-bytes response is an integrity failure, not a flake, and must
# fail immediately.
for attempt in 1 2 3; do
    if curl --fail --location --silent --show-error "$url" --output "$temp_dir/$archive"; then
        break
    fi
    if (( attempt == 3 )); then
        echo "ERROR: cargo-deny $version download failed after 3 attempts; check runner DNS/HTTPS access to github.com" >&2
        exit 1
    fi
    echo "Retrying cargo-deny $version download ($attempt/3 failed)" >&2
    sleep "$((attempt * 5))"
done
printf '%s  %s\n' "$checksum" "$temp_dir/$archive" | sha256sum --check
tar -xzf "$temp_dir/$archive" -C "$temp_dir" --strip-components=1
install -d "$install_dir"
install -m 0755 "$temp_dir/cargo-deny" "$install_dir/cargo-deny"
"$install_dir/cargo-deny" --version
