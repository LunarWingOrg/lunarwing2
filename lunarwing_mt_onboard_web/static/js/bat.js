/* Mascot — "Lunar" the batpony (animated GIF sprites).
   Mood -> file: content=walk, excited=jump, sleeping=sleep, angry=rage,
   greet=greet (welcome wave), sup=sup (job-success nod).
   GIFs self-animate, so there are no CSS keyframes. Public API:
   set / greet / celebrate / startIdleCycle / stopIdle.
   greet() and celebrate() are one-shot event moods that play once and then
   settle back into the idle cycle; they are intentionally kept OUT of the
   random idle rotation (which stays walk/jump/sleep) so they only appear on
   their trigger — a welcome and a job-success beat respectively. */
(function () {
  const LW = (window.LW = window.LW || {});

  const FILES = {
    content: 'lunar_walk.gif',
    excited: 'lunar_jump.gif',
    sleeping: 'lunar_sleep.gif',
    angry: 'lunar_rage.gif',
    greet: 'lunar_greet.gif',
    sup: 'lunar_sup.gif',
    fiery: 'lunar_fiery.gif',
    love: 'lunar_love.gif',
  };
  // Ambient moods randomly cycled while idle on the picker. greet/sup stay out
  // (they are one-shot event gestures); angry is reserved for failures.
  const IDLE_MOODS = ['content', 'excited', 'sleeping', 'fiery', 'love'];
  // One GIF loop is 12 frames * 13cs ≈ 1.56s; let a one-shot gesture play ~1.5
  // loops before settling so it reads as a complete beat.
  const ONESHOT_MS = 2300;
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
    let oneShotTimer = null;

    function apply(m) {
      if (!FILES[m]) m = 'content';
      mood = m;
      // Reassigning src (even to the same file) restarts the GIF from frame 0,
      // so a repeated one-shot gesture always replays from the top.
      img.src = src(m);
      img.className = 'bat-sprite ' + m;
      if (moodLabel) moodLabel.textContent = m;
    }

    function clearOneShot() {
      if (oneShotTimer) {
        clearTimeout(oneShotTimer);
        oneShotTimer = null;
      }
    }

    function set(m) {
      // An explicit mood set cancels any pending one-shot settle.
      clearOneShot();
      apply(m);
    }

    // Play a one-shot gesture, then land on `settle` (default: resume idle).
    function playOnce(m, settle) {
      stopIdle();
      clearOneShot();
      apply(m);
      oneShotTimer = setTimeout(() => {
        oneShotTimer = null;
        if (settle) settle();
        else startIdleCycle();
      }, ONESHOT_MS);
    }

    // Welcome wave — plays once, then eases into the ambient idle cycle.
    function greet() {
      playOnce('greet', null);
    }

    // Job-success nod — plays once, then holds on `content` (the run view has
    // finished, so we don't want the picker's ambient cycling here).
    function celebrate() {
      playOnce('sup', () => apply('content'));
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
      greet,
      celebrate,
      startIdleCycle,
      stopIdle,
      get mood() {
        return mood;
      },
    };
  };
})();
