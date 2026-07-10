/* Animated bat mascot. Moods: content, excited, sleeping, angry.
   Mood is driven by app.js from job events, and idle-cycles on the picker. */
(function () {
  const LW = (window.LW = window.LW || {});

  const SVG =
    '<svg class="bat mood-content" viewBox="0 0 150 120" width="150" height="120">' +
    // wings
    '<path class="wing wing-l" d="M62 58 C 24 34, 6 50, 12 74 C 28 62, 44 68, 60 70 Z" fill="#1b2540" stroke="#33477a" stroke-width="1.5"/>' +
    '<path class="wing wing-r" d="M88 58 C 126 34, 144 50, 138 74 C 122 62, 106 68, 90 70 Z" fill="#1b2540" stroke="#33477a" stroke-width="1.5"/>' +
    // ears
    '<path d="M63 34 L58 14 L74 30 Z" fill="#232d4d"/>' +
    '<path d="M87 34 L92 14 L76 30 Z" fill="#232d4d"/>' +
    // body + face
    '<ellipse cx="75" cy="62" rx="25" ry="27" fill="#28335a" stroke="#3a4c85" stroke-width="1.5"/>' +
    '<ellipse cx="75" cy="66" rx="17" ry="18" fill="#2f3d6b"/>' +
    // eyes: open
    '<g class="eye-open">' +
    '<circle cx="67" cy="60" r="6" fill="#eaf4ff"/><circle class="pupil" cx="68" cy="61" r="3" fill="#0b1020"/>' +
    '<circle cx="83" cy="60" r="6" fill="#eaf4ff"/><circle class="pupil" cx="84" cy="61" r="3" fill="#0b1020"/>' +
    '</g>' +
    // eyes: closed
    '<g class="eye-closed" stroke="#cde2ff" stroke-width="2" fill="none" stroke-linecap="round">' +
    '<path d="M61 61 Q67 65 73 61"/><path d="M77 61 Q83 65 89 61"/>' +
    '</g>' +
    // eyes: angry (red glow)
    '<g class="eye-angry">' +
    '<circle cx="67" cy="61" r="6.5" fill="#ff3b3b"/><circle cx="83" cy="61" r="6.5" fill="#ff3b3b"/>' +
    '<circle cx="67" cy="61" r="3" fill="#7a0000"/><circle cx="83" cy="61" r="3" fill="#7a0000"/>' +
    '</g>' +
    // angry brows
    '<g class="brow" stroke="#0b1020" stroke-width="3" stroke-linecap="round">' +
    '<path d="M60 52 L74 58"/><path d="M90 52 L76 58"/>' +
    '</g>' +
    // mouths
    '<path class="mouth-smile" d="M68 74 Q75 80 82 74" stroke="#cde2ff" stroke-width="2" fill="none" stroke-linecap="round"/>' +
    '<path class="mouth-grin" d="M67 73 Q75 84 83 73 Q75 78 67 73 Z" fill="#0b1020"/>' +
    // fangs (angry)
    '<g class="fang" fill="#ffffff">' +
    '<path d="M69 72 L72 80 L74.5 72 Z"/><path d="M76 72 L78.5 80 L81 72 Z"/>' +
    '</g>' +
    // zzz (sleeping)
    '<text class="zzz" x="99" y="42" fill="#cde2ff" font-size="13" font-family="monospace">z</text>' +
    '<text class="zzz" x="107" y="30" fill="#cde2ff" font-size="18" font-family="monospace">Z</text>' +
    '</svg>';

  const IDLE_MOODS = ['content', 'excited', 'sleeping'];

  LW.Bat = function (host, moodLabel) {
    host.innerHTML = SVG;
    const el = host.querySelector('.bat');
    let mood = 'content';
    let idleTimer = null;

    function apply(m) {
      mood = m;
      el.className = 'bat mood-' + m;
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
