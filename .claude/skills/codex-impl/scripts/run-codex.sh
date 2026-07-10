#!/usr/bin/env bash
# Run Codex non-interactively in a worktree with a prompt file.
# usage: run-codex.sh <worktree> <prompt-file> [extra codex args...]
set -euo pipefail

WT="${1:?usage: run-codex.sh <worktree> <prompt-file> [extra codex args...]}"
PROMPT_FILE="${2:?usage: run-codex.sh <worktree> <prompt-file> [extra codex args...]}"
shift 2 || true

if [[ ! -d "$WT" ]]; then
  echo "ERROR: worktree not found: $WT" >&2
  exit 2
fi
if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "ERROR: prompt file not found: $PROMPT_FILE" >&2
  exit 2
fi
if ! command -v codex >/dev/null 2>&1; then
  echo "ERROR: codex not on PATH" >&2
  exit 127
fi

LOG_DIR="${WT}/.codex-runs"
mkdir -p "$LOG_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"
LOG="${LOG_DIR}/${STAMP}.log"
LAST_MSG="${LOG_DIR}/${STAMP}.last-message.md"

# Defaults tuned for unattended worker use. Override by exporting:
#   CODEX_SANDBOX=workspace-write|danger-full-access|read-only
#   CODEX_ASK=never|on-request|untrusted
#   CODEX_MODEL=...
#   CODEX_EXTRA_ARGS='...'
SANDBOX="${CODEX_SANDBOX:-workspace-write}"
ASK="${CODEX_ASK:-never}"
MODEL_ARGS=()
if [[ -n "${CODEX_MODEL:-}" ]]; then
  MODEL_ARGS=(-m "$CODEX_MODEL")
fi

# shellcheck disable=SC2206
EXTRA=(${CODEX_EXTRA_ARGS:-})

{
  echo "=== codex run $STAMP ==="
  echo "cwd=$WT"
  echo "sandbox=$SANDBOX"
  echo "ask=$ASK"
  echo "codex=$(command -v codex)"
  echo "version=$(codex --version 2>/dev/null || true)"
  echo "prompt_file=$PROMPT_FILE"
  echo "prompt:"
  cat "$PROMPT_FILE"
  echo
  echo "=== output ==="
} | tee "$LOG"

# Non-interactive: read prompt from file via stdin, pin cwd to worktree.
# --json keeps a machine-readable trail on stdout (also teed to log).
set +e
codex exec \
  -C "$WT" \
  -s "$SANDBOX" \
  -a "$ASK" \
  --skip-git-repo-check \
  --ephemeral \
  --color never \
  --json \
  -o "$LAST_MSG" \
  "${MODEL_ARGS[@]}" \
  "${EXTRA[@]}" \
  "$@" \
  - <"$PROMPT_FILE" 2>&1 | tee -a "$LOG"
EC=${PIPESTATUS[0]}
set -e

{
  echo
  echo "EXIT=$EC"
  echo "LAST_MESSAGE_FILE=$LAST_MSG"
  echo "LOG=$LOG"
} | tee -a "$LOG"

exit "$EC"
