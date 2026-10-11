import assert from "node:assert/strict"
import { existsSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { expandWorkspacePath, resolveTaskWorkspace } from "./workspace_path"

const workspaceRoot = "/workspace"

assert.equal(expandWorkspacePath("~/a/b", workspaceRoot), "/workspace/a/b")
assert.equal(expandWorkspacePath("~", workspaceRoot), "/workspace")
assert.equal(expandWorkspacePath("/abs", workspaceRoot), "/abs")
assert.equal(expandWorkspacePath("rel", workspaceRoot), "/workspace/rel")
assert.equal(expandWorkspacePath("~opencode/foo", workspaceRoot), "~opencode/foo")

console.log("workspace path expansion self-check passed")

const root = mkdtempSync(join(tmpdir(), "oc-cron-"))
const outside = mkdtempSync(join(tmpdir(), "oc-cron-outside-"))
try {
  const context = { project_dir: "missions/default", cron_tick: { event: "cron.tick" } }
  resolveTaskWorkspace({ project_dir: context.project_dir }, root)
  assert.equal(existsSync(join(root, "missions")), false)
  assert.equal(resolveTaskWorkspace(context, root), join(root, context.project_dir))
  assert.equal(resolveTaskWorkspace(context, root), join(root, context.project_dir))
  symlinkSync(outside, join(root, "escape"))
  for (const project_dir of ["../bad", "/tmp/bad", "~", "missions/../bad", "escape/new"]) {
    assert.throws(() => resolveTaskWorkspace({ ...context, project_dir }, root), /Invalid workspace path/)
  }
  assert.equal(existsSync(join(outside, "new")), false)
  // DS-2/DS-7: a non-directory component is a permanent failure naming the component.
  const fileRoot = mkdtempSync(join(tmpdir(), "oc-cron-file-"))
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
  rmSync(outside, { recursive: true, force: true })
}
