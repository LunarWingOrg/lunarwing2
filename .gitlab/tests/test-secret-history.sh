#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
scan_script="$script_dir/../scripts/scan-secret-history.sh"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

secret="sk_"
secret+="live_"
secret+="aB3dE5fG7hJ9kL1mN3pQ5rS7"
new_secret="sk_"
new_secret+="live_"
new_secret+="zY8xW7vU6tS5rQ4pN3mL2kJ1"

commit_fixture() {
    local repo=$1 message=$2
    git -C "$repo" -c "user.name=SweetieBot (autonomous)" \
        -c "user.email=sweetiebot@lunarwing.org" \
        -c commit.gpgsign=false -c core.hooksPath=/dev/null \
        commit -qm "$message"
}

assert_redacted() {
    local log=$1
    if grep -Fq -- "$secret" "$log" || grep -Fq -- "$new_secret" "$log"; then
        printf 'secret-history test: scanner output was not fully redacted\n' >&2
        return 1
    fi
}

run_scan() {
    local repo=$1 expected=$2 label=$3 log=$4 rc
    if (cd "$repo" && bash "$scan_script" "$repo/history.ignore") >"$log" 2>&1; then
        rc=0
    else
        rc=$?
    fi
    assert_redacted "$log"
    if [[ $rc -ne $expected ]]; then
        printf 'secret-history test: %s returned %s, expected %s\n' \
            "$label" "$rc" "$expected" >&2
        return 1
    fi
}

base="$temporary/base"
mkdir -p "$base"
git -C "$base" init -q
printf 'token = "%s"\n' "$secret" >"$base/fixture.txt"
printf 'fixture.txt:stripe-access-token:1\n' >"$base/.gitleaksignore"
git -C "$base" add .
commit_fixture "$base" "baselined synthetic fixture"
fingerprint="$(git -C "$base" rev-parse HEAD):fixture.txt:stripe-access-token:1"
printf '%s\n' "$fingerprint" >"$base/history.ignore"
git -C "$base" add history.ignore
commit_fixture "$base" "pin synthetic fixture fingerprint"
run_scan "$base" 0 "unchanged history" "$temporary/unchanged.log"

new_leak="$temporary/new-leak"
git clone -q "$base" "$new_leak"
printf 'token = "%s"\n' "$new_secret" >"$new_leak/new-fixture.txt"
git -C "$new_leak" add new-fixture.txt
commit_fixture "$new_leak" "new synthetic finding"
run_scan "$new_leak" 1 "new finding" "$temporary/new-leak.log"

repeated="$temporary/repeated"
git clone -q "$base" "$repeated"
rm "$repeated/fixture.txt"
git -C "$repeated" add fixture.txt
commit_fixture "$repeated" "remove synthetic fixture"
printf 'token = "%s"\n' "$secret" >"$repeated/fixture.txt"
git -C "$repeated" add fixture.txt
commit_fixture "$repeated" "repeat baselined synthetic finding"
run_scan "$repeated" 1 "repeated finding in a new commit" "$temporary/repeated.log"

shallow="$temporary/shallow"
git clone -q --depth 1 "file://$base" "$shallow"
run_scan "$shallow" 2 "shallow checkout" "$temporary/shallow.log"

printf 'secret-history: 4 regression tests passed\n'
