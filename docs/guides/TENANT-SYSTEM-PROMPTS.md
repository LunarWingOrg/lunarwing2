# Edit a tenant's system-prompt sources

Available in v2.1.4.0 (WI#220). Open the tenant's web gateway, authenticate,
and select **Settings → System Prompt**. The displayed **Workspace scope** is
the database scope selected by your token, not necessarily the OS account name.

**Re-checked 2026-10-01 (v2.1.4.0):** written with the goal-9 implementation
and re-verified against the unchanged prompt-editor handlers, workspace
preview composition, and Engine V2 initialization snapshot; no changes needed.

## Editing

1. Select a source file. Missing files appear as empty and are created on save.
2. Edit its text and choose **Save file**. Each save replaces that one file.
   Other files' drafts remain available while switching files or tabs; `*`
   identifies unsaved changes. Failed saves retain the draft.
3. Inspect **Saved workspace prompt preview**. It refreshes after a successful
   save and uses saved database content, not drafts. Set a preview timezone
   (IANA name, default `UTC`) or group context as needed.
4. For **Engine V2**, restart the tenant through mt-admin and then create a new
   conversation:

   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
   ```

   The bridge loads workspace instructions once when it initializes the thread
   manager (`ic/src/bridge/router.rs`). A new conversation alone cannot refresh
   an already initialized manager. Existing/persisted threads keep their System
   message even after a restart (`executor/loop_engine.rs`).

| Source | Purpose |
|---|---|
| `AGENTS.md` | Behavior and working instructions |
| `SOUL.md` | Core values, personality and tone |
| `USER.md` | Context about the person being assisted |
| `IDENTITY.md` | Agent name and identity |
| `TOOLS.md` | Environment/tool guidance; does not change tool availability |
| `MEMORY.md` | Long-term memory; omitted from group-context previews |
| `BOOTSTRAP.md` | First-run instructions, suppressed when onboarding is complete |

**Revert draft** restores the selected file's last-loaded content locally.
**Reload saved files** reloads all sources and asks before discarding drafts.
Drafts are held only in the current page and cleared when authentication expires.
Saves follow the existing workspace's last-write-wins behavior; reload before
editing if another operator or the agent has updated the files. To undo a saved
edit, save the previous text again and repeat the Engine V2 activation steps.
Empty content removes that file's contribution without deleting its database row.

## What the preview means

The API calls `Workspace::system_prompt_for_context_tz()`, including its ordered
identity sections, bootstrap gating, today's/yesterday's notes, and eligible
profile tiers. Source editing always uses the authenticated workspace's **primary
scope**, including `MEMORY.md`. The preview preserves the runtime workspace
builder's read-scope behavior: missing primary memory, daily notes and profiles
can fall back to authorized read scopes. Identity/configuration files never do.
The gateway carries the initialized owner's bootstrap-completed flag into its
pooled preview workspace for the same scope.

Group context excludes `MEMORY.md` and profile/onboarding personalization, but
still includes other identity sources and daily notes. This control previews
workspace composition; it does not change channel routing or the live engine.
Engine V2 currently loads the workspace component in direct context using UTC
dates at manager initialization. The legacy dispatcher selects group context and
timezone per request.

This is **not the full effective prompt of a running thread**. Engine templates,
generated tool documentation, selected skills, channel constraints and gated
runtime overlays are separate inputs. Overlay auditing is tracked by WI#268;
this editor does not change its self-modification gates. Daily/profile documents
remain available in the existing **Memory** tab.

## Authenticated API

All routes use the gateway's existing bearer-token authentication and workspace
resolution; clients cannot choose a different tenant by passing a user ID.
Successful responses carry `Cache-Control: no-store`.

| Request | Response |
|---|---|
| `GET /api/prompts` | `{scope, files: [{path, content, exists, updated_at}]}` |
| `GET /api/prompts/preview?is_group_chat=false&timezone=UTC` | `{scope, kind: "workspace", timezone, is_group_chat, prompt}` |
| `POST /api/prompts/write` with `{"path":"SOUL.md","content":"..."}` | Saved `{path, content, exists, updated_at}` |

Writes accept only the seven exact filenames above and at most **256 KiB of
UTF-8 content**. They use the normal workspace prompt-injection validation and
search indexing on both PostgreSQL and libSQL. Unknown write fields (including
`force`, `layer`, and `user_id`) are rejected. Expected errors include `400` for
an invalid source/timezone, `413` for oversized content, `422` for rejected
content/request fields, and `503` when no workspace is available.

The changes use existing workspace storage, with no schema migration. Reverting
the application version removes the new UI/API while retaining saved documents.
