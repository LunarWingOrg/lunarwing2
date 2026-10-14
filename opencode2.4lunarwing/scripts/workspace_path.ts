import { closeSync, constants, existsSync, mkdirSync, openSync, realpathSync } from "node:fs"
import { isAbsolute, relative, resolve } from "node:path"

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
// DS-2: only validation, ENOTDIR/ELOOP (non-directory component), EACCES/EPERM
// and containment failures are permanent; other errnos (EMFILE/ENFILE/ENOMEM,
// ENOSPC/EDQUOT/EIO on the first mkdir, ENOENT/EINVAL from a swap race) are
// reported under a transient wording the host supervisor retries.
const NON_RETRYABLE_ERRNOS = new Set(["ENOTDIR", "ELOOP", "EACCES", "EPERM"])
export function resolveTaskWorkspace(
  context: { project_dir?: string; cron_tick?: unknown } | undefined,
  workspaceRoot: string,
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
  let fd = openSync(root, flags)
  try {
    for (const part of path.split("/")) {
      const child = `/proc/self/fd/${fd}/${part}`
      try {
        mkdirSync(child)
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw annotateComponent(error, part)
      }
      let next: number
      try {
        next = openSync(child, flags)
      } catch (error) {
        throw annotateComponent(error, part)
      }
      closeSync(fd)
      fd = next
    }
    return resolveWorkspacePath(path, root)
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
