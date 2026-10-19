import assert from "node:assert/strict"
import { test } from "node:test"

import { taskIdentity, taskSetprivArgv } from "./task_identity.js"

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

test("taskSetprivArgv: exact flag sequence, cap-clearing pair mandatory (WI#400)", () => {
  // setpriv(1) preserves capabilities across the uid change unless the
  // ambient/inheritable sets are cleared — a task keeping ambient
  // SETUID/SETGID could regain the bridge uid (run-98 FY-1 prototype).
  assert.deepEqual(taskSetprivArgv({ uid: 10002, gid: 10002 }), [
    "--reuid=10002",
    "--regid=10002",
    "--clear-groups",
    "--ambient-caps=-all",
    "--inh-caps=-all",
    "--",
  ])
})
