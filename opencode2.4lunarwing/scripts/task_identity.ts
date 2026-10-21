/**
 * WI#400 (WI#449 migration) task identity: the worker bridge runs as
 * container root while task code (inside `opencode serve`, and every
 * one-shot cli/acp process) runs as the separate task uid via setpriv — so
 * the bridge's and PID 1's environ are EACCES to task code. The image pins
 * the identity via OPENCODE2_TASK_UID/OPENCODE2_TASK_GID; both unset means
 * "no split" (host-side dev runs). Exactly one set is a configuration error
 * and fails closed.
 */
export interface TaskIdentity {
  uid: number
  gid: number
}

export function taskIdentity(): TaskIdentity | null {
  const uidRaw = process.env.OPENCODE2_TASK_UID
  const gidRaw = process.env.OPENCODE2_TASK_GID
  if (uidRaw === undefined && gidRaw === undefined) return null
  if (uidRaw === undefined || gidRaw === undefined) {
    throw new Error("OPENCODE2_TASK_UID and OPENCODE2_TASK_GID must be set together")
  }
  // LV-18: agree with the entrypoint's ^[1-9][0-9]*$ gate — parseInt alone
  // accepts a partially numeric id ("1000x" parses as 1000) that the
  // entrypoint refuses; the two layers must refuse the same shapes.
  if (!/^[1-9][0-9]*$/.test(uidRaw) || !/^[1-9][0-9]*$/.test(gidRaw)) {
    throw new Error(`OPENCODE2_TASK_UID/OPENCODE2_TASK_GID must be non-zero numeric ids (a zero id would make the privilege drop a no-op) (got uid=${uidRaw}, gid=${gidRaw})`)
  }
  const uid = Number.parseInt(uidRaw, 10)
  const gid = Number.parseInt(gidRaw, 10)
  if (!Number.isInteger(uid) || !Number.isInteger(gid)) {
    throw new Error(`OPENCODE2_TASK_UID/OPENCODE2_TASK_GID must be non-zero numeric ids (a zero id would make the privilege drop a no-op) (got uid=${uidRaw}, gid=${gidRaw})`)
  }
  return { uid, gid }
}
