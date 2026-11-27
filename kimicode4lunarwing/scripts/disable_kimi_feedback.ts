import { createHash } from "node:crypto"
import { chmodSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

// Review feedback call sites before adding a bundle. Even drift outside the
// patched functions could introduce another registration or publication path.
const reviewedBundles = new Map([
  ["2.7.2", { sha256: "dafef22f56c4c1fff755eb3af5e42a76d32d09a9dc858ebdc055a6d1a6008e67", kapFeedback: false }],
  ["2.8.1", { sha256: "8239be59316fde21d7fb24d9bfffab934033d0c6f21c0a5d9e3be3fa774a3ec4", kapFeedback: true }],
])
const refusal = 'Feedback is disabled in LunarWing workers (WI#916).'

const tuiTarget = [
  "\t\tasync resolveTarget(modelAlias, refresh) {",
  "\t\t\tconst custom = this.customProvider(modelAlias);",
  "\t\t\tif (custom !== void 0) return this.customTarget(custom, refresh);",
  "\t\t\treturn this.managedTarget(refresh);",
  "\t\t}",
].join("\n")
const kapRegistration = [
  "\t\tregisterFeedbackRoutes(apiV1, core, {",
  "\t\t\thostIdentity: opts.hostIdentity,",
  "\t\t\tconversations: opts.conversations,",
  "\t\t\thostBridges: opts.hostBridges",
  "\t\t});\n",
].join("\n")
const kapGateway = [
  "async function openFeedbackGateway(core) {",
  "\tconst provider = core.accessor.get(IProviderService).get(KIMI_CODE_PROVIDER_NAME);",
  "\tconst auth = resolveKimiCodeRuntimeAuth({",
  "\t\tconfiguredBaseUrl: provider?.baseUrl,",
  "\t\tconfiguredOAuthRef: provider?.oauth",
  "\t});",
  '\tconst url = feedbackUrl(core.accessor.get(IBootstrapService).getEnv("KIMI_CODE_FEEDBACK_BASE_URL") ?? auth.baseUrl);',
  "\treturn new FeedbackGateway(url, await requireFeedbackToken(core, auth.oauthRef));",
  "}",
].join("\n")

function replaceOnce(source: string, anchor: string, replacement: string, name: string): string {
  const start = source.indexOf(anchor)
  if (start < 0 || start !== source.lastIndexOf(anchor)) {
    throw new Error(`Expected exactly one Kimi feedback anchor: ${name}`)
  }
  return source.slice(0, start) + replacement + source.slice(start + anchor.length)
}

function disableFeedback(version: string, prefix: string): void {
  const reviewed = reviewedBundles.get(version)
  if (!reviewed) throw new Error(`Kimi ${version} has no reviewed feedback patch`)
  const bundle = join(prefix, "node_modules/@moonshot-ai/kimi-code/dist/main.mjs")
  const bytes = readFileSync(bundle)
  if (createHash("sha256").update(bytes).digest("hex") !== reviewed.sha256) {
    throw new Error(`Kimi ${version} bundle differs from the reviewed feedback source`)
  }

  let source = replaceOnce(bytes.toString("utf8"), tuiTarget, [
    "\t\tasync resolveTarget() {",
    `\t\t\treturn { kind: "error", message: ${JSON.stringify(refusal)} };`,
    "\t\t}",
  ].join("\n"), "TUI feedback target")
  if (reviewed.kapFeedback) {
    source = replaceOnce(source, kapRegistration, "", "KAP route registration")
    source = replaceOnce(source, "\t\t\tfeedback: true,\n", "", "KAP capability")
    source = replaceOnce(source, kapGateway, [
      "async function openFeedbackGateway() {",
      `\tthrow new Error(${JSON.stringify(refusal)});`,
      "}",
    ].join("\n"), "KAP gateway and URL override")
  }
  writeFileSync(bundle, source)
  chmodSync(bundle, 0o555)
}

const [, , version, prefix] = process.argv
if (!version || !prefix) throw new Error("Usage: disable_kimi_feedback.js <version> <npm prefix>")
disableFeedback(version, prefix)
