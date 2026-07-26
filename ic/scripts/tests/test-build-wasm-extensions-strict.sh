#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
script="$repo_root/ic/scripts/build-wasm-extensions.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

mkdir -p "$fixture/ic/scripts" "$fixture/ic/registry/tools" \
    "$fixture/ic/registry/channels" "$fixture/ic/tools-src/valid" \
    "$fixture/ic/channels-src/valid" "$fixture/bin"
cp "$script" "$fixture/ic/scripts/build-wasm-extensions.sh"
touch "$fixture/ic/tools-src/valid/tool.capabilities.json"
touch "$fixture/ic/channels-src/valid/channel.capabilities.json"

cat >"$fixture/ic/registry/tools/valid-tool.json" <<'JSON'
{"source":{"dir":"tools-src/valid","capabilities":"tool.capabilities.json","crate_name":"valid-tool"}}
JSON
cat >"$fixture/ic/registry/channels/valid-channel.json" <<'JSON'
{"source":{"dir":"channels-src/valid","capabilities":"channel.capabilities.json","crate_name":"valid-channel"}}
JSON

cat >"$fixture/bin/rustup" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == "target list --installed" ]]; then
    echo wasm32-wasip2
    exit 0
fi
exit 1
SH
cat >"$fixture/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
manifest=""
while (($#)); do
    if [[ "$1" == "--manifest-path" ]]; then
        manifest="$2"
        break
    fi
    shift
done
printf '%s\n' "$manifest" >>"$CARGO_LOG"
case "$manifest" in
    tools-src/valid/Cargo.toml)
        artifact=tools-src/valid/target/wasm32-wasip2/release/valid_tool.wasm
        ;;
    channels-src/valid/Cargo.toml)
        artifact=channels-src/valid/target/wasm32-wasip2/release/valid_channel.wasm
        ;;
    *) exit 2 ;;
esac
mkdir -p "$(dirname "$artifact")"
touch "$artifact"
SH
chmod +x "$fixture/bin/rustup" "$fixture/bin/cargo"

run_fixture() {
    CARGO_LOG="$fixture/cargo.log" PATH="$fixture/bin:$PATH" \
        bash "$fixture/ic/scripts/build-wasm-extensions.sh" "$@"
}

: >"$fixture/cargo.log"
run_fixture --strict --tools >/dev/null
grep -q '^tools-src/valid/Cargo.toml$' "$fixture/cargo.log"
if grep -q '^channels-src/' "$fixture/cargo.log"; then
    echo "FAIL: --tools built channel fixture" >&2
    exit 1
fi

: >"$fixture/cargo.log"
run_fixture --channels --strict >/dev/null
grep -q '^channels-src/valid/Cargo.toml$' "$fixture/cargo.log"
if grep -q '^tools-src/' "$fixture/cargo.log"; then
    echo "FAIL: --channels built tool fixture" >&2
    exit 1
fi

cat >"$fixture/ic/registry/tools/missing-capability.json" <<'JSON'
{"source":{"dir":"tools-src/valid","capabilities":"missing.capabilities.json","crate_name":"missing-capability"}}
JSON
if output=$(run_fixture --tools --strict 2>&1); then
    echo "FAIL: missing capability passed strict validation" >&2
    exit 1
fi
grep -q 'capabilities file missing: missing.capabilities.json' <<<"$output"

echo "build-wasm-extensions strict fixture tests passed"
