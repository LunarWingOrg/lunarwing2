#!/bin/bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: harden_image_modes.sh <closure-root> [...]" >&2
    exit 2
fi

for root in "$@"; do
    if [ ! -d "$root" ]; then
        echo "closure root is not a directory: $root" >&2
        exit 1
    fi
    find "$root" -type d -exec chmod 00555 {} +
    find "$root" -type f -exec chmod 00444 {} +
done

if [ -n "$(find "$@" \( -perm /6000 -o -perm /222 \) -print -quit)" ]; then
    echo "closure retains set-ID or write permission bits" >&2
    exit 1
fi
