/* Form builder for the five modes. Provision is a multi-step wizard; the
   others are single compact forms. Calls opts.onRun(mode, payload) to launch. */
(function () {
  const LW = (window.LW = window.LW || {});

  const NAME_RE = /^[a-z0-9][a-z0-9-]*$/;
  const RESERVED = ['pg-', 'proxy-', 'nanocode-', 'pebble-', 'opencode-', 'weechat-'];

  function validateName(name) {
    if (!name) return 'tenant name is required';
    if (!NAME_RE.test(name)) return 'lowercase alphanumeric and hyphens only';
    if (RESERVED.some((p) => name.startsWith(p))) return 'name collides with a reserved prefix';
    return null;
  }

  // -- small DOM builders ---------------------------------------------------
  function h(tag, attrs, children) {
    const e = document.createElement(tag);
    if (attrs) for (const k in attrs) {
      if (k === 'class') e.className = attrs[k];
      else if (k === 'html') e.innerHTML = attrs[k];
      else e.setAttribute(k, attrs[k]);
    }
    (children || []).forEach((c) => e.appendChild(typeof c === 'string' ? document.createTextNode(c) : c));
    return e;
  }

  function textField(data, key, label, opts) {
    opts = opts || {};
    const input = h('input', { type: opts.type || 'text', value: data[key] != null ? data[key] : '' });
    if (opts.placeholder) input.placeholder = opts.placeholder;
    input.addEventListener('input', () => {
      data[key] = input.value;
      if (opts.onInput) opts.onInput(input.value, input);
    });
    const field = h('div', { class: 'field' }, [h('label', {}, [label]), input]);
    if (opts.hint) field.appendChild(h('div', { class: 'hint' }, [opts.hint]));
    if (opts.datalist) {
      const id = 'dl-' + key;
      input.setAttribute('list', id);
      const dl = h('datalist', { id });
      opts.datalist.forEach((v) => dl.appendChild(h('option', { value: v })));
      field.appendChild(dl);
    }
    field._input = input;
    return field;
  }

  function checkField(data, key, label, onChange) {
    const box = h('input', { type: 'checkbox' });
    box.checked = !!data[key];
    box.addEventListener('change', () => {
      data[key] = box.checked;
      if (onChange) onChange(box.checked);
    });
    return h('div', { class: 'check-row' }, [box, h('label', {}, [label])]);
  }

  function selectField(data, key, label, options, onChange) {
    const sel = h('select', {});
    options.forEach((o) => {
      const opt = h('option', { value: o.value }, [o.label]);
      if (data[key] === o.value) opt.selected = true;
      sel.appendChild(opt);
    });
    sel.addEventListener('change', () => {
      data[key] = sel.value;
      if (onChange) onChange(sel.value);
    });
    return h('div', { class: 'field' }, [h('label', {}, [label]), sel]);
  }

  // -- Provision (multi-step) ----------------------------------------------
  function mountProvision(host, opts) {
    const data = {
      name: '', gateway_host: '127.0.0.1', docker_group: true,
      enable_darkirc: false,
      xmpp_enabled: false, xmpp_jid: '', xmpp_password: '', xmpp_allow_from: '',
      gotify_enabled: false, gotify_url: '', gotify_title: '',
      w_nanocode: false, w_pebble: false, w_opencode: false, toolchains: false,
      tensorzero_url: 'http://192.168.1.157:3000/openai/v1',
      model_choice: 'lunarwing', model_custom: '',
      llm_api_key: '', secrets_master_key: '',
      ssh_harness: true, health_pipeline: true,
      skip_build: false, skip_start: false,
    };

    const steps = [
      { title: 'Identity', render: stepIdentity, validate: () => validateName(data.name.trim()) },
      { title: 'Channels', render: stepChannels },
      { title: 'Workers', render: stepWorkers },
      { title: 'LLM', render: stepLLM },
      { title: 'Secrets', render: stepSecrets },
      { title: 'Review', render: stepReview },
    ];
    let cur = 0;

    const body = h('div', { class: 'wizard-body' });
    const dots = h('div', { class: 'step-dots' });
    const countEl = h('span', { class: 'step-count' });
    const backBtn = h('button', { class: 'btn btn-secondary' }, ['Back']);
    const nextBtn = h('button', { class: 'btn btn-primary' }, ['Next']);
    backBtn.addEventListener('click', () => { if (cur > 0) { cur--; render(); } });
    nextBtn.addEventListener('click', onNext);

    const wrap = h('div', { class: 'wizard glass' }, [
      h('div', { class: 'wizard-head' }, [h('h2', {}, ['Provision a tenant']), countEl]),
      dots, body,
      h('div', { class: 'wizard-nav' }, [backBtn, nextBtn]),
    ]);
    host.appendChild(wrap);

    function onNext() {
      const step = steps[cur];
      if (step.validate) {
        const err = step.validate();
        if (err) { flagError(err); return; }
      }
      if (cur < steps.length - 1) { cur++; render(); }
      else opts.onRun('provision', buildPayload());
    }

    function flagError(msg) {
      const input = body.querySelector('input');
      if (input) input.classList.add('invalid');
      let e = body.querySelector('.field-error');
      if (!e) { e = h('div', { class: 'field-error' }, [msg]); body.appendChild(e); }
      else e.textContent = msg;
    }

    function render() {
      body.innerHTML = '';
      dots.innerHTML = '';
      steps.forEach((_, i) => {
        const d = h('div', { class: 'step-dot' + (i === cur ? ' active' : i < cur ? ' done' : '') });
        dots.appendChild(d);
      });
      countEl.textContent = 'step ' + (cur + 1) + ' / ' + steps.length;
      steps[cur].render(body);
      backBtn.disabled = cur === 0;
      nextBtn.textContent = cur === steps.length - 1 ? 'Provision ✦' : 'Next';
    }

    function stepIdentity(b) {
      b.appendChild(textField(data, 'name', "Tenant name (lowercase, e.g. 'sphinx')", {
        placeholder: 'sphinx', hint: 'lowercase alphanumeric + hyphens',
      }));
      b.appendChild(textField(data, 'gateway_host', 'Gateway bind host/IP', { placeholder: '127.0.0.1' }));
      b.appendChild(checkField(data, 'docker_group', 'Add tenant user to docker/podman group'));
    }

    function stepChannels(b) {
      b.appendChild(checkField(data, 'enable_darkirc', 'Enable DarkIRC services'));
      b.appendChild(checkField(data, 'xmpp_enabled', 'Enable XMPP bridge', () => render()));
      if (data.xmpp_enabled) {
        const sub = h('div', { class: 'subgroup' });
        sub.appendChild(textField(data, 'xmpp_jid', 'XMPP JID', { placeholder: data.name + '@xmpp.localhost' }));
        sub.appendChild(textField(data, 'xmpp_password', 'XMPP password (blank = auto-generate)', { type: 'password' }));
        sub.appendChild(textField(data, 'xmpp_allow_from', 'Extra allowed DM senders (comma-separated)', {}));
        b.appendChild(sub);
      }
      b.appendChild(checkField(data, 'gotify_enabled', 'Enable Gotify notifications', () => render()));
      if (data.gotify_enabled) {
        const sub = h('div', { class: 'subgroup' });
        sub.appendChild(textField(data, 'gotify_url', 'Gotify URL', { placeholder: 'https://gotify.example.com/' }));
        sub.appendChild(textField(data, 'gotify_title', 'Gotify title override', { placeholder: data.name }));
        b.appendChild(sub);
      }
    }

    function stepWorkers(b) {
      b.appendChild(checkField(data, 'w_nanocode', 'nanocode (NanoGPT)'));
      b.appendChild(checkField(data, 'w_pebble', 'pebble (Rust harness)'));
      b.appendChild(checkField(data, 'w_opencode', 'opencode (sst/opencode)'));
      b.appendChild(checkField(data, 'toolchains', 'Include Rust/Go/C++ toolchains (+~5GB image)'));
    }

    function stepLLM(b) {
      b.appendChild(textField(data, 'tensorzero_url', 'TensorZero upstream URL', {}));
      b.appendChild(selectField(data, 'model_choice', 'LLM model', [
        { value: 'FrontierCODE', label: 'FrontierCODE' },
        { value: 'lunarwing', label: 'lunarwing' },
        { value: 'custom', label: 'Custom…' },
      ], () => render()));
      if (data.model_choice === 'custom') {
        b.appendChild(textField(data, 'model_custom', 'Custom LLM model ID', { placeholder: 'tensorzero::function_name::…' }));
      }
      b.appendChild(textField(data, 'llm_api_key', 'LLM API key (blank if unneeded)', { type: 'password' }));
    }

    function stepSecrets(b) {
      b.appendChild(textField(data, 'secrets_master_key', 'Secrets master key (64-hex, blank = auto-generate)', {
        type: 'password', hint: 'A new key is generated and shown once if left blank.',
      }));
      b.appendChild(checkField(data, 'ssh_harness', 'Provision SSH harness for tenant'));
      b.appendChild(checkField(data, 'health_pipeline', 'Enable host health pipeline'));
    }

    function stepReview(b) {
      const workers = [data.w_nanocode && 'nanocode', data.w_pebble && 'pebble', data.w_opencode && 'opencode'].filter(Boolean);
      const rows = [
        ['Tenant', data.name || '(unset)'],
        ['Gateway host', data.gateway_host],
        ['DarkIRC', String(data.enable_darkirc)],
        ['XMPP', data.xmpp_enabled ? (data.xmpp_jid || '(auto)') : 'disabled'],
        ['Gotify', data.gotify_enabled ? data.gotify_url : 'disabled'],
        ['Workers', workers.join(', ') || 'none'],
        ['Toolchains', String(data.toolchains)],
        ['Model', data.model_choice === 'custom' ? data.model_custom : data.model_choice],
        ['Master key', data.secrets_master_key ? '(provided)' : 'auto-generate'],
        ['SSH / Health', data.ssh_harness + ' / ' + data.health_pipeline],
      ];
      const dl = h('dl', { class: 'summary-grid' });
      rows.forEach((r) => { dl.appendChild(h('dt', {}, [r[0]])); dl.appendChild(h('dd', {}, [String(r[1])])); });
      b.appendChild(dl);
      const adv = h('div', { class: 'subgroup' });
      adv.appendChild(checkField(data, 'skip_build', 'Advanced: skip build-tenant'));
      adv.appendChild(checkField(data, 'skip_start', 'Advanced: skip start-tenant'));
      b.appendChild(adv);
    }

    function buildPayload() {
      const workers = [];
      if (data.w_nanocode) workers.push('nanocode');
      if (data.w_pebble) workers.push('pebble');
      if (data.w_opencode) workers.push('opencode');
      let model = 'tensorzero::function_name::lunarwing';
      if (data.model_choice === 'FrontierCODE') model = 'tensorzero::function_name::FrontierCODE';
      else if (data.model_choice === 'custom') model = data.model_custom.trim();
      return {
        name: data.name.trim(),
        gateway_host: data.gateway_host.trim() || '127.0.0.1',
        docker_group: data.docker_group,
        enable_darkirc: data.enable_darkirc,
        xmpp_enabled: data.xmpp_enabled,
        xmpp_jid: data.xmpp_jid.trim(),
        xmpp_password: data.xmpp_password,
        xmpp_allow_from: data.xmpp_allow_from.split(',').map((s) => s.trim()).filter(Boolean),
        gotify_enabled: data.gotify_enabled,
        gotify_url: data.gotify_url.trim(),
        gotify_title: data.gotify_title.trim(),
        workers: workers,
        toolchains: data.toolchains,
        tensorzero_url: data.tensorzero_url.trim(),
        llm_model: model,
        llm_api_key: data.llm_api_key,
        secrets_master_key: data.secrets_master_key.trim(),
        no_ssh: !data.ssh_harness,
        no_health: !data.health_pipeline,
        skip_build: data.skip_build,
        skip_start: data.skip_start,
      };
    }

    render();
  }

  // -- Single-form modes ----------------------------------------------------
  function simpleForm(host, title, subtitle, fieldsFn, submitLabel, buildPayload, mode, opts) {
    const data = {};
    const body = h('div', {});
    const btn = h('button', { class: 'btn btn-primary' }, [submitLabel]);
    const err = h('div', { class: 'field-error hidden' });
    btn.addEventListener('click', () => {
      const problem = buildPayload.validate ? buildPayload.validate(data) : null;
      if (problem) { err.textContent = problem; err.classList.remove('hidden'); return; }
      opts.onRun(mode, buildPayload(data));
    });
    const wrap = h('div', { class: 'wizard glass' }, [
      h('div', { class: 'wizard-head' }, [h('h2', {}, [title]), h('span', { class: 'step-count' }, [subtitle || ''])]),
      body, err,
      h('div', { class: 'wizard-nav' }, [h('span', {}, []), btn]),
    ]);
    fieldsFn(body, data);
    host.appendChild(wrap);
  }

  function mountUpgrade(host, opts) {
    simpleForm(host, 'Upgrade a tenant', 'dry-run by default', (b, data) => {
      Object.assign(data, { tenant: '', target: '', source_version_override: '', run_preflight: true, apply: false, force: false, auto_yes: false });
      // Truthful warning: this form drives the v1-only version-tag upgrade
      // script (upgrade-tenant-version.sh), which parses vX.Y.Z and defaults to
      // v1.1.2. It CANNOT do a v2.0.0.0 -> v2.0.0.0 (or any v2) upgrade.
      b.appendChild(h('div', { class: 'form-warn' }, [
        h('strong', null, ['⚠ v1 upgrades only.']),
        ' This drives ',
        h('code', null, ['upgrade-tenant-version.sh']),
        ' (v1 release tags; blank target = ',
        h('code', null, ['v1.1.2']),
        '). It does NOT support v2.0.0.0 → v2.0.0.0. For a v2 tenant, use the CLI instead: ',
        h('code', null, ['sudo ic/scripts/lunarwing-mt-admin.sh upgrade-tenant <tenant> --target <ref>']),
        '.',
      ]));
      b.appendChild(textField(data, 'tenant', 'Tenant', { datalist: opts.tenants, placeholder: 'sphinx' }));
      b.appendChild(textField(data, 'target', 'Target release tag (blank = script default)', { placeholder: 'v1.1.9' }));
      b.appendChild(textField(data, 'source_version_override', 'Source version override (optional)', { placeholder: 'v1.1.7' }));
      b.appendChild(checkField(data, 'run_preflight', 'Run preflight checks first'));
      b.appendChild(checkField(data, 'apply', 'Apply changes (unchecked = dry-run)'));
      b.appendChild(checkField(data, 'force', 'Force (continue after preflight failure)'));
      b.appendChild(checkField(data, 'auto_yes', 'Assume yes to script prompts'));
    }, 'Run upgrade', Object.assign(
      (data) => ({ ...data, tenant: data.tenant.trim(), target: data.target.trim(), source_version_override: data.source_version_override.trim() }),
      { validate: (d) => (d.tenant && d.tenant.trim() ? null : 'tenant is required') }
    ), 'upgrade', opts);
  }

  function mountExport(host, opts) {
    simpleForm(host, 'Export a tenant', 'Kawarimi migration · dry-run by default', (b, data) => {
      Object.assign(data, { tenant: '', out_dir: '/var/lib/lunarwing-migrate', apply: false, no_quiesce: false });
      b.appendChild(textField(data, 'tenant', 'Tenant', { datalist: opts.tenants, placeholder: 'sphinx' }));
      b.appendChild(textField(data, 'out_dir', 'Output directory', {}));
      b.appendChild(checkField(data, 'apply', 'Apply (write bundle; unchecked = dry-run)'));
      b.appendChild(checkField(data, 'no_quiesce', 'Skip auto-stopping services (--no-quiesce)'));
    }, 'Run export', Object.assign(
      (data) => ({ ...data, tenant: data.tenant.trim(), out_dir: data.out_dir.trim() }),
      { validate: (d) => (d.tenant && d.tenant.trim() ? null : 'tenant is required') }
    ), 'export', opts);
  }

  function mountImport(host, opts) {
    simpleForm(host, 'Import a tenant', 'stage-only by default; start requires old host stopped', (b, data) => {
      Object.assign(data, {
        bundle: '', name: '', apply: false, start: false, old_stopped: false, force: false,
        with_opencode: false, with_toolchains: false, with_nanocode: false,
        with_pebble: false, with_vision: false, docker_group: false,
        tensorzero_url: '', owner_scope: '',
      });
      b.appendChild(textField(data, 'bundle', 'Migration bundle path', { placeholder: '/var/lib/lunarwing-migrate/tenant.tar' }));
      b.appendChild(textField(data, 'name', 'Tenant name override (optional)', { placeholder: 'sphinx-new' }));
      b.appendChild(checkField(data, 'apply', 'Apply import (unchecked = dry-run)'));
      b.appendChild(checkField(data, 'start', 'Start restored tenant (--start)'));
      b.appendChild(checkField(data, 'old_stopped', 'Confirm old host stopped (--old-stopped)'));
      b.appendChild(checkField(data, 'force', 'Force import (--force)'));
      const workers = h('div', { class: 'subgroup' });
      workers.appendChild(checkField(data, 'with_opencode', 'Build opencode worker'));
      workers.appendChild(checkField(data, 'with_toolchains', 'Include worker toolchains'));
      workers.appendChild(checkField(data, 'with_nanocode', 'Build nanocode worker'));
      workers.appendChild(checkField(data, 'with_pebble', 'Build pebble worker'));
      workers.appendChild(checkField(data, 'with_vision', 'Restore LunarVision'));
      workers.appendChild(checkField(data, 'docker_group', 'Add tenant user to docker/podman group'));
      b.appendChild(workers);
      b.appendChild(textField(data, 'tensorzero_url', 'TensorZero upstream URL (optional)', {}));
      b.appendChild(textField(data, 'owner_scope', 'Legacy owner scope (optional)', {}));
    }, 'Run import', Object.assign(
      (data) => ({
        ...data,
        bundle: data.bundle.trim(),
        name: data.name.trim(),
        tensorzero_url: data.tensorzero_url.trim(),
        owner_scope: data.owner_scope.trim(),
      }),
      { validate: (d) => (!d.bundle.trim() ? 'bundle path is required' : d.name.trim() ? validateName(d.name.trim()) : null) }
    ), 'import', opts);
  }

  function mountSecrets(host, opts) {
    simpleForm(host, 'Store a secret', 'encrypted into the tenant secrets store', (b, data) => {
      Object.assign(data, { tenant: '', name: '', value: '', confirm: '' });
      b.appendChild(textField(data, 'tenant', 'Tenant', { datalist: opts.tenants, placeholder: 'sphinx' }));
      b.appendChild(textField(data, 'name', 'Secret name', { placeholder: 'gotify_app_token', hint: 'letters, numbers, _ / -' }));
      b.appendChild(textField(data, 'value', 'Secret value', { type: 'password' }));
      b.appendChild(textField(data, 'confirm', 'Confirm value', { type: 'password' }));
    }, 'Store secret', Object.assign(
      (data) => ({ tenant: data.tenant.trim(), name: data.name.trim(), value: data.value }),
      { validate: (d) => (!d.tenant.trim() ? 'tenant is required' : !d.name.trim() ? 'secret name is required' : !d.value ? 'value is required' : d.value !== d.confirm ? 'values do not match' : null) }
    ), 'secrets', opts);
  }

  LW.mountWizard = function (host, mode, opts) {
    host.innerHTML = '';
    if (mode === 'provision') mountProvision(host, opts);
    else if (mode === 'upgrade') mountUpgrade(host, opts);
    else if (mode === 'export') mountExport(host, opts);
    else if (mode === 'import') mountImport(host, opts);
    else if (mode === 'secrets') mountSecrets(host, opts);
  };
})();
