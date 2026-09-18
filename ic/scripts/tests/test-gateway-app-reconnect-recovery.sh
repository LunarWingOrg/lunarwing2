#!/usr/bin/env bash
# Exercise the shipped browser functions with controlled HTTP/SSE ordering.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
node - "$ROOT/src/channels/web/static/app.js" <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(process.argv[2], 'utf8');
const names = ['connectSSE', 'isCurrentThread', 'loadHistory', 'loadPendingGates',
  'renderPendingGates', 'rememberPendingGate', 'forgetPendingGate',
  'showApproval', 'humanizeToolName', 'queryByDataAttribute', 'handleAuthRequired',
  'handleAuthCompleted', 'showAuthCard', 'removeAuthCard', 'getAuthOverlay', 'getAuthCard',
  'getConfigureOverlay', 'submitAuthToken', 'cancelAuth', 'showAuthCardError', 'setAuthFlowPending'];
const functions = names.map(name => {
  const match = source.match(new RegExp('^function ' + name + '\\([^]*?^}', 'm'));
  assert(match, `missing shipped function ${name}`);
  return match[0];
}).join('\n');

class Element {
  constructor(fragment = false) {
    this.fragment = fragment; this.children = []; this.attrs = {}; this.style = {};
    this.listeners = {}; this.tagName = '';
    this.className = ''; this.scrollTop = 0;
    this.classList = { add() {}, remove() {}, contains() { return false; } };
  }
  set innerHTML(_) { this.replaceChildren(); }
  appendChild(child) {
    if (child.fragment) { for (const item of [...child.children]) this.appendChild(item); }
    else { child.parent = this; this.children.push(child); }
    return child;
  }
  replaceChildren(...children) { this.children = []; children.forEach(child => this.appendChild(child)); }
  setAttribute(key, value) { this.attrs[key] = value; }
  getAttribute(key) { return this.attrs[key]; }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  focus() {}
  matches(selector) {
    const match = selector.match(/^([.\w-]+)(?:\[([^=]+)="([^"]*)"\])?$/);
    if (!match) return false;
    const [, name, attr, value] = match;
    const kind = name.startsWith('.') ? this.className.split(' ').includes(name.slice(1)) : this.tagName === name;
    return kind && (!attr || this.attrs[attr] === value);
  }
  querySelectorAll(selector) {
    return this.children.flatMap(child => [child, ...child.querySelectorAll('*')])
      .filter(child => selector === '*' || child.matches(selector));
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  closest(selector) { return this.matches(selector) ? this : this.parent?.closest(selector); }
  remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); }
}

function harness() {
  const chat = new Element();
  const body = new Element(); body.appendChild(chat);
  const elements = new Map([['chat-messages', chat]]);
  const requests = [];
  const document = {
    body,
    getElementById(id) {
      if (id === 'connection-banner') return null;
      if (!elements.has(id)) elements.set(id, new Element());
      return elements.get(id);
    },
    createElement(tag) { const element = new Element(); element.tagName = tag; return element; },
    createDocumentFragment: () => new Element(true),
    querySelectorAll: selector => body.querySelectorAll(selector),
    querySelector: selector => body.querySelector(selector),
  };
  class EventSource {
    constructor() { this.handlers = {}; }
    addEventListener(name, fn) { this.handlers[name] = fn; }
    close() {}
    emit(name, data) { this.handlers[name]({ data: JSON.stringify(data) }); }
  }
  const context = vm.createContext({
    document, EventSource, console, Map, Set, CSS: { escape: s => s },
    window: { CSS: { escape: s => s } }, I18n: { t: key => key },
    setTimeout: () => 0, clearTimeout() {},
    token: 'test', eventSource: null, currentThreadId: 'thread-a', currentTab: 'chat',
    currentThreadIsReadOnly: false, sseHasConnectedBefore: false,
    _pendingGateRequestSequence: 0, _pendingGateRevision: 0, _pendingGateRetryTimer: null, _pendingGates: new Map(),
    _resolvedGateRequests: new Set(), _historyRequestSequence: 0,
    _streamingThreadId: null, _streamSnapshots: new Map(), _chatAutoScroll: true, _responseRenderedAt: new Map(),
    _connectionLostTimer: null, _connectionLostAt: null, _reconnectAttempts: 0, isRestarting: false,
    hasMore: false, oldestTimestamp: null, loadingOlder: false, unreadThreads: new Map(),
    apiFetch(url, options) { return new Promise((resolve, reject) => requests.push({ url, options, resolve, reject })); },
    hasActiveStreamForThread: () => false,
    renderSkeleton: () => new Element(), createMessageElement: () => new Element(),
    createToolCallsSummaryElement: () => new Element(), createReasoningSummaryElement: () => new Element(),
    closeConfigureModal() {}, showConfigureModal() { throw new Error('engine gate must retain its credential controls'); },
  });
  for (const name of ['clearSuggestionChips', 'resetStreamingState', 'stashStreamingState',
    'resetActivityState', 'maybeInsertTimeSeparator', 'showWelcomeCard', 'showActivityThinking',
    'scheduleChatScrollToBottom', 'removeScrollSpinner', 'finalizeActivityGroup',
    'debouncedLoadThreads', 'refreshCurrentSettingsTab', 'clearStreamingSnapshot',
    'enableChatInput', 'showToast', 'loadExtensions']) context[name] = () => {};
  vm.runInContext(functions, context);
  return { context, requests,
    authCard: () => document.querySelector('.auth-card'),
    cards: () => chat.children.filter(child => child.className === 'approval-card'),
    request(part) {
      const index = requests.findIndex(request => request.url.includes(part));
      assert(index >= 0, `no pending request for ${part}`);
      return requests.splice(index, 1)[0];
    },
  };
}

const approval = (id = 'gate-a', thread = 'thread-a') => ({
  request_id: id, thread_id: thread, gate_name: 'approval', tool_name: 'shell', description: 'Run command',
  parameters: '{"command":"echo ok"}', resume_kind: { Approval: { allow_always: false } },
});
const auth = (id = 'auth-a', thread = 'thread-a', url = null) => ({
  request_id: id, thread_id: thread, gate_name: 'authentication', resume_kind: { Authentication: {
    credential_name: 'gitlab', instructions: 'Paste token', auth_url: url,
  } },
});
const history = { turns: [{ user_input: 'hi', response: null, state: 'Processing' }], pending_approval: null };
const flush = async () => { for (let i = 0; i < 4; i++) await new Promise(setImmediate); };

(async () => {
  for (const gatesFirst of [true, false]) {
    const h = harness();
    h.context.connectSSE();
    h.context.sseHasConnectedBefore = true;
    h.context.eventSource.onopen(); // exercise actual reconnect wiring
    const gates = h.request('pending-gates');
    const past = h.request('history');
    if (gatesFirst) {
      gates.resolve({ gates: [approval()] }); await flush();
      assert.equal(h.cards().length, 1);
      past.resolve(history); await flush();
      // The card must survive immediately, even before a refresh completes.
      assert.equal(h.cards().length, 1);
    } else {
      past.resolve(history); await flush();
      gates.resolve({ gates: [approval()] }); await flush();
    }
    h.request('pending-gates').resolve({ gates: [approval()] }); await flush();
    assert.equal(h.cards().length, 1);
    h.context.eventSource.emit('approval_needed', { ...approval(), allow_always: false });
    assert.equal(h.cards().length, 1, 'live prompt must dedupe recovered card');
    assert(!h.cards()[0].children.at(-1).children.some(child => child.className === 'always'));
    console.log(`PASS reconnect ${gatesFirst ? 'gates then history' : 'history then gates'}`);
  }

  {
    const h = harness();
    h.context.connectSSE(); h.context.eventSource.onopen();
    h.request('pending-gates').resolve({ gates: [approval()] }); await flush();
    assert.equal(h.cards().length, 1, 'first connection must recover pending gates');
    h.context.loadPendingGates();
    const old = h.request('pending-gates');
    h.context.eventSource.emit('gate_resolved', { request_id: 'gate-a' });
    old.resolve({ gates: [approval()] }); await flush();
    assert.equal(h.cards().length, 0, 'stale snapshot must not resurrect resolved approval');
    h.request('pending-gates').resolve({ gates: [] }); await flush();
    console.log('PASS first connect and resolution racing recovery');
  }

  {
    const h = harness();
    h.context.connectSSE(); h.context.eventSource.onopen();
    const old = h.request('pending-gates');
    h.context.eventSource.emit('gate_required', approval('new-live'));
    h.context.eventSource.emit('approval_needed', { ...approval('new-live'), allow_always: false });
    old.resolve({ gates: [] }); await flush();
    assert.equal(h.cards().length, 1, 'stale snapshot must not discard a new live gate');
    h.request('pending-gates').resolve({ gates: [approval('new-live')] }); await flush();
    console.log('PASS live gate racing older empty snapshot');
  }

  {
    const h = harness();
    h.context.loadPendingGates();
    h.request('pending-gates').resolve({ gates: [approval('gate-b', 'thread-b'), {
      request_id: 'auth-a', thread_id: 'thread-a', gate_name: 'authentication', resume_kind: { Authentication: {
        credential_name: 'gitlab', instructions: 'Paste token', auth_url: 'https://example.test/oauth',
      } },
    }] }); await flush();
    assert.equal(h.cards().length, 0, 'other thread approval must not leak into current chat');
    assert.equal(h.authCard().querySelector('.auth-instructions').textContent, 'Paste token');
    h.context.currentThreadId = 'thread-b'; h.context.loadHistory();
    h.request('history').resolve(history); await flush();
    assert.equal(h.cards().length, 1, 'thread switch recovers cached approval after history render');
    h.request('pending-gates').reject(new Error('offline')); await flush();
    assert.equal(h.cards().length, 1, 'failed refresh must preserve known pending state');
    h.context.loadPendingGates();
    h.request('pending-gates').resolve({ gates: [approval('gate-b', 'thread-b')] }); await flush();
    assert.equal(h.authCard(), null, 'offline auth completion must clear its prompt');
    console.log('PASS auth recovery, thread isolation/switch, failed refresh');
  }

  {
    const h = harness();
    h.context.loadPendingGates();
    h.request('pending-gates').resolve({ gates: [approval()] }); await flush();
    assert.equal(h.cards().length, 1);
    h.context.loadPendingGates();
    h.request('pending-gates').resolve({ gates: [] }); await flush();
    assert.equal(h.cards().length, 0, 'resolved/expired offline gates must disappear on recovery');
    console.log('PASS authoritative snapshot removes obsolete gate');
  }

  {
    const h = harness();
    h.context.connectSSE();
    h.context.eventSource.emit('stream_gap', { missed_events: 256 });
    // Gate recovery must proceed independently of history's response.
    h.request('pending-gates').resolve({ gates: [approval()] }); await flush();
    assert.equal(h.cards().length, 1);
    console.log('PASS stream-gap recovery fetches gates independently');
  }
  for (const url of [null, 'https://example.test/oauth']) {
    const h = harness();
    h.context.loadPendingGates();
    h.request('pending-gates').resolve({ gates: [auth('auth-a', 'thread-a', url), approval('gate-b', 'thread-b')] });
    await flush();
    const card = h.authCard();
    assert.equal(card.getAttribute('data-request-id'), 'auth-a');
    card.querySelector('input').value = ' credential ';
    card.querySelector('.auth-submit').listeners.click();
    const submit = h.request('auth-token');
    assert.deepEqual(JSON.parse(JSON.stringify(submit.options.body)), {
      extension_name: 'gitlab', token: 'credential', request_id: 'auth-a', thread_id: 'thread-a',
    });
    submit.resolve({ success: true, activated: true }); await flush();
    assert.equal(h.context._pendingGates.size, 2, 'queue acceptance must not resolve a gate');
    h.request('pending-gates').resolve({ gates: [approval('gate-b', 'thread-b')] }); await flush();
    assert.equal(h.authCard(), null);
    console.log(`PASS recovered ${url ? 'OAuth' : 'token-only'} auth retains identity through submit`);
  }

  {
    const h = harness();
    h.context.loadPendingGates(); h.request('pending-gates').resolve({ gates: [auth()] }); await flush();
    h.context.loadPendingGates(); const stale = h.request('pending-gates');
    h.authCard().querySelector('.auth-cancel').listeners.click();
    const cancel = h.request('auth-cancel');
    assert.deepEqual(JSON.parse(JSON.stringify(cancel.options.body)), {
      extension_name: 'gitlab', request_id: 'auth-a', thread_id: 'thread-a',
    });
    assert(h.authCard(), 'keep prompt until backend confirms cancellation');
    cancel.resolve({ success: true }); await flush();
    stale.resolve({ gates: [auth()] }); await flush();
    h.request('pending-gates').resolve({ gates: [auth()] }); await flush();
    h.context.renderPendingGates();
    assert.equal(h.authCard(), null, 'cancelled prompt must not reappear');
    assert.equal(h.context._pendingGates.size, 0);
    console.log('PASS confirmed cancellation tombstones stale snapshots');
  }

  {
    const h = harness();
    h.context.loadPendingGates(); h.request('pending-gates').resolve({ gates: [auth()] }); await flush();
    h.authCard().querySelector('.auth-cancel').listeners.click();
    h.request('auth-cancel').reject(new Error('offline')); await flush();
    assert.equal(h.context._pendingGates.size, 1);
    assert.equal(h.authCard().querySelector('.auth-cancel').disabled, false);
    assert.match(h.authCard().querySelector('.auth-error').textContent, /offline/);
    console.log('PASS failed cancellation retains retryable prompt');
  }

  for (const credential of ['gitlab', 'other']) {
    const h = harness();
    const sibling = auth('auth-b', 'thread-b');
    sibling.resume_kind.Authentication.credential_name = credential;
    h.context.loadPendingGates();
    h.request('pending-gates').resolve({ gates: [auth(), sibling] }); await flush();
    h.authCard().querySelector('.auth-cancel').listeners.click();
    const cancel = h.request('auth-cancel');
    h.context.currentThreadId = 'thread-b'; h.context.renderPendingGates();
    cancel.resolve({ success: true }); await flush();
    assert.equal(h.authCard().getAttribute('data-request-id'), 'auth-b');
    h.context.handleAuthCompleted({ extension_name: 'gitlab', success: true });
    assert.equal(h.authCard().getAttribute('data-request-id'), 'auth-b', 'extension completion must not dismiss another gate');
    assert.equal(h.context._pendingGates.size, 1);
    assert.equal(h.context.authFlowPending, true, 'remaining prompt must retain auth mode');
    console.log(`PASS delayed cancellation and completion preserve ${credential} sibling`);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
NODE
