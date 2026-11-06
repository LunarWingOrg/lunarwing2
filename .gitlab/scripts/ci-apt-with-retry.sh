#!/bin/sh
set -u

max_attempts=3
attempt=1
update_timeout="${CI_APT_UPDATE_TIMEOUT:-240}"
install_timeout="${CI_APT_INSTALL_TIMEOUT:-600}"
attempt_log="${CI_PROJECT_DIR:?CI_PROJECT_DIR is required}/.ci-apt-attempt.log"
attempt_rc="${CI_PROJECT_DIR:?CI_PROJECT_DIR is required}/.ci-apt-attempt.rc"

cleanup_attempt_files() {
    rm -f "$attempt_log" "$attempt_rc"
}

transient_snapshot_failure() {
    [ "$1" = 124 ] && return 0
    grep -qE 'Failed to fetch https?://snapshot\.debian\.org|Some index files failed to download|Unable to fetch some archives' "$attempt_log" ||
        grep -qE "Temporary failure resolving 'snapshot\.debian\.org'|Could not connect to snapshot\.debian\.org" "$attempt_log"
}

# The base image's apt fetches only apt itself with automatic retries disabled.
# Both installs share install_timeout, preserving the existing ladder budget.
# Once the fixed client is verified, it installs the requested packages with
# bounded retries; an unreachable index fails the attempt instead of passing
# silently with stale or missing security metadata.
while [ "$attempt" -le "$max_attempts" ]; do
    : > "$attempt_rc"
    { timeout -k 30 "$update_timeout" apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=0 -o Acquire::https::Timeout=60 --error-on=any update &&
        timeout -k 30 "$install_timeout" sh -u -c '
            apt-get -o Acquire::Retries=0 -o Acquire::https::Timeout=60 install -y --no-install-recommends apt &&
            dpkg --compare-versions "$(dpkg-query -W -f="\${Version}" apt)" ge 2.6.2 &&
            apt-get -o Acquire::Retries=3 -o Acquire::https::Timeout=60 install -y --no-install-recommends "$@" &&
            update-ca-certificates
        ' sh "$@" ||
        printf '%s\n' "$?" > "$attempt_rc"; } 2>&1 | tee "$attempt_log"
    if [ ! -s "$attempt_rc" ]; then
        cleanup_attempt_files
        exit 0
    fi

    IFS= read -r status < "$attempt_rc"
    if [ "$attempt" -lt "$max_attempts" ] && transient_snapshot_failure "$status"; then
        case "$attempt" in
            1) base_delay=120 ;;
            *) base_delay=300 ;;
        esac
        delay=$((base_delay + ${CI_JOB_ID:-0} % 30))
        echo "retrying after a transient snapshot.debian.org failure (attempt $attempt/$max_attempts) in ${delay}s" >&2
        sleep "$delay"
        attempt=$((attempt + 1))
    else
        echo "apt snapshot attempt $attempt/$max_attempts failed (exit $status); not retrying" >&2
        cleanup_attempt_files
        exit "$status"
    fi
done
