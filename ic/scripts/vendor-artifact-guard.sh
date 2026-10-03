#!/usr/bin/env bash
# Drop stale cargo build artifacts of vendored crates that changed in place.
#
# cargo treats crates.io sources (including the vendor/ directory that
# .cargo/config.toml substitutes for crates.io) as immutable per name@version:
# it never re-checks their files. A vendored crate patched IN PLACE at the same
# version, like the WI#292 rig-core patch (vendor/rig-core/LUNARWING-PATCH.md),
# therefore leaves the pre-patch artifacts in every existing target dir, and
# the next incremental build compiles the new code against them. That is how
# upgrade-tenant failed with E0599 `finish_reason` while CI, which always
# starts from an empty target dir, passed.
#
# The guard uses the sidecar-hash pattern mt-admin uses for container configs
# (_container_config_changed in mt-admin-lib/00-common.sh). It hashes every
# vendored crate's .cargo-checksum.json (cargo verifies the crate's files
# against it, so it changes whenever the content does) and compares the hashes
# with the ones recorded for this target dir and profile. Each crate whose
# content changed at an unchanged name@version gets `cargo clean -p`, so the
# build that follows recompiles it. Version bumps need nothing: cargo rebuilds
# a new name@version by itself. An existing target dir with no recorded state
# yet has every crate that carries a LUNARWING-PATCH.md cleaned once.
#
# Run it as the user that owns the target dir, from the directory the build
# runs in: once before the build, and again with --record after the build
# succeeds (that records the baseline for a target dir that did not exist yet).
#
# Usage: vendor-artifact-guard.sh [options]
#   --manifest-dir DIR  cargo workspace root holding Cargo.lock (default: ic/)
#   --target-dir DIR    cargo target dir (default: $CARGO_TARGET_DIR, else
#                       what `cargo metadata` reports)
#   --profile NAME      cargo profile of the build (default: release)
#   --target TRIPLE     target triple of the build (default:
#                       $CARGO_BUILD_TARGET, else the host)
#   --record            only record the current vendor state
#   --dry-run           report what would be cleaned; change nothing

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IC_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
VENDOR_DIR="$IC_DIR/vendor"
STATE_NAME=".lunarwing-vendor-state"
STATE_HEADER="# lunarwing vendor-artifact-guard state v1"
PATCH_MARKER="LUNARWING-PATCH.md"

manifest_dir="$IC_DIR"
target_dir=""
profile="release"
target="${CARGO_BUILD_TARGET:-}"
record=false
dry_run=false
target_root=""
artifact_dir=""
state_file=""
work=""

log() { printf 'vendor-artifact-guard: %s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
}

cleanup() {
  if [[ -n "$work" ]]; then
    rm -rf -- "$work"
  fi
}

# Cargo's output directory name for a profile.
profile_dir() {
  case "$1" in
    dev | test) printf 'debug\n' ;;
    bench) printf 'release\n' ;;
    *) printf '%s\n' "$1" ;;
  esac
}

resolve_target_dir() {
  local dir="$target_dir"
  [[ -n "$dir" ]] || dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$dir" ]]; then
    dir="$(cd "$manifest_dir" && cargo metadata --format-version 1 --no-deps --offline 2>/dev/null \
      | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')" || dir=""
  fi
  [[ -n "$dir" ]] || dir="$manifest_dir/target"
  [[ "$dir" == /* ]] || dir="$PWD/$dir"
  printf '%s\n' "$dir"
}

# "<name@version> <sha256 of .cargo-checksum.json> <crate dir>" for every
# vendored crate, sorted by name@version. Every step is checked explicitly:
# callers run this under `|| die`, where bash ignores `set -e`.
vendor_listing() {
  (
    cd "$VENDOR_DIR" || exit 1
    shopt -s nullglob
    checksums=(*/.cargo-checksum.json)
    manifests=(*/Cargo.toml)
    [[ ${#checksums[@]} -gt 0 && ${#manifests[@]} -gt 0 ]] || exit 0
    "${SHA256[@]}" -- "${checksums[@]}" >"$work/sums" || exit 1
    # Vendored manifests are normalized: [package] carries exactly one
    # `name = "..."` and one `version = "..."` line.
    awk '
      FNR == 1 { in_pkg = 0; name = ""; version = "" }
      /^\[/ { in_pkg = ($0 == "[package]"); next }
      in_pkg && name == "" && /^name = "/ {
        name = $0; sub(/^name = "/, "", name); sub(/".*/, "", name)
      }
      in_pkg && version == "" && /^version = "/ {
        version = $0; sub(/^version = "/, "", version); sub(/".*/, "", version)
      }
      in_pkg && name != "" && version != "" && !(FILENAME in seen) {
        seen[FILENAME] = 1
        dir = FILENAME; sub(/\/Cargo\.toml$/, "", dir)
        print dir, name "@" version
      }
    ' "${manifests[@]}" >"$work/ids" || exit 1
    [[ -s "$work/ids" ]] || exit 0
    awk '
      FNR == NR { id[$1] = $2; next }
      { dir = $2; sub(/\/\.cargo-checksum\.json$/, "", dir); if (dir in id) print id[dir], $1, dir }
    ' "$work/ids" "$work/sums" | LC_ALL=C sort
  )
}

# name@version of every registry package in a Cargo.lock, sorted.
lock_identities() {
  awk '
    /^\[\[package\]\]$/ { in_pkg = 1; name = ""; version = ""; next }
    /^\[/ { in_pkg = 0; next }
    !in_pkg { next }
    /^name = "/ { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name) }
    /^version = "/ { version = $0; sub(/^version = "/, "", version); sub(/"$/, "", version) }
    /^source = "(registry|sparse)[+]/ && name != "" && version != "" { print name "@" version }
  ' "$1" | LC_ALL=C sort -u
}

# name@version of crates whose recorded checksum digest differs from now.
changed_identities() {  # <state-file> <listing>
  awk '
    FNR == NR { if ($0 !~ /^#/) recorded[$1] = $2; next }
    ($1 in recorded) && recorded[$1] != $2 { print $1 }
  ' "$1" "$2" | LC_ALL=C sort -u
}

# name@version of vendored crates that carry a LUNARWING-PATCH.md.
patched_identities() {  # <listing>
  local marker dirs=""
  for marker in "$VENDOR_DIR"/*/"$PATCH_MARKER"; do
    [[ -f "$marker" ]] || continue
    marker="${marker%/"$PATCH_MARKER"}"
    dirs+="${marker##*/} "
  done
  [[ -n "$dirs" ]] || return 0
  awk -v dirs="$dirs" '
    BEGIN { n = split(dirs, d, " "); for (i = 1; i <= n; i++) want[d[i]] = 1 }
    ($3 in want) { print $1 }
  ' "$1" | LC_ALL=C sort -u
}

state_is_valid() {
  local first=""
  [[ -f "$state_file" ]] || return 1
  IFS= read -r first <"$state_file" || true
  [[ "$first" == "$STATE_HEADER" ]]
}

# Record <listing> as the vendor state of the artifact dir. Identities listed
# in <held> keep the digest recorded in <old-state>: they changed but were not
# cleaned (this workspace does not build them), so they must still count as
# changed if the workspace starts building them again.
write_state() {  # <listing> [<held> <old-state>]
  local listing="$1" held="${2:-/dev/null}" old="${3:-/dev/null}" tmp
  tmp="$(mktemp "$artifact_dir/$STATE_NAME.XXXXXX")" || return 1
  {
    printf '%s\n' "$STATE_HEADER"
    awk -v held_file="$held" -v old_file="$old" '
      BEGIN {
        while ((getline line < held_file) > 0) held[line] = 1
        while ((getline line < old_file) > 0) {
          if (line ~ /^#/) continue
          split(line, f, " ")
          old[f[1]] = f[2]
        }
      }
      { print $1, ((($1 in held) && ($1 in old)) ? old[$1] : $2) }
    ' "$listing"
  } >"$tmp" || { rm -f -- "$tmp"; return 1; }
  if cmp -s "$tmp" "$state_file"; then
    rm -f -- "$tmp"
  else
    mv -f -- "$tmp" "$state_file"
  fi
}

clean_stale() {  # <identities-to-clean> <mode>
  local ids name
  local -a args=(clean --target-dir "$target_root" --profile "$profile")
  ids="$(tr '\n' ' ' <"$1")"
  if [[ "$2" == first ]]; then
    log "no vendor state recorded for $artifact_dir yet; cleaning locally patched vendored crates once: ${ids% }"
  else
    log "vendored crates changed in place since the last build in $artifact_dir: ${ids% }"
  fi
  [[ -z "$target" ]] || args+=(--target "$target")
  while IFS= read -r name; do
    args+=(-p "$name")
  done < <(sed 's/@[^@]*$//' "$1" | LC_ALL=C sort -u)
  if [[ "$dry_run" == true ]]; then
    log "dry run; would run in $manifest_dir: cargo ${args[*]}"
    return 0
  fi
  log "cargo ${args[*]}"
  (cd "$manifest_dir" && cargo "${args[@]}") \
    || die "cargo clean failed; refusing to build against stale artifacts of ${ids% }"
}

main() {
  local lockfile mode
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --manifest-dir | --target-dir | --profile | --target)
        [[ $# -ge 2 && -n "$2" ]] || die "$1 requires a value"
        case "$1" in
          --manifest-dir) manifest_dir="$2" ;;
          --target-dir) target_dir="$2" ;;
          --profile) profile="$2" ;;
          --target) target="$2" ;;
        esac
        shift 2
        ;;
      --record) record=true; shift ;;
      --dry-run) dry_run=true; shift ;;
      -h | --help) usage; exit 0 ;;
      *) die "unknown argument: $1 (see --help)" ;;
    esac
  done
  [[ "$profile" =~ ^[A-Za-z0-9_-]+$ ]] || die "invalid profile name: $profile"
  [[ -z "$target" || "$target" =~ ^[A-Za-z0-9_.-]+$ ]] || die "invalid target triple: $target"

  # Not a vendored checkout: cargo fetches immutable registry sources itself.
  [[ -d "$VENDOR_DIR" ]] || exit 0
  if command -v sha256sum >/dev/null 2>&1; then
    SHA256=(sha256sum)
  elif command -v shasum >/dev/null 2>&1; then
    SHA256=(shasum -a 256)
  else
    die "sha256sum (or shasum) is required"
  fi

  manifest_dir="$(cd "$manifest_dir" 2>/dev/null && pwd)" || die "no such directory: $manifest_dir"
  lockfile="$manifest_dir/Cargo.lock"
  if [[ ! -f "$lockfile" ]]; then
    log "no Cargo.lock in $manifest_dir; nothing to check"
    exit 0
  fi
  target_root="$(resolve_target_dir)"
  artifact_dir="$target_root${target:+/$target}/$(profile_dir "$profile")"
  state_file="$artifact_dir/$STATE_NAME"
  # Nothing built for this profile yet, so nothing can be stale. The --record
  # run after the build stores the baseline once cargo has created the dir.
  [[ -d "$artifact_dir" ]] || exit 0

  work="$(mktemp -d)"
  trap cleanup EXIT
  vendor_listing >"$work/listing" || die "could not hash the vendored crates in $VENDOR_DIR"

  if [[ "$record" == true ]]; then
    if [[ "$dry_run" == false ]]; then
      write_state "$work/listing" || die "could not record the vendor state in $state_file"
    fi
    return 0
  fi

  if state_is_valid; then
    mode=changed
    changed_identities "$state_file" "$work/listing" >"$work/stale" \
      || die "could not compare with the vendor state in $state_file"
  else
    mode=first
    patched_identities "$work/listing" >"$work/stale" \
      || die "could not list the locally patched crates in $VENDOR_DIR"
  fi
  lock_identities "$lockfile" >"$work/locked" || die "could not read $lockfile"
  LC_ALL=C comm -12 "$work/stale" "$work/locked" >"$work/clean"
  LC_ALL=C comm -23 "$work/stale" "$work/locked" >"$work/held"

  if [[ -s "$work/clean" ]]; then
    clean_stale "$work/clean" "$mode"
  elif [[ "$dry_run" == true ]]; then
    log "no stale vendored-crate artifacts in $artifact_dir"
  fi
  [[ "$dry_run" == false ]] || return 0
  if [[ "$mode" == changed ]]; then
    write_state "$work/listing" "$work/held" "$state_file"
  else
    write_state "$work/listing"
  fi || die "could not record the vendor state in $state_file"
}

main "$@"
