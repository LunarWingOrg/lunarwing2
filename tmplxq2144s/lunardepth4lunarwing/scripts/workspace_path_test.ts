import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, rmSync, symlinkSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

import { mapWorkspaceInput, resolveWorkspacePath, workspacePathsOverlap } from "./workspace_path.js"

test("maps daemon host paths and relative paths into the workspace", () => {
  assert.equal(mapWorkspaceInput("/srv/tenant/project", "/workspace", "/srv/tenant"), "/workspace/project")
  assert.equal(mapWorkspaceInput("project", "/workspace"), "/workspace/project")
  assert.equal(mapWorkspaceInput("~/project", "/workspace"), "/workspace/project")
  assert.equal(mapWorkspaceInput(undefined, "/workspace"), "/workspace")
})

test("resolves real project directories and detects overlaps", () => {
  const root = mkdtempSync(join(tmpdir(), "lunardepth-workspace-"))
  try {
    mkdirSync(join(root, "project"))
    mkdirSync(join(root, "project", "child"))
    mkdirSync(join(root, "other"))
    const project = resolveWorkspacePath("project", root)
    const child = resolveWorkspacePath("project/child", root)
    const other = resolveWorkspacePath("other", root)
    assert.equal(workspacePathsOverlap(project, child), true)
    assert.equal(workspacePathsOverlap(project, other), false)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("rejects absolute and symlink escapes", () => {
  const root = mkdtempSync(join(tmpdir(), "lunardepth-workspace-"))
  const outside = mkdtempSync(join(tmpdir(), "lunardepth-outside-"))
  try {
    assert.throws(() => resolveWorkspacePath(outside, root), /escapes WORKSPACE_ROOT/)
    symlinkSync(outside, join(root, "escape"))
    assert.throws(() => resolveWorkspacePath("escape", root), /escapes WORKSPACE_ROOT/)
    assert.throws(() => resolveWorkspacePath("../outside", root), /escapes WORKSPACE_ROOT|ENOENT/)
  } finally {
    rmSync(root, { recursive: true, force: true })
    rmSync(outside, { recursive: true, force: true })
  }
})
