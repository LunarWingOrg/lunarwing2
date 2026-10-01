// Per-tenant prompt sources. Drafts stay in memory only, including across tab
// switches, and are cleared with the authentication session.
window.PromptEditor = (function() {
  const el = (id) => document.getElementById('prompt-' + id);
  const sourceKeys = {
    'AGENTS.md': 'agents', 'SOUL.md': 'soul', 'USER.md': 'user',
    'IDENTITY.md': 'identity', 'TOOLS.md': 'tools', 'MEMORY.md': 'memory',
    'BOOTSTRAP.md': 'bootstrap',
  };
  let files = [];
  let busy = false;
  let generation = 0;
  let previewRequest = 0;

  function selected() {
    return files.find((file) => file.path === el('source').value);
  }

  function isDirty() {
    return files.some((file) => file.draft !== file.content);
  }

  function updateControls() {
    const file = selected();
    el('source').disabled = busy || !files.length;
    el('editor').disabled = busy || !file;
    el('reload').disabled = busy;
    el('save').disabled = busy || !file || (file.exists && file.draft === file.content);
    el('revert').disabled = busy || !file || file.draft === file.content;
    for (const option of el('source').options) {
      const source = files.find((item) => item.path === option.value);
      option.textContent = source.path + ' — ' + I18n.t('prompts.file.' + sourceKeys[source.path])
        + (source.draft !== source.content ? ' *' : '');
    }
  }

  function showSource() {
    const file = selected();
    el('editor').value = file ? file.draft : '';
    el('source-info').textContent = file
      ? I18n.t('prompts.help.' + sourceKeys[file.path]) + ' ' + (file.exists
        ? I18n.t('prompts.updated', { date: file.updated_at })
        : I18n.t('prompts.missing'))
      : '';
    updateControls();
  }

  async function refreshPreview() {
    const request = ++previewRequest;
    const current = generation;
    el('preview').textContent = '';
    el('preview-status').textContent = I18n.t('common.loading');
    const query = new URLSearchParams({
      is_group_chat: String(el('group').checked),
      timezone: el('timezone').value.trim() || 'UTC',
    });
    try {
      const data = await apiFetch('/api/prompts/preview?' + query);
      if (current !== generation || request !== previewRequest) return;
      el('preview').textContent = data.prompt || I18n.t('prompts.empty');
      el('preview-status').textContent = I18n.t('prompts.previewScope', {
        scope: data.scope, timezone: data.timezone,
      });
    } catch (error) {
      if (current !== generation || request !== previewRequest) return;
      el('preview-status').textContent = I18n.t('prompts.previewError', { error: error.message });
    }
  }

  async function load() {
    // Revisiting settings must not discard a draft or replace newer edits with
    // a late network response. Operators explicitly reload saved sources.
    if (files.length || busy) return;
    busy = true;
    const current = generation;
    el('status').textContent = I18n.t('common.loading');
    updateControls();
    try {
      const data = await apiFetch('/api/prompts');
      if (current !== generation) return;
      files = data.files.map((file) => ({ ...file, draft: file.content }));
      el('scope').textContent = data.scope;
      el('source').replaceChildren();
      for (const file of files) {
        const option = document.createElement('option');
        option.value = file.path;
        el('source').appendChild(option);
      }
      showSource();
      el('status').textContent = '';
      await refreshPreview();
    } catch (error) {
      if (current !== generation) return;
      el('status').textContent = I18n.t('prompts.loadError', { error: error.message });
    } finally {
      if (current === generation) {
        busy = false;
        updateControls();
      }
    }
  }

  async function save() {
    const file = selected();
    if (!file || busy) return;
    busy = true;
    const current = generation;
    el('status').textContent = I18n.t('prompts.saving');
    updateControls();
    try {
      const saved = await apiFetch('/api/prompts/write', {
        method: 'POST', body: { path: file.path, content: file.draft },
      });
      if (current !== generation) return;
      Object.assign(file, saved, { draft: saved.content });
      showSource();
      el('status').textContent = I18n.t('prompts.saved', { path: file.path });
      await refreshPreview();
    } catch (error) {
      if (current !== generation) return;
      el('status').textContent = I18n.t('prompts.saveError', { error: error.message });
    } finally {
      if (current === generation) {
        busy = false;
        updateControls();
      }
    }
  }

  function reset() {
    generation++;
    previewRequest++;
    files = [];
    busy = false;
    el('source').replaceChildren();
    el('editor').value = '';
    for (const id of ['scope', 'source-info', 'status', 'preview', 'preview-status']) {
      el(id).textContent = '';
    }
    updateControls();
  }

  el('source').addEventListener('change', showSource);
  el('editor').addEventListener('input', function() {
    const file = selected();
    if (!file) return;
    file.draft = el('editor').value;
    el('status').textContent = isDirty() ? I18n.t('prompts.unsaved') : '';
    updateControls();
  });
  el('save').addEventListener('click', save);
  el('revert').addEventListener('click', function() {
    const file = selected();
    if (!file || busy) return;
    file.draft = file.content;
    showSource();
    el('status').textContent = isDirty() ? I18n.t('prompts.unsaved') : '';
  });
  el('reload').addEventListener('click', function() {
    if (busy) return;
    const current = generation;
    function reload() {
      if (current !== generation || busy) return;
      reset();
      load();
    }
    if (isDirty()) {
      showConfirmModal(I18n.t('prompts.reload'), I18n.t('prompts.discard'), reload);
    } else {
      reload();
    }
  });
  el('preview-refresh').addEventListener('click', refreshPreview);
  el('group').addEventListener('change', refreshPreview);
  el('timezone').addEventListener('change', refreshPreview);
  window.addEventListener('beforeunload', function(event) {
    if (!isDirty()) return;
    event.preventDefault();
    event.returnValue = '';
  });

  return { load, reset };
})();
