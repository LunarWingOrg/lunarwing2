import { realpathSync } from "node:fs"
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
