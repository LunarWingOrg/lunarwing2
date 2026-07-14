/* Onboarding tips — a rotating hint panel for the cosmos dock.
   Public API: LW.Tips(host) -> { show, hide, start, stop }.
   Rotation pauses while hidden so we don't burn timers off-screen. */
(function () {
  const LW = (window.LW = window.LW || {});

  const TIPS = [
    'Provision builds a fresh tenant end to end — grab the master key when it appears.',
    'Every action here is audit-logged to logs/ — nothing runs silently.',
    'Upgrade, Export and Import default to dry-run / stage-only. Confirm to go live.',
    'The master key is shown once. Store it now — it is the tenant vault key.',
    'Toggle “follow” to keep the activity log pinned to the newest line.',
    'Import restores a Kawarimi bundle; export packs one for migration.',
  ];

  LW.Tips = function (host) {
    let i = 0;
    let timer = null;
    const body = host.querySelector('.tips-body') || host;

    function render() {
      body.textContent = TIPS[i % TIPS.length];
    }
    function start() {
      stop();
      render();
      timer = setInterval(() => { i += 1; render(); }, 6000);
    }
    function stop() {
      if (timer) { clearInterval(timer); timer = null; }
    }
    function show() { host.classList.remove('hidden'); start(); }
    function hide() { host.classList.add('hidden'); stop(); }

    render();
    return { show, hide, start, stop };
  };
})();
