/* Startup splash — a one-shot branded overlay that fades out on load.
   Runs immediately (the #splash-screen markup ships hidden-capable in the
   HTML). Honors the 'splash' pref and prefers-reduced-motion. */
(function () {
  const LW = (window.LW = window.LW || {});
  const el = document.getElementById('splash-screen');
  if (!el) return;

  const enabled = LW.prefs ? LW.prefs.get('splash', true) : true;
  const reduce = window.matchMedia &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  if (!enabled) {
    el.remove();
    return;
  }

  function dismiss() {
    el.classList.add('splash-hidden');
    setTimeout(() => el.remove(), 450);
  }

  // Reduced motion: show briefly, no lingering animation.
  setTimeout(dismiss, reduce ? 400 : 1300);
  // Let the user skip it.
  el.addEventListener('click', dismiss);
})();
