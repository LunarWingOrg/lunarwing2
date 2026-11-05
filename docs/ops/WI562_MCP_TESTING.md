# WI#562 — MCP testing on persistent tenant `mcpvalidation`

Date: 2026-11-05  
Branch: `rarity/kimi-43-20261105-2131`  
Work item: [WI#562](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/562)

## Scope and preservation

This pass creates a new core-daemon test tenant, installs CoinGecko, Exa,
Brave Search, and Context7 MCP servers without supplying authentication
tokens, and asks the tenant agent to use each server. Results below distinguish
installation, MCP activation/discovery, real tool execution, and agent-driven
use.

The tenant is named `mcpvalidation` (not an animal name). **Do not remove or
purge this tenant.** It must remain running so an operator can add API tokens
later and repeat the tests.

## Confirmed setup

- Host: Gentoo/OpenRC with rootless Podman; all lifecycle operations use
  `ic/scripts/lunarwing-mt-admin.sh`.
- Source revision at provisioning: `f1ebb3649`.
- Tenant port block: `10030-10039`; gateway `10030`, PostgreSQL `10033`.
- No external worker lanes are selected. MCP servers run in the core daemon's
  host placement.
- The daemon uses the existing private OpenAI-compatible LLM endpoint. A real
  unauthenticated-to-LunarWing chat-completions probe through that endpoint
  returned HTTP 200 before provisioning; its generated bearer value is not
  recorded here.
- No CoinGecko, Exa, Brave, or Context7 credential is configured or used.
- Tenant creation completed through `add-tenant`; the sandbox image and
  PostgreSQL were ready and the OpenRC units were rendered.

## Server configurations and expected authentication contract

| Server | Configuration under test | Authentication contract checked against upstream documentation |
| --- | --- | --- |
| CoinGecko | Remote HTTP `https://mcp.api.coingecko.com/mcp` | The official free endpoint is keyless and has shared rate limits. |
| Exa | Remote HTTP `https://mcp.exa.ai/mcp` | Hosted search and fetch allow anonymous, rate-limited use; Exa Agent requires authentication. Keyless access requires a direct IPv4 path. |
| Brave Search | Official `@brave/brave-search-mcp-server` stdio package, pinned to version `2.1.4` | `BRAVE_API_KEY` (or the supported key-file alternative) is required. No hosted keyless Brave endpoint is assumed. |
| Context7 | Remote HTTP `https://mcp.context7.com/mcp` | The standard hosted endpoint is tested without an Authorization header to determine whether anonymous discovery and documentation calls work; an API key/OAuth can be added later. |

Upstream references:

- [CoinGecko MCP](https://docs.coingecko.com/ai-integration/mcp-server)
- [Exa MCP](https://exa.ai/docs/get-started/exa-mcp) and
  [exa-labs/exa-mcp-server](https://github.com/exa-labs/exa-mcp-server/)
- [Brave Search MCP server](https://github.com/brave/brave-search-mcp-server)
- [Context7 MCP clients](https://context7.com/docs/resources/all-clients)

## Test procedure

1. Build and start the tenant through mt-admin.
2. Install each MCP configuration through LunarWing's authenticated extension
   API, then activate it and record discovered tools or the exact failure.
3. For an active server, make a narrow real MCP call through the tenant agent:
   a public Bitcoin price for CoinGecko, a small web search for Exa, a library
   resolution/documentation lookup for Context7, and a web search for Brave.
4. Inspect the gateway history/tool-call record rather than treating a prose
   answer as proof of execution. Record authentication and transport failures
   verbatim where they are safe to publish.
5. Re-check tenant status and leave all four configurations installed for the
   later authenticated pass. Do not tear down the tenant.

## Results

The daemon and XMPP bridge release builds completed, and the tenant service
graph is running. Agent-driven tests are still in progress; no result is
inferred from the upstream authentication contracts above.

### LunarWing installation and activation

All four configurations installed successfully through the authenticated
`POST /api/extensions/install` route. Activation through
`POST /api/extensions/{name}/activate` produced these observed states:

| Server | LunarWing result | Registered tools / failure |
| --- | --- | --- |
| CoinGecko | Active, enabled | `coingecko_execute`, `coingecko_search_docs` |
| Exa | Active, enabled | `exa_web_search_exa`, `exa_web_fetch_exa` |
| Context7 | Active, enabled | `context7_resolve-library-id`, `context7_query-docs` |
| Brave Search | Inactive, disabled after failed activation | `Activation failed: External service error: [brave-search] MCP server closed connection before responding to request Some(1)` |

Exa and Context7 also returned optional OAuth authorization URLs after their
successful activation. No OAuth callback or credential was used; the active
keyless configurations are the ones under test. Brave's generic connection
error is the daemon-facing form of the direct missing-key exit documented
below, not evidence that Brave offers anonymous search.

### Agent-driven results

The prompts below went through the real Engine V2 gateway chat path. Evidence
comes from the persisted `tool_calls` in `/api/chat/history`, not only from the
model's answer.

- **Exa — keyless agent call passed.** The agent called
  `exa_web_search_exa` once with one requested result. History records
  `has_result: true`, `has_error: false`, and a result preview whose top URL is
  `https://modelcontextprotocol.io/llms.txt`. The turn completed and the agent
  reported the same URL. An `echo` entry is the Engine V2 response artifact,
  not a substitute search tool.
- **Context7 — keyless agent calls passed.** The agent called
  `context7_resolve_library_id` and then `context7_query_docs`. History records
  `has_result: true` and `has_error: false` for both. Resolution selected
  `/vercel/next.js`; the documentation preview contains Next.js Route Handler
  guidance and its `route.js|ts` convention. The turn completed with no retry
  or fallback server.
- **CoinGecko and Brave Search:** agent attempts are pending.

### Hosted upstream discovery preflight

Before LunarWing activation, the tenant account used the official MCP
TypeScript SDK against each hosted URL with no Authorization header or API-key
parameter. Initialize and `tools/list` succeeded on all three endpoints:

| Server | Discovered tools |
| --- | --- |
| CoinGecko | `execute`, `search_docs` |
| Exa | `web_search_exa`, `web_fetch_exa` |
| Context7 | `resolve-library-id`, `query-docs` |

These are upstream/network/keyless-discovery results only. They are not treated
as proof that LunarWing can activate the servers or that the tenant agent can
execute the tools.

Narrow direct calls then established these upstream baselines:

- Exa `web_search_exa` returned a result from
  `https://modelcontextprotocol.io/llms.txt` for a one-result search.
- Context7 `resolve-library-id` resolved Next.js to `/vercel/next.js` with
  source metadata and available versions.
- CoinGecko `execute` ran the documented `client.simple.price.get` request for
  Bitcoin in USD but returned `isError: true` with `Error: 429 Throttled`.
  Discovery therefore works, while the keyless shared execution quota was
  exhausted at this attempt. A later agent attempt is still required; this is
  not classified as a LunarWing transport failure.

### Brave Search — direct package preflight

The pinned package was installed under the tenant account with npm lifecycle
scripts disabled. Running its real stdio entrypoint with both `BRAVE_API_KEY`
and `BRAVE_API_KEY_FILE` removed exited with status 1 before MCP startup:

```text
Error: A Brave API key is required via --brave-api-key, BRAVE_API_KEY, --brave-api-key-file, or BRAVE_API_KEY_FILE. You can get one at https://brave.com/search/api/.
Invalid configuration
```

This confirms that the official Brave server cannot perform a keyless search.
The LunarWing installation, activation, and agent-attempt results are recorded
separately below once the tenant is running.
