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

_env_set "$env_dir/voice.env" VOICE_STT_MODEL whisper-base
mkdir -p "$VOICE_MODELS_DIR/sherpa-onnx-whisper-base"
: >"$VOICE_MODELS_DIR/sherpa-onnx-whisper-base/base-encoder.int8.onnx"
: >"$VOICE_MODELS_DIR/sherpa-onnx-whisper-base/base-decoder.int8.onnx"
: >"$VOICE_MODELS_DIR/sherpa-onnx-whisper-base/base-tokens.txt"
printf '%s\n' "$hash64" >"$VOICE_MODELS_DIR/.lunarvoice-whisper-base.sha256"
if voice_models_ready alpha; then ready=yes; else ready=no; fi
assert_eq "$ready" yes "configured Whisper size resolves matching artifacts"
_env_set "$env_dir/voice.env" VOICE_STT_ENCODER /models/escape-encoder.onnx
ln -s /etc/passwd "$VOICE_MODELS_DIR/escape-encoder.onnx"
if voice_models_ready alpha; then ready=yes; else ready=no; fi
assert_eq "$ready" no "model readiness rejects symlinked artifacts"
_env_set "$env_dir/voice.env" VOICE_STT_ENCODER ""

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
# Keep ownership operations inert: tenant users do not exist in this harness.
chown() { :; }

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
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_PREVIOUS_TRANSCRIPTION_PROVIDER)" openai "enable captures prior provider"
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_PREVIOUS_TRANSCRIPTION_MODEL)" whisper-1 "enable captures prior model"
assert_eq "$(grep -c '^TRANSCRIPTION_PROVIDER=' "$env_dir/lunarwing.env")" 1 "env update avoids duplicate keys"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" PRESERVE_ME)" yes "env update preserves unrelated keys"

configure_voice alpha false >/dev/null
assert_eq "$(jq -r '.tenants.alpha.enable_voice' "$registry")" false "disable clears registry flag"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_ENABLED)" true "disable restores prior transcription state"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_PROVIDER)" openai "disable restores prior provider"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_MODEL)" whisper-1 "disable restores prior model"
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_PREVIOUS_TRANSCRIPTION_CAPTURED)" false "disable consumes saved provider state"

if _voice_validate_setting VOICE_STT_ENGINE whisper \
    && _voice_validate_setting VOICE_STT_LANGUAGE auto \
    && _voice_validate_setting VOICE_STT_LANGUAGE JA \
    && _voice_validate_setting VOICE_MAX_AUDIO_SECS 1 \
    && _voice_validate_setting VOICE_CACHE_TTL_SECS 0 \
    && ! _voice_validate_setting VOICE_STT_ENGINE sensevoice \
    && ! _voice_validate_setting VOICE_STT_LANGUAGE english \
    && ! _voice_validate_setting VOICE_STT_MODEL ../escape \
    && ! _voice_validate_setting VOICE_MAX_AUDIO_SECS 0 \
    && ! _voice_validate_setting VOICE_UNKNOWN value; then
  ok "managed voice settings enforce names and numeric bounds"
else
  bad "managed voice settings enforce names and numeric bounds" "validation mismatch"
fi

printf 'CUSTOM_KEEP=yes\n' >>"$env_dir/voice.env"
configure_voice_settings alpha VOICE_MAX_AUDIO_SECS=45 VOICE_ENABLE_TTS=false >/dev/null
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_MAX_AUDIO_SECS)" 45 "inactive configure updates bounded setting"
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_ENABLE_TTS)" false "inactive configure updates feature flag"
assert_eq "$(_env_existing "$env_dir/voice.env" CUSTOM_KEEP)" yes "configure preserves unmanaged settings"
before_invalid="$(sha256sum "$env_dir/voice.env" | cut -d' ' -f1)"
if configure_voice_settings alpha VOICE_MAX_AUDIO_SECS=0 >/dev/null 2>&1; then
  bad "invalid configure is rejected" "zero duration accepted"
else
  ok "invalid configure is rejected"
fi
assert_eq "$(sha256sum "$env_dir/voice.env" | cut -d' ' -f1)" "$before_invalid" "invalid configure leaves env unchanged"

tenant_quadlet_dir() { printf '%s' "$ROOT/quadlets"; }
tenant_home() { printf '%s' "$ROOT/home"; }
OPENRC_INITD_DIR="$ROOT/init.d"
# shellcheck disable=SC2034  # Mock container runtime consumed by sourced mt-admin functions.
CONTAINER_RT=true
mkdir -p "$OPENRC_INITD_DIR"
ports_set_voice alpha true
# Imported above, then replaced with a call-recording mock later in this test.
# shellcheck disable=SC2218
render_voice_quadlet alpha
assert_eq "$(grep -c '^Before=lunarwing-alpha.service$' "$ROOT/quadlets/lunarwing-voice-alpha.container")" 1 "systemd orders voice before daemon"
assert_eq "$(grep -c '^EnvironmentFile=' "$ROOT/quadlets/lunarwing-voice-alpha.container")" 1 "quadlet uses protected tenant env"
# Imported above, then replaced with a call-recording mock later in this test.
# shellcheck disable=SC2218
render_voice_openrc_unit alpha
assert_eq "$(grep -c 'before lunarwing-alpha' "$OPENRC_INITD_DIR/lunarwing-voice-alpha")" 1 "OpenRC orders voice before daemon"

# ── Hardening parity with LunarVision (systemd/OpenRC supervision) ───────────

CALLS=()
assert_call() {
  local expected="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "$expected" ]] && { ok "$expected"; return; }
  done
  bad "missing call: $expected" "not recorded"
}
assert_no_call_prefix() {
  local prefix="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" != "$prefix"* ]] || { bad "unexpected call: $call" "prefix $prefix"; return; }
  done
  ok "no call starting with: $prefix"
}
assert_before() {
  local first="$1" second="$2" first_index=-1 second_index=-1 index
  for index in "${!CALLS[@]}"; do
    [[ "${CALLS[$index]}" == "$first" ]] && first_index="$index"
    [[ "${CALLS[$index]}" == "$second" ]] && second_index="$index"
  done
  if [[ "$first_index" -ge 0 && "$second_index" -gt "$first_index" ]]; then
    ok "$first before $second"
  else
    bad "expected '$first' before '$second'" "order violated"
  fi
}

if _voice_port_valid 40000 && ! _voice_port_valid 0 && ! _voice_port_valid 65536 \
    && ! _voice_port_valid "" && ! _voice_port_valid abc; then
  ok "voice ports are range-checked"
else
  bad "voice ports are range-checked" "validation misclassified a port"
fi

quadlet="$ROOT/quadlets/lunarwing-voice-alpha.container"
assert_eq "$(grep -c '^HealthCmd=curl -sf http://127.0.0.1:8091/health && curl -sf http://127.0.0.1:8091/ready || exit 1$' "$quadlet")" 1 "quadlet health-checks liveness and readiness"
assert_eq "$(grep -c '^PodmanArgs=--security-opt=no-new-privileges$' "$quadlet")" 1 "quadlet sets no-new-privileges"
assert_eq "$(grep -c '^PublishPort=127.0.0.1:40000:8090$' "$quadlet")" 1 "quadlet publishes API port on loopback"
assert_eq "$(grep -c '^PublishPort=127.0.0.1:40001:8091$' "$quadlet")" 1 "quadlet publishes health port on loopback"
assert_eq "$(grep -c "^Volume=${VOICE_MODELS_DIR}:/models:ro\$" "$quadlet")" 1 "quadlet mounts model store read-only"
assert_eq "$(grep -c '^TimeoutStartSec=180$' "$quadlet")" 1 "quadlet allows slow STT model load"
if grep -qF 'stable-token' "$quadlet"; then bad "quadlet embeds no auth token" "token found"; else ok "quadlet embeds no auth token"; fi

mv "$quadlet" "$quadlet.saved"
ln -s "$ROOT/elsewhere" "$quadlet"
if render_voice_quadlet alpha >/dev/null 2>&1; then
  bad "quadlet refuses symlink path" "symlink target accepted"
else
  ok "quadlet refuses symlink path"
fi
rm -f "$quadlet"
mv "$quadlet.saved" "$quadlet"

mv "$env_dir/voice.env" "$env_dir/voice.env.saved"
if render_voice_quadlet alpha >/dev/null 2>&1; then
  bad "quadlet requires existing env file" "missing env accepted"
else
  ok "quadlet requires existing env file"
fi
mv "$env_dir/voice.env.saved" "$env_dir/voice.env"

openrc_unit="$OPENRC_INITD_DIR/lunarwing-voice-alpha"
assert_eq "$(grep -c 'container runtime unavailable' "$openrc_unit")" 1 "OpenRC fails loudly without runtime"
assert_eq "$(grep -c '_voice exec "\${voice_container}" curl' "$openrc_unit")" 1 "OpenRC probes health inside container"
assert_eq "$(grep -c 'checkpath -d -m 0700' "$openrc_unit")" 1 "OpenRC prepares rootless runtime dir"

# Supervised user-unit fallback for rootless Podman without Quadlet support.
helper="$ROOT/lunarwing-ctr-babysit"
printf '#!/bin/sh\nexit 0\n' >"$helper"
chmod 0755 "$helper"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
LUNARWING_MT_BABYSITTER_HELPER="$helper"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
CONTAINER_RT=podman
ensure_babysitter_helper() { :; }
command() {
  if [[ "${1:-}" == "-v" && "${2:-}" == "podman" ]]; then printf '/usr/bin/podman\n'; return 0; fi
  builtin command "$@"
}
id() {
  if [[ "${1:-}" == "-u" && "${2:-}" == "alpha" ]]; then printf '1001\n'; return 0; fi
  builtin command id "$@"
}
# Imported above, then replaced with a call-recording mock later in this test.
# shellcheck disable=SC2218
render_voice_systemd_fallback_unit alpha
fallback="$ROOT/home/.config/systemd/user/lunarwing-voice-alpha.service"
assert_eq "$(grep -c '^# Managed by lunarwing-mt-admin: LunarVoice fallback$' "$fallback")" 1 "fallback carries managed marker"
assert_eq "$(grep -c "^ExecStart=${helper} lunarwing-voice-alpha\$" "$fallback")" 1 "fallback supervises podman wait"
assert_eq "$(grep -c '^Restart=always$' "$fallback")" 1 "fallback restarts babysitter"
assert_eq "$(grep -c '^Environment=XDG_RUNTIME_DIR=/run/user/1001$' "$fallback")" 1 "fallback has rootless runtime dir"

rm "$fallback"
printf '# handwritten override\n' >"$fallback"
if render_voice_systemd_fallback_unit alpha >/dev/null 2>&1; then
  bad "fallback refuses unmanaged unit" "handwritten unit overwritten"
else
  ok "fallback refuses unmanaged unit"
fi
if _remove_voice_systemd_fallback alpha >/dev/null 2>&1; then
  bad "fallback removal refuses unmanaged unit" "handwritten unit removed"
else
  ok "fallback removal refuses unmanaged unit"
fi
rm -f "$fallback"

# OpenRC registration renders the direct health unit and the crash supervisor.
render_voice_openrc_unit() { CALLS+=("render-openrc:$1"); }
render_container_babysitter_unit() { CALLS+=("render-supervisor:$*"); }
rc-update() { CALLS+=("rc-update:$*"); }
rc-service() { CALLS+=("rc-service:$*"); return 0; }
CALLS=()
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
MT_ROOTLESS=true
_register_voice_openrc alpha
assert_call 'render-openrc:alpha'
assert_call 'rc-update:add lunarwing-voice-alpha default'
assert_call 'rc-service:lunarwing-voice-alpha start'
assert_call "render-supervisor:alpha voice lunarwing-voice-alpha 1001 $ROOT/home"
assert_call 'rc-update:add lunarwing-voice-alpha-sup default'
assert_call 'rc-service:lunarwing-voice-alpha-sup start'
assert_before 'rc-service:lunarwing-voice-alpha start' 'rc-service:lunarwing-voice-alpha-sup start'

# Runtime readiness requires both process liveness and a loaded STT model.
_ctr() {
  local name="$1" action="$2"
  shift 2
  CALLS+=("ctr:$name $action $*")
  if [[ "$action" == inspect ]]; then
    printf 'true\n'
  fi
}
CALLS=()
if _voice_container_healthy alpha lunarwing-voice-alpha; then
  ok "voice runtime health requires liveness and readiness"
else
  bad "voice runtime health requires liveness and readiness" "probe failed"
fi
assert_call 'ctr:alpha exec lunarwing-voice-alpha curl -sf -o /dev/null --max-time 3 http://127.0.0.1:8091/health'
assert_call 'ctr:alpha exec lunarwing-voice-alpha curl -sf -o /dev/null --max-time 3 http://127.0.0.1:8091/ready'

# Imperative container reconciliation is convergent and health-gated.
CONTAINER_EXISTS=true
CONFIG_CHANGED=false
UNSUPPORTED_HEALTH=false
CONTAINER_HEALTHY=true
_ctr() {
  local name="$1" action="$2"
  shift 2
  CALLS+=("ctr:$name $action $*")
  case "$action" in
    inspect) [[ "$CONTAINER_EXISTS" == true ]] ;;
    rm) CONTAINER_EXISTS=false ;;
  esac
}
_container_config_changed() { [[ "$CONFIG_CHANGED" == true ]]; }
_voice_has_unsupported_openrc_healthcheck() { [[ "$UNSUPPORTED_HEALTH" == true ]]; }
_voice_container_healthy() { [[ "$CONTAINER_HEALTHY" == true ]]; }
_wait_tenant_voice_healthy() { CALLS+=("wait:$*"); return 0; }
_stop_voice_supervision() { CALLS+=("stop-supervision:$*"); }
_create_voice_container() { CALLS+=("create:$*"); CONTAINER_EXISTS=true; }
_store_container_hash() { CALLS+=("store:$*"); }

CALLS=()
_reconcile_voice_container alpha lunarwing-voice-alpha "$env_dir/voice.env" contract-a
assert_no_call_prefix 'stop-supervision:'
assert_no_call_prefix 'create:'
assert_no_call_prefix 'ctr:alpha stop'
assert_no_call_prefix 'ctr:alpha start'
assert_call "store:alpha lunarwing-voice-alpha $env_dir/voice.env contract-a"

CONFIG_CHANGED=true
CALLS=()
_reconcile_voice_container alpha lunarwing-voice-alpha "$env_dir/voice.env" contract-b
assert_call 'stop-supervision:alpha lunarwing-voice-alpha'
assert_call 'ctr:alpha stop --time 30 lunarwing-voice-alpha'
assert_call 'ctr:alpha rm -f lunarwing-voice-alpha'
assert_call "create:alpha lunarwing-voice-alpha $env_dir/voice.env 40000 40001"
assert_before 'stop-supervision:alpha lunarwing-voice-alpha' 'ctr:alpha rm -f lunarwing-voice-alpha'

CONFIG_CHANGED=false
CONTAINER_HEALTHY=false
CALLS=()
_reconcile_voice_container alpha lunarwing-voice-alpha "$env_dir/voice.env" contract-c
assert_call 'stop-supervision:alpha lunarwing-voice-alpha'
assert_call 'ctr:alpha stop --time 30 lunarwing-voice-alpha'
assert_call 'ctr:alpha start lunarwing-voice-alpha'
assert_call 'wait:alpha lunarwing-voice-alpha'

# render-units refreshes existing voice assets without starting services.
CONTAINER_HEALTHY=true
render_voice_quadlet() { CALLS+=("render-quadlet:$1"); }
render_voice_systemd_fallback_unit() { CALLS+=("render-fallback:$1"); }
QUADLET_SUPPORTED=true
podman_supports_quadlet() { [[ "$QUADLET_SUPPORTED" == true ]]; }
qfile="$ROOT/quadlets/lunarwing-voice-alpha.container"
rm -f -- "$qfile" "$fallback"

CONTAINER_EXISTS=false
CALLS=()
render_voice_systemd_assets alpha
assert_no_call_prefix 'render-quadlet:'
assert_no_call_prefix 'render-fallback:'

CONTAINER_EXISTS=true
CALLS=()
render_voice_systemd_assets alpha
assert_call 'render-quadlet:alpha'

mkdir -p "$(dirname "$fallback")"
touch "$fallback"
QUADLET_SUPPORTED=false
CALLS=()
render_voice_systemd_assets alpha
assert_call 'render-fallback:alpha'
assert_no_call_prefix 'render-quadlet:'
rm -f -- "$fallback"

touch "$qfile"
QUADLET_SUPPORTED=true
CALLS=()
render_voice_systemd_assets alpha
assert_call 'render-quadlet:alpha'

touch "$fallback"
CALLS=()
if render_voice_systemd_assets alpha >/dev/null 2>&1; then
  bad "conflicting voice quadlet and fallback rejected" "both shapes accepted"
else
  ok "conflicting voice quadlet and fallback rejected"
fi
rm -f -- "$qfile" "$fallback"

ports_set_voice alpha false
touch "$qfile"
CALLS=()
render_voice_systemd_assets alpha
assert_no_call_prefix 'render-quadlet:'
assert_no_call_prefix 'render-fallback:'
[[ ! -e "$qfile" ]] && ok "disabled tenant quadlet removed" || bad "disabled tenant quadlet removed" "quadlet still present"
ports_set_voice alpha true

CONTAINER_EXISTS=false
rm -f -- "$OPENRC_INITD_DIR/lunarwing-voice-alpha" "$OPENRC_INITD_DIR/lunarwing-voice-alpha-sup"
CALLS=()
render_voice_openrc_assets alpha
assert_no_call_prefix 'render-openrc:'
assert_no_call_prefix 'render-supervisor:'

CONTAINER_EXISTS=true
touch "$OPENRC_INITD_DIR/lunarwing-voice-alpha"
CALLS=()
render_voice_openrc_assets alpha
assert_call 'render-openrc:alpha'
assert_call "render-supervisor:alpha voice lunarwing-voice-alpha 1001 $ROOT/home"
assert_no_call_prefix 'rc-service:'
assert_no_call_prefix 'rc-update:'

ports_set_voice alpha false
CALLS=()
render_voice_openrc_assets alpha
assert_no_call_prefix 'render-openrc:'
[[ ! -e "$OPENRC_INITD_DIR/lunarwing-voice-alpha" ]] \
  && ok "disabled tenant OpenRC unit removed" \
  || bad "disabled tenant OpenRC unit removed" "unit still present"
ports_set_voice alpha true

# Required ports fail before any image/container mutation.
# shellcheck disable=SC2034  # Updates state consumed by sourced mt-admin functions.
ensure_init_system() { INIT_SYSTEM=systemd; }
_ensure_tenant_image() { CALLS+=("ensure-image:$*"); }
ports_get() {
  case "$2" in
    voice_service) printf '40000' ;;
    voice_health) return 1 ;;
    *) return 1 ;;
  esac
}
CALLS=()
if start_tenant_voice alpha >/dev/null 2>&1; then
  bad "missing voice health port accepted" "start succeeded"
else
  ok "missing voice health port fails start"
fi
assert_no_call_prefix 'ensure-image:'

ports_get() {
  case "$2" in
    voice_service) printf '40001' ;;
    voice_health) printf '40001' ;;
    *) return 1 ;;
  esac
}
CALLS=()
if start_tenant_voice alpha >/dev/null 2>&1; then
  bad "identical voice ports accepted" "start succeeded"
else
  ok "identical voice ports fail start"
fi
assert_no_call_prefix 'ensure-image:'

# Full install/configure/activate/deactivate/remove lifecycle with mocked init.
ports_get() {
  case "$2" in
    voice_service) printf '40000' ;;
    voice_health) printf '40001' ;;
    *) return 1 ;;
  esac
}
ensure_container_runtime() { :; }
_ensure_tenant_image() { CALLS+=("ensure-image:$*"); return 0; }
START_OK=true
start_tenant_voice() { CALLS+=("start-voice:$*"); [[ "$START_OK" == true ]]; }
stop_tenant_voice() { CALLS+=("stop-voice:$*"); return 0; }
remove_tenant_voice_unit() { CALLS+=("remove-unit:$*"); return 0; }
_reload_voice_consumer_if_running() { CALLS+=("reload-daemon:$*"); return 0; }
CONTAINER_RT=true
CONTAINER_EXISTS=true
_ctr() {
  local name="$1" action="$2"
  shift 2
  CALLS+=("ctr:$name $action $*")
  case "$action" in
    inspect) [[ "$CONTAINER_EXISTS" == true ]] ;;
    rm) CONTAINER_EXISTS=false ;;
  esac
}

ports_set_voice alpha false
CALLS=()
install_tenant_voice alpha "" >/dev/null
assert_call 'ensure-image:alpha lunarwing/voice-service:latest'
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_ALLOW_UNAUTHENTICATED)" false "install enforces authenticated API"
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_AUTO_DOWNLOAD)" false "install disables implicit downloads"
assert_eq "$(stat -c %a "$env_dir/voice.env")" 600 "install writes owner-only voice env"

CALLS=()
activate_tenant_voice alpha >/dev/null
assert_call 'start-voice:alpha'
assert_call 'reload-daemon:alpha'
assert_eq "$(jq -r '.tenants.alpha.enable_voice' "$registry")" true "activate persists enabled state"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_PROVIDER)" lunarvoice "activate switches daemon provider"

old_voice_hash="$(sha256sum "$env_dir/voice.env" | cut -d' ' -f1)"
START_OK=false
if configure_tenant_voice alpha VOICE_MAX_AUDIO_SECS=61 >/dev/null 2>&1; then
  bad "failed active reconfigure reports failure" "runtime failure ignored"
else
  ok "failed active reconfigure reports failure"
fi
assert_eq "$(sha256sum "$env_dir/voice.env" | cut -d' ' -f1)" "$old_voice_hash" "failed active reconfigure restores voice env"
START_OK=true
configure_tenant_voice alpha VOICE_MAX_AUDIO_SECS=60 >/dev/null
assert_eq "$(_env_existing "$env_dir/voice.env" VOICE_MAX_AUDIO_SECS)" 60 "active configure reconciles validated setting"

CALLS=()
deactivate_tenant_voice alpha >/dev/null
assert_call 'stop-voice:alpha'
assert_call 'remove-unit:alpha'
assert_call 'reload-daemon:alpha'
assert_eq "$(jq -r '.tenants.alpha.enable_voice' "$registry")" false "deactivate clears enabled state"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_PROVIDER)" openai "deactivate restores cloud provider"

START_OK=false
if (activate_tenant_voice alpha >/dev/null 2>&1); then
  bad "failed activation reports failure" "health-gated start failure ignored"
else
  ok "failed activation reports failure"
fi
assert_eq "$(jq -r '.tenants.alpha.enable_voice' "$registry")" false "failed activation rolls back enabled state"
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_PROVIDER)" openai "failed activation restores provider"
START_OK=true
activate_tenant_voice alpha >/dev/null

CALLS=()
remove_tenant_voice alpha >/dev/null
assert_call 'stop-voice:alpha'
assert_call 'remove-unit:alpha'
assert_call 'ctr:alpha rm -f lunarwing-voice-alpha'
[[ ! -e "$env_dir/voice.env" ]] && ok "remove deletes tenant voice env" \
  || bad "remove deletes tenant voice env" "voice.env remains"
if grep -qE '^(VOICE_SERVICE_URL|VOICE_AUTH_TOKEN)=' "$env_dir/lunarwing.env"; then
  bad "remove deletes daemon voice credentials" "voice credential remains"
else
  ok "remove deletes daemon voice credentials"
fi
assert_eq "$(_env_existing "$env_dir/lunarwing.env" TRANSCRIPTION_PROVIDER)" openai "remove retains restored provider"

help_text="$(usage)"
for command_name in install-voice configure-voice activate-voice deactivate-voice remove-voice; do
  assert_has="$command_name"
  if grep -q "$assert_has" <<<"$help_text"; then
    ok "usage exposes $command_name"
  else
    bad "usage exposes $command_name" "command missing"
  fi
done

printf '%s passed, %s failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
