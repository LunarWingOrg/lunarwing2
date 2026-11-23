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
# .gitlab/tests/test_worker_download_retry.py asserts it.

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
    grep -qiE \
        -e 'EINTEGRITY|ERR_PNPM_TARBALL_INTEGRITY|IntegrityCheckFailed|Integrity check failed|checksum did NOT match' \
        -e 'ERR_PNPM_[A-Z_]*LOCKFILE|lockfile had changes, but lockfile is frozen' \
        -e 'ENOSPC|No space left on device' \
        -e '(returned error:|Server answered with HTTP) (401|403|404)([^0-9]|$)' \
        -e 'unexpected status from [A-Z]+ request to [^ ]*: (401|403|404)([^0-9]|$)' \
        -e '(^|[[:space:]])(E|ERR_PNPM_FETCH_)(401|403|404)([^[:alnum:]]|$)' \
        -e 'error: [A-Z]+ https?://[^ ]+ - (401|403|404)([^0-9]|$)' \
        -e "remote: (Unauthorized|Forbidden|Not Found)|repository '[^']*' not found|Authentication failed" "$1"
}

# A dropped or refused connection: the edge in front of a pinned input, a
# registry, or a toolchain host. Everything here is retried; nothing else is.
wdr_transient() {
    if wdr_fatal "$1"; then
        return 1
    fi
    # Bun's generic "Failed to install/download" and pnpm's generic fetch
    # errors don't identify a transport failure. Corepack does distinguish
    # a fetch exception from an HTTP response in its diagnostic.
    grep -qiE \
        -e 'RPC failed; curl (18|52|55|56|92)([^0-9]|$)|the remote end hung up unexpectedly|early EOF|unexpected disconnect while reading sideband packet' \
        -e 'Could not connect to server|Failed to connect to |Connection (reset by peer|refused|timed out)|Recv failure|Temporary failure in name resolution|Could not resolve host' \
        -e 'ECONNRESET|ECONNREFUSED|ETIMEDOUT|EAI_AGAIN|ENETUNREACH|EPIPE|socket hang up|network timeout' \
        -e 'Error when performing the request to ' \
        -e 'error: (ConnectionRefused|ConnectionClosed|ConnectionReset|Timeout) downloading (package manifest|tarball)' "$1"
}

# wdr_retry <label> <cleanup command, or - for none> <command>
# Use && to chain dependent downloads: the attempt needs an explicit status.
wdr_retry() {
    _wdr_label=$1
    _wdr_cleanup=$2
    _wdr_command=$3
    _wdr_attempt=1
    while [ "$_wdr_attempt" -le "$wdr_max_attempts" ]; do
        # tee truncates the log and streams only this attempt's output.
        # The EXIT trap records the download's status (also for explicit
        # exit), then returns the status of writing it, not the download.
        # Guard the pipeline so LunarDepth's -e and pipefail don't abort
        # classification and a tee or status-file failure stops the build.
        : > "$wdr_attempt_rc" || return 1
        if ! ( trap 'printf "%s\n" "$?" > "$wdr_attempt_rc"; exit "$?"' EXIT
               eval "$_wdr_command" ) 2>&1 | tee "$wdr_attempt_log" ||
            [ ! -s "$wdr_attempt_rc" ]; then
            echo "worker-download-retry: could not capture '$_wdr_label' output or exit status" >&2
            rm -f "$wdr_attempt_log" "$wdr_attempt_rc"
            return 1
        fi
        _wdr_status=$(cat "$wdr_attempt_rc")
        _wdr_retryable=false
        if [ "$_wdr_status" -ne 0 ] && wdr_transient "$wdr_attempt_log"; then
            _wdr_retryable=true
        fi
        rm -f "$wdr_attempt_log" "$wdr_attempt_rc" || return 1
        if [ "$_wdr_status" -eq 0 ]; then
            return 0
        fi
        if [ "$_wdr_retryable" = false ]; then
            echo "worker-download-retry: attempt $_wdr_attempt/$wdr_max_attempts of '$_wdr_label' failed (exit $_wdr_status); not retrying" >&2
            return "$_wdr_status"
        fi
        if [ "$_wdr_attempt" -eq "$wdr_max_attempts" ]; then
            # The CI wrapper accepts this only as the failing step's last
            # output. Recovered downloads never emit it. Keep it on tee's
            # stream and start a new line even after partial command output.
            printf '\nworker-download-retry: transient download retries exhausted (exit %s)\n' "$_wdr_status"
            return "$_wdr_status"
        fi
        if [ "$_wdr_cleanup" != "-" ]; then
            # A retry starts from a clean state: a half-fetched checkout or
            # node_modules from the failed attempt is removed first.
            eval "$_wdr_cleanup" || return "$?"
        fi
        _wdr_delay=$((wdr_base_delay * (1 << (_wdr_attempt - 1)) + $$ % 10))
        echo "worker-download-retry: attempt $_wdr_attempt/$wdr_max_attempts of '$_wdr_label' failed on a dropped or refused connection; retrying in ${_wdr_delay}s" >&2
        sleep "$_wdr_delay" || return "$?"
        _wdr_attempt=$((_wdr_attempt + 1))
    done
    # Only reachable if the loop body never returned; a caller under `set -e`
    # must still see a failure, and a caller not under it must see the
    # attempt's own status rather than the loop's zero.
    return "${_wdr_status:-1}"
}
