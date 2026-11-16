import assert from "node:assert/strict"
import { mkdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

import { verifyDeployedClosure } from "./verify_deployed_closure.js"

function packageJson(root: string, relative: string, name: string, version: string): void {
  const directory = join(root, relative)
  mkdirSync(directory, { recursive: true })
  writeFileSync(join(directory, "package.json"), `${JSON.stringify({ name, version })}\n`)
}

function fixture(): { root: string; source: string; closure: string; lock: object } {
  const root = join(tmpdir(), `lunardepth-deploy-${process.pid}-${Math.random().toString(16).slice(2)}`)
  const source = join(root, "source")
  const closure = join(root, "closure")
  mkdirSync(source, { recursive: true })
  mkdirSync(closure, { recursive: true })
  packageJson(source, "packages/app", "@lunardepth/dsh-app", "0.2.1-alpha.1")
  packageJson(closure, "node_modules/@lunardepth/dsh-app", "@lunardepth/dsh-app", "0.2.1-alpha.1")
  packageJson(closure, "node_modules/@aws-sdk/nested-clients", "@aws-sdk/nested-clients", "3.997.46")
  packageJson(closure, "node_modules/.pnpm/is-number@7.0.0/node_modules/is-number", "is-number", "7.0.0")
  packageJson(closure, "node_modules/string-width", "string-width", "4.2.3")
  const lock = {
    importers: { "packages/app": {} },
    packages: { "/@aws-sdk/nested-clients@3.997.46": {}, "is-number@7.0.0": {} },
    snapshots: { "string-width-cjs@npm:string-width@4.2.3": {} },
  }
  return { root, source, closure, lock }
}

test("accepts only lockfile and importer package versions with a stable inventory", () => {
  const { root, source, closure, lock } = fixture()
  try {
    assert.deepEqual(verifyDeployedClosure(source, closure, lock), [
      "@aws-sdk/nested-clients@3.997.46",
      "@lunardepth/dsh-app@0.2.1-alpha.1",
      "is-number@7.0.0",
      "string-width@4.2.3",
    ])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("rejects a deployed registry version absent from the lockfile", () => {
  const { root, source, closure, lock } = fixture()
  try {
    packageJson(closure, "node_modules/@aws-sdk/nested-clients", "@aws-sdk/nested-clients", "3.998.3")
    assert.throws(
      () => verifyDeployedClosure(source, closure, lock),
      /@aws-sdk\/nested-clients@3\.998\.3.*pnpm-lock\.yaml|pnpm-lock\.yaml.*@aws-sdk\/nested-clients@3\.998\.3/,
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
