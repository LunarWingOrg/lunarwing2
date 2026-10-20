import assert from "node:assert/strict"

import { taskIdentity } from "./task_identity"

delete process.env.OPENCODE_TASK_UID
delete process.env.OPENCODE_TASK_GID
assert.equal(taskIdentity(), null, "both unset must mean no split")

process.env.OPENCODE_TASK_UID = "10001"
assert.throws(() => taskIdentity(), /must be set together/)
delete process.env.OPENCODE_TASK_UID
process.env.OPENCODE_TASK_GID = "10001"
assert.throws(() => taskIdentity(), /must be set together/)
delete process.env.OPENCODE_TASK_GID

process.env.OPENCODE_TASK_UID = "abc"
process.env.OPENCODE_TASK_GID = "10001"
assert.throws(() => taskIdentity(), /numeric ids/)
delete process.env.OPENCODE_TASK_UID
delete process.env.OPENCODE_TASK_GID

// WI#400 LP-17: a zero id parses as a valid integer but makes the setpriv
// privilege drop a no-op while every caller keeps treating the split as
// active — it must fail closed exactly like a non-numeric id.
for (const [uid, gid] of [["0", "10001"], ["10001", "0"], ["0", "0"]]) {
  process.env.OPENCODE_TASK_UID = uid
  process.env.OPENCODE_TASK_GID = gid
  assert.throws(() => taskIdentity(), /non-zero numeric ids/, `uid=${uid} gid=${gid}`)
  delete process.env.OPENCODE_TASK_UID
  delete process.env.OPENCODE_TASK_GID
}

process.env.OPENCODE_TASK_UID = "10001"
process.env.OPENCODE_TASK_GID = "10001"
assert.deepEqual(taskIdentity(), { uid: 10001, gid: 10001 })
delete process.env.OPENCODE_TASK_UID
delete process.env.OPENCODE_TASK_GID

// WI#400 LV-18: parseInt("1000x") is 1000, so a partially numeric id must
// fail closed too — the helper agrees with the entrypoint's ^[1-9][0-9]*$
// gate (defence in depth; the entrypoint runs first and exports checked
// values).
for (const [uid, gid] of [["1000x", "10001"], ["10001", "1000x"], [" 10001", "10001"], ["10001", "10001 "]]) {
  process.env.OPENCODE_TASK_UID = uid
  process.env.OPENCODE_TASK_GID = gid
  assert.throws(() => taskIdentity(), /non-zero numeric ids/, `uid=${uid} gid=${gid}`)
  delete process.env.OPENCODE_TASK_UID
  delete process.env.OPENCODE_TASK_GID
}

console.log("task identity self-check passed")
