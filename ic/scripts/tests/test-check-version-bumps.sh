#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' EXIT

mkdir -p \
    "$fixture_root/.gitlab/scripts" \
    "$fixture_root/ic/scripts" \
    "$fixture_root/ic/tools-src/demo" \
    "$fixture_root/ic/registry/tools"
cp "$repo_root/ic/scripts/check-version-bumps.sh" "$fixture_root/ic/scripts/"
cp "$repo_root/.gitlab/scripts/version-policy.sh" "$fixture_root/.gitlab/scripts/"

git -C "$fixture_root" init -q
git -C "$fixture_root" config user.email "ci@example.invalid"
git -C "$fixture_root" config user.name "CI Test"

printf 'base\n' > "$fixture_root/ic/tools-src/demo/source.txt"
cat > "$fixture_root/ic/tools-src/demo/Cargo.toml" <<'TOML'
[package]
name = "demo-tool"
version = "1.0.0"
TOML
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

# A metadata-only alignment to an already-shipped registry release must fail
# through the exact CI wrapper, even when all current version fields agree.
base_sha=$(git -C "$fixture_root" rev-parse HEAD)
sed -i 's/version = "1.0.0"/version = "1.0.1"/' "$fixture_root/ic/tools-src/demo/Cargo.toml"
git -C "$fixture_root" add -- ic/tools-src/demo/Cargo.toml
git -C "$fixture_root" commit -qm "align metadata without registry bump"
before_sha=$(git -C "$fixture_root" rev-parse HEAD)
run_mr_policy() {
    (
        cd "$fixture_root"
        CI_MERGE_REQUEST_TARGET_BRANCH_NAME=aragents/kimil00p/v2.1.4.0 \
        CI_MERGE_REQUEST_DIFF_BASE_SHA="$base_sha" \
        CI_COMMIT_BEFORE_SHA="$before_sha" \
        CI_MERGE_REQUEST_LABELS="" \
            bash .gitlab/scripts/version-policy.sh
    )
}
if run_mr_policy > "$fixture_root/metadata-failure.log" 2>&1; then
    echo "expected metadata-only alignment without a registry bump to fail" >&2
    exit 1
fi
grep -Fq "Base commit: $base_sha" "$fixture_root/metadata-failure.log"
grep -Fq "Registry version: 1.0.1 -> 1.0.1" "$fixture_root/metadata-failure.log"
grep -Fq "registry/tools/demo-tool.json version was not bumped" "$fixture_root/metadata-failure.log"

sed -i 's/"version": "1.0.1"/"version": "1.0.2"/' "$fixture_root/ic/registry/tools/demo-tool.json"
sed -i 's/version = "1.0.1"/version = "1.0.2"/' "$fixture_root/ic/tools-src/demo/Cargo.toml"
git -C "$fixture_root" add -- ic/registry/tools/demo-tool.json ic/tools-src/demo/Cargo.toml
git -C "$fixture_root" commit -qm "advance corrected extension release"
before_sha=$(git -C "$fixture_root" rev-parse HEAD)
output=$(run_mr_policy)
grep -Fq "Base commit: $base_sha" <<< "$output"
grep -Fq "Registry version: 1.0.1 -> 1.0.2" <<< "$output"
grep -Fq "All version checks passed." <<< "$output"

echo "check-version-bumps tests passed"
