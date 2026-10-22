import assert from "node:assert/strict"
import { after, test } from "node:test"
import { connect, type Socket } from "node:net"
import { mkdtempSync, rmSync, symlinkSync, writeFileSync, chmodSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { TaskRunner } from "./task_runner.js"
import {
  bridgeRunnerSocketPath,
  cancelRunnerTask,
  delegateTaskToRunner,
  runnerSocketLabel,
} from "./task_runner_client.js"
import type { TaskRequest, TaskProgress, TaskResult } from "./lunarwing_runtime.js"

const scratchDirs: string[] = []
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), "kimi-runner-client-"))
  scratchDirs.push(dir)
  return dir
}
after(() => {
  for (const dir of scratchDirs) rmSync(dir, { recursive: true, force: true })
})

function makeMockKimi(content: string): string {
  const dir = scratch()
  const binPath = join(dir, "mock-kimi")
  writeFileSync(binPath, `#!/usr/bin/env node\n${content}\n`)
  chmodSync(binPath, 0o755)
  return binPath
}

function taskRequest(prompt: string, overrides: Partial<TaskRequest> = {}): TaskRequest {
  return { task_id: "t-1", prompt, ...overrides }
}

async function startRunner(socketDir: string, workspaceRoot?: string): Promise<{ runner: TaskRunner; path: string }> {
  const runner = new TaskRunner(workspaceRoot ?? scratch())
  const path = await runner.listen(join(socketDir, "runner.sock"))
  assert.ok(path, "runner listen failed")
  return { runner, path }
}

test("bridgeRunnerSocketPath is env-driven and empty by default", () => {
  const previous = process.env.LUNARWING_TASK_RUNNER_SOCKET
  try {
    delete process.env.LUNARWING_TASK_RUNNER_SOCKET
    assert.equal(bridgeRunnerSocketPath(), "")
    process.env.LUNARWING_TASK_RUNNER_SOCKET = "/tmp/delegate.sock"
    assert.equal(bridgeRunnerSocketPath(), "/tmp/delegate.sock")
  } finally {
    if (previous === undefined) delete process.env.LUNARWING_TASK_RUNNER_SOCKET
    else process.env.LUNARWING_TASK_RUNNER_SOCKET = previous
  }
})

test("delegateTaskToRunner streams progress and resolves with the result (full path: real client to real runner)", async () => {
  process.env.KIMI_BIN = makeMockKimi(
    `const messages = [
      { role: "assistant", content: "Working..." },
      { role: "assistant", content: "Done. Delegated." },
    ]
    for (const m of messages) process.stdout.write(JSON.stringify(m) + "\\n")`,
  )
  try {
    const socketDir = scratch()
    const { runner, path } = await startRunner(socketDir)
    try {
      const progress: TaskProgress[] = []
      const result = await delegateTaskToRunner(path, taskRequest("do it"), undefined, (p) => progress.push(p))
      assert.equal(result.status, "success")
      assert.match(result.output, /Done\. Delegated\./)
      assert.ok(progress.length >= 1, "client never surfaced progress")
      assert.equal(progress[0].task_id, "t-1")
      assert.equal(runner.activeRunCount, 0)
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
  }
})

test("delegateTaskToRunner rejects on a missing socket", async () => {
  const missing = join(scratch(), "missing.sock")
  await assert.rejects(
    delegateTaskToRunner(missing, taskRequest("x"), undefined, () => {}),
    /ENOENT|ECONNREFUSED|connect/i,
  )
})

test("delegateTaskToRunner rejects on a symlinked socket path", async () => {
  const socketDir = scratch()
  const target = scratch()
  const link = join(socketDir, "delegate-link.sock")
  symlinkSync(target, link)
  await assert.rejects(
    delegateTaskToRunner(link, taskRequest("x"), undefined, () => {}),
    /ENOENT|ECONNREFUSED|EACCES|connect/i,
  )
})

test("delegateTaskToRunner surfaces a runner refusal as a rejection", async () => {
  // A fake runner that refuses every run.
  const socketDir = scratch()
  const { runner, path } = await startRunner(socketDir)
  try {
    const fakeDir = scratch()
    const fakePath = join(fakeDir, "fake.sock")
    const { createServer } = await import("node:net")
    const fake = createServer((conn) => {
      conn.on("data", () => {
        conn.write(JSON.stringify({ ok: false, error: "runner offline for maintenance" }) + "\n")
        conn.end()
      })
    })
    await new Promise<void>((resolve) => fake.listen(fakePath, resolve))
    try {
      await assert.rejects(
        delegateTaskToRunner(fakePath, taskRequest("x"), undefined, () => {}),
        /runner offline for maintenance/,
      )
    } finally {
      fake.close()
    }
  } finally {
    runner.close()
  }
})

test("delegateTaskToRunner rejects a malformed result line fail-closed", async () => {
  const { createServer } = await import("node:net")
  const fakeDir = scratch()
  const fakePath = join(fakeDir, "malformed.sock")
  const fake = createServer((conn) => {
    conn.on("data", () => {
      conn.write(JSON.stringify({ op: "result", result: { task_id: "t-1" } }) + "\n")
      conn.end()
    })
  })
  await new Promise<void>((resolve) => fake.listen(fakePath, resolve))
  try {
    await assert.rejects(
      delegateTaskToRunner(fakePath, taskRequest("x"), undefined, () => {}),
      /malformed result/,
    )
  } finally {
    fake.close()
  }
})

test("delegateTaskToRunner aborts cleanly mid-run", async () => {
  process.env.KIMI_BIN = makeMockKimi(
    `process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)`,
  )
  try {
    const socketDir = scratch()
    const { runner, path } = await startRunner(socketDir)
    try {
      const controller = new AbortController()
      const promise = delegateTaskToRunner(
        path,
        taskRequest("hang", { timeout_ms: 60_000 }),
        undefined,
        () => {},
        controller.signal,
      )
      setTimeout(() => controller.abort(), 200)
      await assert.rejects(promise, /cancelled/)
      // The runner side is reaped by its own executor escalation; give it a
      // moment, then close must not hang.
      await new Promise((resolve) => setTimeout(resolve, 200))
      runner.close()
    } catch (err) {
      runner.close()
      throw err
    }
  } finally {
    delete process.env.KIMI_BIN
  }
})

test("cancelRunnerTask never throws on a dead socket", () => {
  const missing = join(scratch(), "gone.sock")
  const controller = new AbortController()
  controller.abort()
  cancelRunnerTask(missing, "t-9")
  // No throw, no unhandled error events (the client destroys the socket).
})

test("runnerSocketLabel names the socket basename", () => {
  assert.equal(runnerSocketLabel("/tmp/dir/runner.sock"), "runner.sock")
})

test("NB-13: a task outlasting the connection deadline is NOT killed by it (accepted clears the handshake timer)", async () => {
  // Review run 195 NB-13 probe shape: with the connection deadline at its
  // 1 s minimum, a 3 s task was rejected at 1 s while the CLI kept running.
  // The fix clears the connection deadline when the runner acks and arms a
  // run deadline of timeout_ms + margin instead — so a 3 s task with a
  // 60 s run bound completes.
  process.env.KIMI_BIN = makeMockKimi(
    `void (async () => { await new Promise((r) => setTimeout(r, 3000)); process.stdout.write(JSON.stringify({ role: "assistant", content: "slow but done" }) + "\\n") })()`,
  )
  process.env.LUNARWING_TASK_RUNNER_TIMEOUT_MS = "1000"
  try {
    const socketDir = scratch()
    const { runner, path } = await startRunner(socketDir)
    try {
      const result = await delegateTaskToRunner(path, taskRequest("slow work", { timeout_ms: 60_000 }), undefined, () => {})
      assert.equal(result.status, "success")
      assert.match(result.output, /slow but done/)
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
    delete process.env.LUNARWING_TASK_RUNNER_TIMEOUT_MS
  }
})

test("NB-13: a 5 s task completes despite the 1 s connection deadline (run bound supersedes it)", async () => {
  // Same deadline posture as the previous pin, but the mock answers at 5 s:
  // with the connection deadline (1 s) still cleared on accept, the run
  // deadline (60 s + margin) governs, and the task succeeds.
  process.env.KIMI_BIN = makeMockKimi(
    `void (async () => { await new Promise((r) => setTimeout(r, 5000)); process.stdout.write(JSON.stringify({ role: "assistant", content: "slower still" }) + "\\n") })()`,
  )
  process.env.LUNARWING_TASK_RUNNER_TIMEOUT_MS = "1000"
  try {
    const socketDir = scratch()
    const { runner, path } = await startRunner(socketDir)
    try {
      const result = await delegateTaskToRunner(path, taskRequest("slow", { timeout_ms: 60_000 }), undefined, () => {})
      assert.equal(result.status, "success")
      assert.match(result.output, /slower still/)
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
    delete process.env.LUNARWING_TASK_RUNNER_TIMEOUT_MS
  }
})

test("NB-15: runner frames with a foreign task_id are dropped, not forwarded", async () => {
  // Review NB-15 probe: a stand-in runner answered task `mine` with a
  // success result for `someone-else` and it was accepted. The fixed
  // client drops foreign frames; a stand-in cannot forge another task's
  // result into the hub.
  const socketDir = scratch()
  // Hand-rolled runner that answers task t-1 with a result for t-OTHER.
  const { createServer } = await import("node:net")
  const sockPath = join(socketDir, "foreign.sock")
  const server = createServer((conn) => {
    let buf = Buffer.alloc(0)
    conn.on("data", (chunk: Buffer) => {
      buf = Buffer.concat([buf, chunk])
      if (!buf.includes(0x0a)) return
      conn.write(JSON.stringify({ ok: true, op: "accepted", task_id: "t-1" }) + "\n")
      conn.write(JSON.stringify({ op: "progress", progress: { task_id: "t-OTHER", delta: "hijack" } }) + "\n")
      conn.write(JSON.stringify({ op: "result", result: { task_id: "t-OTHER", status: "success", output: "forged", duration_ms: 1 } }) + "\n")
      conn.write(JSON.stringify({ op: "result", result: { task_id: "t-1", status: "success", output: "legit", duration_ms: 2 } }) + "\n")
      conn.end()
    })
  })
  await new Promise<void>((resolve) => server.listen(sockPath, resolve))
  try {
    const progress: TaskProgress[] = []
    const result = await delegateTaskToRunner(sockPath, taskRequest("x"), undefined, (p) => progress.push(p))
    assert.equal(result.status, "success")
    assert.equal(result.output, "legit", "forged t-OTHER result must not be the terminal frame")
    assert.equal(progress.length, 0, "foreign progress frames must be dropped")
  } finally {
    server.close()
  }
})

test("NB-15: an unknown result status is rejected fail-closed", async () => {
  const socketDir = scratch()
  const { createServer } = await import("node:net")
  const sockPath = join(socketDir, "badstatus.sock")
  const server = createServer((conn) => {
    let buf = Buffer.alloc(0)
    conn.on("data", (chunk: Buffer) => {
      buf = Buffer.concat([buf, chunk])
      if (!buf.includes(0x0a)) return
      conn.write(JSON.stringify({ ok: true, op: "accepted", task_id: "t-1" }) + "\n")
      conn.write(JSON.stringify({ op: "result", result: { task_id: "t-1", status: "pwned", output: "", duration_ms: 1 } }) + "\n")
      conn.end()
    })
  })
  await new Promise<void>((resolve) => server.listen(sockPath, resolve))
  try {
    await assert.rejects(
      delegateTaskToRunner(sockPath, taskRequest("x"), undefined, () => {}),
      /unknown result status/,
    )
  } finally {
    server.close()
  }
})
