#!/bin/sh
set -eu

: "${CI_COMMIT_SHA:?}"
: "${CI_JOB_ID:?}"
: "${CI_PIPELINE_ID:?}"
: "${CI_PROJECT_DIR:?}"
: "${CI_REGISTRY:?}"
: "${CI_REGISTRY_IMAGE:?}"
: "${CI_REGISTRY_PASSWORD:?}"
: "${CI_REGISTRY_USER:?}"

if [ "$#" -ne 1 ]; then
    echo "usage: build-worker-image.sh pebble|opencode|opencode2|kimi" >&2
    exit 2
fi

case "$1" in
    pebble)
        image_name=worker-pebble
        context=.
        dockerfile_dir=pebble4lunarwing
        input_files='pebble4lunarwing/Dockerfile
pebble4lunarwing/Cargo.lock
pebble4lunarwing/upstream/Cargo.lock
pebble4lunarwing/scripts/verify_upstream_lock.py
pebble4lunarwing/scripts/toolchain-profile.sh
pebble4lunarwing/scripts/rustup-wrapper.sh'
        source_ref=2d0f3786cbebb705cbd4f6212b0aaf47d8d8590b
        build_args="PEBBLE_REF=2d0f3786cbebb705cbd4f6212b0aaf47d8d8590b
WITH_TOOLCHAINS=false
SOURCE_REVISION=$CI_COMMIT_SHA"
        ;;
    opencode)
        image_name=worker-opencode
        context=opencode4lunarwing
        dockerfile_dir=opencode4lunarwing
        input_files=opencode4lunarwing/Dockerfile
        source_ref=cb7d8b2f5e44876ef98b661dc10590c915af3a9f
        build_args="OPENCODE_VERSION=v1.18.25
OPENCODE_REF=cb7d8b2f5e44876ef98b661dc10590c915af3a9f
WITH_TOOLCHAINS=false
SOURCE_REVISION=$CI_COMMIT_SHA"
        ;;
    opencode2)
        image_name=worker-opencode2
        context=opencode2.4lunarwing
        dockerfile_dir=opencode2.4lunarwing
        # WI#357 exposure-policy inputs executed by the build (CY-4
        # precedent: build-executed scripts must hash into worker-inputs):
        # the wrapper is installed over the binary path, the assert runs
        # against the lane + built binary, and the compile-out patch runs
        # against the upstream checkout; their self-tests execute in-image.
        # Also covers the WI#369 SDK-surface gate files and the WI#324
        # tool-proxy relay/plugin/bridge sources plus every bun suite the
        # Dockerfile self-test chain executes (KL-13).
        input_files='opencode2.4lunarwing/Dockerfile
opencode2.4lunarwing/entrypoint.sh
opencode2.4lunarwing/scripts/prepare_reproducible_build.py
opencode2.4lunarwing/scripts/path_expand_test.ts
opencode2.4lunarwing/scripts/opencode_result_test.ts
opencode2.4lunarwing/scripts/sdk_surface.ts
opencode2.4lunarwing/scripts/sdk_surface_assert.ts
opencode2.4lunarwing/scripts/sdk_surface_test.ts
opencode2.4lunarwing/scripts/sdk_surface_assert_test.ts
opencode2.4lunarwing/scripts/tsconfig.sdk-surface.json
opencode2.4lunarwing/scripts/gate-env.d.ts
opencode2.4lunarwing/scripts/opencode_task_executor.ts
opencode2.4lunarwing/scripts/opencode_task_executor_test.ts
opencode2.4lunarwing/scripts/lunarwing_runtime.ts
opencode2.4lunarwing/scripts/lunarwing_bridge.ts
opencode2.4lunarwing/scripts/lunarwing_bridge_test.ts
opencode2.4lunarwing/scripts/lunarwing_tool_proxy.ts
opencode2.4lunarwing/scripts/lunarwing_tool_proxy_test.ts
opencode2.4lunarwing/scripts/lunarwing_tool_plugin/index.ts
opencode2.4lunarwing/scripts/lunarwing_tool_plugin_test.ts
opencode2.4lunarwing/scripts/opencode-exposure-guard.sh
opencode2.4lunarwing/scripts/exposure_policy_assert.py
opencode2.4lunarwing/scripts/exposure_policy_assert_test.py
opencode2.4lunarwing/scripts/entrypoint_config_test.py
opencode2.4lunarwing/scripts/compile_out_remote_tunnel.py
opencode2.4lunarwing/scripts/compile_out_remote_tunnel_test.py'
        source_ref=843a6c514e606b436a7fe713422ace0a93ad3d61
        # WI#357 run-106 GJ-3(a): the checkout's wrapper digest, compared by
        # the in-image --image-filesystem assert against the installed wrapper.
        # Detects accidental drift; an adversarial Dockerfile can still defeat
        # it. Out-of-band image verification remains a WI#357 follow-up (GW-3).
        wrapper_file="$CI_PROJECT_DIR/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
        wrapper_anchor=$(sha256sum < "$wrapper_file") || {
            echo "cannot hash $wrapper_file" >&2; exit 1;
        }
        wrapper_anchor=${wrapper_anchor%% *}
        case "$wrapper_anchor" in
            *[!0-9a-f]*) echo "invalid wrapper sha256 for $wrapper_file" >&2; exit 1 ;;
        esac
        [ "${#wrapper_anchor}" -eq 64 ] || {
            echo "invalid wrapper sha256 for $wrapper_file" >&2; exit 1;
        }
        build_args="OPENCODE_VERSION=v2.0.32
OPENCODE_REF=843a6c514e606b436a7fe713422ace0a93ad3d61
WITH_TOOLCHAINS=false
LUNARWING_WRAPPER_SHA256=$wrapper_anchor
SOURCE_REVISION=$CI_COMMIT_SHA"
        ;;
    kimi)
        image_name=worker-kimi
        context=kimicode4lunarwing
        dockerfile_dir=kimicode4lunarwing
        # CY-4 precedent (see the opencode2 block): build-read inputs hash
        # into worker-inputs. The builder stage npm-ci's scripts/, compiles
        # and executes every scripts/*.ts, and the in-build test gate reads
        # entrypoint.sh, hooks/ and config/; the CLI install cross-checks
        # kimi-cli/package.json against KIMI_CODE_VERSION. Runtime COPYs also
        # ship health_server.py and agent_comm_protocol.json (IL-1).
        # The Dockerfile's `COPY scripts/*.ts` glob means a new bridge source
        # must join this list in the same commit (validate_config.py enforces it).
        input_files='kimicode4lunarwing/Dockerfile
kimicode4lunarwing/entrypoint.sh
kimicode4lunarwing/health_server.py
kimicode4lunarwing/agent_comm_protocol.json
kimicode4lunarwing/config/config.toml
kimicode4lunarwing/hooks/headless_command_guard.mjs
kimicode4lunarwing/kimi-cli/package.json
kimicode4lunarwing/kimi-cli/package-lock.json
kimicode4lunarwing/scripts/package.json
kimicode4lunarwing/scripts/package-lock.json
kimicode4lunarwing/scripts/tsconfig.json
kimicode4lunarwing/scripts/entrypoint_config_test.ts
kimicode4lunarwing/scripts/headless_command_guard_test.ts
kimicode4lunarwing/scripts/kimi_cli_contract_test.ts
kimicode4lunarwing/scripts/kimi_result.ts
kimicode4lunarwing/scripts/kimi_result_test.ts
kimicode4lunarwing/scripts/kimi_task_executor.ts
kimicode4lunarwing/scripts/kimi_task_executor_test.ts
kimicode4lunarwing/scripts/lunarwing_bridge.ts
kimicode4lunarwing/scripts/lunarwing_bridge_test.ts
kimicode4lunarwing/scripts/lunarwing_mcp_server.ts
kimicode4lunarwing/scripts/lunarwing_mcp_server_test.ts
kimicode4lunarwing/scripts/lunarwing_runtime.ts
kimicode4lunarwing/scripts/lunarwing_runtime_test.ts
kimicode4lunarwing/scripts/lunarwing_tool_proxy.ts
kimicode4lunarwing/scripts/lunarwing_tool_proxy_test.ts
kimicode4lunarwing/scripts/path_expand_test.ts
kimicode4lunarwing/scripts/smoke_test.ts
kimicode4lunarwing/scripts/task_prompt_test.ts
kimicode4lunarwing/scripts/workspace_path.ts'
        source_ref=2.2.0
        build_args="KIMI_CODE_VERSION=2.2.0
WITH_TOOLCHAINS=false
SOURCE_REVISION=$CI_COMMIT_SHA"
        ;;
    *) echo "unknown worker: $1" >&2; exit 2 ;;
esac

candidate_tag="${CI_COMMIT_SHA}-${CI_PIPELINE_ID}-${CI_JOB_ID}"
candidate_ref="${CI_REGISTRY_IMAGE}/${image_name}:${candidate_tag}"
rebuild_ref="${candidate_ref}-rebuild"
metadata_file="$CI_PROJECT_DIR/worker-image-metadata.json"
rebuild_metadata_file="$CI_PROJECT_DIR/worker-image-rebuild-metadata.json"
inputs_file="$CI_PROJECT_DIR/worker-inputs.sha256"
env_file="$CI_PROJECT_DIR/worker-image.env"
source_date_epoch=$(git -C "$CI_PROJECT_DIR" show -s --format=%ct "$CI_COMMIT_SHA")
case "$source_date_epoch" in
    '' | *[!0-9]*) echo "invalid source commit timestamp" >&2; exit 1 ;;
esac
build_args="$build_args
SOURCE_DATE_EPOCH=$source_date_epoch"

mkdir -p "$HOME/.docker"
auth=$(printf '%s:%s' "$CI_REGISTRY_USER" "$CI_REGISTRY_PASSWORD" | base64 | tr -d '\n')
printf '{"auths":{"%s":{"auth":"%s"}}}\n' "$CI_REGISTRY" "$auth" > "$HOME/.docker/config.json"

: > "$inputs_file"
printf '%s\n' "$input_files" > "$CI_PROJECT_DIR/worker-input-files.txt"
while IFS= read -r input_file; do
    if [ -n "$input_file" ]; then
        checksum=$(sha256sum "$CI_PROJECT_DIR/$input_file" | cut -d ' ' -f 1)
        printf '%s  %s\n' "$checksum" "$input_file" >> "$inputs_file"
    fi
done < "$CI_PROJECT_DIR/worker-input-files.txt"
{
    printf 'source_ref  %s\n' "$source_ref"
    printf 'source_sha  %s\n' "$CI_COMMIT_SHA"
    printf 'source_date_epoch  %s\n' "$source_date_epoch"
} >> "$inputs_file"
printf '%s\n' "$build_args" > "$CI_PROJECT_DIR/worker-build-args.txt"

build_image() {
    output_ref=$1
    output_metadata=$2
    clean=$3
    set -- buildctl-daemonless.sh build
    [ "$clean" = true ] && set -- "$@" --no-cache
    set -- "$@" \
        --frontend dockerfile.v0 \
        --local "context=$CI_PROJECT_DIR/$context" \
        --local "dockerfile=$CI_PROJECT_DIR/$dockerfile_dir" \
        --opt filename=Dockerfile \
        --metadata-file "$output_metadata" \
        --output "type=image,name=$output_ref,push=true,oci-mediatypes=false,rewrite-timestamp=true"
    while IFS= read -r build_arg; do
        [ -n "$build_arg" ] && set -- "$@" --opt "build-arg:$build_arg"
    done < "$CI_PROJECT_DIR/worker-build-args.txt"
    "$@"
}

metadata_digest() {
    awk -F'"' '/"containerimage.digest"/ { print $4; exit }' "$1"
}

# Best-effort layer-level diagnosis for a candidate/rebuild digest mismatch
# (work item #177): fetch both manifests from the project registry with the
# job's registry credentials, then report whether the divergence is in the
# layer digests or the image config, and print a diff pinpointing the first
# differing entry. Nothing here may mask the real failure, so every step is
# guarded and the function only ever emits informational output.
print_reproducibility_diagnosis() {
    _diag_image=$1
    _diag_cand_tag=$2
    _diag_rebuild_tag=$3
    _diag_full="${CI_REGISTRY_IMAGE}/${_diag_image}"
    case "$_diag_full" in
        "$CI_REGISTRY"/*) _diag_repo=${_diag_full#"$CI_REGISTRY"/} ;;
        *) _diag_repo=$_diag_full ;;
    esac
    echo "INFO: fetching candidate and rebuild manifests from the registry for a layer-level comparison (best effort)" >&2
    _diag_auth=$(printf '%s:%s' "$CI_REGISTRY_USER" "$CI_REGISTRY_PASSWORD" | base64 | tr -d '\n')
    _diag_token=$(wget -q -T 30 -O - \
        --header "Authorization: Basic ${_diag_auth}" \
        "https://${CI_SERVER_HOST}/jwt/auth?service=container_registry&scope=repository:${_diag_repo}:pull" \
        2>/dev/null | sed -n 's/.*"token" *: *"\([^"]*\)".*/\1/p') || _diag_token=""
    if [ -z "$_diag_token" ]; then
        echo "WARN: could not obtain a registry pull token — diagnosis unavailable" >&2
        return 0
    fi
    _diag_manifest() {
        wget -q -T 30 -O - \
            --header "Authorization: Bearer ${_diag_token}" \
            --header 'Accept: application/vnd.docker.distribution.manifest.v2+json' \
            "https://${CI_REGISTRY}/v2/${_diag_repo}/manifests/$1" 2>/dev/null || return 1
    }
    _diag_cand_manifest=$(_diag_manifest "$_diag_cand_tag") || _diag_cand_manifest=""
    _diag_rebuild_manifest=$(_diag_manifest "$_diag_rebuild_tag") || _diag_rebuild_manifest=""
    if [ -z "$_diag_cand_manifest" ] || [ -z "$_diag_rebuild_manifest" ]; then
        echo "WARN: could not fetch the candidate/rebuild manifests — diagnosis unavailable" >&2
        return 0
    fi
    # Docker manifests serialize the config descriptor before the layers, so
    # the first digest is the config digest and the rest are the layers.
    _diag_cand_digests=$(printf '%s\n' "$_diag_cand_manifest" | grep -o '"digest" *: *"sha256:[0-9a-f]*"' | sed 's/.*"sha256:/sha256:/;s/"$//')
    _diag_rebuild_digests=$(printf '%s\n' "$_diag_rebuild_manifest" | grep -o '"digest" *: *"sha256:[0-9a-f]*"' | sed 's/.*"sha256:/sha256:/;s/"$//')
    _diag_cand_config=$(printf '%s\n' "$_diag_cand_digests" | sed -n '1p')
    _diag_rebuild_config=$(printf '%s\n' "$_diag_rebuild_digests" | sed -n '1p')
    _diag_cand_layers=$(printf '%s\n' "$_diag_cand_digests" | sed '1d' | grep -c .)
    _diag_rebuild_layers=$(printf '%s\n' "$_diag_rebuild_digests" | sed '1d' | grep -c .)
    echo "INFO: candidate config ${_diag_cand_config} (${_diag_cand_layers} layers); rebuild config ${_diag_rebuild_config} (${_diag_rebuild_layers} layers)" >&2
    _diag_tmp=$(mktemp -d /tmp/worker-diagnosis.XXXXXX) || return 0
    printf '%s\n' "$_diag_cand_digests" | sed '1d' >"$_diag_tmp/candidate-layers"
    printf '%s\n' "$_diag_rebuild_digests" | sed '1d' >"$_diag_tmp/rebuild-layers"
    if diff -q "$_diag_tmp/candidate-layers" "$_diag_tmp/rebuild-layers" >/dev/null; then
        echo "INFO: all layer digests are identical — the divergence is in the image config" >&2
        _diag_cand_config_json=$(wget -q -T 30 -O - \
            --header "Authorization: Bearer ${_diag_token}" \
            "https://${CI_REGISTRY}/v2/${_diag_repo}/blobs/${_diag_cand_config}" 2>/dev/null) || _diag_cand_config_json=""
        _diag_rebuild_config_json=$(wget -q -T 30 -O - \
            --header "Authorization: Bearer ${_diag_token}" \
            "https://${CI_REGISTRY}/v2/${_diag_repo}/blobs/${_diag_rebuild_config}" 2>/dev/null) || _diag_rebuild_config_json=""
        if [ -n "$_diag_cand_config_json" ] && [ -n "$_diag_rebuild_config_json" ]; then
            printf '%s' "$_diag_cand_config_json" | tr ',' '\n' >"$_diag_tmp/candidate-config"
            printf '%s' "$_diag_rebuild_config_json" | tr ',' '\n' >"$_diag_tmp/rebuild-config"
            echo "INFO: image config diff (one field per line)" >&2
            diff -u "$_diag_tmp/candidate-config" "$_diag_tmp/rebuild-config" >&2 || true
        else
            echo "WARN: could not fetch the image configs for diffing" >&2
        fi
    else
        echo "INFO: layer digest diff follows (hunk headers are 1-based layer indexes, counting from the first image layer)" >&2
        diff -u "$_diag_tmp/candidate-layers" "$_diag_tmp/rebuild-layers" >&2 || true
    fi
    rm -rf "$_diag_tmp"
    return 0
}

build_image "$candidate_ref" "$metadata_file" false
digest=$(metadata_digest "$metadata_file")
case "$digest" in
    sha256:????????????????????????????????????????????????????????????????) ;;
    *) echo "BuildKit did not report a valid image digest" >&2; exit 1 ;;
esac

build_image "$rebuild_ref" "$rebuild_metadata_file" true
rebuild_digest=$(metadata_digest "$rebuild_metadata_file")
if [ "$rebuild_digest" != "$digest" ]; then
    echo "clean rebuild digest mismatch: $rebuild_digest != $digest" >&2
    print_reproducibility_diagnosis "$image_name" "$candidate_tag" "${candidate_tag}-rebuild"
    exit 1
fi

{
    printf 'WORKER_IMAGE_NAME=%s\n' "$image_name"
    printf 'WORKER_CANDIDATE_TAG=%s\n' "$candidate_tag"
    printf 'WORKER_IMAGE_REF=%s\n' "$candidate_ref"
    printf 'WORKER_IMAGE_DIGEST=%s\n' "$digest"
    printf 'WORKER_REBUILD_REF=%s\n' "$rebuild_ref"
    printf 'WORKER_REBUILD_DIGEST=%s\n' "$rebuild_digest"
} > "$env_file"

printf 'Published reproducible candidate %s@%s\n' "$candidate_ref" "$digest"
