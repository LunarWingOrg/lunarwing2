// WI#665 managed-policy evaluation, run inside a built worker image.
//
// Starts a fake OpenCode Console on loopback, starts the image's real
// opencode binary, presents a Console credential whose `metadata.server`
// points at the fake (ConsoleAccess.server() honours it), and reports what
// reached the server:
//
//   consoleRequests  — every request the plugin made, so "no request goes to
//                      the Console" can be answered rather than assumed;
//   consoleSkills    — whether a Console-authored skill was registered;
//   providerIDs      — every provider the session lists, the lane's own and
//                      the Console's, so a provider the Console adds and a
//                      provider a Console policy removes are both visible;
//   integrationDocuments — the `info` of every config document /api/config
//                      lists without a `path`. In this harness that is the
//                      Console integration's document: the API serializes it
//                      as a plain document, with no integration ID to match
//                      on, and every file-backed document has a path.
//
// Two switches, set by the driver through the container environment:
//
//   WI665_EVAL_ORG            — the saved credential names this organization.
//                               The `integration.use` resource is then
//                               `opencode#<org>` (the form a saved Console
//                               login takes), not the bare integration ID;
//   WI665_EVAL_DENY_PROVIDER  — the fake Console's config also carries the
//                               policy statement `provider.use` deny for this
//                               provider ID.
//   WI665_EVAL_DENY_TOOL      — the fake Console's config also carries the
//                               policy statement `tool.use` deny for this
//                               `action:resource`. The harness executes the
//                               matching permission assertion and reports
//                               whether it succeeded.
//
// Nothing leaves the container: the fake listens on 127.0.0.1 and the caller
// runs the container with --network=none. Instructions and skills travel
// through one gate upstream (syncInstructionsAndSkills(allowed)); the skill
// list is the part of it the server exposes.
//
// Printed last: one JSON object on stdout prefixed with EVAL_JSON:.

type Reported = {
  ok: boolean;
  consoleRequests: string[];
  consoleSkills: string[];
  consoleProviders: string[];
  consoleMcpServers: string[];
  providerIDs: string[];
  integrationDocuments: unknown[];
  credentialOrganization: string;
  consolePolicyDeniesProvider: string;
  consolePolicyDeniesTool: string;
  consoleToolEffect: string;
  consoleToolAskEffect: string;
  managedPolicyPresent: boolean;
  error?: string;
};

const CONSOLE_PORT = 9912;
const SERVE_PORT = 4097;
const BIN = "/usr/local/lib/opencode2/opencode";
const PASSWORD = "wi665-eval-password";
const EVAL_ORG = process.env.WI665_EVAL_ORG ?? "";
const EVAL_DENY_PROVIDER = process.env.WI665_EVAL_DENY_PROVIDER ?? "";
const EVAL_DENY_TOOL = process.env.WI665_EVAL_DENY_TOOL ?? "";
const INSTRUCTION = {
  id: "wi665-eval-instruction",
  content: "Console-authored instruction from the WI#665 evaluation harness.",
};
const SKILL_SLUG = "wi665-console-eval";
const SKILL_BUNDLE = {
  slug: SKILL_SLUG,
  files: [
    {
      path: "SKILL.md",
      content: [
        "---",
        "name: wi665-console-eval",
        "description: Console-authored skill from the WI#665 evaluation harness",
        "---",
        "",
        "Console-authored skill body.",
        "",
      ].join("\n"),
    },
  ],
};
// A Console config document that supplies a provider and an MCP server, so the
// merge surface is observable as well as the Library: both must be absent when
// the deny holds. With WI665_EVAL_DENY_PROVIDER it also carries a policy
// statement: upstream keeps a denied integration's policies
// (packages/core/src/config.ts:343-349 at the v2.2.7 pin), so that statement
// is expected to apply while everything else in the document is withheld.
const CONSOLE_PROVIDER = "wi665-console-provider";
const CONSOLE_MCP = "wi665-console-mcp";
const CONSOLE_CONFIG = {
  providers: {
    [CONSOLE_PROVIDER]: {
      name: "WI665 Console Provider",
      package: "@ai-sdk/openai-compatible",
      settings: { apiKey: "unused", baseURL: "http://127.0.0.1:9/v1" },
      models: { "wi665-console-model": { name: "wi665-console-model" } },
    },
  },
  mcp: { servers: { [CONSOLE_MCP]: { type: "remote", url: "http://127.0.0.1:9/mcp" } } },
  ...(() => {
    const policies = [
      ...(EVAL_DENY_PROVIDER
        ? [{ action: "provider.use", resource: EVAL_DENY_PROVIDER, effect: "deny" }]
        : []),
      ...(EVAL_DENY_TOOL
        ? [{ action: "tool.use", resource: EVAL_DENY_TOOL, effect: "deny" }]
        : []),
    ];
    return policies.length ? { policies } : {};
  })(),
};

const requests: string[] = [];
const consoleServer = Bun.serve({
  port: CONSOLE_PORT,
  hostname: "127.0.0.1",
  fetch(request) {
    const path = new URL(request.url).pathname;
    requests.push(`${request.method} ${path}`);
    if (path === "/api/v2/config") return Response.json(CONSOLE_CONFIG);
    if (path === "/api/v2/instructions") return Response.json({ instructions: [INSTRUCTION] });
    if (path === "/api/v2/skills") return Response.json({ skills: [{ slug: SKILL_SLUG, version: "1" }] });
    if (path === `/api/v2/skills/${SKILL_SLUG}`) return Response.json(SKILL_BUNDLE);
    return new Response("not found", { status: 404 });
  },
});

const auth = { authorization: `Basic ${btoa(`opencode:${PASSWORD}`)}` };

async function evaluate(): Promise<Reported> {
  const serverEnv: Record<string, string | undefined> = {
    ...process.env,
    OPENCODE_CONFIG: "/eval/workdir/opencode2.json",
    OPENCODE_CONFIG_DIR: "/eval/config-dir",
    OPENCODE_SERVER_PASSWORD: PASSWORD,
  };
  // The switches steer the harness, not the binary under evaluation.
  delete serverEnv.WI665_EVAL_ORG;
  delete serverEnv.WI665_EVAL_DENY_PROVIDER;
  delete serverEnv.WI665_EVAL_DENY_TOOL;
  const child = Bun.spawn([BIN, "serve", "--port", String(SERVE_PORT), "--hostname", "127.0.0.1"], {
    cwd: "/eval/workdir",
    env: serverEnv,
    stdout: "inherit",
    stderr: "inherit",
  });

  const base = `http://127.0.0.1:${SERVE_PORT}`;
  const location = encodeURIComponent("/eval/workdir");
  const get = (path: string) => fetch(`${base}${path}`, { headers: auth }).then((r) => r.json());

  const deadline = Date.now() + 90_000;
  for (;;) {
    try {
      const probe = await fetch(`${base}/api/config?location[directory]=${location}`, { headers: auth });
      if (probe.ok) break;
    } catch {
      // server still starting
    }
    if (Date.now() > deadline) throw new Error("opencode serve did not become ready");
    await Bun.sleep(250);
  }

  const created = await fetch(`${base}/api/credential`, {
    method: "POST",
    headers: { ...auth, "content-type": "application/json" },
    body: JSON.stringify({
      integrationID: "opencode",
      label: "WI#665 evaluation",
      value: {
        type: "key",
        key: "wi665-eval-key",
        metadata: {
          server: `http://127.0.0.1:${CONSOLE_PORT}`,
          ...(EVAL_ORG ? { orgID: EVAL_ORG } : {}),
        },
      },
    }),
  });
  const createdBody = await created.text();
  if (!created.ok) throw new Error(`credential create failed: ${created.status} ${createdBody}`);

  // The plugin loads on connection, then the skill download runs in the
  // background; give both a bounded moment before reading.
  await Bun.sleep(8_000);

  // Exercise the real tool hook: create a session and assert the exact
  // `action:resource` the Console policy denies. A granted assertion returns
  // 204; a refusal is an error response, so the status is the observation.
  const createdSession = await fetch(`${base}/api/session`, {
    method: "POST",
    headers: { ...auth, "content-type": "application/json" },
    body: JSON.stringify({ title: "WI#745 managed-policy evaluation" }),
  });
  const sessionBody = await createdSession.text();
  if (!createdSession.ok) throw new Error(`session create failed: ${createdSession.status} ${sessionBody}`);
  const sessionID = (JSON.parse(sessionBody) as { data?: { id?: string } }).data?.id;
  if (!sessionID) throw new Error("session create returned no id");
  const assertTool = async (): Promise<string> => {
    if (!EVAL_DENY_TOOL) return "absent";
    const [action, ...rest] = EVAL_DENY_TOOL.split(":");
    const asserted = await fetch(`${base}/api/session/${sessionID}/permission`, {
      method: "POST",
      headers: { ...auth, "content-type": "application/json" },
      body: JSON.stringify({ action, resources: [rest.join(":")], save: [] }),
    });
    const text = await asserted.text();
    if (!asserted.ok) throw new Error(`permission evaluate failed: ${asserted.status} ${text}`);
    const effect = (JSON.parse(text) as { data?: { effect?: string } }).data?.effect;
    if (!effect) throw new Error(`permission evaluate returned no effect: ${text}`);
    return effect;
  };
  const toolEffect = await assertTool();
  let toolAskEffect = "absent";
  if (EVAL_DENY_TOOL) {
    const updated = await fetch(`${base}/api/session/${sessionID}`, {
      method: "PATCH",
      headers: { ...auth, "content-type": "application/json" },
      body: JSON.stringify({ permissions: [{ action: EVAL_DENY_TOOL.split(":")[0], resource: "*", effect: "ask" }] }),
    });
    if (!updated.ok) throw new Error(`session ask seed failed: ${updated.status} ${await updated.text()}`);
    toolAskEffect = await assertTool();
  }

  const skills = (await get(`/api/skill?location[directory]=${location}`)) as {
    data?: { id?: string; name?: string; path?: string }[];
  };
  const providers = (await get(`/api/provider?location[directory]=${location}`)) as {
    data?: { id?: string }[];
  } | { id?: string }[];
  const mcp = (await get(`/api/mcp?location[directory]=${location}`)) as {
    data?: { name?: string }[];
  } | { name?: string }[];
  const providerList = Array.isArray(providers) ? providers : (providers.data ?? []);
  const mcpList = Array.isArray(mcp) ? mcp : (mcp.data ?? []);
  const providerIDs = providerList.map((provider) => provider.id ?? "").filter((id) => id !== "");
  const config = (await get(`/api/config?location[directory]=${location}`)) as
    | { type?: string; path?: string; info?: unknown }[]
    | { data?: { type?: string; path?: string; info?: unknown }[] };
  const entries = Array.isArray(config) ? config : (config.data ?? []);
  const integrationDocuments = entries
    .filter((entry) => entry?.type === "document" && entry.path === undefined)
    .map((entry) => entry.info ?? {});

  let managedPolicyPresent = false;
  try {
    const text = await Bun.file("/etc/opencode/opencode.json").text();
    managedPolicyPresent = text.includes("integration.use");
  } catch {
    // absent in the control run
  }

  child.kill();
  return {
    ok: true,
    consoleRequests: requests,
    consoleSkills: (skills.data ?? []).map((skill) => `${skill.id} ${skill.path ?? ""}`.trim()),
    consoleProviders: providerIDs.filter((id) => id.includes("wi665")),
    consoleMcpServers: mcpList
      .map((server) => server.name ?? "")
      .filter((name) => name.includes("wi665")),
    providerIDs,
    integrationDocuments,
    credentialOrganization: EVAL_ORG,
    consolePolicyDeniesProvider: EVAL_DENY_PROVIDER,
    consolePolicyDeniesTool: EVAL_DENY_TOOL,
    consoleToolEffect: toolEffect,
    consoleToolAskEffect: toolAskEffect,
    managedPolicyPresent,
  };
}

let reported: Reported;
try {
  reported = await evaluate();
} catch (error) {
  reported = {
    ok: false,
    consoleRequests: requests,
    consoleSkills: [],
    consoleProviders: [],
    consoleMcpServers: [],
    providerIDs: [],
    integrationDocuments: [],
    credentialOrganization: EVAL_ORG,
    consolePolicyDeniesProvider: EVAL_DENY_PROVIDER,
    consolePolicyDeniesTool: EVAL_DENY_TOOL,
    consoleToolEffect: "",
    consoleToolAskEffect: "",
    managedPolicyPresent: false,
    error: error instanceof Error ? error.message : String(error),
  };
} finally {
  consoleServer.stop(true);
}
console.log(`EVAL_JSON:${JSON.stringify(reported)}`);
process.exit(reported.ok ? 0 : 1);
