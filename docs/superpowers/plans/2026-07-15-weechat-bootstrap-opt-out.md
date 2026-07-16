# WeeChat Bootstrap Opt-Out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--no-weechat-bootstrap` across supported new-tenant provisioning surfaces while keeping automatic relay bootstrap enabled by default.

**Architecture:** Reuse the process-local opt-out pattern already used by `--no-health` and `--no-ssh`; do not extend the 21-argument `add_tenant` signature. The opt-out skips WeeChat command execution and `relay.conf` generation but still writes the dedicated minimal `weechat.env`, allowing rendered WeeChat services to start without loading `lunarwing.env`.

**Tech Stack:** Bash, Python 3.10 dataclasses/unittest, Pydantic, vanilla JavaScript, shellcheck.

## Global Constraints

- Default behavior remains automatic WeeChat relay bootstrap.
- The exact flag and serialized field names are `--no-weechat-bootstrap` and `no_weechat_bootstrap`.
- The flag disables relay bootstrap only; service rendering remains enabled.
- The opted-out path writes mode-`0600` `weechat.env` containing only `RELAY_PASSWORD`.
- Do not add a positional argument to `add_tenant`.
- Do not change Kawarimi import behavior or forward the flag through `import-tenant.sh`.
- Preserve systemd/OpenRC parity and secret-handling guarantees.
- Do not create tenants, restart services, or touch live tenant state during verification.
- Do not commit unless the user explicitly requests a commit.

---

### Task 1: Core mt-admin opt-out behavior

**Files:**
- Modify: `ic/scripts/tests/test-weechat-relay-bootstrap.sh:680-752`
- Modify: `ic/scripts/tests/test-add-tenant-env-flags.sh:220-240`
- Modify: `ic/scripts/lunarwing-mt-admin.sh:63-74, 239-278, 6199-6210, 6267, 6894-6979`

**Interfaces:**
- Consumes: existing `_read_env_value <file> <key>`, `_write_weechat_env <tenant> <password>`, and `configure_weechat_relay <tenant>` helpers.
- Produces: process-local `WEECHAT_BOOTSTRAP_OPT_OUT` boolean and CLI flag `--no-weechat-bootstrap` for `add-tenant` and `add-tenants`.

- [ ] **Step 1: Add failing opted-out behavior coverage**

Extend the `add_tenant` section in `test-weechat-relay-bootstrap.sh`. Stub `_write_weechat_env` separately from `configure_weechat_relay`, reset the call log, set `WEECHAT_BOOTSTRAP_OPT_OUT=true`, and assert:

```bash
: >"$call_order_log"
bootstrap_fail_mode=""
WEECHAT_BOOTSTRAP_OPT_OUT=true
DEFAULT_SSH_ENABLED="false" SSH_OPT_OUT="true" CONTAINER_RT="podman" \
  VISION_SIDECAR_IMAGE="dummy" INIT_SYSTEM="systemd" \
  DEFAULT_HEALTH_ENABLED="false" HEALTH_OPT_OUT="true" \
  add_tenant "auto-boot-disabled" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" \
  >"$MT_FIXTURE/disabled-output" 2>&1 || true

assert_fail "opt-out does not invoke WeeChat bootstrap" \
  grep -q '^weechat-bootstrap$' "$call_order_log"
assert_ok "opt-out still writes minimal WeeChat env" \
  grep -q '^weechat-env$' "$call_order_log"
assert_ok "opt-out continues base provisioning" \
  grep -q '^postgres$' "$call_order_log"
assert_ok "opt-out summary is explicit" \
  grep -qF 'weechat relay:    disabled (--no-weechat-bootstrap)' "$MT_FIXTURE/disabled-output"
WEECHAT_BOOTSTRAP_OPT_OUT=false
```

Update the existing `_write_weechat_env` test stub to append `weechat-env` to `call_order_log` while preserving its existing fixture behavior.

- [ ] **Step 2: Run the focused harness and verify RED**

Run:

```bash
bash ic/scripts/tests/test-weechat-relay-bootstrap.sh
```

Expected: nonzero exit because bootstrap is still invoked and the disabled summary does not exist.

- [ ] **Step 3: Add CLI parser regression coverage**

Add `--no-weechat-bootstrap` to the existing `add-tenant` dispatch smoke invocation in `test-add-tenant-env-flags.sh`. Add a second dry parser invocation for `add-tenants` and assert neither reports `unknown flag`.

Expected test intent:

```bash
assert_not_contains "add-tenant accepts --no-weechat-bootstrap" \
  "$dispatch_output" "unknown flag: --no-weechat-bootstrap"
assert_not_contains "add-tenants accepts --no-weechat-bootstrap" \
  "$batch_dispatch_output" "unknown flag: --no-weechat-bootstrap"
```

- [ ] **Step 4: Implement the minimal shell gate**

Add near the other global opt-outs:

```bash
WEECHAT_BOOTSTRAP_OPT_OUT=false   # set true by --no-weechat-bootstrap
```

Add the flag to help and both dispatch blocks:

```bash
--no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
```

Replace the unconditional bootstrap block with:

```bash
local weechat_bootstrap_ok="configured"
if [[ "$WEECHAT_BOOTSTRAP_OPT_OUT" == "true" ]]; then
  local relay_password
  relay_password="$(_read_env_value "$(tenant_env_dir "$name")/lunarwing.env" RELAY_PASSWORD)"
  if [[ -n "$relay_password" ]] && _write_weechat_env "$name" "$relay_password"; then
    weechat_bootstrap_ok="disabled (--no-weechat-bootstrap)"
  else
    weechat_bootstrap_ok="disabled (credential env setup failed)"
    say "WARNING: WeeChat bootstrap was disabled, but the minimal credential env could not be written." >&2
  fi
elif ! configure_weechat_relay "$name" 2>&1; then
  weechat_bootstrap_ok="needs recovery"
  say ""
  say "WARNING: WeeChat relay auto-bootstrap failed for tenant '$name'."
  say "         WeeChat will start but the relay is not configured."
  say "         Recovery: sudo $0 configure-weechat-relay $name"
  say ""
fi
```

Do not pass the global through `add_tenant` positionally.

- [ ] **Step 5: Run core shell verification**

Run:

```bash
bash -n ic/scripts/lunarwing-mt-admin.sh ic/scripts/tests/test-weechat-relay-bootstrap.sh
bash ic/scripts/tests/test-weechat-relay-bootstrap.sh
bash ic/scripts/tests/test-add-tenant-env-flags.sh
shellcheck -S warning ic/scripts/tests/test-weechat-relay-bootstrap.sh
```

Expected: syntax exit 0; both harnesses report `ALL TESTS PASSED`; no new shellcheck warning.

- [ ] **Step 6: Review checkpoint**

Inspect only the Task 1 diff. Confirm default, failure, and disabled states are distinct and no secret value is printed. Do not commit unless explicitly requested.

---

### Task 2: Python onboarding propagation

**Files:**
- Modify: `lunarwing_mt_onboard/config.py:31-121`
- Modify: `lunarwing_mt_onboard/provisioner.py:128-172`
- Modify: `lunarwing_mt_onboard/cli.py:403-456`
- Modify: `lunarwing_mt_onboard/tests.py:30-100, 101-247`
- Modify: `lunarwing_mt_onboard/README.md` configuration example

**Interfaces:**
- Consumes: Task 1 CLI flag `--no-weechat-bootstrap`.
- Produces: `TenantConfig.no_weechat_bootstrap: bool`, serialized resume support, interactive prompt, summary row, and argv forwarding.

- [ ] **Step 1: Write failing config and argv tests**

Add tests proving default false, JSON round-trip, and argument forwarding:

```python
def test_weechat_bootstrap_opt_out_round_trips(self):
    config = TenantConfig(name="alpha", no_weechat_bootstrap=True)
    restored = TenantConfig.from_dict(config.to_dict())
    self.assertTrue(restored.no_weechat_bootstrap)

def test_weechat_bootstrap_opt_out_is_forwarded(self):
    with tempfile.TemporaryDirectory() as tmp:
        script = os.path.join(tmp, "lunarwing-mt-admin.sh")
        with open(script, "w") as f:
            f.write("#!/bin/sh\n")
        os.chmod(script, 0o700)
        previous = provisioner.MT_ADMIN_SCRIPT
        provisioner.MT_ADMIN_SCRIPT = script
        try:
            opted_out = provisioner.build_add_tenant_args(
                TenantConfig(name="alpha", no_weechat_bootstrap=True)
            )
            default = provisioner.build_add_tenant_args(TenantConfig(name="beta"))
        finally:
            provisioner.MT_ADMIN_SCRIPT = previous
    self.assertIn("--no-weechat-bootstrap", opted_out)
    self.assertNotIn("--no-weechat-bootstrap", default)
```

- [ ] **Step 2: Run the Python tests and verify RED**

Run:

```bash
PYTHONPATH=. python3 -m unittest lunarwing_mt_onboard.tests.TestConfigSerialization lunarwing_mt_onboard.tests.TestProvisionerArgs -v
```

Expected: errors for unknown `no_weechat_bootstrap` and missing field.

- [ ] **Step 3: Implement the Python field and forwarding**

Add to `TenantConfig`:

```python
no_weechat_bootstrap: bool = False
```

Add to `from_dict`:

```python
no_weechat_bootstrap=data.get("no_weechat_bootstrap", False),
```

Add to `build_add_tenant_args`:

```python
if config.no_weechat_bootstrap:
    args.append("--no-weechat-bootstrap")
```

Add an enabled-by-default prompt and summary row:

```python
config.no_weechat_bootstrap = not _q_confirm(
    "Automatically configure the WeeChat relay?", default=True
)
table.add_row("WeeChat relay bootstrap", str(not config.no_weechat_bootstrap))
```

- [ ] **Step 4: Update the serialized configuration example**

Add to `lunarwing_mt_onboard/README.md`:

```json
"no_weechat_bootstrap": false
```

Place it adjacent to `no_ssh` and `no_health`.

- [ ] **Step 5: Run Python verification**

Run:

```bash
bash ic/scripts/test-mt-onboard.sh
python3 -m compileall -q lunarwing_mt_onboard
```

Expected: all unit tests pass, module import check passes, compileall exits 0.

- [ ] **Step 6: Review checkpoint**

Verify old resume JSON lacking the field defaults to `False` and therefore retains automatic bootstrap. Do not commit unless explicitly requested.

---

### Task 3: Browser onboarding propagation

**Files:**
- Modify: `lunarwing_mt_onboard_web/models.py:21-68`
- Modify: `lunarwing_mt_onboard_web/web_tests.py:26-39`
- Modify: `lunarwing_mt_onboard_web/static/js/wizard.js:191-252`
- Modify: `lunarwing_mt_onboard_web/README.md`

**Interfaces:**
- Consumes: `TenantConfig.no_weechat_bootstrap` and Task 2 argv forwarding.
- Produces: `ProvisionRequest.no_weechat_bootstrap`, default-enabled UI checkbox, and payload mapping.

- [ ] **Step 1: Write the failing backend mapping test**

Extend `ModelMappingTests.test_provision_maps_fields_and_workers`:

```python
req = ProvisionRequest(
    name="  sphinx  ",
    workers=["nanocode", "bogus", "opencode"],
    no_ssh=True,
    no_weechat_bootstrap=True,
    xmpp_allow_from=["a@x", "  ", "b@y"],
)
cfg = req.to_tenant_config()
self.assertTrue(cfg.no_weechat_bootstrap)
```

- [ ] **Step 2: Run the mapping test and verify RED**

Run:

```bash
PYTHONPATH=. python3 -m unittest lunarwing_mt_onboard_web.web_tests.ModelMappingTests -v
```

Expected: failure because the field is not forwarded.

- [ ] **Step 3: Implement the request mapping**

Add to `ProvisionRequest` and `to_tenant_config`:

```python
no_weechat_bootstrap: bool = False
# ...
no_weechat_bootstrap=self.no_weechat_bootstrap,
```

- [ ] **Step 4: Add the default-enabled wizard control**

Extend the provisioning form state at `wizard.js:84`, where `ssh_harness` and `health_pipeline` default to true:

```javascript
weechat_bootstrap: true,
```

Add the control, review row, and payload mapping:

```javascript
b.appendChild(checkField(data, 'weechat_bootstrap', 'Automatically configure WeeChat relay'));
['WeeChat relay bootstrap', String(data.weechat_bootstrap)],
no_weechat_bootstrap: !data.weechat_bootstrap,
```

- [ ] **Step 5: Add a static UI contract assertion**

In `web_tests.py`, read `static/js/wizard.js` and assert all three contract tokens exist:

```python
wizard = (Path(__file__).parent / "static/js/wizard.js").read_text()
self.assertIn("weechat_bootstrap: true", wizard)
self.assertIn("no_weechat_bootstrap: !data.weechat_bootstrap", wizard)
self.assertIn("Automatically configure WeeChat relay", wizard)
```

- [ ] **Step 6: Update web documentation**

Document that provisioning defaults to automatic relay configuration and that unchecking the control forwards `--no-weechat-bootstrap`; clarify that this does not disable rendered WeeChat services.

- [ ] **Step 7: Run web verification**

Run:

```bash
PYTHONPATH=. python3 -m unittest lunarwing_mt_onboard_web.web_tests -v
```

Expected: all web tests pass.

Because this changes UI, also load the frontend and visual-qa skills during execution and verify the demo wizard at desktop and narrow viewport widths. Confirm the checkbox defaults checked, appears in the review summary, and produces `no_weechat_bootstrap: true` only when unchecked.

- [ ] **Step 8: Review checkpoint**

Confirm import/Kawarimi forms have no new WeeChat opt-out control. Do not commit unless explicitly requested.

---

### Task 4: OpenRC bulk-provisioner propagation

**Files:**
- Modify: `ic/scripts/lunarwing-mt-provision-openrc.sh:54-64, 232-246`
- Create: `ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh`

**Interfaces:**
- Consumes: Task 1 `add-tenants --no-weechat-bootstrap` parser.
- Produces: editable `ENABLE_WEECHAT_BOOTSTRAP=true` OpenRC bulk-provisioning setting.

- [ ] **Step 1: Write a failing static wiring harness**

Create a focused shell test that reads the provisioner without executing it:

```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="$SCRIPT_DIR/../lunarwing-mt-provision-openrc.sh"
failures=0

assert_contains() {
  local label="$1" pattern="$2"
  if grep -qF "$pattern" "$TARGET"; then
    printf '  PASS: %s\n' "$label"
  else
    printf '  FAIL: %s\n' "$label"
    failures=$((failures + 1))
  fi
}

assert_contains "bootstrap defaults enabled" 'ENABLE_WEECHAT_BOOTSTRAP=true'
assert_contains "disabled setting forwards opt-out" '[[ "$ENABLE_WEECHAT_BOOTSTRAP" == false ]] && flags+=(--no-weechat-bootstrap)'

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
```

- [ ] **Step 2: Run the harness and verify RED**

Run:

```bash
bash ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh
```

Expected: nonzero exit with two missing-pattern failures.

- [ ] **Step 3: Implement the OpenRC setting**

Add beside existing feature booleans:

```bash
ENABLE_WEECHAT_BOOTSTRAP=true       # Generate relay.conf during add-tenants
```

Add to `phase_1`:

```bash
[[ "$ENABLE_WEECHAT_BOOTSTRAP" == false ]] && flags+=(--no-weechat-bootstrap)
```

- [ ] **Step 4: Run OpenRC verification**

Run:

```bash
bash -n ic/scripts/lunarwing-mt-provision-openrc.sh ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh
bash ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh
shellcheck -S warning ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh
```

Expected: syntax and shellcheck exit 0; harness reports `ALL TESTS PASSED`.

- [ ] **Step 5: Review checkpoint**

Confirm the setting affects only new tenants created by phase 1 and does not alter OpenRC service rendering. Do not commit unless explicitly requested.

---

### Task 5: Authoritative docs and integrated verification

**Files:**
- Modify: `docs/specs/WeeChat-Relay.md`
- Modify: `docs/ops/WEECHAT-SERVICES.md`
- Modify: `docs/superpowers/specs/2026-07-15-weechat-bootstrap-opt-out-design.md` only if implementation details changed
- Test: all files listed in Tasks 1-4

**Interfaces:**
- Consumes: completed shell, Python, web, and OpenRC behavior.
- Produces: operator-facing flag and recovery documentation with no Kawarimi behavior change.

- [ ] **Step 1: Update the feature specification**

Add a functional requirement stating:

```markdown
- New-tenant provisioning accepts `--no-weechat-bootstrap`. The flag skips
  WeeChat command execution and relay.conf generation, still writes the minimal
  `weechat.env`, still renders services, and is not forwarded by Kawarimi import.
```

- [ ] **Step 2: Update operator documentation**

Add to `WEECHAT-SERVICES.md`:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --no-weechat-bootstrap
```

Explain the three states (`configured`, `needs recovery`, `disabled`), that services remain rendered, and that later recovery uses `configure-weechat-relay <tenant>` followed by preflight.

- [ ] **Step 3: Run the complete focused suite**

Run from the repository root:

```bash
bash -n \
  ic/scripts/lunarwing-mt-admin.sh \
  ic/scripts/lunarwing-mt-provision-openrc.sh \
  ic/scripts/lunarwing-weechat-preflight.sh \
  ic/scripts/tests/test-weechat-relay-bootstrap.sh \
  ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh

bash ic/scripts/tests/test-weechat-relay-bootstrap.sh
bash ic/scripts/tests/test-weechat-service-rendering.sh
bash ic/scripts/tests/test-weechat-preflight.sh
bash ic/scripts/tests/test-add-tenant-env-flags.sh
bash ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh
bash ic/scripts/tests/test-kawarimi-import-flags.sh
bash ic/scripts/test-mt-onboard.sh
PYTHONPATH=. python3 -m unittest lunarwing_mt_onboard_web.web_tests -v
RUN_LIVE_WEECHAT=1 bash ic/scripts/tests/test-weechat-relay-bootstrap.sh
```

Expected: every harness exits 0; Kawarimi still invokes normal bootstrap/preflight behavior; live smoke leaves no temp directory or process.

- [ ] **Step 4: Run lint and diff gates**

Run:

```bash
shellcheck -S warning \
  ic/scripts/lunarwing-weechat-preflight.sh \
  ic/scripts/tests/test-weechat-relay-bootstrap.sh \
  ic/scripts/tests/test-weechat-service-rendering.sh \
  ic/scripts/tests/test-weechat-preflight.sh \
  ic/scripts/tests/test-openrc-weechat-bootstrap-opt-out.sh

python3 -m compileall -q lunarwing_mt_onboard lunarwing_mt_onboard_web
git diff --check
```

Expected: exit 0. Report pre-existing unrelated `lunarwing-mt-admin.sh` shellcheck warnings separately rather than changing them.

- [ ] **Step 5: Run post-implementation review**

Use the required review-work workflow. All review lanes must pass after checking default behavior, opt-out behavior, secret isolation, systemd/OpenRC parity, UI propagation, and unchanged Kawarimi semantics.

- [ ] **Step 6: Final review checkpoint**

Inspect `git status --short` and the scoped diff. Confirm no live state, generated secrets, build artifacts, or unrelated files were added. Do not commit unless explicitly requested.
