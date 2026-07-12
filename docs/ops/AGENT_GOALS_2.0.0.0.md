# AGENT PRE-RELEASE CHECKLIST for 2.0.0.0 Codename `?` (Unknown at this time)

**Open TODOs (2.0.0.0) — To be done before release**

> **NOTE:** Edit the dev autonomous loop routine to use this file. Ensure each checkbox is checked off in the corresponding branch before committing, pushing, and opening a PR. Items are listed in rough recommended order but are not strictly sequential unless noted.

---

1. [x] Ensure references to 1.2.0 in the code and documentation are replaced by 2.0.0 (where applicable only of course)
2. [x] Create ov dir ( cd {this repo} && mkdir -p docs/ov/ )
3. [x] Bump crates version from 1.1.9 to 2.0.0
4. [x] Make sure you DIDNT MISS ANY CRATES. RELEVANT LUNARWING crates and things such as xmpp bridge and wasm channels must have 2.0.0 version, NOT 1.1.9 or 1.1.8
5. [ ] Multiple steps here for this one: First, read the blog post: https://blog.lunarwing.org/2026/07/12/lunarwingv2-the-next-frontier-of-private-self-hosted-ai-agents/ — THEN: Analyze the features not yet included in this AGENT_GOALS_2.0.0.0.md and document all of them into a new document under docs/proposals please.
6. [ ] Drop legacy `ironclaw-agent-v1` subprotocol offer from the daemon (delete the `SUBPROTOCOL_LEGACY` offer in `ic/src/orchestrator/external_worker.rs`) and `git rm` the 1.1.9-only repo-root compat symlinks `ironclaw_weechat_wss`, `darkirc_channel_for_ironclaw` (deployed tenants must have re-run mt-admin unit regen by then)
7. [ ] LunarWing Web UI performance overhaul - lot of issues with UI - laggy, buttons dont animate, etc
8. [x] LunarWing Web MT admin setup integration (part 2 of an earlier plan discussed some time ago) - started - go see: lunarwing_mt_onboard_web/ - this is being worked on by another agent or human
9. [ ] Write up a short doc with details of currently open PRs and Issues. Save it to docs/ops
10. [ ] Inspect status of cargo crates and create documented report of any crates that might still need to be updated. Verify if the info dump below is still correct, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?

    <details>
    <summary><b>INFO DUMP — Crate audit reference</b></summary>

    tower-http | 0.6.10 | 0.7.0 | Available but not required. 0.6.11 patch is available. That's the only crate with a major version available. And it's just 0.7.0 — not a huge jump.

    Things You Might Have Missed:

    1 rand 0.8.6 — Still on 0.8. The dry-run shows 0.10.2 is available but it would be a breaking change (API redesign in 0.9+). If you've deliberately stayed on 0.8, that's fine. Just be aware it's two major versions behind.

    2 base64 0.21.7 — Still on 0.21. Version 0.22 is available. Minor API changes (Engine trait moved). Used in ssh_hostkeys.rs for fingerprint computation.
 
    3 wasmparser 0.220.1 — Bundled with wasmtime 36, so it's fine. The dry-run doesn't try to update it independently.

    4 pathdiff being removed — The dry-run removes pathdiff v0.2.3 as unused. Good, less deps.

    5 wasip3 being removed — Also cleaned up as unused. Good.

    My Recommendation:
    # Safe to run right now — all patch/minor bumps
    cargo update
    # Then verify
    cargo check --lib
    cargo test --lib

    NOTE: Crates to be deferred until 2.0.0+:
    - rand 0.8 → 0.10 — Breaking, but can be deferred
    - base64 0.21 → 0.22 — Minor breaking, can be deferred
    - tower-http 0.6 → 0.7 — Can be deferred

    This issue documents a crate audit. Current state:
    - tower-http 0.6.10 → 0.7.0: Still on 0.6 in ic/Cargo.toml. Deferred per issue notes.
    - rand 0.8.6 → 0.10.2: Still on 0.8 in ic/Cargo.toml. Breaking change, deferred.
    - base64 0.21.7 → 0.22: Still on 0.21 in ic/Cargo.toml. Minor breaking, deferred.
    - The cargo update (patch/minor bumps) recommendation may or may not have been run.

    Verdict: The deferred crates (rand, base64, tower-http) are intentionally held back for 2.0.0+. The patch-level cargo update should be verified. Genuinely open — deferred to 2.0.0+.

    </details>

11. [ ] Run all cargo tests — full `--all-features --no-fail-fast` run: lib 4087 passed/0 failed/4 ignored; all integration binaries + doctests pass except the 6 known-deferred (4 `multi_tenant_system_prompt` architectural, 2 `e2e_advanced_traces` bootstrap-greeting). See `docs/proposals/CARGO_TESTS_FIX.md`.
12. [ ] Fix any remaining broken cargo tests and ensure updated documentation. Create (or rewrite) new tests if necessary. then re-run cargo tests to ensure — Fixed the 3 stale `lib` failures this cycle: `registry::embedded::tests::test_load_embedded_parses` (github→ssh sentinel), `cli::tests::test_help_output` + `test_long_help_output` (accepted rebranded insta snapshots). Lib re-run: 4087 passed/0 failed. The 6 remaining failures are documented known-deferred (architectural / harness), not regressions.
13. [ ] Verify below Gentoo Issue, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?

    <details>
    <summary><b>INFO DUMP — Gentoo WeeChat relay-api</b></summary>

    Issue: -relay-api is disabled (the API protocol needs cJSON). emerge needs cjson.

    Fix: (commands)
    echo "net-irc/weechat relay-api" | sudo tee -a /etc/portage/package.use/weechat
    sudo emerge --oneshot --changed-use net-irc/weechat

    Document the accuracy of this issue.

    </details>

14. [ ] Verify (only) if the information below is accurate, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?
    <details>
    <summary><b>INFO DUMP — WeeChat Services for Multi-Tenant Deployments</b></summary>
    CLI pairing approve env var requirement in WeeChat ops guide. Adds troubleshooting section for the 'no pairing file' error caused by LUNARWING_BASE_DIR not being in the tenant user's shell environment. Includes three workarounds: inline env var, sourcing lunarwing.env, and gateway API. Ultraworked with Sisyphus (https://github.com/code-yeongyu/oh-my-openagent). Co-authored-by: Sisyphus <clio-agent@sisyphuslabs.ai>
WeeChat Services for Multi-Tenant Deployments
    How WeeChat IRC access is managed as init services in LunarWing multi-tenant deployments. Each tenant runs a WeeChat instance in a tmux session plus a Python WebSocket adapter that bridges WeeChat's relay API to an HTTP endpoint polled by the LunarWing WASM channel.
    For general multi-tenant setup, see MULTITENANCY-PRODUCTION.md.
    Architecture:
    WASM channel (poll) ──GET──► ws_adapter.py (port base+9)
                                  │
                                  ▼
                              WebSocket
                                  │
                                  ▼
                         WeeChat relay (port base+5, 127.0.0.1)
    WASM channel (send) ──POST──► WeeChat relay (direct)
    Three components per tenant:
    - WeeChat: weechat in tmux — IRC client, runs relay API on 127.0.0.1:<base+5>
    - WS adapter: ws_adapter.py — Bridges WeeChat's WebSocket relay to a local HTTP API
    - WASM channel: weechat_relay_channel — Polls the adapter every 3s; sends directly to the relay

    The adapter script lives at lunarwing_weechat_wss/weechat_relay/ws_adapter.py in the source repo.

    Service Dependency Chain:
    
    weechat-<name> → lunarwing-weechat-adapter-<name> → lunarwing-<name>


    WeeChat must be running before the adapter starts. The adapter must be running before the main daemon starts. The MT admin script wires this automatically via Requires=/After= (systemd) or need/before (OpenRC).

    Port Allocation:

    WeeChat uses two ports from each tenant's 10-port block (allocated from /etc/lunarwing/ports.json):
    - Offset +5, port name "weechat": WeeChat relay API
    - Offset +9, port name "weechat_adapter": WS adapter HTTP endpoint

    Example: a tenant with base port 10050 gets WeeChat relay on 10055 and adapter on 10059. Both ports bind to 127.0.0.1 only.

    Environment Variables:

    The following are written to lunarwing.env by the MT admin script:
    - RELAY_URL: http://127.0.0.1:<base+5> — consumed by adapter + WASM channel — WeeChat relay endpoint
    - WS_ADAPTER_URL: http://127.0.0.1:<base+9> — consumed by WASM channel — Full adapter URL the in-process WASM channel polls
    - RELAY_PASSWORD: auto-generated 32-char token — consumed by adapter + WeeChat + WASM channel — Shared secret; the WASM authenticates to the adapter with it
    - ADAPTER_PORT: <base+9> — consumed by adapter — Bare HTTP port the standalone adapter listens on
    - WEECHAT_ADAPTER_PORT: <base+9> — consumed by adapter — Alias of ADAPTER_PORT

    RELAY_PASSWORD is generated per tenant during add-tenant and must match the password configured inside WeeChat (see WeeChat Relay Setup below).

    Per-tenant ports & the in-process WASM channel: The LunarWing daemon (which hosts the WeeChat WASM channel in-process) sources relay_url, ws_adapter_url, and relay_password from RELAY_URL, WS_ADAPTER_URL, and RELAY_PASSWORD at startup. Without these the channel falls back to the hardcoded :9001/:6681 defaults and silently fails for every tenant whose ports differ. See the archived WEECHAT-MULTITENANT-PORT-BUG.md in docs/internal/history/archive/ops/. Existing tenants need WS_ADAPTER_URL backfilled — run mt-admin patch-env <name>.

    Generated Service Units — Systemd (user-level):

    Units are installed to ~/.config/systemd/user/ per tenant.

    weechat-<name>.service:
    [Unit]
    Description=WeeChat IRC client (<name>)
    After=network.target

    [Service]
    Type=forking
    ExecStart=/usr/bin/tmux -L weechat-<name> new-session -d -s weechat '/usr/bin/weechat --dir /home/<name>/.config/weechat'
    ExecStop=/usr/bin/tmux -L weechat-<name> kill-session -t weechat
    Restart=on-failure
    RestartSec=5

    [Install]
    WantedBy=default.target
    Uses Type=forking because tmux daemonizes after creating the session.

    lunarwing-weechat-adapter-<name>.service:
    [Unit]
    Description=LunarWing WeeChat WS adapter (<name>)
    After=network.target weechat-<name>.service
    Requires=weechat-<name>.service
    PartOf=lunarwing-<name>.service

    [Service]
    Type=simple
    WorkingDirectory=<repo>/lunarwing_weechat_wss/weechat_relay
    EnvironmentFile=<env_dir>/lunarwing.env
    ExecStart=/usr/bin/python3 <repo>/lunarwing_weechat_wss/weechat_relay/ws_adapter.py
    Restart=on-failure
    RestartSec=5
    NoNewPrivileges=true

    [Install]
    WantedBy=default.target
    The PartOf=lunarwing-<name>.service means stopping the main daemon also stops the adapter.

    Main daemon unit (lunarwing-<name>.service) includes WeeChat services in its dependency list:
    After=... weechat-<name>.service lunarwing-weechat-adapter-<name>.service
    Wants=... weechat-<name>.service lunarwing-weechat-adapter-<name>.service

    Generated Service Units — OpenRC (system-level):

    Init scripts are installed to /etc/init.d/ with conf.d files in /etc/conf.d/.

    /etc/init.d/weechat-<name>: Runs WeeChat in a tmux session via start-stop-daemon. The start() function creates the tmux session; stop() kills it with tmux kill-session. Configurable via conf.d variables: weechat_user, weechat_group, weechat_home.

    Dependency wiring:
    depend() {
        need net
        use dns
        after firewall
        before lunarwing-weechat-adapter-<name> lunarwing-<name>
    }

    /etc/init.d/lunarwing-weechat-adapter-<name>: Uses supervise-daemon with automatic respawn (respawn_delay=5, respawn_max=5, respawn_period=60). Reads env from the tenant's lunarwing.env. Logs to <log_dir>/weechat-adapter.log and <log_dir>/weechat-adapter.err.

    Dependency wiring:
    depend() {
        need net weechat-<name>
        use dns
        after firewall weechat-<name>
        before lunarwing-<name>
    }

    Conf.d for the main daemon (/etc/conf.d/lunarwing-<name>) includes:
    lunarwing_rc_need="xmpp-bridge-<name> lunarwing-proxy-<name> weechat-<name> lunarwing-weechat-adapter-<name>"

    WeeChat Relay Setup:

    After starting the WeeChat service for the first time, the relay must be configured inside WeeChat. Attach to the tmux session and run these commands in WeeChat:
    /relay add api <weechat_port>
    /set relay.network.password "<RELAY_PASSWORD>"
    /set relay.network.bind_address "127.0.0.1"
    Replace <weechat_port> with the tenant's allocated relay port (base+5) and <RELAY_PASSWORD> with the value from lunarwing.env.

    Save the configuration so it persists across restarts:
    /save

    Manual Operations — Attach to WeeChat:

    Each tenant's WeeChat runs in a named tmux socket:
    # As the tenant user
    tmux -L weechat-<name> attach -t weechat
    # As root
    sudo -u <name> tmux -L weechat-<name> attach -t weechat
    Detach with Ctrl-b d (standard tmux detach).

    Manual Operations — Check service status:

    Systemd:
    # As root (for any tenant)
    sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
      systemctl --user status weechat-<name>.service \
                              lunarwing-weechat-adapter-<name>.service
    # As the tenant user
    systemctl --user status weechat-<name>.service
    systemctl --user status lunarwing-weechat-adapter-<name>.service

    OpenRC:
    rc-service weechat-<name> status
    rc-service lunarwing-weechat-adapter-<name> status

    Manual Operations — View logs:

    Systemd:
    sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
      journalctl --user -u weechat-<name>.service -f
    sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
      journalctl --user -u lunarwing-weechat-adapter-<name>.service -f

    OpenRC:
    tail -f /home/<name>/lunarwing/logs/weechat.log
    tail -f /home/<name>/lunarwing/logs/weechat-adapter.log

    Manual Operations — Restart services:

    Restart WeeChat and the adapter together (the dependency chain handles ordering):

    Systemd:
    sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
      systemctl --user restart weechat-<name>.service

    OpenRC:
    rc-service weechat-<name> restart
    rc-service lunarwing-weechat-adapter-<name> restart

    Adding WeeChat to an Existing Tenant:

    If a tenant was created before WeeChat services were added, re-render units and add the environment variables manually.

    Add env vars to lunarwing.env:
    # Get the tenant's WeeChat ports
    sudo ic/scripts/lunarwing-mt-admin.sh status <name>
    # Edit the env file (as the tenant user or root)
    # Add these lines:
    RELAY_URL=http://127.0.0.1:<weechat_port>
    RELAY_PASSWORD=<generate-a-token>
    ADAPTER_PORT=<weechat_adapter_port>
    WEECHAT_ADAPTER_PORT=<weechat_adapter_port>
    WS_ADAPTER_URL=http://127.0.0.1:<weechat_adapter_port>

    RELAY_URL, WS_ADAPTER_URL, and RELAY_PASSWORD are what the in-process WASM channel reads — omitting WS_ADAPTER_URL makes the channel poll the hardcoded :6681 default. mt-admin patch-env <name> adds WS_ADAPTER_URL (and RELAY_URL if missing) idempotently.

    Generate a relay password:
    openssl rand -hex 16

    Create WeeChat config directory:
    sudo -u <name> mkdir -p /home/<name>/.config/weechat

    Re-render service units:
    # Stop the tenant first
    sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant <name>
    # Re-render (regenerates all units including WeeChat)
    sudo ic/scripts/lunarwing-mt-admin.sh render-units <name>
    # Reload and start
    sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>

    Configure the WeeChat relay: Attach to the tmux session and run the relay setup commands (see WeeChat Relay Setup above).

    Troubleshooting — Adapter fails to connect to WeeChat relay:

    Symptom: Adapter logs show connection refused or timeout errors.
    - Verify WeeChat is running: tmux -L weechat-<name> list-sessions
    - Verify the relay is configured inside WeeChat: attach and run /relay list
    - Confirm the relay port matches RELAY_URL in lunarwing.env
    - Confirm relay.network.bind_address is 127.0.0.1 (not 0.0.0.0 or empty)

    Troubleshooting — WeeChat relay not configured:

    Symptom: WeeChat is running but the adapter cannot authenticate.
    The relay must be set up manually inside WeeChat on first start. Attach to the tmux session and run the /relay add api <port> commands described in WeeChat Relay Setup above.

    Troubleshooting — tmux session died:

    Symptom: tmux -L weechat-<name> list-sessions returns "no server running" or "no sessions".

    Restart the WeeChat service:
    # Systemd
    sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
      systemctl --user restart weechat-<name>.service
    # OpenRC
    rc-service weechat-<name> restart

    If the tmux socket file is stale (exists but no server), remove it first:
    rm -f /tmp/tmux-$(id -u <name>)/weechat-<name>

    Troubleshooting — WASM channel reports no messages:

    Symptom: LunarWing is running but not receiving IRC messages.
    - Check the adapter is running and healthy (see Check service status above)
    - Verify WEECHAT_ADAPTER_PORT is set in lunarwing.env
    - Confirm the weechat_relay_channel WASM module is installed in the tenant's state/channels/ directory
    - Check the daemon logs for WASM channel load errors

    Troubleshooting — Password mismatch:

    Symptom: Adapter connects but authentication fails.
    The RELAY_PASSWORD in lunarwing.env must exactly match the value set inside WeeChat via /set relay.network.password. Attach to WeeChat and verify:
    /set relay.network.password
    If they differ, update one to match the other and restart the adapter.

    Troubleshooting — CLI pairing approve fails with "no pairing file":

    Symptom: lunarwing pairing approve weechat <CODE> returns "Invalid channel: no pairing file", even though the pairing request is visible via IRC and the gateway API works.

    The CLI resolves the pairing store from LUNARWING_BASE_DIR. In multi-tenant deployments this is set per-tenant in lunarwing.env (typically /home/<name>/lunarwing/state), but it is not exported into the tenant user's shell environment. Running sudo -u <name> lunarwing pairing approve ... without that variable causes the CLI to look in the wrong directory.

    Either source the env file first, or pass LUNARWING_BASE_DIR explicitly:
    # Option A: pass the variable inline
    sudo -u <name> LUNARWING_BASE_DIR=/home/<name>/lunarwing/state \
      lunarwing pairing approve weechat <CODE>
    # Option B: source the env file
    sudo -u <name> bash -c '
      set -a
      source /home/<name>/lunarwing/env/lunarwing.env
      set +a
      lunarwing pairing approve weechat <CODE>

    Alternatively, use the gateway API (no env vars needed):
    curl -sf -X POST http://127.0.0.1:<gateway_port>/api/pairing/weechat/approve \
      -H "Authorization: Bearer <GATEWAY_AUTH_TOKEN>" \
      -H "Content-Type: application/json" \
      -d '{"code":"<CODE>"}'

    </details>

15. [ ] dark irc key exchange. automate the process secruely. ** For this task I want you to help plan it out. Write up a doc in docs/proposals **
16. [ ] memory_impl: implement third party memory cleaning, de-duping, correction routines into project. I've created several advanced memory de-duplication routines on my own which are being used across three production agents (on 1.1.2). My goal is to either integrate these routines into LunarWing directly, or make it easy for new users to import them. I'll look into adding more to this idea in this issue at some point. kind of just a stub for the timebeing. ** For this task I want you to help plan it out. Write up a doc in docs/proposals **
17. [x] Work on integrated testing routing - checked this off because not giving auto-dev-loop permission for this
18. [ ] MCP additions: (please reference the following branch for a hint on getting started, old failed implementation from previous opencode worker: faility/failed-partial-old-item-3-20260711-0601) Pick ONE from the recommended list below, implement it, check off this box.

    <details>
    <summary><b>DONE — First-class host-local stdio MCP installation</b></summary>

    Implemented first-class host-local stdio MCP installation across LunarWing:
    - Added typed stdio MCP registry manifests with command, structured args, and non-secret env.
    - Added a shared validated ExtensionManager::install_mcp_config persistence path.
    - Extended conversational tool_install and the web API to accept stdio configuration.
    - Added HTTP/stdio controls to the web MCP settings UI.
    - Added stdio support to lunarwing registry list/info/install, including --force handling.
    - Exposed transport and command metadata when listing installed MCP servers.
    - Treated stdio servers as requiring no OAuth authentication.
    - Added validation for empty commands, NUL bytes, and invalid environment names/values.
    - Ensured removal stops the managed child process before deleting its configuration.
    - Preserved existing HTTP MCP and registry precedence behavior.
    - Kept installation separate from execution: installation stores configuration; activation starts the process and discovers tools.
    - Updated the architecture proposal, historical supergateway note, and MCP documentation with neutral local-files examples.

    Worker-local execution, automatic npm/pip installation, secret injection through stdio environment variables, and gateway changes were intentionally excluded. All targeted tests, formatting, JavaScript syntax checks, and cargo check passed.

    </details>

    <details>
    <summary><b>Recommended List — Pick ONE, implement it, check off item 18</b></summary>

    Recommended Next:

    1. MCP deactivate/re-enable
       - Stop the child, unregister its tools, and preserve configuration.
       - Persist enabled = false so restart does not relaunch it.
       - Add tool_deactivate, API, and web controls.
       - This completes the lifecycle without involving WASM or workers.

    2. Diagnostics and command preflight
       - Extend doctor/status with transport, enabled state, and executable availability.
       - Validate absolute commands or resolve commands through PATH.
       - Report spawn and negotiation failures in the installed-extension response.
       - Do not execute anything during installation.

    3. Registry validation
       - Add a validation test or registry validate command covering:
         - exactly one of url or transport
         - valid stdio command/args/env
         - auth: none for stdio
         - duplicate names and unsupported transport types
       - This is almost entirely isolated to registry code and CI.

    4. In-place configuration updates
       - Let users edit command, args, env, or URL without remove/reinstall.
       - If active, require explicit restart confirmation.
       - Preserve existing registry precedence and approval rules.

    5. Focused integration coverage
       - Exercise the real install API through the router.
       - Verify install → list → activate failure reporting → deactivate → remove.
       - Add a browser-level check for HTTP/stdio mode switching and mobile layout.

    Useful, Slightly Larger (optional):
    - Optional stdio working directory (cwd) with path validation.
    - Per-server startup and request timeouts.
    - A visible risk label explaining that host-local processes are unsandboxed.
    - Better process cleanup when a spawn replaces an existing managed transport.
    - Tenant/owner selection for mcp add and registry install; today the CLI persistence path still assumes the default owner.

    Defer For Now:
    Worker-local MCP, automatic npm/pip installation, secret injection through process environment, a general runtime-adapter refactor, gateway integration, and automatic crash restart all increase the security or lifecycle surface materially. Recommended: implement deactivate/re-enable, diagnostics, registry validation, and integration tests as one contained follow-up. That provides a complete and inspectable host-local lifecycle before introducing another execution placement.

    </details>

19. [ ] Finish going through all the documents under architecture directory in docs/ and update all outdated documentation. Then, consolidate documents if possible.
20. [ ] Go through all documents under bugs directory in docs/ and update all outdated documentation. Then, consolidate documents.
21. [ ] Go through all documents under proposals directory in docs/ and update all outdated documentation. Then, consolidate documents if deemed necessary.
22. [ ] Go through all documents under reviews directory in docs/ and update all outdated documentation. Then, consolidate documents.
23. [ ] Go through all documents under guides directory in docs/ and update all outdated documentation. Then, consolidate documents.
24. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.0.0 explaining all relevant changes since v1.1.9.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/release for reference as to how to write up this document. The codename for this release is: `Unknown` — The file you write will be RELEASE-v2.0.0.0.md and should be written to the ROOT of the repo.
25. [ ] Improve accuracy of RELEASE-v2.0.0.0.md

---
