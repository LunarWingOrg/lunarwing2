# Pre-Release Testing

> Last reconciled with the repository on 2026-08-02. Do not carry historical
> pass counts forward: record the tested commit and fresh results for each
> release candidate.

Use the full manual checklist in [`../guides/TESTING_GUIDE.md`](../guides/TESTING_GUIDE.md)
in addition to the automated tiers below.

## 1. Rust checks

Run Cargo commands from `ic/`, limited to six CPUs and six jobs:

```bash
cd ic

taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo check -j6 --all-features
taskset -c 0-5 cargo test -j6 -- --test-threads=6
taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples \
  --all-features -- -D warnings
```

The opt-in `integration` feature enables especially heavy test targets. Run it
only in the environment required by those targets, and record any unavailable
external dependency rather than reporting the tier as passed:

```bash
taskset -c 0-5 cargo test -j6 --features integration -- --test-threads=6
```

Current deferred E2E details live in
[`../bugs/BUG-e2e-test-bugs.md`](../bugs/BUG-e2e-test-bugs.md): the two
bootstrap-greeting assertions are ignored because of a harness race, and the
browser clipboard scenario still needs a supported Playwright run. The retired
Telegram and Gmail fixture paths are not current test files.

## 2. Full-stack harness

The harness requires runnable daemon, bridge, and REPL binaries; its `build`
command performs those builds and `--with-wasm` also installs the WASM
artifacts. `up` does not build missing binaries.

```bash
cd ic
scripts/lunarwing-xmpp-test-env.sh init
scripts/lunarwing-xmpp-test-env.sh build --with-wasm
scripts/lunarwing-xmpp-test-env.sh up
scripts/lunarwing-xmpp-test-env.sh verify
scripts/lunarwing-xmpp-test-env.sh down
```

Always run `down` after the test, including after a failed `verify`. If only the
TensorZero proxy check fails, inspect the configured upstream
`LUNARWING_TEST_TENSORZERO_URL` before diagnosing the local install.

See [`HARNESS-SINGLE-TENANT.md`](HARNESS-SINGLE-TENANT.md) and
[`MULTITENANCY-HARNESS.md`](MULTITENANCY-HARNESS.md) for platform-specific and
two-tenant flows.

## 3. Gateway smoke tests

With a harness or staging gateway already running, execute:

```bash
GATEWAY_URL=http://127.0.0.1:9098 \
GATEWAY_AUTH_TOKEN='<token>' \
  ./ic/scripts/release-test.sh
```

Keep the bearer token out of shell history and captured logs. The script checks
gateway APIs and core subsystems; its header lists the channel, worker,
provider, rollback, and monitoring checks that remain manual.

## 4. External-worker matrix

This tier requires Docker Compose and the Python dependencies in
`tests/requirements.txt`:

```bash
python3 -m venv tests/.venv
. tests/.venv/bin/activate
python -m pip install -r tests/requirements.txt
cd tests
python runner.py --mode smoke
```

Use `python runner.py --mode full` for configured chaos scenarios and
`--worker <name>` to isolate one worker. See [`../../tests/README.md`](../../tests/README.md).

## 5. Browser E2E and supplemental checks

- `ic/tests/e2e/` contains the Python/Playwright browser suite; follow
  [`../../ic/tests/e2e/README.md`](../../ic/tests/e2e/README.md).
- `ic/scripts/check-boundaries.sh` checks architecture boundaries.
- `ic/scripts/coverage.sh` generates coverage through `cargo-llvm-cov`; coverage
  is evidence, not a substitute for the release tiers above.

A release candidate is not fully verified unless every required tier has a
fresh result or an explicit, reviewed deferral.
