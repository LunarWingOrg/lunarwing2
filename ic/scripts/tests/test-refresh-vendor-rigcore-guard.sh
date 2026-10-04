#!/usr/bin/env bash
# Tests for the WI#292 rig-core patch guards in scripts/refresh-vendor.sh
# (WI#294): the pre-vendor clean-tree and every-marker checks, the post-vendor
# restore from git, the version-bump refusal and its discard hint, the
# RIGCORE_MARKER_FILES drift warning (whole crate, not only src/), and the
# recovery when cargo vendor itself fails or is signalled part-way (rig-core
# restore even with a dead log reader, stale .vendor-staging* cleanup).
# A stub `cargo` plays `cargo vendor` (re-extracts vendor/rig-core pristine:
# no markers, no LUNARWING-PATCH.md; a version bump also adds a new file) and
# a stub replaces the network-bound cargo-deny gate, so the harness is
# hermetic. REFRESH_VENDOR_SCRIPT overrides the script under test (e.g. an
# older revision, to show these checks fail there) and RIGCORE_TREE the
# committed rig-core tree the marker list is checked against.
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${REFRESH_VENDOR_SCRIPT:-$TEST_DIR/../refresh-vendor.sh}"
RIGCORE_TREE="${RIGCORE_TREE:-$TEST_DIR/../../vendor/rig-core}"
MARKER=LUNARWING_WI292_FINISH_REASON_PATCH
MARKER_FILES=(
  src/completion/request.rs
  src/providers/copilot/mod.rs
  src/providers/internal/openai_chat_completions_compatible.rs
  src/providers/openai/completion/mod.rs
  src/providers/openai/completion/streaming.rs
)

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
check() {  # <label> <command...>
  local label="$1"
  shift
  if "$@"; then pass "$label"; else fail "$label"; fi
}

sorted_lines() { printf '%s\n' "$@" | LC_ALL=C sort; }

# 0. One marker list everywhere: the script's RIGCORE_MARKER_FILES, this
#    harness and the committed tree (so CI catches list drift without anyone
#    running a refresh).
script_list="$(sed -n '/^RIGCORE_MARKER_FILES=(/,/^)/s/^  \([^ ]*\)$/\1/p' "$SCRIPT" | LC_ALL=C sort)"
tree_marker_files() {
  (cd "$RIGCORE_TREE" && grep -rl "$MARKER" .) | sed 's|^\./||' | grep -vx 'LUNARWING-PATCH.md' | LC_ALL=C sort
}
tree_list="$(tree_marker_files || true)"
check 'refresh-vendor.sh lists exactly the marker-bearing rig-core files' \
  [ "$script_list" = "$(sorted_lines "${MARKER_FILES[@]}")" ]
check 'the committed rig-core tree carries the marker in exactly those files' \
  [ "$tree_list" = "$(sorted_lines "${MARKER_FILES[@]}")" ]

# Hermetic git: ignore the caller's global/system config (signing, hooks) and
# any repository a calling git hook exported, so fixture commits can never
# land in the real index or object store.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid
export GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid
export CARGO_CALLS="$fixture/cargo-calls"

# ── Fixture checkout: ic/scripts/refresh-vendor.sh, ic/vendor/rig-core, ic/Cargo.lock ──
repo="$fixture/repo"
rig="$repo/ic/vendor/rig-core"
mkdir -p "$repo/ic/scripts" "$repo/.gitlab/scripts" "$fixture/bin"
cp "$SCRIPT" "$repo/ic/scripts/refresh-vendor.sh"
printf '#!/usr/bin/env bash\necho "stub cargo-deny gate ok"\n' >"$repo/.gitlab/scripts/cargo-deny-check.sh"
for rel in "${MARKER_FILES[@]}"; do
  mkdir -p "$rig/${rel%/*}"
  printf '// %s\npub fn patched() {}\n' "$MARKER" >"$rig/$rel"
done
mkdir -p "$rig/src/test_utils"
printf 'pub fn profiles() {}\n' >"$rig/src/test_utils/internal_streaming_profiles.rs"
printf '# LunarWing local patch\n\nMarker: %s\n' "$MARKER" >"$rig/LUNARWING-PATCH.md"
printf '[package]\nname = "rig-core"\nversion = "0.40.0"\n' >"$rig/Cargo.toml"
printf '{"files":{},"package":"00"}\n' >"$rig/.cargo-checksum.json"
printf 'version = 4\n\n[[package]]\nname = "rig-core"\nversion = "0.40.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n' \
  >"$repo/ic/Cargo.lock"
git -c init.defaultBranch=main init -q "$repo"
git -C "$repo" add -A
git -C "$repo" commit -qm fixture
base="$(git -C "$repo" rev-parse HEAD)"

# Stub cargo: `cargo vendor` re-extracts vendor/rig-core pristine (markers and
# LUNARWING-PATCH.md gone). STUB_VENDOR_VERSION bumps the extracted version
# (and adds a file, as a real upgrade does), STUB_VENDOR_EXIT makes the run
# fail after the rewrite (like a later crate failing to copy) and
# STUB_VENDOR_FAIL_EARLY before it (like a --locked resolution error).
# STUB_KILL_READER kills the process reading the refresh's output and
# STUB_SIGNAL_GROUP signals the stub's own process group, both after the
# rewrite.
cat >"$fixture/bin/cargo" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CARGO_CALLS"
[[ -z "${STUB_VENDOR_FAIL_EARLY:-}" ]] || exit 101
rig=vendor/rig-core
rm -f "$rig/LUNARWING-PATCH.md"
grep -rl LUNARWING_WI292_FINISH_REASON_PATCH "$rig" | while IFS= read -r f; do
  grep -v LUNARWING_WI292_FINISH_REASON_PATCH "$f" >"$f.pristine" || true
  mv "$f.pristine" "$f"
done
if [[ -n "${STUB_VENDOR_VERSION:-}" ]]; then
  printf '[package]\nname = "rig-core"\nversion = "%s"\n' "$STUB_VENDOR_VERSION" >"$rig/Cargo.toml"
  printf 'pub fn new_upstream_file() {}\n' >"$rig/src/new_upstream_file.rs"
fi
if [[ -n "${STUB_KILL_READER:-}" ]]; then
  # The process reading the refresh's output dies (Ctrl-C reached `| tee`).
  kill -KILL "$STUB_KILL_READER" 2>/dev/null
  for _ in $(seq 1 100); do
    state="$(sed -n 's/^State:[[:space:]]*\([A-Z]\).*/\1/p' "/proc/$STUB_KILL_READER/status" 2>/dev/null)"
    [[ -z "$state" || "$state" == Z ]] && break
    sleep 0.05
  done
fi
if [[ -n "${STUB_SIGNAL_GROUP:-}" ]]; then
  # Ctrl-C / tmux kill-session / job cancel: the whole process group. The
  # harness runs this under setsid; never signal the harness's own group.
  own_pgid="$(ps -o pgid= -p "$$" | tr -d ' ')"
  if [[ -n "$own_pgid" && -n "${STUB_HARNESS_PGID:-}" && "$own_pgid" != "$STUB_HARNESS_PGID" ]]; then
    kill -s "$STUB_SIGNAL_GROUP" 0
  fi
fi
exit "${STUB_VENDOR_EXIT:-0}"
SH
chmod +x "$fixture/bin/cargo"

out=""
status=0
run_refresh() {
  : >"$CARGO_CALLS"
  if out="$(cd "$repo/ic" && PATH="$fixture/bin:$PATH" bash scripts/refresh-vendor.sh 2>&1)"; then
    status=0
  else
    status=$?
  fi
}
reset_fixture() {
  git -C "$repo" reset -q --hard "$base"
  git -C "$repo" clean -qfdx
}
commit_fixture() {  # <message>
  git -C "$repo" add -A
  git -C "$repo" commit -qm "$1"
}
output_has() { [[ "$out" == *"$1"* ]]; }
output_lacks() { [[ "$out" != *"$1"* ]]; }
exited_ok() { [[ "$status" -eq 0 ]]; }
exited_with() { [[ "$status" -eq "$1" ]]; }
exited_nonzero() { [[ "$status" -ne 0 ]]; }
cargo_skipped() { [[ ! -s "$CARGO_CALLS" ]]; }
cargo_ran() { [[ -s "$CARGO_CALLS" ]]; }
rig_clean() { [[ -z "$(git -C "$repo" status --porcelain -- ic/vendor/rig-core)" ]]; }
rig_dirty() { ! rig_clean; }
patch_present() {
  local rel
  for rel in "${MARKER_FILES[@]}"; do
    grep -q "$MARKER" "$rig/$rel" || return 1
  done
  [[ -f "$rig/LUNARWING-PATCH.md" ]]
}
patch_absent() { ! grep -rq "$MARKER" "$rig/src"; }

# 1. Same-version refresh: the patch is restored and every gate runs.
run_refresh
check 'same-version refresh exits 0' exited_ok
check 'cargo vendor ran once with --locked --no-delete' grep -qx 'vendor --locked --no-delete vendor' "$CARGO_CALLS"
check 'restore is reported' output_has 'restored rig-core WI#292 patch from git'
check 'vendor set check ran' output_has 'vendor set matches synced Cargo.lock files'
check 'advisory gate ran' output_has 'stub cargo-deny gate ok'
check 'all five markers and LUNARWING-PATCH.md are back' patch_present
check 'rig-core is clean after the refresh' rig_clean
check 'LUNARWING-PATCH.md is not reported as list drift' output_lacks 'WARNING'

# 2. A committed revert of one sibling file is refused before cargo runs.
reverted="$(grep -v "$MARKER" "$rig/src/providers/openai/completion/streaming.rs")"
printf '%s\n' "$reverted" >"$rig/src/providers/openai/completion/streaming.rs"
commit_fixture 'revert streaming.rs'
run_refresh
check 'single-file revert is refused' exited_nonzero
check 'refusal names the reverted file' output_has 'vendor/rig-core/src/providers/openai/completion/streaming.rs'
check 'refusal does not name intact files' output_lacks 'openai_chat_completions_compatible.rs'
check 'cargo vendor never ran after the refusal' cargo_skipped
reset_fixture

# 3. Uncommitted rig-core edits are refused before cargo runs and are kept.
printf '// local edit\n' >>"$rig/src/completion/request.rs"
run_refresh
check 'uncommitted rig-core edit is refused' exited_nonzero
check 'refusal explains the uncommitted changes' output_has 'has uncommitted changes'
check 'cargo vendor never ran with a dirty rig-core' cargo_skipped
check 'the uncommitted edit survives' grep -q '// local edit' "$rig/src/completion/request.rs"
reset_fixture

# 4. List drift: a marker-bearing file outside RIGCORE_MARKER_FILES warns
#    (non-fatal), inside src/ and elsewhere in the crate (review S12).
printf '// %s\n' "$MARKER" >"$rig/src/extra.rs"
printf '// %s\nfn main() {}\n' "$MARKER" >"$rig/build.rs"
commit_fixture 'unlisted marker files'
run_refresh
check 'list drift does not fail the refresh' exited_ok
check 'unlisted src/ marker file is flagged' output_has 'WARNING vendor/rig-core/src/extra.rs carries the WI#292 marker'
check 'unlisted marker file outside src/ is flagged' output_has 'WARNING vendor/rig-core/build.rs carries the WI#292 marker'
check 'LUNARWING-PATCH.md is still not flagged' output_lacks 'WARNING vendor/rig-core/LUNARWING-PATCH.md'
reset_fixture

# 5. cargo vendor fails after rewriting rig-core: the committed patch is put
#    back before the script exits, so a plain re-run succeeds.
STUB_VENDOR_EXIT=101 run_refresh
check 'failed cargo vendor fails the refresh with its status' exited_with 101
check 'aborted vendor run reports the restore' output_has 'cargo vendor stopped after rewriting vendor/rig-core'
check 'patch is restored after the failed vendor run' patch_present
check 'rig-core is clean after the failed vendor run' rig_clean
run_refresh
check 're-run after a failed vendor run succeeds' exited_ok
reset_fixture

# 5b. cargo vendor fails before touching rig-core: nothing to restore.
STUB_VENDOR_FAIL_EARLY=1 run_refresh
check 'early cargo vendor failure keeps its status' exited_with 101
check 'early failure reports no restore' output_lacks 'cargo vendor stopped after rewriting'
check 'rig-core is untouched by an early failure' rig_clean
reset_fixture

# 5c. A version bump and a later cargo vendor failure: the restore also drops
#     the new version's extra files, so a plain re-run starts clean.
STUB_VENDOR_VERSION=0.41.0 STUB_VENDOR_EXIT=101 run_refresh
check 'bump plus failed vendor run keeps the cargo status' exited_with 101
check 'bump plus failed vendor run restores the patch' patch_present
check 'bump plus failed vendor run leaves no new-version files' rig_clean
reset_fixture

# 5d. The log reader dies with cargo vendor (`refresh-vendor.sh 2>&1 | tee log`
#     and Ctrl-C, or tmux kill-session): the restore must not depend on stderr.
fifo="$fixture/log.fifo"
mkfifo "$fifo"
cat "$fifo" >/dev/null &
reader=$!
: >"$CARGO_CALLS"
if (cd "$repo/ic" && STUB_KILL_READER="$reader" STUB_VENDOR_EXIT=101 PATH="$fixture/bin:$PATH" \
  bash scripts/refresh-vendor.sh) >"$fifo" 2>&1; then status=0; else status=$?; fi
wait "$reader" 2>/dev/null || true
check 'cargo vendor ran under the dying log reader' cargo_ran
check 'the run with a dead log reader fails' exited_nonzero
check 'patch is restored when the log reader died' patch_present
check 'rig-core is clean when the log reader died' rig_clean
reset_fixture

# 5e. SIGTERM to the whole process group while cargo vendor runs.
harness_pgid="$(ps -o pgid= -p "$$" 2>/dev/null | tr -d ' ' || true)"
if command -v setsid >/dev/null 2>&1 && [[ -n "$harness_pgid" ]]; then
  : >"$CARGO_CALLS"
  if out="$(cd "$repo/ic" && STUB_SIGNAL_GROUP=TERM STUB_HARNESS_PGID="$harness_pgid" \
    PATH="$fixture/bin:$PATH" setsid -w bash scripts/refresh-vendor.sh 2>&1)"; then status=0; else status=$?; fi
  check 'cargo vendor ran before the signal' cargo_ran
  check 'signalled vendor run fails' exited_nonzero
  check 'signalled vendor run restores the patch' patch_present
  check 'rig-core is clean after the signalled vendor run' rig_clean
  reset_fixture
else
  printf 'SKIP: setsid or ps not available; signal scenario not run\n'
fi

# 6. cargo vendor's own temp dirs from an interrupted run are cleared before
#    vendoring instead of failing the set check as "lockfiles drifted".
mkdir -p "$repo/ic/vendor/.vendor-stagingAbC123/serde-1.0.0" "$repo/ic/vendor/.vendor-stagingXyZ789"
: >"$repo/ic/vendor/.vendor-stagingAbC123/serde-1.0.0/Cargo.toml"
run_refresh
check 'stale cargo staging dirs do not fail the refresh' exited_ok
check 'stale cargo staging dirs are reported' output_has 'left behind by an interrupted cargo vendor run'
staging_gone() { ! compgen -G "$repo/ic/vendor/.vendor-staging*" >/dev/null; }
check 'stale cargo staging dirs are removed' staging_gone
reset_fixture

# 7. A version bump is refused and leaves the new pristine sources for the
#    manual re-apply; a re-run points at the way out.
STUB_VENDOR_VERSION=0.41.0 run_refresh
check 'version bump is refused' exited_nonzero
check 'refusal names both versions' output_has '(0.40.0 -> 0.41.0)'
check 'refusal points at the committed LUNARWING-PATCH.md' \
  output_has 'git show HEAD:ic/vendor/rig-core/LUNARWING-PATCH.md'
check 'old patch is not restored over the new version' patch_absent
check 'new pristine sources are left for the re-apply' rig_dirty
run_refresh
check 're-run with the unported bump is refused' exited_nonzero
check 're-run explains how to discard the leftover tree' \
  output_has 'git checkout HEAD -- ic/vendor/rig-core && git clean -fd -- ic/vendor/rig-core'
check 'cargo vendor never ran on the re-run' cargo_skipped
(cd "$repo" && git checkout -q HEAD -- ic/vendor/rig-core && git clean -fdq -- ic/vendor/rig-core)
run_refresh
check 'a re-run after the discard commands completes' exited_ok
reset_fixture

if ((failures > 0)); then
  printf '%d refresh-vendor rig-core guard check(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'All refresh-vendor rig-core guard checks passed\n'
