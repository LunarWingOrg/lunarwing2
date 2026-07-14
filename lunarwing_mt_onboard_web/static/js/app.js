/* App orchestrator: token handling, view routing, and the per-job WebSocket
   that drives the run panel, moon phase, and bat mascot. */
(function () {
  const LW = window.LW || {};
  const $ = (id) => document.getElementById(id);

  const url = new URL(window.location.href);
  let token = url.searchParams.get('token') || sessionStorage.getItem('lw_token') || '';
  if (token) sessionStorage.setItem('lw_token', token);

  LW.startStarfield($('starfield'));
  const moon = LW.Moon($('moon-host'), $('moon-label'));
  const bat = LW.Bat($('bat-host'), $('bat-mood'));
  const tips = LW.Tips($('tips-panel'));
  // Wave hello on first load, then Lunar eases into the ambient idle cycle.
  bat.greet();

  // Appearance settings: mascot + tips visibility (splash is handled at load
  // by splash.js). Mascot hides Lunar's dock elements without stopping its JS.
  LW.initSettings($('settings-btn'), $('settings-panel'), [
    { key: 'splash', label: 'splash', default: true },
    {
      key: 'mascot', label: 'mascot', default: true,
      onChange: (on) => {
        $('bat-host').classList.toggle('hidden', !on);
        $('bat-mood').classList.toggle('hidden', !on);
      },
    },
    {
      key: 'tips', label: 'tips', default: true,
      onChange: (on) => (on ? tips.show() : tips.hide()),
    },
  ]);

  const panel = LW.RunPanel({
    title: $('run-title'),
    stepper: $('phase-stepper'),
    activity: $('progress-activity'),
    elapsed: $('progress-elapsed'),
    fill: $('progress-fill'),
    shimmer: $('progress-shimmer'),
    console: $('log-console'),
    autoscroll: $('autoscroll'),
    masterKeyBox: $('masterkey-box'),
    result: $('result-summary'),
    cancelBtn: $('btn-cancel'),
    newBtn: $('btn-new'),
  });

  let tenants = [];
  let demo = false;
  let ws = null;
  let longTimer = null;
  const LONG_PHASES = new Set(['build-tenant', 'build-darkirc', 'upgrade', 'export', 'import']);

  const withToken = (path) => path + (path.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(token);

  async function init() {
    if (!token) $('token-error').classList.remove('hidden');
    try {
      const r = await fetch(withToken('/api/config'));
      if (r.status === 401) {
        $('token-error').classList.remove('hidden');
        return;
      }
      const cfg = await r.json();
      demo = !!cfg.demo;
      const badge = $('mode-badge');
      badge.textContent = demo ? 'DEMO' : 'REAL';
      badge.className = 'badge ' + (demo ? 'badge-demo' : 'badge-real');
      badge.classList.remove('hidden');
    } catch (e) {
      /* config unavailable; keep going */
    }
    try {
      const r = await fetch(withToken('/api/tenants'));
      if (r.ok) tenants = (await r.json()).tenants || [];
    } catch (e) {
      /* tenant list optional */
    }
  }

  function show(view) {
    ['view-picker', 'view-wizard', 'view-run'].forEach((v) => $(v).classList.toggle('hidden', v !== view));
  }

  function closeWs() {
    if (ws) {
      try { ws.close(); } catch (e) { /* noop */ }
      ws = null;
    }
    if (longTimer) { clearTimeout(longTimer); longTimer = null; }
  }

  function scheduleSleepy(phaseName) {
    if (longTimer) { clearTimeout(longTimer); longTimer = null; }
    if (LONG_PHASES.has(phaseName)) longTimer = setTimeout(() => bat.set('sleeping'), 6000);
  }

  document.querySelectorAll('.mode-card').forEach((card) => {
    card.addEventListener('click', () => {
      LW.mountWizard($('wizard-host'), card.dataset.mode, { tenants, demo, onRun });
      show('view-wizard');
    });
  });

  document.querySelectorAll('[data-action="to-picker"]').forEach((b) => {
    b.addEventListener('click', () => {
      closeWs();
      // Back at the picker — wave hello again, then resume idle cycling.
      bat.greet();
      moon.setPhase(0, 1);
      show('view-picker');
    });
  });

  $('btn-cancel').addEventListener('click', () => {
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ action: 'cancel' }));
    $('btn-cancel').disabled = true;
  });

  async function onRun(mode, payload) {
    show('view-run');
    panel.reset('Starting ' + mode + '…');
    bat.stopIdle();
    bat.set('excited');
    moon.setPhase(0, 1);
    let jobId;
    try {
      const r = await fetch(withToken('/api/' + mode), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (!r.ok) {
        panel.error('server rejected request (' + r.status + ')');
        bat.set('angry');
        return;
      }
      jobId = (await r.json()).job_id;
    } catch (e) {
      panel.error('failed to start job: ' + e);
      bat.set('angry');
      return;
    }
    connect(jobId);
  }

  function connect(jobId) {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    ws = new WebSocket(proto + '://' + location.host + withToken('/api/jobs/' + jobId + '/ws'));
    ws.onmessage = (m) => {
      let ev;
      try { ev = JSON.parse(m.data); } catch (e) { return; }
      handle(ev);
    };
    ws.onerror = () => panel.log('[client] websocket error');
  }

  function handle(ev) {
    switch (ev.type) {
      case 'phase':
        panel.phase(ev);
        moon.setPhase(ev.index, ev.total);
        bat.set('excited');
        scheduleSleepy(ev.name);
        break;
      case 'log':
        panel.log(ev.line);
        break;
      case 'verify':
        panel.verify(ev.checks || []);
        break;
      case 'master_key':
        panel.masterKey(ev.key);
        break;
      case 'gateway_auth_token':
        panel.gatewayAuthToken(ev);
        break;
      case 'secret_stored':
        panel.log('secret stored: ' + ev.name);
        break;
      case 'done':
        if (longTimer) { clearTimeout(longTimer); longTimer = null; }
        panel.done(ev);
        if (ev.ok) {
          moon.setPhase(1, 1);
          // Job finished OK — Lunar throws the success nod, then holds content.
          bat.celebrate();
        } else {
          bat.set('angry');
        }
        break;
      case 'error':
        if (longTimer) { clearTimeout(longTimer); longTimer = null; }
        panel.error(ev.message);
        bat.set('angry');
        break;
      default:
        break;
    }
  }

  init();
  show('view-picker');
})();
