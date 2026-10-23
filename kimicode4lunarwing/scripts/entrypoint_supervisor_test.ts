import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { closeSync, existsSync, mkdtempSync, openSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"

// NI-3 (!1868 run 198): the runner supervisor is a `( ... ) &` subshell that
// forks BEFORE the bridge exists. Killing $BRIDGE_PID from inside it was a
// no-op and `exit 1` only ended the subshell, so the container kept a root
// bridge accepting tasks it could never complete.
//
// This test runs the supervisor text taken verbatim from entrypoint.sh with a
// runner that always dies and a stand-in bridge started after the fork (the
// entrypoint's ordering), and asserts the launcher's cleanup trap ran and the
// bridge is gone. The launcher blocks in `wait "$BRIDGE_PID"` like the real
// entrypoint, so the old code shape cannot pass by falling off the end.

const entrypoint = readFileSync(fileURLToPath(new URL("../entrypoint.sh", import.meta.url)), "utf8")

// The supervisor block, verbatim: from its opening `(` through the closing
// `) &`. Both markers are unique in the launcher.
function supervisorBlock(): string {
  const start = entrypoint.indexOf("      (\n        _attempts=0")
  const end = entrypoint.indexOf("      ) &", start)
  assert.ok(start > 0 && end > start, "supervisor block not found in entrypoint.sh")
  return entrypoint.slice(start, end + "      ) &".length)
}

// Shim the launcher-side helpers the block calls so the test runs unprivileged
// and without the image: `as_task` just execs, `log` writes to stderr, and the
// restart gap is short (`sleep 30` → 0.05 s). `node` is a stub that exits 1 at
// once, so each generation is already gone when the grace window closes and
// the attempt counter keeps climbing (the real runner also fails far inside
// the real 30 s window when it is broken).
const HARNESS = `
set -u
log() { printf '[%s] %s\\n' "$$" "$*" >&2; }
as_task() { "$@"; }
sleep() { command sleep 0.05; }
mkdir -p "$WORK/bin"
printf '#!/bin/sh\\nexit 1\\n' > "$WORK/bin/node"
chmod 0755 "$WORK/bin/node"
export PATH="$WORK/bin:$PATH"
RUNNER_SOCKET="$WORK/runner.sock"
RUNNER_PID_FILE="$WORK/runner.pid"
WORKSPACE_ROOT="$WORK"
# A real, already-reaped pid: the first watch loop exits immediately.
( exit 0 ) & RUNNER_PID=$!
wait "$RUNNER_PID" 2>/dev/null || true
`

const LAUNCHER = `
# The real entrypoint arms its cleanup trap before any mode work, so the trap
# exists before the supervisor does.
cleanup() {
  log "cleanup ran"
  if [ -n "\${BRIDGE_PID:-}" ]; then kill "$BRIDGE_PID" 2>/dev/null || true; fi
  echo "CLEANED" > "$WORK/cleanup.marker"
  exit 0
}
trap cleanup EXIT INT TERM
(
__SUPERVISOR_BLOCK__
)
SUPERVISOR_PID=$!
echo $$ > "$WORK/launcher.pid"
# The entrypoint starts the bridge after the supervisor fork.
command sleep 300 &
BRIDGE_PID=$!
echo $BRIDGE_PID > "$WORK/bridge.pid"
# The real entrypoint waits on the bridge, so a supervisor that merely exits
# leaves the launcher (and the bridge) alive.
wait "$BRIDGE_PID"
echo "LAUNCHER_FELL_THROUGH" > "$WORK/fell-through.marker"
`

function runSupervisor(): { work: string; log: string; elapsedMs: number; timedOut: boolean } {
  const work = mkdtempSync(join(tmpdir(), "kimi-supervisor-"))
  const scriptPath = join(work, "launcher.sh")
  // Function replacement: a string replacement would treat `$$` in the
  // supervisor text as a replace pattern and collapse it to `$`, silently
  // turning `kill -TERM "$$"` into a no-op.
  writeFileSync(scriptPath, `${HARNESS}\n${LAUNCHER.replace("__SUPERVISOR_BLOCK__", () => supervisorBlock())}`)
  const logPath = join(work, "launcher.log")
  const fd = openSync(logPath, "a")
  const startedAt = Date.now()
  let result: ReturnType<typeof spawnSync> | undefined
  try {
    result = spawnSync("bash", [scriptPath], {
      cwd: work,
      stdio: ["ignore", fd, fd],
      env: { ...process.env, WORK: work },
      // Generous for five 0.05 s attempts; short enough that a launcher which
      // never learns its runner died is caught as a timeout, not a pass.
      timeout: 30_000,
      killSignal: "SIGKILL",
    })
  } finally {
    closeSync(fd)
  }
  return {
    work,
    log: readFileSync(logPath, "utf8"),
    elapsedMs: Date.now() - startedAt,
    timedOut: result?.signal === "SIGKILL",
  }
}

function readPid(path: string): number | null {
  try {
    const value = Number(readFileSync(path, "utf8").trim())
    return Number.isInteger(value) && value > 0 ? value : null
  } catch {
    return null
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

test("supervisor give-up terminates the launcher and its bridge (NI-3)", () => {
  const { work, log, elapsedMs, timedOut } = runSupervisor()

  assert.match(log, /Task runner failed 5 restart attempts/, log)
  assert.match(log, /stopping the container for orchestrator restart/, log)

  // The launcher must stop on its own, quickly: with 0.05 s restarts five
  // attempts take well under a second. A supervisor that only exits leaves the
  // launcher blocked in `wait "$BRIDGE_PID"` until the timeout, whose SIGKILL
  // would still let the EXIT trap run — so elapsed time, not the marker alone,
  // is what separates the fixed code from the old shape.
  assert.equal(timedOut, false, `launcher never stopped on its own: ${log}`)
  assert.ok(elapsedMs < 20_000, `launcher took ${elapsedMs} ms to stop`)
  assert.ok(existsSync(join(work, "cleanup.marker")), `launcher cleanup trap never ran: ${log}`)
  assert.ok(!existsSync(join(work, "fell-through.marker")), "launcher fell through its bridge wait")

  const launcherPid = readPid(join(work, "launcher.pid"))
  const bridgePid = readPid(join(work, "bridge.pid"))
  assert.ok(launcherPid, "launcher pid not recorded")
  assert.ok(bridgePid, "bridge pid not recorded")
  const survivors = [launcherPid, bridgePid].filter((pid) => alive(pid!))
  assert.deepEqual(survivors, [], `processes still alive after give-up: ${survivors.join(",")}`)
})
