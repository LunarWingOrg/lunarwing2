# pebble4lunarwing

WebSocket bridge that integrates the [Pebble](https://github.com/nanogpt-community/pebble) agentic coding harness as a LunarWing external worker.

## Quick Start

### Native

```bash
cargo build --release
PEBBLE_BIN=../pebble/target/release/pebble \
WORKSPACE_ROOT=/tmp/workspace \
./target/release/pebble4lunarwing
```

### Docker

```bash
docker compose build
docker compose up -d pebble-worker
```

The Dockerfile pins upstream Pebble to a reviewed commit and supplies a
reviewed `PEBBLE_LOCK_SHA256` default for `upstream/Cargo.lock`. Plain Docker
builds, both Compose services, mt-admin's Docker/Podman builders, and CI use
that same default without an extra argument. The build rejects a digest
mismatch and compares complete parsed package records against the pinned
upstream lock; only `rustls` and `rustls-webpki` may differ. Package additions,
removals, or source/checksum/dependency edits outside that allowlist fail even
when a caller supplies a matching digest override. Update the reviewed digest
together with any intentional lock change; CI checks they match.

Fast regression checks (no image build):

```bash
# From the repository root, with Python >= 3.11 and PyYAML installed:
python3 .gitlab/scripts/validate_config.py
python3 -m unittest discover -s .gitlab/tests -p 'test_*.py'
bash ic/scripts/tests/test-mt-admin-pebble-build-inputs.sh
```

### CLI mode (one-shot)

```bash
docker compose --profile cli run pebble-cli prompt "hello world"
```

## LunarWing Configuration

Add to `config.toml` under `LUNARWING_BASE_DIR`:

```toml
[[sandbox.external_workers]]
name = "pebble"
url = "ws://localhost:9090/ws/agent"
timeout_ms = 3600000
prompt_context = { max_messages = 12, max_chars_per_message = 2000, max_total_chars = 12000 }
```

The agent logs `External workers configured: pebble` on startup.

## Usage

```
create_job(title: "Fix the tests", description: "Run cargo test and fix failures", mode: "pebble")
```

Add `context_scope: "recent"` to include bounded user/assistant messages from
current conversation. Default `none` sends no history. History is untrusted
reference JSON; task remains authoritative. Metadata, identity, environment,
system prompts, and tool results never enter worker prompt. Configured limits
cannot exceed 50 messages, 10,000 characters per message, or 50,000 total.

External jobs default to `wait=false`; use `job_events <id>` and `job_status <id>`
to monitor progress.

## Environment Variables

See `.env.example` for the full list with descriptions.

## Health Endpoints

- `GET /health` — Worker status, uptime, version
- `GET /ready` — WebSocket readiness and connection count

## Protocol

Speaks the `lunarwing-agent-v1` WebSocket subprotocol (the legacy `ironclaw-agent-v1` alias is no longer accepted). See `../opencode4lunarwing/agent_comm_protocol.json` for the full spec.
