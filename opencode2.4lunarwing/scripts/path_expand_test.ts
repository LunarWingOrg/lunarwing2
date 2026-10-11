import assert from "node:assert/strict"
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { expandWorkspacePath, resolveWorkspacePath, resolveTaskWorkspace, workspacePathsOverlap } from "./workspace_path"

const workspaceRoot = "/workspace"

assert.equal(expandWorkspacePath("~/a/b", workspaceRoot), "/workspace/a/b")
assert.equal(expandWorkspacePath("~", workspaceRoot), "/workspace")
assert.equal(expandWorkspacePath("/abs", workspaceRoot), "/abs")
assert.equal(expandWorkspacePath("rel", workspaceRoot), "/workspace/rel")
assert.equal(expandWorkspacePath("~opencode/foo", workspaceRoot), "~opencode/foo")

// resolveWorkspacePath canonicalizes and confines against real filesystem paths.
const root = mkdtempSync(join(tmpdir(), "oc2-ws-"))
try {
  const project = join(root, "project")
  mkdirSync(project, { recursive: true })
  const alias = join(root, "alias")
  symlinkSync(project, alias)

  assert.equal(resolveWorkspacePath("project", root), project)
  assert.equal(resolveWorkspacePath("alias", root), project)
  assert.equal(resolveWorkspacePath("~", root), root)
  assert.throws(() => resolveWorkspacePath("..", root), /escapes WORKSPACE_ROOT/)
  const escape = join(root, "escape")
  symlinkSync(tmpdir(), escape)
  assert.throws(() => resolveWorkspacePath("escape", root), /escapes WORKSPACE_ROOT/)
  const cron = { project_dir: "missions/default", cron_tick: { event: "cron.tick" } }
  assert.throws(() => resolveTaskWorkspace({ project_dir: cron.project_dir }, root), /ENOENT/)
  assert.equal(resolveTaskWorkspace(cron, root), join(root, cron.project_dir))
  assert.equal(resolveTaskWorkspace(cron, root), join(root, cron.project_dir))
  for (const project_dir of ["../bad", "/tmp/bad", "~", "missions/../bad", "escape/cron-escape", "alias/new"]) {
    assert.throws(() => resolveTaskWorkspace({ ...cron, project_dir }, root), /Invalid workspace path/)
  }
  assert.equal(existsSync(join(project, "new")), false)
  // DS-3: assert the actual escape target was not created, not just a decoy.
  assert.equal(existsSync(join(tmpdir(), "cron-escape")), false)
  // DS-2/DS-7: a non-directory component is a permanent failure naming the component.
  const fileRoot = mkdtempSync(join(tmpdir(), "oc2-cron-file-"))
  try {
    writeFileSync(join(fileRoot, "missions"), "not a directory")
    assert.throws(
      () => resolveTaskWorkspace({ project_dir: "missions/x", cron_tick: { event: "cron.tick" } }, fileRoot),
      /Invalid workspace path for cron tick:.*component 'missions'.*ENOTDIR/,
    )
  } finally {
    rmSync(fileRoot, { recursive: true, force: true })
  }
} finally {
  rmSync(root, { recursive: true, force: true })
}

assert.equal(workspacePathsOverlap("/workspace/project", "/workspace/project"), true)
assert.equal(workspacePathsOverlap("/workspace/project", "/workspace/project/src"), true)
assert.equal(workspacePathsOverlap("/workspace/project/src", "/workspace/project"), true)
assert.equal(workspacePathsOverlap("/workspace/project-a", "/workspace/project-b"), false)

console.log("workspace path expansion self-check passed")
