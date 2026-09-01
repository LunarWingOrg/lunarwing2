/* LunarWing emoting mascot widget.
 *
 * A draggable corner sprite that mirrors the agent's expressed personality:
 * the gateway emote parser (src/channels/web/emote.rs) classifies the
 * agent's roleplay action phrases ("*stamps hooves*") and attaches the
 * resulting emote names to assistant `response` events; this widget plays
 * the matching sprite animation. When no emote is playing it falls back to
 * the working sprite while the agent is busy and the idle sprite otherwise.
 *
 * app.js forwards agent activity via window.LunarMascot.onAgentEvent(type,
 * data). Types: 'open' (SSE connected), 'working' (agent busy), 'response'
 * (data.emotes: string[]), 'error'. Presentation layer only — it never
 * touches chat content.
 */
(function () {
  'use strict';

  var SPRITE_BASE = '/mascot/';
  var EMOTE_SPRITES = {
    greet: 'lunar_greet.gif',
    love: 'lunar_love.gif',
    rage: 'lunar_rage.gif',
    jump: 'lunar_jump.gif',
    walk: 'lunar_walk.gif',
    sleep: 'lunar_sleep.gif',
    alert: 'lunar_sup.gif',
    fiery: 'lunar_fiery.gif'
  };
  var FALLBACK_SPRITES = {
    idle: 'lunar_sleep.gif',
    working: 'lunar_walk.gif',
    error: 'lunar_rage.gif',
    open: 'lunar_greet.gif'
  };

  var EMOTE_MS = 2600; // per-emote display time
  var GREET_MS = 3000; // connect greeting display time
  var ERROR_MS = 4000; // error flash display time
  var IDLE_AFTER_MS = 30000; // inactivity before the idle fallback
  var DRAG_THRESHOLD_PX = 5;

  var POS_KEY = 'lunar.mascot.pos';
  var HIDDEN_KEY = 'lunar.mascot.hidden';

  // --- State machine -------------------------------------------------------
  // States: 'idle' | 'working' | 'emote' | 'error'. Emotes play one at a
  // time from a FIFO queue; error flashes override an emote but not the
  // queue; 'working' is remembered while a higher-priority state plays.
  var state = 'idle';
  var currentSprite = null;
  var emoteQueue = [];
  var stateTimer = null;
  var idleTimer = null;
  var pendingWorking = false;
  var hasGreeted = false;

  var widget = null;
  var img = null;
  var restoreBtn = null;

  function spriteFor(name) {
    return Object.prototype.hasOwnProperty.call(EMOTE_SPRITES, name)
      ? EMOTE_SPRITES[name]
      : null;
  }

  function showSprite(file, restart) {
    if (!file || !img) return;
    if (file === currentSprite && !restart) return;
    currentSprite = file;
    if (restart) img.src = '';
    img.src = SPRITE_BASE + file;
  }

  function clearStateTimer() {
    if (stateTimer) {
      clearTimeout(stateTimer);
      stateTimer = null;
    }
  }

  // Move to a fallback state after an emote/error/greeting finishes playing.
  function settleAfterPlayback() {
    if (emoteQueue.length > 0) {
      playNextEmote();
      return;
    }
    if (pendingWorking) {
      enterWorking();
      return;
    }
    enterIdle();
  }

  function enterIdle() {
    clearStateTimer();
    state = 'idle';
    pendingWorking = false;
    showSprite(FALLBACK_SPRITES.idle);
  }

  function enterWorking() {
    clearStateTimer();
    state = 'working';
    pendingWorking = false;
    showSprite(FALLBACK_SPRITES.working);
  }

  function playNextEmote() {
    var next = emoteQueue.shift();
    if (!next) {
      settleAfterPlayback();
      return;
    }
    var file = spriteFor(next);
    if (!file) {
      // Unknown emote name: skip without stalling the queue.
      playNextEmote();
      return;
    }
    clearStateTimer();
    state = 'emote';
    showSprite(file, true);
    stateTimer = setTimeout(settleAfterPlayback, EMOTE_MS);
  }

  function enqueueEmotes(emotes) {
    if (!Array.isArray(emotes) || emotes.length === 0) return false;
    var added = false;
    for (var i = 0; i < emotes.length; i++) {
      if (spriteFor(emotes[i])) {
        emoteQueue.push(emotes[i]);
        added = true;
      }
    }
    if (!added) return false;
    if (state !== 'emote' && state !== 'error') playNextEmote();
    return true;
  }

  function flashError() {
    clearStateTimer();
    state = 'error';
    showSprite(FALLBACK_SPRITES.error, true);
    stateTimer = setTimeout(settleAfterPlayback, ERROR_MS);
  }

  function greet() {
    clearStateTimer();
    state = 'emote'; // behaves like a queued greeting
    showSprite(FALLBACK_SPRITES.open, true);
    stateTimer = setTimeout(settleAfterPlayback, GREET_MS);
  }

  function noteActivity() {
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = setTimeout(enterIdle, IDLE_AFTER_MS);
  }

  function onAgentEvent(type, data) {
    if (isHidden()) return;
    noteActivity();
    switch (type) {
      case 'open':
        if (!hasGreeted) {
          hasGreeted = true;
          greet();
        } else if (state === 'idle') {
          greet();
        }
        break;
      case 'working':
        if (state === 'idle') enterWorking();
        else if (state !== 'working') pendingWorking = true;
        break;
      case 'response':
        if (!enqueueEmotes(data && data.emotes)) {
          // Response carried no emote: the turn is done, drop to idle.
          pendingWorking = false;
          if (state === 'working') enterIdle();
        }
        break;
      case 'error':
        flashError();
        break;
      default:
        break;
    }
  }

  // --- Persistence ---------------------------------------------------------

  function readJson(key) {
    try {
      var raw = window.localStorage.getItem(key);
      return raw ? JSON.parse(raw) : null;
    } catch (_e) {
      return null;
    }
  }

  function writeJson(key, value) {
    try {
      window.localStorage.setItem(key, JSON.stringify(value));
    } catch (_e) {
      /* storage unavailable: widget stays functional for the session */
    }
  }

  function isHidden() {
    try {
      return window.localStorage.getItem(HIDDEN_KEY) === '1';
    } catch (_e) {
      return false;
    }
  }

  // --- DOM + dragging ------------------------------------------------------

  function clampPosition(left, top) {
    var w = widget.offsetWidth;
    var h = widget.offsetHeight;
    var maxLeft = Math.max(0, window.innerWidth - w);
    var maxTop = Math.max(0, window.innerHeight - h);
    return {
      left: Math.min(Math.max(0, left), maxLeft),
      top: Math.min(Math.max(0, top), maxTop)
    };
  }

  function applyPosition(left, top) {
    var pos = clampPosition(left, top);
    widget.style.left = pos.left + 'px';
    widget.style.top = pos.top + 'px';
    widget.style.right = 'auto';
    widget.style.bottom = 'auto';
  }

  function restorePosition() {
    var saved = readJson(POS_KEY);
    if (saved && typeof saved.left === 'number' && typeof saved.top === 'number') {
      applyPosition(saved.left, saved.top);
    }
  }

  function makeDraggable() {
    var dragging = false;
    var moved = false;
    var grabOffsetX = 0;
    var grabOffsetY = 0;

    widget.addEventListener('pointerdown', function (e) {
      if (e.target.closest('.mascot-close')) return;
      dragging = true;
      moved = false;
      var rect = widget.getBoundingClientRect();
      grabOffsetX = e.clientX - rect.left;
      grabOffsetY = e.clientY - rect.top;
      widget.setPointerCapture(e.pointerId);
      e.preventDefault();
    });

    widget.addEventListener('pointermove', function (e) {
      if (!dragging) return;
      if (Math.abs(e.movementX) + Math.abs(e.movementY) > 0) moved = true;
      applyPosition(e.clientX - grabOffsetX, e.clientY - grabOffsetY);
    });

    widget.addEventListener('pointerup', function (e) {
      if (!dragging) return;
      dragging = false;
      widget.releasePointerCapture(e.pointerId);
      if (moved) {
        var rect = widget.getBoundingClientRect();
        writeJson(POS_KEY, { left: rect.left, top: rect.top });
      }
    });

    widget.addEventListener('pointercancel', function () {
      dragging = false;
    });

    window.addEventListener('resize', function () {
      var rect = widget.getBoundingClientRect();
      if (widget.style.right === 'auto') applyPosition(rect.left, rect.top);
    });
  }

  function buildDom() {
    widget = document.createElement('div');
    widget.id = 'lunar-mascot';
    widget.setAttribute('role', 'img');
    widget.setAttribute('aria-label', 'Lunar mascot');
    widget.title = 'Lunar mascot — mirrors the agent\'s emotes';

    img = document.createElement('img');
    img.alt = '';
    img.draggable = false;
    widget.appendChild(img);

    var close = document.createElement('button');
    close.type = 'button';
    close.className = 'mascot-close';
    close.title = 'Hide mascot';
    close.setAttribute('aria-label', 'Hide mascot');
    close.textContent = '×';
    close.addEventListener('click', function (e) {
      e.stopPropagation();
      setHidden(true);
    });
    widget.appendChild(close);

    document.body.appendChild(widget);

    restoreBtn = document.createElement('button');
    restoreBtn.type = 'button';
    restoreBtn.id = 'lunar-mascot-restore';
    restoreBtn.title = 'Show mascot';
    restoreBtn.setAttribute('aria-label', 'Show mascot');
    var restoreImg = document.createElement('img');
    restoreImg.src = SPRITE_BASE + FALLBACK_SPRITES.idle;
    restoreImg.alt = '';
    restoreImg.draggable = false;
    restoreBtn.appendChild(restoreImg);
    restoreBtn.addEventListener('click', function () {
      setHidden(false);
    });
    document.body.appendChild(restoreBtn);
  }

  function setHidden(hidden) {
    try {
      window.localStorage.setItem(HIDDEN_KEY, hidden ? '1' : '0');
    } catch (_e) {
      /* see writeJson */
    }
    widget.style.display = hidden ? 'none' : '';
    restoreBtn.style.display = hidden ? '' : 'none';
    if (!hidden) {
      enterIdle();
      noteActivity();
    }
  }

  function init() {
    if (window.LunarMascot) return; // double-include guard
    buildDom();
    if (isHidden()) {
      widget.style.display = 'none';
      restoreBtn.style.display = '';
    } else {
      restoreBtn.style.display = 'none';
    }
    makeDraggable();
    restorePosition();
    enterIdle();
    noteActivity();

    window.LunarMascot = {
      onAgentEvent: onAgentEvent,
      show: function () { setHidden(false); },
      hide: function () { setHidden(true); }
    };
  }

  init();
})();
