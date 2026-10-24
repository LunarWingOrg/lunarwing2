import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { expandWorkspacePath, isNonRetryableBootstrapErrno, resolveTaskWorkspace } from "./workspace_path"

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

// WI#400 run-164 KW-4: the bridge runs as root while the cron task runs
// under the separate task uid; a freshly created leaf must be fchowned to
// the task identity or the task cannot write its own project dir. LP-17
// refuses a zero task id, so a root runner (CI) hands the leaf to a fixed
// unprivileged identity instead of its own; root may fchown to any id. A
// non-root runner keeps its own ids (fchown to itself is permitted
// unprivileged), so this runs on dev hosts and in-image alike.
{
  const uid = process.getuid!() === 0 ? 10002 : process.getuid!()
  const gid = process.getgid!() === 0 ? 10002 : process.getgid!()
  process.env.OPENCODE_TASK_UID = String(uid)
  process.env.OPENCODE_TASK_GID = String(gid)
  const kw4Root = mkdtempSync(join(tmpdir(), "oc-cron-kw4-"))
  try {
    const context = { project_dir: "missions/kw4", cron_tick: { event: "cron.tick" } }
    const dir = resolveTaskWorkspace(context, kw4Root)
    const landed = statSync(dir)
    assert.equal(landed.uid, uid, "new leaf must be owned by the task uid")
    assert.equal(landed.gid, gid, "new leaf must be owned by the task gid")
    // A pre-existing leaf is never re-chowned: the second tick takes the
    // EEXIST path and leaves the identity untouched.
    assert.equal(resolveTaskWorkspace(context, kw4Root), dir)
  } finally {
    delete process.env.OPENCODE_TASK_UID
    delete process.env.OPENCODE_TASK_GID
    rmSync(kw4Root, { recursive: true, force: true })
  }
}

// WI#400 LD-6 (root-only): a created INTERMEDIATE must be handed to the
// task identity as well, or the task cannot create siblings under it; a
// pre-existing component keeps its owner. The handoff targets a foreign
// uid, so only the root shape (CI lane and in-image gate) can observe it.
if (process.getuid!() === 0) {
  const TASK_ID = 65534
  process.env.OPENCODE_TASK_UID = String(TASK_ID)
  process.env.OPENCODE_TASK_GID = String(TASK_ID)
  const ld6Root = mkdtempSync(join(tmpdir(), "oc-cron-ld6-"))
  try {
    mkdirSync(join(ld6Root, "pre"), { mode: 0o755 })
    const dir = resolveTaskWorkspace(
      { project_dir: "pre/missions/kw4", cron_tick: { event: "cron.tick" } }, ld6Root)
    assert.equal(statSync(join(ld6Root, "pre")).uid, 0, "pre-existing component keeps its owner")
    assert.equal(statSync(join(ld6Root, "pre", "missions")).uid, TASK_ID,
      "created intermediate must be owned by the task uid")
    assert.equal(statSync(dir).uid, TASK_ID, "new leaf must be owned by the task uid")
  } finally {
    delete process.env.OPENCODE_TASK_UID
    delete process.env.OPENCODE_TASK_GID
    rmSync(ld6Root, { recursive: true, force: true })
  }
}

// The non-vacuous half: without the KW-4 fchown this bootstrap would succeed
// silently. Unprivileged, the handoff to a foreign uid must fail (EPERM →
// permanent "Invalid" wording). In-image the gate runs as root, where the
// same handoff legitimately succeeds (covered by the previous block).
if (process.getuid!() !== 0) {
  process.env.OPENCODE_TASK_UID = String(process.getuid!() + 1000)
  process.env.OPENCODE_TASK_GID = String(process.getgid!() + 1000)
  const kw4ForeignRoot = mkdtempSync(join(tmpdir(), "oc-cron-kw4-foreign-"))
  try {
    assert.throws(
      () => resolveTaskWorkspace({ project_dir: "missions/kw4", cron_tick: { event: "cron.tick" } }, kw4ForeignRoot),
      /Invalid workspace path/,
    )
  } finally {
    delete process.env.OPENCODE_TASK_UID
    delete process.env.OPENCODE_TASK_GID
    rmSync(kw4ForeignRoot, { recursive: true, force: true })
  }
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
  const hookRoot = realpathSync(mkdtempSync(join(tmpdir(), "oc-ds1-hook-")))
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

// WI#449 LP-16 (run-170): EINVAL is permanent, not retried. Its only source
// in this bootstrap is fchown on an unmapped uid, which no re-dispatch can
// fix — retrying it forever would strand the cron tick. Pinned through the
// classifier because a host user namespace maps every id the suite can drop
// to, so the fchown path itself cannot be made to fail with EINVAL here.
assert.equal(isNonRetryableBootstrapErrno("EINVAL"), true, "EINVAL must be permanent (LP-16)")
for (const code of ["ENOTDIR", "ELOOP", "EACCES", "EPERM"]) {
  assert.equal(isNonRetryableBootstrapErrno(code), true, `${code} must stay permanent (DS-2)`)
}
for (const code of ["EMFILE", "ENFILE", "ENOMEM", "ENOSPC", "EDQUOT", "EIO", "ENOENT"]) {
  assert.equal(isNonRetryableBootstrapErrno(code), false, `${code} must stay transient (DS-2)`)
}
assert.equal(isNonRetryableBootstrapErrno(undefined), false, "an errno-less failure is transient")
