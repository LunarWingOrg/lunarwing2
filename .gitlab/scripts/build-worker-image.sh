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
: "${WORKER_CONTEXT:?}"
: "${WORKER_DOCKERFILE_DIR:?}"
: "${WORKER_IMAGE_NAME:?}"
: "${WORKER_INPUT_FILES:?}"

candidate_tag="${CI_COMMIT_SHA}-${CI_PIPELINE_ID}-${CI_JOB_ID}"
candidate_ref="${CI_REGISTRY_IMAGE}/${WORKER_IMAGE_NAME}:${candidate_tag}"
metadata_file="$CI_PROJECT_DIR/worker-image-metadata.json"
inputs_file="$CI_PROJECT_DIR/worker-inputs.sha256"
env_file="$CI_PROJECT_DIR/worker-image.env"

mkdir -p "$HOME/.docker"
auth=$(printf '%s:%s' "$CI_REGISTRY_USER" "$CI_REGISTRY_PASSWORD" | base64 | tr -d '\n')
printf '{"auths":{"%s":{"auth":"%s"}}}\n' "$CI_REGISTRY" "$auth" > "$HOME/.docker/config.json"

: > "$inputs_file"
printf '%s\n' "$WORKER_INPUT_FILES" > "$CI_PROJECT_DIR/worker-input-files.txt"
while IFS= read -r input_file; do
    [ -n "$input_file" ] && sha256sum "$CI_PROJECT_DIR/$input_file" >> "$inputs_file"
done < "$CI_PROJECT_DIR/worker-input-files.txt"
printf 'source_ref  %s\n' "${WORKER_SOURCE_REF:-$CI_COMMIT_SHA}" >> "$inputs_file"
printf 'source_sha  %s\n' "$CI_COMMIT_SHA" >> "$inputs_file"

set -- buildctl-daemonless.sh build \
    --frontend dockerfile.v0 \
    --local "context=$CI_PROJECT_DIR/$WORKER_CONTEXT" \
    --local "dockerfile=$CI_PROJECT_DIR/$WORKER_DOCKERFILE_DIR" \
    --opt "filename=${WORKER_DOCKERFILE_FILENAME:-Dockerfile}" \
    --opt "label:org.opencontainers.image.revision=$CI_COMMIT_SHA" \
    --metadata-file "$metadata_file" \
    --output "type=image,name=$candidate_ref,push=true"

printf '%s\n' "${WORKER_BUILD_ARGS:-}" > "$CI_PROJECT_DIR/worker-build-args.txt"
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
    printf 'WORKER_IMAGE_NAME=%s\n' "$WORKER_IMAGE_NAME"
    printf 'WORKER_CANDIDATE_TAG=%s\n' "$candidate_tag"
    printf 'WORKER_IMAGE_REF=%s\n' "$candidate_ref"
    printf 'WORKER_IMAGE_DIGEST=%s\n' "$digest"
} > "$env_file"

printf 'Published immutable candidate %s@%s\n' "$candidate_ref" "$digest"
