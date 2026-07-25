#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' EXIT

mkdir -p \
    "$fixture_root/ic/scripts" \
    "$fixture_root/ic/tools-src/demo" \
    "$fixture_root/ic/registry/tools"
cp "$repo_root/ic/scripts/check-version-bumps.sh" "$fixture_root/ic/scripts/"

git -C "$fixture_root" init -q
git -C "$fixture_root" config user.email "ci@example.invalid"
git -C "$fixture_root" config user.name "CI Test"

printf 'base\n' > "$fixture_root/ic/tools-src/demo/source.txt"
cat > "$fixture_root/ic/registry/tools/demo-tool.json" <<'JSON'
{
  "name": "demo-tool",
  "version": "1.0.0",
  "source": {
    "dir": "tools-src/demo"
  }
}
JSON
git -C "$fixture_root" add .
git -C "$fixture_root" commit -qm "base"
base_sha=$(git -C "$fixture_root" rev-parse HEAD)

printf 'changed\n' > "$fixture_root/ic/tools-src/demo/source.txt"
cat > "$fixture_root/ic/registry/tools/demo-tool.json" <<'JSON'
{
  "name": "demo-tool",
  "version": "1.0.1",
  "source": {
    "dir": "tools-src/demo"
  }
}
JSON
git -C "$fixture_root" add .
git -C "$fixture_root" commit -qm "bump extension"

output=$(BASE_SHA="$base_sha" bash "$fixture_root/ic/scripts/check-version-bumps.sh")
grep -Fq "Registry version: 1.0.0 -> 1.0.1" <<< "$output"

base_sha=$(git -C "$fixture_root" rev-parse HEAD)
printf 'changed again\n' > "$fixture_root/ic/tools-src/demo/source.txt"
git -C "$fixture_root" add .
git -C "$fixture_root" commit -qm "miss version bump"

if BASE_SHA="$base_sha" bash "$fixture_root/ic/scripts/check-version-bumps.sh" > "$fixture_root/failure.log" 2>&1; then
    echo "expected unchanged registry version to fail" >&2
    exit 1
fi
grep -Fq "registry/tools/demo-tool.json version was not bumped" "$fixture_root/failure.log"

echo "check-version-bumps tests passed"
