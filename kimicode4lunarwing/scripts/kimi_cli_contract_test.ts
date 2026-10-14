import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"

/**
 * Real-CLI flag contract tests (WI#195 finding 2).
 *
 * Every other executor test points KIMI_BIN at a throwaway mock, so the CLI's
 * flag surface was pinned only in a comment. These tests execute the REAL
 * installed kimi binary and assert the contract the worker shells out to:
 *
 *   1. the headless invocation `-p <prompt> --output-format stream-json` is
 *      usage-valid: stream-json mode engages, the seeded model resolves, and
 *      the run reaches the provider layer — never a usage rejection
 *      (exit 64 / exit 2 / "unknown option"), and
 *   2. `--yolo` is rejected alongside `-p` (kimi-code 2.0.x+ enforces this
 *      explicitly: "error: Cannot combine --prompt with --yolo."), which is
 *      why kimi_task_executor.ts relies on config-only auto-approval, and
 *   3. (WI#324) the CLI loads `$KIMI_CODE_HOME/mcp.json`, spawns the stdio
 *      MCP server with the parent env passed through, and completes
 *      initialize -> tools/list over newline-delimited JSON-RPC — the
 *      contract the lunarwing tool-proxy shim relies on.
 *
 * Determinism (the !1072 build failure): model/config resolution happens
 * BEFORE any provider contact, so an unconfigured environment fails with
 * "No model configured" and would make test 1 trip in the clean image build
 * while passing on hosts that happen to have a kimi config — the wrong way
 * around. Both tests therefore run under a seeded temp HOME containing a
 * minimal worker-shaped config.toml (default_model + provider + max_context_size,
 * mirroring the baked config/config.toml layout). The provider base_url in
 * the seed is a guaranteed-unreachable loopback endpoint so the image build
 * never contacts the real gateway.
 *
 * kimi-code 2.x does NOT exit 0 on provider failure: it retries connection
 * errors up to 10 times with exponential backoff (stream-json
 * `turn.step.retrying` meta events) before giving up. Test 1 therefore
 * terminates the child as soon as the first provider-layer retry event is
 * observed and asserts on observed events, not on a completed exit:
 * reaching the retry loop proves flags parsed, the model resolved, and the
 * provider was contacted — everything the contract needs, in a few seconds
 * instead of the full ~60s+ retry exhaustion.
 *
 * Self-skip: when no real kimi binary is on PATH (host dev machines) all
 * four tests skip. In the worker image build (`npm test` in the Dockerfile) the
 * gate asserts the contract for real — `bin/kimi` is on PATH there; if the
 * build layout ever changes, KIMI_BIN can point at the installed CLI
 * explicitly.
 */

interface ParsedResult {
  code: number | null
  signal: string | null
  stdout: string
  stderr: string
}

function runCli(bin: string, args: string[], env: NodeJS.ProcessEnv): Promise<ParsedResult> {
  return new Promise((resolve, reject) => {
    const proc = spawn(bin, args, { env, stdio: ["ignore", "pipe", "pipe"] })
    let stdout = ""
    let stderr = ""
    proc.stdout.on("data", (chunk: Buffer) => { stdout += chunk.toString("utf8") })
    proc.stderr.on("data", (chunk: Buffer) => { stderr += chunk.toString("utf8") })
    proc.on("error", reject)
    proc.on("close", (code, signal) => resolve({ code, signal, stdout, stderr }))
  })
}

interface ContractObservation extends ParsedResult {
  /** stream-json mode engaged (system.version meta event on stdout). */
  sawStreamJsonMeta: boolean
  /** Provider layer reached (a connection-retry meta event was emitted). */
  sawProviderLayer: boolean
}

function runHeadlessContract(bin: string, env: NodeJS.ProcessEnv): Promise<ContractObservation> {
  return new Promise((resolve, reject) => {
    const proc = spawn(
      bin,
      ["-p", "say hi", "--output-format", "stream-json"],
      { env, stdio: ["ignore", "pipe", "pipe"] },
    )
    let stdout = ""
    let stderr = ""
    let sawStreamJsonMeta = false
    let sawProviderLayer = false
    let settled = false
    const finish = (code: number | null, signal: string | null) => {
      if (settled) return
      settled = true
      clearTimeout(watchdog)
      resolve({ code, signal, stdout, stderr, sawStreamJsonMeta, sawProviderLayer })
    }
    // Safety net: if the CLI ever stops responding to SIGTERM, SIGKILL so the
    // gate cannot hang the image build. The 60s test timeout is the hard backstop.
    const watchdog = setTimeout(() => { if (!settled) proc.kill("SIGKILL") }, 45_000)
    proc.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString("utf8")
      if (stdout.includes('"type":"system.version"')) sawStreamJsonMeta = true
      if (
        stdout.includes('"type":"turn.step.retrying"') ||
        stdout.includes("APIConnectionError")
      ) {
        sawProviderLayer = true
        // Contract satisfied as far as it can be against an unreachable
        // provider — stop before the full retry exhaustion (~60s+).
        if (!settled) proc.kill("SIGTERM")
      }
    })
    proc.stderr.on("data", (chunk: Buffer) => { stderr += chunk.toString("utf8") })
    proc.on("error", reject)
    proc.on("close", (code, signal) => finish(code, signal))
  })
}

async function usable(bin: string): Promise<boolean> {
  try {
    const run = await runCli(bin, ["--version"], process.env)
    return run.code === 0
  } catch {
    return false
  }
}

async function probeRealKimi(): Promise<string | undefined> {
  const candidates = process.env.KIMI_BIN ? [process.env.KIMI_BIN] : ["bin/kimi", "kimi"]
  for (const candidate of candidates) {
    if (await usable(candidate)) return candidate
  }
  return undefined
}

const realKimi = await probeRealKimi()
const contractOpts: { skip: boolean; timeout: number } = { skip: realKimi === undefined, timeout: 60_000 }
const realKimiBin = realKimi ?? "kimi"

// Seeded temp HOME: minimal worker-shaped config so the CLI gets past model
// resolution without depending on any host kimi state. The provider endpoint
// is a loopback port (1) that always refuses connections instantly.
const seededHome = mkdtempSync(join(tmpdir(), "kimi-contract-home-"))
mkdirSync(join(seededHome, ".kimi-code"), { recursive: true })
writeFileSync(
  join(seededHome, ".kimi-code", "config.toml"),
  [
    "default_model = \"tensorzero/FrontierCODEK\"",
    "telemetry = false",
    "",
    "[providers.tensorzero]",
    "type = \"openai\"",
    "base_url = \"http://127.0.0.1:1/openai/v1\"",
    "api_key = \"test\"",
    "",
    "[models.\"tensorzero/FrontierCODEK\"]",
    "provider = \"tensorzero\"",
    "model = \"tensorzero::function_name::FrontierCODEK\"",
    "max_context_size = 859000",
    "capabilities = [\"tool_use\"]",
    "",
  ].join("\n"),
  { mode: 0o600 },
)

function contractEnv(): NodeJS.ProcessEnv {
  return {
    ...process.env,
    HOME: seededHome,
    // The CLI reads config from $KIMI_CODE_HOME/config.toml in the worker
    // layout; setting both keeps the seed authoritative either way.
    KIMI_CODE_HOME: join(seededHome, ".kimi-code"),
    // WI#206: the guard env is scrubbed per task in production; the contract
    // lane must not re-disable it. The mode reminder stays off (prompt noise
    // for a fixed-mode worker).
    KIMI_CODE_PERMISSION_MODE_REMINDER: process.env.KIMI_CODE_PERMISSION_MODE_REMINDER ?? "0",
    KIMI_CODE_NO_AUTO_UPDATE: "1",
    KIMI_DISABLE_TELEMETRY: "1",
  }
}

function usageRejection(run: ParsedResult): boolean {
  return run.code === 64 || run.code === 2 || /unknown option|unexpected argument/i.test(run.stderr)
}

test(
  "real CLI accepts the headless worker invocation (-p --output-format stream-json)",
  contractOpts,
  async () => {
    const run = await runHeadlessContract(realKimiBin, contractEnv())
    const detail =
      `exit=${run.code} signal=${run.signal ?? "none"} ` +
      `stderr: ${run.stderr.trim().slice(0, 400) || "<empty>"}`
    assert.ok(
      !usageRejection(run),
      `CLI rejected the headless flag contract (usage error). ${detail}`,
    )
    assert.ok(
      !/No model configured/.test(run.stderr),
      `seeded config failed to satisfy model resolution. ${detail}`,
    )
    assert.ok(
      run.sawStreamJsonMeta,
      `stream-json mode must engage (system.version meta event). ${detail}`,
    )
    assert.ok(
      run.sawProviderLayer,
      `invocation must reach the provider layer (model resolved + connection ` +
        `attempted); no connection-retry event observed. ${detail}`,
    )
  },
)

test(
  "real CLI rejects --yolo alongside -p (pins the config-only auto-approval rationale)",
  contractOpts,
  async () => {
    const run = await runCli(realKimiBin, ["-p", "say hi", "--yolo"], contractEnv())
    assert.notEqual(run.code, 0, "--yolo should NOT be accepted in print mode")
    assert.match(
      run.stderr,
      /--yolo/,
      `rejection should name --yolo; stderr: ${run.stderr.trim().slice(0, 400)}`,
    )
  },
)

// WI#206: the baked [[hooks]] PreToolUse guard table must be valid for the
// pinned CLI. Two probes, both provider-free:
//   1. `kimi doctor config` on a seeded config that includes the exact
//      [[hooks]] entry the worker bakes — doctor validates config.toml shape
//      and exits 1 on an invalid file.
//   2. a headless `-p` startup run with that config must not print an
//      "Ignored invalid config section" warning (the way an unknown or
//      malformed hooks table would be rejected at runtime).
// End-to-end denial behavior (hook blocks a dangerous tool call in print
// mode) was verified against kimi-code 2.0.2 with a live mock provider during WI#206
// investigation; the image build has no provider, so it is pinned here at
// the config-contract level instead.
test(
  "real CLI accepts the baked PreToolUse guard hook config (WI#206)",
  contractOpts,
  async () => {
    const guardHome = mkdtempSync(join(tmpdir(), "kimi-guard-home-"))
    mkdirSync(join(guardHome, ".kimi-code"), { recursive: true })
    const guardHookPath = fileURLToPath(new URL("../hooks/headless_command_guard.mjs", import.meta.url))
    const guardConfigPath = join(guardHome, ".kimi-code", "config.toml")
    writeFileSync(
      guardConfigPath,
      [
        'default_model = "tensorzero/FrontierCODEK"',
        'default_permission_mode = "auto"',
        "telemetry = false",
        "",
        "[providers.tensorzero]",
        'type = "openai"',
        'base_url = "http://127.0.0.1:1/openai/v1"',
        'api_key = "test"',
        "",
        '[models."tensorzero/FrontierCODEK"]',
        'provider = "tensorzero"',
        'model = "tensorzero::function_name::FrontierCODEK"',
        "max_context_size = 859000",
        'capabilities = ["tool_use"]',
        "",
        "[[hooks]]",
        'event = "PreToolUse"',
        'matcher = "Bash"',
        `command = "node ${guardHookPath}"`,
        "timeout = 10",
        "",
      ].join("\n"),
      { mode: 0o600 },
    )
    const doctor = await runCli(
      realKimiBin,
      ["doctor", "config", guardConfigPath],
      { ...contractEnv(), KIMI_CODE_HOME: join(guardHome, ".kimi-code"), HOME: guardHome },
    )
    assert.equal(
      doctor.code,
      0,
      `doctor must accept the guard hook config: ${doctor.stdout} ${doctor.stderr}`.slice(0, 600),
    )

    const probe = await runHeadlessContract(
      realKimiBin,
      {
        ...contractEnv(),
        KIMI_CODE_HOME: join(guardHome, ".kimi-code"),
        HOME: guardHome,
      },
    )
    assert.ok(
      !/Ignored invalid config section/.test(probe.stderr + probe.stdout),
      `hooks table rejected at runtime: ${(probe.stderr || probe.stdout).trim().slice(0, 400)}`,
    )
    assert.ok(
      probe.sawStreamJsonMeta,
      `startup probe must engage stream-json; stderr: ${probe.stderr.trim().slice(0, 300)}`,
    )
  },
)

// WI#324 EF-5 (MR !1549): pin the real-CLI MCP contract the worker bridge
// relies on — in print mode the CLI must read $KIMI_CODE_HOME/mcp.json,
// spawn the stdio server, pass the parent env through to the MCP child, and
// complete initialize -> notifications/initialized -> tools/list over
// newline-delimited JSON-RPC. A recorder shim (written inline, baking in the
// record path so the assertion does not itself depend on env pass-through)
// logs the handshake and the proxied env vars.
test(
  "real CLI spawns mcp.json servers with env pass-through and runs the MCP handshake (WI#324)",
  contractOpts,
  async () => {
    const mcpHome = mkdtempSync(join(tmpdir(), "kimi-mcp-home-"))
    const kimiHome = join(mcpHome, ".kimi-code")
    mkdirSync(kimiHome, { recursive: true })
    const recordPath = join(mcpHome, "mcp-record.jsonl")
    const shimPath = join(mcpHome, "recorder-shim.js")
    // The CLI must get past model resolution (dead provider, no LLM spend)
    // or it exits before loading mcp.json.
    writeFileSync(
      join(kimiHome, "config.toml"),
      [
        "default_model = \"tensorzero/FrontierCODEK\"",
        "telemetry = false",
        "",
        "[providers.tensorzero]",
        "type = \"openai\"",
        "base_url = \"http://127.0.0.1:1/openai/v1\"",
        "api_key = \"test\"",
        "",
        "[models.\"tensorzero/FrontierCODEK\"]",
        "provider = \"tensorzero\"",
        "model = \"tensorzero::function_name::FrontierCODEK\"",
        "max_context_size = 859000",
        "capabilities = [\"tool_use\"]",
        "",
      ].join("\n"),
      { mode: 0o600 },
    )
    writeFileSync(
      shimPath,
      `#!/usr/bin/env node
const fs = require("node:fs")
const RECORD = ${JSON.stringify(recordPath)}
fs.writeFileSync(RECORD, JSON.stringify({
  event: "spawn",
  task_id: process.env.LUNARWING_TASK_ID ?? null,
  socket: process.env.LUNARWING_TOOL_PROXY_SOCKET ?? null,
}) + "\\n")
let buf = ""
process.stdin.on("data", (chunk) => {
  buf += chunk.toString("utf8")
  let i = buf.indexOf("\\n")
  while (i >= 0) {
    const line = buf.slice(0, i)
    buf = buf.slice(i + 1)
    i = buf.indexOf("\\n")
    if (!line.trim()) continue
    let msg
    try { msg = JSON.parse(line) } catch { continue }
    if (msg.method) fs.appendFileSync(RECORD, msg.method + "\\n")
    if (msg.method === "initialize" && msg.id !== undefined) {
      process.stdout.write(JSON.stringify({
        jsonrpc: "2.0", id: msg.id,
        result: {
          protocolVersion: "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "recorder", version: "0" },
        },
      }) + "\\n")
    } else if (msg.method === "tools/list" && msg.id !== undefined) {
      process.stdout.write(JSON.stringify({
        jsonrpc: "2.0", id: msg.id, result: { tools: [] },
      }) + "\\n")
    }
  }
})
`,
      { mode: 0o700 },
    )
    writeFileSync(
      join(kimiHome, "mcp.json"),
      JSON.stringify({ mcpServers: { lunarwing: { command: "node", args: [shimPath] } } }),
      { mode: 0o600 },
    )

    const proc = spawn(
      realKimiBin,
      ["-p", "say hi", "--output-format", "stream-json"],
      {
        env: {
          ...contractEnv(),
          KIMI_CODE_HOME: kimiHome,
          HOME: mcpHome,
          LUNARWING_TASK_ID: "mcp-contract-task",
          LUNARWING_TOOL_PROXY_SOCKET: "/tmp/nonexistent-contract.sock",
        },
        stdio: ["ignore", "pipe", "pipe"],
      },
    )
    let stderr = ""
    proc.stderr.on("data", (chunk: Buffer) => { stderr += chunk.toString("utf8") })
    try {
      const deadline = Date.now() + 30_000
      let record = ""
      while (Date.now() < deadline) {
        try {
          record = readFileSync(recordPath, "utf8")
        } catch {
          // not written yet
        }
        if (record.includes("tools/list")) break
        await new Promise((resolve) => setTimeout(resolve, 250))
      }
      assert.ok(record.length > 0, `recorder shim was never spawned; stderr: ${stderr.trim().slice(0, 400)}`)
      const spawnEvent = JSON.parse(record.split("\n")[0]) as Record<string, unknown>
      assert.equal(
        spawnEvent.task_id,
        "mcp-contract-task",
        "parent env must pass through to the MCP child (LUNARWING_TASK_ID)",
      )
      assert.equal(
        spawnEvent.socket,
        "/tmp/nonexistent-contract.sock",
        "parent env must pass through to the MCP child (LUNARWING_TOOL_PROXY_SOCKET)",
      )
      assert.ok(record.includes("initialize\n"), `MCP initialize missing: ${record}`)
      assert.ok(record.includes("tools/list\n"), `MCP tools/list missing: ${record}`)
    } finally {
      proc.kill("SIGTERM")
      setTimeout(() => proc.kill("SIGKILL"), 5_000).unref()
    }
  },
)

console.log(realKimi ? `kimi CLI contract self-check passed (real binary: ${realKimi})` : "kimi CLI contract self-check skipped (no real kimi binary)")
