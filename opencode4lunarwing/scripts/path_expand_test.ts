import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
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
  rmSync(outside, { recursive: true, force: true })
}

// WI#375 DS-3: bounded swap race — an attacker process swaps the parent
// component between a real directory and a symlink to an outside directory
// while the cron bootstrap walks it. A successful resolve must never land
// outside the root, and the outside directory must stay empty.
{
  const swapRoot = mkdtempSync(join(tmpdir(), "oc-swap-root-"))
  const swapOutside = mkdtempSync(join(tmpdir(), "oc-swap-outside-"))
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
    // MR !1669 review IK-4: results are collected outside the try so a bare
    // catch can never swallow the escape assertion, and the returned path is
    // compared as text (it is already canonical — re-resolving it would race
    // the attacker and flake).
    let succeeded = 0
    const escaped: string[] = []
    for (let i = 0; i < 2000; i++) {
      let resolved: string | undefined
      try {
        resolved = resolveTaskWorkspace(context, swapRoot)
      } catch {
        // Swap races legitimately fail (ELOOP/ENOENT/redirect mismatch); the
        // invariant is that a success never lands outside the root.
      }
      if (resolved !== undefined) {
        if (resolved === realRoot || resolved.startsWith(realRoot + "/")) succeeded++
        else escaped.push(resolved)
      }
    }
    assert.deepEqual(escaped, [])
    assert.ok(succeeded > 0, "harness never completed a bootstrap — vacuous")
    assert.deepEqual(readdirSync(swapOutside), [])
  } finally {
    attacker.kill("SIGKILL")
    rmSync(swapRoot, { recursive: true, force: true })
    rmSync(swapOutside, { recursive: true, force: true })
  }
}

// WI#375 DS-1 / MR !1669 review IK-3: an in-root redirect raced between the
// descriptor walk and the path re-resolve must be refused (fstat(fd) vs
// stat(path) dev/ino compare). The decoy already contains "x", so without
// the compare a raced resolve returns root/decoy/x. The loop runs until the
// race window is hit once (a redirected success or the DS-1 refusal) or the
// time budget runs out, and every check happens outside the catch.
{
  const ds1Root = realpathSync(mkdtempSync(join(tmpdir(), "oc-ds1-root-")))
  const linkPath = join(ds1Root, "missions")
  const decoy = join(ds1Root, "decoy")
  mkdirSync(linkPath)
  mkdirSync(join(decoy, "x"), { recursive: true })
  const attackerPath = join(ds1Root, "ds1-attacker.cjs")
  writeFileSync(attackerPath, `
const fs = require("fs")
const [linkPath, decoy] = process.argv.slice(2)
for (;;) {
  try { fs.rmSync(linkPath, { recursive: true, force: true }) } catch {}
  try { fs.symlinkSync(decoy, linkPath) } catch {}
  try { fs.rmSync(linkPath, { recursive: true, force: true }) } catch {}
  try { fs.mkdirSync(linkPath) } catch {}
}
`)
  const attacker = spawn(process.execPath, [attackerPath, linkPath, decoy], { stdio: "ignore" })
  const redirected: string[] = []
  let refused = 0
  try {
    const deadline = Date.now() + 30000
    while (Date.now() < deadline) {
      try { if (lstatSync(linkPath).isSymbolicLink()) break } catch {}
    }
    const context = { project_dir: "missions/x", cron_tick: { event: "cron.tick" } }
    while (Date.now() < deadline && redirected.length === 0 && refused === 0) {
      let resolved = ""
      try {
        resolved = resolveTaskWorkspace(context, ds1Root)
      } catch (error) {
        if (String(error).includes("resolved path changed during bootstrap")) refused++
        continue
      }
      if (resolved.startsWith(decoy)) redirected.push(resolved)
    }
  } finally {
    attacker.kill("SIGKILL")
    rmSync(ds1Root, { recursive: true, force: true })
  }
  assert.deepEqual(redirected, [])
  assert.ok(refused > 0, "race window never hit within the time budget")
}
