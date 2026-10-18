/**
 * sdk_surface_test.ts — unit pins for the sdk_surface.ts machinery (WI#369):
 * the recursive mock flatten, the comment/string-blanking extractors, the
 * behavioural recordingSdkProxy binding, and the V2Event union parser.
 * Several tests are mutation pins by construction: a non-recursive flatten,
 * a substring/comment-blind extractor, or a corpus-wide event check fails
 * exactly the test named for it.
 */

import assert from "node:assert/strict"

import {
  BRIDGE_SDK_EVENTS,
  BRIDGE_SDK_METHODS,
  blankCommentsAndStrings,
  extractEventTypes,
  extractSdkCallPaths,
  findUndeclaredSdkPaths,
  mockMethodPaths,
  parseV2EventTypes,
  recordingSdkProxy,
} from "./sdk_surface.ts"

// ── recursive mock flatten (CY-3 residue) ────────────────────────────────
// A three-level mock pins the recursion itself: a two-level-only flatten
// (the pre-WI#329 shape) drops "a.b.c.d" / "a.b.e" and fails this test.
{
  const deep = {
    a: {
      b: {
        c: {
          d: () => {},
        },
        e: () => {},
      },
      f: () => {},
    },
    g: () => {},
    h: { i: { j: { k: { l: () => {} } } } },
    notAFunction: 42,
    alsoNot: null,
  }
  assert.deepEqual(mockMethodPaths(deep), ["a.b.c.d", "a.b.e", "a.f", "g", "h.i.j.k.l"])
}

// ── comment/string blanking ──────────────────────────────────────────────
{
  const source = [
    `// sdk.session.message.list( is prose, not a call`,
    `/* event.type === "bogus.event" is prose too */`,
    `const s = "sdk.session.summarize("`,
    `const t = 'event.type === "bogus.yoda"'`,
    "const tpl = `prefix ${sdk.session.create( x )} suffix`",
    `sdk.session.prompt(y)`,
  ].join("\n")
  const { blanked, strings } = blankCommentsAndStrings(source)
  assert.ok(!blanked.includes("session.message.list"), "comment text must be blanked")
  assert.ok(!blanked.includes("bogus.event"), "block-comment text must be blanked")
  assert.ok(!blanked.includes("bogus.yoda"), "quoted contents must be blanked")
  assert.ok(
    blanked.includes("sdk.session.create") && blanked.includes("sdk.session.prompt"),
    "real code — including inside a template expression — must survive",
  )
  const values = strings.map((span) => span.value)
  assert.deepEqual(values, ["sdk.session.summarize(", 'event.type === "bogus.yoda"'])
}

// ── extractSdkCallPaths: the DF-1 mutation shapes bind by syntax ─────────
{
  // Base multi-line form (the executor's real style).
  assert.deepEqual(
    extractSdkCallPaths("sdk.session\n  .interrupt({ sessionID }, { signal })"),
    ["session.interrupt"],
  )
  // Optional call: sdk.session.summarize?.(…) — a silent no-op on a missing
  // method; the pre-WI#369 pattern missed it entirely.
  assert.deepEqual(extractSdkCallPaths("sdk.session.summarize?.({ sessionID })"), [
    "session.summarize",
  ])
  // Cast receiver: (sdk as any).session.summarize().
  assert.deepEqual(extractSdkCallPaths("(sdk as any).session.summarize()"), ["session.summarize"])
  assert.deepEqual(extractSdkCallPaths("(sdk as OpenCodeClient).message.list(input, opts)"), [
    "message.list",
  ])
  // Non-null assertions inside the chain.
  assert.deepEqual(extractSdkCallPaths("sdk.session!.create(input)"), ["session.create"])
  assert.deepEqual(extractSdkCallPaths("sdk!.session.create(input)"), ["session.create"])
  // Comments and strings never produce call paths (DF-4): the pre-WI#369
  // extractor failed the self-test with a misleading "call sites differ".
  assert.deepEqual(extractSdkCallPaths("// sdk.session.message.list("), [])
  assert.deepEqual(extractSdkCallPaths('const s = "sdk.session.message.list("'), [])
  assert.deepEqual(extractSdkCallPaths("/* sdk.session.message.list( */"), [])
}

// ── extractEventTypes: Yoda form, no false matches ───────────────────────
{
  assert.deepEqual(extractEventTypes('event.type === "session.idle"'), ["session.idle"])
  // Yoda form ("x" === event.type) — missed by the pre-WI#369 pattern.
  assert.deepEqual(extractEventTypes('"session.idle" === event.type'), ["session.idle"])
  // Multi-line comparison.
  assert.deepEqual(extractEventTypes('event.type\n  ===\n  "session.idle"'), ["session.idle"])
  assert.deepEqual(extractEventTypes('"session.idle"\n  ===\n  event.type'), ["session.idle"])
  // Comments and other strings never match (DF-4).
  assert.deepEqual(extractEventTypes('// event.type === "bogus.event"'), [])
  assert.deepEqual(extractEventTypes('const s = "event.type === \\"bogus.event\\""'), [])
  assert.deepEqual(extractEventTypes("const s = 'event.type === \"bogus.event\"'"), [])
  assert.deepEqual(extractEventTypes('/* "bogus.event" === event.type */'), [])
  // A SET.has(event.type) refactor removes names from the extracted set —
  // the self-test's set-equality assertion then forces the list to stay.
  assert.deepEqual(extractEventTypes('SET.has(event.type)'), [])
}

// ── recordingSdkProxy: the DF-1 mutation shapes bind by behaviour ────────
{
  const makeMock = () => ({
    session: {
      create: async () => ({}),
      prompt: async () => ({}),
      interrupt: async () => ({}),
    },
    permission: { reply: async () => ({}) },
    event: { subscribe: () => ({}) },
    message: { list: async () => ({}) },
  })

  // Declared paths touched directly are not flagged.
  {
    const accessed = new Set<string>()
    const sdk = recordingSdkProxy(makeMock(), accessed)
    await sdk.session.create({})
    await sdk.message.list({})
    sdk.event.subscribe()
    assert.deepEqual(findUndeclaredSdkPaths(accessed), [])
    assert.ok(accessed.has("session.create") && accessed.has("message.list"))
  }

  // Alias call: const s = sdk.session; s.summarize() — records the leaf
  // however it was reached.
  {
    const accessed = new Set<string>()
    const sdk = recordingSdkProxy(makeMock(), accessed)
    const s = sdk.session
    s.create({})
    assert.deepEqual(findUndeclaredSdkPaths(accessed), [])
    // @ts-expect-error deliberate non-existent method for the mutation pin
    try { s.summarize() } catch { /* TypeError on the mock — the access was already recorded */ }
    assert.deepEqual(findUndeclaredSdkPaths(accessed), ["session.summarize"])
  }

  // Cast: (sdk as any).session.summarize().
  {
    const accessed = new Set<string>()
    const sdk = recordingSdkProxy(makeMock(), accessed) as never as {
      session: { summarize(): void }
    }
    try { sdk.session.summarize() } catch { /* TypeError on the mock — the access was already recorded */ }
    assert.deepEqual(findUndeclaredSdkPaths(accessed), ["session.summarize"])
  }

  // Bracket access: sdk["session"].summarize().
  {
    const accessed = new Set<string>()
    const sdk = recordingSdkProxy(makeMock(), accessed) as never as Record<
      string,
      { summarize(): void }
    >
    try { sdk["session"]!.summarize() } catch { /* TypeError on the mock — the access was already recorded */ }
    assert.deepEqual(findUndeclaredSdkPaths(accessed), ["session.summarize"])
  }

  // Destructuring: const { session } = sdk; session.summarize().
  {
    const accessed = new Set<string>()
    const sdk = recordingSdkProxy(makeMock(), accessed)
    const { session } = sdk
    await session.create({})
    assert.deepEqual(findUndeclaredSdkPaths(accessed), [])
    const abused = session as never as { summarize(): void }
    try { abused.summarize() } catch { /* TypeError on the mock — the access was already recorded */ }
    assert.deepEqual(findUndeclaredSdkPaths(accessed), ["session.summarize"])
  }

  // Helper with a parameter NOT named sdk: the pre-WI#369 text binding
  // covered fetchAssistantText only because its parameter happened to be
  // named sdk; the proxy binds any parameter name on exercised paths.
  {
    const accessed = new Set<string>()
    const sdk = recordingSdkProxy(makeMock(), accessed)
    const helper = (client: { session: { summarize?: () => void } }) => client.session.summarize?.()
    helper(sdk)
    assert.deepEqual(findUndeclaredSdkPaths(accessed), ["session.summarize"])
  }

  // Optional-call no-op: sdk.session.summarize?.() resolves undefined and
  // silently does nothing (exactly the WI#320 F7 failure class) — the proxy
  // still records the access, so the suite fails on the undeclared path.
  {
    const accessed = new Set<string>()
    const sdk = recordingSdkProxy(makeMock(), accessed)
    const optional = sdk.session as never as { summarize?: () => void }
    optional.summarize?.()
    assert.deepEqual(findUndeclaredSdkPaths(accessed), ["session.summarize"])
  }
}

// ── parseV2EventTypes: union-scoped event gate (DF-2) ────────────────────
{
  const fixture = [
    `export type LocationShutdown = {`,
    `  id: string`,
    `  type: "location.shutdown"`,
    `  data: {}`,
    `}`,
    ``,
    `export type SessionIdle = {`,
    `  id: string`,
    `  metadata?: { [x: string]: any } | undefined`,
    `  type: "session.idle"`,
    `  data: { sessionID: string }`,
    `}`,
    ``,
    `export type V2EventRpc = {`,
    `  id: string`,
    `  type: \`\${"rpc."}\${string}\``,
    `  data: {}`,
    `}`,
    ``,
    `export type V2Event =`,
    `  | LocationShutdown`,
    `  | SessionIdle`,
    `  | V2EventRpc`,
    ``,
    `export type EventSubscribeOutput = V2Event`,
  ].join("\n")
  const parsed = parseV2EventTypes(fixture)
  assert.deepEqual(parsed.literals, ["location.shutdown", "session.idle"])
  assert.deepEqual(parsed.problems, [])
  // Dynamic families (template-literal type expressions) are skipped, not
  // reported — only a member with NO top-level type field is drift.
  assert.deepEqual(parsed.dynamicMembers, ["V2EventRpc"])

  // A name still present elsewhere in the corpus (the v1 compat schema
  // pattern) but absent from the union no longer counts — that was the
  // corpus-wide substring blind spot.
  assert.ok(!parsed.literals.includes("permission.asked"))
  assert.ok(fixture.length > 0)

  // Missing union → fail closed.
  assert.ok(
    parseV2EventTypes("export type SomethingElse = {\n  type: \"x\"\n}\n").problems.some((p) =>
      p.includes("V2Event"),
    ),
  )
  // Member with no top-level type field → fail closed.
  const noTypeField = fixture.replace('  type: "session.idle"\n', "")
  assert.ok(
    parseV2EventTypes(noTypeField).problems.some((p) => p.includes("SessionIdle")),
  )
  // Missing EventSubscribeOutput alias → fail closed.
  const noAlias = fixture.replace("export type EventSubscribeOutput = V2Event", "")
  assert.ok(parseV2EventTypes(noAlias).problems.some((p) => p.includes("EventSubscribeOutput")))
  // Missing member definition → fail closed.
  const noMemberDef = fixture
    .replace(
      `export type SessionIdle = {\n  id: string\n  metadata?: { [x: string]: any } | undefined\n  type: "session.idle"\n  data: { sessionID: string }\n}\n\n`,
      "",
    )
  assert.ok(
    parseV2EventTypes(noMemberDef).problems.some((p) => p.includes("SessionIdle")),
  )
}

// Every bridge event name must be sane input for the union gate.
assert.ok(BRIDGE_SDK_EVENTS.length > 0 && BRIDGE_SDK_METHODS.length > 0)

console.log("sdk_surface unit pins passed")
