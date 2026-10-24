import assert from "node:assert/strict"
import { after, test } from "node:test"
import { connect, type Socket } from "node:net"
import { existsSync, mkdtempSync, rmSync, symlinkSync, mkdirSync, writeFileSync, chmodSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import {
  TaskRunner,
  inheritedListenFd,
  main,
  presentCredentialKeys,
  refuseSymlinkedRunnerDir,
  startupRefusal,
  taskRunnerSocketPath,
} from "./task_runner.js"
import { TASK_TOOLCHAIN_DIRS } from "./kimi_task_executor.js"
import type { TaskRequest, TaskResult } from "./lunarwing_runtime.js"

const scratchDirs: string[] = []
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), "kimi-runner-"))
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

/** One IPC client connection with newline framing. */
class RunnerClient {
  private buffer = Buffer.alloc(0)
  private readonly pending: Array<(line: Record<string, unknown>) => void> = []
  private readonly lines: Array<Record<string, unknown>> = []
  /** NL-3 {"op":"spawned"} frames, kept out of the reply queue. */
  readonly spawned: Array<Record<string, unknown>> = []
  constructor(private readonly sock: Socket) {
    sock.on("data", (chunk: Buffer) => {
      this.buffer = Buffer.concat([this.buffer, chunk])
      let newline = this.buffer.indexOf(0x0a)
      while (newline >= 0) {
        const line = this.buffer.subarray(0, newline).toString("utf8").trim()
        this.buffer = this.buffer.subarray(newline + 1)
        if (line.length > 0) {
          const parsed = JSON.parse(line) as Record<string, unknown>
          if (parsed.op === "spawned") {
            this.spawned.push(parsed)
            newline = this.buffer.indexOf(0x0a)
            continue
          }
          const waiter = this.pending.shift()
          if (waiter) waiter(parsed)
          else this.lines.push(parsed)
        }
        newline = this.buffer.indexOf(0x0a)
      }
    })
  }
  static connectTo(path: string): Promise<RunnerClient> {
    return new Promise((resolve, reject) => {
      const sock = connect(path)
      sock.once("connect", () => resolve(new RunnerClient(sock)))
      sock.once("error", reject)
    })
  }
  send(message: Record<string, unknown>): void {
    this.sock.write(JSON.stringify(message) + "\n")
  }
  /** Resolve once the server half-closes the connection. */
  waitClose(timeoutMs = 10_000): Promise<void> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("runner close timed out")), timeoutMs)
      this.sock.once("close", () => {
        clearTimeout(timer)
        resolve()
      })
    })
  }
  /** Resolve once the server half-closes the connection. */
  writeRaw(text: string): void {
    this.sock.write(text)
  }
  /** Wait for the next line, drawing on buffered lines first. */
  nextLine(timeoutMs = 10_000): Promise<Record<string, unknown>> {
    const buffered = this.lines.shift()
    if (buffered) return Promise.resolve(buffered)
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("runner reply timed out")), timeoutMs)
      this.pending.push((line) => {
        clearTimeout(timer)
        resolve(line)
      })
    })
  }
  close(): void {
    this.sock.destroy()
  }
}

async function startRunner(workspaceRoot: string, socketDir: string): Promise<{ runner: TaskRunner; path: string }> {
  const runner = new TaskRunner(workspaceRoot)
  const path = await runner.listen(join(socketDir, "runner.sock"))
  assert.ok(path, "runner listen failed")
  return { runner, path }
}

test("task_runner_socket_path env is read", () => {
  const previous = process.env.LUNARWING_TASK_RUNNER_SOCKET
  process.env.LUNARWING_TASK_RUNNER_SOCKET = "/tmp/runner-probe.sock"
  try {
    assert.equal(taskRunnerSocketPath(), "/tmp/runner-probe.sock")
  } finally {
    if (previous === undefined) delete process.env.LUNARWING_TASK_RUNNER_SOCKET
    else process.env.LUNARWING_TASK_RUNNER_SOCKET = previous
  }
  assert.equal(taskRunnerSocketPath(), "")
})

test("refuseSymlinkedRunnerDir refuses a symlink and accepts a missing path", () => {
  const dir = scratch()
  const link = join(dir, "link-dir")
  symlinkSync(dir, link)
  assert.throws(() => refuseSymlinkedRunnerDir(link), /symlink/)
  // ENOENT is fine (the runner creates it)
  refuseSymlinkedRunnerDir(join(dir, "missing"))
  // A plain file is refused
  const plain = join(dir, "plain")
  writeFileSync(plain, "x")
  assert.throws(() => refuseSymlinkedRunnerDir(plain), /not a directory/)
})

test("ping answers pong with the runner uid", async () => {
  const socketDir = scratch()
  const { runner, path } = await startRunner(scratch(), socketDir)
  try {
    const client = await RunnerClient.connectTo(path)
    try {
      client.send({ op: "ping" })
      const reply = await client.nextLine()
      assert.equal(reply.ok, true)
      assert.equal(reply.op, "pong")
      assert.equal(reply.uid, process.getuid?.())
    } finally {
      client.close()
    }
  } finally {
    runner.close()
  }
})

test("run streams progress and exactly one result, then ends the connection", async () => {
  const workspace = scratch()
  const socketDir = scratch()
  process.env.KIMI_BIN = makeMockKimi(
    `const messages = [
      { role: "assistant", content: "Working..." },
      { role: "assistant", content: "Done. Result." },
    ]
    for (const m of messages) process.stdout.write(JSON.stringify(m) + "\\n")`,
  )
  try {
    const { runner, path } = await startRunner(workspace, socketDir)
    try {
      const client = await RunnerClient.connectTo(path)
      try {
        client.send({ op: "run", request: taskRequest("do it") })
        // Collect until the terminal result; the mock emits one progress
        // line per assistant message, so the count is mock-defined. The run
        // is acknowledged first (NB-13: accepted clears the bridge's
        // connection deadline and arms the run deadline).
        const progressLines: Array<Record<string, unknown>> = []
        let resultLine: Record<string, unknown> | undefined
        let acceptedLine = false
        for (let i = 0; i < 8 && resultLine === undefined; i++) {
          const line = await client.nextLine()
          if (line.op === "accepted") acceptedLine = true
          else if (line.op === "progress") progressLines.push(line)
          else if (line.op === "result") resultLine = line
          else assert.fail(`unexpected runner line: ${JSON.stringify(line)}`)
        }
        assert.ok(acceptedLine, "runner never acknowledged the run (NB-13)")
        assert.ok(resultLine !== undefined, "runner never sent the terminal result")
        // NL-3: the CLI's process group is reported for the bridge's reap.
        assert.equal(client.spawned.length, 1, "runner must report the spawned CLI exactly once (NL-3)")
        assert.equal(client.spawned[0].task_id, "t-1")
        assert.ok(Number.isSafeInteger(client.spawned[0].pgid) && (client.spawned[0].pgid as number) >= 2)
        assert.ok(progressLines.length >= 1, "runner never sent progress")
        for (const progress of progressLines) {
          assert.equal((progress.progress as TaskResult).task_id, "t-1")
        }
        const resultPayload = resultLine.result as TaskResult
        assert.equal(resultPayload.status, "success")
        assert.match(resultPayload.output, /Done\. Result\./)
        assert.equal(runner.activeRunCount, 0)
        // The connection half-closes after the terminal result.
        await client.waitClose()
      } finally {
        client.close()
      }
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
  }
})

test("cancel aborts the in-flight run", async () => {
  const workspace = scratch()
  const socketDir = scratch()
  process.env.KIMI_BIN = makeMockKimi(
    `process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)`,
  )
  try {
    const { runner, path } = await startRunner(workspace, socketDir)
    try {
      const client = await RunnerClient.connectTo(path)
      try {
        client.send({ op: "run", request: taskRequest("hang", { timeout_ms: 60_000 }) })
        // The run is acked before it registers (NB-13); consume the ack so
        // the cancel reply isn't misread as the ack.
        const accepted = await client.nextLine()
        assert.equal(accepted.op, "accepted")
        // Wait for the run to be registered (heartbeat/wait would race).
        await new Promise((resolve) => setTimeout(resolve, 300))
        assert.equal(runner.activeRunCount, 1)
        client.send({ op: "cancel", task_id: "t-1" })
        const ack = await client.nextLine()
        assert.equal(ack.ok, true)
        assert.equal(ack.op, "cancelled")
        const result = await client.nextLine()
        assert.equal(result.op, "result")
        assert.equal((result.result as TaskResult).status, "cancelled")
      } finally {
        client.close()
      }
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
  }
})

test("duplicate run and unknown op are refused", async () => {
  const workspace = scratch()
  const socketDir = scratch()
  process.env.KIMI_BIN = makeMockKimi(
    `process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)`,
  )
  try {
    const { runner, path } = await startRunner(workspace, socketDir)
    try {
      const client = await RunnerClient.connectTo(path)
      try {
        client.send({ op: "run", request: taskRequest("one", { task_id: "dup", timeout_ms: 60_000 }) })
        // Consume the NB-13 acceptance ack before the duplicate-refusal
        // reply arrives (both are queued on the same connection).
        const dupAccepted = await client.nextLine()
        assert.equal(dupAccepted.op, "accepted")
        await new Promise((resolve) => setTimeout(resolve, 300))
        client.send({ op: "run", request: taskRequest("two", { task_id: "dup", timeout_ms: 60_000 }) })
        const dup = await client.nextLine()
        assert.equal(dup.ok, false)
        assert.match(dup.error as string, /already in flight/)
        client.send({ op: "bogus" })
        const bogus = await client.nextLine()
        assert.equal(bogus.ok, false)
        assert.match(bogus.error as string, /unknown op/)
      } finally {
        client.close()
      }
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
  }
})

test("invalid JSON and non-object lines are refused", async () => {
  const workspace = scratch()
  const socketDir = scratch()
  const { runner, path } = await startRunner(workspace, socketDir)
  try {
    const client = await RunnerClient.connectTo(path)
    try {
      client.writeRaw("not json\n")
      let reply = await client.nextLine()
      assert.equal(reply.ok, false)
      assert.match(reply.error as string, /not valid JSON/)
      client.writeRaw("42\n")
      reply = await client.nextLine()
      assert.equal(reply.ok, false)
      assert.match(reply.error as string, /object with an op field/)
    } finally {
      client.close()
    }
  } finally {
    runner.close()
  }
})

test("close cancels active runs and removes the socket file", async () => {
  const workspace = scratch()
  const socketDir = scratch()
  process.env.KIMI_BIN = makeMockKimi(
    `process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)`,
  )
  try {
    const { runner, path } = await startRunner(workspace, socketDir)
    const client = await RunnerClient.connectTo(path)
    client.send({ op: "run", request: taskRequest("hang", { timeout_ms: 60_000 }) })
    await new Promise((resolve) => setTimeout(resolve, 300))
    assert.equal(runner.activeRunCount, 1)
    runner.close()
    assert.equal(runner.activeRunCount, 0)
    let refused = false
    const probe = connect(path)
    probe.on("error", (err) => {
      const code = (err as NodeJS.ErrnoException).code
      if (code === "ECONNREFUSED" || code === "ENOENT") refused = true
    })
    await new Promise((resolve) => setTimeout(resolve, 300))
    probe.destroy()
    assert.ok(refused, "socket should stop accepting after close")
    client.close()
  } finally {
    delete process.env.KIMI_BIN
  }
})

test("listen refuses a symlinked socket directory", async () => {
  const workspace = scratch()
  const target = scratch()
  const socketDir = scratch()
  const link = join(socketDir, "link")
  symlinkSync(target, link)
  const runner = new TaskRunner(workspace)
  const path = await runner.listen(join(link, "runner.sock"))
  assert.equal(path, null, "listen must fail on a symlinked directory")
  runner.close()
})

test("listen replaces a stale socket file", async () => {
  const workspace = scratch()
  const socketDir = scratch()
  mkdirSync(socketDir, { recursive: true })
  writeFileSync(join(socketDir, "runner.sock"), "stale")
  const runner = new TaskRunner(workspace)
  const path = await runner.listen(join(socketDir, "runner.sock"))
  assert.ok(path, "listen must replace the stale socket")
  runner.close()
})

test("NB-13: closing the bridge connection aborts the in-flight run", async () => {
  // Review NB-13 probe: after a deadline rejection, the abandoned run kept
  // editing the workspace. The runner must abort the run when its bridge
  // connection closes, so the workspace lock frees with the error result.
  const workspace = scratch()
  const socketDir = scratch()
  process.env.KIMI_BIN = makeMockKimi(
    `process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)`,
  )
  try {
    const { runner, path } = await startRunner(workspace, socketDir)
    const client = await RunnerClient.connectTo(path)
    client.send({ op: "run", request: taskRequest("hang", { timeout_ms: 60_000 }) })
    const accepted = await client.nextLine()
    assert.equal(accepted.op, "accepted")
    await new Promise((resolve) => setTimeout(resolve, 300))
    assert.equal(runner.activeRunCount, 1)
    // Vanish the bridge connection.
    client.close()
    // The close handler aborts the run; give the abort a beat to land.
    await new Promise((resolve) => setTimeout(resolve, 500))
    assert.equal(runner.activeRunCount, 0, "run must be aborted when the bridge connection closes (NB-13)")
    runner.close()
  } finally {
    delete process.env.KIMI_BIN
  }
})

test("NB-14: a delegated task executes in its project directory, not the workspace root", async () => {
  const workspace = scratch()
  const projectDir = join(workspace, "proj")
  mkdirSync(projectDir, { recursive: true })
  const socketDir = scratch()
  // The mock writes a marker into its CWD; the executor runs the CLI with
  // cwd = the workspace the runner resolved.
  process.env.KIMI_BIN = makeMockKimi(
    `require("fs").writeFileSync(require("path").join(process.cwd(), "marker.txt"), "cwd-marker")
    process.stdout.write(JSON.stringify({ role: "assistant", content: "marked" }) + "\\n")`,
  )
  try {
    const { runner, path } = await startRunner(workspace, socketDir)
    try {
      const client = await RunnerClient.connectTo(path)
      try {
        const request = taskRequest("mark it")
        ;(request as { context?: { project_dir?: string } }).context = { project_dir: "proj" }
        client.send({ op: "run", request })
        const accepted = await client.nextLine()
        assert.equal(accepted.op, "accepted")
        let resultLine: Record<string, unknown> | undefined
        for (let i = 0; i < 6 && resultLine === undefined; i++) {
          const line = await client.nextLine()
          if (line.op === "result") resultLine = line
        }
        assert.ok(resultLine !== undefined, "runner never sent the result")
        assert.equal((resultLine!.result as TaskResult).status, "success")
        assert.ok(existsSync(join(projectDir, "marker.txt")),
          "task must run in the project dir (NB-14)")
        assert.ok(!existsSync(join(workspace, "marker.txt")),
          "task must NOT run in the workspace root (NB-14)")
      } finally {
        client.close()
      }
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
  }
})

// ── !1873 run-200 fixes (NL-1, NL-8) ─────────────────────────────────────────

async function runOnce(runner: TaskRunner, path: string, request: TaskRequest): Promise<TaskResult> {
  const client = await RunnerClient.connectTo(path)
  try {
    client.send({ op: "run", request })
    for (let i = 0; i < 16; i++) {
      const line = await client.nextLine(20_000)
      if (line.op === "result") return line.result as TaskResult
    }
    throw new Error("runner never sent a result")
  } finally {
    client.close()
  }
}

test("NL-1: the delegated runner never runs a task-plantable bare kimi from its PATH", async () => {
  // Run-200 probe shape: the runner resolved a bare `kimi` through its
  // inherited PATH, whose first entry is the task-owned ~/.cargo/bin, so one
  // task could plant a `kimi` that every later task ran. The plant here is
  // first on the runner's PATH in a world-writable directory: it is skipped
  // whether the suite runs as root (CI) or not, and with no root-owned kimi
  // the task fails closed instead.
  const plantDir = scratch()
  chmodSync(plantDir, 0o777)
  const marker = join(scratch(), "planted-ran")
  writeFileSync(join(plantDir, "kimi-nl1-probe"), `#!/bin/sh\necho planted > ${JSON.stringify(marker)}\nexit 0\n`, { mode: 0o755 })
  chmodSync(join(plantDir, "kimi-nl1-probe"), 0o755)
  const savedPath = process.env.PATH
  process.env.PATH = `${plantDir}:${savedPath}`
  process.env.KIMI_BIN = "kimi-nl1-probe"
  try {
    const { runner, path } = await startRunner(scratch(), scratch())
    try {
      const result = await runOnce(runner, path, taskRequest("probe", { task_id: "nl1-plant", timeout_ms: 10_000 }))
      assert.equal(result.status, "error", JSON.stringify(result))
      assert.match(result.error ?? "", /not found on the bridge PATH/)
      assert.ok(!existsSync(marker), "the planted kimi must never run (NL-1)")
    } finally {
      runner.close()
    }
  } finally {
    process.env.PATH = savedPath
    delete process.env.KIMI_BIN
  }
})

test("NL-1: the delegated CLI gets the runner's PATH first and the task toolchain dirs after (WI#462 order)", async () => {
  const workspace = scratch()
  process.env.KIMI_BIN = makeMockKimi(
    `process.stdout.write(JSON.stringify({ role: "assistant", content: JSON.stringify({ path: process.env.PATH, fd: process.env.LUNARWING_TASK_RUNNER_LISTEN_FD ?? null }) }) + "\\n")`,
  )
  process.env.LUNARWING_TASK_RUNNER_LISTEN_FD = "3"
  try {
    const { runner, path } = await startRunner(workspace, scratch())
    try {
      const result = await runOnce(runner, path, taskRequest("print env", { task_id: "nl1-path", timeout_ms: 10_000 }))
      assert.equal(result.status, "success", JSON.stringify(result))
      const seen = JSON.parse(result.output) as { path: string; fd: string | null }
      const expectedTail = `:${TASK_TOOLCHAIN_DIRS.join(":")}`
      assert.equal(seen.path, `${process.env.PATH}${expectedTail}`,
        "the CLI PATH must be the runner's own PATH followed by the task toolchain dirs (NL-1)")
      assert.equal(seen.fd, null, "the runner's listening-fd handoff must not reach task code (NL-7)")
    } finally {
      runner.close()
    }
  } finally {
    delete process.env.KIMI_BIN
    delete process.env.LUNARWING_TASK_RUNNER_LISTEN_FD
  }
})

test("NL-8: the runner request cap follows WS_MAX_PAYLOAD_BYTES", async () => {
  // A 2 MiB hub envelope cap must admit a 1.5 MiB task through the runner
  // (the old fixed 1 MiB cap refused it and dropped the connection).
  const saved = process.env.WS_MAX_PAYLOAD_BYTES
  process.env.WS_MAX_PAYLOAD_BYTES = String(2 * 1_048_576)
  process.env.KIMI_BIN = makeMockKimi(
    `process.stdout.write(JSON.stringify({ role: "assistant", content: "big request ok" }) + "\\n")`,
  )
  try {
    const runner = new TaskRunner(scratch())
    const path = await runner.listen(join(scratch(), "runner.sock"))
    assert.ok(path)
    try {
      assert.equal(runner.lineCapBytes, 2 * 1_048_576 + 65_536)
      // The bulk rides in context metadata (a 1.5 MB argv string would hit
      // the kernel's per-argument limit, which is not what this pins).
      const request = taskRequest("small prompt", { task_id: "nl8-big", timeout_ms: 20_000 })
      ;(request as { context?: Record<string, unknown> }).context = { metadata: { blob: "x".repeat(1_500_000) } }
      const client = await RunnerClient.connectTo(path)
      try {
        client.send({ op: "run", request })
        const first = await client.nextLine(20_000)
        assert.equal(first.op, "accepted", `a 1.5 MB request under a 2 MiB hub cap must be accepted: ${JSON.stringify(first).slice(0, 200)}`)
        let result: Record<string, unknown> | undefined
        for (let i = 0; i < 8 && result === undefined; i++) {
          const line = await client.nextLine(20_000)
          if (line.op === "result") result = line
        }
        assert.equal((result?.result as TaskResult | undefined)?.status, "success")
      } finally {
        client.close()
      }
    } finally {
      runner.close()
    }
  } finally {
    if (saved === undefined) delete process.env.WS_MAX_PAYLOAD_BYTES
    else process.env.WS_MAX_PAYLOAD_BYTES = saved
    delete process.env.KIMI_BIN
  }
})

test("NL-7: LUNARWING_TASK_RUNNER_LISTEN_FD is parsed strictly", () => {
  assert.equal(inheritedListenFd(undefined), null)
  assert.equal(inheritedListenFd(""), null)
  assert.equal(inheritedListenFd("3"), 3)
  assert.equal(inheritedListenFd("17"), 17)
  for (const bad of ["0", "2", "-1", "3x", " 3", "abc"]) {
    assert.throws(() => inheritedListenFd(bad), /descriptor number >= 3/, bad)
  }
})

test("WI#454: the runner's environment contract names both hub credential keys", () => {
  // Empty and missing values are both "absent": the launcher unsets the name,
  // and an empty export is the same non-credential.
  assert.deepEqual(presentCredentialKeys({}), [])
  assert.deepEqual(presentCredentialKeys({ AGENT_AUTH_TOKEN: "", AGENT_AUTH_TOKEN_FILE: "" }), [])
  // Either name carrying a value is a contract violation the runner refuses.
  assert.deepEqual(presentCredentialKeys({ AGENT_AUTH_TOKEN: "hub-bearer" }), ["AGENT_AUTH_TOKEN"])
  assert.deepEqual(
    presentCredentialKeys({ AGENT_AUTH_TOKEN: "hub-bearer", AGENT_AUTH_TOKEN_FILE: "/run/lunarwing/agent_auth_token" }),
    ["AGENT_AUTH_TOKEN", "AGENT_AUTH_TOKEN_FILE"],
  )
  // Nothing else in the environment is the credential.
  assert.deepEqual(presentCredentialKeys({ KIMI_API_KEY: "provider-key", PATH: "/usr/bin" }), [])
})

test("WI#454: main() refuses to start when the hub credential reaches the runner", async () => {
  // The launcher unsets both names before the exec; this pin makes that a
  // refusal instead of a convention. A runner that inherited the token would
  // pass it to every task it spawns, so the daemon must not come up at all.
  // Asserted through startupRefusal() as well, because this suite also runs as
  // root in the image gate, where main()'s uid gate answers first.
  const previousToken = process.env.AGENT_AUTH_TOKEN
  const previousFile = process.env.AGENT_AUTH_TOKEN_FILE
  process.env.AGENT_AUTH_TOKEN = "hub-bearer"
  delete process.env.AGENT_AUTH_TOKEN_FILE
  try {
    const socketPath = join(scratch(), "unused.sock")
    const refusal = startupRefusal({ listenFd: null, socketPath, env: process.env, uid: 10002 })
    assert.match(String(refusal), /hub credential in its environment \(AGENT_AUTH_TOKEN\)/)
    // And in the real entry path, whatever uid this suite runs as the refusal
    // is the credential one (it is checked before the root gate).
    const rc = await main([socketPath])
    if ((process.getuid?.() ?? -1) !== 0) {
      assert.equal(rc, 1, "main() must fail closed rather than listen with the credential present")
    }
    assert.deepEqual(process.exitCode, undefined, "main() reports through its return, never process.exit")
  } finally {
    if (previousToken === undefined) delete process.env.AGENT_AUTH_TOKEN
    else process.env.AGENT_AUTH_TOKEN = previousToken
    if (previousFile === undefined) delete process.env.AGENT_AUTH_TOKEN_FILE
    else process.env.AGENT_AUTH_TOKEN_FILE = previousFile
  }
})

test("WI#454: startupRefusal gates socket, credential, then uid", () => {
  const ok = { listenFd: null, socketPath: "/tmp/x.sock", env: {}, uid: 10002 }
  assert.equal(startupRefusal(ok), null)
  // An inherited descriptor satisfies the socket gate.
  assert.equal(startupRefusal({ ...ok, socketPath: "", listenFd: 3 }), null)
  // No socket at all is refused before anything else is considered.
  assert.match(
    String(startupRefusal({ ...ok, socketPath: "", listenFd: null, env: { AGENT_AUTH_TOKEN: "t" } })),
    /socket path required/,
  )
  // The credential gate precedes the uid gate, so a root launch that also
  // leaked the token reports the leak (the actionable config error).
  assert.match(
    String(startupRefusal({ ...ok, uid: 0, env: { AGENT_AUTH_TOKEN_FILE: "/run/lunarwing/agent_auth_token" } })),
    /hub credential in its environment \(AGENT_AUTH_TOKEN_FILE\)/,
  )
  // Root with a clean environment is still refused.
  assert.match(String(startupRefusal({ ...ok, uid: 0 })), /refusing to run as root/)
})
