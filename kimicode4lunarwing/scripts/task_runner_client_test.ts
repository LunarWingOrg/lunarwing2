import assert from "node:assert/strict"
import { after, test } from "node:test"
import { spawn } from "node:child_process"
import { connect } from "node:net"
import { chmodSync, chownSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

import { TaskRunner } from "./task_runner.js"
import {
  bridgeRunnerSocketPath,
  delegateTaskToRunner,
  processGroupMembers,
  reapTaskProcessGroup,
  runnerSocketLabel,
  verifyRunnerSocket,
} from "./task_runner_client.js"
import { executeTask } from "./kimi_task_executor.js"
import { createEnvelope, serializeEnvelopeWithinLimit, type TaskRequest, type TaskProgress, type TaskResult } from "./lunarwing_runtime.js"

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

test("delegateTaskToRunner aborts cleanly mid-run: the hub gets the runner's cancelled result", async () => {
  // NL-3: an abort cancels the run on its own connection and resolves with
  // the runner's terminal `cancelled` result (local execution's shape; this
  // also retires NC-11, where a delegated cancel reached the hub as error).
  process.env.KIMI_BIN = makeMockKimi(
    `process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)`,
  )
  process.env.KIMI_KILL_GRACE_MS = "300"
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
      const result = await promise
      assert.equal(result.status, "cancelled")
      assert.equal(runner.activeRunCount, 0)
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
    delete process.env.KIMI_KILL_GRACE_MS
  }
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

test("NC-6: a timed_out result status is accepted (protocol name, not 'timeout')", async () => {
  // The protocol's TaskResult.status is 'success' | 'error' | 'cancelled' | 'timed_out'.
  // The old allowlist used 'timeout' instead of 'timed_out', so every delegated
  // run that hit its task timeout was rejected as 'unknown result status'.
  const socketDir = scratch()
  const { createServer } = await import("node:net")
  const sockPath = join(socketDir, "timedout.sock")
  const server = createServer((conn) => {
    let buf = Buffer.alloc(0)
    conn.on("data", (chunk: Buffer) => {
      buf = Buffer.concat([buf, chunk])
      if (!buf.includes(0x0a)) return
      conn.write(JSON.stringify({ ok: true, op: "accepted", task_id: "t-1" }) + "\n")
      conn.write(JSON.stringify({ op: "result", result: { task_id: "t-1", status: "timed_out", output: "partial work", duration_ms: 1500 } }) + "\n")
      conn.end()
    })
  })
  await new Promise<void>((resolve) => server.listen(sockPath, resolve))
  try {
    const result = await delegateTaskToRunner(sockPath, taskRequest("x", { timeout_ms: 60_000 }), undefined, () => {})
    assert.equal(result.status, "timed_out")
    assert.equal(result.output, "partial work")
  } finally {
    server.close()
  }
})

test("NC-8: a runner that accepts and closes without a result is rejected at once", async () => {
  // Review run 196 NC-8 probe: a stand-in runner sends 'accepted' then drops
  // the connection. Without a close handler, the client waited the full run
  // deadline (task timeout + 60s, potentially hours) before rejecting.
  const socketDir = scratch()
  const { createServer } = await import("node:net")
  const sockPath = join(socketDir, "died.sock")
  const server = createServer((conn) => {
    let buf = Buffer.alloc(0)
    conn.on("data", (chunk: Buffer) => {
      buf = Buffer.concat([buf, chunk])
      if (!buf.includes(0x0a)) return
      conn.write(JSON.stringify({ ok: true, op: "accepted", task_id: "t-1" }) + "\n")
      conn.end()
    })
  })
  await new Promise<void>((resolve) => server.listen(sockPath, resolve))
  try {
    const start = Date.now()
    await assert.rejects(
      delegateTaskToRunner(sockPath, taskRequest("x", { timeout_ms: 60_000 }), undefined, () => {}),
      /runner connection closed without a result/,
    )
    const elapsed = Date.now() - start
    assert.ok(elapsed < 5000, `should reject within seconds, not wait the full deadline (took ${elapsed}ms)`)
  } finally {
    server.close()
  }
})

// ── !1873 run-200 fixes (NL-2, NL-3, NL-7) ───────────────────────────────────

/** True while pid is a live (non-zombie) process. */
function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0)
  } catch {
    return false
  }
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8")
    const state = stat.slice(stat.lastIndexOf(")") + 2).split(" ")[0]
    return state !== "Z" && state !== "X"
  } catch {
    return false
  }
}

async function waitForFile(path: string, timeoutMs = 10_000): Promise<string> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      const text = readFileSync(path, "utf8").trim()
      if (text) return text
    } catch { /* not yet */ }
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  throw new Error(`${path} never appeared`)
}

// A CLI stand-in that ignores SIGTERM, records its pid, and writes a marker
// into the workspace after `markerAfterMs` if it is still alive then.
function hangingCli(pidFile: string, marker: string, markerAfterMs: number): string {
  return makeMockKimi(
    `process.on("SIGTERM", () => {})
    require("fs").writeFileSync(${JSON.stringify(pidFile)}, String(process.pid))
    setTimeout(() => { require("fs").writeFileSync(${JSON.stringify(marker)}, "still editing") }, ${markerAfterMs})
    setInterval(() => {}, 1000)`,
  )
}

for (const shape of ["success", "timed_out", "timed_out-escapes"] as const) {
  test(`NL-2: a ~1 MiB delegated ${shape} result arrives truncated like a local one, not refused`, async () => {
    // Run-200 probe: the client refused any runner line over 1 MiB, but JSON
    // escaping grows a 1 MiB output past that (a timed_out result with 1 MiB
    // of newline-separated output was a 1.57 MB line), and a 1,048,500-byte
    // success output failed the separate 1 MiB output check. Both now reach
    // the hub, truncated by the same envelope rule as local execution.
    const body = shape === "success"
      // One 1,048,500-byte assistant message: its stdout line stays under
      // the CLI's 1 MiB line cap, but the result line around it does not.
      ? `process.stdout.write(JSON.stringify({ role: "assistant", content: "x".repeat(1_048_500) }) + "\\n")`
      // Many short messages: the executor joins them with newlines up to
      // its 1 MiB output cap, then the task times out.
      : shape === "timed_out"
        ? `const line = JSON.stringify({ role: "assistant", content: "x".repeat(1000) + "\\"" }) + "\\n"
         for (let i = 0; i < 1100; i++) process.stdout.write(line)
         setInterval(() => {}, 1000)`
        // 1 MiB of control characters: each becomes a 6-byte JSON escape,
        // so the result line (about 6 MB) is far over the runner line cap
        // and only the runner's own budgeting gets it through.
        : `const line = JSON.stringify({ role: "assistant", content: "\\u0001".repeat(1000) }) + "\\n"
         for (let i = 0; i < 1100; i++) process.stdout.write(line)
         setInterval(() => {}, 1000)`
    process.env.KIMI_BIN = makeMockKimi(body)
    process.env.KIMI_KILL_GRACE_MS = "200"
    const timeoutMs = shape === "success" ? 30_000 : 2_000
    try {
      const { runner, path } = await startRunner(scratch())
      try {
        const delegated = await delegateTaskToRunner(path, taskRequest("big", { task_id: `nl2-${shape}`, timeout_ms: timeoutMs }), undefined, () => {})
        const expectedStatus = shape === "success" ? "success" : "timed_out"
        assert.equal(delegated.status, expectedStatus, `${delegated.status}: ${delegated.error}`)
        let local: TaskResult | undefined
        await executeTask(taskRequest("big", { task_id: `nl2-${shape}`, timeout_ms: timeoutMs }), () => {}, (r) => { local = r }, undefined, scratch())
        assert.equal(local?.status, expectedStatus)
        // Same hub frame in both modes (the bridge's sendEnvelope rule).
        // duration_ms differs between the two runs; pin it so only the output
        // decides the truncation point.
        const hub = (result: TaskResult) => (JSON.parse(serializeEnvelopeWithinLimit(
          createEnvelope("task_result", { ...result, duration_ms: 0 } as unknown as Record<string, unknown>), 1_048_576,
        )).payload as TaskResult).output
        const hubDelegated = hub(delegated)
        assert.ok(hubDelegated.length > (shape === "timed_out-escapes" ? 150_000 : 500_000), `hub output unexpectedly small: ${hubDelegated.length}`)
        assert.equal(hubDelegated, hub(local!), "delegated and local results must reach the hub identically")
      } finally {
        runner.close()
      }
    } finally {
      delete process.env.KIMI_BIN
      delete process.env.KIMI_KILL_GRACE_MS
    }
  })
}

test("NL-3: a runner killed mid-task does not settle the run while its CLI still runs", async () => {
  // Run-200 probe: SIGKILL the runner process mid-task; the client rejected
  // at once and the bridge freed the workspace lock while the detached CLI
  // kept editing (its marker landed 8 s later, past its own 3 s timeout).
  // The fixed client reaps the CLI's process group before it settles.
  const work = scratch()
  const pidFile = join(work, "cli.pid")
  const marker = join(work, "marker")
  const kimi = hangingCli(pidFile, marker, 4_000)
  const socketPath = join(scratch(), "runner.sock")
  const runnerScript = fileURLToPath(new URL("./task_runner.js", import.meta.url))
  const runnerProc = spawn(process.execPath, [runnerScript, socketPath], {
    env: { ...process.env, KIMI_BIN: kimi, WORKSPACE_ROOT: work, LUNARWING_TASK_RUNNER_STATE_FILE: join(work, "state.json") },
    stdio: "ignore",
  })
  try {
    const deadline = Date.now() + 10_000
    while (!existsSync(socketPath) && Date.now() < deadline) await new Promise((r) => setTimeout(r, 50))
    const promise = delegateTaskToRunner(socketPath, taskRequest("edit", { task_id: "nl3-kill", timeout_ms: 3_000 }), undefined, () => {})
    const cliPid = Number(await waitForFile(pidFile))
    assert.ok(processAlive(cliPid))
    runnerProc.kill("SIGKILL")
    const started = Date.now()
    await assert.rejects(promise, /runner connection closed without a result|ECONNRESET|EPIPE/)
    assert.ok(Date.now() - started < 8_000, "the reap must be prompt")
    assert.equal(processAlive(cliPid), false, "the CLI must be dead before the run settles (NL-3)")
    await new Promise((resolve) => setTimeout(resolve, 4_500))
    assert.ok(!existsSync(marker), "the orphaned CLI kept editing the workspace after the run settled")
  } finally {
    runnerProc.kill("SIGKILL")
    try { process.kill(Number(readFileSync(pidFile, "utf8")), "SIGKILL") } catch { /* gone */ }
  }
})

test("NL-3: a delegated cancel settles only after the CLI is gone", async () => {
  // Run-200 probe: local mode settled a cancel when the CLI exited (8.0 s
  // with the default grace), delegated mode at 1.0 s with the CLI alive.
  const work = scratch()
  const pidFile = join(work, "cli.pid")
  const marker = join(work, "marker")
  process.env.KIMI_BIN = hangingCli(pidFile, marker, 60_000)
  process.env.KIMI_KILL_GRACE_MS = "1500"
  try {
    const { runner, path } = await startRunner(scratch(), work)
    try {
      const controller = new AbortController()
      const promise = delegateTaskToRunner(path, taskRequest("edit", { task_id: "nl3-cancel", timeout_ms: 60_000 }), undefined, () => {}, controller.signal)
      const cliPid = Number(await waitForFile(pidFile))
      const started = Date.now()
      controller.abort()
      const result = await promise
      const elapsed = Date.now() - started
      assert.equal(result.status, "cancelled")
      assert.equal(processAlive(cliPid), false, "the CLI must be dead when the cancel settles (NL-3)")
      assert.ok(elapsed >= 1_000, `settled after ${elapsed} ms, before the 1.5 s kill grace could have stopped the CLI`)
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
    delete process.env.KIMI_KILL_GRACE_MS
  }
})

test("NL-3: the reap signals only groups whose members all belong to the task uid", async () => {
  // A child in its own process group, owned by this test's uid.
  const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { detached: true, stdio: "ignore" })
  try {
    const pgid = child.pid!
    await new Promise((resolve) => setTimeout(resolve, 100))
    const members = processGroupMembers(pgid)
    assert.ok(members && members.length === 1 && members[0].uid === process.geteuid?.())
    // A uid that does not match refuses (the root bridge must never signal
    // a group it cannot prove is the task's).
    assert.equal(await reapTaskProcessGroup(pgid, { allowedUid: (process.geteuid?.() ?? 0) + 1 }), "refused")
    assert.ok(processAlive(pgid), "a refused reap must not signal")
    for (const bad of [0, 1, -5, process.pid, 1.5]) {
      assert.equal(await reapTaskProcessGroup(bad), "refused", String(bad))
    }
    assert.equal(await reapTaskProcessGroup(pgid), "killed")
    assert.equal(processAlive(pgid), false)
    assert.equal(await reapTaskProcessGroup(pgid), "gone")
  } finally {
    child.kill("SIGKILL")
  }
})

// NL-7: the root launcher's bind helper, end to end (unprivileged here; the
// image runs it as root before setpriv). The runner gets only the listening
// descriptor, which never reaches task code.
const bindHelper = fileURLToPath(new URL("../task_runner_bind.py", import.meta.url))
const scriptsDir = dirname(fileURLToPath(import.meta.url))

function runBindHelper(socketPath: string, program: string[], env: NodeJS.ProcessEnv = {}) {
  return spawn("python3", ["-I", bindHelper, socketPath, "--", ...program], {
    env: { ...process.env, ...env },
    stdio: ["ignore", "ignore", "pipe"],
  })
}

async function exitOf(proc: ReturnType<typeof spawn>): Promise<{ code: number | null; stderr: string }> {
  let stderr = ""
  proc.stderr?.on("data", (chunk: Buffer) => { stderr += chunk.toString("utf8") })
  return await new Promise((resolve) => proc.once("exit", (code) => resolve({ code, stderr })))
}

test("NL-7: the bind helper hands the runner a listener in an owner-only directory, and task code never inherits it", async () => {
  const base = scratch()
  const socketDir = join(base, "sock")
  const socketPath = join(socketDir, "task-runner.sock")
  const report = join(base, "fds.json")
  const kimi = makeMockKimi(
    `const fs = require("fs")
    const fds = fs.readdirSync("/proc/self/fd").map((fd) => { try { return fs.readlinkSync("/proc/self/fd/" + fd) } catch { return "" } })
    fs.writeFileSync(${JSON.stringify(report)}, JSON.stringify({ fds, listenFdEnv: process.env.LUNARWING_TASK_RUNNER_LISTEN_FD ?? null }))
    process.stdout.write(JSON.stringify({ role: "assistant", content: "fd report written" }) + "\\n")`,
  )
  const runner = runBindHelper(socketPath, [process.execPath, join(scriptsDir, "task_runner.js")], {
    KIMI_BIN: kimi,
    WORKSPACE_ROOT: base,
    LUNARWING_TASK_RUNNER_STATE_FILE: join(base, "state.json"),
    LUNARWING_TASK_RUNNER_SOCKET: "",
  })
  try {
    const deadline = Date.now() + 10_000
    while (!existsSync(join(base, "state.json")) && Date.now() < deadline) await new Promise((r) => setTimeout(r, 50))
    assert.ok(existsSync(join(base, "state.json")), "runner never came up on the inherited fd")
    // The socket directory is owner-only and the runner is the exec'd pid.
    assert.equal(statSync(socketDir).mode & 0o777, 0o700)
    assert.match(readlinkSync(`/proc/${runner.pid}/exe`), /node/, "the helper must exec the runner in place (NL-4)")
    const state = JSON.parse(readFileSync(join(base, "state.json"), "utf8")) as Record<string, unknown>
    assert.deepEqual(Object.keys(state).sort(), ["heartbeat_at", "ready"], "the state file carries liveness only (NL-8)")
    const result = await delegateTaskToRunner(socketPath, taskRequest("report fds", { task_id: "nl7-fd", timeout_ms: 10_000 }), undefined, () => {})
    assert.equal(result.status, "success", result.error ?? "")
    const runnerFd3 = readlinkSync(`/proc/${runner.pid}/fd/3`)
    assert.match(runnerFd3, /^socket:/)
    const seen = JSON.parse(readFileSync(report, "utf8")) as { fds: string[]; listenFdEnv: string | null }
    assert.ok(!seen.fds.includes(runnerFd3), `task code inherited the runner's listening socket ${runnerFd3}: ${seen.fds.join(",")}`)
    assert.equal(seen.listenFdEnv, null)
  } finally {
    runner.kill("SIGKILL")
  }
})

test("NL-7: the bind helper refuses unsafe directories, non-socket paths and relative programs", async () => {
  const base = scratch()
  const open = join(base, "open")
  mkdirSync(open, { mode: 0o755 })
  chmodSync(open, 0o755)
  let out = await exitOf(runBindHelper(join(open, "s.sock"), ["/bin/true"]))
  assert.equal(out.code, 1)
  assert.match(out.stderr, /must not be accessible to group or others/)

  const link = join(base, "link")
  const target = join(base, "target")
  mkdirSync(target, { mode: 0o700 })
  symlinkSync(target, link)
  out = await exitOf(runBindHelper(join(link, "s.sock"), ["/bin/true"]))
  assert.equal(out.code, 1)
  assert.match(out.stderr, /not a directory/)

  const dir = join(base, "dir")
  mkdirSync(dir, { mode: 0o700 })
  chmodSync(dir, 0o700)
  writeFileSync(join(dir, "s.sock"), "not a socket")
  out = await exitOf(runBindHelper(join(dir, "s.sock"), ["/bin/true"]))
  assert.equal(out.code, 1)
  assert.match(out.stderr, /exists and is not a socket/)

  out = await exitOf(runBindHelper(join(dir, "t.sock"), ["true"]))
  assert.equal(out.code, 1)
  assert.match(out.stderr, /must be an absolute path/)

  if (process.geteuid?.() === 0) {
    const foreign = join(base, "foreign")
    mkdirSync(foreign, { mode: 0o700 })
    chownSync(foreign, 65534, 65534)
    out = await exitOf(runBindHelper(join(foreign, "s.sock"), ["/bin/true"]))
    assert.equal(out.code, 1)
    assert.match(out.stderr, /owned by uid 65534/)
  }
})

test("NL-7: a root bridge accepts only a root-owned runner socket in a root-owned, non-writable directory", { skip: process.geteuid?.() !== 0 ? "root-only check (CI runs this suite as root)" : false }, async () => {
  const base = scratch()
  const dir = join(base, "sock")
  mkdirSync(dir, { mode: 0o700 })
  const { createServer } = await import("node:net")
  const socketPath = join(dir, "runner.sock")
  const server = createServer(() => {})
  await new Promise<void>((resolve) => server.listen(socketPath, resolve))
  try {
    verifyRunnerSocket(socketPath)
    // A task-owned socket (the old shape) is refused even with the task
    // identity pinned.
    process.env.KIMI_TASK_UID = "10002"
    process.env.KIMI_TASK_GID = "10002"
    chownSync(socketPath, 10002, 10002)
    assert.throws(() => verifyRunnerSocket(socketPath), /expected root/)
    chownSync(socketPath, 0, 0)
    chmodSync(dir, 0o777)
    assert.throws(() => verifyRunnerSocket(socketPath), /root-owned directory that group and others cannot write/)
    chmodSync(dir, 0o700)
    chownSync(dir, 10002, 10002)
    assert.throws(() => verifyRunnerSocket(socketPath), /root-owned directory/)
  } finally {
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    server.close()
  }
})
