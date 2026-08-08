#!/bin/sh
set -eu

if [ "${VOICE_AUTO_DOWNLOAD:-false}" = "true" ]; then
    manifest="${VOICE_MODEL_MANIFEST:-/models/model-manifest.json}"
    [ -f "$manifest" ] || {
        printf 'VOICE_AUTO_DOWNLOAD=true but manifest is missing: %s\n' "$manifest" >&2
        exit 1
    }
    /app/download-models.sh "$manifest" "${VOICE_MODEL_DIR:-/models}"
fi

exec /app/lunarwing-voice-sidecar
