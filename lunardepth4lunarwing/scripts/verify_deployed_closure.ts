import { createHash } from "node:crypto"
import { existsSync, readFileSync, readdirSync } from "node:fs"
import { createRequire } from "node:module"
import { join } from "node:path"

interface PackageManifest {
  name?: unknown
  version?: unknown
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function packageManifest(path: string): Required<PackageManifest> {
  const value: unknown = JSON.parse(readFileSync(path, "utf8"))
  if (!isRecord(value) || typeof value.name !== "string" || typeof value.version !== "string") {
    throw new Error(`${path}: installed package has no name and version`)
  }
  return { name: value.name, version: value.version }
}

function addLockedKey(key: string, allowed: Set<string>): void {
  const normalized = key.replace(/^\//, "").split("(", 1)[0]
  const separator = normalized.startsWith("@") ? normalized.indexOf("@", 1) : normalized.indexOf("@")
  if (separator <= 0) return
  const name = normalized.slice(0, separator)
  const specifier = normalized.slice(separator + 1)
  if (specifier.startsWith("npm:")) {
    addLockedKey(specifier.slice(4), allowed)
    return
  }
  if (/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(specifier)) {
    allowed.add(`${name}@${specifier}`)
  }
}

export function lockedPackagePairs(source: string, lock: unknown): Set<string> {
  if (!isRecord(lock)) throw new Error("pnpm-lock.yaml is not an object")
  const allowed = new Set<string>()
  for (const section of ["packages", "snapshots"]) {
    const entries = lock[section]
    if (entries !== undefined && !isRecord(entries)) throw new Error(`pnpm-lock.yaml ${section} is not an object`)
    for (const key of Object.keys(entries ?? {})) addLockedKey(key, allowed)
  }
  if (!isRecord(lock.importers)) throw new Error("pnpm-lock.yaml importers is not an object")
  for (const importer of Object.keys(lock.importers)) {
    const path = join(source, importer, "package.json")
    if (!existsSync(path)) throw new Error(`pnpm-lock.yaml importer is missing: ${importer}`)
    const manifest = packageManifest(path)
    allowed.add(`${manifest.name}@${manifest.version}`)
  }
  return allowed
}

function addInstalledPackage(directory: string, installed: Set<string>): void {
  const manifestPath = join(directory, "package.json")
  if (!existsSync(manifestPath)) throw new Error(`installed package manifest is missing: ${manifestPath}`)
  const manifest = packageManifest(manifestPath)
  installed.add(`${manifest.name}@${manifest.version}`)
  scanNodeModules(join(directory, "node_modules"), installed)
}

function scanNodeModules(directory: string, installed: Set<string>): void {
  if (!existsSync(directory)) return
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.name === ".bin" || !entry.isDirectory()) continue
    const path = join(directory, entry.name)
    if (entry.name === ".pnpm") {
      for (const variant of readdirSync(path, { withFileTypes: true })) {
        if (variant.isDirectory()) scanNodeModules(join(path, variant.name, "node_modules"), installed)
      }
    } else if (entry.name.startsWith("@")) {
      for (const scoped of readdirSync(path, { withFileTypes: true })) {
        if (scoped.isDirectory()) addInstalledPackage(join(path, scoped.name), installed)
      }
    } else if (!entry.name.startsWith(".")) {
      addInstalledPackage(path, installed)
    }
  }
}

function readLockfile(source: string): unknown {
  const requireFromSource = createRequire(join(source, "package.json"))
  const yaml = requireFromSource("js-yaml") as { load: (text: string) => unknown }
  return yaml.load(readFileSync(join(source, "pnpm-lock.yaml"), "utf8"))
}

export function verifyDeployedClosure(source: string, closure: string, lock?: unknown): string[] {
  const allowed = lockedPackagePairs(source, lock ?? readLockfile(source))
  const installed = new Set<string>()
  scanNodeModules(join(closure, "node_modules"), installed)
  if (installed.size === 0) throw new Error(`${closure}: deployed package inventory is empty`)
  const unexpected = [...installed].filter(pair => !allowed.has(pair)).sort()
  if (unexpected.length > 0) {
    throw new Error(`deployed versions absent from pnpm-lock.yaml: ${unexpected.join(", ")}`)
  }
  return [...installed].sort()
}

function main(): void {
  const source = process.argv[2]
  const closure = process.argv[3]
  if (!source || !closure) throw new Error("usage: verify_deployed_closure.ts <source-root> <deploy-root>")
  const inventory = verifyDeployedClosure(source, closure)
  const digest = createHash("sha256").update(`${inventory.join("\n")}\n`).digest("hex")
  process.stdout.write(`${digest}\n`)
}

if (/verify_deployed_closure\.[jt]s$/.test(process.argv[1] ?? "")) {
  try {
    main()
  } catch (error) {
    console.error(`[lunardepth-deploy] ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  }
}
