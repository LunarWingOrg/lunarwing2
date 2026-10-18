/**
 * lunarwing_tool_plugin_test.ts — unit tests for the WI#324 opencode tool
 * plugin (the serve-side half of the worker tool proxy).
 *
 * Drives the real relay (lunarwing_tool_proxy.ts) on a temp socket dir with a
 * fake hub socket, and a stub plugin-host client whose session titles model
 * the bridge's `lunarwing-opencode-<task_id>` naming. Plain-script style like
 * the rest of this lane (bun scripts/*_test.ts).
 *
 * Run: bun scripts/lunarwing_tool_plugin_test.ts
 */

import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { createEnvelope } from "./lunarwing_runtime.ts"
import { ToolProxyRelay } from "./lunarwing_tool_proxy.ts"
import {
  createProxyTools,
  resolveTaskId,
  type ProxyToolContext,
  type SessionTitleClient,
} from "./lunarwing_tool_plugin.ts"

class FakeHubSocket {
  readonly sent: string[] = []
  send(data: string): void {
    this.sent.push(data)
  }

  lastFrame(): { type: string; payload: Record<string, unknown> } {
    assert.ok(this.sent.length > 0, "expected at least one hub frame")
    return JSON.parse(this.sent[this.sent.length - 1]) as { type: string; payload: Record<string, unknown> }
  }
}

/** Stub of the plugin host's SDK client: titles per session id. */
function stubClient(
  titles: Record<string, string | undefined>,
  parents: Record<string, string | undefined> = {},
): SessionTitleClient & { calls: number } {
  const state = {
    calls: 0,
    session: {
      get: async (options: { path: { id: string } }) => {
        state.calls++
        const title = titles[options.path.id]
        if (title === undefined) throw new Error("404: session not found")
        return { data: { title, parentID: parents[options.path.id] } }
      },
    },
  }
  return state
}

async function settle(ms = 100): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms))
}

/** Execute and return the thrown error's message (fails when nothing throws). */
async function thrown(fn: () => Promise<string>): Promise<string> {
  try {
    await fn()
  } catch (err) {
    return err instanceof Error ? err.message : String(err)
  }
  assert.fail("expected the tool call to throw")
}

// See the JC-1 note in lunarwing_tool_proxy_test.ts: restore, don't delete.
const savedProxyDir = process.env.LUNARWING_TOOL_PROXY_DIR
const workDir = mkdtempSync(join(tmpdir(), "tool-plugin-test-"))
process.env.LUNARWING_TOOL_PROXY_DIR = workDir
const relay = new ToolProxyRelay()

// ── the static tool surface is exactly the WI#324 phase-1 set ────────────────
{
  const tools = createProxyTools(stubClient({}))
  const names = Object.keys(tools).sort()
  assert.deepEqual(names, [
    "lunarwing_gitlab-tool",
    "lunarwing_gitlab_tool",
    "lunarwing_knowledge_get_topic",
    "lunarwing_knowledge_search",
    "lunarwing_knowledge_submit_note",
    "lunarwing_memory_read",
    "lunarwing_memory_write",
  ])
  for (const [name, def] of Object.entries(tools)) {
    assert.equal(typeof def.description, "string")
    assert.ok(def.description.includes("tool_allowlist"), `${name} documents the grant`)
    assert.equal(typeof def.execute, "function")
  }
  // Only hub-required arguments are declared (the registry marks every
  // declared arg required): memory_write must not re-advertise the denied
  // layer/force, and gitlab_tool keeps its action-specific payload free-form.
  assert.deepEqual(Object.keys(tools.lunarwing_memory_write!.args).sort(), ["content", "target"])
  assert.deepEqual(Object.keys(tools.lunarwing_memory_read!.args), ["path"])
  assert.deepEqual(Object.keys(tools.lunarwing_gitlab_tool!.args), ["action"])
  assert.equal(tools.lunarwing_memory_write!.args.layer, undefined)
}

// ── happy path: call relays to the hub with the title-resolved task id ───────
{
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("job-42", hub)
  assert.ok(handle)
  try {
    const client = stubClient({ "ses_1": "lunarwing-opencode-job-42" })
    const tools = createProxyTools(client)
    const context: ProxyToolContext = { sessionID: "ses_1" }

    const pending = tools.lunarwing_memory_read!.execute({ path: "notes/x.md" }, context)
    await settle()
    assert.equal(hub.sent.length, 1)
    const frame = hub.lastFrame()
    assert.equal(frame.type, "tool_call")
    assert.equal(frame.payload.task_id, "job-42")
    assert.equal(frame.payload.name, "memory_read")
    assert.deepEqual(frame.payload.arguments, { path: "notes/x.md" })
    const callId = frame.payload.call_id as string
    assert.match(callId, /^[A-Za-z0-9_-]{1,128}$/)

    relay.handleHubFrame(
      hub,
      createEnvelope("tool_result", { call_id: callId, status: "success", output: "file contents", error: null }),
    )
    assert.equal(await pending, "file contents")
    assert.equal(client.calls, 1, "the session title is resolved once")
  } finally {
    handle.close()
  }
}

// ── denied/error replies surface as tool errors carrying the hub's reason ────
{
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("job-denied", hub)
  assert.ok(handle)
  try {
    const client = stubClient({ "ses_2": "lunarwing-opencode-job-denied" })
    const tools = createProxyTools(client)
    const context: ProxyToolContext = { sessionID: "ses_2" }

    const pending = assert.rejects(
      tools.lunarwing_memory_write!.execute({ content: "x", target: "MEMORY.md" }, context),
      /not in this worker's tool_allowlist/,
    )
    await settle()
    const callId = hub.lastFrame().payload.call_id as string
    assert.equal(hub.lastFrame().payload.name, "memory_write")
    relay.handleHubFrame(
      hub,
      createEnvelope("tool_result", {
        call_id: callId,
        status: "denied",
        output: "",
        error: "tool 'memory_write' is not in this worker's tool_allowlist",
      }),
    )
    await pending
  } finally {
    handle.close()
  }
}

// ── sessions the bridge did not dispatch get a clean error, no hub traffic ───
{
  const hub = new FakeHubSocket()
  const client = stubClient({
    "ses_interactive": "untitled",
    "ses_gone": undefined,
  })
  const tools = createProxyTools(client)

  const interactive = await thrown(() =>
    tools.lunarwing_knowledge_search!.execute({ query: "x" }, { sessionID: "ses_interactive" }),
  )
  assert.match(interactive, /not dispatched by LunarWing/)

  const gone = await thrown(() =>
    tools.lunarwing_knowledge_search!.execute({ query: "x" }, { sessionID: "ses_gone" }),
  )
  assert.match(gone, /not dispatched by LunarWing/)

  const noSession = await thrown(() =>
    tools.lunarwing_knowledge_search!.execute({ query: "x" }, {}),
  )
  assert.match(noSession, /no session context/)

  assert.equal(hub.sent.length, 0, "no task binding must mean no hub traffic")
}

// ── task resolution: prefix rules, caching, first-title-wins ─────────────────
{
  const client = stubClient({
    "ses_a": "lunarwing-opencode-job-a",
    "ses_empty": "lunarwing-opencode-",
    "ses_other": "lunarwing-kimi-job-b",
  })
  const cache = new Map<string, string | null>()
  assert.equal(await resolveTaskId(client, cache, "ses_a"), "job-a")
  assert.equal(await resolveTaskId(client, cache, "ses_a"), "job-a")
  assert.equal(client.calls, 1, "cached resolutions never re-hit the serve API")
  assert.equal(await resolveTaskId(client, cache, "ses_empty"), null, "empty task id rejected")
  assert.equal(await resolveTaskId(client, cache, "ses_other"), null, "foreign prefix rejected")
  // Not cached: a lookup that fails may be retried (serve hiccup vs a session
  // that will never qualify — only successful resolutions are pinned).
  assert.equal(cache.has("ses_empty"), false)
}

// ── JU-1: subagent children resolve through their root session's title ───────
{
  const titles = {
    "ses_root": "lunarwing-opencode-job-root",
    "ses_child": "check the study notes (@general subagent)",
    "ses_grandchild": "lunarwing-opencode-job-forged (@general subagent)",
    "ses_missing-parent": "orphan (@general subagent)",
  }
  const parents = {
    "ses_child": "ses_root",
    "ses_grandchild": "ses_child",
    "ses_missing-parent": "ses_absent",
  }
  const client = stubClient(titles, parents)
  const cache = new Map<string, string | null>()

  // A child resolves to its root's task binding, not its own subagent title.
  assert.equal(await resolveTaskId(client, cache, "ses_child"), "job-root")
  // A grandchild whose own title is a forged task prefix still resolves to
  // the root: the binding is on the root session, not a child that happened
  // to inherit the title. (For a child, non-zero parent wins over the title.)
  assert.equal(await resolveTaskId(client, cache, "ses_grandchild"), "job-root")
  // A missing parent (404) yields null, never a crash or a wrong binding.
  assert.equal(await resolveTaskId(client, cache, "ses_missing-parent"), null)

  // A parent cycle is refused (never loops forever).
  const cycleTitle = { a: "lunarwing-opencode-job-cycle", b: "no-title" }
  const cycle = stubClient(cycleTitle, { a: "b", b: "a" })
  assert.equal(await resolveTaskId(cycle, new Map(), "a"), null)
}

// ── a task whose relay never started fails the call, never hangs ─────────────
{
  const client = stubClient({ "ses_3": "lunarwing-opencode-job-no-relay" })
  const tools = createProxyTools(client)
  const message = await thrown(() =>
    tools.lunarwing_memory_read!.execute({ path: "notes/x.md" }, { sessionID: "ses_3" }),
  )
  assert.match(message, /lunarwing proxy: tool proxy socket error/)
}

if (savedProxyDir === undefined) delete process.env.LUNARWING_TOOL_PROXY_DIR
else process.env.LUNARWING_TOOL_PROXY_DIR = savedProxyDir
rmSync(workDir, { recursive: true, force: true })
console.log("opencode tool-proxy plugin self-check passed")
