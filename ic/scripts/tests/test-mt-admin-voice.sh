#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ROOT="$(mktemp -d "${TMPDIR:-/tmp}/mt-admin-voice-test.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT
pass=0; fail=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s (%s)\n' "$1" "$2"; fail=$((fail + 1)); }
assert_eq() { [[ "$1" == "$2" ]] && ok "$3" || bad "$3" "got '$1', want '$2'"; }

registry="$ROOT/ports.json"
cat >"$registry" <<'JSON'
{"version":14,"range":{"start":10000,"end":19999},"tenants":{"alpha":{"base_port":10000,"ports":{"gateway":10000},"extended_ports":{"vision_health":20006},"kimi_ports":{"kimi_wss":30000}},"beta":{"base_port":10020,"ports":{"gateway":10020},"extended_ports":{"vision_health":20026},"kimi_ports":{"kimi_wss":30020}}}}
JSON
PORTS_REGISTRY="$registry" bash "$SCRIPTS_DIR/migrate-ports-v15.sh" >/dev/null
assert_eq "$(jq -r '.version' "$registry")" 15 "migration bumps registry version"
assert_eq "$(jq -r '.tenants.alpha.voice_ports.voice_service' "$registry")" 40000 "alpha voice API port mapped"
assert_eq "$(jq -r '.tenants.alpha.voice_ports.voice_health' "$registry")" 40001 "alpha voice health port mapped"
assert_eq "$(jq -r '.tenants.beta.voice_ports.voice_service' "$registry")" 40020 "tenant spacing preserved"
assert_eq "$(jq -r '.tenants.alpha.enable_voice' "$registry")" false "existing tenants default disabled"
assert_eq "$(jq -r '.tenants.alpha.extended_ports.vision_health' "$registry")" 20006 "existing ports unchanged"
PORTS_REGISTRY="$registry" bash "$SCRIPTS_DIR/migrate-ports-v15.sh" >/dev/null
assert_eq "$(jq -r '.tenants.alpha.voice_ports.voice_service' "$registry")" 40000 "migration rerun is idempotent"

# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPTS_DIR/lunarwing-mt-admin.sh"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
PORTS_REGISTRY="$registry"
env_dir="$ROOT/env"
mkdir -p "$env_dir"
cat >"$env_dir/voice.env" <<'ENV'
VOICE_ENABLED=false
VOICE_AUTH_TOKEN=stable-token
ENV
cat >"$env_dir/lunarwing.env" <<'ENV'
TRANSCRIPTION_ENABLED=true
TRANSCRIPTION_PROVIDER=openai
TRANSCRIPTION_MODEL=whisper-1
PRESERVE_ME=yes
ENV
chmod 0600 "$env_dir/voice.env" "$env_dir/lunarwing.env"
# Distinct name: mt-admin locals named env_dir would shadow the test global
# under bash dynamic scoping when the mock runs inside a command substitution.
TENANT_ENV_DIR_MOCK="$env_dir"
tenant_env_dir() { printf '%s' "$TENANT_ENV_DIR_MOCK"; }

# --- STT engine selection: whisper default, parakeet-tdt support ---
VOICE_MODELS_DIR="$ROOT/models"
mkdir -p "$VOICE_MODELS_DIR"
hash64=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
chown() { :; }  # No root in the test harness; tenant users do not exist.

write_tenant_voice_env alpha >/dev/null
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_STT_ENGINE)" whisper "voice env defaults to whisper engine"
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_STT_MODEL)" whisper-tiny "voice env defaults to whisper model"

if voice_models_ready alpha; then ready=yes; else ready=no; fi
assert_eq "$ready" no "whisper readiness fails without verified models"
mkdir -p "$VOICE_MODELS_DIR/sherpa-onnx-whisper-tiny"
: >"$VOICE_MODELS_DIR/sherpa-onnx-whisper-tiny/tiny-encoder.int8.onnx"
: >"$VOICE_MODELS_DIR/sherpa-onnx-whisper-tiny/tiny-decoder.int8.onnx"
: >"$VOICE_MODELS_DIR/sherpa-onnx-whisper-tiny/tiny-tokens.txt"
printf '%s\n' "$hash64" >"$VOICE_MODELS_DIR/.lunarvoice-whisper-tiny.sha256"
if voice_models_ready alpha; then ready=yes; else ready=no; fi
assert_eq "$ready" yes "whisper readiness passes with verified models"

_env_set "$env_dir/voice.env" VOICE_STT_ENGINE parakeet
_env_set "$env_dir/voice.env" VOICE_STT_MODEL parakeet-tdt-0.6b-v3-int8
if voice_models_ready alpha; then ready=yes; else ready=no; fi
assert_eq "$ready" no "parakeet readiness fails without verified models"
mkdir -p "$VOICE_MODELS_DIR/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
: >"$VOICE_MODELS_DIR/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/encoder.int8.onnx"
: >"$VOICE_MODELS_DIR/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/decoder.int8.onnx"
: >"$VOICE_MODELS_DIR/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/tokens.txt"
printf '%s\n' "$hash64" >"$VOICE_MODELS_DIR/.lunarvoice-parakeet-tdt-0.6b-v3-int8.sha256"
if voice_models_ready alpha; then ready=yes; else ready=no; fi
assert_eq "$ready" no "parakeet readiness requires the joiner artifact"
: >"$VOICE_MODELS_DIR/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/joiner.int8.onnx"
if voice_models_ready alpha; then ready=yes; else ready=no; fi
assert_eq "$ready" yes "parakeet readiness passes with verified models"

write_tenant_voice_env alpha >/dev/null
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_STT_ENGINE)" parakeet "voice env rewrite preserves parakeet engine"
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_STT_MODEL)" parakeet-tdt-0.6b-v3-int8 "voice env rewrite preserves parakeet model"
unset -f chown

# Restore the original whisper fixture state for the configure_voice checks.
cat >"$env_dir/voice.env" <<'ENV'
VOICE_ENABLED=false
VOICE_AUTH_TOKEN=stable-token
ENV

voice_models_ready() { return 0; }

configure_voice alpha false >/dev/null
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_PROVIDER)" openai "disable preserves cloud provider"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_ENABLED)" true "disable preserves cloud transcription"

configure_voice alpha true >/dev/null
assert_eq "$(jq -r '.tenants.alpha.enable_voice' "$registry")" true "enable persists registry flag"
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_ENABLED)" true "enable persists sidecar flag"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_PROVIDER)" lunarvoice "enable selects local provider"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_ENABLED)" true "enable activates transcription"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_MODEL)" whisper-tiny "enable propagates sidecar model name"
assert_eq "$(grep -c '^TRANSCRIPTION_PROVIDER=' "$env_dir/lunarwing.env")" 1 "env update avoids duplicate keys"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" PRESERVE_ME)" yes "env update preserves unrelated keys"

configure_voice alpha false >/dev/null
assert_eq "$(jq -r '.tenants.alpha.enable_voice' "$registry")" false "disable clears registry flag"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_ENABLED)" false "disable deactivates local provider"

tenant_quadlet_dir() { printf '%s' "$ROOT/quadlets"; }
tenant_home() { printf '%s' "$ROOT/home"; }
OPENRC_INITD_DIR="$ROOT/init.d"
# shellcheck disable=SC2034  # Mock container runtime consumed by sourced mt-admin functions.
CONTAINER_RT=true
mkdir -p "$OPENRC_INITD_DIR"
ports_set_voice alpha true
render_voice_quadlet alpha
assert_eq "$(grep -c '^Before=lunarwing-alpha.service$' "$ROOT/quadlets/lunarwing-voice-alpha.container")" 1 "systemd orders voice before daemon"
assert_eq "$(grep -c '^EnvironmentFile=' "$ROOT/quadlets/lunarwing-voice-alpha.container")" 1 "quadlet uses protected tenant env"
render_voice_openrc_unit alpha
assert_eq "$(grep -c 'before lunarwing-alpha' "$OPENRC_INITD_DIR/lunarwing-voice-alpha")" 1 "OpenRC orders voice before daemon"

printf '%s passed, %s failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
