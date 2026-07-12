# Lunar Mascot Sprite — build-tenant Fiery Mapping & State Review

**Date:** 2026-07-12
**Status:** Draft spec, pending review
**Target:** `lunarwing_mt_onboard_web/static/js/app.js`, `bat.js`, `app.css` (+ web_tests.py)
**Origin:** Provisioning builds take 20–50 min; Lunar currently shows the generic `excited`→`sleeping` cycle for every long phase. The `fiery` sprite exists and is in the ambient idle rotation but never fires on its semantic trigger (cargo compilation). Wire it up, escalate past 20 min, and review the full state table.

---

## Current State (as of 2026-07-12)

### Sprite inventory

| Mood key | GIF file | CSS class | Glow color | Role |
|---|---|---|---|---|
| `content` | `lunar_walk.gif` | `.bat-sprite.content` | default shadow | Ambient walk |
| `excited` | `lunar_jump.gif` | `.bat-sprite.excited` | default shadow | Phase-start anticipation |
| `sleeping` | `lunar_sleep.gif` | `.bat-sprite.sleeping` | default shadow | Long-phase idle (6s after phase start) |
| `angry` | `lunar_rage.gif` | `.bat-sprite.angry` | red `rgba(230,76,76,.7)` | Failure / error |
| `greet` | `lunar_greet.gif` | `.bat-sprite.greet` | blue `rgba(122,162,247,.6)` | One-shot: page load, back-to-picker |
| `sup` | `lunar_sup.gif` | `.bat-sprite.sup` | green `rgba(52,211,153,.6)` | One-shot: job success (`celebrate()`) |
| `fiery` | `lunar_fiery.gif` | `.bat-sprite.fiery` | orange `rgba(245,130,32,.7)` | **Ambient only — never fires on build trigger** |
| `love` | `lunar_love.gif` | `.bat-sprite.love` | pink `rgba(236,72,153,.7)` | Ambient only |

### Ambient idle rotation (`IDLE_MOODS`)

```js
const IDLE_MOODS = ['content', 'excited', 'sleeping', 'fiery', 'love'];
```

Cycles every 7s on the picker view. `greet` and `sup` are intentionally excluded (one-shot event gestures). `angry` is excluded (failure-only).

### Phase → mood wiring (`app.js`)

| Event | Current mood | Code |
|---|---|---|
| Page load / back to picker | `greet` → ambient idle | `bat.greet()` |
| Job starts (any mode) | `excited` | `bat.stopIdle(); bat.set('excited')` |
| Phase start (all phases) | `excited` + sleepy timer | `bat.set('excited'); scheduleSleepy(ev.name)` |
| Long phase > 6s | `sleeping` | `longTimer = setTimeout(() => bat.set('sleeping'), 6000)` |
| Job success | `sup` → holds `content` | `bat.celebrate()` |
| Job failure / error | `angry` | `bat.set('angry')` |

`LONG_PHASES = new Set(['build-tenant', 'build-darkirc', 'upgrade', 'export', 'import'])`

### The gap

`build-tenant` (cargo compilation, 20–50 min) gets the same treatment as `export` or `import` — `excited` for 6 seconds, then `sleeping` for the entire build. The `fiery` sprite, which semantically represents "compiling / working hard," only appears randomly in the ambient picker rotation and never on its intended trigger.

---

## Design

### 1. Wire `fiery` to cargo-build phases

**Decision:** When `build-tenant` or `build-darkirc` is the active phase, Lunar goes `fiery` instead of the generic `excited`→`sleeping` pattern.

**Phases that get fiery:**
- `build-tenant` — cargo release build of the LunarWing daemon (+ optional WASM/nanocode/pebble)
- `build-darkirc` — cargo build of the DarkIRC daemon

Both are `cargo build --release` operations. The other long phases (`upgrade`, `export`, `import`) are script-driven, not compilation — they keep the existing `excited`→`sleeping` pattern.

**Implementation in `app.js`:**

Add a set of "build" phases and rework `scheduleSleepy` into a more general phase-mood scheduler:

```js
const BUILD_PHASES = new Set(['build-tenant', 'build-darkirc']);
const LONG_PHASES = new Set(['build-tenant', 'build-darkirc', 'upgrade', 'export', 'import']);
const BUILD_ESCALATION_MS = 20 * 60 * 1000; // 20 minutes

let longTimer = null;
let escalationTimer = null;

function schedulePhaseMood(phaseName) {
    // Clear any pending timers from the previous phase.
    if (longTimer) { clearTimeout(longTimer); longTimer = null; }
    if (escalationTimer) { clearTimeout(escalationTimer); escalationTimer = null; }
    bat.clearEscalation();

    if (BUILD_PHASES.has(phaseName)) {
        // Cargo build → fiery immediately, escalate if it drags on.
        bat.set('fiery');
        escalationTimer = setTimeout(() => bat.escalate(), BUILD_ESCALATION_MS);
    } else if (LONG_PHASES.has(phaseName)) {
        // Other long phases → excited briefly, then sleeping.
        bat.set('excited');
        longTimer = setTimeout(() => bat.set('sleeping'), 6000);
    } else {
        // Short phases → excited.
        bat.set('excited');
    }
}
```

In the `handle()` switch, replace the two calls (`bat.set('excited'); scheduleSleepy(ev.name);`) with a single `schedulePhaseMood(ev.name)`.

Also clear both timers in the `done` and `error` handlers (where `longTimer` is currently cleared).

### 2. 20-minute escalation

**Decision:** If a build phase is still active after 20 minutes, intensify Lunar's fiery glow with a CSS pulse animation and an `escalated` class. No mood change (still `fiery`) — the escalation is a visual intensifier, not a new sprite.

**Rationale:** The phase label already warns "can take 20-50 min." Twenty minutes is the point where even a normal build is getting long; the escalation signals "still going, still hot" without implying failure (which would be `angry`).

**`bat.js` additions:**

```js
function escalate() {
    img.classList.add('escalated');
    if (moodLabel) moodLabel.textContent = mood + ' (escalated)';
}

function clearEscalation() {
    img.classList.remove('escalated');
}
```

Export `escalate` and `clearEscalation` from the returned API.

**`app.css` additions:**

```css
.bat-sprite.escalated {
    animation: fiery-pulse 1.2s ease-in-out infinite;
}

@keyframes fiery-pulse {
    0%, 100% {
        filter: drop-shadow(0 0 12px rgba(245, 130, 32, 0.7))
                drop-shadow(0 6px 14px rgba(0, 0, 0, 0.5));
    }
    50% {
        filter: drop-shadow(0 0 28px rgba(245, 130, 32, 1.0))
                drop-shadow(0 0 16px rgba(230, 76, 32, 0.8));
        transform: translateY(-2px);
    }
}
```

The pulse intensifies the orange glow to near-white-hot at peak and adds a slight vertical bob — like Lunar is getting impatient but staying determined.

**`prefers-reduced-motion`:** The existing media query at the bottom of `app.css` already nukes all animations for reduced-motion users. No extra handling needed.

### 3. Timer lifecycle

All three timers (`longTimer`, `escalationTimer`, idle cycle) must be cleaned up at every transition point:

| Event | Clear |
|---|---|
| Phase start | `longTimer`, `escalationTimer`, `bat.clearEscalation()` — then set new timers per phase type |
| Job done (ok) | `longTimer`, `escalationTimer`, `bat.clearEscalation()` — then `bat.celebrate()` |
| Job done (fail) | `longTimer`, `escalationTimer`, `bat.clearEscalation()` — then `bat.set('angry')` |
| Error event | Same as fail |
| Cancel | Same as fail |
| Back-to-picker | `longTimer`, `escalationTimer`, `bat.clearEscalation()` — then `bat.greet()` |
| WebSocket close | `longTimer`, `escalationTimer`, `bat.clearEscalation()` |

The `closeWs()` function (already clears `longTimer`) should also clear `escalationTimer` and call `bat.clearEscalation()`.

### 4. Public API changes (`bat.js`)

The returned object from `LW.Bat()` gains two new methods:

```js
return {
    set,
    greet,
    celebrate,
    escalate,        // NEW — intensify fiery glow (CSS class)
    clearEscalation, // NEW — remove escalation class
    startIdleCycle,
    stopIdle,
    get mood() { return mood; },
};
```

`set()` already calls `clearOneShot()` — it should NOT clear escalation (the caller controls escalation lifecycle). The `escalate`/`clearEscalation` pair is managed entirely by `app.js`'s phase scheduler.

---

## Full State Review

| Mood | Current trigger | Assessment | Recommendation |
|---|---|---|---|
| **`content`** (walk) | Ambient idle, post-success hold | Fine | No change |
| **`excited`** (jump) | Phase start (non-build), job start | Fine | No change (build phases now skip this) |
| **`sleeping`** (sleep) | Long non-build phases > 6s | Fine | No change. Only fires for `upgrade`, `export`, `import` now |
| **`angry`** (rage) | Failure, error, cancel | Fine | No change |
| **`greet`** (wave) | Page load, back-to-picker | Fine | No change |
| **`sup`** (nod) | Job success | Fine | No change |
| **`fiery`** (fire) | Ambient only | **Was disconnected from build trigger** | **Wire to build phases** (this spec) |
| **`love`** (hearts) | Ambient only | Underutilized — no semantic trigger | See note below |

### Note on `love`

`love` is currently decorative-only in the ambient rotation. Two candidate triggers if we want to give it a semantic home:

1. **Secrets stored successfully** — The `secret_stored` event in `app.js` currently just logs the name. Could fire `bat.set('love')` briefly before returning to idle.
2. **Gateway auth token revealed** — The `gateway_auth_token` event is a "here's your key, you're all set" moment.

Neither is urgent. If Christopher doesn't want a trigger, `love` can stay as a pure ambient mood. **Leaving as-is unless directed otherwise.**

### Note on `fiery` in ambient rotation

`fiery` currently appears randomly in the picker's idle cycle alongside `content`, `excited`, `sleeping`, and `love`. This means Lunar occasionally bursts into flames while you're just standing on the picker choosing an operation. It's whimsical and on-brand for a batpony, but it slightly dilutes the semantic meaning now that `fiery` has a real trigger.

**Two options:**
- **A) Keep it in ambient** — It's cute, and the picker is a low-stakes context where mood mixing is fine.
- **B) Remove from ambient** — `fiery` becomes build-exclusive, like `angry` is failure-exclusive.

**Recommendation: A (keep).** The ambient rotation is about personality, not precision. Having fiery occasionally show up when idle doesn't confuse the build-phase mapping — the context (build in progress vs. picker) makes the meaning clear. But Christopher should decide.

---

## Demo mode

The fake-mt-admin script (`scripts/fake-mt-admin.sh`) simulates `build-tenant` with 7 steps and short naps. The fiery mapping will work in demo mode — the phase events are identical. However, the 20-minute escalation will never fire in demo (the whole build completes in seconds).

To test escalation in demo: temporarily lower `BUILD_ESCALATION_MS` to ~3000 in a dev branch, or add a `?debug` query param that shortens it. This is a testing convenience, not a production feature.

---

## Testing

### Unit tests (`web_tests.py`)

Add to `MascotSpriteTests`:

1. **`test_bat_js_exports_escalate_and_clear_escalation`** — Assert `bat.js` source contains `escalate,` and `clearEscalation,` in the returned API object.

2. **`test_app_js_wires_fiery_to_build_phases`** — Assert `app.js` source contains `BUILD_PHASES` set with `'build-tenant'` and `'build-darkirc'`, and that `bat.set('fiery')` is called (not just in `bat.js` idle moods).

3. **`test_app_js_has_escalation_timer`** — Assert `app.js` source contains `BUILD_ESCALATION_MS` and `bat.escalate()` call.

4. **`test_css_has_escalated_class`** — Assert `app.css` contains `.bat-sprite.escalated` and `@keyframes fiery-pulse`.

### Manual / demo verification

1. `./run.sh --demo` → start a provision job with any non-`fail` tenant name.
2. Observe: `add-tenant` phase → `excited`.
3. Observe: `build-tenant` phase → **`fiery`** (not sleeping).
4. Observe: `build-darkirc` phase (if enabled) → **`fiery`**.
5. Observe: `start-tenant` phase → `excited` → `sleeping` after 6s (unchanged).
6. Observe: success → `sup` (celebrate) → holds `content`.
7. Test failure: tenant name containing `fail` → `angry` at the failed phase.
8. Escalation: temporarily set `BUILD_ESCALATION_MS = 3000` in dev, verify pulse animation kicks in during the demo build phase and clears on phase transition.

---

## Files touched

| File | Changes |
|---|---|
| `static/js/bat.js` | Add `escalate()` and `clearEscalation()` to the Bat API |
| `static/js/app.js` | Add `BUILD_PHASES`, `BUILD_ESCALATION_MS`, `escalationTimer`; rework `scheduleSleepy` → `schedulePhaseMood`; clear escalation at all transition points |
| `static/css/app.css` | Add `.bat-sprite.escalated` class + `@keyframes fiery-pulse` |
| `web_tests.py` | 4 new test assertions in `MascotSpriteTests` |
| `README.md` | Mention fiery/escalation behavior in the mascot blurb (optional) |

---

## Out of scope

- New sprite GIFs (Christopher is redoing sprites separately per VAULT_DUMP notes)
- `love` mood semantic trigger (leave as ambient unless directed)
- Removing `fiery` from ambient idle rotation (recommendation: keep)
- Backend changes — the phase events from `runner.py` are already correct and need no changes
- Sound effects or haptics
