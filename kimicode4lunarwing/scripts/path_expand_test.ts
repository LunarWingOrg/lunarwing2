import assert from "node:assert/strict"
import { test } from "node:test"
import { expandWorkspacePath } from "./workspace_path.js"

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

console.log("path expand self-check passed")
