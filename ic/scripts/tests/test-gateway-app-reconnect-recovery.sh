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
  'showApproval', 'humanizeToolName', 'queryByDataAttribute'];
const functions = names.map(name => {
  const match = source.match(new RegExp('^function ' + name + '\\([^]*?^}', 'm'));
  assert(match, `missing shipped function ${name}`);
  return match[0];
}).join('\n');

class Element {
  constructor(fragment = false) {
    this.fragment = fragment; this.children = []; this.attrs = {}; this.style = {};
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
  addEventListener() {}
  remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); }
}

function harness() {
  const chat = new Element();
  const elements = new Map([['chat-messages', chat]]);
  const requests = [];
  const authCards = new Map();
  const document = {
    getElementById(id) {
      if (id === 'connection-banner') return null;
      if (!elements.has(id)) elements.set(id, new Element());
      return elements.get(id);
    },
    createElement: () => new Element(),
    createDocumentFragment: () => new Element(true),
    querySelectorAll(selector) { return chat.children.filter(child => child.className === selector.slice(1)); },
    querySelector(selector) {
      const match = selector.match(/\.approval-card\[data-request-id="([^"]+)"\]/);
      return match ? chat.children.find(child => child.attrs['data-request-id'] === match[1]) || null : null;
    },
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
    apiFetch(url) { return new Promise((resolve, reject) => requests.push({ url, resolve, reject })); },
    hasActiveStreamForThread: () => false,
    renderSkeleton: () => new Element(), createMessageElement: () => new Element(),
    createToolCallsSummaryElement: () => new Element(), createReasoningSummaryElement: () => new Element(),
    getAuthCard: name => authCards.get(name), getConfigureOverlay: () => null,
    handleAuthRequired(data) { authCards.set(data.extension_name, data); },
    removeAuthCard(name) { authCards.delete(name); }, closeConfigureModal() {},
  });
  for (const name of ['clearSuggestionChips', 'resetStreamingState', 'stashStreamingState',
    'resetActivityState', 'maybeInsertTimeSeparator', 'showWelcomeCard', 'showActivityThinking',
    'scheduleChatScrollToBottom', 'removeScrollSpinner', 'finalizeActivityGroup',
    'debouncedLoadThreads', 'refreshCurrentSettingsTab', 'clearStreamingSnapshot']) context[name] = () => {};
  vm.runInContext(functions, context);
  return { context, requests, authCards,
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
    assert.equal(h.authCards.get('gitlab').instructions, 'Paste token');
    h.context.currentThreadId = 'thread-b'; h.context.loadHistory();
    h.request('history').resolve(history); await flush();
    assert.equal(h.cards().length, 1, 'thread switch recovers cached approval after history render');
    h.request('pending-gates').reject(new Error('offline')); await flush();
    assert.equal(h.cards().length, 1, 'failed refresh must preserve known pending state');
    h.context.loadPendingGates();
    h.request('pending-gates').resolve({ gates: [approval('gate-b', 'thread-b')] }); await flush();
    assert.equal(h.authCards.size, 0, 'offline auth completion must clear its prompt');
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
})().catch(error => { console.error(error); process.exitCode = 1; });
NODE
