# Web Gateway UI — Fix Suggestions (v2.0.3.0, items #40 + #25)

Date: 2026-07-29 (first pass, item #40); updated 2026-07-29 with a second-pass audit (item #25, see "Additional findings" below).
Scope: browser UI of the web gateway — `ic/src/channels/web/static/` (`index.html`, `app.js`, `style.css`, `i18n/`).
Method: static audit of the shipped assets; every finding below cites the exact file:line it was verified against. This document is suggestions only — no code was changed.

Priorities: **High** = user-visible breakage in supported environments. **Medium** = degraded UX or inconsistency. **Low** = polish / hardening.

---

## High

### H1. CDN-loaded DOMPurify/marked break the UI in offline / private-network installs

`index.html:17-28` loads DOMPurify from `cdnjs.cloudflare.com` and `marked` from `cdn.jsdelivr.net`, while fonts are deliberately self-hosted (`index.html:9` comment). This is inconsistent, and it breaks exactly the environments LunarWing targets (harness and MT installs seed `ALLOW_PRIVATE_IPS=1`; air-gapped hosts have no internet):

- If DOMPurify fails to load, `sanitizeRenderedHtml()` returns `''` (`app.js:1124-1125`), so **every rendered assistant message displays as blank** — silent content loss with no error surfaced.
- If `marked` fails to load, `renderMarkdown()` falls back to `escapeHtml(text)` (`app.js:1109`), which is then injected via `innerHTML` (e.g. `app.js:1325`) with no `<br>` conversion — messages render as one unformatted blob.
- External CDN fetches also leak the gateway's existence/timing to a third party on every page load.

Suggested fix: vendor both libraries into `ic/src/channels/web/static/vendor/` (pinned versions, with their licenses) and reference them locally, matching the self-hosted font approach. Additionally, change the DOMPurify-missing fallback from `''` to `escapeHtml(text)` so degraded mode still shows message content.

### H2. On mobile (≤768px) the Logs tab is unreachable and there is no connection indicator

At `style.css:5559-5600` the mobile bottom tab bar hides `.status-logs-btn`, `.status`, the language switcher, and the theme toggle. The hidden Logs button (`index.html:117`) is the **only** entry point to the Logs tab, so mobile users cannot open Logs at all; and with `.status` hidden they also lose the SSE connected/reconnecting indicator entirely — a reconnect loop looks identical to a healthy session.

Suggested fix: keep the five primary tabs in the bottom bar but add an overflow ("More") affordance that exposes Logs, language, and theme; retain a compact status dot (even icon-only) on mobile so connection state is always visible.

---

## Medium

### M1. Jobs and Routines tables are unusable on narrow screens

`.jobs-container` (`style.css:2021-2025`) and `.routines-container` (`style.css:2691`) set only `overflow-y: auto`; the 6-column jobs table and 8-column routines table (`index.html:231-243`, `index.html:278-292`) have `width: 100%` and no `overflow-x` wrapper, and no `@media` block restyles them. On phones the columns are crushed into unreadable slivers or overflow the panel.

Suggested fix: wrap both tables in a horizontally scrollable container (`.table-scroll { overflow-x: auto; }`) as a minimal fix; a card-per-row layout under 768px is the nicer fix but larger.

### M2. Native `alert()`/`confirm()` bypass the app's own modal/toast system, with hardcoded English

A styled confirmation modal exists (`index.html:432-441`, used elsewhere), yet ~9 call sites still use blocking native dialogs:

- `app.js:401`, `app.js:412` — `alert(I18n.t('error.startConversation'))` (translated, but native).
- `app.js:924`, `app.js:928` — image size/count limit alerts, hardcoded English template strings.
- `app.js:2947` — `confirm('Clear all logs?')`; `app.js:3950` — `confirm('Cancel this job?')`; `app.js:4607` — `confirm('Delete routine …')`; `app.js:5356`, `app.js:5440` — `confirm('Install skill …')`. All hardcoded English.

Native dialogs block the event loop, render unstyled on mobile, and are untranslated here despite the zh-CN locale.

Suggested fix: route all confirmations through the existing `#confirm-modal` component and all notifications through `showToast`; add the missing i18n keys (with interpolation support, since several strings embed names/sizes).

### M3. i18n gaps: hardcoded English strings in dynamic UI

Beyond the dialogs above, several rendered strings are not keyed for translation:

- `app.js:2474` — "Loading older messages..." scroll spinner.
- `app.js:539`, `app.js:550` — "Connection lost. Reconnecting... (attempt N)" banner.
- `app.js:2593` — "No files in workspace" memory-tree empty state.
- `index.html:265` — "Filter by target..." placeholder; `index.html:360` — "URL to .tar.gz bundle" placeholder; `index.html:253-256` — "Server: ERROR/WARN/INFO/DEBUG" option labels.

Suggested fix: add keys to `i18n/en.js` and `i18n/zh-CN.js` and wire `data-i18n` / `I18n.t()`; a quick grep for `'> *[A-Z][a-z]+` in `index.html` and string literals passed to `textContent`/`innerHTML` in `app.js` will catch the remainder.

### M4. Logs auto-scroll yanks the view to the top on every batch

`flushLogEntries()` sets `output.scrollTop = 0` unconditionally whenever the Auto-scroll checkbox is on (`app.js:2903-2906`). Newest entries prepend at the top, so a user who has scrolled down to read older entries is snapped back to the top on every incoming log batch — the Logs tab is effectively unreadable under load unless auto-scroll is manually disabled. The chat view already solves this by tracking "near bottom" per scroll event (`app.js:2467`, `_chatAutoScroll`).

Suggested fix: track "near top" (scrollTop < threshold) on the logs scroll event and only stick to the top when the user is already there — mirroring the chat behavior.

---

## Low

### L1. Memory-tree load failure looks like an empty workspace

`loadMemoryTree()` swallows fetch errors (`.catch(() => {})`, `app.js:2586`), so a failed `/api/memory/list` renders as "No files in workspace" (`app.js:2592-2594`) — indistinguishable from a genuinely empty workspace.

Suggested fix: render an explicit error row ("Failed to load workspace — retry") with a retry button, and/or a toast.

### L2. Log target filter applies O(n) DOM writes per keystroke without debounce

`applyLogFilters()` re-queries all `#logs-output .log-entry` nodes and toggles `style.display` on each (`app.js:2968-2976`), scheduled per `input` event (`app.js:2958`) with only rAF coalescing. With the entry cap (LOG_MAX_ENTRIES) this is survivable, but typing into the target filter still does full-list DOM writes per frame.

Suggested fix: add a ~150ms debounce on the target-filter input (the memory search already debounces this way, `app.js:2564-2572`).

### L3. Conflicting mobile tab-bar overflow rules

`style.css:4474-4476` sets `.tab-bar { overflow-x: auto }` for ≤768px while the later, equal-specificity block at `style.css:5570` sets `overflow-x: visible`, silently overriding it. It happens to work today because the same block hides most buttons, but the dead rule invites regressions.

Suggested fix: delete the losing rule and add a comment stating the intended mobile tab-bar behavior.

### L4. (Security-adjacent) SSE bearer token travels in the URL query string

Both EventSource connections embed the gateway token in the URL (`app.js:491`, `app.js:2815`: `/api/chat/events?token=…`, `/api/logs/events?token=…`). Native `EventSource` cannot set headers, so the token lands in server access logs, proxy logs, and browser history. This is a known trade-off, not a regression — flag it here because any UI auth rework should address it.

Suggested fix (when touching gateway auth): issue a short-lived, single-purpose SSE ticket via an authenticated POST, or move the web UI to cookie-scoped session auth with `EventSource({ withCredentials: true })`. Do not weaken the existing bearer-token checks in the process (per repo security invariants).

---

## Additional findings (second pass, item #25)

Verified against the same shipped assets on 2026-07-29. None of these duplicate the findings above; IDs continue the existing scheme.

## Medium

### M5. No 401 handling: an expired/invalid token degrades into an infinite "Reconnecting" loop

`apiFetch()` stamps `err.status` on failures (`app.js:276-283`), but the only statuses ever checked anywhere are 404 (`app.js:327`) and 429 (`app.js:850`) — 401/403 are never special-cased (grep for `401` in `app.js` returns nothing). If the gateway token changes or is revoked while the UI is open (e.g. server restart with a regenerated `GATEWAY_AUTH_TOKEN`), every API call fails with a raw error toast and the SSE `onerror` handler (`app.js:531-554`) keeps cycling "Connection lost. Reconnecting..." forever. The user is never told the real problem or sent back to the auth screen.

Suggested fix: in `apiFetch()`, on 401/403 clear `sessionStorage('lunarwing_token')`, close both EventSources, and show the auth screen with a "session expired — re-enter token" notice; also have the SSE `onerror` path give up and do the same after the server answers 401 (EventSource surfaces this as a readyState of `CLOSED`).

### M6. `loadThreads()` silently reverts the translated "Assistant" label to English

`loadThreads()` contains `if (labelEl.textContent !== 'Assistant') labelEl.textContent = 'Assistant';` (`app.js:2290-2291`). The label is an i18n-managed element (`index.html:157`, `data-i18n="chat.assistant"`; zh-CN translates it to "助手", `i18n/zh-CN.js:83`). After switching to zh-CN, the very next `loadThreads()` poll sees non-English text, assumes it is stale, and resets it to hardcoded English — the translation never survives a thread-list refresh.

Suggested fix: delete the reset (the element is already managed by `I18n.updatePageContent()`), or compare/set via `I18n.t('chat.assistant')` instead of the literal.

### M7. Gateway-status and TEE popovers are hover-only — unreachable on touch devices

Both popovers are opened exclusively via `mouseenter`/`mouseleave`: gateway dashboard (`app.js:4727-4732`) and TEE attestation (`app.js:4831-4836`). There is no click/tap or keyboard path, and neither trigger is focusable (`index.html:130-141` — `.tee-shield` is a plain `div`, `.status` is a `div` with no `tabindex`). On phones/tablets the dashboard data (version, connections, cost, token usage) and the attestation report are simply inaccessible; on desktop, keyboard users can't reach them either.

Suggested fix: toggle `.visible` on click (with outside-click dismissal, mirroring the language menu at `i18n-app.js:70-77`), and give both triggers `tabindex="0"`, `role="button"`, and Enter/Space handlers.

### M8. Memory viewer read race: last *response* wins, not last click

`readMemoryFile()` (`app.js:2679-2703`) issues `apiFetch('/api/memory/read?...')` with no sequencing or cancellation. Clicking file A then file B quickly renders whichever response arrives last — the breadcrumb (`app.js:2682`) is set synchronously at click time, so the viewer can show file A's content under file B's breadcrumb, and `currentMemoryPath`/`currentMemoryContent` (`app.js:2680`, `app.js:2689`) can disagree (a following Save then writes the wrong pairing). The same pattern exists in the lazy tree expand (`app.js:2664-2676`) and the job-files viewer (`app.js:4235-4240`).

Suggested fix: keep a monotonically increasing read counter (or an `AbortController`) per viewer and ignore responses that are not the latest request.

### M9. More hardcoded English beyond the M3 list

The second pass found additional untranslated strings not covered by M3, concentrated in the job/routine detail views and status widgets:

- Job detail: sub-tab labels "Overview/Activity/Files" (`app.js:4022`), "Back" (`app.js:4002`), "Retry" (`app.js:4007`), "Browse Files" (`app.js:4010`), "Cancel" (`app.js:3935`), "Description" (`app.js:4078`), "State Transitions" (`app.js:4092`), "Select a file to view" (`app.js:4121`), "No project files" (`app.js:4139`).
- Job activity toolbar: "All Events/Messages/Tool Calls/Results" (`app.js:4256-4259`), "Auto-scroll" (`app.js:4261`), "Send follow-up prompt..." / "Send" / "Done" (`app.js:4267-4269`).
- Routines: "Enable/Disable" (`app.js:4461`), "Run"/"Delete" (`app.js:4476-4478`).
- Gateway/TEE widgets: "Token Usage", "in:"/"out:" (`app.js:4704`, `app.js:4716-4717`), "Loading attestation report..." / "Could not load attestation report" (`app.js:4785`, `app.js:4793`), "Reconnected" flash (`app.js:505`).
- `copyMessage()` button feedback "Copied/Copy/Failed" (`app.js:1148-1152`) — inconsistent with `copyCodeBlock()`, which already uses `I18n.t('btn.copied')` (`app.js:1133-1134`).
- Toasts: "Job cancelled" (`app.js:3953`), "Failed to cancel job" (`app.js:3958`), "Job restarted as …" (`app.js:3965`), "Failed to load routine" (`app.js:4489`), "Saved …" / "Save failed" (`app.js:2727`, `app.js:2731`), "Pairing approved" / "Approve failed" (`app.js:3784`, `app.js:3787`).

Suggested fix: same as M3 — key them in `i18n/en.js` / `i18n/zh-CN.js` (several need interpolation); the `copyMessage` fix is a two-line change to match `copyCodeBlock`.

## Low

### L5. (Security-adjacent) `escapeHtml()` is not attribute-safe, but is used inside HTML attributes

`escapeHtml()` is `div.textContent = str; return div.innerHTML` (`app.js:6464-6468`), which escapes `&`, `<`, `>` but **not** double quotes. It is interpolated into double-quoted attribute contexts throughout: `data-path` in the memory breadcrumb (`app.js:2741`), `data-id`/`title` in the jobs list and detail (`app.js:3935`, `app.js:3939-3940`, `app.js:4007`), `href` for job `browse_url` (`app.js:4010`), and `title`/`data-id`/`data-name` in the routines list (`app.js:4464`, `app.js:4467`, `app.js:4476-4478`). A workspace filename or routine name containing `"` breaks out of the attribute and injects arbitrary markup (e.g. a `style` attribute or new elements). The gateway CSP (`ic/src/channels/web/server.rs:652-664`, `script-src` without `'unsafe-inline'`) blocks inline *script* execution, so this is markup injection rather than full XSS — but it is defense-in-depth the templates assume they have and don't.

Suggested fix: extend `escapeHtml()` to also replace `"` with `&quot;` (and `'` with `&#39;`) so it is safe in both text and attribute contexts, or add a dedicated `escapeAttr()` and use it at the attribute call sites above. (Note `escapeHtml(job.browse_url)` at `app.js:4010` also does nothing to prevent a `javascript:` scheme — validate the scheme server-side or prepend origin when building the link.)

### L6. Screen-reader gaps: toasts not announced, stale `aria-expanded`, missing `aria-pressed`

- `#toasts` (`index.html:443`) has no `aria-live`, so success/error toasts (`showToast`, `app.js:6257+`) are silent to screen readers. The auth error (`index.html:45`) likewise has no `role="alert"`.
- `#language-btn` declares `aria-expanded="false"` (`index.html:109-110`) but `toggleLanguageMenu()` (`i18n-app.js:26-31`) never updates it, so it is permanently wrong.
- `#logs-pause-btn` (`index.html:267`) is a binary Pause/Resume toggle whose state is conveyed only by its label text (`app.js:2937-2938`); no `aria-pressed`.

Suggested fix: add `aria-live="polite"` to `#toasts` and `role="alert"` to `#auth-error`; sync `aria-expanded` in `toggleLanguageMenu()` (and on the outside-click close at `i18n-app.js:70-77`); add `aria-pressed` to the pause button and toggle it in `toggleLogsPause()`.

---

## Suggested implementation order

1. **H1** (vendor DOMPurify/marked + non-empty fallback) — small diff, fixes total message loss offline.
2. **M4** (logs stick-to-top) — few lines, big daily-usability win.
3. **M6** (assistant-label i18n revert) — one-line deletion of an actively wrong behavior.
4. **M2 + M3 + M9** (modal/toast consolidation + i18n keys) — one sweep, mostly mechanical.
5. **M5** (401 → auth screen) — pairs naturally with any L4 gateway auth work.
6. **H2** (mobile "More" menu + status dot), **M1** (table scroll wrappers), and **M7** (tap/focus access to popovers) — mobile pass, bundle together.
7. **M8** (memory viewer read sequencing) — small, self-contained.
8. **L1–L3, L5, L6** — opportunistic cleanups (L5 is a one-function fix plus call-site audit).
9. **L4** — only alongside planned gateway auth work.

## Out of scope / intentionally not suggested

- Framework migration (React/Vue/etc.) or bundler introduction — the dependency-free static UI is a deliberate fit for the gateway; fixes above keep it that way.
- Streaming markdown render performance — the stream path already batches via `requestAnimationFrame` (`_streamRenderFrame`, `app.js:1310-1326`) and renders once at finalize; no action needed.
- IME/Enter handling — already correct (`app.js:2433-2436` handles Safari's `compositionend`/`keyCode 229` quirk).
