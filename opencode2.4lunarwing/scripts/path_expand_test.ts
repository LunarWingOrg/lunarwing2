import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { expandWorkspacePath, isNonRetryableBootstrapErrno, resolveWorkspacePath, resolveTaskWorkspace, workspacePathsOverlap } from "./workspace_path"

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
  const ds1Root = realpathSync(mkdtempSync(join(tmpdir(), "opencode2-ds1-root-")))
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
  // WI#444: the refusal count is a harness liveness signal, not the security
  // property (that is the empty redirected list above); a quiet runner must
  // not fail CI on it.
  if (refused === 0) console.error("DS-1 race window never hit within the time budget (harness liveness only; no redirect was returned)")
}

// WI#444: deterministic DS-1 pin — the injected hook fires between the
// descriptor walk and the re-resolve and swaps `missions` for a symlink to
// the decoy (which already contains "x"), so the dev/ino re-check must
// refuse on every run with no race window. Mutation target: removing the
// dev/ino compare returns the decoy path instead of throwing.
{
  const hookRoot = realpathSync(mkdtempSync(join(tmpdir(), "oc2-ds1-hook-")))
  const linkPath = join(hookRoot, "missions")
  const decoy = join(hookRoot, "decoy")
  mkdirSync(linkPath)
  mkdirSync(join(decoy, "x"), { recursive: true })
  try {
    let hookFired = 0
    const hook = () => {
      hookFired++
      rmSync(linkPath, { recursive: true, force: true })
      symlinkSync(decoy, linkPath)
    }
    const context = { project_dir: "missions/x", cron_tick: { event: "cron.tick" } }
    assert.throws(
      () => resolveTaskWorkspace(context, hookRoot, { beforeReresolve: hook }),
      (error: unknown) => {
        const message = String(error)
        assert.ok(message.includes("resolved path changed during bootstrap"),
          `expected the dev/ino refusal, got: ${message}`)
        assert.ok(message.includes("Transient workspace error for cron tick"),
          `refusal must classify transient (DS-2), got: ${message}`)
        return true
      },
    )
    assert.equal(hookFired, 1, "hook must fire exactly once")
    // The redirect is never returned: the decoy holds only its planted "x".
    assert.deepEqual(readdirSync(decoy), ["x"])
  } finally {
    rmSync(hookRoot, { recursive: true, force: true })
  }
}


// WI#449 LP-16 (run-170): EINVAL is permanent, not retried — see the v1 lane's
// pin for why the classifier is exercised directly rather than through fchown.
assert.equal(isNonRetryableBootstrapErrno("EINVAL"), true, "EINVAL must be permanent (LP-16)")
for (const code of ["ENOTDIR", "ELOOP", "EACCES", "EPERM"]) {
  assert.equal(isNonRetryableBootstrapErrno(code), true, `${code} must stay permanent (DS-2)`)
}
for (const code of ["EMFILE", "ENFILE", "ENOMEM", "ENOSPC", "EDQUOT", "EIO", "ENOENT"]) {
  assert.equal(isNonRetryableBootstrapErrno(code), false, `${code} must stay transient (DS-2)`)
}
assert.equal(isNonRetryableBootstrapErrno(undefined), false, "an errno-less failure is transient")

console.log("workspace path expansion self-check passed")
