/**
 * WI#400 task identity: the worker bridge runs as container root while task
 * code (inside `opencode serve`, and every one-shot cli/acp process) runs as
 * the separate task uid via setpriv — so the bridge's and PID 1's environ
 * are EACCES to task code. The image pins the identity via
 * OPENCODE_TASK_UID/OPENCODE_TASK_GID; both unset means "no split"
 * (host-side dev runs). Exactly one set is a configuration error and fails
 * closed.
 */
export interface TaskIdentity {
  uid: number
  gid: number
}

export function taskIdentity(): TaskIdentity | null {
  const uidRaw = process.env.OPENCODE_TASK_UID
  const gidRaw = process.env.OPENCODE_TASK_GID
  if (uidRaw === undefined && gidRaw === undefined) return null
  if (uidRaw === undefined || gidRaw === undefined) {
    throw new Error("OPENCODE_TASK_UID and OPENCODE_TASK_GID must be set together")
  }
  const uid = Number.parseInt(uidRaw, 10)
  const gid = Number.parseInt(gidRaw, 10)
  if (!Number.isInteger(uid) || uid < 0 || !Number.isInteger(gid) || gid < 0) {
    throw new Error(`OPENCODE_TASK_UID/OPENCODE_TASK_GID must be numeric ids (got uid=${uidRaw}, gid=${gidRaw})`)
  }
  return { uid, gid }
}
