import assert from "node:assert/strict"
import { test } from "node:test"

import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { execFileSync } from "node:child_process"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { dropLoaderEnv, resolveFromBridgePath, taskIdentity, taskSetprivArgv } from "./task_identity.js"

test("taskIdentity: both unset means no uid split (host dev)", () => {
  delete process.env.KIMI_TASK_UID
  delete process.env.KIMI_TASK_GID
  assert.equal(taskIdentity(), null)
})

test("taskIdentity: exactly one set fails closed", () => {
  process.env.KIMI_TASK_UID = "10002"
  delete process.env.KIMI_TASK_GID
  assert.throws(() => taskIdentity(), /must be set together/)
  delete process.env.KIMI_TASK_UID
  process.env.KIMI_TASK_GID = "10002"
  assert.throws(() => taskIdentity(), /must be set together/)
  delete process.env.KIMI_TASK_GID
})

test("taskIdentity: non-numeric ids fail closed", () => {
  process.env.KIMI_TASK_UID = "abc"
  process.env.KIMI_TASK_GID = "10002"
  assert.throws(() => taskIdentity(), /numeric ids/)
  delete process.env.KIMI_TASK_UID
  delete process.env.KIMI_TASK_GID
})

test("taskIdentity: zero ids fail closed (WI#400 LP-17 — a zero id would make the privilege drop a no-op)", () => {
  process.env.KIMI_TASK_UID = "0"
  process.env.KIMI_TASK_GID = "10002"
  assert.throws(() => taskIdentity(), /non-zero numeric ids/)
  process.env.KIMI_TASK_UID = "10002"
  process.env.KIMI_TASK_GID = "0"
  assert.throws(() => taskIdentity(), /non-zero numeric ids/)
  process.env.KIMI_TASK_UID = "0"
  process.env.KIMI_TASK_GID = "0"
  assert.throws(() => taskIdentity(), /non-zero numeric ids/)
  delete process.env.KIMI_TASK_UID
  delete process.env.KIMI_TASK_GID
})

test("taskIdentity: partially numeric ids fail closed (WI#400 LV-18 — parseInt(\"1000x\") is 1000; the helper must agree with the entrypoint's ^[1-9][0-9]*$ gate)", () => {
  for (const [uid, gid] of [["1000x", "10002"], ["10002", "1000x"], [" 10002", "10002"], ["10002", "10002 "]]) {
    process.env.KIMI_TASK_UID = uid
    process.env.KIMI_TASK_GID = gid
    assert.throws(() => taskIdentity(), /non-zero numeric ids/, `uid=${uid} gid=${gid}`)
  }
  delete process.env.KIMI_TASK_UID
  delete process.env.KIMI_TASK_GID
})

test("taskSetprivArgv: exact flag sequence, cap-clearing pair + no-new-privs mandatory (WI#400)", () => {
  // setpriv(1) preserves capabilities across the uid change unless the
  // ambient/inheritable sets are cleared — a task keeping ambient
  // SETUID/SETGID could regain the bridge uid (run-98 FY-1 prototype).
  // --no-new-privs keeps the dropped task from regaining privilege through a
  // setuid or file-capability binary in the image (run-164 KW-2).
  assert.deepEqual(taskSetprivArgv({ uid: 10002, gid: 10002 }), [
    "--reuid=10002",
    "--regid=10002",
    "--clear-groups",
    "--ambient-caps=-all",
    "--inh-caps=-all",
    "--no-new-privs",
    "--",
  ])
})

test("resolveFromBridgePath: finds an executable on the given PATH and ignores relative entries (WI#400 LD-11)", () => {
  const base = mkdtempSync(join(tmpdir(), "task-identity-path-"))
  try {
    const good = join(base, "good")
    const noexec = join(base, "noexec")
    mkdirSync(good)
    mkdirSync(noexec)
    writeFileSync(join(good, "setpriv"), "#!/bin/sh\nexit 0\n")
    chmodSync(join(good, "setpriv"), 0o755)
    writeFileSync(join(noexec, "setpriv"), "not executable")
    chmodSync(join(noexec, "setpriv"), 0o644)
    assert.equal(resolveFromBridgePath("setpriv", `${noexec}:${good}`), join(good, "setpriv"))
    assert.equal(resolveFromBridgePath("setpriv", `relative/dir:${good}`), join(good, "setpriv"))
    assert.throws(() => resolveFromBridgePath("setpriv", `${noexec}:relative/dir:`), /not found on the bridge PATH/)
    assert.throws(() => resolveFromBridgePath("setpriv", ""), /not found on the bridge PATH/)
  } finally {
    rmSync(base, { recursive: true, force: true })
  }
})

test("resolveFromBridgePath: skips a group-writable directory and a non-root-owned candidate (WI#454/ME-1)", () => {
  // Only meaningful as root: the resolver runs as the root bridge in the
  // container, and only root can create root-owned fixtures.
  if (process.geteuid?.() !== 0) {
    console.log("SKIP: resolveFromBridgePath ownership pin requires root (WI#454/ME-1)")
    return
  }
  const base = mkdtempSync(join(tmpdir(), "task-identity-owned-"))
  try {
    const writableDir = join(base, "writable-dir")
    const ownedDir = join(base, "owned-dir")
    const cleanDir = join(base, "clean-dir")
    mkdirSync(writableDir)
    mkdirSync(ownedDir)
    mkdirSync(cleanDir)
    // Explicit chmod: the process umask masks mkdirSync's mode, and the pin
    // must hold whatever umask the test runs under.
    chmodSync(writableDir, 0o775)
    chmodSync(ownedDir, 0o755)
    chmodSync(cleanDir, 0o755)
    // A candidate inside a group-writable directory must be skipped.
    writeFileSync(join(writableDir, "setpriv"), "#!/bin/sh\nexit 0\n")
    chmodSync(join(writableDir, "setpriv"), 0o755)
    // A candidate executable but non-root-owned must be skipped: drop its
    // ownership to a non-root uid (nobody).
    writeFileSync(join(ownedDir, "setpriv"), "#!/bin/sh\nexit 0\n")
    chmodSync(join(ownedDir, "setpriv"), 0o755)
    execFileSync("chown", ["65534", join(ownedDir, "setpriv")])
    // The first clean, root-owned candidate wins even when the hostile
    // entries come first on the PATH.
    writeFileSync(join(cleanDir, "setpriv"), "#!/bin/sh\nexit 0\n")
    chmodSync(join(cleanDir, "setpriv"), 0o755)
    assert.equal(
      resolveFromBridgePath("setpriv", `${writableDir}:${ownedDir}:${cleanDir}`),
      join(cleanDir, "setpriv"),
      "the hostile entries must be skipped, not resolved",
    )
    // With nothing else on the PATH the hostile entries still fail closed.
    assert.throws(
      () => resolveFromBridgePath("setpriv", `${writableDir}:${ownedDir}`),
      /not found on the bridge PATH/,
    )
  } finally {
    rmSync(base, { recursive: true, force: true })
  }
})

test("dropLoaderEnv: removes LD_*, GCONV_PATH and GLIBC_TUNABLES and keeps everything else (WI#400 LD-11)", () => {
  const env: NodeJS.ProcessEnv = {
    PATH: "/usr/bin",
    HOME: "/home/kimi",
    LD_PRELOAD: "/tmp/evil.so",
    LD_LIBRARY_PATH: "/tmp",
    GCONV_PATH: "/tmp/gconv",
    GLIBC_TUNABLES: "glibc.malloc.check=3",
    LDFLAGS: "-O2",
  }
  assert.deepEqual(dropLoaderEnv(env), ["GCONV_PATH", "GLIBC_TUNABLES", "LD_LIBRARY_PATH", "LD_PRELOAD"])
  assert.deepEqual(env, { PATH: "/usr/bin", HOME: "/home/kimi", LDFLAGS: "-O2" })
  assert.deepEqual(dropLoaderEnv(env), [])
})
