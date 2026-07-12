/* Run panel: phase stepper, progress bar (coarse phase + heuristic within-phase),
   live log console, master-key reveal, and the final result summary. */
(function () {
  const LW = (window.LW = window.LW || {});

  function fmtElapsed(sec) {
    if (sec < 60) return sec.toFixed(1) + 's';
    const m = Math.floor(sec / 60);
    const s = Math.floor(sec % 60);
    return m + 'm ' + (s < 10 ? '0' : '') + s + 's';
  }

  function classify(line) {
    if (/\b(error|fail(ed|ure)?)\b/i.test(line)) return 'l-err';
    if (/^===/.test(line)) return 'l-phase';
    if (/^---/.test(line)) return 'l-sub';
    if (/\b(complete|completed|added|started|ready|passed|finished|stored|ok)\b/i.test(line)) return 'l-ok';
    return '';
  }

  function activityOf(line) {
    let m;
    if ((m = line.match(/^===\s*(.*?)\s*===$/))) return m[1];
    if ((m = line.match(/^---\s*(.*?)\s*---$/))) return m[1];
    if ((m = line.match(/Compiling\s+(\S+)/))) return 'Compiling ' + m[1];
    if (/receiving objects/i.test(line)) return 'Fetching objects…';
    if ((m = line.match(/^(step\s+\d+\s*\/\s*\d+.*)$/i))) return m[1];
    return null;
  }

  function fractionOf(line) {
    let m;
    if ((m = line.match(/(\d{1,3})\s*%/))) return Math.min(1, Math.max(0, +m[1] / 100));
    if ((m = line.match(/step\s+(\d+)\s*\/\s*(\d+)/i))) {
      const t = +m[2];
      if (t > 0) return Math.min(1, +m[1] / t);
    }
    return null;
  }

  LW.RunPanel = function (el) {
    let total = 1;
    let index = 0;
    let within = 0;
    let startTime = 0;
    let timer = null;
    let finalized = false;
    const verifyChecks = [];

    function reset(title) {
      total = 1;
      index = 0;
      within = 0;
      finalized = false;
      verifyChecks.length = 0;
      el.title.textContent = title || 'Starting…';
      el.stepper.innerHTML = '';
      el.console.textContent = '';
      el.activity.textContent = 'Preparing…';
      el.fill.className = 'progress-fill';
      el.fill.style.width = '0%';
      el.shimmer.classList.remove('hidden');
      el.masterKeyBox.classList.add('hidden');
      el.masterKeyBox.innerHTML = '';
      el.result.classList.add('hidden');
      el.result.innerHTML = '';
      el.cancelBtn.classList.remove('hidden');
      el.cancelBtn.disabled = false;
      el.newBtn.classList.add('hidden');
      startTime = performance.now();
      if (timer) clearInterval(timer);
      timer = setInterval(tick, 100);
    }

    function tick() {
      el.elapsed.textContent = fmtElapsed((performance.now() - startTime) / 1000);
    }

    function ensureSlots(n) {
      while (el.stepper.children.length < n) {
        const li = document.createElement('li');
        li.textContent = '…';
        el.stepper.appendChild(li);
      }
    }

    function paintBar() {
      const frac = Math.min(1, (index - 1 + within) / total);
      el.fill.style.width = (frac * 100).toFixed(1) + '%';
    }

    function phase(ev) {
      total = ev.total || 1;
      index = ev.index || 1;
      within = 0;
      ensureSlots(total);
      const kids = el.stepper.children;
      for (let i = 0; i < kids.length; i++) {
        if (i < index - 1) {
          if (!kids[i].classList.contains('failed')) kids[i].className = 'done';
        } else if (i === index - 1) {
          kids[i].className = 'active';
          kids[i].textContent = ev.name || 'step ' + index;
        }
      }
      el.title.textContent = ev.label || ev.name || 'Working…';
      el.activity.textContent = ev.label || ev.name || '';
      paintBar();
    }

    function log(line) {
      const div = document.createElement('div');
      const cls = classify(line);
      if (cls) div.className = cls;
      div.textContent = line;
      el.console.appendChild(div);
      if (el.autoscroll.checked) el.console.scrollTop = el.console.scrollHeight;

      const f = fractionOf(line);
      if (f !== null) {
        within = f;
        paintBar();
      }
      const act = activityOf(line);
      if (act) el.activity.textContent = act;
    }

    function verify(checks) {
      for (const c of checks) {
        verifyChecks.push(c);
        log((c.ok ? '[verify] PASS ' : '[verify] FAIL ') + c.label + (c.detail ? ' — ' + c.detail : ''));
      }
    }

    function ensureCredBox() {
      el.masterKeyBox.classList.remove('hidden');
      if (!el.masterKeyBox.querySelector('.cred-list')) {
        el.masterKeyBox.innerHTML =
          '<h4>⚠ Tenant credentials — shown once</h4>' +
          '<div class="cred-list"></div>' +
          '<div class="warn">Store these now. The secrets master key decrypts this tenant\'s vault and is not recoverable. The gateway Web UI token is the bearer used to open the tenant dashboard.</div>';
      }
      return el.masterKeyBox.querySelector('.cred-list');
    }

    function upsertCred(list, key, title, value, note) {
      let row = list.querySelector('[data-cred="' + key + '"]');
      if (!row) {
        row = document.createElement('div');
        row.className = 'cred-row';
        row.dataset.cred = key;
        row.innerHTML =
          '<div class="cred-title"></div>' +
          '<code></code>' +
          (note ? '<div class="cred-note"></div>' : '');
        list.appendChild(row);
      }
      row.querySelector('.cred-title').textContent = title;
      row.querySelector('code').textContent = value;
      const noteEl = row.querySelector('.cred-note');
      if (noteEl) noteEl.textContent = note || '';
    }

    function masterKey(key) {
      const list = ensureCredBox();
      upsertCred(list, 'master_key', 'Secrets master key', key, '');
    }

    function gatewayAuthToken(ev) {
      const list = ensureCredBox();
      const host = (ev && ev.host) || '127.0.0.1';
      const port = ev && ev.port ? Number(ev.port) : 0;
      const note = port
        ? 'Gateway Web UI: http://' + host + ':' + port + '/  (Authorization: Bearer <token>)'
        : 'Gateway Web UI bearer token (port not yet known — check mt-admin tokens)';
      upsertCred(list, 'gateway_auth_token', 'Gateway Web UI token', (ev && ev.token) || '', note);
    }

    function finalize(ok, phases, cancelled) {
      if (finalized) return;
      finalized = true;
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
      el.shimmer.classList.add('hidden');
      el.cancelBtn.classList.add('hidden');
      el.newBtn.classList.remove('hidden');

      const kids = el.stepper.children;
      if (ok) {
        for (const k of kids) if (!k.classList.contains('failed')) k.className = 'done';
        el.fill.className = 'progress-fill done';
        el.fill.style.width = '100%';
      } else {
        if (index >= 1 && kids[index - 1]) kids[index - 1].className = 'failed';
        el.fill.className = 'progress-fill error';
      }

      const rows = (phases || [])
        .map(
          (p) =>
            '<div class="result-line"><span>' +
            p.name +
            '</span><span class="' +
            (p.ok ? 'tag-ok">OK' : 'tag-fail">FAIL (' + p.code + ')') +
            '</span></div>'
        )
        .join('');
      const verifyRows = verifyChecks
        .map(
          (c) =>
            '<div class="result-line"><span>' +
            c.label +
            '</span><span class="' +
            (c.ok ? 'tag-ok">PASS' : 'tag-fail">FAIL') +
            '</span></div>'
        )
        .join('');
      let verdict;
      if (cancelled) verdict = '<span class="verdict-fail">Cancelled</span>';
      else verdict = ok ? '<span class="verdict-ok">Success</span>' : '<span class="verdict-fail">Failed</span>';

      el.result.className = 'result-summary ' + (ok ? 'ok' : 'fail');
      el.result.classList.remove('hidden');
      el.result.innerHTML =
        '<h3>Result: ' + verdict + '</h3>' + rows + (verifyRows ? '<h3 style="margin-top:12px">Verification</h3>' + verifyRows : '');
    }

    function done(ev) {
      finalize(!!ev.ok, ev.phases || [], !!ev.cancelled);
    }

    function error(msg) {
      log('ERROR: ' + msg);
      finalize(false, [], false);
    }

    return { reset, phase, log, verify, masterKey, gatewayAuthToken, done, error };
  };
})();
