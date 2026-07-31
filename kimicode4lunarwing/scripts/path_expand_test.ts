import assert from "node:assert/strict"
import { test } from "node:test"
import { mkdirSync, mkdtempSync, symlinkSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { expandWorkspacePath, resolveWorkspacePath } from "./workspace_path.js"

const workspaceRoot = "/workspace"

test("expandWorkspacePath: ~ resolves to workspace root", () => {
  assert.equal(expandWorkspacePath("~", workspaceRoot), workspaceRoot)
})

test("expandWorkspacePath: ~/foo joins under root", () => {
  assert.equal(expandWorkspacePath("~/foo", workspaceRoot), "/workspace/foo")
})

test("expandWorkspacePath: absolute path preserved", () => {
  assert.equal(expandWorkspacePath("/srv/project", workspaceRoot), "/srv/project")
})

test("expandWorkspacePath: relative joined under root", () => {
  assert.equal(expandWorkspacePath("subdir", workspaceRoot), "/workspace/subdir")
})

test("expandWorkspacePath: ~user preserved literally", () => {
  assert.equal(expandWorkspacePath("~kimi/foo", workspaceRoot), "~kimi/foo")
})

test("resolveWorkspacePath: canonicalizes aliases for consistent locking", () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-workspace-"))
  const project = join(root, "project")
  mkdirSync(project)
  symlinkSync(project, join(root, "alias"))
  assert.equal(resolveWorkspacePath("project", root), project)
  assert.equal(resolveWorkspacePath("alias", root), project)
})

test("resolveWorkspacePath: rejects traversal and escaping symlinks", () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-workspace-"))
  const outside = mkdtempSync(join(tmpdir(), "kimi-outside-"))
  symlinkSync(outside, join(root, "escape"))
  assert.throws(() => resolveWorkspacePath("..", root), /escapes WORKSPACE_ROOT/)
  assert.throws(() => resolveWorkspacePath("escape", root), /escapes WORKSPACE_ROOT/)
})

console.log("path expand self-check passed")
