import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { test } from "node:test"

/**
 * Real-CLI flag contract tests (WI#195 finding 2).
 *
 * Every other executor test points KIMI_BIN at a throwaway mock, so the CLI's
 * flag surface was pinned only in a comment ("this kimi-code version rejects
 * --yolo in print mode"). These tests execute the REAL installed kimi binary
 * and assert the contract the worker shells out to:
 *
 *   1. the headless invocation `-p <prompt> --output-format stream-json` is
 *      accepted (must exit 0 against an unreachable provider, never 64/2), and
 *   2. `--yolo` is rejected alongside `-p` (the exact reason
 *      kimi_task_executor.ts relies on config-only auto-approval).
 *
 * Self-skip: when no real kimi binary is on PATH (host dev machines) both
 * tests skip. In the worker image build (`npm test` in the Dockerfile) the
 * gate asserts the contract for real — `bin/kimi` is on PATH there; if the
 * build layout ever changes, KIMI_BIN can point at the installed CLI
 * explicitly.
 */

interface ParsedResult {
  code: number | null
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
    proc.on("close", (code) => resolve({ code, stdout, stderr }))
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
const contractOpts: { skip: boolean; timeout: number } = { skip: realKimi === undefined, timeout: 120_000 }
const realKimiBin = realKimi ?? "kimi"

function contractEnv(): NodeJS.ProcessEnv {
  return {
    ...process.env,
    // Production guard-off is the baked config + entrypoint exports; these
    // defaults keep the contract run deterministic if a developer probes.
    KIMI_CODE_DANGEROUS_COMMAND_GUARD: process.env.KIMI_CODE_DANGEROUS_COMMAND_GUARD ?? "false",
    KIMI_CODE_PERMISSION_MODE_REMINDER: process.env.KIMI_CODE_PERMISSION_MODE_REMINDER ?? "0",
    // Unreachable provider: -p must be PARSABLE without ever contacting a
    // model. A provider/connection failure is fine; usage rejection
    // (exit 64 / exit 2 "unknown option") is the bug this test guards against.
    KIMI_BASE_URL: "http://127.0.0.1:1/openai/v1",
    KIMI_API_KEY: "test",
    KIMI_CODE_NO_AUTO_UPDATE: "1",
    KIMI_DISABLE_TELEMETRY: "1",
  }
}

test(
  "real CLI accepts the headless worker invocation (-p --output-format stream-json)",
  contractOpts,
  async () => {
    const run = await runCli(
      realKimiBin,
      ["-p", "say hi", "--output-format", "stream-json"],
      contractEnv(),
    )
    if (run.code === 64 || run.code === 2) {
      assert.fail(
        `CLI rejected the headless flag contract (exit ${run.code}); ` +
          `stderr head: ${run.stderr.trim().slice(0, 400)}`,
      )
    }
    assert.equal(
      run.code,
      0,
      `expected exit 0 against an unreachable provider (usage was valid); ` +
        `got ${run.code}. stderr tail: ${run.stderr.trim().slice(-400)}`,
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

console.log(realKimi ? `kimi CLI contract self-check passed (real binary: ${realKimi})` : "kimi CLI contract self-check skipped (no real kimi binary)")
