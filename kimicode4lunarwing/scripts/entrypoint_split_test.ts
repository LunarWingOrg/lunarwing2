import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs"
import { createServer, type Server } from "node:net"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { after, test } from "node:test"

const scratchDirs: string[] = []
const entrypoint = join(dirname(fileURLToPath(import.meta.url)), "..", "entrypoint-split.sh")

function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), "kimi-split-entrypoint-"))
  scratchDirs.push(dir)
  return dir
}

after(() => {
  for (const dir of scratchDirs) rmSync(dir, { recursive: true, force: true })
})

function makeApp(): { app: string; report: string; setupReport: string } {
  const app = scratch()
  mkdirSync(join(app, "scripts"), { recursive: true })
  const report = join(app, "report.json")
  const setupReport = join(app, "setup-report.txt")
  const reportProgram = `const fs = require("fs");
fs.writeFileSync(process.env.REPORT_FILE, JSON.stringify({
  token: process.env.AGENT_AUTH_TOKEN ?? null,
  tokenFile: process.env.AGENT_AUTH_TOKEN_FILE ?? null,
  taskUid: process.env.KIMI_TASK_UID ?? null,
  expectedTaskUid: process.env.LUNARWING_TASK_UID ?? null,
  remote: process.env.LUNARWING_TASK_RUNNER_REMOTE ?? null,
  llmKey: process.env.KIMI_API_KEY ?? null
}));`
  writeFileSync(join(app, "scripts", "task_runner.js"), reportProgram)
  writeFileSync(join(app, "scripts", "lunarwing_bridge.js"), reportProgram)
  writeFileSync(join(app, "health_server.py"), "import time\ntime.sleep(60)\n")
  writeFileSync(join(app, "entrypoint-task-setup.sh"), `#!/bin/sh
printf '%s' "\${AGENT_AUTH_TOKEN:+present}" > "$SETUP_REPORT"
`)
  chmodSync(join(app, "entrypoint-task-setup.sh"), 0o755)
  return { app, report, setupReport }
}

function roleEnv(app: string, report: string, setupReport: string, role: string): NodeJS.ProcessEnv {
  const uid = process.getuid?.() ?? 1000
  const gid = process.getgid?.() ?? 1000
  return {
    ...process.env,
    LUNARWING_CONTAINER_ROLE: role,
    LUNARWING_APP_DIR: app,
    KIMI_TASK_UID: String(uid),
    KIMI_TASK_GID: String(gid),
    KIMI_BRIDGE_UID: String(uid + 1),
    KIMI_TASK_RUNNER_SOCKET: join(app, "shared", "runner.sock"),
    KIMI_TASK_RUNNER_STATE_FILE: join(app, "shared", "state.json"),
    LUNARWING_TOOL_PROXY_DIR: join(app, "shared", "proxy"),
    WORKSPACE_ROOT: app,
    REPORT_FILE: report,
    SETUP_REPORT: setupReport,
    HOME: join(app, "home"),
    TENSORZERO_API_KEY: "test-llm-key",
  }
}

test("split-task refuses the hub credential before setup", () => {
  const { app, report, setupReport } = makeApp()
  const env = roleEnv(app, report, setupReport, "split-task")
  env.AGENT_AUTH_TOKEN = "must-not-reach-task"
  const result = spawnSync(entrypoint, [], { env, encoding: "utf8", timeout: 10_000 })
  assert.equal(result.status, 1)
  assert.match(result.stderr, /hub credential variables must never reach the task container/)
  assert.equal(existsSync(report), false)
  assert.equal(existsSync(setupReport), false)
})

test("split-task runs setup and the runner without bridge identity or credential", () => {
  const { app, report, setupReport } = makeApp()
  const env = roleEnv(app, report, setupReport, "split-task")
  delete env.AGENT_AUTH_TOKEN
  delete env.AGENT_AUTH_TOKEN_FILE
  const result = spawnSync(entrypoint, [], { env, encoding: "utf8", timeout: 10_000 })
  assert.equal(result.status, 0, result.stderr)
  const reportValue = JSON.parse(readFileSync(report, "utf8")) as Record<string, unknown>
  assert.equal(reportValue.token, null)
  assert.equal(reportValue.tokenFile, null)
  assert.equal(reportValue.taskUid, null)
  assert.equal(reportValue.remote, "1")
  assert.equal(reportValue.llmKey, "test-llm-key")
  assert.equal(readFileSync(setupReport, "utf8"), "")
})

async function listen(path: string): Promise<Server> {
  mkdirSync(dirname(path), { recursive: true })
  const server = createServer(() => {})
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject)
    server.listen(path, () => resolve())
  })
  return server
}

test("split-bridge moves the token to its private file and exports only the expected task identity", async () => {
  const { app, report, setupReport } = makeApp()
  const env = roleEnv(app, report, setupReport, "split-bridge")
  env.KIMI_BRIDGE_UID = String(process.getuid?.() ?? 1000)
  env.KIMI_TASK_UID = String((process.getuid?.() ?? 1000) + 1)
  env.AGENT_AUTH_TOKEN = "bridge-test-token"
  mkdirSync(env.HOME as string, { recursive: true })
  const server = await listen(env.KIMI_TASK_RUNNER_SOCKET as string)
  try {
    const result = spawnSync(entrypoint, [], { env, encoding: "utf8", timeout: 10_000 })
    assert.equal(result.status, 0, result.stderr)
    const reportValue = JSON.parse(readFileSync(report, "utf8")) as Record<string, unknown>
    assert.equal(reportValue.token, null)
    assert.equal(reportValue.taskUid, null)
    assert.equal(reportValue.expectedTaskUid, env.KIMI_TASK_UID)
    assert.equal(reportValue.remote, "1")
    const tokenFile = String(reportValue.tokenFile)
    assert.ok(tokenFile.startsWith(`${env.HOME}/`), "token must stay in bridge-private state")
    assert.equal(readFileSync(tokenFile, "utf8"), "bridge-test-token")
    assert.equal(statSync(tokenFile).mode & 0o777, 0o400)
  } finally {
    server.close()
  }
})
