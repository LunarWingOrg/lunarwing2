#!/usr/bin/env bash
set -euo pipefail

# Behavioral regression for the gateway client's SSE-reconnect approval
# recovery (WI#164 / MR !1002 review P2).
#
# The race: on reconnect, `loadHistory()` and `loadPendingGates()` run
# concurrently. When the pending-gates response lands first, the recovered
# approval card renders into `#chat-messages` — but the slower history
# response then calls `container.replaceChildren(fragment)`, erasing the card.
# The legacy `pending_approval` history field does not reliably restore an
# Engine V2 gate, and no second pending-gate fetch followed that replacement.
#
# This harness loads the real `app.js` (plus the real i18n pack) in a Node
# `vm` sandbox with a minimal DOM/API stub, then drives both response
# orderings:
#
#   1. gates-then-history: the recovered card must survive the history
#      replacement (re-rendered by the follow-up pending-gates fetch).
#   2. history-then-gates: the card renders exactly once (deduplicated).
#
# The DOM stub implements only what app.js touches on the exercised paths;
# unrelated UI surfaces are inert. Missing node skips cleanly so the
# shell-harnesses gate stays green on runners without node (same convention
# as test-coord-knowledge-matrix.sh).

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATIC_DIR="$TEST_DIR/../../src/channels/web/static"

command -v node >/dev/null 2>&1 || {
  printf 'SKIPPED: requires node\n'
  exit 0
}

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

[[ -f "$STATIC_DIR/app.js" ]] || { fail "app.js missing at $STATIC_DIR/app.js"; exit 1; }
[[ -f "$STATIC_DIR/i18n/index.js" ]] || { fail "i18n/index.js missing"; exit 1; }
[[ -f "$STATIC_DIR/i18n/en.js" ]] || { fail "i18n/en.js missing"; exit 1; }

APP_JS="$STATIC_DIR/app.js"
I18N_JS="$STATIC_DIR/i18n/index.js"
EN_JS="$STATIC_DIR/i18n/en.js"

node - "$APP_JS" "$I18N_JS" "$EN_JS" <<'NODE'
'use strict';
const fs = require('fs');
const vm = require('vm');

const [appPath, i18nPath, enPath] = process.argv.slice(2);

let failures = 0;
function pass(msg) { console.log('PASS: ' + msg); }
function fail(msg) { failures += 1; console.log('FAIL: ' + msg); }

// ---------------------------------------------------------------------------
// Minimal DOM stub. Elements carry children/attrs/classList; the container
// supports replaceChildren (the erasure vector) and querySelector on
// descendants. Everything else is inert.
// ---------------------------------------------------------------------------
class ClassList {
  constructor() { this._set = new Set(); }
  add(...names) { names.forEach((n) => this._set.add(n)); }
  remove(...names) { names.forEach((n) => this._set.delete(n)); }
  toggle(name, force) {
    const want = force === undefined ? !this._set.has(name) : !!force;
    if (want) this._set.add(name); else this._set.delete(name);
    return want;
  }
  contains(name) { return this._set.has(name); }
}

class StubElement {
  constructor(tagName) {
    this.tagName = String(tagName).toUpperCase();
    this.children = [];
    this.parentNode = null;
    this.attributes = new Map();
    this.classList = new ClassList();
    this.style = {};
    this.dataset = {};
    this._text = '';
    this._innerHTML = '';
    this.listeners = new Map();
    this.disabled = false;
    this.value = '';
    this.placeholder = '';
    this.type = '';
    this.title = '';
    this.scrollTop = 0;
    this.scrollHeight = 0;
    this.clientHeight = 0;
    this.offsetLeft = 0;
  }
  // app.js assigns `el.className = "a b"` directly; keep classList in sync so
  // `.class` selector matching works either way.
  get className() {
    return Array.from(this.classList._set).join(' ');
  }
  set className(value) {
    this.classList._set = new Set(String(value).split(/\s+/).filter(Boolean));
  }
  get isConnected() {
    return documentRoot === this || this.parentNode !== null;
  }
  get firstChild() { return this.children[0] || null; }
  get childNodes() { return this.children.slice(); }
  get textContent() {
    if (this.children.length === 0) return this._text;
    return this.children.map((c) => c.textContent).join('');
  }
  set textContent(v) {
    this.children = [];
    this._text = String(v);
    this._innerHTML = '';
  }
  get innerHTML() {
    if (this.children.length === 0) return this._innerHTML;
    return this.children.map((c) => c.outerHTML).join('');
  }
  set innerHTML(v) {
    this.children = [];
    this._innerHTML = String(v);
  }
  get outerHTML() { return `<${this.tagName}>${this.innerHTML}</${this.tagName}>`; }
  appendChild(child) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.children.push(child);
    return child;
  }
  append(...nodes) { nodes.forEach((n) => this.appendChild(n)); }
  removeChild(child) {
    const i = this.children.indexOf(child);
    if (i >= 0) { this.children.splice(i, 1); child.parentNode = null; }
    return child;
  }
  remove() {
    if (this.parentNode) this.parentNode.removeChild(this);
  }
  replaceChildren(...nodes) {
    this.children.forEach((c) => { c.parentNode = null; });
    this.children = [];
    this._innerHTML = '';
    this._text = '';
    nodes.forEach((n) => this.appendChild(n));
  }
  insertBefore(node, ref) {
    if (node.parentNode) node.parentNode.removeChild(node);
    node.parentNode = this;
    const i = ref ? this.children.indexOf(ref) : -1;
    if (i >= 0) this.children.splice(i, 0, node);
    else this.children.push(node);
    return node;
  }
  setAttribute(name, value) {
    this.attributes.set(name, String(value));
    if (name.startsWith('data-')) this.dataset[name.slice(5)] = String(value);
  }
  getAttribute(name) { return this.attributes.has(name) ? this.attributes.get(name) : null; }
  hasAttribute(name) { return this.attributes.has(name); }
  removeAttribute(name) { this.attributes.delete(name); }
  addEventListener(type, fn) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(fn);
  }
  querySelector(selector) {
    for (const child of this.children) {
      if (matchesSelector(child, selector)) return child;
      const deep = child.querySelector(selector);
      if (deep) return deep;
    }
    return null;
  }
  querySelectorAll(selector) {
    const out = [];
    for (const child of this.children) {
      if (matchesSelector(child, selector)) out.push(child);
      out.push(...child.querySelectorAll(selector));
    }
    return out;
  }
  focus() {}
  click() {}
}

function matchesSelector(el, selector) {
  // Support simple selectors: tag, .class, #id, tag[attr=value],
  // tag[attr], and combinations like tag.class.
  let rest = selector;
  const tagMatch = rest.match(/^([a-zA-Z0-9-]+)/);
  if (tagMatch) {
    if (el.tagName.toLowerCase() !== tagMatch[1].toLowerCase()) return false;
    rest = rest.slice(tagMatch[1].length);
  }
  while (rest.length > 0) {
    if (rest[0] === '.') {
      const m = rest.match(/^\.([a-zA-Z0-9_-]+)/);
      if (!m) return false;
      if (!el.classList.contains(m[1])) return false;
      rest = rest.slice(m[0].length);
    } else if (rest[0] === '#') {
      const m = rest.match(/^#([a-zA-Z0-9_-]+)/);
      if (!m) return false;
      if (el.id !== m[1]) return false;
      rest = rest.slice(m[0].length);
    } else if (rest[0] === '[') {
      const end = rest.indexOf(']');
      if (end < 0) return false;
      const inner = rest.slice(1, end);
      rest = rest.slice(end + 1);
      const eq = inner.indexOf('=');
      const key = (eq >= 0 ? inner.slice(0, eq) : inner).trim();
      const value = eq >= 0 ? inner.slice(eq + 1).trim().replace(/^["']|["']$/g, '') : null;
      if (value === null) {
        if (!el.hasAttribute(key)) return false;
      } else if (el.getAttribute(key) !== value) {
        return false;
      }
    } else {
      return false;
    }
  }
  return true;
}

const documentRoot = new StubElement('#document');
const body = new StubElement('body');
documentRoot.appendChild(body);

// Elements app.js binds at load time. Unknown ids are created lazily so the
// many getElementById(...).addEventListener(...) registrations no-op safely.
const knownIds = new Map();
function getElementById(id) {
  if (knownIds.has(id)) return knownIds.get(id);
  const el = new StubElement('div');
  el.id = id;
  knownIds.set(id, el);
  body.appendChild(el);
  return el;
}

// Selector-keyed lazy elements so load-time `document.querySelector(...)`
// registrations bind to an inert stub instead of null. Only the exact
// selectors app.js queries unguarded at load time qualify — runtime lookups
// (like approval-card dedup) must still return null when nothing rendered.
const LOAD_TIME_SELECTORS = new Set(['.tab-bar']);
const knownSelectors = new Map();
function querySelector(selector) {
  const found = documentRoot.querySelector(selector);
  if (found) return found;
  if (LOAD_TIME_SELECTORS.has(selector)) {
    if (knownSelectors.has(selector)) return knownSelectors.get(selector);
    const el = new StubElement('div');
    body.appendChild(el);
    knownSelectors.set(selector, el);
    return el;
  }
  return null;
}

const chatMessages = getElementById('chat-messages');
const documentElement = getElementById('html');

const documentStub = {
  getElementById,
  createElement: (tag) => new StubElement(tag),
  createDocumentFragment: () => new StubElement('#fragment'),
  querySelector,
  querySelectorAll: (selector) => documentRoot.querySelectorAll(selector),
  addEventListener: () => {},
  documentElement,
  body,
};

const windowStub = {
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  addEventListener: () => {},
  innerWidth: 1200,
  location: { search: '', hostname: 'localhost', protocol: 'http:' },
  history: { replaceState: () => {} },
  CSS: { escape: (s) => String(s).replace(/[^a-zA-Z0-9_-]/g, (c) => '\\' + c) },
};

const sandbox = {
  document: documentStub,
  window: windowStub,
  CSS: windowStub.CSS,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  sessionStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  navigator: { language: 'en-US', clipboard: { writeText: () => Promise.resolve() } },
  URLSearchParams: class { constructor() {} get() { return null; } },
  URL: class {},
  EventSource: class {
    constructor() { this.listeners = new Map(); }
    addEventListener() {}
    close() {}
  },
  requestAnimationFrame: (fn) => { fn(); return 1; },
  cancelAnimationFrame: () => {},
  setInterval: () => 1,
  clearInterval: () => {},
  setTimeout: () => 1,
  clearTimeout: () => {},
  console,
  Intl,
  Date,
  Math,
  JSON,
  Promise,
  Map,
  Set,
  String,
  Number,
  Boolean,
  Object,
  Array,
  RegExp,
  Error,
  isNaN,
  encodeURIComponent,
};

vm.createContext(sandbox);

// The real i18n pack (registers 'en'), then the real app.js.
vm.runInContext(fs.readFileSync(i18nPath, 'utf8'), sandbox, { filename: 'i18n/index.js' });
vm.runInContext(fs.readFileSync(enPath, 'utf8'), sandbox, { filename: 'i18n/en.js' });
vm.runInContext(fs.readFileSync(appPath, 'utf8'), sandbox, { filename: 'app.js' });

// ---------------------------------------------------------------------------
// Controlled API: queue responses per URL, resolve in the order the test
// wants. apiFetch is redefined inside the sandbox to consume the queue.
// ---------------------------------------------------------------------------
const queue = [];
function enqueue(url, response) {
  queue.push({ url, response });
}
function flush() {
  // Resolve queued promises in FIFO order, one microtask at a time. A queued
  // response whose fetch never happened (the client did not issue the
  // request) is dropped — that is exactly the regression shape case 1
  // asserts against.
  return new Promise((resolve) => {
    const step = () => {
      if (queue.length === 0) return resolve();
      const { url, response } = queue.shift();
      const hadPending = vm.runInContext(
        'window.__resolveFetch("' + url + '", ' + JSON.stringify(response) + ');',
        sandbox
      );
      if (!hadPending) {
        // No fetch was issued for this response; the client never asked.
      }
      setImmediate(step);
    };
    step();
  });
}

vm.runInContext(`
(function () {
  const pending = [];
  window.__resolveFetch = function (url, response) {
    const entry = pending.shift();
    if (!entry) return false;
    entry.resolve({ ok: true, status: 200, json: () => Promise.resolve(response), text: () => Promise.resolve('') });
    return true;
  };
  fetch = function (path) {
    return new Promise((resolve, reject) => {
      pending.push({ url: path, resolve, reject });
    });
  };
})();
`, sandbox);

function approvalCardCount() {
  return vm.runInContext(
    "document.querySelectorAll('.approval-card').length",
    sandbox
  );
}

function setCurrentThread(threadId) {
  vm.runInContext('currentThreadId = ' + JSON.stringify(threadId) + ';', sandbox);
}

// ---------------------------------------------------------------------------
// Case 1: gates-then-history ordering. The recovered card renders first, the
// slower history replacement erases it, and the follow-up pending-gates fetch
// (added by the P2 fix) must re-render it.
// ---------------------------------------------------------------------------
async function caseGatesThenHistory() {
  const gate = {
    request_id: 'req-1',
    thread_id: 'thread-1',
    gate_name: 'approval',
    tool_name: 'shell',
    description: 'run a command',
    parameters: '{}',
    resume_kind: { Approval: { allow_always: false } },
  };

  setCurrentThread('thread-1');
  vm.runInContext('loadPendingGates();', sandbox);
  enqueue('/api/chat/pending-gates', { gates: [gate] });
  await flush();

  const before = approvalCardCount();
  if (before !== 1) {
    fail('case 1: recovered card should render before history returns (got ' + before + ')');
    return;
  }

  vm.runInContext('loadHistory();', sandbox);
  enqueue('/api/chat/history?limit=50&thread_id=thread-1', {
    thread_id: 'thread-1',
    turns: [{ user_input: 'hi', response: 'hello', started_at: new Date().toISOString() }],
    has_more: false,
    pending_approval: null,
  });
  // The P2 fix re-fetches pending gates after the DOM replacement.
  enqueue('/api/chat/pending-gates', { gates: [gate] });
  await flush();

  const after = approvalCardCount();
  if (after === 1) {
    pass('case 1 (gates-then-history): recovered card survives history replacement');
  } else {
    fail('case 1 (gates-then-history): card lost after history replacement (got ' + after + ')');
  }
}

// ---------------------------------------------------------------------------
// Case 2: history-then-gates ordering. The card renders once from the
// pending-gates fetch and the dedup keeps it unique.
// ---------------------------------------------------------------------------
async function caseHistoryThenGates() {
  const gate = {
    request_id: 'req-2',
    thread_id: 'thread-1',
    gate_name: 'approval',
    tool_name: 'shell',
    description: 'run a command',
    parameters: '{}',
    resume_kind: { Approval: { allow_always: false } },
  };

  setCurrentThread('thread-1');
  vm.runInContext('loadHistory();', sandbox);
  enqueue('/api/chat/history?limit=50&thread_id=thread-1', {
    thread_id: 'thread-1',
    turns: [],
    has_more: false,
    pending_approval: null,
  });
  enqueue('/api/chat/pending-gates', { gates: [gate] });
  await flush();

  const count = approvalCardCount();
  if (count === 1) {
    pass('case 2 (history-then-gates): card rendered exactly once');
  } else {
    fail('case 2 (history-then-gates): expected exactly one card (got ' + count + ')');
  }
}

(async () => {
  await caseGatesThenHistory();
  await caseHistoryThenGates();
  if (failures > 0) process.exit(1);
  process.exit(0);
})().catch((err) => {
  console.error('harness error:', err);
  process.exit(1);
});
NODE
