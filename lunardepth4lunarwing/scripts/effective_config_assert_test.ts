import assert from "node:assert/strict"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

import {
  assertCompositionRows,
  assertLaunchEnvironment,
  assertManifestHash,
  assertProfileSchema,
  EXPECTED_ROW_IDS,
  manifestSha256,
} from "./effective_config_assert.js"

const validLaunchEnvironment: NodeJS.ProcessEnv = {
  HOME: "/home/lunardepth-home",
  DSH_HOME: "/home/lunardepth/.dsh",
  TMPDIR: "/tmp",
  TMP: "/tmp",
  TEMP: "/tmp",
  WS_NO_BUFFER_UTIL: "1",
  WS_NO_UTF_8_VALIDATE: "1",
  DSH_TELEMETRY_DISABLED: "1",
  LUNARDEPTH_WS_URL: "ws://127.0.0.1:19091/ws/agent",
  LUNARDEPTH_LOCAL_AUTH_TOKEN: "test-only-local-token",
  PATH: "/usr/local/bin:/usr/bin:/bin",
}

test("launch gate retains the sourced task environment", () => {
  assertLaunchEnvironment(validLaunchEnvironment)
  assert.throws(
    () => assertLaunchEnvironment({ ...validLaunchEnvironment, HOME: "/home/lunardepth" }),
    /launch environment drift: HOME/,
  )
  assert.throws(
    () => assertLaunchEnvironment({ ...validLaunchEnvironment, LUNARDEPTH_LOCAL_AUTH_TOKEN: undefined }),
    /missing the local credential/,
  )
  assert.throws(
    () => assertLaunchEnvironment({ ...validLaunchEnvironment, PATH: "/usr/bin:" }),
    /PATH contains a relative or empty entry/,
  )
})

function manifest(directory: string, schemaVersion?: number): string {
  const path = join(directory, "package.json")
  writeFileSync(path, `${JSON.stringify({
    name: "dsh-profile-lunarwing-worker",
    private: true,
    dependencies: {},
    dsh: { profile: { bundles: [], ...schemaVersion === undefined ? {} : { schemaVersion } } },
  }, null, 2)}\n`)
  return path
}

test("accepts the schema exported by the pin and rejects drift", () => {
  const directory = mkdtempSync(join(tmpdir(), "lunardepth-schema-"))
  try {
    const unversioned = manifest(directory)
    assertProfileSchema(unversioned, undefined)
    assert.throws(() => assertProfileSchema(unversioned, 1), /PROFILE_SCHEMA_VERSION 1/)
    const current = manifest(directory, 1)
    assertProfileSchema(current, 1)
    assert.throws(() => assertProfileSchema(current, 2), /PROFILE_SCHEMA_VERSION 2/)
    assert.throws(() => assertProfileSchema(current, undefined), /unsupported by the pinned source/)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test("manifest hashes detect a launcher rewrite and name package.json", () => {
  const directory = mkdtempSync(join(tmpdir(), "lunardepth-hash-"))
  try {
    const path = manifest(directory)
    const before = manifestSha256(path)
    assertManifestHash(path, before)
    writeFileSync(path, "{}\n")
    assert.throws(() => assertManifestHash(path, before), /package\.json was rewritten/)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test("composition accepts only the exact row inventory and applied security patches", () => {
  const permission = {
    presets: { "workspace-write": { sandbox: "workspace-write", approval: "ask" } },
    defaultPreset: "workspace-write",
  }
  const rows = [...EXPECTED_ROW_IDS].map(id => ({
    id,
    name: "@lunardepth/dsh-test",
    disabled: false,
    config: id === "permission" ? permission : {},
  }))
  assertCompositionRows(rows, [{ id: "permission", name: "@lunardepth/dsh-test", disabled: false, config: permission }])
  assert.throws(
    () => assertCompositionRows(rows, [{ id: "permission", name: "@lunardepth/dsh-test", disabled: true }]),
    /disabled mismatch/,
  )
  assert.throws(() => assertCompositionRows([...rows, { id: "planted", name: "evil", config: {} }], []), /unknown or duplicate/)
  assert.throws(
    () => assertCompositionRows(rows.map(row => row.id === "sandbox" ? { ...row, config: { runnerCommand: ["bwrap"] } } : row), []),
    /runnerCommand override is forbidden/,
  )
  assert.throws(
    () => assertCompositionRows(rows.map(row => row.id === "permission" ? { ...row, config: { mode: "danger-full-access" } } : row), []),
    /unconfined execution value/,
  )
})
