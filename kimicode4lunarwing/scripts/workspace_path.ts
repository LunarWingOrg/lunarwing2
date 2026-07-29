export function expandWorkspacePath(path: string, workspaceRoot: string): string {
  if (path === "~") return workspaceRoot
  if (path.startsWith("~/")) return `${workspaceRoot}/${path.slice(2)}`
  if (path.startsWith("~") || path.startsWith("/")) return path
  return `${workspaceRoot}/${path}`
}
