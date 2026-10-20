import assert from "node:assert/strict"
import { test } from "node:test"

import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
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
