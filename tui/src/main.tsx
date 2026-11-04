/**
 * Thin entry point (design 3.1, 3.2): configuration, renderer, controllers, mount. Started by bin/wingtip.
 *
 * Order: neutralizeProxyEnv() first, before anything can fetch (3.11.4: Bun's fetch would send even loopback
 * requests through an inherited HTTP_PROXY, and GatewayTransport refuses to run with one set). Then
 * loadConfig() (tokens on argv exit 2; the tenant picker renders on the renderer when several tenants are
 * configured), the renderer with the 3.1 settings, the gateway transport, the recorders, clipboard and image
 * exporter, and createAppRuntime(), which wires every controller. renderer.on("destroy") shuts the controllers
 * down, so parec and other children never outlive the TUI; the profile lock is released at the end.
 *
 * The image protocol flag (salvaged from v4:tui/src/main.tsx:21-56): --image-protocol and
 * LUNARWING_TUI_IMAGE_PROTOCOL are parsed by config/config.ts into ui.imageProtocol; a forced protocol is
 * reported as a notice instead of on stderr, because OpenTUI owns the terminal.
 */
import { createCliRenderer, SystemClock, type CliRenderer, type CliRendererConfig, type Clock } from "@opentui/core"
import { createRoot } from "@opentui/react"
import { App, createAppRuntime, type AppRuntime } from "./app.tsx"
import { buildChildEnv } from "./config/child-env.ts"
import { isConfigError, loadConfig, type GatewayTarget, type LoadConfigResult, type ResolvedConfig } from "./config/config.ts"
import { neutralizeProxyEnv } from "./config/proxy-env.ts"
import type { ChildEnv } from "./config/types.ts"
import { resolveTheme } from "./components/theme.ts"
import { pickTenantOn } from "./components/tenant-picker.tsx"
import { createClipboardReader } from "./images/clipboard.ts"
import { decodeDataUrl } from "./images/data-url.ts"
import { createImageExporter } from "./images/export.ts"
import { imageLimitsFor } from "./images/limits.ts"
import { openDebugLog, type DebugLog } from "./log.ts"
import { GatewayTransport } from "./transport/gateway.ts"
import { createOpenTuiRecorder } from "./voice/recorder-opentui.ts"
import { createParecRecorder } from "./voice/recorder-parec.ts"
import type { Recorder } from "./voice/types.ts"

/** The renderer settings of design 3.1. */
function rendererConfig(clock: Clock): CliRendererConfig {
  return {
    exitOnCtrlC: false, // Ctrl+C is handled by controller/keymap.ts
    useKittyKeyboard: { events: true }, // release events where KKP is negotiated; inert inside tmux
    useMouse: true,
    enableMouseMovement: false, // drag tracking: press, drag, release only
    synchronizedOutput: "always", // 0.8.x "auto" can show partial frames in tmux
    targetFps: 30,
    clock,
  }
}

function timezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone ?? ""
  } catch {
    return ""
  }
}

function gatewayTransport(target: GatewayTarget, config: ResolvedConfig, clock: Clock): GatewayTransport {
  return new GatewayTransport({
    baseUrl: target.url,
    token: target.token,
    thread: config.thread,
    clock,
    timezone: timezone(),
    stateFile: config.stateFile,
    expectedListener: target.expectedListener,
    tenant: target.tenant?.tenant ?? null,
    allowInsecureHttp: target.allowInsecureHttp,
    decodeImage: (url) => decodeDataUrl(url),
  })
}

function recorders(config: ResolvedConfig, clock: Clock, env: ChildEnv): Recorder[] {
  if (!config.voice.enabled) return []
  return [createOpenTuiRecorder({ clock }), createParecRecorder({ clock, env })]
}

function buildRuntime(renderer: CliRenderer, config: ResolvedConfig, target: GatewayTarget, clock: Clock, log: DebugLog): AppRuntime {
  const env = buildChildEnv()
  let runtime: AppRuntime | null = null
  const clipboard = createClipboardReader({
    maxImageBytes: () => imageLimitsFor(runtime?.store.getState().server ?? null).maxImageBytes,
    env,
    clock,
  })
  const exporter = createImageExporter({ runtimeDir: config.dirs.runtime, env, clock })
  const owner = target.expectedListener
  runtime = createAppRuntime({
    clock,
    // The renderer's SystemClock reads performance.now(); server times, file names and stamps need epoch time.
    wallNow: () => Date.now(),
    transport: gatewayTransport(target, config, clock),
    stateFile: config.stateFile,
    config: {
      profile: config.profile,
      tenant: target.tenant?.tenant ?? null,
      primary: config.primary,
      ui: config.ui,
      voice: config.voice,
      cwd: process.cwd(),
      listenerOwner: owner === null ? null : `uid ${owner.uid} (${owner.label})`,
      tokenSource: target.token.describe(),
    },
    view: { width: renderer.width, height: renderer.height },
    env,
    recorders: recorders(config, clock, env),
    clipboard,
    exporter,
    log,
    notices: forcedProtocolNotice(config),
    noColor: process.env.NO_COLOR,
    onQuit: () => renderer.destroy(),
  })
  return runtime
}

function forcedProtocolNotice(config: ResolvedConfig): ResolvedConfig["notices"] {
  if (config.ui.imageProtocol === "auto") return config.notices
  return [...config.notices, { level: "info", text: `image protocol forced to "${config.ui.imageProtocol}"`, sticky: false }]
}

async function closeRenderer(renderer: CliRenderer | null): Promise<void> {
  if (renderer === null || renderer.isDestroyed) return
  renderer.destroy()
  await renderer.closed
}

type Loaded = LoadConfigResult | { kind: "error"; code: number; message: string }

async function loadOrExit(ensureRenderer: () => Promise<CliRenderer>): Promise<Loaded> {
  const pickerTheme = resolveTheme({ color: "auto", noColor: process.env.NO_COLOR })
  try {
    return await loadConfig({
      argv: process.argv.slice(2),
      pickTenant: async (choices, preselect) => pickTenantOn(await ensureRenderer(), pickerTheme)(choices, preselect),
    })
  } catch (error) {
    if (!isConfigError(error)) throw error
    return { kind: "error", code: error.exitCode, message: error.message }
  }
}

async function main(): Promise<number> {
  neutralizeProxyEnv()
  const clock = new SystemClock()
  let renderer: CliRenderer | null = null
  const ensureRenderer = async (): Promise<CliRenderer> => (renderer ??= await createCliRenderer(rendererConfig(clock)))
  let loaded: Loaded
  try {
    loaded = await loadOrExit(ensureRenderer)
  } catch (error) {
    await closeRenderer(renderer)
    throw error
  }
  if (loaded.kind !== "ready") {
    // The picker's renderer (if any) is gone before anything is printed: OpenTUI owned the terminal.
    await closeRenderer(renderer)
    if (loaded.kind === "error") process.stderr.write(`${loaded.message}\n`)
    else if (loaded.output !== "") process.stdout.write(`${loaded.output}\n`)
    return loaded.code
  }
  return run(loaded.config, clock, ensureRenderer)
}

async function run(config: ResolvedConfig, clock: Clock, ensureRenderer: () => Promise<CliRenderer>): Promise<number> {
  const target = config.target
  if (target.transport !== "gateway") {
    await config.lock.release()
    process.stderr.write("wingtip: the socket transport is not in this build yet (M4); use --tenant or --gateway-url\n")
    return 2
  }
  let log: DebugLog
  try {
    log = await openDebugLog(config.debugLogPath)
  } catch (error) {
    await config.lock.release()
    process.stderr.write(`${isConfigError(error) ? error.message : "the debug log cannot be opened"}\n`)
    return 2
  }
  const renderer = await ensureRenderer()
  const runtime = buildRuntime(renderer, config, target, clock, log)
  const destroyed = new Promise<void>((resolve) => renderer.once("destroy", () => resolve()))
  renderer.on("destroy", () => void runtime.shutdown("quit"))
  createRoot(renderer).render(<App runtime={runtime} />)
  runtime.start()
  await destroyed
  await renderer.closed
  await runtime.shutdown("quit")
  await config.lock.release()
  await log.close()
  return 0
}

try {
  process.exit(await main())
} catch (error) {
  // Never print a token: errors here come from setup code whose messages are built without secrets.
  process.stderr.write(`wingtip: ${error instanceof Error ? error.message : "unexpected failure"}\n`)
  process.exit(1)
}
