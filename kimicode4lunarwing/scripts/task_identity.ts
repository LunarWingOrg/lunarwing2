/**
 * WI#400 task identity: the worker bridge runs as container root and drops
 * every spawned task to a separate uid/gid via setpriv, so the bridge's
 * (and PID 1's) environ is EACCES to task code. The image pins the identity
 * via KIMI_TASK_UID/KIMI_TASK_GID; both unset means "no split" (host-side
 * dev runs and the bridge test suite, where the bridge is unprivileged
 * anyway). Exactly one set is a configuration error and fails closed.
 */
import { accessSync, constants as fsConstants, statSync } from "node:fs"
import { delimiter, isAbsolute, join } from "node:path"

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
  // LV-18: agree with the entrypoint's ^[1-9][0-9]*$ gate — parseInt alone
  // accepts a partially numeric id ("1000x" parses as 1000) that the
  // entrypoint refuses; the two layers must refuse the same shapes.
  if (!/^[1-9][0-9]*$/.test(uidRaw) || !/^[1-9][0-9]*$/.test(gidRaw)) {
    throw new Error(`KIMI_TASK_UID/KIMI_TASK_GID must be non-zero numeric ids (a zero id would make the privilege drop a no-op) (got uid=${uidRaw}, gid=${gidRaw})`)
  }
  const uid = Number.parseInt(uidRaw, 10)
  const gid = Number.parseInt(gidRaw, 10)
  if (!Number.isInteger(uid) || !Number.isInteger(gid)) {
    throw new Error(`KIMI_TASK_UID/KIMI_TASK_GID must be non-zero numeric ids (a zero id would make the privilege drop a no-op) (got uid=${uidRaw}, gid=${gidRaw})`)
  }
  return { uid, gid }
}

/** setpriv argv prefix for the pinned task identity. The two cap-clearing
 * flags are mandatory: setpriv(1) preserves capabilities across the uid
 * change otherwise, and the task could regain the bridge uid (run-98 FY-1
 * prototype finding). --no-new-privs is mandatory too (run-164 KW-2): it
 * keeps the dropped task from ever regaining privilege through a setuid or
 * file-capability binary in the image. */
export function taskSetprivArgv(identity: TaskIdentity): string[] {
  return [
    `--reuid=${identity.uid}`,
    `--regid=${identity.gid}`,
    "--clear-groups",
    "--ambient-caps=-all",
    "--inh-caps=-all",
    "--no-new-privs",
    "--",
  ]
}

/** WI#400 LD-11/LP-6 + WI#454/ME-1: the privilege-dropping helper runs as
 * root BEFORE the drop, so it must be looked up on the bridge's own PATH —
 * never resolved through the child environment, which carries task-supplied
 * values. Only absolute PATH entries are considered. When the resolver
 * itself runs as root (the container bridge since WI#400), a candidate the
 * task uid could plant or rewrite — not root-owned, or sitting in a
 * group-/world-writable directory — is SKIPPED: resolving and executing it
 * would run task-planted code as container root. Unprivileged (host dev)
 * runs keep the plain lookup unless the caller forces the ownership rule:
 * the delegated task runner does (NL-1), because a candidate any earlier
 * task could plant would otherwise run inside every later task. */
export function resolveFromBridgePath(
  name: string,
  pathEnv: string | undefined = process.env.PATH,
  // NL-1 (!1873 run 200): the delegated task runner is unprivileged but must
  // still refuse task-plantable candidates, so it forces the rule on.
  enforceRootOwnership: boolean = process.geteuid?.() === 0,
): string {
  for (const dir of (pathEnv ?? "").split(delimiter)) {
    if (!dir || !isAbsolute(dir)) continue
    const candidate = join(dir, name)
    try {
      const candidateStat = statSync(candidate)
      if (!candidateStat.isFile()) continue
      accessSync(candidate, fsConstants.X_OK)
      if (enforceRootOwnership) {
        // WI#454/ME-1: root executes the resolved path — skip a candidate
        // the task uid could plant or rewrite (not root-owned, or sitting
        // in a group/world-writable directory).
        if (candidateStat.uid !== 0) continue
        const dirStat = statSync(dir)
        if (dirStat.uid !== 0 || dirStat.mode & 0o022) continue
      }
      return candidate
    } catch {
      continue
    }
  }
  throw new Error(`${name} not found on the bridge PATH (WI#400 LD-11)`)
}

/** WI#400 LD-11/LP-6: variables the dynamic loader and glibc honour BEFORE
 * the drop; a task-supplied value would run inside the still-root setpriv.
 * Removes them in place and returns the keys that were dropped.
 * The list beyond LD_* is glibc's own unsafe-environment set (LP-14/ME-3,
 * matching pebble's is_loader_env_key): LOCPATH/NLSPATH for locale loading,
 * HOSTALIASES for resolver overrides, RES_OPTIONS/LOCALDOMAIN for the
 * resolver, TZDIR for timezone loading, MALLOC_TRACE for allocator tracing. */
export function dropLoaderEnv(env: NodeJS.ProcessEnv): string[] {
  const loaderKeys = new Set([
    "GCONV_PATH",
    "GLIBC_TUNABLES",
    "LOCPATH",
    "NLSPATH",
    "HOSTALIASES",
    "RES_OPTIONS",
    "LOCALDOMAIN",
    "TZDIR",
    "MALLOC_TRACE",
  ])
  const dropped: string[] = []
  for (const key of Object.keys(env)) {
    if (key.startsWith("LD_") || loaderKeys.has(key)) {
      delete env[key]
      dropped.push(key)
    }
  }
  return dropped.sort()
}
