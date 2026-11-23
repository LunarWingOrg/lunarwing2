#!/bin/sh
# Bounded retry for a download of a pinned input inside a worker image build
# (WI#862). Source it from a RUN step, then wrap each download command:
#
#   . /build/worker-download-retry.sh
#   wdr_retry "lunardepth source clone" "rm -rf /src" \
#       'git clone --filter=blob:none --no-checkout "$LUNARDEPTH_REPOSITORY" /src'
#
# Only the command it wraps is retried. The checks that follow it in the same
# RUN — the repository-URL test, the 40-hex ref tests, rev-parse against the
# pin, the ancestry checks, the lockfile hash, sha256sum -c and the test
# suite — still run once, after a download finally succeeds, so a pin or
# integrity failure can never be papered over by a retry.
#
# The classifier reads the wrapped command's own output for this attempt only
# (the attempt log is truncated first), so text an earlier step printed can
# never classify a later failure — the WI#559 (VK-1) over-match. A refusal,
# an integrity failure or a full disk ends the build on the first attempt.
#
# The lane Dockerfiles are shared by the CI candidate build
# (.gitlab/scripts/build-worker-image.sh) and by mt-admin's build-tenant and
# upgrade-tenant path (ic/scripts/mt-admin-lib/), so this one helper covers
# both. The copy in each lane's scripts/ directory must stay byte-identical:
# .gitlab/tests/test_worker_build_args.py asserts it.

# Keep in step with the time-budget arithmetic in
# .gitlab/scripts/build-worker-image.sh: three attempts from a 20 s base add
# at most 20 + 40 = 60 s, plus two jitter values, per wrapped download.
wdr_max_attempts=3
wdr_base_delay=20
wdr_attempt_log=/tmp/worker-download-retry.log
wdr_attempt_rc=/tmp/worker-download-retry.rc

# Causes that must end the build on the first attempt (WI#862 point 4). These
# win over the transient patterns below, because the same output can carry
# both a connection error and the decision that actually stopped the step.
wdr_fatal() {
    grep -qE "EINTEGRITY|ERR_PNPM_OUTDATED_LOCKFILE|ENOSPC|No space left on device|returned error: (401|403|404)|unexpected status from [A-Z]+ request to [^ ]*: (401|403|404)|remote: (Unauthorized|Forbidden|Not Found)|repository '[^']*' not found|Authentication failed|checksum did NOT match" "$1"
}

# A dropped or refused connection: the edge in front of a pinned input, a
# registry, or a toolchain host. Everything here is retried; nothing else is.
wdr_transient() {
    if wdr_fatal "$1"; then
        return 1
    fi
    grep -qE "RPC failed; curl (18|52|55|56|92)|the remote end hung up unexpectedly|early EOF|unexpected disconnect while reading sideband packet|Could not connect to server|Failed to connect to |Connection reset by peer|Connection timed out|Recv failure|Temporary failure in name resolution|Could not resolve host|ECONNRESET|ECONNREFUSED|ETIMEDOUT|EAI_AGAIN|ENETUNREACH|EPIPE|socket hang up|network timeout|ERR_PNPM_(FETCH|META_FETCH)|Error when performing the request|error: Failed to (install|download)" "$1"
}

# wdr_retry <label> <cleanup command, or - for none> <command>
wdr_retry() {
    _wdr_label=$1
    _wdr_cleanup=$2
    _wdr_command=$3
    _wdr_attempt=1
    while [ "$_wdr_attempt" -le "$wdr_max_attempts" ]; do
        # Truncated per attempt: an earlier attempt's text must never
        # classify this one.
        : > "$wdr_attempt_log"
        : > "$wdr_attempt_rc"
        # tee keeps the download streaming into the build log. The command
        # runs in a subshell (a pipeline member), so its status is recorded
        # from an EXIT trap: that survives both a command that returns
        # non-zero and one that calls exit itself. POSIX sh has no pipefail,
        # so the pipeline's own status is tee's and is not consulted.
        ( trap 'printf "%s\n" "$?" > "$wdr_attempt_rc"' EXIT
          eval "$_wdr_command" ) 2>&1 | tee "$wdr_attempt_log"
        if [ ! -s "$wdr_attempt_rc" ]; then
            echo "worker-download-retry: '$_wdr_label' left no exit status; treating as a failure" >&2
            rm -f "$wdr_attempt_log" "$wdr_attempt_rc"
            return 1
        fi
        _wdr_status=$(cat "$wdr_attempt_rc")
        if [ "$_wdr_status" -eq 0 ]; then
            rm -f "$wdr_attempt_log" "$wdr_attempt_rc"
            return 0
        fi
        if [ "$_wdr_attempt" -lt "$wdr_max_attempts" ] && wdr_transient "$wdr_attempt_log"; then
            # A retry starts from a clean state: a half-fetched checkout or
            # node_modules from the failed attempt is removed first, so no
            # attempt builds from a partial tree.
            if [ "$_wdr_cleanup" != "-" ]; then
                eval "$_wdr_cleanup"
            fi
            _wdr_delay=$((wdr_base_delay * (1 << (_wdr_attempt - 1)) + $$ % 10))
            echo "worker-download-retry: attempt $_wdr_attempt/$wdr_max_attempts of '$_wdr_label' failed on a dropped or refused connection; retrying in ${_wdr_delay}s" >&2
            sleep "$_wdr_delay"
            _wdr_attempt=$((_wdr_attempt + 1))
        else
            echo "worker-download-retry: attempt $_wdr_attempt/$wdr_max_attempts of '$_wdr_label' failed (exit $_wdr_status); not retrying" >&2
            rm -f "$wdr_attempt_log" "$wdr_attempt_rc"
            return "$_wdr_status"
        fi
    done
    # Only reachable if the loop body never returned; a caller under `set -e`
    # must still see a failure, and a caller not under it must see the
    # attempt's own status rather than the loop's zero.
    return "${_wdr_status:-1}"
}
