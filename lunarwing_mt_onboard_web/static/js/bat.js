/* Bat mascot — sprite swap. The four SVGs (content/excited/sleeping/angry)
   are adapted from GLM's sprite set. Public API is unchanged so app.js keeps
   driving moods via bat.set(mood); idle-cycles on the picker. */
(function () {
  const LW = (window.LW = window.LW || {});
  const MOODS = ['content', 'excited', 'sleeping', 'angry'];
  const IDLE_MOODS = ['content', 'excited', 'sleeping'];
  const src = (m) => '/static/bat/' + m + '.svg';

  // Preload all moods so swaps don't flicker.
  MOODS.forEach((m) => {
    const img = new Image();
    img.src = src(m);
  });

  LW.Bat = function (host, moodLabel) {
    host.innerHTML = '<img class="bat-sprite content" id="lw-bat-img" alt="LunarWing bat mascot" />';
    const img = host.querySelector('#lw-bat-img');
    let mood = 'content';
    let idleTimer = null;

    function apply(m) {
      mood = m;
      img.src = src(m);
      img.className = 'bat-sprite ' + m;
      if (moodLabel) moodLabel.textContent = m;
    }

    function set(m) {
      apply(m);
    }

    function startIdleCycle() {
      stopIdle();
      idleTimer = setInterval(() => {
        const opts = IDLE_MOODS.filter((x) => x !== mood);
        apply(opts[Math.floor(Math.random() * opts.length)]);
      }, 7000);
    }

    function stopIdle() {
      if (idleTimer) {
        clearInterval(idleTimer);
        idleTimer = null;
      }
    }

    apply('content');
    return {
      set,
      startIdleCycle,
      stopIdle,
      get mood() {
        return mood;
      },
    };
  };
})();
