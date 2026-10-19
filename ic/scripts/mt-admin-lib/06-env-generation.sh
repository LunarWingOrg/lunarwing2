#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
# shellcheck disable=SC2164  # bare `cd` is covered by `set -e` from the composition root
# shellcheck disable=SC2178  # DarkIRC argument parsers use intentional array namerefs
#
# 06-env-generation.sh — part of the lunarwing-mt-admin library.
#
# Tenant env-file generation (lunarwing/bridge/tensorzero/darkirc-
# adapter/weechat), XMPP allow-list builders, DarkIRC writer lock + contacts +
# managed exchange, SSH harness provisioning, env patching, and Gotify config.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# ── Environment file generation ──────────────────────────────────────────────

# Echo the existing VALUE of KEY from a tenant env file (empty if file/key absent).
# Used to PRESERVE secrets across re-runs so re-provisioning never rotates them.
_env_existing() {  # <env_file> <KEY>
  _read_env_value "$1" "$2"
}

_env_set() {  # <env_file> <KEY> <VALUE>
  local path="$1" key="$2" value="$3" tmp parent owner
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid environment key: $key"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
    || die "environment value for $key contains a newline"
  [[ -f "$path" && ! -L "$path" ]] || die "environment file missing or unsafe: $path"
  parent="$(dirname "$path")"
  darkirc_path_components_safe "$path" || die "unsafe environment path: $path"
  owner="$(stat -c %U -- "$parent")" || return 1
  if [[ "$owner" != root ]]; then
    _tenant_env_replace "$owner" "$path" "$key" "$value"
    return
  fi
  # Root-owned staging files are allowed only in a root-private directory.
  # No tenant can replace a name between mktemp and the following operations.
  local parent_mode
  parent_mode="$(stat -c %a -- "$parent")" || return 1
  [[ "$parent_mode" =~ ^[0-7]+$ ]] || return 1
  (( (8#$parent_mode & 022) == 0 )) || return 1
  [[ "$(stat -c %U -- "$path")" == root && "$(stat -c %h -- "$path")" == 1 ]] || return 1
  (
    tmp="$(mktemp "$parent/.env-set.XXXXXX")" || exit 1
    trap 'rm -f -- "$tmp"' EXIT
    awk -v prefix="$key=" -v replacement="$key=$value" '
      index($0, prefix) != 1 { print }
      END { print replacement }
    ' "$path" >"$tmp" || exit 1
    chmod --reference="$path" "$tmp" && chown --reference="$path" "$tmp" \
      && mv -T -- "$tmp" "$path"
  )
}

validate_study_notes_path() {  # <absolute path> <label>
  [[ "$1" =~ ^/[A-Za-z0-9._/-]+$ ]] \
    || die "$2 must be an absolute path without whitespace or shell metacharacters"
}

validate_study_notes_home() {  # <absolute checkout path>
  local home="$1"
  validate_study_notes_path "$home" "Study Notes home"
  [[ -d "$home" ]] || die "Study Notes home directory missing: $home"
  [[ -f "$home/study_notes_mcp.py" ]] || die "Study Notes file missing: $home/study_notes_mcp.py"
  [[ -f "$home/memorydb.py" ]] || die "Study Notes file missing: $home/memorydb.py"
  [[ -f "$home/agent-notes.sqlite3" ]] || die "Study Notes file missing: $home/agent-notes.sqlite3"
  [[ -d "$home/agent-notes-by-agent" ]] || die "Study Notes directory missing: $home/agent-notes-by-agent"
  [[ -f "$home/skills/study-notes/SKILL.md" ]] || die "Study Notes skill missing: $home/skills/study-notes/SKILL.md"
  local home_real relative
  home_real="$(readlink -f -- "$home")"
  for relative in \
    study_notes_mcp.py \
    memorydb.py \
    agent-notes.sqlite3 \
    agent-notes-by-agent \
    skills/study-notes/SKILL.md; do
    [[ "$(readlink -f -- "$home/$relative")" == "$home_real/$relative" ]] \
      || die "Study Notes code path must not be a symlink: $home/$relative"
  done
}

validate_study_notes_state_home() {  # <absolute state path>
  local state_home="$1"
  validate_study_notes_path "$state_home" "Study Notes state home"
  [[ -d "$state_home" ]] || die "Study Notes state directory missing: $state_home"
  [[ -f "$state_home/agent-notes.sqlite3" ]] \
    || die "Study Notes state database missing: $state_home/agent-notes.sqlite3"
  [[ -d "$state_home/agent-notes-by-agent" ]] \
    || die "Study Notes state notes directory missing: $state_home/agent-notes-by-agent"
  local state_real database_real notes_real
  state_real="$(readlink -f -- "$state_home")"
  database_real="$(readlink -f -- "$state_home/agent-notes.sqlite3")"
  notes_real="$(readlink -f -- "$state_home/agent-notes-by-agent")"
  [[ "$database_real" == "$state_real/agent-notes.sqlite3" ]] \
    || die "Study Notes state database must not be a symlink"
  [[ "$notes_real" == "$state_real/agent-notes-by-agent" ]] \
    || die "Study Notes notes directory must not be a symlink"
}

# WI#423: these values feed quadlet `Volume=` mount sources for the worker
# containers — a swapped read could mount another tenant's study-notes
# checkout into this tenant's worker, so they come from the tenant-side
# per-key reader (symlink/ownership/link-count/mode checks as the tenant).
# An absent env file reads as empty (no mount); an unsafe path refuses, and
# the render/start callers abort fail-closed on that refusal.
tenant_study_notes_home() {  # <tenant>
  read_tenant_env_value "$1" "$(tenant_env_dir "$1")/lunarwing.env" LW2STUDYMCP_HOME
}

tenant_study_notes_state_home() {  # <tenant>
  read_tenant_env_value "$1" "$(tenant_env_dir "$1")/lunarwing.env" LW2STUDYMCP_STATE_HOME
}

tenant_study_notes_allow_write() {  # <tenant>
  read_tenant_env_value "$1" "$(tenant_env_dir "$1")/lunarwing.env" LW2STUDYMCP_ALLOW_WRITE
}

study_notes_write_enabled() {  # <raw boolean>
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    ""|0|false|FALSE|no|NO|off|OFF) return 1 ;;
    *) die "LW2STUDYMCP_ALLOW_WRITE must be a boolean" ;;
  esac
}

publish_study_notes_generation() {  # <checkout home>
  local home="$1" revision manifest tmp published_at
  validate_study_notes_home "$home"
  revision="$(git -C "$home" rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" \
    || die "Study Notes checkout has no valid HEAD commit: $home"
  [[ "$revision" =~ ^[0-9a-fA-F]{7,128}$ ]] \
    || die "Study Notes checkout returned an invalid revision: $home"
  published_at="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  manifest="$home/.git/lunarwing-study-notes-generation.json"
  tmp="$home/.git/.lunarwing-study-notes-generation.$$.tmp"
  ( umask 022
    printf '{"schema_version":1,"source_revision":"%s","published_at":"%s"}\n' \
      "$revision" "$published_at" > "$tmp"
  ) || die "failed to stage Study Notes generation manifest: $tmp"
  chmod 0644 "$tmp"
  mv -fT -- "$tmp" "$manifest" \
    || die "failed to publish Study Notes generation manifest: $manifest"
  say "published Study Notes generation: $revision"
}

sync_study_notes_checkout() {  # <checkout home>
  # Clone the shared lw2studymcp checkout when missing, or fast-forward an
  # existing clean git checkout to the latest upstream. A non-git directory or
  # a dirty checkout is never modified — the operator must repair it by hand.
  local home="$1" parent manifest last_good
  validate_study_notes_path "$home" "Study Notes home"
  require_cmd git
  parent="$(dirname -- "$home")"
  manifest="$home/.git/lunarwing-study-notes-generation.json"
  last_good="$home/.git/.lunarwing-study-notes-generation.last-good"
  if [[ ! -d "$parent" ]]; then
    install -d -m 0755 -- "$parent" \
      || die "failed to create Study Notes parent directory: $parent"
  fi
  # Serialize concurrent updates and invalidate the publication barrier before
  # mutating files. A tenant scan spanning an update therefore discards its
  # pass when the before/after manifest check observes the missing/new marker.
  (
    flock -x 200
    # Keep the current manifest aside so a transient sync failure (e.g. a
    # network blip during git pull) can restore the last-good generation
    # instead of leaving tenants without any published one. Restoration only
    # happens while HEAD still matches the recorded revision: a manifest
    # claiming the old revision over moved-forward content would defeat the
    # before/after scan barrier, so a partial move keeps the barrier down.
    cp -f -- "$manifest" "$last_good" 2>/dev/null || true
    restore_last_good_generation() {
      local revision recorded
      [[ -f "$last_good" ]] || return 0
      revision="$(git -C "$home" rev-parse --verify 'HEAD^{commit}' 2>/dev/null || true)"
      recorded="$(jq -r '.source_revision // empty' "$last_good" 2>/dev/null || true)"
      if [[ -z "$revision" || "$revision" != "$recorded" ]]; then
        rm -f -- "$last_good"
        say "WARNING: Study Notes sync moved the checkout; last-good generation stays invalidated"
        return 0
      fi
      if mv -f -- "$last_good" "$manifest"; then
        say "retained last-good Study Notes generation after a failed sync"
      else
        say "WARNING: failed to restore the last-good Study Notes generation manifest"
      fi
    }
    rm -f -- "$manifest"
    if [[ ! -e "$home" ]]; then
      say "cloning Study Notes MCP checkout: $STUDY_NOTES_REPO -> $home"
      git clone --quiet "$STUDY_NOTES_REPO" "$home" \
        || { restore_last_good_generation; die "Study Notes clone failed: $STUDY_NOTES_REPO"; }
    else
      [[ -d "$home" ]] || die "Study Notes home is not a directory: $home"
      [[ -d "$home/.git" ]] \
        || die "Study Notes home is not a git checkout; cannot sync to latest: $home"
      [[ -z "$(git -C "$home" status --porcelain 2>/dev/null)" ]] \
        || die "Study Notes checkout has local changes; refusing to sync: $home"
      say "syncing Study Notes MCP checkout to latest upstream: $home"
      git -C "$home" pull --ff-only --quiet || {
        restore_last_good_generation
        die "Study Notes checkout could not fast-forward to latest: $home (repair or remove it)"
      }
    fi
    rm -f -- "$last_good"
    publish_study_notes_generation "$home"
  ) 200>"$parent/.study-notes-sync.lock"
}

# Resolve a tenant's XMPP JID: an explicit <jid> wins; else PRESERVE the one
# already in lunarwing.env; else fall back to <name>@xmpp.localhost (first-time
# provision). lunarwing.env is the authoritative source — the bridge binary
# reads only XMPP_BRIDGE_{BIND,TOKEN,MAX_MESSAGES} and receives the JID from the
# daemon via POST /v1/configure, so a JID lost here silently repoints the tenant.
# Without this, re-running add-tenant without --xmpp-domain/--xmpp-jid reset a
# live tenant to the non-resolving xmpp.localhost default and the bridge sat in
# an endless connect-retry while /v1/status still reported configured+running.
resolve_tenant_xmpp_jid() {  # <name> [explicit_jid]
  local name="$1" explicit="${2:-}"
  if [[ -n "$explicit" ]]; then
    printf '%s\n' "$explicit"
    return 0
  fi
  local existing
  # WI#423: read through the tenant-side per-key reader. An unsafe live path
  # refuses loudly instead of silently resetting the JID to xmpp.localhost
  # (which would repoint a live tenant's bridge); an absent file still reads
  # as empty so first provisioning keeps its default.
  existing="$(read_tenant_env_value "$name" "$(tenant_env_dir "$name")/lunarwing.env" XMPP_JID)" \
    || die "could not safely read XMPP_JID from $(tenant_env_dir "$name")/lunarwing.env as tenant '$name'"
  printf '%s\n' "${existing:-$name@xmpp.localhost}"
}

# Build the XMPP_ALLOW_FROM comma-separated value: owner JID first, then any
# extra JIDs from --xmpp-allow-from (comma-separated), deduped (owner JID and
# duplicate extras collapse), surrounding whitespace trimmed. <owner_jid> may
# be empty only in error paths; <extras_csv> is the raw flag value.
build_xmpp_allow_from() {  # <owner_jid> <extras_csv>
  local owner="$1"
  local extras="$2"
  local seen=""
  local result=""
  local jid
  # Owner first (skip if empty, though it normally isn't). `seen` uses leading
  # and trailing commas so substring matching on ",<jid>," is unambiguous.
  if [[ -n "$owner" ]]; then
    result="$owner"
    seen=",$owner,"
  fi
  # Extras: split on comma, trim whitespace, dedupe
  if [[ -n "$extras" ]]; then
    local IFS=','
    read -ra parts <<< "$extras"
    for jid in "${parts[@]}"; do
      jid="$(echo "$jid" | xargs)"   # trim leading/trailing whitespace
      [[ -n "$jid" ]] || continue
      [[ "$seen" == *",$jid,"* ]] && continue
      result="${result:+$result,}$jid"
      seen="${seen}$jid,"
    done
  fi
  echo "$result"
}

# Build the XMPP_ALLOW_FROM_JSON value: same semantics as build_xmpp_allow_from
# but emits a JSON array. Each JID is wrapped in double quotes; no escaping is
# applied (XMPP JIDs do not contain characters that require JSON escaping under
# the XEP-0029 node/domain rules in normal use). Output is a single line.
build_xmpp_allow_from_json() {  # <owner_jid> <extras_csv>
  local owner="$1"
  local extras="$2"
  local csv
  csv="$(build_xmpp_allow_from "$owner" "$extras")"
  local IFS=','
  local parts=()
  [[ -n "$csv" ]] && read -ra parts <<< "$csv"
  local quoted=()
  local jid
  for jid in "${parts[@]}"; do
    quoted+=("\"$jid\"")
  done
  echo "[${quoted[*]}]" | tr ' ' ',' | sed 's/,,*/,/g; s/^\[,/[/; s/,\]$/\]/'
}

# ── WeeChat relay auto-bootstrap ──────────────────────────────────────────────

# WeeChat home directory for a tenant.
tenant_weechat_home() {  # <tenant>
  printf '%s/.config/weechat' "$(tenant_home "$1")"
}

# Read the LAST literal value of KEY from an env file without sourcing it.
# Returns empty if file or key is absent. Never prints the file's other content.
_read_env_value() {  # <file> <KEY>
  [[ -f "$1" ]] || return 0
  local value
  value="$(sed -n "s/^$2=//p" "$1" | tail -1)"
  # Current writers use systemd-compatible double quotes. Keep decoding the
  # older onboarding client's single-quoted representation during migration.
  if [[ "$value" =~ ^\"(.*)\"$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\"/\"}"
    value="${value//\\\\/\\}"
  elif [[ "$value" =~ ^\'(.*)\'$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\'/\'}"
    value="${value//\\\\/\\}"
  fi
  printf '%s\n' "$value"
}

# Read one value without opening tenant-controlled paths as root. The tenant-side
# process rejects unsafe ownership, permissions, symlinks, and shared hard links.
# The inner bash runs with --norc --noprofile: stdout is the value transport, so
# a host whose bash sources a banner-printing rc even for -c shells (the dev VM)
# must not be able to corrupt the captured value (WI#423 widened this helper's
# reach from the kimi lane to most env reads).
read_tenant_env_value() {  # <tenant> <file> <KEY>
  local name="$1" target="$2" key="$3" value
  local -a drop=(sudo -u "$name")
  [[ "$(id -un)" != "$name" ]] || drop=()
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid tenant env key"
  darkirc_path_components_safe "$target" || die "unsafe tenant env path: $target"
  # First add_tenant pass: the target does not exist yet AND the tenant
  # account has not been created (create_tenant_user runs later), so the
  # tenant-side sudo below could only fail on the unknown account. That shape
  # reads as absent (run-166 LF-1) — an existing target without an account is
  # not this path and still refuses at the sudo boundary.
  if [[ ! -e "$target" && ! -L "$target" ]] && ! id -u -- "$name" >/dev/null 2>&1; then
    return 0
  fi
  value="$("${drop[@]}" env -i PATH="/usr/bin:/bin" \
    TARGET_PATH="$target" TARGET_KEY="$key" bash --norc --noprofile -c '
      set -euo pipefail
      target="$TARGET_PATH"
      parent="${target%/*}"
      uid="$(id -u)"
      # A parent that does not exist at all means nothing was provisioned yet
      # (the first add_tenant pass reads before create_tenant_user makes the
      # env dir): read as absent. But -e cannot tell ENOENT from EACCES on an
      # untraversable ancestor, so before treating the parent as absent, walk
      # up to the deepest VISIBLE ancestor and require it to be a searchable,
      # non-symlink directory — otherwise the absence is not provable and the
      # read refuses (run-166 LF-7; this is the fail-open path an empty-value
      # refusal elsewhere would otherwise miss).
      if [[ ! -e "$parent" && ! -L "$parent" ]]; then
        probe="${parent%/*}"
        while [[ -n "$probe" && "$probe" != "/" && ! -e "$probe" && ! -L "$probe" ]]; do
          probe="${probe%/*}"
        done
        [[ -z "$probe" || ( -d "$probe" && ! -L "$probe" && -x "$probe" ) ]] || exit 73
        exit 0
      fi
      [[ -d "$parent" && ! -L "$parent" ]] || exit 73
      parent_owner="$(stat -c %u "$parent" 2>/dev/null || printf -- -1)"
      parent_mode="$(stat -c %a "$parent" 2>/dev/null || true)"
      [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
      (( (8#$parent_mode & 077) == 0 )) || exit 73
      [[ ! -e "$target" ]] && exit 0
      [[ -f "$target" && ! -L "$target" ]] || exit 73
      owner="$(stat -c %u "$target" 2>/dev/null || printf -- -1)"
      mode="$(stat -c %a "$target" 2>/dev/null || true)"
      links="$(stat -c %h "$target" 2>/dev/null || printf 0)"
      [[ "$owner" == "$uid" && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || exit 73
      (( (8#$mode & 077) == 0 && (8#$mode & 0400) != 0 )) || exit 73
      prefix="$TARGET_KEY="
      value=""
      while IFS= read -r line || [[ -n "$line" ]]; do
        [[ "$line" == "$prefix"* ]] && value="${line#"$prefix"}"
      done <"$target"
      printf "%s\n" "$value"
    ')" || return 1
  if [[ "$value" =~ ^\"(.*)\"$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\"/\"}"
    value="${value//\\\\/\\}"
  elif [[ "$value" =~ ^\'(.*)\'$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\'/\'}"
    value="${value//\\\\/\\}"
  fi
  printf '%s\n' "$value"
}

# Write stdin to a tenant-owned file through a same-directory candidate. Root
# never opens the destination for writing and secret contents never enter argv.
# CONTRACT: stdin must already be a complete, validated result (a regular file
# or an infallible producer). Never pipe a fallible producer straight in: this
# writer commits whatever bytes it reads and cannot observe an upstream
# failure. Stage the producer's output (root-private mktemp), check its exit
# status, and only then redirect the staged file here.
write_tenant_file_atomic() {  # <tenant> <file> [mode] [private|owned-parent]
  local name="$1" target="$2" mode="${3:-600}" parent_policy="${4:-private}"
  local -a drop=(sudo -u "$name")
  [[ "$(id -un)" != "$name" ]] || drop=()
  [[ "$mode" =~ ^[0-7]{3,4}$ ]] || die "invalid tenant file mode"
  [[ "$parent_policy" == private || "$parent_policy" == owned-parent ]] || return 1
  # Walk the ANCESTOR components here (a symlinked or non-absolute parent path
  # is a hard die — the caller's target directory shape is already hostile).
  # The final component's own symlink check belongs to the tenant-side writer's
  # `! -L` refusal (exit 73) so callers holding a failure contract (rc 2 in the
  # worker-config upgrade helpers, || die elsewhere) see a refusal instead of a
  # bare process abort (WI#390, review IG-3).
  local target_parent="${target%/*}"
  [[ "$target" == /* && "$target_parent" != "$target" && -n "$target_parent" ]] \
    || die "unsafe tenant file path: $target"
  darkirc_path_components_safe "$target_parent" \
    || die "unsafe tenant file path: $target"
  "${drop[@]}" env -i PATH="/usr/bin:/bin" \
    TARGET_PATH="$target" TARGET_MODE="$mode" PARENT_POLICY="$parent_policy" bash -c '
      set -euo pipefail
      target="$TARGET_PATH"
      parent="${target%/*}"
      base="${target##*/}"
      uid="$(id -u)"
      [[ -d "$parent" && ! -L "$parent" ]] || exit 73
      parent_owner="$(stat -c %u "$parent" 2>/dev/null || printf -- -1)"
      parent_mode="$(stat -c %a "$parent" 2>/dev/null || true)"
      [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
      if [[ "$PARENT_POLICY" == private ]]; then
        (( (8#$parent_mode & 077) == 0 )) || exit 73
      else
        (( (8#$parent_mode & 022) == 0 )) || exit 73
      fi
      [[ ! -L "$target" && ( ! -e "$target" || -f "$target" ) ]] || exit 73
      if [[ -e "$target" ]]; then
        owner="$(stat -c %u "$target" 2>/dev/null || printf -- -1)"
        links="$(stat -c %h "$target" 2>/dev/null || printf 0)"
        [[ "$owner" == "$uid" && "$links" == 1 ]] || exit 73
      fi
      umask 077
      tmp="$(mktemp "$parent/.${base}.next.XXXXXX")"
      trap '"'"'rm -f -- "$tmp"'"'"' EXIT
      cat >"$tmp"
      chmod "$TARGET_MODE" "$tmp"
      mv -T -f -- "$tmp" "$target"
      trap - EXIT
    '
}

# Append complete lines (stdin) to a tenant-owned env file without root ever
# opening the tenant-writable path (WI#390): the current content is staged out
# (read as the tenant), the new lines land on the staged copy, and the result
# is committed through the tenant-side atomic writer. A pre-placed or raced
# symlink at the destination cannot redirect the write.
_tenant_env_append() {  # <tenant> <env_file> — new lines on stdin
  local name="$1" path="$2" staged rc=1
  staged="$(mktemp)" || return 1
  if tenant_cat "$name" "$path" >"$staged" \
    && cat >>"$staged" \
    && write_tenant_file_atomic "$name" "$path" 600 <"$staged"; then
    rc=0
  fi
  rm -f -- "$staged"
  return "$rc"
}

# Apply one sed expression in place to a tenant-owned env file without root
# ever opening the tenant-writable path (WI#390). GNU sed -i follows symlinks,
# so running it as root directly on the tenant path would rewrite an
# attacker-chosen file; the expression is applied to the staged copy instead.
_tenant_env_sed() {  # <tenant> <env_file> <sed-expression>
  local name="$1" path="$2" expr="$3" staged rc=1
  staged="$(mktemp)" || return 1
  if tenant_cat "$name" "$path" >"$staged" \
    && sed -i -e "$expr" -- "$staged" \
    && write_tenant_file_atomic "$name" "$path" 600 <"$staged"; then
    rc=0
  fi
  rm -f -- "$staged"
  return "$rc"
}

# Read-side counterpart of the tenant-side writers (WI#390, review IG-1): root
# must never resolve a tenant-writable path for its VALUE reads either. A
# symlink present for the read window hands root another tenant's env, and the
# patch/render then writes those values (COORD_ADMIN_TOKEN, GATEWAY_AUTH_TOKEN,
# SECRETS_MASTER_KEY, XMPP_PASSWORD, KIMI_API_KEY, …) back into the tenant's
# own file. Snapshot the file once AS THE TENANT — a foreign 0600 file in a
# 0700 dir is unreadable to the tenant-side read, so a cross-tenant symlink
# dies here instead of leaking — and point every value/presence read at the
# root-private snapshot. The tenant-side writers still re-read the live file at
# commit time, so a post-snapshot swap fails closed there. A missing path
# snapshots as empty (first-render shape); an unreadable or dangling one dies.
# The result is delivered through a nameref (never a command substitution) so
# the die stays fatal under a caller's suppressed errexit. The caller must rm
# the snapshot on every path that does not die first.
_snapshot_tenant_env() {  # <tenant> <env_file> <out_var>
  local name="$1" path="$2" snapshot
  local -n _snapshot_out="$3"
  snapshot="$(mktemp)" || die "could not snapshot $path"
  if [[ -e "$path" || -L "$path" ]]; then
    if ! tenant_cat "$name" "$path" >"$snapshot"; then
      rm -f -- "$snapshot"
      die "could not read $path as tenant '$name'"
    fi
  fi
  _snapshot_out="$snapshot"
}

_quote_env_value() {  # <one-line value>
  local value="$1"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
    || die "environment values cannot contain newlines"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  printf '"%s"' "$value"
}

_read_secret_file() {  # <path> <description>
  local path="$1" description="$2" value
  [[ -f "$path" && -r "$path" ]] || die "$description file is not readable: $path"
  value="$(<"$path")"
  [[ -n "$value" ]] || die "$description file is empty: $path"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
    || die "$description file must contain exactly one line"
  printf '%s\n' "$value"
}

# Success when <dir> contains any entry (normal or dotfile).
_weechat_config_dir_has_entries() {  # <dir>
  [[ -d "$1" ]] || return 1
  local count
  count="$(ls -A "$1" 2>/dev/null | wc -l)"
  [[ "$count" -gt 0 ]]
}

# Validate a generated WeeChat relay config directory.
# Requires relay.conf with the literal ${env:RELAY_PASSWORD} expression,
# bind_address=127.0.0.1, ipv6=off, allow_empty_password=off, [api] section with
# api=<port>, and rejects any occurrence of the plaintext password.
_weechat_validate_relay_config() {  # <dir> <port> <plaintext_password>
  local dir="$1" port="$2" plaintext="$3"
  local conf="$dir/relay.conf"
  [[ -f "$conf" ]] || return 1
  # Reject plaintext password leak
  if [[ -n "$plaintext" ]] && grep -qF -- "$plaintext" "$conf"; then
    return 1
  fi
  # Require literal env expression
  grep -qF '${env:RELAY_PASSWORD}' "$conf" || return 1
  # Require loopback bind
  grep -q 'bind_address.*127\.0\.0\.1' "$conf" || return 1
  # Require allow_empty_password = off
  grep -q 'allow_empty_password.*off' "$conf" || return 1
  # An IPv4 loopback bind is invalid while WeeChat's relay IPv6 mode is enabled.
  grep -q '^[[:space:]]*ipv6[[:space:]]*=[[:space:]]*off[[:space:]]*$' "$conf" || return 1
  # Require [api] section
  grep -q '\[api\]' "$conf" || return 1
  # Require api = <port>
  grep -q "^[[:space:]]*api[[:space:]]*=[[:space:]]*${port}\$" "$conf" || return 1
  return 0
}

# Invoke WeeChat as the tenant to generate a relay.conf in <temp_dir>.
# The plaintext password is read from <lunarwing_env> and passed via
# environment inheritance only; never in argv.
_weechat_generate_relay_config() {  # <tenant> <temp_dir> <port> <lunarwing_env>
  local tenant="$1" temp_dir="$2" relay_port="$3" lunarwing_env="$4"
  local relay_password weechat_bin
  # WI#423: tenant-side per-key read — the value is passed to a WeeChat run as
  # the tenant, so a symlink swap must refuse rather than hand it the other
  # tenant's relay credential.
  relay_password="$(read_tenant_env_value "$tenant" "$lunarwing_env" RELAY_PASSWORD)" || return 1
  [[ -n "$relay_password" ]] || return 1
  weechat_bin="$(command -v weechat 2>/dev/null || true)"
  if [[ -z "$weechat_bin" ]]; then
    printf 'error: required command not found: weechat\n' >&2
    return 1
  fi
  # The backslash before ${env:RELAY_PASSWORD} is mandatory: WeeChat evaluates
  # --run-command arguments before /set stores the value, so the escape ensures
  # the literal expression is written to relay.conf, not the resolved password.
  RELAY_PASSWORD="$relay_password" sudo --preserve-env=RELAY_PASSWORD -u "$tenant" \
    "$weechat_bin" --dir "$temp_dir" \
    --run-command '/set relay.network.password "\${env:RELAY_PASSWORD}"' \
    --run-command '/set relay.network.allow_empty_password off' \
    --run-command '/set relay.network.ipv6 off' \
    --run-command '/set relay.network.bind_address "127.0.0.1"' \
    --run-command "/relay add api ${relay_port}" \
    --run-command '/set weechat.look.save_layout_on_exit buffers' \
    --run-command '/save' \
    --run-command '/quit'
}

# Full one-shot relay bootstrap for a tenant: generate, validate, atomically
# promote. Rejects non-empty existing config. Cleans all failure paths.
configure_weechat_relay() {  # <tenant>
  local tenant="$1"
  local target_home relay_port lunarwing_env relay_password

  target_home="$(tenant_weechat_home "$tenant")"
  relay_port="$(ports_get "$tenant" weechat)"
  if [[ -z "$relay_port" || "$relay_port" == "0" ]]; then
    printf 'error: no weechat port allocated for tenant %q\n' "$tenant" >&2
    return 1
  fi
  lunarwing_env="$(tenant_env_dir "$tenant")/lunarwing.env"
  if [[ ! -f "$lunarwing_env" ]]; then
    printf 'error: tenant env file not found: %s\n' "$lunarwing_env" >&2
    return 1
  fi
  relay_password="$(read_tenant_env_value "$tenant" "$lunarwing_env" RELAY_PASSWORD)" \
    || { printf 'error: could not safely read RELAY_PASSWORD from %s as tenant %q\n' "$lunarwing_env" "$tenant" >&2; return 1; }
  if [[ -z "$relay_password" ]]; then
    printf 'error: RELAY_PASSWORD missing from %s\n' "$lunarwing_env" >&2
    return 1
  fi

  # Invariant: the weechat.env credential file must exist before the
  # preserve-and-fail check so legacy existing configs receive the file
  # required by the rendered systemd/OpenRC service unit.
  #
  # Non-empty legacy config (preserve-and-fail): backfill weechat.env only
  # when it does not exist yet, then return 1 — existing env values must
  # survive alongside the existing config.
  local weechat_env_file
  weechat_env_file="$(tenant_env_dir "$tenant")/weechat.env"
  if [[ -d "$target_home" ]] && _weechat_config_dir_has_entries "$target_home"; then
    if [[ ! -f "$weechat_env_file" ]]; then
      if ! _write_weechat_env "$tenant" "$relay_password"; then
        printf 'error: unable to write the dedicated WeeChat credential environment\n' >&2
        return 1
      fi
    fi
    printf 'error: WeeChat config already exists at %s — configure-weechat-relay never overwrites existing config\n' "$target_home" >&2
    return 1
  fi

  # Fresh generation path: atomically synchronize weechat.env to the current
  # lunarwing.env password before generating config. This overwrites stale
  # values left from a prior tenant or aborted run.
  if ! _write_weechat_env "$tenant" "$relay_password"; then
    printf 'error: unable to write the dedicated WeeChat credential environment\n' >&2
    return 1
  fi

  # Temp directory as a sibling of the target (same filesystem for atomic rename).
  # Both directories are created AS the tenant: a root mkdir/chown here would
  # follow a symlink the tenant planted at ~/.config (WI#416).
  local config_parent temp_home
  config_parent="$(dirname "$target_home")"
  if ! sudo -u "$tenant" mkdir -p "$config_parent"; then
    printf 'error: unable to prepare WeeChat config parent: %s\n' "$config_parent" >&2
    return 1
  fi
  temp_home="$(sudo -u "$tenant" mktemp -d "${config_parent}/.weechat-bootstrap.XXXXXX")" || {
    printf 'error: unable to create WeeChat bootstrap directory under %s\n' "$config_parent" >&2
    return 1
  }

  (
    trap '[[ -n "${temp_home:-}" && -d "$temp_home" ]] && rm -rf -- "$temp_home"' EXIT

    # Generate via WeeChat one-shot.
    if ! _weechat_generate_relay_config "$tenant" "$temp_home" "$relay_port" "$lunarwing_env"; then
      return 1
    fi

    # Validate the generated config.
    if ! _weechat_validate_relay_config "$temp_home" "$relay_port" "$relay_password"; then
      say "generated WeeChat relay config failed validation"
      return 1
    fi

    # Atomically promote: remove empty target if present, then rename.
    if [[ -d "$target_home" ]]; then
      if ! rmdir "$target_home" 2>/dev/null; then
        printf 'error: target %s exists and is not empty (cannot promote)\n' "$target_home" >&2
        return 1
      fi
    fi
    if ! mv -T -- "$temp_home" "$target_home"; then
      return 1
    fi
    temp_home=""
  )

  local rc=$?
  if [[ $rc -ne 0 ]]; then
    return $rc
  fi

  # No root chown of the promoted tree: the parent dir, the bootstrap temp and
  # the generated config were all created as the tenant, so the promoted tree
  # is already tenant-owned. A root chown -R here would resolve through the
  # tenant-controlled ~/.config and, after a post-promote symlink swap, hand a
  # victim's tree to the tenant (WI#416, !1709 run-143 JN-1).
  say "WeeChat relay configured for tenant '$tenant' (port $relay_port, loopback)"
}

# Write a dedicated mode-0600 weechat.env containing only RELAY_PASSWORD.
# The tenant-side atomic writer never lets root reopen a name in this directory.
_write_weechat_env() {  # <tenant> <relay_password>
  local tenant="$1" relay_password="$2"
  local env_dir env_file
  env_dir="$(tenant_env_dir "$tenant")"
  env_file="$env_dir/weechat.env"
  mkdir -p "$env_dir" || return 1
  printf 'RELAY_PASSWORD=%s\n' "$relay_password" \
    | write_tenant_file_atomic "$tenant" "$env_file" 600
}

# No secret is passed via argv or the child environment. Read the old file as
# the tenant too: a swapped source path cannot make root read another file.
# The transformed result is staged in a root-private file first; the tenant-
# side atomic commit runs only after the producer fully succeeded, so a failed
# read/transform can never publish a partial or empty replacement.
_tenant_env_replace() {  # <tenant> <env_file> <KEY> <VALUE>
  local tenant="$1" path="$2" key="$3" value="$4" staged rc=1
  local -a drop=(sudo -u "$tenant")
  [[ "$(id -un)" != "$tenant" ]] || drop=()
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || return 1
  [[ -f "$path" && ! -L "$path" && "$(stat -c %h -- "$path")" == 1 ]] || return 1
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] || return 1
  darkirc_path_components_safe "$path" || return 1
  staged="$(mktemp)" || return 1
  if printf '%s\n' "$value" | "${drop[@]}" env -i PATH="/usr/bin:/bin" \
    TARGET_PATH="$path" TARGET_KEY="$key" bash -c '
      set -euo pipefail
      target="$TARGET_PATH"
      parent="${target%/*}"
      uid="$(id -u)"
      [[ -d "$parent" && ! -L "$parent" ]] || exit 73
      parent_owner="$(stat -c %u -- "$parent")"
      parent_mode="$(stat -c %a -- "$parent")"
      [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
      (( (8#$parent_mode & 077) == 0 )) || exit 73
      [[ -f "$target" && ! -L "$target" ]] || exit 73
      [[ "$(stat -c %u -- "$target")" == "$uid" && "$(stat -c %h -- "$target")" == 1 ]] || exit 73
      IFS= read -r value
      prefix="$TARGET_KEY="
      while IFS= read -r line || [[ -n "$line" ]]; do
        [[ "$line" == "$prefix"* ]] || printf "%s\n" "$line"
      done <"$target"
      printf "%s%s\n" "$prefix" "$value"
    ' >"$staged" \
    && write_tenant_file_atomic "$tenant" "$path" 600 <"$staged"; then
    rc=0
  fi
  rm -f -- "$staged"
  return "$rc"
}

_relay_env_set_password() {  # <tenant> <lunarwing.env> <password>
  _tenant_env_replace "$1" "$2" RELAY_PASSWORD "$3"
}

_relay_service_active() {  # <tenant> <service basename>
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$1" is-active --quiet "$2.service"
  else
    _openrc_service "$2" status >/dev/null 2>&1
  fi
}

_relay_service_action() {  # <tenant> <stop|start|restart> <service basename>
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    # Concurrent denied starts may have reached systemd's start-rate limit.
    if [[ "$2" == restart ]]; then
      _systemctl_user "$1" reset-failed "$3.service" || return 1
    fi
    _systemctl_user "$1" "$2" "$3.service"
  else
    _openrc_service "$3" "$2"
  fi
}

# Install once per render, on either init. Never replace the lock inode: a
# detached/in-flight start may already have it open. Preserve recovery markers.
prepare_relay_start_barrier() {  # <tenant>
  local name="$1" dir lock helper_dir
  require_cmd python3
  require_cmd flock
  [[ -f "$RELAY_ENV_EXEC_SRC" && ! -L "$RELAY_ENV_EXEC_SRC" ]] \
    || die "relay launcher source is missing or unsafe"
  helper_dir="$(dirname "$RELAY_ENV_EXEC")"
  [[ ! -L "$helper_dir" && ! -L "$RELAY_ENV_EXEC" ]] \
    || die "unsafe relay launcher path"
  install -d -o root -g root -m 0755 "$helper_dir" || return 1
  [[ "$(stat -c '%u:%g:%a' "$helper_dir")" == 0:0:755 ]] || return 1
  install -o root -g root -m 0755 "$RELAY_ENV_EXEC_SRC" "$RELAY_ENV_EXEC" || return 1
  dir="$(tenant_env_dir "$name")"
  [[ -d "$dir" && ! -L "$dir" && "$(stat -c %a "$dir")" == 700 ]] \
    || die "missing or unsafe tenant env directory"
  lock="$dir/.relay-start.lock"
  if [[ ! -e "$lock" && ! -L "$lock" ]]; then
    # Create the lock as the tenant: a root-side noclobber redirect drops
    # O_EXCL for a non-regular target, and the follow-up root chown would
    # follow a symlink the tenant swaps in (WI#416). Tenant-side creation
    # makes the lock tenant-owned at birth; the checks below still verify it.
    local -a drop=(sudo -u "$name")
    [[ "$(id -un)" != "$name" ]] || drop=()
    "${drop[@]}" env -i PATH="/usr/bin:/bin" bash -c '
      umask 077
      set -o noclobber
      : >"$1"
    ' _ "$lock" || return 1
  fi
  [[ -f "$lock" && ! -L "$lock" && "$(stat -c '%h:%a' "$lock")" == 1:600 ]] \
    || die "unsafe relay start lock"
  [[ "$(stat -c %u "$lock")" == "$(stat -c %u "$dir")" ]] \
    || die "relay start lock must be owned by the tenant"
}

# Refuse rotations on old deployed units. For systemd inspect the manager's
# effective command, not just the file on disk (daemon-reload may be missing).
_relay_require_start_barrier() {  # <tenant>
  local name="$1" svc command
  [[ -x "$RELAY_ENV_EXEC" ]] || return 1
  for svc in "lunarwing-$name" "lunarwing-weechat-adapter-$name" "lunarwing-weechat-$name"; do
    if [[ "$INIT_SYSTEM" == systemd ]]; then
      command="$(_systemctl_user "$name" show -p ExecStart --value "$svc.service")" || return 1
    else
      [[ -f "$OPENRC_INITD_DIR/$svc" ]] || return 1
      command="$(<"$OPENRC_INITD_DIR/$svc")"
    fi
    [[ "$command" == *"$RELAY_ENV_EXEC --env-file "* ]] || return 1
  done
}

_relay_release_start_barrier() {  # <marker> <lock fd>
  local fd="$2"
  rmdir "$1" || return 1
  flock -u "$fd" || return 1
  exec {fd}<&-
}

_relay_quiesce_services() {  # <tenant>
  local name="$1" svc failed=false
  for svc in "lunarwing-$name" "lunarwing-weechat-adapter-$name" "lunarwing-weechat-$name"; do
    _relay_service_action "$name" stop "$svc" || failed=true
  done
  for svc in "lunarwing-$name" "lunarwing-weechat-adapter-$name" "lunarwing-weechat-$name"; do
    _relay_service_active "$name" "$svc" && failed=true
  done
  [[ "$failed" == false ]]
}

# The caller holds the lifecycle lock. Restore only services that were running
# on entry; never bring an intentionally stopped daemon or relay online.
_relay_restore_services() {  # <tenant> <weechat-active> <adapter-active> <daemon-active>
  local name="$1" rc=0
  # Restart also resets an OpenRC supervisor exhausted by denied respawns.
  [[ "$2" != true ]] || _relay_service_action "$name" restart "lunarwing-weechat-$name" || rc=1
  [[ "$3" != true ]] || _relay_service_action "$name" restart "lunarwing-weechat-adapter-$name" || rc=1
  [[ "$4" != true ]] || _relay_service_action "$name" restart "lunarwing-$name" || rc=1
  return "$rc"
}

rotate_tenant_weechat_relay_password() (  # <tenant>, under lifecycle lock; close FDs on every exit
  local name="$1" lw_env wc_env conf port old_pw new_pw
  local lock marker relay_lock_fd
  local wc_active=false adapter_active=false daemon_active=false
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  tenant_weechat_enabled "$name" || die "WeeChat is disabled for tenant '$name'"
  lw_env="$(tenant_env_dir "$name")/lunarwing.env"
  wc_env="$(tenant_env_dir "$name")/weechat.env"
  conf="$(tenant_weechat_home "$name")/relay.conf"
  [[ -f "$lw_env" && ! -L "$lw_env" && -f "$wc_env" && ! -L "$wc_env" ]] \
    || die "missing or unsafe relay credential file for '$name'"
  [[ "$(stat -c %h -- "$wc_env")" == 1 ]] || die "unsafe weechat.env hard link"
  ensure_init_system || return 1
  _relay_require_start_barrier "$name" \
    || die "relay start barrier missing; run render-units $name before rotating"
  lock="$(tenant_env_dir "$name")/.relay-start.lock"
  marker="$(tenant_env_dir "$name")/.relay-rotation-blocked"
  [[ -f "$lock" && ! -L "$lock" && "$(stat -c '%h:%a' "$lock")" == 1:600 ]] \
    || die "missing or unsafe relay start lock; run render-units $name"
  exec {relay_lock_fd}<"$lock" || return 1
  # Launchers hold a short shared lock until exec, never for service lifetime.
  # Taking this BEFORE stop also covers starts already admitted by the guard.
  flock -x -w 30 "$relay_lock_fd" || die "timed out waiting for relay starts"
  old_pw="$(_read_env_value "$lw_env" RELAY_PASSWORD)"
  [[ -n "$old_pw" && "$old_pw" == "$(_read_env_value "$wc_env" RELAY_PASSWORD)" ]] \
    || die "relay credentials already differ; repair before rotating"
  port="$(ports_get "$name" weechat)"
  [[ -n "$port" && "$port" != 0 && -f "$conf" && ! -L "$conf" ]] \
    && [[ "$(<"$conf")" != *"$old_pw"* ]] \
    && _weechat_validate_relay_config "$(tenant_weechat_home "$name")" "$port" "" \
    || die "relay.conf is missing or does not use the validated environment expression"
  new_pw="$(generate_token | cut -c1-32)"
  [[ "$new_pw" =~ ^[0-9a-f]{32}$ && "$new_pw" != "$old_pw" ]] \
    || die "failed to generate a fresh relay password"

  # A broken user manager must not be mistaken for three stopped services.
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" show-environment >/dev/null \
      || die "tenant user service manager unavailable for '$name'"
  fi
  _relay_service_active "$name" "lunarwing-weechat-$name" && wc_active=true
  _relay_service_active "$name" "lunarwing-weechat-adapter-$name" && adapter_active=true
  _relay_service_active "$name" "lunarwing-$name" && daemon_active=true
  # Durable fail-closed marker: SIGKILL/reboot/failed rollback must not release
  # consumers onto a split pair when the kernel drops the exclusive lock.
  [[ ! -L "$marker" ]] || die "unsafe relay rotation marker"
  (umask 077; mkdir -p "$marker") || return 1
  # Stop even inactive/activating services to cancel queued init start jobs.
  # All operator/self-heal/supervisor starts now fail at the exec boundary.
  if ! _relay_quiesce_services "$name"; then
    _relay_release_start_barrier "$marker" "$relay_lock_fd" || return 1
    _relay_restore_services "$name" "$wc_active" "$adapter_active" "$daemon_active" || true
    die "could not quiesce relay consumers; credentials unchanged"
  fi

  if ! _relay_env_set_password "$name" "$lw_env" "$new_pw" \
    || ! _write_weechat_env "$name" "$new_pw" \
    || [[ "$(_read_env_value "$lw_env" RELAY_PASSWORD)" != "$new_pw" ]] \
    || [[ "$(_read_env_value "$wc_env" RELAY_PASSWORD)" != "$new_pw" ]]; then
    # Do not restart with mismatched credentials. Even the first writer may
    # have succeeded before reporting failure, so restore BOTH files.
    local rollback_failed=false
    _relay_env_set_password "$name" "$lw_env" "$old_pw" || rollback_failed=true
    _write_weechat_env "$name" "$old_pw" || rollback_failed=true
    if [[ "$rollback_failed" == true ]] \
      || [[ "$(_read_env_value "$lw_env" RELAY_PASSWORD)" != "$old_pw" ]] \
      || [[ "$(_read_env_value "$wc_env" RELAY_PASSWORD)" != "$old_pw" ]]; then
      say "CRITICAL: relay rollback failed for '$name'; starts remain blocked. Repair both env files, then rerun rotate-relay-password." >&2
      return 1
    fi
    _relay_release_start_barrier "$marker" "$relay_lock_fd" || return 1
    _relay_restore_services "$name" "$wc_active" "$adapter_active" "$daemon_active" || true
    say "relay rotation failed; old credentials restored for '$name'" >&2
    return 1
  fi
  _relay_release_start_barrier "$marker" "$relay_lock_fd" || return 1
  if ! _relay_restore_services "$name" "$wc_active" "$adapter_active" "$daemon_active"; then
    say "relay credentials updated but a service failed to start for '$name'; inspect status and restart via mt-admin" >&2
    return 1
  fi
  say "Rotated WeeChat relay password for tenant '$name' (running services restarted)."
)

# TensorZero's OpenAI-compatible surface lives under /openai/v1. The daemon's
# openai_compatible client appends /chat/completions to LLM_BASE_URL, so a bare
# gateway root (http://host:3000) 404s every LLM turn while every health check
# stays green (v2.1.0.0 item-23 rerun finding 1: a fresh proxy-disabled tenant
# whose --tensorzero-url was the bare root could not answer a single prompt
# until an operator hand-appended /openai/v1). Derive the path-aware base a
# tenant needs when it dials TENSORZERO_URL directly (proxy disabled).
# Idempotent: an already-suffixed URL is returned unchanged, and a
# reverse-proxy path prefix is kept (http://host/tz -> http://host/tz/openai/v1).
tensorzero_openai_base() {
  local url="$1"
  [[ -n "$url" ]] || die "tensorzero_openai_base: empty TensorZero URL"
  url="${url%"${url##*[!/]}"}" # strip trailing slashes
  case "$url" in
    */openai/v1) printf '%s\n' "$url" ;;
    */openai)    printf '%s/v1\n' "$url" ;;
    *)           printf '%s/openai/v1\n' "$url" ;;
  esac
}

# ── LLM endpoint validation + worker inheritance (WI#330) ─────────────────────
# Worker images ship placeholder endpoints their entrypoints refuse to run, so
# every URL mt-admin renders for a worker must be both well-formed and
# reachable from inside a rootless container. These helpers are the single
# funnel: add-tenant/add-tenants, the env writer, patch_tenant_env, and the
# configure-* commands all go through them.

# Shape-check an operator-supplied LLM endpoint. Dies on any violation.
# Call directly (never inside $(...)) so die aborts the caller.
validate_llm_url() {  # <url> <label>
  local url="$1" label="$2" LC_ALL=C
  [[ "$url" =~ ^https?:// ]] \
    || die "$label must be an http(s) URL: $url"
  # Whitespace/control characters would inject extra lines into env files.
  [[ "$url" != *[[:space:][:cntrl:]]* ]] \
    || die "$label must not contain whitespace or control characters"
  # Credentials do not belong in a URL that is copied into untrusted worker
  # containers and logged by worker entrypoints; use the API-key settings.
  [[ "${url#*://}" != *@* ]] \
    || die "$label must not embed userinfo (user@host); pass credentials via the API-key settings"
  # Tenant envs feed systemd unit renders, where '%' introduces a specifier.
  [[ "$url" != *%* ]] \
    || die "$label must not contain '%' (systemd specifier expansion)"
  # RFC 3986 reserved + unreserved ASCII characters. '%' and userinfo remain
  # disallowed above. URI delimiters such as $, &, and ; are DATA, not shell
  # escaping: no allowlist replaces the post-drop environment boundary.
  local uri_chars="^[][a-zA-Z0-9._~:/?#@!\$&'()*+,;=-]+$"
  [[ "$url" =~ $uri_chars ]] \
    || die "$label must contain only RFC 3986 URI characters"
  # A host is required.
  local rest="${url#*://}"
  [[ -n "${rest%%[:/]*}" ]] \
    || die "$label has no host: $url"
}

# True when <url> points at host loopback. Canonical mt-admin copy; the
# standalone export-tenant.sh keeps an equivalent local copy (it does not
# source this library).
is_loopback_url() {  # <url>
  local lower="${1,,}"
  [[ "$lower" =~ ^https?://(localhost|127\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|\[::1\]|\[0:0:0:0:0:0:0:1\])(:[0-9]+)?(/|$) ]]
}

# True when <url> is usable from inside a rootless worker container. Host
# loopback is the container itself, and overlay MagicDNS names (NetBird
# *.netbird.selfhosted / *.netbird.cloud, Tailscale *.ts.net) do not resolve
# inside rootless containers on the MT hosts.
worker_llm_url_container_ok() {  # <url>
  local url="$1"
  ! is_loopback_url "$url" || return 1
  local host="${url#*://}"
  host="${host%%[:/]*}"
  host="${host,,}"
  case "$host" in
    *.netbird.selfhosted|*.netbird.cloud|*.ts.net|*.tailscale.net) return 1 ;;
  esac
  return 0
}

# Echo the LLM base URL a worker container should inherit for a tenant:
# an explicit --llm-base-url (or LUNARWING_MT_LLM_BASE_URL) when a container
# can reach it, else the tenant's TensorZero upstream normalized to its
# /openai/v1 surface. The daemon's proxy-loopback default is deliberately NOT
# consulted — worker containers cannot reach host loopback. Prints nothing and
# returns 1 when neither source is container-reachable.
derive_worker_llm_base_url() {  # <llm_base_url-or-empty> <tensorzero_url-or-empty>
  local llm_base_url="${1:-}" tensorzero_url="${2:-}"
  if [[ -n "$llm_base_url" ]] && worker_llm_url_container_ok "$llm_base_url"; then
    printf '%s\n' "$llm_base_url"
    return 0
  fi
  if [[ -n "$tensorzero_url" ]] && worker_llm_url_container_ok "$tensorzero_url"; then
    tensorzero_openai_base "$tensorzero_url"
    return 0
  fi
  return 1
}

# WI#330: resolve one external worker's LLM base URL for a lunarwing.env
# (re)render. Precedence: an explicit per-worker flag > an operator-set value
# already in the env (anything without a <KEY>_SOURCE=inherited marker) > the
# tenant's inherited endpoint. Inherited values are re-derived on EVERY render
# so a TensorZero move reaches the workers without a manual configure-* per
# worker, and they carry <KEY>_SOURCE=inherited so export-tenant does not pin
# a migrated tenant to the source host's endpoint. Sets the two nameref outputs
# (<out_source> is "explicit" | "inherited" | "").
_resolve_worker_base_url() {  # <env_path> <KEY> <flag_value> <inherited_default> <out_url_var> <out_source_var>
  local env_path="$1" key="$2" flag_value="$3" inherited="$4"
  local -n _worker_url_ref="$5" _worker_source_ref="$6"
  _worker_url_ref=""
  _worker_source_ref=""
  if [[ -n "$flag_value" ]]; then
    validate_llm_url "$flag_value" "--$(printf '%s' "$key" | tr 'A-Z_' 'a-z-')"
    _worker_url_ref="$flag_value"
    _worker_source_ref="explicit"
    return 0
  fi
  local existing existing_source
  existing="$(_env_existing "$env_path" "$key")"
  existing_source="$(_env_existing "$env_path" "${key}_SOURCE")"
  if [[ -n "$existing" && "$existing_source" != "inherited" ]]; then
    _worker_url_ref="$existing"
    _worker_source_ref="explicit"
    return 0
  fi
  if [[ -n "$inherited" ]]; then
    _worker_url_ref="$inherited"
    _worker_source_ref="inherited"
  fi
  return 0
}

# An enabled worker must never render an empty base URL: the image placeholder
# guard would kill the worker at startup. Fail the render with the fix instead.
_require_enabled_worker_url() {  # <tenant> <worker> <KEY> <resolved_url>
  local name="$1" worker="$2" key="$3" url="$4"
  [[ -z "$url" ]] || return 0
  tenant_worker_enabled "$name" "$worker" || return 0
  die "tenant '$name' has the $worker worker enabled but no container-reachable LLM endpoint to inherit (host loopback and overlay MagicDNS names are unreachable from worker containers). Pass --llm-base-url/--tensorzero-url with a reachable address, or set one explicitly: $0 configure-$worker $name --base-url <url>"
}

# WI#330: there is no built-in LLM endpoint default — the historical
# developer-LAN default was unreachable from every other host, and rendering it
# silently defeated the worker placeholder guards. A new tenant therefore must
# be given an endpoint: --tensorzero-url (or fleet LUNARWING_MT_TENSORZERO_URL)
# always satisfies this; --llm-base-url (or LUNARWING_MT_LLM_BASE_URL) suffices
# only when the tenant does not run the local TensorZero proxy (the proxy still
# needs its upstream). Call directly (never inside $(...)) so die aborts.
require_tenant_llm_endpoint() {  # <tensorzero_url> <llm_base_url> <enable_proxy>
  local tensorzero_url="${1:-}" llm_base_url="${2:-}" enable_proxy="${3:-false}"
  if [[ -n "$tensorzero_url" ]]; then
    validate_llm_url "$tensorzero_url" "--tensorzero-url"
  elif [[ "$enable_proxy" == "true" ]]; then
    die "add-tenant requires --tensorzero-url <url> (or LUNARWING_MT_TENSORZERO_URL): the tenant's TensorZero proxy needs an upstream, and there is no built-in default (WI#330)"
  elif [[ -z "$llm_base_url" ]]; then
    die "add-tenant requires an LLM endpoint: pass --tensorzero-url <url> (or LUNARWING_MT_TENSORZERO_URL), or --llm-base-url <url> for a direct non-proxy endpoint; there is no built-in default (WI#330)"
  fi
  [[ -z "$llm_base_url" ]] || validate_llm_url "$llm_base_url" "--llm-base-url"
}

write_tenant_lunarwing_env() {
  local name="$1"
  # Empty $2 means "caller supplied none" -> preserve an existing JID (see
  # resolve_tenant_xmpp_jid); only a first write falls back to xmpp.localhost.
  local xmpp_jid
  xmpp_jid="$(resolve_tenant_xmpp_jid "$name" "${2:-}")"
  local xmpp_password="${3:-}"   # resolved below (preserve an existing one on re-run)
  local tensorzero_url="${4:-$DEFAULT_TENSORZERO_URL}"
  local llm_api_key="${5:-}"
  local llm_base_url="${6:-}"
  local llm_model="${7:-}"
  local gateway_host="${8:-}"
  local xmpp_allow_from="${9:-}"
  local opencode_model="${10:-}"
  local opencode_base_url="${11:-}"
  local kimi_model="${12:-}"
  local kimi_base_url="${13:-}"
  local kimi_api_key="${14:-}"
  local opencode2_model="${15:-}"
  local opencode2_base_url="${16:-}"

  local path gateway_port http_port bridge_port pg_port proxy_port weechat_port weechat_adapter_port orchestrator_port pebble_wss_port opencode_wss_port kimi_wss_port opencode2_wss_port
  path="$(tenant_env_dir "$name")/lunarwing.env"
  gateway_port="$(ports_get "$name" gateway)"
  http_port="$(ports_get "$name" http)"
  bridge_port="$(ports_get "$name" bridge)"
  pg_port="$(ports_get "$name" postgres)"
  proxy_port="$(ports_get "$name" proxy)"
  weechat_port="$(ports_get "$name" weechat)"
  weechat_adapter_port="$(ports_get "$name" weechat_adapter)"
  orchestrator_port="$(ports_get "$name" orchestrator)"
  pebble_wss_port="$(ports_get "$name" pebble_wss)"
  opencode_wss_port="$(ports_get "$name" opencode_wss)"
  kimi_wss_port="$(ports_get "$name" kimi_wss)"
  opencode2_wss_port="$(ports_get "$name" opencode2_wss)"

  local state_dir run_dir
  state_dir="$(tenant_state_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"

  # WI#390 (review IG-1): every existing-value read below comes from ONE
  # root-private snapshot per file, taken as the tenant — never from root
  # resolving the live tenant-writable path. A cross-tenant symlink present
  # for the read window is unreadable to the tenant-side cat and dies here;
  # without the snapshot, root's reads would follow it and this render would
  # write the other tenant's GATEWAY_AUTH_TOKEN / SECRETS_MASTER_KEY /
  # XMPP_PASSWORD / KIMI_API_KEY / … back into this tenant's env. vision.env
  # and voice.env are the same tenant-writable class (their tokens are carried
  # into this render) and are snapshotted the same way. The commit at the end
  # still re-validates the live path in write_tenant_file_atomic, so a
  # post-snapshot swap fails closed there. resolve_tenant_xmpp_jid above no
  # longer touches the live path either (WI#423): XMPP_JID comes from the
  # tenant-side per-key reader, and a foreign symlink refuses loudly there
  # before any value is rendered.
  # WI#423: tenant_pg_password below also reads pg.secret/lunarwing.env
  # through its own tenant-side snapshots and commits through the tenant-side
  # atomic writer — no root resolution of the live paths remains in this
  # render.
  local env_snapshot vision_snapshot voice_snapshot
  _snapshot_tenant_env "$name" "$path" env_snapshot
  _snapshot_tenant_env "$name" "$(tenant_env_dir "$name")/vision.env" vision_snapshot
  _snapshot_tenant_env "$name" "$(tenant_env_dir "$name")/voice.env" voice_snapshot

  # Idempotent on re-run (F4-A/F4-B): PRESERVE existing secrets when lunarwing.env
  # already exists. Regenerating SECRETS_MASTER_KEY would permanently orphan the
  # tenant's encrypted DB secrets (it is the AES-256-GCM vault key); rotating the
  # tokens would break live clients/workers; minting a fresh XMPP_PASSWORD would
  # break the already-registered XMPP account. Generate fresh ONLY on first write.
  local gateway_token bridge_token relay_password secrets_key webhook_secret pg_password darkirc_adapter_secret
  local coord_enabled coord_agent_id coord_agent_aliases coord_responder_roster coord_channels coord_admin_users coord_admin_token coord_admin_port
  local coord_knowledge_share coord_knowledge_publish coord_knowledge_publishers coord_knowledge_max_per_hour coord_knowledge_global_max_per_hour coord_knowledge_min_chars
  local study_notes_home study_notes_state_home study_notes_allow_write knowledge_seed_flag
  local knowledge_sync_enabled knowledge_sync_cadence knowledge_sync_max_records knowledge_sync_max_bytes
  gateway_token="$(_env_existing "$env_snapshot" GATEWAY_AUTH_TOKEN)";   gateway_token="${gateway_token:-$(generate_token)}"
  # Per-worker credential for the external-worker WebSocket handshake. External
  # workers run `WS_ROLE=server` and validate an inbound bearer on the upgrade;
  # the daemon presents the same value as `auth_token`. This must NOT be the
  # gateway bearer: the worker never calls the gateway API, and the value is
  # readable via `podman inspect` / tenant-readable unit files, so reusing the
  # gateway token handed a contained lane the tenant's full API surface (WI#205).
  worker_token="$(_env_existing "$env_snapshot" WORKER_AUTH_TOKEN)";     worker_token="${worker_token:-$(generate_token)}"
  bridge_token="$(_env_existing "$env_snapshot" XMPP_BRIDGE_TOKEN)";     bridge_token="${bridge_token:-$(generate_token | cut -c1-32)}"
  relay_password="$(_env_existing "$env_snapshot" RELAY_PASSWORD)";      relay_password="${relay_password:-$(generate_token | cut -c1-32)}"
  secrets_key="$(_env_existing "$env_snapshot" SECRETS_MASTER_KEY)";     secrets_key="${secrets_key:-$(generate_token)}"
  webhook_secret="$(_env_existing "$env_snapshot" HTTP_WEBHOOK_SECRET)"; webhook_secret="${webhook_secret:-$(generate_token)}"
  ssh_api_token="$(_env_existing "$env_snapshot" SSH_API_TOKEN)";       ssh_api_token="${ssh_api_token:-$(generate_token)}"
  coord_enabled="$(_env_existing "$env_snapshot" COORD_ENABLED)"; coord_enabled="${coord_enabled:-false}"
  coord_agent_id="$(_env_existing "$env_snapshot" COORD_AGENT_ID)"; coord_agent_id="${coord_agent_id:-$name}"
  coord_agent_aliases="$(_env_existing "$env_snapshot" COORD_AGENT_ALIASES)"
  coord_responder_roster="$(_env_existing "$env_snapshot" COORD_RESPONDER_ROSTER)"
  coord_channels="$(_env_existing "$env_snapshot" COORD_CHANNELS)"
  coord_knowledge_share="$(_env_existing "$env_snapshot" COORD_KNOWLEDGE_SHARE)"; coord_knowledge_share="${coord_knowledge_share:-false}"
  coord_knowledge_publish="$(_env_existing "$env_snapshot" COORD_KNOWLEDGE_PUBLISH)"; coord_knowledge_publish="${coord_knowledge_publish:-false}"
  coord_knowledge_publishers="$(_env_existing "$env_snapshot" COORD_KNOWLEDGE_PUBLISHERS)"; coord_knowledge_publishers="${coord_knowledge_publishers:-}"
  coord_knowledge_max_per_hour="$(_env_existing "$env_snapshot" COORD_KNOWLEDGE_MAX_PER_HOUR)"; coord_knowledge_max_per_hour="${coord_knowledge_max_per_hour:-30}"
  coord_knowledge_global_max_per_hour="$(_env_existing "$env_snapshot" COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR)"; coord_knowledge_global_max_per_hour="${coord_knowledge_global_max_per_hour:-300}"
  coord_knowledge_min_chars="$(_env_existing "$env_snapshot" COORD_KNOWLEDGE_MIN_CHARS)"; coord_knowledge_min_chars="${coord_knowledge_min_chars:-12}"
  coord_admin_users="$(_env_existing "$env_snapshot" COORD_ADMIN_USERS)"
  coord_admin_token="$(_env_existing "$env_snapshot" COORD_ADMIN_TOKEN)"; coord_admin_token="${coord_admin_token:-$(generate_token)}"
  coord_admin_port="$(_env_existing "$env_snapshot" COORD_ADMIN_PORT)"
  if [[ -z "$coord_admin_port" ]]; then
    coord_admin_port="$(ports_get "$name" coord_admin || true)"
    [[ -n "$coord_admin_port" ]] || die "coordination admin port missing for tenant '$name'"
  fi
  if tenant_darkirc_enabled "$name"; then
    darkirc_adapter_secret="$(_env_existing "$env_snapshot" DARKIRC_ADAPTER_SECRET)"
  fi
  # XMPP password: an explicit --xmpp-password wins; else preserve an existing one;
  # else mint a fresh one (first-time provision).
  [[ -n "$xmpp_password" ]] || { xmpp_password="$(_env_existing "$env_snapshot" XMPP_PASSWORD)"; xmpp_password="${xmpp_password:-$(generate_token | cut -c1-32)}"; }
  [[ -n "$opencode_model" ]]    || opencode_model="$(_env_existing "$env_snapshot" OPENCODE_MODEL)"
  [[ -n "$kimi_model" ]]        || kimi_model="$(_env_existing "$env_snapshot" KIMI_MODEL)"
  [[ -n "$kimi_api_key" ]]      || kimi_api_key="$(_env_existing "$env_snapshot" KIMI_API_KEY)"
  [[ -n "$opencode2_model" ]]    || opencode2_model="$(_env_existing "$env_snapshot" OPENCODE2_MODEL)"
  # WI#330: the worker images ship a placeholder LLM endpoint their entrypoints
  # refuse to run, so an unset worker base URL can no longer fall through to a
  # baked default. Each worker URL resolves as: explicit per-worker flag >
  # operator-set value already in lunarwing.env > inherited from the tenant's
  # own LLM endpoint (re-derived every render, marked <KEY>_SOURCE=inherited).
  local worker_llm_base_default
  worker_llm_base_default="$(derive_worker_llm_base_url "$llm_base_url" "$tensorzero_url" || true)"
  local opencode_base_url_source kimi_base_url_source opencode2_base_url_source
  _resolve_worker_base_url "$env_snapshot" OPENCODE_BASE_URL "$opencode_base_url" "$worker_llm_base_default" opencode_base_url opencode_base_url_source
  _resolve_worker_base_url "$env_snapshot" KIMI_BASE_URL "$kimi_base_url" "$worker_llm_base_default" kimi_base_url kimi_base_url_source
  _resolve_worker_base_url "$env_snapshot" OPENCODE2_BASE_URL "$opencode2_base_url" "$worker_llm_base_default" opencode2_base_url opencode2_base_url_source
  _require_enabled_worker_url "$name" opencode OPENCODE_BASE_URL "$opencode_base_url"
  _require_enabled_worker_url "$name" kimi KIMI_BASE_URL "$kimi_base_url"
  _require_enabled_worker_url "$name" opencode2 OPENCODE2_BASE_URL "$opencode2_base_url"
  study_notes_home="$(_env_existing "$env_snapshot" LW2STUDYMCP_HOME)"
  study_notes_state_home="$(_env_existing "$env_snapshot" LW2STUDYMCP_STATE_HOME)"
  study_notes_allow_write="$(_env_existing "$env_snapshot" LW2STUDYMCP_ALLOW_WRITE)"
  # One-shot knowledge seed: carry the flag only when it already exists (set
  # solely on the add-tenant path), so upgraded pre-seed tenants stay unarmed.
  knowledge_seed_flag="$(_env_existing "$env_snapshot" KNOWLEDGE_SEED_FROM_STUDY_MCP)"
  knowledge_sync_enabled="$(_env_existing "$env_snapshot" KNOWLEDGE_SYNC_ENABLED)"
  knowledge_sync_cadence="$(_env_existing "$env_snapshot" KNOWLEDGE_SYNC_CADENCE_HOURS)"
  knowledge_sync_max_records="$(_env_existing "$env_snapshot" KNOWLEDGE_SYNC_MAX_RECORDS_PER_RUN)"
  knowledge_sync_max_bytes="$(_env_existing "$env_snapshot" KNOWLEDGE_SYNC_MAX_BYTES_PER_RUN)"
  local vision_mode vision_external_url vision_token_existing
  vision_mode="$(tenant_vision_mode "$name")"
  vision_external_url="$(tenant_vision_external_url "$name")"
  vision_token_existing="$(_env_existing "$env_snapshot" VISION_AUTH_TOKEN)"
  if [[ "$vision_mode" == external ]]; then
    vision_external_url="$(_normalize_external_vision_url "$vision_external_url")" \
      || die "stored external LunarVision URL is invalid for tenant '$name'"
    [[ -n "$vision_token_existing" ]] \
      || die "external LunarVision token is missing for tenant '$name'"
  fi

  local transcription_enabled transcription_provider transcription_model voice_enabled
  voice_enabled="$(_env_existing "$voice_snapshot" VOICE_ENABLED)"
  voice_enabled="${voice_enabled:-false}"
  transcription_enabled="$(_env_existing "$env_snapshot" TRANSCRIPTION_ENABLED)"
  transcription_enabled="${transcription_enabled:-$voice_enabled}"
  transcription_provider="$(_env_existing "$env_snapshot" TRANSCRIPTION_PROVIDER)"
  transcription_provider="${transcription_provider:-lunarvoice}"
  transcription_model="$(_env_existing "$env_snapshot" TRANSCRIPTION_MODEL)"
  transcription_model="${transcription_model:-whisper-tiny}"
  # LLM circuit breaker: fleet defaults (open after 5 fully-retried failures,
  # probe again after 15s — WI#239). These are tunables, so preserve an
  # operator's per-tenant override across re-provision rather than clobbering
  # it.
  local cb_threshold cb_recovery
  cb_threshold="$(_env_existing "$env_snapshot" LLM_CIRCUIT_BREAKER_THRESHOLD)";  cb_threshold="${cb_threshold:-5}"
  cb_recovery="$(_env_existing "$env_snapshot" LLM_CIRCUIT_BREAKER_RECOVERY_SECS)"; cb_recovery="${cb_recovery:-15}"
  # Stable + migration-safe; resolved before the heredoc so it can read an
  # existing DATABASE_URL (preserving an already-initialised DB's password).
  # Run-166 LF-3: a refusal must be fatal — an empty password rendered into
  # DATABASE_URL would brick the tenant's DB connection at first start.
  if ! pg_password="$(tenant_pg_password "$name")"; then
    die "could not resolve the PostgreSQL password for tenant '$name'"
  fi

  # LLM endpoint the daemon's OpenAI-compatible client dials. When the proxy is
  # enabled, defaults to this tenant's local TensorZero proxy. When disabled,
  # defaults to the upstream TENSORZERO_URL's OpenAI-compatible surface
  # (tensorzero_openai_base appends /openai/v1 to a bare gateway root — see the
  # helper for the 404 regression this prevents). An explicit value (from
  # --llm-base-url or LUNARWING_MT_LLM_BASE_URL) always overrides as-is.
  local llm_base_url_effective
  if tenant_proxy_enabled "$name"; then
    llm_base_url_effective="${llm_base_url:-http://127.0.0.1:${proxy_port}/v1}"
  else
    llm_base_url_effective="${llm_base_url:-$(tensorzero_openai_base "$tensorzero_url")}"
  fi

  # Idempotent overrides for the configurable flags (--llm-model,
  # --gateway-host, --xmpp-allow-from). An explicit flag value wins; else an
  # existing file value is preserved on re-run; else the hardcoded default
  # (LLM_MODEL, GATEWAY_HOST, or the local user JID for XMPP_ALLOW_FROM).
  local llm_model_effective gateway_host_effective mascot_enabled xmpp_allow_from_effective
  if [[ -n "$llm_model" ]]; then
    llm_model_effective="$llm_model"
  else
    llm_model_effective="$(_env_existing "$env_snapshot" LLM_MODEL)"
    llm_model_effective="${llm_model_effective:-tensorzero::function_name::lunarwing}"
  fi
  if [[ -n "$gateway_host" ]]; then
    gateway_host_effective="$gateway_host"
  else
    gateway_host_effective="$(_env_existing "$env_snapshot" GATEWAY_HOST)"
    gateway_host_effective="${gateway_host_effective:-127.0.0.1}"
  fi
  mascot_enabled="$(_env_existing "$env_snapshot" MASCOT_ENABLED)"
  mascot_enabled="${mascot_enabled:-false}"
  if [[ -n "$xmpp_allow_from" ]]; then
    xmpp_allow_from_effective="$(build_xmpp_allow_from "$xmpp_jid" "$xmpp_allow_from")"
  else
    # Preserve an existing list; allow the local user to DM on first write.
    xmpp_allow_from_effective="$(_env_existing "$env_snapshot" XMPP_ALLOW_FROM)"
    xmpp_allow_from_effective="${xmpp_allow_from_effective:-user@xmpp.localhost}"
  fi

  # Engine V2 rollout (v2.0.6.0): V2 engine channels ON by default for all
  # tenants. An existing ENGINE_V2 / ENGINE_V2_CHANNELS value is preserved
  # across upgrade/rebuild/re-render — never clobber an operator's setting;
  # only a first write (or an absent key) falls back to the defaults.
  local engine_v2 engine_v2_channels orchestrator_self_modify
  engine_v2="$(_env_existing "$env_snapshot" ENGINE_V2)"; engine_v2="${engine_v2:-true}"
  engine_v2_channels="$(_env_existing "$env_snapshot" ENGINE_V2_CHANNELS)"
  engine_v2_channels="${engine_v2_channels:-xmpp,weechat,darkirc}"
  # WI#282: new tenants opt into stored orchestrator and prompt-overlay updates.
  # Preserve an explicit operator opt-out across add-tenant re-renders.
  orchestrator_self_modify="$(_env_existing "$env_snapshot" ORCHESTRATOR_SELF_MODIFY)"
  orchestrator_self_modify="${orchestrator_self_modify:-true}"

  # Sandbox egress (WI#154): multi-tenant hosts run worker WSS ports, tenant
  # PostgreSQL, and health endpoints on host loopback, and the proxy egress
  # mode is advisory (env-var based) — a sandboxed process that ignores proxy
  # env vars can reach every tenant's loopback services. MT tenants therefore
  # default to SANDBOX_NETWORK=none (kernel-enforced). An operator-selected
  # SANDBOX_NETWORK=proxy is preserved on re-render (explicit opt-in for
  # tenants that genuinely need allowlisted web access).
  local sandbox_network
  sandbox_network="$(_env_existing "$env_snapshot" SANDBOX_NETWORK)"
  sandbox_network="${sandbox_network:-none}"

  # Generic sandbox defaults (WI#223): MT tenants pin the runtime and image so
  # `auto` detection cannot drift away from the host's managed choice, the
  # lean lunarwing-sandbox image (built from ic/docker/sandbox.Dockerfile) is
  # used instead of the heavy lunarwing-worker image, and auto-pull is off
  # because the image is local-only (a pull attempt would fail against a
  # registry that has never hosted it). Existing values are preserved on
  # re-render; the daemon still falls back podman->docker when SANDBOX_RUNTIME
  # names a binary the host lacks.
  local sandbox_runtime sandbox_image sandbox_auto_pull
  sandbox_runtime="$(_env_existing "$env_snapshot" SANDBOX_RUNTIME)"
  sandbox_runtime="${sandbox_runtime:-podman}"
  sandbox_image="$(_env_existing "$env_snapshot" SANDBOX_IMAGE)"
  sandbox_image="${sandbox_image:-lunarwing-sandbox:latest}"
  sandbox_auto_pull="$(_env_existing "$env_snapshot" SANDBOX_AUTO_PULL)"
  sandbox_auto_pull="${sandbox_auto_pull:-false}"
  # Sandbox-visible daemon log (WI#287): info caps the copy at INFO; an
  # operator-set off or debug is preserved on re-render.
  local sandbox_tenant_logs
  sandbox_tenant_logs="$(_env_existing "$env_snapshot" SANDBOX_TENANT_LOGS)"
  sandbox_tenant_logs="${sandbox_tenant_logs:-info}"

  # Sensible new-tenant defaults (WI#239): each of these is written into the
  # rendered lunarwing.env with the value below UNLESS the operator has
  # already set it — re-renders preserve per-tenant overrides, matching how
  # SANDBOX_RUNTIME / SANDBOX_IMAGE / SANDBOX_AUTO_PULL and the circuit
  # breaker are handled above. The listed defaults are what new tenants get
  # on first write.
  local llm_turn_budget llm_request_timeout allow_local_tools auto_approve_tools agent_max_tool_iterations
  local sandbox_policy sandbox_allow_full_access
  local engine_v2_model_recovery_attempts engine_v2_model_recovery_total_attempts engine_v2_tool_async_timeout_secs
  local skill_self_improvement skill_prune_quarantine_days reflex_compiler_enabled
  llm_turn_budget="$(_env_existing "$env_snapshot" LLM_TURN_BUDGET_SECS)";                llm_turn_budget="${llm_turn_budget:-599}"
  llm_request_timeout="$(_env_existing "$env_snapshot" LLM_REQUEST_TIMEOUT_SECS)";        llm_request_timeout="${llm_request_timeout:-2599}"
  allow_local_tools="$(_env_existing "$env_snapshot" ALLOW_LOCAL_TOOLS)";                 allow_local_tools="${allow_local_tools:-false}"
  # Migrate the legacy key on re-render, but prefer the canonical setting when
  # both exist. Only write the canonical key so an operator override survives.
  auto_approve_tools="$(_env_existing "$env_snapshot" AGENT_AUTO_APPROVE_TOOLS)"
  if [[ -z "$auto_approve_tools" ]]; then
    auto_approve_tools="$(_env_existing "$env_snapshot" AUTO_APPROVE_TOOLS)"
  fi
  auto_approve_tools="${auto_approve_tools:-false}"
  agent_max_tool_iterations="$(_env_existing "$env_snapshot" AGENT_MAX_TOOL_ITERATIONS)"; agent_max_tool_iterations="${agent_max_tool_iterations:-99}"
  sandbox_policy="$(_env_existing "$env_snapshot" SANDBOX_POLICY)";                       sandbox_policy="${sandbox_policy:-workspace_write}"
  sandbox_allow_full_access="$(_env_existing "$env_snapshot" SANDBOX_ALLOW_FULL_ACCESS)"; sandbox_allow_full_access="${sandbox_allow_full_access:-false}"
  engine_v2_model_recovery_attempts="$(_env_existing "$env_snapshot" ENGINE_V2_MODEL_RECOVERY_ATTEMPTS)"
  engine_v2_model_recovery_attempts="${engine_v2_model_recovery_attempts:-2}"
  engine_v2_model_recovery_total_attempts="$(_env_existing "$env_snapshot" ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS)"
  engine_v2_model_recovery_total_attempts="${engine_v2_model_recovery_total_attempts:-2}"
  engine_v2_max_output_tokens="$(_env_existing "$env_snapshot" ENGINE_V2_MAX_OUTPUT_TOKENS)"
  engine_v2_max_output_tokens="${engine_v2_max_output_tokens:-8192}"
  engine_v2_tool_async_timeout_secs="$(_env_existing "$env_snapshot" ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS)"
  engine_v2_tool_async_timeout_secs="${engine_v2_tool_async_timeout_secs:-7999}"
  skill_self_improvement="$(_env_existing "$env_snapshot" SKILL_SELF_IMPROVEMENT)";       skill_self_improvement="${skill_self_improvement:-true}"
  skill_prune_quarantine_days="$(_env_existing "$env_snapshot" SKILL_PRUNE_QUARANTINE_DAYS)"
  skill_prune_quarantine_days="${skill_prune_quarantine_days:-15}"
  reflex_compiler_enabled="$(_env_existing "$env_snapshot" REFLEX_COMPILER_ENABLED)";     reflex_compiler_enabled="${reflex_compiler_enabled:-false}"
  # WI#291: operator cap on tool-calling rounds per lightweight routine run.
  # The daemon default (3) left a digest routine no round for its final send.
  local routines_lightweight_max_iterations
  routines_lightweight_max_iterations="$(_env_existing "$env_snapshot" ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS)"
  routines_lightweight_max_iterations="${routines_lightweight_max_iterations:-6}"

  # WI#390: stage the full render in a root-private file, then commit it
  # through the tenant-side atomic writer. Root never opens lunarwing.env
  # inside the tenant-writable env dir, so a pre-placed or raced symlink there
  # cannot redirect the write onto an attacker-chosen path.
  local staged_env
  staged_env="$(mktemp)" || die "could not stage the env render for tenant '$name'"
  cat >"$staged_env" <<ENVEOF
LUNARWING_BASE_DIR=$state_dir
LUNARWING_SOCKET=$run_dir/lunarwing.sock

# Database
DATABASE_BACKEND=postgres
DATABASE_URL=postgres://lunarwing:${pg_password}@127.0.0.1:${pg_port}/lunarwing
DATABASE_SSLMODE=disable
PGSSLMODE=disable

# LLM — OpenAI-compatible endpoint (defaults to the local TensorZero proxy)
LLM_BACKEND=openai_compatible
LLM_BASE_URL=${llm_base_url_effective}
LLM_API_KEY=${llm_api_key:-token-${name}}
LLM_MODEL=$llm_model_effective
ALLOW_PRIVATE_IPS=1
# Sensible new-tenant defaults (WI#239): per-turn wall-clock budget and the
# per-request timeout that wraps a single LLM call. The turn budget must stay
# comfortably below the request timeout so a slow upstream cannot stall a
# turn indefinitely, and the request timeout must clear the harness's
# handle_message envelope (see src/app.rs). Tunables; preserved on re-render.
LLM_TURN_BUDGET_SECS=$llm_turn_budget
LLM_REQUEST_TIMEOUT_SECS=$llm_request_timeout
# Circuit breaker: fast-fail once the LLM backend is degraded, then auto-probe
# to recover. Guards against a sick gateway amplifying transient blips into
# tenant-wide slowness. Backend-agnostic (wraps whatever provider is built).
LLM_CIRCUIT_BREAKER_THRESHOLD=$cb_threshold
LLM_CIRCUIT_BREAKER_RECOVERY_SECS=$cb_recovery

# Runtime identity
AGENT_NAME=$name
LUNARWING_OWNER_ID=$name
SECRETS_MASTER_KEY=$secrets_key

# Multi-agent coordination (disabled until channels are configured explicitly)
COORD_ENABLED=$coord_enabled
COORD_AGENT_ID=$coord_agent_id
COORD_AGENT_ALIASES=$coord_agent_aliases
COORD_RESPONDER_ROSTER=$coord_responder_roster
COORD_CHANNELS=$coord_channels
COORD_KNOWLEDGE_SHARE=$coord_knowledge_share
COORD_KNOWLEDGE_PUBLISH=$coord_knowledge_publish
COORD_KNOWLEDGE_PUBLISHERS=$coord_knowledge_publishers
COORD_KNOWLEDGE_MAX_PER_HOUR=$coord_knowledge_max_per_hour
COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=$coord_knowledge_global_max_per_hour
COORD_KNOWLEDGE_MIN_CHARS=$coord_knowledge_min_chars
COORD_ADMIN_USERS=$coord_admin_users
COORD_ADMIN_TOKEN=$coord_admin_token
COORD_ADMIN_PORT=$coord_admin_port

# XMPP
XMPP_BRIDGE_URL=http://127.0.0.1:${bridge_port}
XMPP_BRIDGE_TOKEN=$bridge_token
XMPP_JID=$xmpp_jid
XMPP_PASSWORD=$xmpp_password
XMPP_DM_POLICY=allowlist
XMPP_ALLOW_FROM=$xmpp_allow_from_effective
XMPP_ALLOW_ROOMS=
XMPP_ENCRYPTED_ROOMS=
XMPP_OMEMO_DEVICE_ID=0
XMPP_OMEMO_STORE_DIR=$state_dir/xmpp
XMPP_ALLOW_PLAINTEXT_FALLBACK=true
XMPP_RESOURCE=$name

# WASM
WASM_ENABLED=true
WASM_CHANNELS_ENABLED=true
WASM_TOOLS_DIR=$state_dir/tools
WASM_CHANNELS_DIR=$state_dir/channels

# Gateway
GATEWAY_ENABLED=true
GATEWAY_HOST=$gateway_host_effective
GATEWAY_PORT=$gateway_port
GATEWAY_AUTH_TOKEN=$gateway_token
# Scoped bearer for the daemon<->external-worker WebSocket handshake only.
# Deliberately distinct from GATEWAY_AUTH_TOKEN (WI#205).
WORKER_AUTH_TOKEN=$worker_token
MASCOT_ENABLED=$mascot_enabled

# HTTP webhook (bound to localhost only; secret-protected)
HTTP_HOST=127.0.0.1
HTTP_PORT=$http_port
HTTP_WEBHOOK_SECRET=$webhook_secret

# SSH API (host/key management on the webhook server; bearer-protected)
SSH_API_TOKEN=$ssh_api_token

# Orchestrator (sandbox container callback)
ORCHESTRATOR_PORT=$orchestrator_port

# Opencode worker (WebSocket port for agent communication)
OPENCODE_WSS_PORT=$opencode_wss_port

# Kimi worker (WebSocket port for agent communication)
KIMI_WSS_PORT=$kimi_wss_port

# OpenCode 2 worker (WebSocket port for agent communication)
OPENCODE2_WSS_PORT=$opencode2_wss_port

# Pebble worker (WebSocket port for agent communication)
PEBBLE_WSS_PORT=$pebble_wss_port

# WeeChat relay + adapter
# RELAY_URL / WS_ADAPTER_URL are full URLs consumed by the in-process WASM
# channel (via the capabilities 'env' source). ADAPTER_PORT/WEECHAT_ADAPTER_PORT
# are the bare port consumed by the standalone ws_adapter.py process.
RELAY_URL=http://127.0.0.1:${weechat_port}
RELAY_PASSWORD=$relay_password
ADAPTER_PORT=$weechat_adapter_port
WEECHAT_ADAPTER_PORT=$weechat_adapter_port
WS_ADAPTER_URL=http://127.0.0.1:${weechat_adapter_port}

# Engine V2 (parallel execution path with streaming, gates, missions).
# ENGINE_V2_CHANNELS is the comma-separated allowlist of non-gateway channels
# routed through Engine V2 (eligible: xmpp, weechat, darkirc) — on by default
# for all tenants; an existing value is preserved on re-render. There is no
# opt-out value: an entry like "none" is an ignored unknown entry, and the
# gateway keeps routing while ENGINE_V2=true. ENGINE_V2=false is the only
# durable opt-out.
ENGINE_V2=$engine_v2
ENGINE_V2_CHANNELS=$engine_v2_channels
# Runtime orchestrator and prompt-overlay updates (WI#282): enabled for new
# tenants; set false to retain the compiled orchestrator and ignore overlays.
ORCHESTRATOR_SELF_MODIFY=$orchestrator_self_modify
# Engine V2 model recovery + async tool budget (WI#239). Attempts are the
# per-turn model-retry ceiling; the async timeout wraps a single in-flight
# tool call so a stuck tool cannot pin a turn forever. Preserved on re-render.
ENGINE_V2_MODEL_RECOVERY_ATTEMPTS=$engine_v2_model_recovery_attempts
ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS=$engine_v2_model_recovery_total_attempts
ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS=$engine_v2_tool_async_timeout_secs
# Per-call output budget (max_tokens) for Engine V2 model requests. Thinking
# models count their reasoning against it; raise to 16384+ if turns end with
# "used its entire output budget before producing visible text". Preserved on
# re-render.
ENGINE_V2_MAX_OUTPUT_TOKENS=$engine_v2_max_output_tokens

# Generic sandbox egress (WI#154): none by default on multi-tenant hosts —
# the advisory proxy mode would let a sandboxed process reach host-loopback
# services (worker WSS/health ports, tenant PostgreSQL, cross-tenant
# loopback). Set SANDBOX_NETWORK=proxy explicitly for tenants that need
# allowlisted web access.
SANDBOX_NETWORK=$sandbox_network

# Generic sandbox runtime/image (WI#223): pin podman (the MT-managed runtime;
# the daemon falls back to docker when podman is absent), use the lean
# locally-built lunarwing-sandbox image (add-tenant builds it from
# ic/docker/sandbox.Dockerfile), and never auto-pull — the tag only exists in
# local stores, not in any registry.
SANDBOX_RUNTIME=$sandbox_runtime
SANDBOX_IMAGE=$sandbox_image
SANDBOX_AUTO_PULL=$sandbox_auto_pull
# Sandbox policy (WI#239): workspace_write is the default posture — sandboxed
# commands may write inside the mounted workspace but not outside it.
# FullAccess stays gated behind SANDBOX_ALLOW_FULL_ACCESS=true (double opt-in
# with SANDBOX_POLICY=full_access; see src/config/sandbox.rs). Both default
# off for new tenants.
SANDBOX_POLICY=$sandbox_policy
SANDBOX_ALLOW_FULL_ACCESS=$sandbox_allow_full_access
# Sandbox-visible daemon log (WI#287): the daemon writes a secret-scrubbed copy
# of its own log under state/logs/sandbox, and the shell sandbox mounts it
# read-only at /tenant-logs (podman 5+, or podman 4.3+ when the tenant uid is
# 1000, or docker). info = INFO and above whatever the runtime level; debug =
# follows the runtime level up to DEBUG, where the tool-call lines are; off =
# no copy and no mount. An invalid value fails every lunarwing command that
# loads the config. Preserved on re-render.
SANDBOX_TENANT_LOGS=$sandbox_tenant_logs

# Directories the generic sandbox must never expose (WI#205). The sandbox
# mounts an agent-chosen working directory, so anything reachable from a mount
# root is readable in-container. Protected, `:`-separated:
#
#   env/    GATEWAY_AUTH_TOKEN, WORKER_AUTH_TOKEN, LLM_API_KEY, DB password
#   state/  config.toml (worker auth_tokens), pending-gates, workspace-template
#   run/    lunarwing.sock, and the world-writable ssh-agent/ dir holding
#          ssh-agent.sock (tenant identity)
#   logs/   raw, unscrubbed OpenRC daemon and service logs (WI#287); the agent
#          sees the daemon's scrubbed copy read-only at /tenant-logs instead
#
# Deliberately NOT the whole tenant root: the daemon's WorkingDirectory is
# <root>/ic, and the shell tool defaults to the daemon cwd, so protecting the
# parent would refuse every default sandboxed command. The remaining children
# (ic/, docs/, per-worker workspaces) hold source, not credentials.
#
# Naming these explicitly is required: they are NOT derivable from
# LUNARWING_BASE_DIR ($state_dir is a child of the root; env/ is a sibling).
# The daemon refuses a mount at or inside one of these, and shadows them with
# an empty mount when a legitimate parent (the tenant home) is mounted.
SANDBOX_PROTECTED_PATHS=$(tenant_lw_root "$name")/env:$(tenant_lw_root "$name")/state:$(tenant_lw_root "$name")/run:$(tenant_lw_root "$name")/logs

# Agent tool loop (WI#239): local-tools dev mode off, auto-approve off, and a
# hard cap on tool iterations per turn so a runaway loop cannot spin forever.
ALLOW_LOCAL_TOOLS=$allow_local_tools
AGENT_AUTO_APPROVE_TOOLS=$auto_approve_tools
AGENT_MAX_TOOL_ITERATIONS=$agent_max_tool_iterations

# Skills (WI#239): opt into the SKILL.md self-improvement pipeline by default
# and prune quarantined skills after 15 days so a skill that keeps failing
# does not accumulate on disk indefinitely.
SKILL_SELF_IMPROVEMENT=$skill_self_improvement
SKILL_PRUNE_QUARANTINE_DAYS=$skill_prune_quarantine_days

# Reflex compiler (WI#239): keep disabled by default for new tenants until the
# feature is promoted out of experimental status.
REFLEX_COMPILER_ENABLED=$reflex_compiler_enabled

# Lightweight routines (WI#291): cap on tool-calling rounds per run. A routine
# gets min(its max_tool_rounds, this cap, 10) rounds plus one final text-only
# reply. The cap does not lift a routine's own max_tool_rounds (3 unless set);
# raise one routine as the tenant with:
#   lunarwing routines edit --name <routine> --max-tool-rounds <n>
# (see docs/ops/TENANT-CONFIGURATION.md). More rounds take longer: keep
# ROUTINES_LIGHTWEIGHT_TIMEOUT_SECS (default 300) in mind when raising it.
ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=$routines_lightweight_max_iterations

# Daemon mode
CLI_ENABLED=false
ONBOARD_COMPLETED=true
HEARTBEAT_ENABLED=false
RUST_LOG=lunarwing=info
ENVEOF
  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_port
    darkirc_adapter_port="$(ports_get "$name" darkirc_adapter)" || true
    darkirc_adapter_secret="${darkirc_adapter_secret:-$(generate_token | cut -c1-32)}"
    printf '\nDARKIRC_ADAPTER_URL=http://127.0.0.1:%s\nDARKIRC_ADAPTER_SECRET=%s\n' \
      "$darkirc_adapter_port" "$darkirc_adapter_secret" >> "$staged_env"
  fi

  # LunarVision: preserve an operator-selected external HTTPS instance across
  # re-renders; older tenants without mode metadata continue using local ports.
  local vision_port vision_token vision_service_url
  vision_port="$(ports_get "$name" vision_service)" || true
  if [[ "$vision_mode" == external ]]; then
    vision_service_url="$vision_external_url"
    vision_token="$vision_token_existing"
  else
    vision_service_url="http://127.0.0.1:${vision_port}"
    vision_token="$(_env_existing "$vision_snapshot" LUNARWING_AUTH_TOKEN)"
    vision_token="${vision_token:-$(generate_token)}"
  fi
  if [[ -n "$vision_port" ]]; then
    printf '\nVISION_SERVICE_URL=%s\nVISION_AUTH_TOKEN=%s\n' \
      "$vision_service_url" "$(_quote_env_value "$vision_token")" >> "$staged_env"
  fi
  # LunarVoice is provisioned but disabled until explicitly enabled. Preserve
  # any operator-selected cloud transcription provider on re-render. The
  # sidecar's synthesis clip cap and request budget are mirrored into the
  # daemon env so the gateway's text_too_long pre-flight and staggered proxy
  # timeout stay in sync with the values the sidecar actually enforces
  # (WI#259).
  local voice_port voice_token voice_max_tts_secs voice_req_timeout
  voice_port="$(ports_get "$name" voice_service)" || true
  if [[ -n "$voice_port" ]]; then
    voice_token="$(_env_existing "$voice_snapshot" VOICE_AUTH_TOKEN)"
    voice_token="${voice_token:-$(generate_token)}"
    voice_max_tts_secs="$(_env_existing "$voice_snapshot" VOICE_MAX_TTS_AUDIO_SECS)"
    voice_max_tts_secs="${voice_max_tts_secs:-300}"
    voice_req_timeout="$(_env_existing "$voice_snapshot" VOICE_REQUEST_TIMEOUT_SECS)"
    voice_req_timeout="${voice_req_timeout:-120}"
    printf '\nVOICE_SERVICE_URL=http://127.0.0.1:%s\nVOICE_AUTH_TOKEN=%s\nVOICE_MAX_TTS_AUDIO_SECS=%s\nVOICE_REQUEST_TIMEOUT_SECS=%s\n' \
      "$voice_port" "$voice_token" "$voice_max_tts_secs" "$voice_req_timeout" >> "$staged_env"
    printf 'TRANSCRIPTION_ENABLED=%s\nTRANSCRIPTION_PROVIDER=%s\nTRANSCRIPTION_MODEL=%s\n' \
      "$transcription_enabled" "$transcription_provider" "$transcription_model" >> "$staged_env"
  fi
  [[ -n "$opencode_model" ]]    && printf '\nOPENCODE_MODEL=%s\n'    "$opencode_model"     >> "$staged_env"
  # WI#330: worker base URLs are operator/tenant-supplied, so quote them like
  # the other validated values. An inherited value carries a SOURCE marker so
  # the next render re-derives it and export-tenant does not carry it as a
  # custom override; explicit/operator values carry no marker.
  if [[ -n "$opencode_base_url" ]]; then
    printf 'OPENCODE_BASE_URL=%s\n' "$(_quote_env_value "$opencode_base_url")" >> "$staged_env"
    [[ "$opencode_base_url_source" != "inherited" ]] \
      || printf 'OPENCODE_BASE_URL_SOURCE=inherited\n' >> "$staged_env"
  fi
  [[ -n "$kimi_model" ]]        && printf '\nKIMI_MODEL=%s\n'        "$kimi_model"        >> "$staged_env"
  if [[ -n "$kimi_base_url" ]]; then
    printf 'KIMI_BASE_URL=%s\n' "$(_quote_env_value "$kimi_base_url")" >> "$staged_env"
    [[ "$kimi_base_url_source" != "inherited" ]] \
      || printf 'KIMI_BASE_URL_SOURCE=inherited\n' >> "$staged_env"
  fi
  [[ -n "$kimi_api_key" ]] \
    && printf 'KIMI_API_KEY=%s\n' "$(_quote_env_value "$kimi_api_key")" >> "$staged_env"
  [[ -n "$opencode2_model" ]]    && printf '\nOPENCODE2_MODEL=%s\n'    "$opencode2_model"     >> "$staged_env"
  if [[ -n "$opencode2_base_url" ]]; then
    printf 'OPENCODE2_BASE_URL=%s\n' "$(_quote_env_value "$opencode2_base_url")" >> "$staged_env"
    [[ "$opencode2_base_url_source" != "inherited" ]] \
      || printf 'OPENCODE2_BASE_URL_SOURCE=inherited\n' >> "$staged_env"
  fi
  if [[ -n "$study_notes_home" ]]; then
    printf '\n# Shared LunarWing Study Notes MCP\nLW2STUDYMCP_HOME=%s\n' \
      "$study_notes_home" >> "$staged_env"
    [[ -z "$study_notes_state_home" ]] \
      || printf 'LW2STUDYMCP_STATE_HOME=%s\n' "$study_notes_state_home" >> "$staged_env"
    [[ -z "$study_notes_allow_write" ]] \
      || printf 'LW2STUDYMCP_ALLOW_WRITE=%s\n' "$study_notes_allow_write" >> "$staged_env"
    [[ -z "$knowledge_seed_flag" ]] \
      || printf 'KNOWLEDGE_SEED_FROM_STUDY_MCP=%s\n' "$knowledge_seed_flag" >> "$staged_env"
    [[ -z "$knowledge_sync_enabled" ]] \
      || printf 'KNOWLEDGE_SYNC_ENABLED=%s\n' "$knowledge_sync_enabled" >> "$staged_env"
    [[ -z "$knowledge_sync_cadence" ]] \
      || printf 'KNOWLEDGE_SYNC_CADENCE_HOURS=%s\n' "$knowledge_sync_cadence" >> "$staged_env"
    [[ -z "$knowledge_sync_max_records" ]] \
      || printf 'KNOWLEDGE_SYNC_MAX_RECORDS_PER_RUN=%s\n' "$knowledge_sync_max_records" >> "$staged_env"
    [[ -z "$knowledge_sync_max_bytes" ]] \
      || printf 'KNOWLEDGE_SYNC_MAX_BYTES_PER_RUN=%s\n' "$knowledge_sync_max_bytes" >> "$staged_env"
  fi
  if ! write_tenant_file_atomic "$name" "$path" 600 <"$staged_env"; then
    rm -f -- "$staged_env" "$env_snapshot" "$vision_snapshot" "$voice_snapshot"
    die "could not safely write tenant env: $path"
  fi
  rm -f -- "$staged_env" "$env_snapshot" "$vision_snapshot" "$voice_snapshot"
  say "wrote: $path"
}

write_tenant_bridge_env() {
  local name="$1"
  # Same preservation rule as write_tenant_lunarwing_env: keep the daemon's JID
  # rather than resetting this file to the xmpp.localhost default on a re-run.
  local xmpp_jid
  xmpp_jid="$(resolve_tenant_xmpp_jid "$name" "${2:-}")"
  local xmpp_password="${3:-}"
  local xmpp_allow_from="${4:-}"

  local path bridge_port
  path="$(tenant_env_dir "$name")/xmpp-bridge.env"
  bridge_port="$(ports_get "$name" bridge)"

  local state_dir bridge_token
  state_dir="$(tenant_state_dir "$name")"
  # WI#390 (review IG-1): XMPP_BRIDGE_TOKEN / XMPP_PASSWORD / XMPP_ALLOW_FROM
  # come from one snapshot of lunarwing.env read as the tenant — root
  # resolving the live tenant-writable path would follow a cross-tenant
  # symlink and this writer would then render the other tenant's XMPP
  # credentials into this tenant's bridge env.
  local lunarwing_snapshot
  _snapshot_tenant_env "$name" "$(tenant_env_dir "$name")/lunarwing.env" lunarwing_snapshot
  bridge_token="$(_read_env_value "$lunarwing_snapshot" XMPP_BRIDGE_TOKEN)"
  [[ -n "$bridge_token" ]] || bridge_token="$(generate_token | cut -c1-32)"

  local xmpp_pass_val
  xmpp_pass_val="$(_read_env_value "$lunarwing_snapshot" XMPP_PASSWORD)"
  [[ -n "$xmpp_pass_val" ]] || xmpp_pass_val="${xmpp_password:-$(generate_token | cut -c1-32)}"

  # Idempotent XMPP allow-from JSON: an explicit flag wins; else re-derive from
  # the daemon's XMPP_ALLOW_FROM (CSV) if present so re-runs without the flag
  # keep the operator-set list in sync; else allow the local user by default.
  local xmpp_allow_from_json_effective
  if [[ -n "$xmpp_allow_from" ]]; then
    xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "$xmpp_jid" "$xmpp_allow_from")"
  else
    local daemon_csv
    daemon_csv="$(_read_env_value "$lunarwing_snapshot" XMPP_ALLOW_FROM)"
    # Do not prepend the agent JID: the daemon list may deliberately contain
    # only user@xmpp.localhost (or a custom operator-set allowlist).
    xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "" "${daemon_csv:-user@xmpp.localhost}")"
  fi

  # WI#390: write as the tenant through the atomic writer — root never opens
  # xmpp-bridge.env inside the tenant-writable env dir (see proxy.env). The
  # explicit || die keeps the refusal fatal even when errexit is suppressed.
  cat <<ENVEOF | write_tenant_file_atomic "$name" "$path" 600 || { rm -f -- "$lunarwing_snapshot"; die "could not safely write $path"; }
LUNARWING_BASE_DIR=$state_dir
IRONCLAW_BASE_DIR=$state_dir
XMPP_BRIDGE_BIND=127.0.0.1:${bridge_port}
XMPP_BRIDGE_TOKEN=$bridge_token
XMPP_BRIDGE_MAX_MESSAGES=256
RUST_LOG=xmpp_bridge=info,info

XMPP_JID=$xmpp_jid
XMPP_PASSWORD=$xmpp_pass_val
XMPP_DM_POLICY=allowlist
XMPP_ALLOW_FROM_JSON=$xmpp_allow_from_json_effective
XMPP_ALLOW_ROOMS_JSON=[]
XMPP_ENCRYPTED_ROOMS_JSON=[]
XMPP_DEVICE_ID=0
XMPP_OMEMO_STORE_DIR=$state_dir/xmpp
XMPP_ALLOW_PLAINTEXT_FALLBACK=true
XMPP_RESOURCE=$name
XMPP_BRIDGE_WAIT_SECONDS=15
ENVEOF
  rm -f -- "$lunarwing_snapshot"
  say "wrote: $path"
}

write_tenant_proxy_env() {
  local name="$1"
  local tensorzero_url="${2:-$DEFAULT_TENSORZERO_URL}"

  local path proxy_port
  path="$(tenant_env_dir "$name")/proxy.env"
  proxy_port="$(ports_get "$name" proxy)"

  # WI#390: write as the tenant through the atomic writer — root never opens
  # proxy.env inside the tenant-writable env dir, so a pre-placed or raced
  # symlink there cannot redirect the write onto an attacker-chosen path.
  # The explicit || die keeps the refusal fatal even when errexit is suppressed
  # (e.g. the whole call sits inside an if-condition).
  cat <<ENVEOF | write_tenant_file_atomic "$name" "$path" 600 || die "could not safely write $path"
PROXY_PORT=$proxy_port
PROXY_BIND=127.0.0.1
TENSORZERO_URL=$tensorzero_url
ENVEOF
  say "wrote: $path"
}

write_tenant_darkirc_adapter_env() {
  local name="$1" lock_owned="false"
  if [[ "$DARKIRC_WRITER_LOCK_HELD" != "true" ]]; then
    darkirc_writer_lock "$name"
    lock_owned="true"
  fi

  local path adapter_port darkirc_irc_port lunarwing_env adapter_secret
  path="$(tenant_env_dir "$name")/darkirc-adapter.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  adapter_port="$(ports_get "$name" darkirc_adapter)" || true
  darkirc_irc_port="$(ports_get "$name" darkirc_irc)" || true
  adapter_secret="$(read_tenant_env_value "$name" "$lunarwing_env" DARKIRC_ADAPTER_SECRET)" \
    || die "could not safely read DarkIRC adapter secret for tenant '$name'"
  [[ -n "$adapter_secret" ]] || adapter_secret="$(generate_token | cut -c1-32)"

  # WI#390 (review IG-4): stage the render root-private and commit through the
  # tenant-side atomic writer with a fatal refusal — the previous pipe form
  # ignored a writer refusal (and both callers ignored the function's return),
  # so a symlinked target failed open under the lifecycle lock's suppressed
  # errexit. Staging also honours the writer contract: a fallible producer
  # never pipes straight in.
  local staged rc=0
  staged="$(mktemp)" || die "could not stage the DarkIRC adapter env for tenant '$name'"
  if [[ -f "$TEMPLATES_DIR/darkirc-adapter.env.template" ]]; then
    render_template_content \
      "darkirc-adapter.env.template" \
      "TENANT_NAME=$name" \
      "DARKIRC_IRC_PORT=$darkirc_irc_port" \
      "DARKIRC_ADAPTER_PORT=$adapter_port" \
      "ADAPTER_SECRET=$adapter_secret" >"$staged" || rc=1
  else
    cat >"$staged" <<ENVEOF
DARKIRC_HOST=127.0.0.1
DARKIRC_PORT=$darkirc_irc_port
DARKIRC_NICK=${name}-bridge
DARKIRC_USER=$name
DARKIRC_REALNAME="LunarWing DarkIRC Bridge ($name)"
ADAPTER_HOST=127.0.0.1
ADAPTER_PORT=$adapter_port
ADAPTER_SECRET=$adapter_secret
ADAPTER_LOG_LEVEL=INFO
ENVEOF
  fi
  if [[ "$rc" -eq 0 ]]; then
    write_tenant_file_atomic "$name" "$path" 600 <"$staged" || rc=1
  fi
  rm -f -- "$staged"
  [[ "$rc" -eq 0 ]] || die "could not safely write the DarkIRC adapter env for tenant '$name'"
  say "wrote: $path"
  if [[ "$lock_owned" == "true" ]]; then
    darkirc_writer_unlock
  fi
}

darkirc_scope_id() {
  local name="$1"
  jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' \
    "$PORTS_REGISTRY" 2>/dev/null
}

validate_darkirc_scope_id() {
  local scope_id="$1"
  [[ "$scope_id" =~ ^[0-9a-f]{32}$ && "$scope_id" != "00000000000000000000000000000000" ]] \
    || die "invalid DarkIRC scope ID"
}

darkirc_writer_lock() {
  local name="$1" scope_id lock_file mode owner links enabled current_scope current_enabled
  [[ "$PORTS_LOCK_HELD" != "true" ]] \
    || die "DarkIRC writer lock must be acquired before registry lock"
  acquire_ports_lock
  scope_id="$(darkirc_scope_id "$name")"
  enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_darkirc // false' "$PORTS_REGISTRY" 2>/dev/null || true)"
  release_ports_lock
  [[ "$enabled" == true ]] || die "DarkIRC is disabled for tenant '$name'"
  validate_darkirc_scope_id "$scope_id"
  if [[ "$DARKIRC_WRITER_LOCK_HELD" == "true" ]]; then
    [[ "$DARKIRC_WRITER_SCOPE" == "$scope_id" ]] \
      || die "cannot nest DarkIRC writer locks for different tenants"
    return 0
  fi
  require_cmd flock
  darkirc_path_components_safe "$(dirname "$DARKIRC_WRITER_LOCK_DIR")" \
    || die "unsafe DarkIRC writer lock parent"
  install -d -o root -g root -m 0700 "$DARKIRC_WRITER_LOCK_DIR"
  mode="$(stat -c '%a' "$DARKIRC_WRITER_LOCK_DIR" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$DARKIRC_WRITER_LOCK_DIR" 2>/dev/null || true)"
  [[ "$mode" == 700 && "$owner" == 0 && ! -L "$DARKIRC_WRITER_LOCK_DIR" ]] \
    || die "unsafe DarkIRC writer lock directory"
  lock_file="$DARKIRC_WRITER_LOCK_DIR/$scope_id.lock"
  [[ ! -L "$lock_file" && ( ! -e "$lock_file" || -f "$lock_file" ) ]] \
    || die "unsafe DarkIRC writer lock file"
  if [[ ! -e "$lock_file" ]]; then
    (umask 077; : >"$lock_file")
  fi
  chmod 0600 "$lock_file"
  owner="$(stat -c '%u' "$lock_file" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$lock_file" 2>/dev/null || true)"
  links="$(stat -c '%h' "$lock_file" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$mode" == 600 && "$links" == 1 ]] \
    || die "unsafe DarkIRC writer lock file"
  if [[ -n "${LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD:-}" ]]; then
    local inherited_fd="$LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD"
    local inherited_scope="${LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE:-}"
    [[ "$inherited_scope" == "$scope_id" ]] \
      || die "invalid inherited DarkIRC writer lock"
    inherited_lock_fd_is_held "$inherited_fd" "$lock_file" \
      || die "inherited DarkIRC writer lock does not match tenant scope"
    DARKIRC_WRITER_LOCK_FD="$inherited_fd"
    DARKIRC_WRITER_LOCK_INHERITED="true"
  else
    exec {DARKIRC_WRITER_LOCK_FD}>"$lock_file"
    flock -x -w 10 "$DARKIRC_WRITER_LOCK_FD" \
      || die "DarkIRC config update is busy for tenant '$name'"
  fi
  acquire_ports_lock
  current_scope="$(darkirc_scope_id "$name")"
  current_enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_darkirc // false' "$PORTS_REGISTRY" 2>/dev/null || true)"
  release_ports_lock
  [[ "$current_enabled" == true && "$current_scope" == "$scope_id" ]] \
    || die "DarkIRC tenant identity changed while acquiring writer lock"
  DARKIRC_WRITER_LOCK_HELD="true"
  DARKIRC_WRITER_SCOPE="$scope_id"
}

darkirc_writer_unlock() {
  [[ "$DARKIRC_WRITER_LOCK_HELD" == "true" ]] || return 0
  if [[ "$DARKIRC_WRITER_LOCK_INHERITED" != "true" ]]; then
    flock -u "$DARKIRC_WRITER_LOCK_FD"
    eval "exec ${DARKIRC_WRITER_LOCK_FD}>&-"
  fi
  DARKIRC_WRITER_LOCK_HELD="false"
  DARKIRC_WRITER_LOCK_FD=""
  DARKIRC_WRITER_SCOPE=""
  DARKIRC_WRITER_LOCK_INHERITED="false"
}

with_darkirc_writer_lock() {
  local name="$1" result=0
  shift
  darkirc_writer_lock "$name"
  "$@" || result=$?
  darkirc_writer_unlock
  return "$result"
}

darkirc_load_compatibility() {
  require_cmd jq
  require_cmd sha256sum
  darkirc_resolve_release || die "DarkIRC release selector is missing or unsafe"
  darkirc_sync_release_durability "$DARKIRC_PINNED_RELEASE" \
    || die "DarkIRC release durability repair failed"
  darkirc_fsync_path "$DARKIRC_RELEASE_ROOT" \
    || die "DarkIRC selector durability repair failed"
  local attestation="$DARKIRC_PINNED_RELEASE/attestation.json"
  local shape schema release_id profile digest helper_digest helper_source_revision
  local key_format source_revision expected_release_id actual helper_actual
  shape="$(jq -r 'if type == "object" and
    (keys == ["daemon_sha256","helper_sha256","helper_source_revision","key_format","profile_id","release_id","schema","source_revision"])
    then "valid" else empty end' "$attestation" 2>/dev/null || true)"
  schema="$(jq -r '.schema // empty' "$attestation" 2>/dev/null || true)"
  release_id="$(jq -r '.release_id // empty' "$attestation" 2>/dev/null || true)"
  profile="$(jq -r '.profile_id // empty' "$attestation" 2>/dev/null || true)"
  digest="$(jq -r '.daemon_sha256 // empty' "$attestation" 2>/dev/null || true)"
  helper_digest="$(jq -r '.helper_sha256 // empty' "$attestation" 2>/dev/null || true)"
  helper_source_revision="$(jq -r '.helper_source_revision // empty' "$attestation" 2>/dev/null || true)"
  key_format="$(jq -r '.key_format // empty' "$attestation" 2>/dev/null || true)"
  source_revision="$(jq -r '.source_revision // empty' "$attestation" 2>/dev/null || true)"
  expected_release_id="${source_revision}-${helper_source_revision:0:16}-${digest:7:16}-${helper_digest:7:16}"
  [[ "$shape" == valid \
    && "$schema" == "lunarwing.darkirc-compat/v2" \
    && "$release_id" == "${DARKIRC_PINNED_RELEASE##*/}" \
    && "$profile" == "darkfi-a05956d41-chacha-v1" \
    && "$key_format" == "darkfi-chacha-base58-32" \
    && "$source_revision" == "$DARKIRC_APPROVED_REV" \
    && "$helper_source_revision" =~ ^[0-9a-f]{40}$ \
    && "$digest" =~ ^sha256:[0-9a-f]{64}$ \
    && "$helper_digest" =~ ^sha256:[0-9a-f]{64}$ \
    && "$release_id" == "$expected_release_id" ]] \
    || die "unsupported DarkIRC compatibility attestation"
  actual="$(sha256sum "$DARKIRC_PINNED_RELEASE/darkirc")"
  actual="sha256:${actual%% *}"
  [[ "$actual" == "$digest" ]] || die "DarkIRC binary digest does not match attestation"
  helper_actual="$(sha256sum "$DARKIRC_PINNED_RELEASE/lunarwing-darkirc-key-helper")"
  helper_actual="sha256:${helper_actual%% *}"
  [[ "$helper_actual" == "$helper_digest" ]] \
    || die "DarkIRC helper digest does not match attestation"
  DARKIRC_PINNED_HELPER="$DARKIRC_PINNED_RELEASE/lunarwing-darkirc-key-helper"
  DARKIRC_PINNED_HELPER_SOURCE_REVISION="$helper_source_revision"
  DARKIRC_COMPAT_ARGS=(
    --generator-profile "$profile"
    --binary-sha256 "$digest"
    --binary-path "$DARKIRC_PINNED_RELEASE/darkirc"
    --helper-sha256 "$helper_digest"
    --release-id "$release_id"
    --key-format "$key_format"
    --source-revision "$source_revision"
  )
}

darkirc_release_matches_current_helper_source() {
  local current_revision
  darkirc_validate_clean_root_checkout "$LUNARWING_ROOT" || return 1
  current_revision="$(darkirc_current_helper_source_revision)" || return 1
  [[ "${DARKIRC_PINNED_HELPER_SOURCE_REVISION:-}" == "$current_revision" ]]
}

run_darkirc_manifest_validator() {
  local unbound="${1:-false}"
  darkirc_load_compatibility
  if [[ "$unbound" == "true" ]]; then
    "$DARKIRC_PINNED_HELPER" validate-migration --json --unbound
  else
    "$DARKIRC_PINNED_HELPER" validate-migration --json "${DARKIRC_COMPAT_ARGS[@]}"
  fi
}

run_darkirc_key_helper() {
  local name="$1"
  shift
  darkirc_load_compatibility
  local uid gid home
  uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
  gid="$(id -g "$name")" || die "cannot resolve gid for tenant '$name'"
  home="$(tenant_home "$name")"
  (
    cd "$home"
    exec sudo -u "$name" env -i HOME="$home" PATH="/usr/bin:/bin" \
      "$DARKIRC_PINNED_HELPER" "$@" "${DARKIRC_COMPAT_ARGS[@]}" \
      --expected-uid "$uid" --expected-gid "$gid"
  )
}

darkirc_contact_command() {
  local operation="${1:-}" name="${2:-}"
  if [[ "$operation" == doctor && "$name" == --all ]]; then
    [[ $# -eq 2 ]] || die "darkirc-contact doctor --all accepts no extra arguments"
    darkirc_contact_doctor_all
    return
  fi
  [[ -n "$operation" && -n "$name" ]] \
    || die "usage: darkirc-contact <operation> <tenant> [contact] [options]"
  shift 2
  name="$(sanitize_name "$name")"
  case "$operation" in
    list|status|doctor|exchanges)
      _darkirc_contact_command_locked "$operation" "$name" "$@"
      ;;
    *)
      with_tenant_lifecycle_lock "$name" _darkirc_contact_command_locked \
        "$operation" "$name" "$@"
      ;;
  esac
}

restore_darkirc_migration_locked() {
  local name="$1" scope_id="$2"
  run_darkirc_key_helper "$name" stage-migration \
    --tenant "$name" --scope-id "$scope_id"
  run_darkirc_key_helper "$name" import-migration \
    --tenant "$name" --scope-id "$scope_id"
}

_darkirc_contact_command_locked() {
  local operation="$1" name="$2" contact=""
  shift 2
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  tenant_darkirc_enabled "$name" || die "DarkIRC is disabled for tenant '$name'"
  local scope_id
  scope_id="$(darkirc_scope_id "$name")"
  validate_darkirc_scope_id "$scope_id"

  local -a helper_args=("$operation" --tenant "$name" --scope-id "$scope_id")
  case "$operation" in
    status)
      contact="${1:-}"
      [[ -n "$contact" && $# -eq 1 ]] || die "darkirc-contact status requires exactly one contact name"
      helper_args+=(--contact "$contact" --json)
      ;;
    prepare)
      contact="${1:-}"
      [[ -n "$contact" ]] || die "darkirc-contact prepare requires contact name"
      shift
      darkirc_exchange_prepare_args helper_args "$contact" "$@"
      ;;
    respond)
      contact="${1:-}"
      [[ -n "$contact" ]] || die "darkirc-contact respond requires contact name"
      shift
      darkirc_exchange_respond_args helper_args "$contact" "$@"
      ;;
    complete)
      contact="${1:-}"
      [[ -n "$contact" ]] || die "darkirc-contact complete requires contact name"
      shift
      darkirc_exchange_complete_args helper_args "$contact" "$@"
      ;;
    cancel)
      darkirc_exchange_cancel_args helper_args "$@"
      ;;
    list|doctor|exchanges)
      [[ $# -eq 0 ]] || die "darkirc-contact $operation accepts no extra arguments"
      helper_args+=(--json)
      ;;
    adopt)
      [[ $# -eq 1 && "$1" == --yes ]] || die "adoption requires --yes"
      helper_args+=(--yes --json)
      ;;
    recover|migration-ready|export-migration|import-migration)
      [[ $# -eq 0 ]] || die "darkirc-contact $operation accepts no extra arguments"
      helper_args+=(--json)
      ;;
    stage-migration)
      [[ $# -eq 2 && "$1" == --in && "$2" == - ]] \
        || die "stage-migration requires --in -"
      ;;
    restore-migration)
      [[ $# -eq 0 ]] || die "restore-migration accepts no extra arguments"
      ;;
    *) die "unsupported DarkIRC contact operation '$operation'" ;;
  esac

  if [[ "$operation" == restore-migration ]]; then
    with_darkirc_writer_lock "$name" restore_darkirc_migration_locked "$name" "$scope_id"
    return 0
  fi

  case "$operation" in
    adopt|prepare|respond|complete|cancel|recover|migration-ready|export-migration|stage-migration|import-migration)
      with_darkirc_writer_lock "$name" run_darkirc_key_helper "$name" "${helper_args[@]}"
      ;;
    list|status|doctor|exchanges)
      run_darkirc_key_helper "$name" "${helper_args[@]}"
      ;;
  esac
}

darkirc_exchange_prepare_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local contact="$2" out="" expires="" json=false
  shift 2
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --out) [[ $# -ge 2 && -z "$out" ]] || die "prepare requires one --out value"; out="$2"; shift 2 ;;
      --expires) [[ $# -ge 2 && -z "$expires" ]] || die "prepare accepts one --expires value"; expires="$2"; shift 2 ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown prepare argument: $1" ;;
    esac
  done
  [[ -n "$out" ]] || die "prepare requires --out <file|->"
  args_ref+=(--contact "$contact" --out "$out")
  [[ -z "$expires" ]] || args_ref+=(--expires "$expires")
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_exchange_respond_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local contact="$2" input="" out="" fingerprint="" expires="" json=false
  shift 2
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --in) [[ $# -ge 2 && -z "$input" ]] || die "respond requires one --in value"; input="$2"; shift 2 ;;
      --out) [[ $# -ge 2 && -z "$out" ]] || die "respond requires one --out value"; out="$2"; shift 2 ;;
      --expect-peer-fingerprint) [[ $# -ge 2 && -z "$fingerprint" ]] || die "respond requires one fingerprint"; fingerprint="$2"; shift 2 ;;
      --expires) [[ $# -ge 2 && -z "$expires" ]] || die "respond accepts one --expires value"; expires="$2"; shift 2 ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown respond argument: $1" ;;
    esac
  done
  [[ -n "$input" && -n "$out" && -n "$fingerprint" ]] \
    || die "respond requires --in, --out, and --expect-peer-fingerprint"
  args_ref+=(--contact "$contact" --in "$input" --out "$out" --expect-peer-fingerprint "$fingerprint")
  [[ -z "$expires" ]] || args_ref+=(--expires "$expires")
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_exchange_complete_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local contact="$2" input="" exchange_id="" fingerprint="" json=false
  shift 2
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --in) [[ $# -ge 2 && -z "$input" ]] || die "complete requires one --in value"; input="$2"; shift 2 ;;
      --exchange-id) [[ $# -ge 2 && -z "$exchange_id" ]] || die "complete requires one exchange ID"; exchange_id="$2"; shift 2 ;;
      --expect-peer-fingerprint) [[ $# -ge 2 && -z "$fingerprint" ]] || die "complete requires one fingerprint"; fingerprint="$2"; shift 2 ;;
      --defer-apply) shift ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown complete argument: $1" ;;
    esac
  done
  [[ -n "$input" && -n "$exchange_id" && -n "$fingerprint" ]] \
    || die "complete requires --in, --exchange-id, and --expect-peer-fingerprint"
  args_ref+=(--contact "$contact" --in "$input" --exchange-id "$exchange_id" \
    --expect-peer-fingerprint "$fingerprint" --defer-apply)
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_exchange_cancel_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local exchange_id="" json=false
  shift
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --exchange-id) [[ $# -ge 2 && -z "$exchange_id" ]] || die "cancel requires one exchange ID"; exchange_id="$2"; shift 2 ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown cancel argument: $1" ;;
    esac
  done
  [[ -n "$exchange_id" ]] || die "cancel requires --exchange-id"
  args_ref+=(--exchange-id "$exchange_id")
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_contact_doctor_all() {
  local name
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    if tenant_darkirc_enabled "$name"; then
      _darkirc_contact_command_locked doctor "$name"
    fi
  done < <(all_tenant_names)
}

darkirc_prepare_tenant_dirs() {
  local name="$1" state_dir config_dir datastore_dir path
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/darkirc"
  datastore_dir="$config_dir/datastore"
  for path in "$state_dir" "$config_dir" "$datastore_dir"; do
    darkirc_path_components_safe "$path" \
      || die "unsafe DarkIRC state path for tenant '$name': $path"
  done
  sudo -u "$name" mkdir -p "$config_dir" "$datastore_dir" \
    || die "could not create DarkIRC state directories for '$name'"
  sudo -u "$name" chmod 0700 "$config_dir" "$datastore_dir" \
    || die "could not secure DarkIRC state directories for '$name'"
  for path in "$state_dir" "$config_dir" "$datastore_dir"; do
    darkirc_path_components_safe "$path" \
      || die "unsafe DarkIRC state path for tenant '$name': $path"
  done
  [[ -d "$config_dir" && ! -L "$config_dir" \
    && -d "$datastore_dir" && ! -L "$datastore_dir" ]] \
    || die "DarkIRC state path is not a regular directory for '$name'"
}

generate_darkirc_config() {
  local name="$1" state_dir config_dir irc_port rpc_port log_dir scope_id baseline
  local registry_lock_owned="false" writer_lock_owned="false"
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/darkirc"

  if [[ "$DARKIRC_WRITER_LOCK_HELD" != "true" ]]; then
    darkirc_writer_lock "$name"
    writer_lock_owned="true"
  fi
  if [[ "$PORTS_LOCK_HELD" != "true" ]]; then
    acquire_ports_lock
    registry_lock_owned="true"
  fi
  scope_id="$(ensure_darkirc_scope_id "$name")"
  irc_port="$(ports_get "$name" darkirc_irc)" || die "no darkirc_irc port for tenant '$name'"
  rpc_port="$(ports_get "$name" darkirc_rpc)" || die "no darkirc_rpc port for tenant '$name'"
  log_dir="$(tenant_log_dir "$name")"

  # Keep the registry snapshot locked through the semantic config commit so its
  # scope and target ports cannot diverge from the generated baseline.
  darkirc_prepare_tenant_dirs "$name"
  [[ -f "$TEMPLATES_DIR/darkirc_config.toml.template" ]] \
    || die "darkirc config template not found at $TEMPLATES_DIR/darkirc_config.toml.template"
  baseline="$(render_template_content \
    "darkirc_config.toml.template" \
    "TENANT_NAME=$name" \
    "IRC_PORT=$irc_port" \
    "RPC_PORT=$rpc_port" \
    "CONFIG_DIR=$config_dir" \
    "LOG_DIR=$log_dir")"
  printf '%s' "$baseline" | run_darkirc_key_helper "$name" rewrite-baseline \
    --tenant "$name" --scope-id "$scope_id" >/dev/null \
    || die "DarkIRC semantic config update failed for tenant '$name'"
  [[ -f "$config_dir/darkirc_config.toml" && ! -L "$config_dir/darkirc_config.toml" ]] \
    || die "DarkIRC helper did not produce a config for '$name'"
  if [[ "$registry_lock_owned" == "true" ]]; then
    release_ports_lock
  fi
  if [[ "$writer_lock_owned" == "true" ]]; then
    darkirc_writer_unlock
  fi
  say "darkirc config updated for $name (irc=$irc_port rpc=$rpc_port scope=$scope_id)"
}

# ── SSH harness config.toml generation ────────────────────────────────────────
#
# Appends a [[ssh.hosts]] block to the tenant's config.toml so the gateway
# initializes the SSH bridge + agent server at boot. The host defaults to
# localhost (the tenant's own user) for self-referential worker SSH access.
# Keys are stored in the encrypted secrets store (uploaded after the daemon
# starts via upload_tenant_ssh_key), NOT in config.toml.
#
# Idempotent: skips if an SSH host entry already exists in config.toml.
# APPENDS only — never overwrites existing config.toml content.
ensure_ssh_config() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local state_dir config_path

  state_dir="$(tenant_state_dir "$name")"
  config_path="$state_dir/config.toml"

  # Already configured? Skip so re-runs are idempotent.
  if [[ -f "$config_path" ]] && grep -q '\[\[ssh\.hosts\]\]' "$config_path" 2>/dev/null; then
    say "SSH host config already present in $config_path (skipping)"
    return 0
  fi

  sudo -u "$name" mkdir -p "$state_dir"

  # WI#390: stage the current content (read as the tenant), extend the staged
  # copy as root, and commit through the tenant-side atomic writer — root never
  # opens the tenant-writable config.toml for writing.
  local staged_cfg
  staged_cfg="$(mktemp)" || return 1
  if [[ -f "$config_path" ]] \
    && ! tenant_cat "$name" "$config_path" >"$staged_cfg"; then
    rm -f -- "$staged_cfg"
    return 1
  fi
  # Create config.toml with a header if it doesn't exist yet (same pattern as
  # ensure_external_worker_config). APPEND only — never truncate.
  if [[ ! -s "$staged_cfg" ]]; then
    printf '# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).\n' >>"$staged_cfg"
  fi

  # Append the SSH host block. This is valid TOML because [[ssh.hosts]] is an
  # array-of-tables and we only append when no entry exists yet.
  {
    printf '\n# SSH harness — centralized SSH host config for worker integration.\n'
    printf '# Keys are stored in the encrypted secrets store, not on disk.\n'
    printf '[[ssh.hosts]]\n'
    printf 'host = "%s"\n' "$ssh_host"
    printf 'port = 22\n'
    printf 'user = "%s"\n' "$ssh_user"
    printf 'key_type = "ed25519"\n'
    printf 'host_key_mode = "AcceptFirst"\n'
  } >>"$staged_cfg"

  if ! write_tenant_file_atomic "$name" "$config_path" 600 owned-parent <"$staged_cfg"; then
    rm -f -- "$staged_cfg"
    return 1
  fi
  rm -f -- "$staged_cfg"
  say "wrote SSH host config to $config_path (host=$ssh_host user=$ssh_user)"
}

# Print the host values of every [[ssh.hosts]] block in the tenant's
# config.toml, one per line. mt-admin writes this file itself (ensure_ssh_config),
# so the shape is known: a `host = "..."` line inside each [[ssh.hosts]] block.
_ssh_hosts_from_config() {
  local name="$1" config_path
  config_path="$(tenant_state_dir "$name")/config.toml"
  [[ -f "$config_path" ]] || return 0
  awk '
    /^\[\[ssh\.hosts\]\]/ { inblk = 1; next }
    /^\[/                 { inblk = 0 }
    inblk && /^host[ ]*=[ ]*"/ {
      line = $0
      sub(/^host[ ]*=[ ]*"/, "", line)
      sub(/".*$/, "", line)
      print line
    }
  ' "$config_path"
}

# Point the installed WASM ssh tool's capability allowlist at the tenant's
# configured [[ssh.hosts]] hosts (the sidecar ships with a "myhost" placeholder).
# Idempotent: always derived from config.toml. Warn-and-continue on any failure.
# Point the installed WASM ssh tool's capability allowlist at the tenant's
# configured [[ssh.hosts]] hosts (the sidecar ships with a "myhost" placeholder).
# Idempotent: always derived from config.toml. Warn-and-continue on any failure:
# every internal step is guarded so a failure can never errexit the script.
# The source file is tenant-controlled too. jq must read it under that UID;
# only the filtered stdout crosses into the tenant-side atomic writer.
tenant_jq() {  # <tenant> <jq arguments...>
  local name="$1"
  shift
  if [[ "$(id -un)" == "$name" ]]; then
    jq "$@"
  else
    sudo -u "$name" jq "$@"
  fi
}

tenant_cat() {  # <tenant> <file>
  local name="$1" path="$2"
  if [[ "$(id -un)" == "$name" ]]; then
    cat -- "$path"
  else
    sudo -u "$name" cat -- "$path"
  fi
}

# chmod as the tenant: chmod follows symlinks, so running it as root on a
# tenant-writable path could change an attacker-chosen file's mode (WI#390).
tenant_chmod() {  # <tenant> <mode> <file>
  local name="$1" mode="$2" path="$3"
  if [[ "$(id -un)" == "$name" ]]; then
    chmod "$mode" -- "$path"
  else
    sudo -u "$name" chmod "$mode" -- "$path"
  fi
}

patch_ssh_tool_allowlist() {
  local name="$1" caps_path hosts_json
  caps_path="$(tenant_state_dir "$name")/tools/ssh-tool.capabilities.json"

  if [[ ! -f "$caps_path" ]]; then
    say "  ssh-tool allowlist: ssh-tool not installed — nothing to patch"
    return 0
  fi

  command -v jq >/dev/null 2>&1 \
    || { say "WARNING: jq not installed; ssh-tool allowlist left unchanged" >&2; return 0; }

  hosts_json="$(_ssh_hosts_from_config "$name" | jq -R . | jq -s . 2>/dev/null)" \
    || { say "WARNING: could not derive ssh hosts for $name; ssh-tool allowlist left unchanged" >&2; return 0; }
  if [[ -z "$hosts_json" || "$hosts_json" == "[]" ]]; then
    say "  ssh-tool allowlist: no [[ssh.hosts]] in config.toml — leaving sidecar as shipped"
    return 0
  fi

  # Stage the filtered result before the tenant-side commit: a jq failure
  # (unreadable or corrupt source) must leave the installed caps untouched.
  staged="$(mktemp)" \
    || { say "WARNING: mktemp failed; ssh-tool allowlist left unchanged" >&2; return 0; }
  if tenant_jq "$name" --argjson hosts "$hosts_json" \
       '.capabilities.ssh.allowed_hosts = $hosts' "$caps_path" >"$staged" 2>/dev/null \
    && write_tenant_file_atomic "$name" "$caps_path" 600 owned-parent <"$staged"; then
    say "  ssh-tool allowlist set to: $(jq -c . <<<"$hosts_json" 2>/dev/null || printf '%s' "$hosts_json")"
  else
    say "WARNING: failed to patch ssh-tool allowlist at $caps_path (edit .capabilities.ssh.allowed_hosts manually)" >&2
  fi
  rm -f -- "$staged"
}

# True if a TCP connect to host:port succeeds within 2s (pure bash /dev/tcp).
_probe_tcp() {
  local host="$1" port="$2"
  timeout 2 bash -c "exec 3<>/dev/tcp/${host}/${port}" 2>/dev/null
}

# Patch the WeeChat WASM channel capabilities with the tenant's allocated ports
# from the registry. The source caps file (channels-src/weechat/*.capabilities.json)
# has generic default ports (9001/6681) that are wrong for every multi-tenant host.
# This is called after install-wasm and on tenant start so the ports are always
# correct. Idempotent: always derived from the registry.
patch_weechat_caps() {
  local name="$1" caps_path weechat_port adapter_port staged
  caps_path="$(tenant_state_dir "$name")/channels/weechat.capabilities.json"

  if [[ ! -f "$caps_path" ]]; then
    say "  weechat caps: weechat channel not installed — nothing to patch"
    return 0
  fi

  command -v jq >/dev/null 2>&1 \
    || { say "WARNING: jq not installed; weechat caps left unchanged" >&2; return 0; }

  weechat_port="$(ports_get "$name" weechat)" \
    || { say "WARNING: could not read weechat port for $name; caps left unchanged" >&2; return 0; }
  adapter_port="$(ports_get "$name" weechat_adapter)" \
    || { say "WARNING: could not read weechat_adapter port for $name; caps left unchanged" >&2; return 0; }

  local relay_url="http://127.0.0.1:${weechat_port}"
  local adapter_url="http://127.0.0.1:${adapter_port}"

  # Stage the rewritten JSON before the tenant-side commit: a jq failure
  # (unreadable or corrupt source) must leave the installed caps untouched.
  staged="$(mktemp)" \
    || { say "WARNING: mktemp failed; weechat caps left unchanged" >&2; return 0; }
  if tenant_jq "$name" --arg relay "$relay_url" --arg adapter "$adapter_url" \
       '.config.relay_url = $relay | .config.ws_adapter_url = $adapter' \
       "$caps_path" >"$staged" 2>/dev/null \
    && write_tenant_file_atomic "$name" "$caps_path" 600 owned-parent <"$staged"; then
    say "  weechat caps patched: relay=$relay_url adapter=$adapter_url"
  else
    say "WARNING: failed to patch weechat caps at $caps_path" >&2
  fi
  rm -f -- "$staged"
}

# Warn (never fail) if the tenant's configured loopback SSH host has no sshd
# listening. Only 127.0.0.1 entries are probed: remote hosts may legitimately
# be unreachable from this box (firewalls, jump hosts).
warn_if_sshd_unreachable() {
  local name="$1" host
  while IFS= read -r host; do
    [[ "$host" == "127.0.0.1" ]] || continue
    if ! _probe_tcp "$host" 22; then
      say "WARNING: no sshd listening on ${host}:22 — the tenant's SSH tools target this host." >&2
      say "         Enable it with: systemctl enable --now sshd   (or 'ssh' on Debian/Ubuntu)" >&2
    fi
  done < <(_ssh_hosts_from_config "$name")
  # Explicit: this probe is warn-only and must never fail its (bare-statement)
  # callers under set -e, regardless of future edits above.
  return 0
}

# ── SSH key provisioning ──────────────────────────────────────────────────────
#
# Generates an ed25519 key pair for the tenant and adds the public key to the
# tenant's authorized_keys. The private key is staged in the tenant's env dir
# (mode 0600) for upload to the secrets store via upload_tenant_ssh_key after
# the daemon starts. The staged copy is deleted after upload so no key material
# is left on disk.
provision_tenant_ssh_key() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local tenant_home ssh_dir key_path pubkey_path staged_key

  tenant_home="$(tenant_home "$name")"
  ssh_dir="$tenant_home/.ssh"
  key_path="$ssh_dir/id_ed25519_lunarwing"
  pubkey_path="${key_path}.pub"
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"

  # Generate a key pair if one doesn't already exist (idempotent).
  if [[ ! -f "$key_path" ]]; then
    sudo -u "$name" mkdir -p "$ssh_dir"
    sudo -u "$name" chmod 700 "$ssh_dir"
    sudo -u "$name" ssh-keygen -t ed25519 -f "$key_path" -N "" \
      -C "lunarwing-ssh-harness-$name" 2>/dev/null
    say "generated ed25519 SSH key pair for $name"
  else
    say "SSH key already exists for $name (reusing)"
  fi

  # Add the public key to authorized_keys (idempotent: skip if already present).
  local authorized_keys="$ssh_dir/authorized_keys"
  sudo -u "$name" touch "$authorized_keys"
  sudo -u "$name" chmod 600 "$authorized_keys"
  # WI#423 (run-171 LS-3): read the public key as the tenant and hand it to
  # grep on stdin. A root `cat` would follow a tenant-planted symlink, and a
  # command-line argument is visible to every local user through /proc.
  local pubkey_line
  if ! pubkey_line="$(tenant_cat "$name" "$pubkey_path" 2>/dev/null)" || [[ -z "$pubkey_line" ]]; then
    die "cannot read the public key $pubkey_path as tenant '$name'"
  fi
  if ! printf '%s\n' "$pubkey_line" | sudo -u "$name" grep -qF -f - "$authorized_keys" 2>/dev/null; then
    sudo -u "$name" bash -c "cat '$pubkey_path' >> '$authorized_keys'"
    say "added public key to authorized_keys for $name"
  fi

  # Stage the private key for upload (mode 0600, tenant-owned).
  # Deleted by upload_tenant_ssh_key after the daemon ingests it.
  # The tenant-side cp creates the file tenant-owned, so no root chown is
  # needed; a root chown here would follow a symlink swapped in by the tenant
  # (WI#416).
  sudo -u "$name" cp "$key_path" "$staged_key"
  sudo -u "$name" chmod 600 "$staged_key"
  say "staged SSH private key for upload to secrets store"
}

# Ensure existing tenants have the dedicated SSH API credential before a staged
# key causes the fail-closed SSH router to be needed. Existing tokens are never
# rotated; _env_set preserves lunarwing.env ownership and mode atomically.
ensure_ssh_api_token() {
  local name="$1" env_path ssh_api_token
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" && ! -L "$env_path" ]] \
    || { say "ERROR: tenant environment missing or unsafe: $env_path" >&2; return 1; }

  ssh_api_token="$(_read_env_value "$env_path" SSH_API_TOKEN)"
  if [[ -n "$ssh_api_token" ]]; then
    return 0
  fi

  ssh_api_token="$(generate_token)"
  [[ -n "$ssh_api_token" ]] \
    || { say "ERROR: failed to generate SSH API token for tenant '$name'" >&2; return 1; }
  _env_set "$env_path" SSH_API_TOKEN "$ssh_api_token"
  say "generated missing SSH API credential for tenant '$name'"
}

tenant_ssh_api_token_present() {
  local name="$1"
  # WI#423: tenant-side read; an unsafe path reads as absent so the caller
  # regenerates the credential through the tenant-side _env_set writer.
  [[ -n "$(read_tenant_env_value "$name" "$(tenant_env_dir "$name")/lunarwing.env" SSH_API_TOKEN)" ]]
}

# Upload the staged SSH private key to the secrets store via the gateway's SSH
# API. Called AFTER the daemon starts (the API is served by the gateway on the
# HTTP port). Deletes the staged key after upload so no key material persists
# on disk. Init-system-agnostic — uses HTTP, works on both systemd and OpenRC.
upload_tenant_ssh_key() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local http_port staged_key ssh_api_token

  http_port="$(ports_get "$name" http)"
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"

  [[ -f "$staged_key" ]] || { say "no staged SSH key for $name (skipping upload)"; return 0; }
  # WI#423 (run-166 LF-4): the bearer crosses into an Authorization header —
  # read it through the tenant-side per-key reader, not a root _read_env_value
  # of the live path. A refusal (symlinked/foreign-owned env shape) is fatal
  # for the upload; the staged key stays for the next pass.
  if ! ssh_api_token="$(read_tenant_env_value "$name" "$(tenant_env_dir "$name")/lunarwing.env" SSH_API_TOKEN)"; then
    say "ERROR: could not safely read SSH_API_TOKEN for tenant '$name'; staged key not uploaded" >&2
    return 1
  fi
  if [[ -z "$ssh_api_token" ]]; then
    say "ERROR: SSH_API_TOKEN missing for tenant '$name'; staged key not uploaded" >&2
    return 1
  fi

  # Keep the credential out of argv and process listings. curl reads this
  # anonymous config FD for both readiness and upload requests.
  local auth_header="Authorization: Bearer ${ssh_api_token}"

  # WI#423 (run-171 LS-2): the staged key sits in the tenant-writable env
  # dir, so root must not open it directly — a symlink planted there would
  # hand any root-readable file to the tenant's own daemon. Snapshot it AS
  # THE TENANT (tenant_cat refuses what the tenant cannot read) into a
  # root-private temp file before probing the gateway, feed jq from that
  # copy, and remove the copy on every path.
  local key_snapshot upload_result upload_rc=0
  key_snapshot="$(mktemp)" || { say "ERROR: cannot create the SSH key snapshot; staged key not uploaded" >&2; return 1; }
  if ! tenant_cat "$name" "$staged_key" >"$key_snapshot" 2>/dev/null; then
    rm -f -- "$key_snapshot"
    say "ERROR: could not safely read the staged SSH key for tenant '$name' as the tenant; staged key not uploaded" >&2
    return 1
  fi
  if [[ ! -s "$key_snapshot" ]]; then
    rm -f -- "$key_snapshot"
    say "ERROR: staged SSH key for tenant '$name' is empty; staged key not uploaded" >&2
    return 1
  fi

  # Wait for the gateway to be reachable (it may still be starting up).
  local i=0
  while ! curl -sf --max-time 2 \
      -K <(printf 'header = "%s"\n' "$auth_header") \
      "http://127.0.0.1:${http_port}/agent/status" >/dev/null 2>&1; do
    i=$((i + 1))
    [[ $i -lt 15 ]] || { rm -f -- "$key_snapshot"; say "WARNING: authenticated SSH API not reachable on port $http_port after repeated probes; SSH key not uploaded" >&2; return 1; }
    sleep 2
  done

  # Upload the key via the SSH API. The key data is sent as a JSON string
  # (base64 not needed — the API accepts raw PEM/OpenSSH format).
  upload_result="$(jq -n --rawfile key "$key_snapshot" '{key_data: $key}' | \
    curl -sf -X POST "http://127.0.0.1:${http_port}/hosts/${ssh_host}/key" \
      -H "Content-Type: application/json" \
      -K <(printf 'header = "%s"\n' "$auth_header") \
      -d @- 2>/dev/null)" || upload_rc=$?
  rm -f -- "$key_snapshot"
  if [[ "$upload_rc" -ne 0 ]]; then
    say "ERROR: SSH key upload request failed; staged key remains at $staged_key" >&2
    return 1
  fi

  if echo "$upload_result" | jq -e '.success == true' >/dev/null 2>&1; then
    say "SSH key uploaded to secrets store for host $ssh_host"
    # Delete the staged key — it's now in the encrypted secrets store only.
    rm -f "$staged_key"
    say "staged SSH key deleted (key material now only in secrets store)"
  else
    # Never echo the response: a faulty endpoint may reflect key material.
    say "ERROR: SSH key upload was rejected; staged key remains at $staged_key" >&2
    return 1
  fi
}

# Upload the DarkIRC adapter secret to the secrets store via the gateway's
# extension setup API. Called AFTER the daemon starts (the API is served by the
# gateway on the tenant gateway port). The secret is optional — if absent from the tenant
# env, this is a no-op. Idempotent: ExtensionManager::configure overwrites an
# existing secret and refreshes/activates the WASM channel, so re-running
# start_tenant is safe and no daemon restart is required (the credential is read
# live per-request from the secrets store). Init-system-agnostic — uses HTTP,
# works on both systemd and OpenRC. Best-effort: a failed upload warns and
# continues; the daemon keeps running.
upload_tenant_darkirc_secret() {
  local name="$1"
  local env_path gateway_port gateway_token darkirc_adapter_secret upload_result

  # Only tenants provisioned with DarkIRC carry this secret.
  tenant_darkirc_enabled "$name" || return 0

  env_path="$(tenant_env_dir "$name")/lunarwing.env"

  darkirc_adapter_secret="$(read_tenant_env_value "$name" "$env_path" DARKIRC_ADAPTER_SECRET)" \
    || { say "WARNING: could not safely read DARKIRC_ADAPTER_SECRET from $env_path; not uploaded (upload manually via the API)" >&2; return 1; }
  # The secret is optional — nothing to upload if it is not set.
  [[ -n "$darkirc_adapter_secret" ]] || return 0

  gateway_token="$(read_tenant_env_value "$name" "$env_path" GATEWAY_AUTH_TOKEN)" \
    || { say "WARNING: could not safely read GATEWAY_AUTH_TOKEN from $env_path; DarkIRC adapter secret not uploaded (upload manually via the API)" >&2; return 1; }
  if [[ -z "$gateway_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found in $env_path; DarkIRC adapter secret not uploaded (upload manually via the API)" >&2
    return 1
  fi

  gateway_port="$(ports_get "$name" gateway)"
  # Wait for the gateway to be reachable (it may still be starting up).
  if ! _wait_tenant_gateway "$name"; then
    say "WARNING: gateway not reachable on port $gateway_port; DarkIRC adapter secret not uploaded (upload manually via the API)" >&2
    return 1
  fi

  # Upload the secret via the extension setup API. The adapter secret is sent
  # on stdin (never argv), and the gateway auth token is sent via curl -K
  # reading an anonymous process-substitution FD (never argv) so neither
  # credential appears in /proc/<pid>/cmdline or shell history.
  local auth_header="Authorization: Bearer ${gateway_token}"
  upload_result="$(jq -n --arg secret "$darkirc_adapter_secret" \
    '{secrets:{darkirc_adapter_secret:$secret},fields:{}}' | \
    curl -sf -X POST "http://127.0.0.1:${gateway_port}/api/extensions/darkirc/setup" \
      -H "Content-Type: application/json" \
      -K <(printf 'header = \"%s\"\\n' "$auth_header") \
      -d @- 2>&1)" || true

  if echo "$upload_result" | jq -e '.success == true' >/dev/null 2>&1; then
    say "DarkIRC adapter secret uploaded to secrets store"
    return 0
  else
    # Do NOT echo $upload_result: the gateway or adapter may echo the secret
    # back in the error message. Emit a generic warning instead.
    say "WARNING: DarkIRC adapter secret upload failed (upload manually via the API)" >&2
    return 1
  fi
}

# Seed the Brave Search API key into the web-search tool's secrets store so
# fresh tenants have working web search on first boot (2026-09-05, Christopher).
# Mirrors upload_tenant_darkirc_secret: best-effort, secret via stdin only,
# auth token via anonymous process-substitution FD (never argv/cmdline).
# Source of truth: the host admin's BRAVE_API_KEY (read once at provision time;
# tenants hold their own encrypted copy — rotating the source key later does not
# affect already-provisioned tenants).
upload_tenant_brave_search_secret() {
  local name="$1"
  local brave_key gateway_port gateway_token upload_result

  # Optional: nothing to seed if the host admin has no Brave key configured.
  # Source of truth: BRAVE_SEARCH_API_KEY (the herd-canonical name, matching
  # ~/.hermes/.env); falls back to BRAVE_API_KEY for parity with the tool's
  # declared env_var if an admin exports only that.
  brave_key="${BRAVE_SEARCH_API_KEY:-${BRAVE_API_KEY:-}}"
  [[ -n "$brave_key" ]] || return 0

  gateway_token="$(read_tenant_env_value "$name" "$(tenant_env_dir "$name")/lunarwing.env" GATEWAY_AUTH_TOKEN)" \
    || { say "WARNING: could not safely read GATEWAY_AUTH_TOKEN for tenant $name; Brave key not uploaded (upload manually via the API)" >&2; return 1; }
  if [[ -z "$gateway_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found for tenant $name; Brave key not uploaded (upload manually via the API)" >&2
    return 1
  fi

  gateway_port="$(ports_get "$name" gateway)"

  if ! _wait_tenant_gateway "$name"; then
    say "WARNING: gateway not reachable on port $gateway_port; Brave key not uploaded (upload manually via the API)" >&2
    return 1
  fi

  local auth_header="Authorization: Bearer $gateway_token"
  upload_result="$(jq -n --arg secret "$brave_key" \
    '{secrets:{brave_api_key:$secret},fields:{}}' | \
    curl -sf -X POST "http://127.0.0.1:${gateway_port}/api/extensions/web-search/setup" \
      -H "Content-Type: application/json" \
      -K <(printf 'header = \"%s\"\n' "$auth_header") \
      -d @- 2>&1)" || true

  if echo "$upload_result" | jq -e '.success == true' >/dev/null 2>&1; then
    say "Brave Search key uploaded to secrets store"
    return 0
  else
    say "WARNING: Brave Search key upload failed (upload manually via the API)" >&2
    return 1
  fi
}

patch_tenant_coordination_env() {
  local name="$1" env_path="$2"
  local coord_admin_port coord_admin_token
  # Mode-harden before any edit can fail — a mid-backfill die must not
  # leave the env world-readable. Done as the tenant: chmod follows
  # symlinks, so a root chmod here could rewrite an attacker-chosen
  # file's mode (WI#390).
  tenant_chmod "$name" 0600 "$env_path"
  # WI#390 (review IG-1): every value/presence read below comes from ONE
  # snapshot taken as the tenant — never from root resolving the live
  # tenant-writable path. A cross-tenant symlink present for the read window
  # is unreadable to the tenant-side cat and dies here; without the snapshot,
  # root's reads would follow it and the appends below would write the other
  # tenant's COORD_ADMIN_TOKEN/COORD_ADMIN_PORT into this tenant's env.
  local env_snapshot
  _snapshot_tenant_env "$name" "$env_path" env_snapshot
  coord_admin_port="$(_env_existing "$env_snapshot" COORD_ADMIN_PORT)"
  if [[ -z "$coord_admin_port" ]]; then
    coord_admin_port="$(ports_get "$name" coord_admin || true)"
    [[ -n "$coord_admin_port" ]] || die "coordination admin port missing for tenant '$name'"
  fi
  coord_admin_token="$(_env_existing "$env_snapshot" COORD_ADMIN_TOKEN)"
  [[ -n "$coord_admin_token" ]] || coord_admin_token="$(generate_token)"
  if ! grep -q '^COORD_ENABLED=' "$env_snapshot"; then
    printf '\n# Multi-agent coordination (disabled until channels are configured explicitly)\nCOORD_ENABLED=false\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_ENABLED=false to $env_path"
  fi
  if ! grep -q '^COORD_AGENT_ID=' "$env_snapshot"; then
    printf 'COORD_AGENT_ID=%s\n' "$name" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_AGENT_ID=$name to $env_path"
  fi
  if ! grep -q '^COORD_CHANNELS=' "$env_snapshot"; then
    printf 'COORD_CHANNELS=\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_CHANNELS to $env_path"
  fi
  if ! grep -q '^COORD_AGENT_ALIASES=' "$env_snapshot"; then
    printf 'COORD_AGENT_ALIASES=\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_AGENT_ALIASES to $env_path"
  fi
  if ! grep -q '^COORD_RESPONDER_ROSTER=' "$env_snapshot"; then
    printf 'COORD_RESPONDER_ROSTER=\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_RESPONDER_ROSTER to $env_path"
  fi
  if ! grep -q '^COORD_KNOWLEDGE_SHARE=' "$env_snapshot"; then
    printf 'COORD_KNOWLEDGE_SHARE=false\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_KNOWLEDGE_SHARE=false to $env_path"
  fi
  if ! grep -q '^COORD_KNOWLEDGE_PUBLISH=' "$env_snapshot"; then
    printf 'COORD_KNOWLEDGE_PUBLISH=false\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_KNOWLEDGE_PUBLISH=false to $env_path"
  fi
  if ! grep -q '^COORD_KNOWLEDGE_PUBLISHERS=' "$env_snapshot"; then
    printf 'COORD_KNOWLEDGE_PUBLISHERS=\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_KNOWLEDGE_PUBLISHERS= (empty allowlist) to $env_path"
  fi
  if ! grep -q '^COORD_KNOWLEDGE_MAX_PER_HOUR=' "$env_snapshot"; then
    printf 'COORD_KNOWLEDGE_MAX_PER_HOUR=30\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_KNOWLEDGE_MAX_PER_HOUR=30 to $env_path"
  fi
  if ! grep -q '^COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=' "$env_snapshot"; then
    printf 'COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=300\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR=300 to $env_path"
  fi
  if ! grep -q '^COORD_KNOWLEDGE_MIN_CHARS=' "$env_snapshot"; then
    printf 'COORD_KNOWLEDGE_MIN_CHARS=12\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_KNOWLEDGE_MIN_CHARS=12 to $env_path"
  fi
  if ! grep -q '^COORD_ADMIN_USERS=' "$env_snapshot"; then
    printf 'COORD_ADMIN_USERS=\n' | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_ADMIN_USERS to $env_path"
  fi
  if grep -q '^COORD_ADMIN_TOKEN=$' "$env_snapshot"; then
    _tenant_env_sed "$name" "$env_path" "s/^COORD_ADMIN_TOKEN=$/COORD_ADMIN_TOKEN=$coord_admin_token/" || die "could not safely update $env_path"
    say "repaired empty COORD_ADMIN_TOKEN in $env_path"
  elif ! grep -q '^COORD_ADMIN_TOKEN=' "$env_snapshot"; then
    printf 'COORD_ADMIN_TOKEN=%s\n' "$coord_admin_token" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_ADMIN_TOKEN to $env_path"
  fi
  if grep -q '^COORD_ADMIN_PORT=$' "$env_snapshot"; then
    _tenant_env_sed "$name" "$env_path" "s/^COORD_ADMIN_PORT=$/COORD_ADMIN_PORT=$coord_admin_port/" || die "could not safely update $env_path"
    say "repaired empty COORD_ADMIN_PORT=$coord_admin_port in $env_path"
  elif ! grep -q '^COORD_ADMIN_PORT=' "$env_snapshot"; then
    printf 'COORD_ADMIN_PORT=%s\n' "$coord_admin_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added COORD_ADMIN_PORT=$coord_admin_port to $env_path"
  fi
  rm -f -- "$env_snapshot"
}

# WI#330: backfill the LLM base URL of every ENABLED external worker whose
# lunarwing.env lacks one (tenants provisioned before the worker images
# started refusing their placeholder endpoints). The value is inherited from
# the tenant's own endpoint — LLM_BASE_URL when a worker container can reach
# it, else the proxy.env TensorZero upstream — and marked
# <KEY>_SOURCE=inherited so full renders re-derive it and export-tenant skips
# it. Dies when an enabled worker's URL cannot be derived: rebuilding worker
# images onto a tenant that cannot start them must fail here, not at the
# worker's first LLM call. Idempotent; existing values are never touched.
backfill_tenant_worker_base_urls() {  # <tenant>
  local name="$1"
  local env_path proxy_env llm_base_url tensorzero_url derived
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path"
  # WI#390 (review IG-1): endpoint values are read from tenant-side snapshots,
  # never from root resolving the tenant-writable paths — a cross-tenant
  # symlink present for the read window is unreadable to the tenant-side cat
  # and dies instead of writing another tenant's endpoints into this env.
  local env_snapshot proxy_snapshot
  _snapshot_tenant_env "$name" "$env_path" env_snapshot
  llm_base_url="$(_read_env_value "$env_snapshot" LLM_BASE_URL)"
  tensorzero_url=""
  proxy_env="$(tenant_env_dir "$name")/proxy.env"
  proxy_snapshot=""
  if [[ -e "$proxy_env" || -L "$proxy_env" ]]; then
    _snapshot_tenant_env "$name" "$proxy_env" proxy_snapshot
    tensorzero_url="$(_read_env_value "$proxy_snapshot" TENSORZERO_URL)"
  fi
  derived="$(derive_worker_llm_base_url "$llm_base_url" "$tensorzero_url" || true)"
  local worker key
  for worker in opencode kimi opencode2; do
    key="$(printf '%s_BASE_URL' "${worker^^}")"
    tenant_worker_enabled "$name" "$worker" || continue
    [[ -z "$(_read_env_value "$env_snapshot" "$key")" ]] || continue
    [[ -n "$derived" ]] \
      || { rm -f -- "$env_snapshot" ${proxy_snapshot:+"$proxy_snapshot"}; die "tenant '$name' has the $worker worker enabled but no container-reachable LLM endpoint to inherit (LLM_BASE_URL is unset/loopback and proxy.env has no usable TENSORZERO_URL). Set one explicitly: $0 configure-$worker $name --base-url <url>"; }
    printf '%s=%s\n%s_SOURCE=inherited\n' "$key" "$(_quote_env_value "$derived")" "$key" | _tenant_env_append "$name" "$env_path" || { rm -f -- "$env_snapshot" ${proxy_snapshot:+"$proxy_snapshot"}; die "could not safely update $env_path"; }
    say "added $key (inherited from the tenant's LLM endpoint) to $env_path"
  done
  rm -f -- "$env_snapshot" ${proxy_snapshot:+"$proxy_snapshot"}
}

patch_tenant_env() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path"

  # LUNARWING_OWNER_ID: sets the daemon's DB-scoping owner_id so sessions,
  # memory, and settings are isolated per tenant. Without it the daemon
  # defaults to "default", sharing state across all tenants on the host.
  if grep -q '^LUNARWING_OWNER_ID=' "$env_path"; then
    say "LUNARWING_OWNER_ID already set in $env_path (skipping)"
  else
    printf '\n# Runtime identity (DB scope — must match tenant name)\nLUNARWING_OWNER_ID=%s\n' "$name" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added LUNARWING_OWNER_ID=$name to $env_path"
    # Auto-migrate existing DB data from 'default' scope to the tenant's scope
    # so the daemon doesn't lose access to conversations, memory, and settings.
    if _owner_scope_needs_migration "$name"; then
      say "  found 'default'-scoped DB data; migrating to '$name' scope"
      migrate_owner_scope "$name" || say "  WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
    fi
  fi

  local orchestrator_port
  orchestrator_port="$(ports_get "$name" orchestrator)"

  if grep -q '^ORCHESTRATOR_PORT=' "$env_path"; then
    say "ORCHESTRATOR_PORT already set in $env_path (skipping)"
  else
    printf '\n# Orchestrator (sandbox container callback)\nORCHESTRATOR_PORT=%s\n' "$orchestrator_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
    say "added ORCHESTRATOR_PORT=$orchestrator_port to $env_path"
  fi

  local pebble_wss_port
  pebble_wss_port="$(ports_get "$name" pebble_wss)"
  if [[ -n "$pebble_wss_port" ]]; then
    if grep -q '^PEBBLE_WSS_PORT=' "$env_path"; then
      say "PEBBLE_WSS_PORT already set in $env_path (skipping)"
    else
      printf '\n# Pebble worker (WebSocket port for agent communication)\nPEBBLE_WSS_PORT=%s\n' "$pebble_wss_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
      say "added PEBBLE_WSS_PORT=$pebble_wss_port to $env_path"
    fi
  fi

  local opencode_wss_port
  opencode_wss_port="$(ports_get "$name" opencode_wss)" || true
  if [[ -n "$opencode_wss_port" ]]; then
    if grep -q '^OPENCODE_WSS_PORT=' "$env_path"; then
      say "OPENCODE_WSS_PORT already set in $env_path (skipping)"
    else
      printf '\n# Opencode worker (WebSocket port for agent communication)\nOPENCODE_WSS_PORT=%s\n' "$opencode_wss_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
      say "added OPENCODE_WSS_PORT=$opencode_wss_port to $env_path"
    fi
  fi

  local kimi_wss_port
  kimi_wss_port="$(ports_get "$name" kimi_wss)" || true
  if [[ -n "$kimi_wss_port" ]]; then
    if grep -q '^KIMI_WSS_PORT=' "$env_path"; then
      say "KIMI_WSS_PORT already set in $env_path (skipping)"
    else
      printf '\n# Kimi worker (WebSocket port for agent communication)\nKIMI_WSS_PORT=%s\n' "$kimi_wss_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
      say "added KIMI_WSS_PORT=$kimi_wss_port to $env_path"
    fi
  fi

  local opencode2_wss_port
  opencode2_wss_port="$(ports_get "$name" opencode2_wss)" || true
  if [[ -n "$opencode2_wss_port" ]]; then
    if grep -q '^OPENCODE2_WSS_PORT=' "$env_path"; then
      say "OPENCODE2_WSS_PORT already set in $env_path (skipping)"
    else
      printf '\n# OpenCode 2 worker (WebSocket port for agent communication)\nOPENCODE2_WSS_PORT=%s\n' "$opencode2_wss_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
      say "added OPENCODE2_WSS_PORT=$opencode2_wss_port to $env_path"
    fi
  fi

  local weechat_adapter_port
  weechat_adapter_port="$(ports_get "$name" weechat_adapter)"
  if [[ -n "$weechat_adapter_port" ]]; then
    if grep -q '^WEECHAT_ADAPTER_PORT=' "$env_path"; then
      say "WEECHAT_ADAPTER_PORT already set in $env_path (skipping)"
    else
      printf '\n# WeeChat adapter (local HTTP adapter bridging WeeChat WS relay to WASM)\nWEECHAT_ADAPTER_PORT=%s\n' "$weechat_adapter_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
      say "added WEECHAT_ADAPTER_PORT=$weechat_adapter_port to $env_path"
    fi
    # WS_ADAPTER_URL is the full adapter URL consumed by the in-process WASM
    # channel (via the capabilities `env` source). Without it the channel
    # falls back to the hardcoded :6681 default and silently fails.
    if grep -q '^WS_ADAPTER_URL=' "$env_path"; then
      say "WS_ADAPTER_URL already set in $env_path (skipping)"
    else
      printf 'WS_ADAPTER_URL=http://127.0.0.1:%s\n' "$weechat_adapter_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
      say "added WS_ADAPTER_URL=http://127.0.0.1:$weechat_adapter_port to $env_path"
    fi
  fi

  local weechat_port
  weechat_port="$(ports_get "$name" weechat)"
  if [[ -n "$weechat_port" ]]; then
    if grep -q '^RELAY_URL=' "$env_path"; then
      say "RELAY_URL already set in $env_path (skipping)"
    else
      printf '\n# WeeChat relay URL consumed by the in-process WASM channel\nRELAY_URL=http://127.0.0.1:%s\n' "$weechat_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
      say "added RELAY_URL=http://127.0.0.1:$weechat_port to $env_path"
    fi
  fi

  patch_tenant_coordination_env "$name" "$env_path"

  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_port
    darkirc_adapter_port="$(ports_get "$name" darkirc_adapter)" || true
    if [[ -n "$darkirc_adapter_port" ]]; then
      if grep -q '^DARKIRC_ADAPTER_URL=' "$env_path"; then
        say "DARKIRC_ADAPTER_URL already set in $env_path (skipping)"
      else
        printf '\nDARKIRC_ADAPTER_URL=http://127.0.0.1:%s\n' "$darkirc_adapter_port" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
        say "added DARKIRC_ADAPTER_URL=http://127.0.0.1:$darkirc_adapter_port to $env_path"
      fi
      if grep -q '^DARKIRC_ADAPTER_SECRET=' "$env_path"; then
        say "DARKIRC_ADAPTER_SECRET already set in $env_path (skipping)"
      else
        local darkirc_adapter_secret
        darkirc_adapter_secret="$(generate_token | cut -c1-32)"
        printf 'DARKIRC_ADAPTER_SECRET=%s\n' "$darkirc_adapter_secret" | _tenant_env_append "$name" "$env_path" || die "could not safely update $env_path"
        say "added DARKIRC_ADAPTER_SECRET to $env_path"
      fi
      write_tenant_darkirc_adapter_env "$name" \
        || die "could not safely write the DarkIRC adapter env for tenant '$name'"
      if ports_get "$name" darkirc_irc >/dev/null 2>&1; then
        generate_darkirc_config "$name"
      fi
    fi
  fi

  # WI#330: enabled workers must have a base URL before the tenant starts on
  # the placeholder-refusing images.
  backfill_tenant_worker_base_urls "$name"

  # Wire supported external workers into config.toml so
  # existing tenants get create_job(mode: ...) routing without a hand-edited
  # config file. WI#390 (review HH-2): a refused tenant-side write must abort
  # the patch here — under the lifecycle lock's suppressed errexit an
  # unchecked non-zero return would be swallowed and the tenant would start
  # without its worker config.
  ensure_external_worker_config "$name" "pebble" "pebble_wss" \
    || die "could not safely write the pebble external-worker config for tenant '$name'"
  ensure_external_worker_config "$name" "opencode" "opencode_wss" \
    || die "could not safely write the opencode external-worker config for tenant '$name'"
  ensure_external_worker_config "$name" "kimi" "kimi_wss" \
    || die "could not safely write the kimi external-worker config for tenant '$name'"
  ensure_external_worker_config "$name" "opencode2" "opencode2_wss" \
    || die "could not safely write the opencode2 external-worker config for tenant '$name'"
}

extract_host_from_url() {
  printf '%s' "$1" | sed -E 's|^https?://||; s|[:/].*||'
}

write_tenant_gotify_config() {
  local name="$1"
  local gotify_url="${2:-}"
  local gotify_title="${3:-}"

  [[ -n "$gotify_url" ]] || return 0

  local state_dir config_dir config_path
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/workspace/config"
  config_path="$config_dir/gotify.json"

  sudo -u "$name" mkdir -p "$config_dir"
  gotify_url="$(printf '%s' "$gotify_url" | sed 's|/$||')"
  # WI#390: commit through the tenant-side atomic writer — root never opens
  # gotify.json inside the tenant-writable state tree. The writer's mode 600
  # also hardens the previous umask-dependent 0644. The explicit returns keep
  # the refusal fatal even when errexit is suppressed by the caller's context.
  if [[ -n "$gotify_title" ]]; then
    printf '{"url": "%s", "title": "%s"}\n' "$gotify_url" "$gotify_title" \
      | write_tenant_file_atomic "$name" "$config_path" 600 owned-parent \
      || return 1
  else
    printf '{"url": "%s"}\n' "$gotify_url" \
      | write_tenant_file_atomic "$name" "$config_path" 600 owned-parent \
      || return 1
  fi
  say "wrote: $config_path"
}

configure_gotify_capabilities() {
  local name="$1"
  local gotify_url="${2:-}"

  [[ -n "$gotify_url" ]] || return 0
  require_cmd jq

  local tools_dir caps_path host staged
  tools_dir="$(tenant_state_dir "$name")/tools"
  caps_path="$tools_dir/gotify-tool.capabilities.json"

  [[ -f "$caps_path" ]] || return 0

  host="$(extract_host_from_url "$gotify_url")"
  [[ -n "$host" ]] || return 0

  # Stage the rewritten JSON first: a jq failure must leave the installed
  # caps untouched instead of publishing a partial stream.
  staged="$(mktemp)" || return 1
  if ! tenant_jq "$name" --arg host "$host" '
    .capabilities.http.allowlist[0].host = $host |
    .capabilities.http.credentials.gotify.host_patterns = [$host]
  ' "$caps_path" >"$staged" \
    || ! write_tenant_file_atomic "$name" "$caps_path" 600 owned-parent <"$staged"; then
    rm -f -- "$staged"
    return 1
  fi
  rm -f -- "$staged"
  say "  configured gotify capabilities for host: $host"
}
