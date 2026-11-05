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
    grep -qE 'Failed to fetch http://snapshot\.debian\.org|Some index files failed to download|Unable to fetch some archives' "$attempt_log" ||
        grep -qE "Temporary failure resolving 'snapshot\.debian\.org'|Could not connect to snapshot\.debian\.org" "$attempt_log"
}

while [ "$attempt" -le "$max_attempts" ]; do
    : > "$attempt_rc"
    { timeout "$update_timeout" apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update &&
        timeout "$install_timeout" apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install -y --no-install-recommends "$@" ||
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
