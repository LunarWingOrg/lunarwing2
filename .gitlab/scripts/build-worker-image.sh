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
        source_ref=0.41.0
        build_args="KIMI_CODE_VERSION=0.41.0
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
