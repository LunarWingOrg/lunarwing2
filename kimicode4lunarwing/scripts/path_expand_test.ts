import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { test } from "node:test"
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync, readFileSync } from "node:fs"
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

test("resolveWorkspacePath: in-root '..' prefixed names are not escapes (WI#375 DS-5)", () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-workspace-"))
  // `relative()` yields "..foo" for this in-root child; a startsWith("..")
  // escape predicate misclassifies it and rejects a legitimate path.
  const dotdotPrefixed = join(root, "..foo")
  mkdirSync(dotdotPrefixed)
  assert.equal(resolveWorkspacePath("..foo", root), dotdotPrefixed)
})

test("workspacePathsOverlap: in-root '..' prefixed names overlap (WI#375 DS-5)", () => {
  assert.equal(workspacePathsOverlap("/workspace", "/workspace/..foo"), true)
  assert.equal(workspacePathsOverlap("/workspace/..foo", "/workspace"), true)
  assert.equal(workspacePathsOverlap("/workspace/..foo", "/workspace/..bar"), false)
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

test("cron bootstrap: a non-directory component is permanent and names the component", () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-cron-file-"))
  try {
    writeFileSync(join(root, "missions"), "not a directory")
    const context = { project_dir: "missions/pr-digest", cron_tick: { event: "cron.tick" } }
    // ENOTDIR is in the permanent set (DS-2), so the host abandons the tick
    // instead of retrying; the wrap names the failing component (DS-7).
    assert.throws(
      () => resolveTaskWorkspace(context, root),
      /Invalid workspace path for cron tick:.*component 'missions'.*ENOTDIR/,
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("cron bootstrap: a FIFO component fails fast as permanent (pins O_DIRECTORY)", () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-cron-fifo-"))
  try {
    execFileSync("mkfifo", [join(root, "missions")])
    const context = { project_dir: "missions/x", cron_tick: { event: "cron.tick" } }
    // DS-3: with O_DIRECTORY the FIFO open fails fast with ENOTDIR; without it
    // the blocking FIFO open would hang the bridge event loop (surfacing only
    // via the outer test timeout — that hang is the mutation this pins).
    assert.throws(
      () => resolveTaskWorkspace(context, root),
      /Invalid workspace path for cron tick:.*component 'missions'.*ENOTDIR/,
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// WI#375 DS-3: bounded swap race — an attacker process swaps the parent
// component between a real directory and a symlink to an outside directory
// while the cron bootstrap walks it. A successful resolve must never land
// outside the root, and the outside directory must stay empty.
test("cron bootstrap swap race never escapes the root", () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-swap-root-"))
  const outside = mkdtempSync(join(tmpdir(), "kimi-swap-outside-"))
  const linkPath = join(root, "missions")
  mkdirSync(linkPath)
  // Plain JS attacker source (runs under node or bun): alternate missions
  // between a symlink to the outside dir and a real dir as fast as possible.
  const attackerPath = join(root, "swap-attacker.cjs")
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
  const attacker = spawn(process.execPath, [attackerPath, linkPath, outside], { stdio: "ignore" })
  try {
    const context = { project_dir: "missions/x", cron_tick: { event: "cron.tick" } }
    const realRoot = realpathSync(root)
    let succeeded = 0
    for (let i = 0; i < 2000; i++) {
      try {
        const resolved = realpathSync(resolveTaskWorkspace(context, root))
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
    assert.deepEqual(readdirSync(outside), [])
  } finally {
    attacker.kill("SIGKILL")
    rmSync(root, { recursive: true, force: true })
    rmSync(outside, { recursive: true, force: true })
  }
})
