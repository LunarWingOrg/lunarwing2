#!/usr/bin/env bash
set -euo pipefail

manifest="${1:?usage: download-models.sh MANIFEST [MODEL_DIR]}"
model_dir="${2:-${VOICE_MODEL_DIR:-/models}}"

command -v curl >/dev/null
command -v jq >/dev/null
command -v sha256sum >/dev/null
command -v flock >/dev/null
command -v tar >/dev/null
mkdir -p "$model_dir"

exec 9>"$model_dir/.download.lock"
flock 9

validate_name() {
    [[ "$1" =~ ^[A-Za-z0-9._-]+$ ]]
}

validate_archive() {
    local archive="$1"
    while IFS= read -r entry; do
        [[ -n "$entry" ]] || continue
        [[ "$entry" != /* \
            && "$entry" != ".." \
            && "$entry" != ../* \
            && "$entry" != */../* \
            && "$entry" != */.. ]] || return 1
    done < <(tar -tjf "$archive")
    while IFS= read -r metadata; do
        case "${metadata:0:1}" in
            -|d) ;;
            *) return 1 ;;
        esac
    done < <(tar -tvjf "$archive")
}

count="$(jq -r '.artifacts | length' "$manifest")"
[[ "$count" =~ ^[0-9]+$ ]] || { printf 'invalid model manifest\n' >&2; exit 1; }

for ((index = 0; index < count; index++)); do
    name="$(jq -r ".artifacts[$index].name" "$manifest")"
    url="$(jq -r ".artifacts[$index].url" "$manifest")"
    expected="$(jq -r ".artifacts[$index].sha256" "$manifest")"
    kind="$(jq -r ".artifacts[$index].kind" "$manifest")"
    destination="$(jq -r ".artifacts[$index].destination" "$manifest")"
    root="$(jq -r ".artifacts[$index].archive_root // empty" "$manifest")"

    validate_name "$name" && validate_name "$destination" \
        || { printf 'unsafe artifact name or destination\n' >&2; exit 1; }
    [[ "$url" == https://* ]] \
        || { printf 'model URL must use HTTPS: %s\n' "$name" >&2; exit 1; }
    [[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] \
        || { printf 'artifact %s needs an exact SHA-256\n' "$name" >&2; exit 1; }
    marker="$model_dir/.lunarvoice-${name}.sha256"
    if [[ -e "$model_dir/$destination" && -f "$marker" ]] \
        && [[ "$(tr -d '\n' <"$marker")" == "${expected,,}" ]]; then
        printf 'model already verified: %s\n' "$name"
        continue
    fi
    [[ ! -e "$model_dir/$destination" ]] \
        || { printf 'refusing to replace unverified model path: %s\n' "$destination" >&2; exit 1; }

    work="$(mktemp -d "$model_dir/.${name}.XXXXXX")"
    trap 'rm -rf "${work:-}"' EXIT
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --output "$work/artifact" "$url"
    actual="$(sha256sum "$work/artifact" | cut -d' ' -f1)"
    [[ "$actual" == "${expected,,}" ]] \
        || { printf 'SHA-256 mismatch for %s\n' "$name" >&2; exit 1; }

    case "$kind" in
        file)
            mv "$work/artifact" "$model_dir/$destination"
            ;;
        tar.bz2)
            validate_name "$root" && validate_archive "$work/artifact" \
                || { printf 'unsafe archive for %s\n' "$name" >&2; exit 1; }
            mkdir "$work/extracted"
            tar --no-same-owner --no-same-permissions -xjf "$work/artifact" -C "$work/extracted"
            [[ -e "$work/extracted/$root" ]] \
                || { printf 'archive root missing for %s\n' "$name" >&2; exit 1; }
            mv "$work/extracted/$root" "$model_dir/$destination"
            ;;
        *)
            printf 'unsupported artifact kind for %s: %s\n' "$name" "$kind" >&2
            exit 1
            ;;
    esac
    printf '%s\n' "${expected,,}" >"$marker"
    chmod -R go-w "$model_dir/$destination" "$marker"
    rm -rf "$work"
    trap - EXIT
    printf 'installed verified model: %s\n' "$name"
done
