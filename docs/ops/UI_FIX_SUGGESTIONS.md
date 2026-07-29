# Web Gateway UI — Fix Suggestions (v2.0.3.0, item #40)

Date: 2026-07-29
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

## Suggested implementation order

1. **H1** (vendor DOMPurify/marked + non-empty fallback) — small diff, fixes total message loss offline.
2. **M4** (logs stick-to-top) — few lines, big daily-usability win.
3. **M2 + M3** (modal/toast consolidation + i18n keys) — one sweep, mostly mechanical.
4. **H2** (mobile "More" menu + status dot) and **M1** (table scroll wrappers) — mobile pass, bundle together.
5. **L1–L3** — opportunistic cleanups.
6. **L4** — only alongside planned gateway auth work.

## Out of scope / intentionally not suggested

- Framework migration (React/Vue/etc.) or bundler introduction — the dependency-free static UI is a deliberate fit for the gateway; fixes above keep it that way.
- Streaming markdown render performance — the stream path already batches via `requestAnimationFrame` (`_streamRenderFrame`, `app.js:1310-1326`) and renders once at finalize; no action needed.
- IME/Enter handling — already correct (`app.js:2433-2436` handles Safari's `compositionend`/`keyCode 229` quirk).
