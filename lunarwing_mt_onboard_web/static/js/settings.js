/* Appearance settings + a tiny localStorage-backed prefs helper.
   Prefs are namespaced under `lw-onboard-<key>` and stored as 'true'/'false'.
   A small gear button in the header toggles a popover with the checkboxes;
   each pref exposes an onChange callback so feature modules can react. */
(function () {
  const LW = (window.LW = window.LW || {});

  const KEY = (k) => 'lw-onboard-' + k;
  LW.prefs = {
    get(k, dflt) {
      const v = localStorage.getItem(KEY(k));
      return v === null ? !!dflt : v === 'true';
    },
    set(k, val) {
      localStorage.setItem(KEY(k), String(!!val));
    },
  };

  // spec: [{ key, label, default, onChange }] — onChange fires on init + change.
  LW.initSettings = function (button, panel, spec) {
    spec.forEach((s) => {
      const cb = panel.querySelector('#set-' + s.key);
      if (!cb) return;
      cb.checked = LW.prefs.get(s.key, s.default);
      if (s.onChange) s.onChange(cb.checked);
      cb.addEventListener('change', () => {
        LW.prefs.set(s.key, cb.checked);
        if (s.onChange) s.onChange(cb.checked);
      });
    });

    const close = () => panel.classList.add('hidden');
    button.addEventListener('click', (e) => {
      e.stopPropagation();
      panel.classList.toggle('hidden');
    });
    // Click-away + Esc dismiss.
    document.addEventListener('click', (e) => {
      if (!panel.classList.contains('hidden') &&
          !panel.contains(e.target) && e.target !== button) close();
    });
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') close();
    });
  };
})();
