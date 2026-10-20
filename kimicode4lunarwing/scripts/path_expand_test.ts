import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { test } from "node:test"
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync, readFileSync } from "node:fs"
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

test("cron bootstrap hands a newly created leaf to the task identity (WI#400 KW-4)", () => {
  // The bridge runs as root while the cron task runs under the separate task
  // uid; a freshly created leaf must be fchowned to the task identity or the
  // task cannot write its own project dir. fchown to the process's own ids
  // is permitted unprivileged, so this runs on dev hosts and in-image alike.
  const uid = process.getuid!()
  const gid = process.getgid!()
  process.env.KIMI_TASK_UID = String(uid)
  process.env.KIMI_TASK_GID = String(gid)
  const root = mkdtempSync(join(tmpdir(), "kimi-cron-kw4-"))
  try {
    const context = { project_dir: "missions/kw4", cron_tick: { event: "cron.tick" } }
    const dir = resolveTaskWorkspace(context, root)
    const landed = statSync(dir)
    assert.equal(landed.uid, uid, "new leaf must be owned by the task uid")
    assert.equal(landed.gid, gid, "new leaf must be owned by the task gid")
    // A pre-existing leaf is never re-chowned: the second tick takes the
    // EEXIST path and leaves the identity untouched.
    assert.equal(resolveTaskWorkspace(context, root), dir)
  } finally {
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    rmSync(root, { recursive: true, force: true })
  }
})

test("cron bootstrap hands created intermediates to the task identity too (WI#400 LD-6, root)", () => {
  // Root-only: the handoff targets a foreign uid, so only the root shape
  // (the CI lane and the in-image gate) can observe real ownership change.
  // A created INTERMEDIATE must be task-owned or the task cannot create
  // siblings under it; a pre-existing component keeps its owner.
  if (process.getuid!() !== 0) return
  const TASK_ID = 65534
  process.env.KIMI_TASK_UID = String(TASK_ID)
  process.env.KIMI_TASK_GID = String(TASK_ID)
  const root = mkdtempSync(join(tmpdir(), "kimi-cron-ld6-"))
  try {
    mkdirSync(join(root, "pre"), { mode: 0o755 })
    const dir = resolveTaskWorkspace(
      { project_dir: "pre/missions/kw4", cron_tick: { event: "cron.tick" } }, root)
    assert.equal(statSync(join(root, "pre")).uid, 0, "pre-existing component keeps its owner")
    assert.equal(statSync(join(root, "pre", "missions")).uid, TASK_ID,
      "created intermediate must be owned by the task uid")
    assert.equal(statSync(dir).uid, TASK_ID, "new leaf must be owned by the task uid")
  } finally {
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    rmSync(root, { recursive: true, force: true })
  }
})

test("cron bootstrap with a foreign task identity fails closed (unprivileged)", () => {
  // The non-vacuous half: without the KW-4 fchown this bootstrap would
  // succeed silently. Unprivileged, the handoff to a foreign uid must fail
  // (EPERM → permanent "Invalid" wording). In-image the gate runs as root,
  // where the same handoff legitimately succeeds — the kimi-lane pin there
  // is the previous test.
  if (process.getuid!() === 0) return
  process.env.KIMI_TASK_UID = String(process.getuid!() + 1000)
  process.env.KIMI_TASK_GID = String(process.getgid!() + 1000)
  const root = mkdtempSync(join(tmpdir(), "kimi-cron-kw4-foreign-"))
  try {
    assert.throws(
      () => resolveTaskWorkspace({ project_dir: "missions/kw4", cron_tick: { event: "cron.tick" } }, root),
      /Invalid workspace path/,
    )
  } finally {
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    rmSync(root, { recursive: true, force: true })
  }
})

test("cron bootstrap: a non-directory component is permanent and names the component", () => {  const root = mkdtempSync(join(tmpdir(), "kimi-cron-file-"))
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
    // MR !1669 review IK-4: results are collected outside the try so a bare
    // catch can never swallow the escape assertion, and the returned path is
    // compared as text (it is already canonical — re-resolving it would race
    // the attacker and flake).
    let succeeded = 0
    const escaped: string[] = []
    for (let i = 0; i < 2000; i++) {
      let resolved: string | undefined
      try {
        resolved = resolveTaskWorkspace(context, root)
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
    assert.deepEqual(readdirSync(outside), [])
  } finally {
    attacker.kill("SIGKILL")
    rmSync(root, { recursive: true, force: true })
    rmSync(outside, { recursive: true, force: true })
  }
})

// WI#375 DS-1 / MR !1669 review IK-3: an in-root redirect raced between the
// descriptor walk and the path re-resolve must be refused (fstat(fd) vs
// stat(path) dev/ino compare). The decoy already contains "x", so without
// the compare a raced resolve returns root/decoy/x. The loop runs until the
// race window is hit once (a redirected success or the DS-1 refusal) or the
// time budget runs out, and every check happens outside the catch.
test("cron bootstrap refuses an in-root redirect raced after the walk (DS-1)", () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "kimi-ds1-root-")))
  const linkPath = join(root, "missions")
  const decoy = join(root, "decoy")
  mkdirSync(linkPath)
  mkdirSync(join(decoy, "x"), { recursive: true })
  const attackerPath = join(root, "ds1-attacker.cjs")
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
        resolved = resolveTaskWorkspace(context, root)
      } catch (error) {
        if (String(error).includes("resolved path changed during bootstrap")) refused++
        continue
      }
      if (resolved.startsWith(decoy)) redirected.push(resolved)
    }
  } finally {
    attacker.kill("SIGKILL")
    rmSync(root, { recursive: true, force: true })
  }
  assert.deepEqual(redirected, [])
  // WI#444: the refusal count is a harness liveness signal, not the security
  // property (that is the empty redirected list above); a quiet runner must
  // not fail CI on it.
  if (refused === 0) console.error("DS-1 race window never hit within the time budget (harness liveness only; no redirect was returned)")
})

// WI#444: deterministic DS-1 pin — the injected hook fires between the
// descriptor walk and the re-resolve and swaps `missions` for a symlink to
// the decoy (which already contains "x"), so the dev/ino re-check must
// refuse on every run with no race window. Mutation target: removing the
// dev/ino compare returns the decoy path instead of throwing.
test("cron bootstrap refuses a hooked in-root redirect deterministically (WI#444)", () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "kimi-ds1-hook-")))
  const linkPath = join(root, "missions")
  const decoy = join(root, "decoy")
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
      () => resolveTaskWorkspace(context, root, { beforeReresolve: hook }),
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
    rmSync(root, { recursive: true, force: true })
  }
})
