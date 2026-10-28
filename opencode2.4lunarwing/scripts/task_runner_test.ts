import assert from "node:assert/strict"
import { mkdtempSync, rmSync, statSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { after, test } from "node:test"
import type { OpenCodeClient } from "@opencode/client/promise"

import { TaskRunner, startupRefusal } from "./task_runner"
import { delegateTaskToRunner } from "./task_runner_client"
import type { TaskRequest } from "./lunarwing_runtime"

const scratchDirs: string[] = []
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), "opencode2-runner-"))
  scratchDirs.push(dir)
  return dir
}
after(() => {
  for (const dir of scratchDirs) rmSync(dir, { recursive: true, force: true })
})

function makeSdk(options: { pending?: boolean } = {}): OpenCodeClient {
  const messages = [{ type: "assistant", content: [{ type: "text", text: " delegated output " }] }]
  return {
    session: {
      create: async () => ({ id: "session-1" }),
      prompt: async () => {
        if (options.pending) await new Promise(() => {})
        return { id: "inbox-1", sessionID: "session-1", type: "user" }
      },
      interrupt: async () => ({ data: true }),
    },
    permission: { reply: async () => ({ data: true }) },
    message: {
      list: async () => ({ data: options.pending ? [] : messages, cursor: {} }),
    },
    event: {
      subscribe: () => (async function* () {
        if (options.pending) {
          await new Promise(() => {})
        } else {
          yield { type: "session.execution.succeeded", data: { sessionID: "session-1" } }
        }
      })(),
    },
  } as unknown as OpenCodeClient
}

async function startRunner(sdk: OpenCodeClient): Promise<{ runner: TaskRunner; path: string }> {
  const runner = new TaskRunner(sdk)
  const path = await runner.listen(join(scratch(), "runner.sock"))
  assert.ok(path, "runner must listen")
  return { runner, path }
}

function request(overrides: Partial<TaskRequest> = {}): TaskRequest {
  return { task_id: "task-1", prompt: "start", ...overrides }
}

test("runner refuses credentials before its uid gate", () => {
  const refusal = startupRefusal({
    listenFd: null,
    socketPath: "/tmp/runner.sock",
    env: { AGENT_AUTH_TOKEN_FILE: "/forbidden" },
    uid: 0,
  })
  assert.match(String(refusal), /hub credential/)
})

test("delegated execution returns canonical assistant output", async () => {
  const { runner, path } = await startRunner(makeSdk())
  try {
    const result = await delegateTaskToRunner(path, request(), undefined, () => {})
    assert.equal(result.status, "success", result.error ?? "")
    assert.equal(result.output, "delegated output")
  } finally {
    runner.close()
  }
})

test("delegated cancellation settles with a terminal cancelled result", async () => {
  const { runner, path } = await startRunner(makeSdk({ pending: true }))
  const controller = new AbortController()
  try {
    const pending = delegateTaskToRunner(path, request({ task_id: "cancel-me" }), undefined, () => {}, controller.signal)
    setTimeout(() => controller.abort(), 100)
    const result = await pending
    assert.equal(result.status, "cancelled")
    assert.equal(result.task_id, "cancel-me")
  } finally {
    runner.close()
  }
})

test("shared runner sockets allow bridge-group access", async () => {
  process.env.LUNARWING_TASK_RUNNER_REMOTE = "1"
  const { runner, path } = await startRunner(makeSdk())
  try {
    assert.equal(statSync(path).mode & 0o777, 0o660)
  } finally {
    runner.close()
    delete process.env.LUNARWING_TASK_RUNNER_REMOTE
  }
})
