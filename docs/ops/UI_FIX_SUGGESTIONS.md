# Web Gateway UI — Fix Suggestions (v2.0.4.0)

Date: 2026-07-29 (first pass); updated 2026-07-29 (second pass, item #25 — added M5–M9, L5–L9, and a CSP cross-reference under H1); updated 2026-08-04 (third pass, item #3 — added H3, M10–M13, and L10).
Scope: browser UI of the web gateway — `ic/src/channels/web/static/` (`index.html`, `app.js`, `style.css`, `i18n/`).
Method: static audit of the shipped assets; every finding below cites the exact file:line it was verified against. Third-pass findings were verified against source commit `b400f25`. This document is suggestions only — no code was changed.

**Implementation status (2026-08-06, item #9):** all findings implemented except **L4** (SSE token in URL), which remains deferred until planned gateway auth rework, as this document itself recommends. Notes: H1 vendored DOMPurify 3.2.3 / marked 17.0.2 into `static/vendor/` (SRI retained) and CSP `script-src` is now `'self'` only; L6 kept the popover triggers as `<div>`s with button semantics because the TEE popover nests an interactive copy button (invalid inside `<button>`); L5 removed the dead `scheduleCodeHighlight()` path rather than vendoring highlight.js.

Priorities: **High** = user-visible breakage in supported environments. **Medium** = degraded UX or inconsistency. **Low** = polish / hardening.

---

## High

### H1. CDN-loaded DOMPurify/marked break the UI in offline / private-network installs

`index.html:17-28` loads DOMPurify from `cdnjs.cloudflare.com` and `marked` from `cdn.jsdelivr.net`, while fonts are deliberately self-hosted (`index.html:9` comment). This is inconsistent, and it breaks exactly the environments LunarWing targets (harness and MT installs seed `ALLOW_PRIVATE_IPS=1`; air-gapped hosts have no internet):

- If DOMPurify fails to load, `sanitizeRenderedHtml()` returns `''` (`app.js:1124-1125`), so **every rendered assistant message displays as blank** — silent content loss with no error surfaced.
- If `marked` fails to load, `renderMarkdown()` falls back to `escapeHtml(text)` (`app.js:1109`), which is then injected via `innerHTML` (e.g. `app.js:1325`) with no `<br>` conversion — messages render as one unformatted blob.
- External CDN fetches also leak the gateway's existence/timing to a third party on every page load.

Suggested fix: vendor both libraries into `ic/src/channels/web/static/vendor/` (pinned versions, with their licenses) and reference them locally, matching the self-hosted font approach. Additionally, change the DOMPurify-missing fallback from `''` to `escapeHtml(text)` so degraded mode still shows message content. When vendoring lands, also drop the two CDN origins from the gateway CSP `script-src` (`ic/src/channels/web/server.rs:654-655`) — they are only whitelisted for these scripts.

### H2. On mobile (≤768px) the Logs tab is unreachable and there is no connection indicator

At `style.css:5559-5600` the mobile bottom tab bar hides `.status-logs-btn`, `.status`, the language switcher, and the theme toggle. The hidden Logs button (`index.html:117`) is the **only** entry point to the Logs tab, so mobile users cannot open Logs at all; and with `.status` hidden they also lose the SSE connected/reconnecting indicator entirely — a reconnect loop looks identical to a healthy session.

Suggested fix: keep the five primary tabs in the bottom bar but add an overflow ("More") affordance that exposes Logs, language, and theme; retain a compact status dot (even icon-only) on mobile so connection state is always visible.

### H3. Memory reads can pair one file's content with another file's path

`readMemoryFile(path)` immediately replaces `currentMemoryPath`, updates the breadcrumb, exposes Edit, and exits edit mode (`app.js:2810-2817`), but it leaves the previous `currentMemoryContent` and viewer in place until the asynchronous read completes (`app.js:2819-2829`). After selecting file B while file A is displayed, clicking Edit before B loads copies A's content into the editor (`app.js:2836-2843`) while Save posts to the now-current path B (`app.js:2851-2857`). There is a second race: if A's request completes after B's, its unguarded callback replaces B's content in the viewer. Either path can lead to accidentally overwriting B with A's content.

Suggested fix: cancel the prior read with `AbortController` or guard both success and failure callbacks with a monotonically increasing request ID. Clear `currentMemoryContent`, show a loading state, and disable Edit while a read is pending; capture the path associated with the editor and refuse to save if it no longer matches the selected file.

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

### M5. Jobs table header has 6 columns; rows render 5 — columns misaligned

`index.html:231-243` declares six headers (ID, Title, **Source**, Status, Created, Actions), but `renderJobsList()` (`app.js:3929-3946`) emits only five `<td>`s per row — there is no Source cell (the API's `JobInfo` has no source field, `ic/src/channels/web/types.rs:669-676`). Every row shifts left by one: the status badge renders under "Source", the date under "Status", the buttons under "Created", and the "Actions" header sits over nothing. The detail-view rebuild path in `loadJobs()` (`app.js:3884-3890`) already emits a 5-column header without Source — but with hardcoded English labels that bypass the original markup's `data-i18n` keys, plus a hardcoded "No jobs found" empty state.

Suggested fix: delete the stale Source `<th>` from `index.html`; make the rebuild path in `loadJobs()` emit the same i18n-keyed header as the static markup (or better, only rebuild `#jobs-summary`/`#jobs-tbody` and never touch the header).

### M6. History messages ignore server timestamps — everything shows page-load time

`createMessageElement()` stamps every message with `new Date()` (`app.js:2061`) and accepts no timestamp, and `loadHistory()` calls it without one (`app.js:1986`, `app.js:1993`, `app.js:2026`, `app.js:2033`). After any reload, every historical message displays the time the page was loaded, and `maybeInsertTimeSeparator()` is also called with no timestamp (`app.js:1985`, `app.js:1992`), so all separators collapse to "Today" (`app.js:1158-1177`) — a multi-day history renders as one day. The history API already returns `started_at`/`completed_at` per turn (`ic/src/channels/web/types.rs:58-66`).

Suggested fix: thread the turn timestamps through `createMessageElement()` and `maybeInsertTimeSeparator()` (use `started_at` for the user message, `completed_at ?? started_at` for the assistant message).

### M7. No auth-expiry path: a revoked token means infinite reconnects and raw error toasts

`apiFetch()` records the HTTP status on errors (`app.js:276-287`) but no caller handles 401/403, and the SSE `onerror` handler reconnects forever with no backoff and no max-attempt cutoff (`app.js:531-554`). If the gateway token is rotated or the session is invalidated, the UI shows "Connection lost. Reconnecting... (attempt N)" indefinitely while every poll (threads, gateway status, pairing) keeps firing failing requests — and the user is never returned to the auth screen to re-enter the token.

Suggested fix: on a 401/403 response, clear `sessionStorage.lunarwing_token` (written at `app.js:186`), close both EventSources, and show the auth screen with a "session expired" notice; in `connectSSE()`, stop reconnecting once a 401 is observed instead of counting attempts forever.

### M8. Restart progress modal can hang forever with no escape hatch

`confirmRestart()` / `tryShowRestartModal()` show `#restart-loader` (`app.js:410-473`); the loader markup has no close control (`index.html:76-92`) and the only code path that hides it is SSE reconnect (`app.js:511-520`). If the process fails to come back (crash loop, port conflict, bad upgrade), the user is stuck on "Restarting LunarWing" until they think to reload the page. `Escape` doesn't help: `closeModals()` only closes the shortcuts overlay and the *confirmation* modal (`app.js:6432-6440`).

Suggested fix: add a watchdog (e.g. 90s) that swaps the loader to an error state — "Restart is taking longer than expected" with "Reload page" and "Dismiss" buttons — and include `#restart-loader` in `closeModals()`. Drop the `console.log('[confirmRestart] ...')` diagnostics (`app.js:433`, `app.js:443`) while there.

### M9. (Security-adjacent) `escapeHtml()` is not attribute-safe, but is used inside quoted attributes

`escapeHtml()` serializes via `textContent`→`innerHTML` (`app.js:6464-6468`), which escapes `&`, `<`, `>` but **not** `"`. Several callers interpolate its output into double-quoted HTML attributes:

- `app.js:2741` — memory breadcrumb `data-path="..."`. Workspace filenames are agent/tool-controlled and may legally contain `"` on Linux.
- `app.js:4464` — routine `title="..."` from `trigger_raw`; `app.js:4478` — `data-name="..."` from the routine name (assistant/user-supplied).
- `app.js:3935-3940`, `app.js:4575` — job/routine IDs (UUIDs, safe today, but the same pattern).

A value containing `"` breaks out of the attribute and injects arbitrary ones. Today's impact is bounded because the gateway CSP omits `'unsafe-inline'` from `script-src` (`ic/src/channels/web/server.rs:654-655`), so injected event handlers won't execute — but that's defense-in-depth, not a reason to keep emitting malformed HTML.

Suggested fix: add an `escapeAttr()` helper that also replaces `"` and `'`, use it for every attribute interpolation, or build these rows with DOM APIs (`setAttribute`/`dataset`) like the extension cards already do.

### M10. Retrying a failed message silently drops its image attachments

`sendMessage()` captures the text, renders the optimistic user message, and clears the input (`app.js:837-844`). It then copies staged images into the request and clears `stagedImages` before the request succeeds (`app.js:846-850`). On failure, the Retry link restores only `content` and calls `sendMessage()` again (`app.js:856-881`): a mixed text/image message retries as text-only, while an image-only message retries as nothing because the empty-payload guard returns immediately (`app.js:838`).

Suggested fix: keep the complete failed request payload (including images) with the optimistic message and have Retry resubmit that payload directly. Do not discard the attachment previews/payload until the server accepts the send, or explicitly restore them on failure.

### M11. Multi-file selection can exceed the five-image attachment limit

`handleImageFiles()` loops over every selected file and checks `stagedImages.length` before starting a `FileReader` (`app.js:928-939`), but images are not pushed until each asynchronous `onload` callback runs (`app.js:940-947`). If several files are selected together, every iteration can observe the same pre-read length and pass the limit check, after which all callbacks append; the UI's stated `MAX_STAGED_IMAGES = 5` limit (`app.js:925-926`) is therefore not enforced.

Suggested fix: compute available slots once, validate and slice the selected files synchronously, then start readers only for the accepted subset. Show one translated notice for rejected overflow rather than one dialog per file; alternatively include in-flight readers in a reserved-slot counter.

### M12. Selecting another memory file silently discards unsaved edits

Every file selection immediately changes `currentMemoryPath` and calls `cancelMemoryEdit()` (`app.js:2810-2817`), which hides the editor without checking whether its textarea differs from the loaded content (`app.js:2846-2849`). The editor wiring only handles explicit Edit, Save, and Cancel clicks (`app.js:6662-6664`); there is no dirty-state tracking or navigation warning. A stray click in the tree or breadcrumb can therefore lose an entire edit with no recovery path.

Suggested fix: track the editor's baseline and dirty state. Before changing files, show the existing confirmation modal with Save/Discard/Stay choices (or prevent selection until the user saves/cancels), and apply the same guard to any action that leaves the editor.

### M13. Toast notifications are not exposed to assistive technology

The toast host is a plain `<div id="toasts">` (`index.html:445`), and `showToast()` appends plain `<div>`/`<span>` nodes before removing them four seconds later (`app.js:6430-6461`). Neither the container nor each toast has `aria-live`, `role="status"`, or `role="alert"`, so screen-reader users can miss send failures, save errors, install results, and other state changes that are communicated only through toasts.

Suggested fix: make the toast host a polite live region for normal updates and give urgent error toasts `role="alert"`; mark decorative icons `aria-hidden="true"`. Keep messages in the accessibility tree long enough to be announced, and test successive queued toasts rather than replacing live-region content too quickly.

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

### L5. Code syntax highlighting is dead code — `hljs` is never loaded

`scheduleCodeHighlight()` (`app.js:1199-1212`) is called from both markdown render paths (`app.js:1326`, `app.js:2075`) but always returns at its first line: highlight.js is not in `index.html:13-29` and there is no vendored copy under `static/`. The state (`app.js:124-125`) and per-frame loop are pure overhead, and code blocks are never highlighted.

Suggested fix: either vendor a pinned highlight.js subset alongside the H1 vendoring work, or delete `scheduleCodeHighlight()` and its state.

### L6. Gateway-status and TEE popovers are hover-only — unreachable on touch and keyboard

Both popovers open exclusively on `mouseenter`/`mouseleave` (`app.js:4727-4732`, `app.js:4831-4837`), and their triggers are non-focusable `<div>`/`<span>` elements (`index.html:130-141`). On touch devices there is no hover, and keyboard users can't tab to the triggers — version/uptime/cost details and the TEE attestation report are inaccessible to both groups (compounds H2 on mobile, where `.status` is hidden entirely).

Suggested fix: make both triggers `<button>`s, toggle the popover on click and focus/blur, and mirror open state into `aria-expanded`.

### L7. Background polling continues in hidden tabs

The pairing poll (10s, `app.js:3792-3799`) and the gateway-status poll (30s, `app.js:4643-4646`) run unconditionally; nothing in `app.js` listens for `document.visibilitychange`. A minimized or backgrounded tab keeps hitting the gateway forever — wasted requests on metered links and extra load on small hosts, multiplied across open tabs.

Suggested fix: skip poll ticks while `document.hidden` and fire one immediate refresh when the tab becomes visible again.

### L8. `target="_blank"` without `rel="noopener"` on server-provided URLs

The job "browse" links (`app.js:1692-1697`, `app.js:4009-4011`) and the OAuth setup link (`app.js:1806-1810`) open in a new tab without `rel="noopener"`, leaving `window.opener` pointed at the gateway tab. The ClawHub skill link already does it right (`app.js:5268-5270`) — the others are just inconsistent.

Suggested fix: add `rel="noopener noreferrer"` at the three sites.

### L9. `window.prompt()` in the skill-publish flow (extend M2's sweep)

`publishSkill()` collects slug and version through two blocking `window.prompt()` dialogs (`app.js:5041-5043`) — the M2 call-site list covers `alert`/`confirm` but not these. Fold them into the same modal-consolidation pass (a small two-field dialog, or extend `#confirm-modal` with optional inputs).

### L10. The tab ARIA roles do not implement keyboard tab navigation

`applyAriaAttributes()` adds `tablist`, `tab`, `tabpanel`, `aria-selected`, and `aria-hidden` (`app.js:6617-6629`), but it does not connect tabs to panels with `aria-controls`/`aria-labelledby` or implement roving `tabindex`. All six tab buttons remain in the normal Tab sequence, and their only navigation handler is click (`app.js:2628-2633`), so arrow keys, Home, and End do not move among tabs as keyboard and screen-reader users expect.

Suggested fix: give each tab a stable ID and `aria-controls`, point each panel back with `aria-labelledby`, keep only the active tab at `tabindex="0"`, and handle Left/Right (or Up/Down where appropriate), Home, and End while moving focus and selection together.

---

## Suggested implementation order

1. **H3 + M12** (memory read race + unsaved-edit guard) — first because the current UI can overwrite one file with another file's content.
2. **H1** (vendor DOMPurify/marked + non-empty fallback + tighten CSP `script-src`) — small diff, fixes total message loss offline.
3. **M10 + M11** (attachment retry payload + synchronous slot reservation) — one image-upload reliability pass.
4. **M5** (jobs table header) — one-line `index.html` fix plus a small `loadJobs()` rebuild cleanup; visible on every Jobs view.
5. **M7** (auth-expiry path) — small, prevents a stuck-forever reconnect loop after token rotation.
6. **M4** (logs stick-to-top) — few lines, big daily-usability win.
7. **M2 + M3 + M13 + L9** (modal/toast consolidation, live-region semantics, and i18n keys, including `window.prompt`) — one mostly mechanical sweep.
8. **M6** (history timestamps) — localized change, high polish value for long-lived threads.
9. **M8** (restart-loader watchdog) — small, prevents a dead-end modal on failed restarts.
10. **H2** (mobile "More" menu + status dot), **M1** (table scroll wrappers), **L6 + L10** (accessible popovers and tab keyboard behavior) — mobile/accessibility pass.
11. **M9** (`escapeAttr`) + **L8** (`noopener`) — small hardening sweep.
12. **L1–L3**, **L5** (hljs dead code), **L7** (visibility-gated polling) — opportunistic cleanups.
13. **L4** — only alongside planned gateway auth work.

## Out of scope / intentionally not suggested

- Framework migration (React/Vue/etc.) or bundler introduction — the dependency-free static UI is a deliberate fit for the gateway; fixes above keep it that way.
- Streaming markdown render performance — the stream path already batches via `requestAnimationFrame` (`_streamRenderFrame`, `app.js:1310-1326`) and renders once at finalize; no action needed.
- IME/Enter handling — already correct (`app.js:2433-2436` handles Safari's `compositionend`/`keyCode 229` quirk).
