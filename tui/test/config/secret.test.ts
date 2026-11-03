import { describe, expect, test } from "bun:test"
import { format, inspect } from "node:util"
import { REDACTED, Secret, isSecret } from "../../src/config/secret.ts"

// A fake value shaped like a gateway token; never a real credential.
const VALUE = "lw-test-0123456789abcdef0123456789abcdef"

function leaks(text: string): boolean {
  return text.includes(VALUE) || text.includes(VALUE.slice(8, 24))
}

describe("Secret never reveals its value implicitly (3.11.3)", () => {
  const secret = new Secret(VALUE)

  test("String(), toString(), template literals and concatenation are redacted", () => {
    expect(String(secret)).toBe(REDACTED)
    expect(secret.toString()).toBe(REDACTED)
    expect(`Bearer ${secret}`).toBe(`Bearer ${REDACTED}`)
    expect("token=" + secret).toBe(`token=${REDACTED}`)
    expect([secret].join(",")).toBe(REDACTED)
  })

  test("JSON.stringify, alone and nested, is redacted", () => {
    expect(JSON.stringify(secret)).toBe(JSON.stringify(REDACTED))
    const body = JSON.stringify({ extension_name: "github", token: secret, nested: [{ secret }] })
    expect(leaks(body)).toBe(false)
    expect(body).toContain(REDACTED)
  })

  test("Bun.inspect and node:util inspect are redacted, including showHidden and inside objects", () => {
    for (const text of [
      Bun.inspect(secret),
      Bun.inspect({ token: secret }),
      inspect(secret),
      inspect({ deep: { token: secret } }, { depth: 10 }),
      inspect(secret, { showHidden: true }),
    ]) {
      expect(leaks(text)).toBe(false)
    }
    expect(Bun.inspect(secret)).toBe(REDACTED)
    expect(inspect(secret)).toBe(REDACTED)
  })

  test("util.format with %s, %o, %O and %j is redacted", () => {
    const text = format("%s %o %O %j", secret, secret, secret, { secret })
    expect(leaks(text)).toBe(false)
  })

  test("inspection without custom hooks shows no value: it is not stored on the object", () => {
    expect(leaks(inspect(secret, { customInspect: false, showHidden: true }))).toBe(false)
    expect(Object.keys(secret)).toEqual([])
    expect(Object.getOwnPropertyNames(secret)).toEqual([])
    expect(Object.getOwnPropertySymbols(secret)).toEqual([])
    expect(leaks(JSON.stringify({ ...secret }))).toBe(false)
    expect(leaks(JSON.stringify(structuredClone(secret)))).toBe(false)
  })

  test("an error message built from a secret is redacted", () => {
    const error = new Error(`auth failed for ${secret}`)
    expect(leaks(error.message)).toBe(false)
    expect(leaks(String(error))).toBe(false)
  })

  test("the instance is frozen", () => {
    expect(Object.isFrozen(secret)).toBe(true)
  })
})

describe("explicit access", () => {
  test("expose() and its alias reveal() return the value", () => {
    const secret = new Secret(VALUE)
    expect(secret.expose()).toBe(VALUE)
    expect(secret.reveal()).toBe(VALUE)
  })

  test("equals() compares values without exposing them", () => {
    const a = new Secret(VALUE)
    expect(a.equals(new Secret(VALUE))).toBe(true)
    expect(a.equals(new Secret(`${VALUE}x`))).toBe(false)
    expect(a.equals(new Secret(VALUE.replace("0", "1")))).toBe(false)
    expect(a.equals(new Secret(""))).toBe(false)
  })

  test("dispose() drops the value; expose() then throws without echoing it", () => {
    const secret = new Secret(VALUE)
    secret.dispose()
    expect(secret.disposed).toBe(true)
    expect(() => secret.expose()).toThrow("secret was disposed")
    try {
      secret.reveal()
    } catch (error) {
      expect(leaks(String(error))).toBe(false)
    }
    expect(secret.equals(new Secret(VALUE))).toBe(false)
    expect(String(secret)).toBe(REDACTED)
  })

  test("isSecret distinguishes wrapped values from strings", () => {
    expect(isSecret(new Secret(VALUE))).toBe(true)
    expect(isSecret(VALUE)).toBe(false)
    expect(isSecret({ expose: () => VALUE })).toBe(false)
  })
})
