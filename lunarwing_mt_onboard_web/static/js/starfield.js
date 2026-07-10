/* Animated twinkling starfield background. */
(function () {
  const LW = (window.LW = window.LW || {});

  LW.startStarfield = function (canvas) {
    const ctx = canvas.getContext('2d');
    const dpr = window.devicePixelRatio || 1;
    let w = 0;
    let h = 0;
    let stars = [];

    function resize() {
      w = canvas.width = Math.floor(window.innerWidth * dpr);
      h = canvas.height = Math.floor(window.innerHeight * dpr);
      const count = Math.min(240, Math.floor((window.innerWidth * window.innerHeight) / 8000));
      stars = Array.from({ length: count }, () => ({
        x: Math.random() * w,
        y: Math.random() * h,
        r: (Math.random() * 1.3 + 0.3) * dpr,
        tw: Math.random() * Math.PI * 2,
        sp: Math.random() * 0.02 + 0.004,
        drift: (Math.random() * 0.12 + 0.02) * dpr,
      }));
    }

    function frame() {
      ctx.clearRect(0, 0, w, h);
      for (const s of stars) {
        s.tw += s.sp;
        s.y += s.drift;
        if (s.y > h) {
          s.y = 0;
          s.x = Math.random() * w;
        }
        const a = 0.35 + 0.55 * Math.abs(Math.sin(s.tw));
        ctx.beginPath();
        ctx.arc(s.x, s.y, s.r, 0, Math.PI * 2);
        ctx.fillStyle = 'rgba(147, 197, 253, ' + a.toFixed(3) + ')';
        ctx.fill();
      }
      requestAnimationFrame(frame);
    }

    resize();
    window.addEventListener('resize', resize);
    frame();
  };
})();
