import { realpathSync, statSync } from "node:fs"
import { isAbsolute, relative, resolve } from "node:path"

function escapes(path: string): boolean {
  return path === ".." || path.startsWith("../") || isAbsolute(path)
}

export function mapWorkspaceInput(path: string | undefined, workspaceRoot: string, hostRoot?: string): string {
  if (path === undefined || path === "" || path === "~") return workspaceRoot
  if (path.startsWith("~/")) return resolve(workspaceRoot, path.slice(2))
  if (!isAbsolute(path)) return resolve(workspaceRoot, path)
  if (hostRoot !== undefined && hostRoot !== "") {
    const fromHost = relative(resolve(hostRoot), path)
    if (fromHost === "") return workspaceRoot
    if (!escapes(fromHost)) return resolve(workspaceRoot, fromHost)
  }
  return path
}

export function resolveWorkspacePath(
  path: string | undefined,
  workspaceRoot: string,
  hostRoot?: string,
): string {
  if (path?.includes("\0")) throw new Error("workspace path contains NUL")
  const root = realpathSync(resolve(workspaceRoot))
  if (!statSync(root).isDirectory()) throw new Error("workspace root is not a directory")
  const mapped = mapWorkspaceInput(path, root, hostRoot)
  const candidate = realpathSync(mapped)
  if (!statSync(candidate).isDirectory()) throw new Error("task workspace is not a directory")
  if (escapes(relative(root, candidate))) throw new Error("workspace path escapes WORKSPACE_ROOT")
  return candidate
}

export function workspacePathsOverlap(left: string, right: string): boolean {
  const canonicalLeft = realpathSync(left)
  const canonicalRight = realpathSync(right)
  return !escapes(relative(canonicalLeft, canonicalRight)) || !escapes(relative(canonicalRight, canonicalLeft))
}
