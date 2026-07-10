/* SVG moon that waxes from new -> full as provisioning phases complete.
   Illumination fraction f = index / total; terminator drawn with an ellipse. */
(function () {
  const LW = (window.LW = window.LW || {});

  const SVG =
    '<svg viewBox="0 0 160 160" width="160" height="160" aria-hidden="true">' +
    '<defs>' +
    '<radialGradient id="moonGrad" cx="38%" cy="34%" r="75%">' +
    '<stop offset="0%" stop-color="#f2f8ff"/>' +
    '<stop offset="65%" stop-color="#bcd9fb"/>' +
    '<stop offset="100%" stop-color="#6f9fd8"/>' +
    '</radialGradient>' +
    '</defs>' +
    '<circle cx="80" cy="80" r="70" fill="#0b1120" stroke="rgba(147,197,253,0.18)" stroke-width="1"/>' +
    '<path id="moon-lit" d="" fill="url(#moonGrad)"/>' +
    '<circle cx="62" cy="66" r="7" fill="rgba(90,130,180,0.25)"/>' +
    '<circle cx="92" cy="94" r="9" fill="rgba(90,130,180,0.22)"/>' +
    '<circle cx="98" cy="58" r="5" fill="rgba(90,130,180,0.2)"/>' +
    '</svg>';

  function phaseName(f) {
    if (f < 0.03) return 'New moon';
    if (f < 0.47) return 'Waxing crescent';
    if (f < 0.53) return 'First quarter';
    if (f < 0.97) return 'Waxing gibbous';
    return 'Full moon';
  }

  LW.Moon = function (host, labelEl) {
    host.innerHTML = SVG;
    const lit = host.querySelector('#moon-lit');
    const R = 70;
    const cx = 80;
    const cy = 80;

    function setPhase(index, total) {
      const f = total > 0 ? Math.max(0, Math.min(1, index / total)) : 0;
      const theta = Math.PI * f; // 0 (new) -> pi (full)
      const rx = R * Math.cos(theta); // +R -> -R
      const sweep = rx > 0 ? 0 : 1; // crescent vs gibbous
      const d =
        'M ' + cx + ' ' + (cy - R) +
        ' A ' + R + ' ' + R + ' 0 0 1 ' + cx + ' ' + (cy + R) +
        ' A ' + Math.abs(rx).toFixed(2) + ' ' + R + ' 0 0 ' + sweep + ' ' + cx + ' ' + (cy - R) +
        ' Z';
      lit.setAttribute('d', d);
      host.style.filter =
        'drop-shadow(0 0 ' + (8 + 22 * f).toFixed(1) + 'px rgba(147,197,253,' + (0.25 + 0.5 * f).toFixed(2) + '))';
      if (labelEl) labelEl.textContent = phaseName(f);
    }

    setPhase(0, 1);
    return { setPhase };
  };
})();
