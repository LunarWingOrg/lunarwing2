import { closeSync, constants, existsSync, fchownSync, fstatSync, mkdirSync, openSync, realpathSync, statSync } from "node:fs"
import { isAbsolute, relative, resolve } from "node:path"

import { taskIdentity } from "./task_identity.ts"

export function expandWorkspacePath(path: string, workspaceRoot: string): string {
  if (path === "~") return workspaceRoot
  if (path.startsWith("~/")) return `${workspaceRoot}/${path.slice(2)}`
  if (path.startsWith("~") || path.startsWith("/")) return path
  return `${workspaceRoot}/${path}`
}

// A relative() result escapes its base only when it is exactly ".." or
// starts with "../" — an in-root component may itself start with ".." (e.g.
// a directory named "..foo") and must not be misclassified as an escape
// (WI#375 DS-5).
function escapesRoot(fromBase: string): boolean {
  return fromBase === ".." || fromBase.startsWith("../") || isAbsolute(fromBase)
}

export function resolveWorkspacePath(path: string, workspaceRoot: string): string {
  const canonicalRoot = realpathSync(resolve(workspaceRoot))
  const expanded = expandWorkspacePath(path, canonicalRoot)
  const candidate = realpathSync(resolve(isAbsolute(expanded) ? expanded : resolve(canonicalRoot, expanded)))
  const fromRoot = relative(canonicalRoot, candidate)
  if (fromRoot === "" || !escapesRoot(fromRoot)) return candidate
  throw new Error(`Workspace path escapes WORKSPACE_ROOT: ${path}`)
}

export function workspacePathsOverlap(left: string, right: string): boolean {
  const fromLeft = relative(left, right)
  if (fromLeft === "" || !escapesRoot(fromLeft)) return true
  const fromRight = relative(right, left)
  return fromRight === "" || !escapesRoot(fromRight)
}

// Name the failing component (the raw error only names the /proc/self/fd
// magic path) while preserving the errno code for transient classification.
function annotateComponent(error: unknown, part: string): Error {
  const errno = error as NodeJS.ErrnoException
  const wrapped = new Error(`component '${part}': ${errno.message ?? String(error)}`)
  ;(wrapped as NodeJS.ErrnoException).code = errno.code
  return wrapped
}

// Keep cron-directory creation in sync with Kimi and OpenCode. Linux workers
// use descriptor-relative mkdir and O_NOFOLLOW to reject symlink traversal.
// DS-2: only validation, ENOTDIR/ELOOP (non-directory component), EACCES/EPERM,
// EINVAL and containment failures are permanent; other errnos (EMFILE/ENFILE/
// ENOMEM, ENOSPC/EDQUOT/EIO on the first mkdir, ENOENT from a swap race) are
// reported under a transient wording the host supervisor retries. EINVAL is
// permanent (run-170 LP-16): its only source here is fchown on an unmapped
// uid, which a re-dispatch can never fix.
const NON_RETRYABLE_ERRNOS = new Set(["ENOTDIR", "ELOOP", "EACCES", "EPERM", "EINVAL"])
export function resolveTaskWorkspace(
  context: { project_dir?: string; cron_tick?: unknown } | undefined,
  workspaceRoot: string,
  testHooks?: { beforeReresolve?: () => void },
): string {
  const tick = context?.cron_tick
  if (!tick || typeof tick !== "object" || !("event" in tick) || tick.event !== "cron.tick") {
    return resolveWorkspacePath(context?.project_dir || workspaceRoot, workspaceRoot)
  }
  const path = context?.project_dir
  if (!path || path.startsWith("~") || isAbsolute(path) || path.includes("\0") ||
      path.split("/").some(part => !part || part === "." || part === "..")) {
    throw new Error("Invalid workspace path for cron tick: expected relative directory components")
  }
  const root = realpathSync(resolve(workspaceRoot))
  // DS-6: fail explicitly when the descriptor-walk transport is unavailable
  // (e.g. a sandbox masking /proc) instead of a misleading mkdir ENOENT.
  if (!existsSync("/proc/self/fd")) {
    throw new Error("Invalid workspace path for cron tick: /proc/self/fd is not available (required for symlink-safe directory creation)")
  }
  const flags = constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW
  // WI#400 run-164 KW-4 + LD-6 (WI#449 migration): the bridge runs as root
  // while the cron task runs under the separate task uid, so every directory
  // THIS walk creates (intermediates and leaf alike) is handed to the task
  // identity — a root-owned intermediate would block sibling creation under
  // it. A pre-existing path is never chowned.
  const identity = taskIdentity()
  let fd = openSync(root, flags)
  try {
    for (const part of path.split("/")) {
      const child = `/proc/self/fd/${fd}/${part}`
      let created = false
      try {
        mkdirSync(child)
        created = true
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw annotateComponent(error, part)
      }
      let next: number
      try {
        next = openSync(child, flags)
      } catch (error) {
        throw annotateComponent(error, part)
      }
      // KW-4/LD-6: hand the created component over through the
      // O_DIRECTORY|O_NOFOLLOW descriptor the walk just opened — no path
      // re-resolution, no symlink window.
      if (created && identity) {
        fchownSync(next, identity.uid, identity.gid)
      }
      closeSync(fd)
      fd = next
    }
    // WI#444: deterministic test hook — a test injects the in-root redirect
    // swap at exactly this point (between the descriptor walk and the
    // re-resolve) so the DS-1 refusal is pinned on every run, with no
    // dependence on winning a race. Never passed by production callers.
    testHooks?.beforeReresolve?.()
    const resolved = resolveWorkspacePath(path, root)
    // DS-1: the descriptor walk and the re-resolved path must name the same
    // directory — a dev/ino mismatch means an in-root redirect raced the
    // walk. Thrown without an errno so the caller reports it under the
    // transient wording and the supervisor re-walks on the retry.
    const walked = fstatSync(fd)
    const landed = statSync(resolved)
    if (walked.dev !== landed.dev || walked.ino !== landed.ino) {
      throw new Error(`resolved path changed during bootstrap: ${path}`)
    }
    return resolved
  } catch (error) {
    const message = String(error)
    const code = (error as NodeJS.ErrnoException).code
    if (message.includes("Workspace path escapes WORKSPACE_ROOT") ||
        (code !== undefined && NON_RETRYABLE_ERRNOS.has(code))) {
      throw new Error(`Invalid workspace path for cron tick: ${message}`)
    }
    throw new Error(`Transient workspace error for cron tick: ${message}`)
  } finally {
    closeSync(fd)
  }
}
