import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
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
    // DS-3: a FIFO component fails fast with ENOTDIR via O_DIRECTORY; without
    // it the blocking FIFO open would hang (outer timeout is the only tell).
    execFileSync("mkfifo", [join(fileRoot, "fifo")])
    assert.throws(
      () => resolveTaskWorkspace({ project_dir: "fifo/x", cron_tick: { event: "cron.tick" } }, fileRoot),
      /Invalid workspace path for cron tick:.*component 'fifo'.*ENOTDIR/,
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

// WI#375 DS-5: in-root names starting with ".." are not path escapes —
// `relative()` yields "..foo" for such a child, and a startsWith("..")
// predicate misclassifies it as an escape.
{
  const ds5Root = mkdtempSync(join(tmpdir(), "opencode2-workspace-"))
  const dotdotPrefixed = join(ds5Root, "..foo")
  mkdirSync(dotdotPrefixed)
  assert.equal(resolveWorkspacePath("..foo", ds5Root), dotdotPrefixed)
  rmSync(ds5Root, { recursive: true, force: true })
}
assert.equal(workspacePathsOverlap("/workspace", "/workspace/..foo"), true)
assert.equal(workspacePathsOverlap("/workspace/..foo", "/workspace"), true)
assert.equal(workspacePathsOverlap("/workspace/..foo", "/workspace/..bar"), false)

// WI#375 DS-3: bounded swap race — an attacker process swaps the parent
// component between a real directory and a symlink to an outside directory
// while the cron bootstrap walks it. A successful resolve must never land
// outside the root, and the outside directory must stay empty.
{
  const swapRoot = mkdtempSync(join(tmpdir(), "opencode2-swap-root-"))
  const swapOutside = mkdtempSync(join(tmpdir(), "opencode2-swap-outside-"))
  const linkPath = join(swapRoot, "missions")
  mkdirSync(linkPath)
  const attackerPath = join(swapRoot, "swap-attacker.cjs")
  writeFileSync(attackerPath, `
const fs = require("fs")
const [linkPath, outside] = process.argv.slice(2)
for (;;) {
  try { fs.rmSync(linkPath, { recursive: true, force: true }) } catch {}
  try { fs.symlinkSync(outside, linkPath) } catch {}
  try { fs.rmSync(linkPath, { recursive: true, force: true }) } catch {}
  try { fs.mkdirSync(linkPath) } catch {}
}
`)
  const attacker = spawn(process.execPath, [attackerPath, linkPath, swapOutside], { stdio: "ignore" })
  try {
    const context = { project_dir: "missions/x", cron_tick: { event: "cron.tick" } }
    const realRoot = realpathSync(swapRoot)
    let succeeded = 0
    for (let i = 0; i < 2000; i++) {
      try {
        const resolved = realpathSync(resolveTaskWorkspace(context, swapRoot))
        assert.ok(
          resolved === realRoot || resolved.startsWith(realRoot + "/"),
          `resolved path escaped the root: ${resolved}`,
        )
        succeeded++
      } catch {
        // Swap races legitimately fail (ELOOP/ENOENT/redirect mismatch); the
        // invariant is that a success never lands outside the root.
      }
      if (i % 50 === 49) {
        // Keep the name swap live (the walk recreates x on demand), but
        // never delete through a symlink.
        try {
          const st = lstatSync(linkPath)
          if (st.isDirectory() && !st.isSymbolicLink()) rmSync(join(linkPath, "x"), { force: true })
        } catch {}
      }
    }
    assert.ok(succeeded > 0, "harness never completed a bootstrap — vacuous")
    assert.deepEqual(readdirSync(swapOutside), [])
  } finally {
    attacker.kill("SIGKILL")
    rmSync(swapRoot, { recursive: true, force: true })
    rmSync(swapOutside, { recursive: true, force: true })
  }
}

console.log("workspace path expansion self-check passed")
