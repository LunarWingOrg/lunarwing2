/**
 * sdk_surface_assert_test.ts — committed negative tests for the image-build
 * gate (WI#369 acceptance): a fixture client missing one bridge method and a
 * fixture V2Event union missing one bridge event MUST fail the gate, both
 * in-process (checkClientSurface / checkBridgeEventsAgainstUnion) and
 * end-to-end through the sdk_surface_assert.ts CLI (env-overridden fixtures,
 * exit code + FATAL lines). Deleting either assertion loop from the CLI
 * makes the matching end-to-end expectation fail. The DF-2 pin: a bridge
 * event still present ELSEWHERE in the file but absent from the V2Event
 * union no longer satisfies the gate.
 */

import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

import {
  BRIDGE_SDK_EVENTS,
  checkBridgeEventsAgainstUnion,
  checkClientSurface,
} from "./sdk_surface.ts"

const ASSERT_SCRIPT = fileURLToPath(new URL("./sdk_surface_assert.ts", import.meta.url))

/** All-six-methods fixture client, shaped like the real promise client. */
const GOOD_CLIENT = {
  session: {
    create: async () => ({}),
    prompt: async () => ({}),
    interrupt: async () => ({}),
  },
  permission: { reply: async () => ({}) },
  event: { subscribe: () => ({}) },
  message: { list: async () => ({}) },
}

/** Missing message.list, and exposing both known-stale paths. */
const BAD_CLIENT = {
  session: {
    create: async () => ({}),
    prompt: async () => ({}),
    interrupt: async () => ({}),
    message: { list: async () => ({}) },
    permission: { reply: async () => ({}) },
  },
  permission: { reply: async () => ({}) },
  event: { subscribe: () => ({}) },
}

/** Fixture client module sources, shaped like the real promise client. */
const GOOD_CLIENT_MODULE = `export const OpenCode = {
  make: (_options: unknown) => ({
    session: {
      create: async () => ({}),
      prompt: async () => ({}),
      interrupt: async () => ({}),
    },
    permission: { reply: async () => ({}) },
    event: { subscribe: () => ({}) },
    message: { list: async () => ({}) },
  }),
}
`

/** Missing message.list, and exposing both known-stale paths. */
const BAD_CLIENT_MODULE = `export const OpenCode = {
  make: (_options: unknown) => ({
    session: {
      create: async () => ({}),
      prompt: async () => ({}),
      interrupt: async () => ({}),
      message: { list: async () => ({}) },
      permission: { reply: async () => ({}) },
    },
    permission: { reply: async () => ({}) },
    event: { subscribe: () => ({}) },
  }),
}
`

const memberName = (event: string) =>
  event
    .split(".")
    .map((part) => part[0]!.toUpperCase() + part.slice(1))
    .join("")

/** A generated-types.ts-shaped fixture whose V2Event union holds `events`. */
function typesSource(events: readonly string[]): string {
  const definitions = events
    .map(
      (event) =>
        `export type ${memberName(event)} = {\n  id: string\n  type: "${event}"\n  data: {}\n}\n`,
    )
    .join("\n")
  const members = events.map((event) => `  | ${memberName(event)}`).join("\n")
  return `${definitions}\nexport type V2Event =\n${members}\n\nexport type EventSubscribeOutput = V2Event\n`
}

// ── in-process: method surface ───────────────────────────────────────────
assert.deepEqual(checkClientSurface(GOOD_CLIENT), [])
{
  const failures = checkClientSurface(BAD_CLIENT)
  assert.ok(
    failures.some((failure) => failure.includes("client.message.list()")),
    "a missing bridge method must be named",
  )
  assert.ok(
    failures.some((failure) => failure.includes("client.session.message.list")),
    "a stale path that starts existing must be named",
  )
  assert.ok(
    failures.some((failure) => failure.includes("client.session.permission.reply")),
    "both stale paths are reported",
  )
}

// ── in-process: V2Event union membership (DF-2) ─────────────────────────
assert.deepEqual(checkBridgeEventsAgainstUnion(typesSource(BRIDGE_SDK_EVENTS)), [])
{
  // "session.idle" is missing from the union but still present elsewhere in
  // the same file — the v1 compat schema pattern (schema/src/v1 carried the
  // literal at v2.0.24). The corpus-wide substring check passed this; the
  // union gate must not.
  const source =
    typesSource(BRIDGE_SDK_EVENTS.filter((event) => event !== "session.idle")) +
    `\n// v1 compat schema still carries "session.idle" as a literal\n`
  assert.ok(source.includes('"session.idle"'), "fixture must keep the stray literal")
  const failures = checkBridgeEventsAgainstUnion(source)
  assert.ok(
    failures.some((failure) => failure.includes('"session.idle"')),
    "a v1-only occurrence must no longer satisfy the event gate",
  )
}

// ── end-to-end through the CLI: pass side and fail side ──────────────────
const fixtureDir = mkdtempSync(join(tmpdir(), "sdk-surface-assert-"))
try {
  const goodClientModule = join(fixtureDir, "good_client.ts")
  const badClientModule = join(fixtureDir, "bad_client.ts")
  const goodTypes = join(fixtureDir, "good_types.ts")
  const badTypes = join(fixtureDir, "bad_types.ts")
  writeFileSync(goodClientModule, GOOD_CLIENT_MODULE)
  writeFileSync(badClientModule, BAD_CLIENT_MODULE)
  writeFileSync(goodTypes, typesSource(BRIDGE_SDK_EVENTS))
  writeFileSync(
    badTypes,
    typesSource(BRIDGE_SDK_EVENTS.filter((event) => event !== "session.idle")),
  )

  const runGate = (clientModule: string, types: string) =>
    spawnSync(process.execPath, [ASSERT_SCRIPT], {
      env: {
        ...process.env,
        SDK_SURFACE_CLIENT_MODULE: pathToFileURL(clientModule).href,
        SDK_SURFACE_TYPES_FILE: types,
      },
      encoding: "utf8",
    })

  // Pass side: a complete fixture exits 0 (a gate that always fails is
  // caught here, not mistaken for vigilance).
  const pass = runGate(goodClientModule, goodTypes)
  assert.equal(
    pass.status,
    0,
    `good fixture must pass the gate, got ${pass.status}: ${pass.stdout}${pass.stderr}`,
  )
  assert.match(pass.stdout, /SDK surface OK/)

  // Fail side: one missing method AND one missing event exit 1 and name
  // both — deleting EITHER assertion loop from the CLI breaks this test.
  const fail = runGate(badClientModule, badTypes)
  assert.equal(fail.status, 1, `bad fixture must fail the build, got ${fail.status}`)
  const failOutput = `${fail.stdout}${fail.stderr}`
  assert.ok(
    failOutput.includes("client.message.list()"),
    `the method loop must name the missing bridge method:\n${failOutput}`,
  )
  assert.ok(
    failOutput.includes('"session.idle"'),
    `the event loop must name the event missing from the union:\n${failOutput}`,
  )
} finally {
  rmSync(fixtureDir, { recursive: true, force: true })
}

console.log("sdk_surface_assert negative pins passed")
