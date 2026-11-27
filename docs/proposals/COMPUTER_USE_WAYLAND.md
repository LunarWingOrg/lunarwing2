# Computer use on GNU/Linux Wayland

Status: **implemented on branch `cua22301`, awaiting live desktop
acceptance.** Roadmap row: "Wayland Desktop Computer Use Capability with
upgraded multimodal capable MCP server" (`docs/ops/ROADMAP_2026.md`).

The agent sees the user's desktop through screenshots and acts on it with
pointer and keyboard input. Two pieces make that work:

1. **The daemon shows tool-result images to the model** (Rust, `ic/`). Before
   this change every tool result was text only, so a screenshot could not
   reach the model.
2. **A desktop connector** (`projects/computer-use/`, Python, stdio MCP
   server) captures the screen and injects input on an existing Wayland
   session.

Reference branches: `origin/computeruse1` (first connector, RemoteDesktop
portal only, never run on a real compositor) and
`origin/mcp-image-forwarding` (WIP image side channel). Both forked on
2026-10-01. The image work here reuses the side-channel idea of the second
branch. Its last hop never worked: images were attached to tool-role
messages, which the bridge drops. The connector reuses the first branch's
portal code and adds a wlroots-protocol backend.

## Decisions

| Question | Decision | Why |
|---|---|---|
| How images reach the model | A user-role message placed right after the tool results of the step that produced them | The daemon reaches models through OpenAI Chat Completions (including TensorZero) and Ollama. Chat Completions accepts only text in tool messages, TensorZero stringifies them, and vendored rig-core rejects or replaces tool-result images on both providers. A user message after the tool results is valid everywhere. |
| Where images live | Next to the text (`ToolOutput.images`, `#[serde(skip)]`), never inside it | The text safety pipeline, previews, transcripts, `conversation_messages`, `job_actions`, Engine V2 documents and events all read text only, so they cannot store or leak image bytes. |
| Validation | Magic-byte sniffing (PNG/JPEG/GIF/WebP) plus a structural check (PNG IHDR/IEND, JPEG end marker, GIF trailer, WebP RIFF size); at most 4 images, 5 MiB each, 7 MiB per result | Same sniffing as gateway uploads (`channels/image_mime.rs`); the declared MIME type is never trusted. The structural check stops one truncated image from failing every later request that resends it. |
| Context growth | A step shows at most 8 images and 8 MiB; only the newest 3 image-bearing steps, within 8 MiB, are resent within a turn (v1) or run (Engine V2) | A computer-use turn takes a screenshot after almost every action. Same limits on both engines. |
| Shared conversations | Images are never shown in group chats or rooms (a note replaces them); tools of `--interactive-only` servers are refused there | The model's description of the owner's screen would reach everyone in the room. |
| Lifetime | Turn-scoped (v1) and run-scoped (Engine V2), memory only | Same rule as inbound image attachments: descriptions persist, bytes do not. |
| Models without vision | `TOOL_RESULT_IMAGES=false` stops forwarding; the tool text then says images were omitted | Nothing in the daemon can tell whether the configured model has vision (`is_vision_model` matches no current model ids). Default is on. |
| Autonomous use | New per-server `interactive_only` flag (`lunarwing mcp add --interactive-only`), kept by `mcp update` and reinstalls | MCP tools used to be offered to routines, jobs and missions without any approval prompt. Desktop control must only run in a private turn the owner is watching. |
| Forcing approval | `--require-approval <tool>` exposes the existing `always_require_approval` list on the CLI; a pinned tool also never runs autonomously (`McpToolWrapper::allows_autonomous_execution`) | Lets an operator require a prompt for every keystroke tool even under session auto-approval. The v1 autonomous allow-list ignores the approval tier, so without the opt-out a routine or job would run a pinned tool with no prompt; Engine V2 background threads and worker MCP activation already refused it. |
| Connector transport | stdio MCP, run as the desktop user, locally or over SSH | No listener (WI#357 spirit). Tenant daemons run as service users without a graphical session; SSH to the desktop user is the cross-user path. A WASM tool cannot reach D-Bus, the portals or the Wayland socket, so it would still need this helper. |
| Wayland backends | RemoteDesktop/ScreenCast portal (GNOME, KDE, COSMIC) and wlroots protocols (Hyprland, Sway, labwc, river, Wayfire, niri) | Hyprland and the wlroots portal do not implement RemoteDesktop, and the main development desktop runs Hyprland. |

## Daemon changes

- **MCP wire format** (`ic/src/tools/mcp/protocol.rs`): `isError`,
  `mimeType` and `structuredContent` use the specification's names (legacy
  snake_case is still accepted). Before this change a spec-compliant
  `isError: true` read as success, and a spec-compliant image block failed
  the whole result. Embedded resources, `audio`, `resource_link` and
  unknown block types no longer fail the result either.
- **MCP result handling** (`ic/src/tools/mcp/client.rs`): text and embedded
  text resources become the text result; images become validated
  `ToolImage`s; anything not forwarded is replaced by a note.
- **Image type and rules** (`ic/src/tools/images.rs`), and
  `execute_tool_with_safety_rich` (`ic/src/tools/execute.rs`).
  Text-only paths (approval resumes, jobs, routines, the external-worker
  proxy, scheduler and container paths) add a note when they drop images.
- **v1 chat loop** (`ic/src/agent/tool_images.rs`, `dispatcher.rs`): one
  image message after each batch, labelled with call ids; a per-step image
  budget; retention before every model call; retry compaction and smart
  routing look at the user's request, never at an image message. When a
  call in the same batch waits for approval, images are noted instead of
  sent: a user message between the tool results of one assistant turn is
  invalid.
- **Engine V2** (`ic/src/bridge/action_images.rs`,
  `ic/crates/lunarwing_engine/src/executor/action_images.rs`,
  `EffectExecutor::take_action_images`): the bridge parks images per thread
  (16 batches, 32 MiB, 300 s). Each run first discards leftovers from an
  earlier run. Before each main model request the orchestrator places a
  step's images, in call order, after the step's last action result
  (structured calls) or its sealed step record (CodeAct). See
  `ENGINE-V2.md`, "Tool-result images".
- **Operator flags**: `McpServerConfig.interactive_only`
  (`Tool::allows_autonomous_execution` and the new
  `Tool::allows_shared_conversation`) and the CLI flags above.
  `mcp update` and reinstalls keep the setting.

## Security notes

- **Screens contain secrets.** Anything visible in a screenshot (passwords
  shown in plain text, tokens in a terminal, private messages) is sent to
  the model provider. This conflicts with the project's "no LLM reads
  secrets" invariant, so it is an explicit trade-off the owner accepts per
  desktop by installing the connector. The leak detector scans text only;
  it cannot see image content.
- **Screen text is untrusted input.** The image message tells the model the
  images are tool output, never instructions, and the connector's tool
  descriptions repeat it. The connector's single-use frame guard means a
  model can only act on what it has just seen.
- **Approvals.** The connector marks every input tool `destructiveHint:
  true`, so the normal approval prompt applies (`UnlessAutoApproved`). Read
  tools (screenshot, status) run without a prompt. With
  `--interactive-only` no tool of the server runs from a routine,
  background job, mission or shared conversation.
- **Logs.** Image bytes are never in tool text, previews, events or
  `Debug` output. Two existing DEBUG/TRACE paths log full provider request
  bodies (the cloud chat client at DEBUG, rig-core at TRACE). They already
  did this for inbound user images.

## Known limits and follow-ups

- stdio and unix MCP transports cap a response line at 4 MiB; an MCP
  server that returns more closes its connection (pre-existing transport
  rule). The connector stays well under it; third-party image servers on
  those transports must too.
- A provider that rejects an image (for example one that validates pixels
  more strictly than the structural check) fails that request; there is no
  automatic retry without images yet.
- Not tested against a real compositor yet: see the connector README's
  acceptance steps.
- Context-size estimators ignore images (true for user attachments too);
  retention keeps the cost bounded.
- Engine V2 drops images on gate resume and restart. v1 drops them on
  approval resume, with a note.
- Image thumbnails in the web UI for what the agent saw are not
  implemented (the existing `ImageGenerated` event is limited to the image
  tools).
- Vision capability detection (`ic/src/llm/vision_models.rs`) is stale and
  unused. A per-model capability flag would let the daemon choose
  `TOOL_RESULT_IMAGES` automatically.
