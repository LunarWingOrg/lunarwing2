/**
 * WI#400 task identity: the worker bridge runs as container root and drops
 * every spawned task to a separate uid/gid via setpriv, so the bridge's
 * (and PID 1's) environ is EACCES to task code. The image pins the identity
 * via KIMI_TASK_UID/KIMI_TASK_GID; both unset means "no split" (host-side
 * dev runs and the bridge test suite, where the bridge is unprivileged
 * anyway). Exactly one set is a configuration error and fails closed.
 */
export interface TaskIdentity {
  uid: number
  gid: number
}

export function taskIdentity(): TaskIdentity | null {
  const uidRaw = process.env.KIMI_TASK_UID
  const gidRaw = process.env.KIMI_TASK_GID
  if (uidRaw === undefined && gidRaw === undefined) return null
  if (uidRaw === undefined || gidRaw === undefined) {
    throw new Error("KIMI_TASK_UID and KIMI_TASK_GID must be set together")
  }
  const uid = Number.parseInt(uidRaw, 10)
  const gid = Number.parseInt(gidRaw, 10)
  if (!Number.isInteger(uid) || uid < 0 || !Number.isInteger(gid) || gid < 0) {
    throw new Error(`KIMI_TASK_UID/KIMI_TASK_GID must be numeric ids (got uid=${uidRaw}, gid=${gidRaw})`)
  }
  return { uid, gid }
}

/** setpriv argv prefix for the pinned task identity. The two cap-clearing
 * flags are mandatory: setpriv(1) preserves capabilities across the uid
 * change otherwise, and the task could regain the bridge uid (run-98 FY-1
 * prototype finding). */
export function taskSetprivArgv(identity: TaskIdentity): string[] {
  return [
    `--reuid=${identity.uid}`,
    `--regid=${identity.gid}`,
    "--clear-groups",
    "--ambient-caps=-all",
    "--inh-caps=-all",
    "--",
  ]
}
