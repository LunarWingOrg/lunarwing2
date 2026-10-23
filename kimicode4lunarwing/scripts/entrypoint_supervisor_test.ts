import assert from "node:assert/strict"
import { spawn, spawnSync } from "node:child_process"
import { closeSync, existsSync, mkdtempSync, openSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"

// NI-3 (!1868 run 198): the runner supervisor is a `( ... ) &` subshell that
// forks BEFORE the bridge exists. Killing $BRIDGE_PID from inside it was a
// no-op and `exit 1` only ended the subshell, so the container kept a root
// bridge accepting tasks it could never complete.
// NL-5 (!1873 run 200): the supervisor only checked `kill -0`, so a
// SIGSTOPped runner was never restarted.
// NL-8 (!1873 run 200): these tests used a stand-in cleanup; they now run the
// launcher's REAL websocket-mode cleanup() and task_runner_alive() verbatim.
//
// The supervisor text, task_runner_alive() and cleanup() are taken verbatim
// from entrypoint.sh. Only run_task_runner (the image exec chain, covered by
// entrypoint_config_test.ts) is stubbed, and `sleep` is shortened.

const entrypoint = readFileSync(fileURLToPath(new URL("../entrypoint.sh", import.meta.url)), "utf8")

function slice(startMarker: string, endMarker: string, label: string): string {
  const start = entrypoint.indexOf(startMarker)
  const end = entrypoint.indexOf(endMarker, start)
  assert.ok(start > 0 && end > start, `${label} not found in entrypoint.sh`)
  return entrypoint.slice(start, end + endMarker.length)
}

// The supervisor block, verbatim: from its opening `(` through the closing
// `) &`. Both markers are unique in the launcher.
function supervisorBlock(): string {
  return slice("      (\n        _attempts=0", "      ) &", "supervisor block")
}

function aliveFunction(): string {
  return slice("task_runner_alive() {\n", "\n}\n", "task_runner_alive")
}

// The websocket-mode cleanup (the one that knows the runner and its
// supervisor), verbatim.
function realCleanup(): string {
  return slice("      cleanup() {\n        log \"Shutting down...\"\n        [ -z \"${RUNNER_SUPERVISOR_PID", "\n      }\n", "websocket cleanup")
}

// RUNNER_MODE=die: every runner generation exits at once (a broken runner).
// RUNNER_MODE=long: every generation is a long-lived process (a healthy one).
// `exec sleep` runs the external binary, never the shortened `sleep` shim.
const HARNESS = `
set -u
log() { printf '[%s] %s\\n' "$$" "$*" >&2; }
sleep() { command sleep 0.05; }
run_task_runner() {
  if [ "$RUNNER_MODE" = die ]; then exit 1; fi
  exec sleep 300
}
RUNNER_PID_FILE="$WORK/runner.pid"
`

function launcherScript(): string {
  // Function replacement: a string replacement would treat `$$` in the
  // supervisor text as a replace pattern and collapse it to `$`, silently
  // turning `kill -TERM "$$"` into a no-op.
  return `${HARNESS}
${aliveFunction()}
${realCleanup()}
# The real entrypoint arms its cleanup trap before any mode work.
trap cleanup EXIT INT TERM
command sleep 300 &
HEALTH_PID=$!
echo $HEALTH_PID > "$WORK/health.pid"
run_task_runner &
RUNNER_PID=$!
echo "$RUNNER_PID" > "$RUNNER_PID_FILE"
__SUPERVISOR_BLOCK__
RUNNER_SUPERVISOR_PID=$!
echo $RUNNER_SUPERVISOR_PID > "$WORK/supervisor.pid"
echo $$ > "$WORK/launcher.pid"
# The entrypoint starts the bridge after the supervisor fork and waits on it.
command sleep 300 &
BRIDGE_PID=$!
echo $BRIDGE_PID > "$WORK/bridge.pid"
wait "$BRIDGE_PID"
`.replace("__SUPERVISOR_BLOCK__", () => supervisorBlock())
}

function readPid(path: string): number | null {
  try {
    const value = Number(readFileSync(path, "utf8").trim())
    return Number.isInteger(value) && value > 0 ? value : null
  } catch {
    return null
  }
}

/** True for a live, non-zombie process that is not stopped. */
function running(pid: number): boolean {
  try {
    process.kill(pid, 0)
  } catch {
    return false
  }
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8")
    const state = stat.slice(stat.lastIndexOf(")") + 2).split(" ")[0]
    return !["Z", "X", "x", "T", "t"].includes(state)
  } catch {
    return false
  }
}

function alive(pid: number): boolean {
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

async function waitFor(check: () => boolean, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (check()) return true
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  return check()
}

test("supervisor give-up terminates the launcher, bridge, runner and health server through the real cleanup (NI-3, NL-8)", () => {
  const work = mkdtempSync(join(tmpdir(), "kimi-supervisor-"))
  const scriptPath = join(work, "launcher.sh")
  writeFileSync(scriptPath, launcherScript())
  const logPath = join(work, "launcher.log")
  const fd = openSync(logPath, "a")
  const startedAt = Date.now()
  let result: ReturnType<typeof spawnSync> | undefined
  try {
    result = spawnSync("bash", [scriptPath], {
      cwd: work,
      stdio: ["ignore", fd, fd],
      env: { ...process.env, WORK: work, RUNNER_MODE: "die" },
      // Generous for five 0.05 s attempts; short enough that a launcher which
      // never learns its runner died is caught as a timeout, not a pass.
      timeout: 30_000,
      killSignal: "SIGKILL",
    })
  } finally {
    closeSync(fd)
  }
  const log = readFileSync(logPath, "utf8")
  const elapsedMs = Date.now() - startedAt

  assert.match(log, /Task runner failed 5 restart attempts/, log)
  assert.match(log, /stopping the container for orchestrator restart/, log)
  // The REAL cleanup ran (it logs before it stops anything).
  assert.match(log, /Shutting down\.\.\./, log)
  // The launcher must stop on its own, quickly: with 0.05 s restarts five
  // attempts take well under a second. A supervisor that only exits leaves
  // the launcher blocked in `wait "$BRIDGE_PID"` until the timeout, so
  // elapsed time is what separates the fixed code from the NI-3 shape.
  assert.equal(result?.signal === "SIGKILL", false, `launcher never stopped on its own: ${log}`)
  assert.ok(elapsedMs < 20_000, `launcher took ${elapsedMs} ms to stop`)

  const pids = ["launcher", "bridge", "health", "supervisor"].map((name) => [name, readPid(join(work, `${name}.pid`))] as const)
  for (const [name, pid] of pids) assert.ok(pid, `${name} pid not recorded`)
  const survivors = pids.filter(([, pid]) => alive(pid!)).map(([name, pid]) => `${name}=${pid}`)
  assert.deepEqual(survivors, [], `processes still alive after give-up: ${survivors.join(",")}`)
})

test("supervisor restarts a STOPPED runner and cleanup stops everything (NL-5, NL-8)", async () => {
  const work = mkdtempSync(join(tmpdir(), "kimi-supervisor-stop-"))
  const scriptPath = join(work, "launcher.sh")
  writeFileSync(scriptPath, launcherScript())
  const launcher = spawn("bash", [scriptPath], {
    cwd: work,
    stdio: ["ignore", "ignore", "pipe"],
    env: { ...process.env, WORK: work, RUNNER_MODE: "long" },
    detached: true,
  })
  let log = ""
  launcher.stderr.on("data", (chunk: Buffer) => { log += chunk.toString("utf8") })
  try {
    assert.ok(await waitFor(() => existsSync(join(work, "bridge.pid")), 10_000), `launcher never started: ${log}`)
    const first = readPid(join(work, "runner.pid"))!
    assert.ok(running(first), "first runner generation should be running")
    // Any task can do this: the runner runs as the task uid.
    process.kill(first, "SIGSTOP")
    const restarted = await waitFor(() => {
      const current = readPid(join(work, "runner.pid"))
      return current !== null && current !== first && running(current)
    }, 10_000)
    assert.ok(restarted, `a stopped runner must be replaced (NL-5): ${log}`)
    assert.equal(alive(first), false, "the stopped generation must be killed, not left behind")
    assert.match(log, /Task runner exited — restarting \(attempt 1\/5\)/)
    const second = readPid(join(work, "runner.pid"))!

    // The real cleanup stops the live generation, the supervisor, the
    // bridge and the health server.
    process.kill(readPid(join(work, "launcher.pid"))!, "SIGTERM")
    const pids = [second, ...["supervisor", "bridge", "health", "launcher"].map((name) => readPid(join(work, `${name}.pid`))!)]
    assert.ok(await waitFor(() => pids.every((pid) => !alive(pid)), 10_000),
      `still alive after cleanup: ${pids.filter((pid) => alive(pid)).join(",")}; ${log}`)
    assert.match(log, /Shutting down\.\.\./)
  } finally {
    try { process.kill(-launcher.pid!, "SIGKILL") } catch { /* gone */ }
    for (const name of ["runner", "bridge", "health"]) {
      const pid = readPid(join(work, `${name}.pid`))
      if (pid) try { process.kill(pid, "SIGKILL") } catch { /* gone */ }
    }
  }
})
