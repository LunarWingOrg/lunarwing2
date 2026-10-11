import { closeSync, constants, mkdirSync, openSync, realpathSync } from "node:fs"
import { isAbsolute, relative, resolve } from "node:path"

export function expandWorkspacePath(path: string, workspaceRoot: string): string {
  if (path === "~") return workspaceRoot
  if (path.startsWith("~/")) return `${workspaceRoot}/${path.slice(2)}`
  if (path.startsWith("~") || path.startsWith("/")) return path
  return `${workspaceRoot}/${path}`
}

export function resolveWorkspacePath(path: string, workspaceRoot: string): string {
  const canonicalRoot = realpathSync(resolve(workspaceRoot))
  const expanded = expandWorkspacePath(path, canonicalRoot)
  const candidate = realpathSync(resolve(isAbsolute(expanded) ? expanded : resolve(canonicalRoot, expanded)))
  const fromRoot = relative(canonicalRoot, candidate)
  if (fromRoot === "" || (!fromRoot.startsWith("..") && !isAbsolute(fromRoot))) return candidate
  throw new Error(`Workspace path escapes WORKSPACE_ROOT: ${path}`)
}

export function workspacePathsOverlap(left: string, right: string): boolean {
  const fromLeft = relative(left, right)
  if (fromLeft === "" || (!fromLeft.startsWith("..") && !isAbsolute(fromLeft))) return true
  const fromRight = relative(right, left)
  return fromRight === "" || (!fromRight.startsWith("..") && !isAbsolute(fromRight))
}

// Cron ticks bootstrap their directory; ordinary tasks still require it to
// exist. Linux containers expose /proc. Descriptor-relative mkdir plus
// O_NOFOLLOW prevents a symlink swap from redirecting creation outside root.
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
  const flags = constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW
  let fd = openSync(root, flags)
  try {
    for (const part of path.split("/")) {
      const child = `/proc/self/fd/${fd}/${part}`
      try {
        mkdirSync(child)
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error
      }
      const next = openSync(child, flags)
      closeSync(fd)
      fd = next
    }
    return resolveWorkspacePath(path, root)
  } catch (error) {
    throw new Error(`Invalid workspace path for cron tick: ${String(error)}`)
  } finally {
    closeSync(fd)
  }
}
