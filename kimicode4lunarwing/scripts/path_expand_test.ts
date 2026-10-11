import assert from "node:assert/strict"
import { test } from "node:test"
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync, readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { expandWorkspacePath, resolveWorkspacePath, resolveTaskWorkspace, workspacePathsOverlap } from "./workspace_path.js"

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

test("workspacePathsOverlap: rejects equal and parent-child workspaces only", () => {
  assert.equal(workspacePathsOverlap("/workspace/project", "/workspace/project"), true)
  assert.equal(workspacePathsOverlap("/workspace/project", "/workspace/project/src"), true)
  assert.equal(workspacePathsOverlap("/workspace/project/src", "/workspace/project"), true)
  assert.equal(workspacePathsOverlap("/workspace/project-a", "/workspace/project-b"), false)
})

console.log("path expand self-check passed")

test("cron bootstrap creates a missing default dir, preserves state, and confines all writes", () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-cron-"))
  const outside = mkdtempSync(join(tmpdir(), "kimi-cron-outside-"))
  try {
    const context = { project_dir: "missions/pr-digest", cron_tick: { event: "cron.tick" } }
    assert.throws(() => resolveTaskWorkspace({ project_dir: context.project_dir }, root), /ENOENT/)
    assert.equal(existsSync(join(root, "missions")), false)
    const dir = resolveTaskWorkspace(context, root)
    assert.equal(dir, join(root, context.project_dir))
    writeFileSync(join(dir, "state"), "tick one")
    assert.equal(resolveTaskWorkspace(context, root), dir)
    assert.equal(readFileSync(join(dir, "state"), "utf8"), "tick one")
    symlinkSync(outside, join(root, "escape"))
    symlinkSync(join(root, "missing"), join(root, "dangling"))
    for (const project_dir of ["../outside/new", "/tmp/new", "~", "missions/../bad", "escape/new", "dangling/new"]) {
      assert.throws(() => resolveTaskWorkspace({ ...context, project_dir }, root), /Invalid workspace path/)
    }
    assert.equal(existsSync(join(outside, "new")), false)
    assert.equal(existsSync(join(root, "missing")), false)
  } finally {
    rmSync(root, { recursive: true, force: true })
    rmSync(outside, { recursive: true, force: true })
  }
})
