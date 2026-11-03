/**
 * Proxy environment neutralization (design 3.11.4, SEC-14).
 *
 * Bun's fetch sends requests through HTTP_PROXY, HTTPS_PROXY or ALL_PROXY whenever the `proxy` option is unset,
 * loopback requests included, and bypasses only hosts listed in NO_PROXY. Every peer of the TUI is local or
 * explicitly configured, so the process disables proxying before its first fetch.
 *
 * Measured on Bun 1.3.14 (and pinned by test/config/proxy-env.test.ts through test/fixtures/proxy-child.ts):
 * `delete process.env.HTTP_PROXY` does NOT stop the proxying, because Bun keeps its own copy of the
 * environment; assigning "" does. Every variable the client wants gone is therefore assigned "", never deleted.
 */

export const PROXY_ENV_NAMES = ["http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"] as const

export type ProxyEnvName = (typeof PROXY_ENV_NAMES)[number]

/** A writable environment (process.env, or a plain object in tests). */
export type MutableEnv = Record<string, string | undefined>

/** Called first in main.tsx. Assigning (not deleting) is what works on Bun 1.3.14. */
export function neutralizeProxyEnv(env: MutableEnv = process.env): void {
  for (const name of PROXY_ENV_NAMES) env[name] = ""
  env.NO_PROXY = "*"
  env.no_proxy = "*"
}

/** Blanks variables the client has consumed (e.g. LUNARWING_GATEWAY_TOKEN), for the same reason: "" not delete. */
export function blankEnv(env: MutableEnv, names: readonly string[]): void {
  for (const name of names) {
    if (env[name] !== undefined) env[name] = ""
  }
}
