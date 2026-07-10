/* Mascot — "Lunar" the batpony (animated GIF sprites).
   Mood -> file: content=walk, excited=jump, sleeping=sleep, angry=rage.
   GIFs self-animate, so there are no CSS keyframes. The public API is
   unchanged (set / startIdleCycle / stopIdle) so app.js is untouched. */
(function () {
  const LW = (window.LW = window.LW || {});

  const FILES = {
    content: 'lunar_walk.gif',
    excited: 'lunar_jump.gif',
    sleeping: 'lunar_sleep.gif',
    angry: 'lunar_rage.gif',
  };
  const IDLE_MOODS = ['content', 'excited', 'sleeping'];
  const src = (m) => '/static/mascot/' + (FILES[m] || FILES.content);

  // Preload so mood swaps are instant.
  Object.keys(FILES).forEach((m) => {
    const img = new Image();
    img.src = src(m);
  });

  LW.Bat = function (host, moodLabel) {
    host.innerHTML = '<img class="bat-sprite content" id="lw-bat-img" alt="Lunar the batpony" />';
    const img = host.querySelector('#lw-bat-img');
    let mood = 'content';
    let idleTimer = null;

    function apply(m) {
      if (!FILES[m]) m = 'content';
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
