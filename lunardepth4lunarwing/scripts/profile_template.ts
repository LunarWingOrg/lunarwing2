import { chmodSync, cpSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { pathToFileURL } from "node:url"

import { assertProfileSchema } from "./effective_config_assert.js"

interface AppBoot {
  initProfile: (directory: string, bundles: readonly string[]) => void
  resolveProfileDir: (name: string, home: string) => string
  PROFILE_TEMPLATES: Record<string, { bundles: readonly string[] }>
  PROFILE_SCHEMA_VERSION?: number
}

async function main(): Promise<void> {
  const closure = resolve(process.argv[2] || "/opt/lunardepth")
  const target = resolve(process.argv[3] || "/opt/lunardepth-templates")
  const modulePath = join(closure, "node_modules", "@lunardepth", "dsh-app-boot", "lib", "index.js")
  const boot = await import(pathToFileURL(modulePath).href) as AppBoot
  const profile = boot.PROFILE_TEMPLATES["lunarwing-worker"]
  if (profile === undefined) throw new Error("pinned source has no lunarwing-worker profile template")
  rmSync(target, { recursive: true, force: true })
  mkdirSync(target, { recursive: true, mode: 0o755 })
  const templateHome = join(target, "home")
  const directory = boot.resolveProfileDir("lunarwing-worker", templateHome)
  boot.initProfile(directory, profile.bundles)
  assertProfileSchema(join(directory, "package.json"), boot.PROFILE_SCHEMA_VERSION)
  const destination = join(target, "profiles", "lunarwing-worker")
  cpSync(directory, destination, { recursive: true, dereference: true })
  rmSync(templateHome, { recursive: true, force: true })
  for (const path of [target, join(target, "profiles"), destination]) chmodSync(path, 0o555)
  for (const name of ["package.json", "cordis.patch.yml", "pnpm-workspace.yaml"]) {
    chmodSync(join(destination, name), 0o444)
  }
  const schema = boot.PROFILE_SCHEMA_VERSION ?? "unversioned"
  writeFileSync(join(target, "profile-schema.json"), `${JSON.stringify({ profileSchemaVersion: schema })}\n`, { mode: 0o444 })
  chmodSync(join(target, "profile-schema.json"), 0o444)
  process.stdout.write(`${schema}\n`)
}

if (process.argv[1]?.endsWith("profile_template.js") === true) {
  void main().catch(error => {
    console.error(`[lunardepth-profile] ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  })
}
