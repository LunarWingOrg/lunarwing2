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
    echo "usage: build-worker-image.sh pebble|opencode|kimi" >&2
    exit 2
fi

case "$1" in
    pebble)
        image_name=worker-pebble
        context=.
        dockerfile_dir=pebble4lunarwing
        input_files='pebble4lunarwing/Dockerfile
pebble4lunarwing/Cargo.lock'
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
    kimi)
        image_name=worker-kimi
        context=kimicode4lunarwing
        dockerfile_dir=kimicode4lunarwing
        input_files='kimicode4lunarwing/Dockerfile
kimicode4lunarwing/scripts/package-lock.json
kimicode4lunarwing/kimi-cli/package-lock.json'
        source_ref=0.39.1
        build_args="KIMI_CODE_VERSION=0.39.1
WITH_TOOLCHAINS=false
SOURCE_REVISION=$CI_COMMIT_SHA"
        ;;
    *) echo "unknown worker: $1" >&2; exit 2 ;;
esac

candidate_tag="${CI_COMMIT_SHA}-${CI_PIPELINE_ID}-${CI_JOB_ID}"
candidate_ref="${CI_REGISTRY_IMAGE}/${image_name}:${candidate_tag}"
metadata_file="$CI_PROJECT_DIR/worker-image-metadata.json"
inputs_file="$CI_PROJECT_DIR/worker-inputs.sha256"
env_file="$CI_PROJECT_DIR/worker-image.env"

mkdir -p "$HOME/.docker"
auth=$(printf '%s:%s' "$CI_REGISTRY_USER" "$CI_REGISTRY_PASSWORD" | base64 | tr -d '\n')
printf '{"auths":{"%s":{"auth":"%s"}}}\n' "$CI_REGISTRY" "$auth" > "$HOME/.docker/config.json"

: > "$inputs_file"
printf '%s\n' "$input_files" > "$CI_PROJECT_DIR/worker-input-files.txt"
while IFS= read -r input_file; do
    [ -n "$input_file" ] && sha256sum "$CI_PROJECT_DIR/$input_file" >> "$inputs_file"
done < "$CI_PROJECT_DIR/worker-input-files.txt"
printf 'source_ref  %s\n' "$source_ref" >> "$inputs_file"
printf 'source_sha  %s\n' "$CI_COMMIT_SHA" >> "$inputs_file"

set -- buildctl-daemonless.sh build \
    --frontend dockerfile.v0 \
    --local "context=$CI_PROJECT_DIR/$context" \
    --local "dockerfile=$CI_PROJECT_DIR/$dockerfile_dir" \
    --opt filename=Dockerfile \
    --metadata-file "$metadata_file" \
    --output "type=image,name=$candidate_ref,push=true"

printf '%s\n' "$build_args" > "$CI_PROJECT_DIR/worker-build-args.txt"
while IFS= read -r build_arg; do
    [ -n "$build_arg" ] && set -- "$@" --opt "build-arg:$build_arg"
done < "$CI_PROJECT_DIR/worker-build-args.txt"
"$@"

digest=$(awk -F'"' '/"containerimage.digest"/ { print $4; exit }' "$metadata_file")
case "$digest" in
    sha256:????????????????????????????????????????????????????????????????) ;;
    *) echo "BuildKit did not report a valid image digest" >&2; exit 1 ;;
esac

{
    printf 'WORKER_IMAGE_NAME=%s\n' "$image_name"
    printf 'WORKER_CANDIDATE_TAG=%s\n' "$candidate_tag"
    printf 'WORKER_IMAGE_REF=%s\n' "$candidate_ref"
    printf 'WORKER_IMAGE_DIGEST=%s\n' "$digest"
} > "$env_file"

printf 'Published immutable candidate %s@%s\n' "$candidate_ref" "$digest"
