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
#   CODEX_APPROVAL=never|on-request|untrusted
#   CODEX_MODEL=...
#   CODEX_EXTRA_ARGS='...'
#   CODEX_BYPASS_APPROVALS=1  # uses --dangerously-bypass-approvals-and-sandbox
SANDBOX="${CODEX_SANDBOX:-workspace-write}"
APPROVAL="${CODEX_APPROVAL:-never}"
MODEL_ARGS=()
if [[ -n "${CODEX_MODEL:-}" ]]; then
  MODEL_ARGS=(-m "$CODEX_MODEL")
fi

BYPASS_ARGS=()
if [[ "${CODEX_BYPASS_APPROVALS:-0}" == "1" ]]; then
  BYPASS_ARGS=(--dangerously-bypass-approvals-and-sandbox)
fi

# shellcheck disable=SC2206
EXTRA=(${CODEX_EXTRA_ARGS:-})

{
  echo "=== codex run $STAMP ==="
  echo "cwd=$WT"
  echo "sandbox=$SANDBOX"
  echo "approval=$APPROVAL"
  echo "bypass=${CODEX_BYPASS_APPROVALS:-0}"
  echo "codex=$(command -v codex)"
  echo "version=$(codex --version 2>/dev/null || true)"
  echo "prompt_file=$PROMPT_FILE"
  echo "prompt:"
  cat "$PROMPT_FILE"
  echo
  echo "=== output ==="
} | tee "$LOG"

# Non-interactive: prompt via stdin, pin cwd to worktree.
# approval_policy is set via -c because `codex exec` has no -a flag.
set +e
codex exec \
  -C "$WT" \
  -s "$SANDBOX" \
  -c "approval_policy=\"${APPROVAL}\"" \
  --skip-git-repo-check \
  --ephemeral \
  --color never \
  --json \
  -o "$LAST_MSG" \
  "${BYPASS_ARGS[@]}" \
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
