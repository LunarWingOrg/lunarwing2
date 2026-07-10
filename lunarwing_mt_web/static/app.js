/*
 * LunarWing MT Web Onboarding — Application Logic
 *
 * No frameworks, no external libraries, no ES modules.
 * All functions are global, attached on DOMContentLoaded.
 */

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STEPS = ['welcome', 'identity', 'network', 'channels', 'workers', 'llm', 'security', 'review', 'provisioning'];

const MOON_FILLS = [0, 14, 28, 42, 56, 70, 80, 90, 100];

const MOON_LABELS = [
    'New Moon',
    'Waxing Crescent',
    'Waxing Crescent',
    'First Quarter',
    'Waxing Gibbous',
    'Waxing Gibbous',
    'Waxing Gibbous',
    'Waning Gibbous',
    'Full Moon',
];

const WORKER_IDS = {
    nanocode: 'worker-nanocode',
    pebble: 'worker-pebble',
    opencode: 'worker-opencode',
};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

let currentStepIndex = 0;
let nameValid = false;
let provisioningActive = false;
let lastConfig = null;
let mascotCycleTimer = null;
let nameDebounceTimer = null;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function getValue(id, defaultValue) {
    var el = document.getElementById(id);
    if (!el) return defaultValue || '';
    return el.value || (defaultValue || '');
}

function isChecked(id) {
    var el = document.getElementById(id);
    return el ? el.checked : false;
}

function maskSecret(value) {
    if (!value) return '—';
    if (value.length > 4) return '****' + value.slice(-4);
    return '****';
}

function parseList(str) {
    if (!str) return [];
    return str.split(',').map(function (s) { return s.trim(); }).filter(function (s) { return s.length > 0; });
}

function getSelectedWorkers() {
    var result = [];
    for (var key in WORKER_IDS) {
        var el = document.getElementById(WORKER_IDS[key]);
        if (el && el.classList.contains('selected')) {
            result.push(key);
        }
    }
    return result;
}

function getLLMModel() {
    var select = document.getElementById('select-llm-model');
    var val = select ? select.value : '';
    if (val === '__custom__') {
        return getValue('input-llm-custom', '');
    }
    return val;
}

// ---------------------------------------------------------------------------
// Step Navigation
// ---------------------------------------------------------------------------

function showStep(name) {
    var sections = document.querySelectorAll('.wizard-section');
    for (var i = 0; i < sections.length; i++) {
        sections[i].classList.remove('active');
    }
    var target = document.getElementById('step-' + name);
    if (target) {
        target.classList.add('active');
    }

    currentStepIndex = STEPS.indexOf(name);
    if (currentStepIndex < 0) currentStepIndex = 0;

    updateStepDots();
    updateMoon(currentStepIndex);

    // Start or stop mascot idle cycle
    if (name === 'provisioning') {
        stopMascotCycle();
    } else {
        startMascotCycle();
    }

    // Populate review when entering review step
    if (name === 'review') {
        populateReview();
    }
}

function nextStep() {
    if (currentStepIndex < STEPS.length - 1) {
        showStep(STEPS[currentStepIndex + 1]);
    }
}

function prevStep() {
    if (currentStepIndex > 0) {
        showStep(STEPS[currentStepIndex - 1]);
    }
}

// ---------------------------------------------------------------------------
// Step Dots
// ---------------------------------------------------------------------------

function buildStepDots() {
    var container = document.getElementById('step-dots');
    if (!container) return;
    container.innerHTML = '';
    for (var i = 0; i < STEPS.length; i++) {
        var dot = document.createElement('div');
        dot.className = 'dot';
        dot.dataset.step = STEPS[i];
        dot.dataset.index = i;
        dot.addEventListener('click', (function (stepName) {
            return function () {
                // Allow navigation to any step up to current or before
                var targetIdx = STEPS.indexOf(stepName);
                // Allow going backwards freely, and forward up to provisioning
                if (targetIdx <= currentStepIndex || targetIdx < STEPS.length - 1) {
                    // Don't allow jumping to provisioning without provisioning
                    if (stepName === 'provisioning') return;
                    // Don't allow jumping past identity if name invalid
                    if (targetIdx > 1 && !nameValid) return;
                    showStep(stepName);
                }
            };
        })(STEPS[i]));
        container.appendChild(dot);
    }
    updateStepDots();
}

function updateStepDots() {
    var dots = document.querySelectorAll('#step-dots .dot');
    for (var i = 0; i < dots.length; i++) {
        dots[i].classList.remove('active', 'completed');
        var dotIdx = parseInt(dots[i].dataset.index, 10);
        if (dotIdx === currentStepIndex) {
            dots[i].classList.add('active');
        } else if (dotIdx < currentStepIndex) {
            dots[i].classList.add('completed');
        }
    }
}

// ---------------------------------------------------------------------------
// Moon Indicator
// ---------------------------------------------------------------------------

function updateMoon(stepIndex) {
    var moon = document.getElementById('moon');
    var label = document.getElementById('phase-label');
    var idx = stepIndex;
    if (idx < 0) idx = 0;
    if (idx >= MOON_FILLS.length) idx = MOON_FILLS.length - 1;
    var fill = MOON_FILLS[idx];
    if (moon) {
        moon.style.setProperty('--moon-fill', fill + '%');
    }
    if (label) {
        label.textContent = MOON_LABELS[idx] || 'Full Moon';
    }
}

// ---------------------------------------------------------------------------
// Mascot (Bat)
// ---------------------------------------------------------------------------

function setBat(mood) {
    var bat = document.getElementById('bat');
    if (!bat) return;
    bat.src = '/static/bat/' + mood + '.svg';
    bat.className = 'bat-sprite ' + mood;
}

function startMascotCycle() {
    stopMascotCycle();
    if (provisioningActive) return;
    var states = ['content', 'sleeping', 'content', 'excited', 'content'];
    var idx = 0;
    setBat('content');
    mascotCycleTimer = setInterval(function () {
        if (provisioningActive) {
            stopMascotCycle();
            return;
        }
        idx = (idx + 1) % states.length;
        setBat(states[idx]);
    }, 15000);
}

function stopMascotCycle() {
    if (mascotCycleTimer) {
        clearInterval(mascotCycleTimer);
        mascotCycleTimer = null;
    }
}

// ---------------------------------------------------------------------------
// Name Validation
// ---------------------------------------------------------------------------

function validateName() {
    var input = document.getElementById('input-name');
    var errorDiv = document.getElementById('name-error');
    var continueBtn = document.getElementById('btn-identity-continue');
    if (!input || !errorDiv) return;

    var name = input.value.trim();

    if (!name) {
        input.classList.remove('valid', 'invalid');
        errorDiv.textContent = '';
        nameValid = false;
        if (continueBtn) continueBtn.disabled = true;
        return;
    }

    // Debounce
    if (nameDebounceTimer) clearTimeout(nameDebounceTimer);
    nameDebounceTimer = setTimeout(function () {
        fetch('/api/validate/name', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: name }),
        })
            .then(function (r) { return r.json(); })
            .then(function (data) {
                if (data.valid) {
                    input.classList.remove('invalid');
                    input.classList.add('valid');
                    errorDiv.textContent = '';
                    nameValid = true;
                    if (continueBtn) continueBtn.disabled = false;
                    // Auto-derive XMPP JID if XMPP enabled and JID empty
                    autoDeriveJID(name);
                } else {
                    input.classList.remove('valid');
                    input.classList.add('invalid');
                    errorDiv.textContent = data.error || 'Invalid name';
                    nameValid = false;
                    if (continueBtn) continueBtn.disabled = true;
                }
            })
            .catch(function () {
                input.classList.remove('valid');
                    input.classList.add('invalid');
                    errorDiv.textContent = 'Validation request failed';
                    nameValid = false;
                    if (continueBtn) continueBtn.disabled = true;
            });
    }, 300);
}

function autoDeriveJID(name) {
    var jidInput = document.getElementById('input-xmpp-jid');
    var domainInput = document.getElementById('input-xmpp-domain');
    if (!jidInput || !domainInput) return;
    // Only auto-derive if JID is empty or was previously auto-derived
    if (!jidInput.value || jidInput.dataset.autoDerived === 'true') {
        var domain = domainInput.value || 'xmpp.localhost';
        jidInput.value = name + '@' + domain;
        jidInput.dataset.autoDerived = 'true';
    }
}

// ---------------------------------------------------------------------------
// Toggle Handlers
// ---------------------------------------------------------------------------

function setupToggleHandlers() {
    // XMPP toggle
    var xmppToggle = document.getElementById('toggle-xmpp');
    var xmppOptions = document.getElementById('xmpp-options');
    if (xmppToggle && xmppOptions) {
        xmppToggle.addEventListener('change', function () {
            xmppOptions.style.display = xmppToggle.checked ? '' : 'none';
        });
    }

    // Gotify toggle
    var gotifyToggle = document.getElementById('toggle-gotify');
    var gotifyOptions = document.getElementById('gotify-options');
    if (gotifyToggle && gotifyOptions) {
        gotifyToggle.addEventListener('change', function () {
            gotifyOptions.style.display = gotifyToggle.checked ? '' : 'none';
        });
    }

    // LLM model select
    var llmSelect = document.getElementById('select-llm-model');
    var llmCustomGroup = document.getElementById('llm-custom-group');
    if (llmSelect && llmCustomGroup) {
        llmSelect.addEventListener('change', function () {
            llmCustomGroup.style.display = llmSelect.value === '__custom__' ? '' : 'none';
        });
    }

    // XMPP domain change — update auto-derived JID
    var domainInput = document.getElementById('input-xmpp-domain');
    if (domainInput) {
        domainInput.addEventListener('input', function () {
            var nameInput = document.getElementById('input-name');
            if (nameInput && nameInput.value.trim()) {
                var jidInput = document.getElementById('input-xmpp-jid');
                if (jidInput && jidInput.dataset.autoDerived === 'true') {
                    jidInput.value = nameInput.value.trim() + '@' + (domainInput.value || 'xmpp.localhost');
                }
            }
        });
    }

    // XMPP JID manual edit — stop auto-deriving
    var jidInput = document.getElementById('input-xmpp-jid');
    if (jidInput) {
        jidInput.addEventListener('input', function () {
            jidInput.dataset.autoDerived = 'false';
        });
    }
}

// ---------------------------------------------------------------------------
// Worker Selection
// ---------------------------------------------------------------------------

function toggleWorker(workerType) {
    var card = document.getElementById(WORKER_IDS[workerType]);
    if (!card) return;
    card.classList.toggle('selected');

    // Show/hide toolchains toggle
    var toolchainsGroup = document.getElementById('toolchains-group');
    var selected = getSelectedWorkers();
    if (toolchainsGroup) {
        toolchainsGroup.style.display = selected.length > 0 ? '' : 'none';
    }

    // Bat gets excited when a worker is selected
    if (selected.length > 0) {
        setBat('excited');
        setTimeout(function () {
            if (!provisioningActive) setBat('content');
        }, 2000);
    }
}

// ---------------------------------------------------------------------------
// Master Key Generation
// ---------------------------------------------------------------------------

function generateMasterKey() {
    var bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    return Array.from(bytes).map(function (b) {
        return b.toString(16).padStart(2, '0');
    }).join('');
}

function generateAndSetKey() {
    var input = document.getElementById('input-master-key');
    if (input) {
        input.value = generateMasterKey();
    }
}

// ---------------------------------------------------------------------------
// Config Collection
// ---------------------------------------------------------------------------

function collectConfig() {
    return {
        name: getValue('input-name'),
        gateway_host: getValue('input-gateway-host', '127.0.0.1'),
        docker_group: isChecked('toggle-docker'),
        enable_darkirc: isChecked('toggle-darkirc'),
        xmpp_enabled: isChecked('toggle-xmpp'),
        xmpp_jid: getValue('input-xmpp-jid'),
        xmpp_password: getValue('input-xmpp-password'),
        xmpp_allow_from: parseList(getValue('input-xmpp-allow-from')),
        gotify_enabled: isChecked('toggle-gotify'),
        gotify_url: getValue('input-gotify-url'),
        gotify_title: getValue('input-gotify-title'),
        workers: getSelectedWorkers(),
        toolchains: isChecked('toggle-toolchains'),
        tensorzero_url: getValue('input-tensorzero-url', 'http://192.168.1.157:3000/openai/v1'),
        llm_model: getLLMModel(),
        llm_api_key: getValue('input-llm-api-key'),
        secrets_master_key: getValue('input-master-key'),
        no_ssh: !isChecked('toggle-ssh'),
        no_health: !isChecked('toggle-health'),
    };
}

// ---------------------------------------------------------------------------
// Review Population
// ---------------------------------------------------------------------------

function populateReview() {
    var config = collectConfig();
    lastConfig = config;

    var rows = [
        { label: 'Tenant Name', value: config.name },
        { label: 'Gateway Host', value: config.gateway_host },
        { label: 'Docker Group', value: config.docker_group ? 'Yes' : 'No' },
        { label: 'DarkIRC', value: config.enable_darkirc ? 'Enabled' : 'Disabled' },
        { label: 'XMPP', value: config.xmpp_enabled ? 'Enabled' : 'Disabled' },
    ];

    if (config.xmpp_enabled) {
        rows.push({ label: '  XMPP JID', value: config.xmpp_jid });
        rows.push({ label: '  XMPP Password', value: maskSecret(config.xmpp_password) });
        if (config.xmpp_allow_from.length > 0) {
            rows.push({ label: '  Allow From', value: config.xmpp_allow_from.join(', ') });
        }
    }

    rows.push({ label: 'Gotify', value: config.gotify_enabled ? 'Enabled' : 'Disabled' });
    if (config.gotify_enabled) {
        rows.push({ label: '  Gotify URL', value: config.gotify_url });
        rows.push({ label: '  Gotify Title', value: config.gotify_title || '(default)' });
    }

    rows.push({ label: 'Workers', value: config.workers.length > 0 ? config.workers.join(', ') : 'None' });
    rows.push({ label: 'Toolchains', value: config.toolchains ? 'Yes (+~5GB)' : 'No' });
    rows.push({ label: 'TensorZero URL', value: config.tensorzero_url });
    rows.push({ label: 'LLM Model', value: config.llm_model });
    rows.push({ label: 'LLM API Key', value: maskSecret(config.llm_api_key) });
    rows.push({ label: 'Secrets Master Key', value: maskSecret(config.secrets_master_key) });
    rows.push({ label: 'SSH Harness', value: config.no_ssh ? 'Disabled' : 'Enabled' });
    rows.push({ label: 'Health Pipeline', value: config.no_health ? 'Disabled' : 'Enabled' });

    var table = document.getElementById('summary-table');
    if (!table) return;
    table.innerHTML = '';

    for (var i = 0; i < rows.length; i++) {
        var row = document.createElement('div');
        row.className = 'summary-row';
        var labelEl = document.createElement('span');
        labelEl.className = 'summary-label';
        labelEl.textContent = rows[i].label;
        var valueEl = document.createElement('span');
        valueEl.className = 'summary-value';
        valueEl.textContent = rows[i].value;
        row.appendChild(labelEl);
        row.appendChild(valueEl);
        table.appendChild(row);
    }
}

// ---------------------------------------------------------------------------
// Provisioning
// ---------------------------------------------------------------------------

async function startProvisioning() {
    var config = collectConfig();
    lastConfig = config;

    showStep('provisioning');
    provisioningActive = true;
    stopMascotCycle();
    setBat('sleeping');

    // Reset UI
    clearTerminal();
    clearPhaseStepper();
    updateProgress(0);
    hideSuccessPanel();
    hideErrorPanel();

    var btnProvision = document.getElementById('btn-provision');
    if (btnProvision) btnProvision.disabled = true;

    try {
        var resp = await fetch('/api/provision', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(config),
        });

        if (!resp.ok) {
            var errData = await resp.json().catch(function () { return {}; });
            throw new Error(errData.error || 'Provisioning request failed (HTTP ' + resp.status + ')');
        }

        var data = await resp.json();
        if (data.error) {
            throw new Error(data.error);
        }

        connectSSE(data.session_id);
    } catch (e) {
        onProvisioningError(e.message || 'Unknown error');
    }
}

function connectSSE(sessionId) {
    var evtSource = new EventSource('/api/provision/' + sessionId + '/stream');

    evtSource.onmessage = function (e) {
        var line;
        try {
            line = JSON.parse(e.data);
        } catch (err) {
            // If not JSON, treat as raw text
            line = e.data;
        }

        if (line === '[DONE]') {
            evtSource.close();
            onProvisioningComplete(sessionId);
            return;
        }

        if (line === '[HEARTBEAT]') {
            return;
        }

        if (typeof line !== 'string') {
            appendTerminal(String(line));
            return;
        }

        if (line.indexOf('[PHASE]') === 0) {
            updatePhase(line);
            setBat('sleeping');
            appendTerminal(line);
            return;
        }

        if (line.indexOf('[OK]') === 0) {
            markPhaseDone(line);
            setBat('excited');
            setTimeout(function () {
                if (provisioningActive) setBat('sleeping');
            }, 2000);
            appendTerminal(line);
            return;
        }

        if (line.indexOf('[FAIL]') === 0) {
            markPhaseFailed(line);
            setBat('angry');
            appendTerminal(line);
            return;
        }

        if (line.indexOf('[ERROR]') === 0) {
            setBat('angry');
            appendTerminal(line);
            return;
        }

        appendTerminal(line);
    };

    evtSource.onerror = function () {
        evtSource.close();
        setBat('angry');
        // If not done yet, check status endpoint
        checkFinalStatus(sessionId);
    };
}

function checkFinalStatus(sessionId) {
    fetch('/api/provision/' + sessionId + '/status')
        .then(function (r) { return r.json(); })
        .then(function (data) {
            if (data.status === 'ok') {
                onProvisioningComplete(sessionId);
            } else if (data.status === 'fail') {
                onProvisioningError(data.error || 'Provisioning failed');
            } else {
                // Still running? Show generic error.
                onProvisioningError('SSE connection lost');
            }
        })
        .catch(function () {
            onProvisioningError('SSE connection lost');
        });
}

function onProvisioningComplete(sessionId) {
    provisioningActive = false;
    setBat('excited');
    updateProgress(100);
    updateMoon(STEPS.length - 1);

    // Fetch verification results
    if (lastConfig) {
        fetch('/api/verify/' + encodeURIComponent(lastConfig.name))
            .then(function (r) { return r.json(); })
            .then(function (data) {
                showSuccessPanel(data.results || []);
            })
            .catch(function () {
                showSuccessPanel([]);
            });
    } else {
        showSuccessPanel([]);
    }
}

function onProvisioningError(msg) {
    provisioningActive = false;
    setBat('angry');
    showErrorPanel(msg);

    var btnProvision = document.getElementById('btn-provision');
    if (btnProvision) btnProvision.disabled = false;
}

function retryProvisioning() {
    hideErrorPanel();
    showStep('review');
    setBat('content');
    startMascotCycle();

    var btnProvision = document.getElementById('btn-provision');
    if (btnProvision) btnProvision.disabled = false;
}

// ---------------------------------------------------------------------------
// Terminal Output
// ---------------------------------------------------------------------------

function appendTerminal(text) {
    var terminal = document.getElementById('terminal-output');
    if (!terminal) return;

    var line = document.createElement('div');
    line.className = 'terminal-line';

    if (typeof text === 'string') {
        if (text.indexOf('[OK]') === 0) {
            line.classList.add('terminal-ok');
        } else if (text.indexOf('[FAIL]') === 0 || text.indexOf('[ERROR]') === 0) {
            line.classList.add('terminal-error');
        } else if (text.indexOf('[PHASE]') === 0) {
            line.classList.add('terminal-phase');
        }
        line.textContent = text;
    } else {
        line.textContent = JSON.stringify(text);
    }

    terminal.appendChild(line);
    terminal.scrollTop = terminal.scrollHeight;
}

function clearTerminal() {
    var terminal = document.getElementById('terminal-output');
    if (terminal) terminal.innerHTML = '';
}

// ---------------------------------------------------------------------------
// Phase Stepper
// ---------------------------------------------------------------------------

function clearPhaseStepper() {
    var stepper = document.getElementById('phase-stepper');
    if (stepper) stepper.innerHTML = '';
}

function updatePhase(line) {
    // Parse "[PHASE] Starting: <name>"
    var match = line.match(/\[PHASE\]\s*Starting:\s*(.+)/);
    var phaseName = match ? match[1].trim() : line;

    var stepper = document.getElementById('phase-stepper');
    if (!stepper) return;

    // Check if this phase already exists
    var existing = stepper.querySelector('[data-phase="' + cssEscape(phaseName) + '"]');
    if (existing) {
        existing.classList.remove('pending');
        existing.classList.add('running');
        return;
    }

    var item = document.createElement('div');
    item.className = 'phase-item running';
    item.dataset.phase = phaseName;

    var icon = document.createElement('span');
    icon.className = 'phase-icon';
    icon.textContent = '\u25CB'; // ○

    var label = document.createElement('span');
    label.className = 'phase-label';
    label.textContent = phaseName;

    item.appendChild(icon);
    item.appendChild(label);
    stepper.appendChild(item);
}

function markPhaseDone(line) {
    // Parse "[OK] <name> (exit N)"
    var match = line.match(/\[OK\]\s*(.+?)\s*\(exit\s*\d+\)/);
    var phaseName = match ? match[1].trim() : '';

    var stepper = document.getElementById('phase-stepper');
    if (!stepper) return;

    if (phaseName) {
        var item = findPhaseItem(stepper, phaseName);
        if (item) {
            item.classList.remove('running', 'pending', 'failed');
            item.classList.add('done');
            var icon = item.querySelector('.phase-icon');
            if (icon) icon.textContent = '\u2713'; // ✓
        }
    }

    updatePhaseProgress();
}

function markPhaseFailed(line) {
    var match = line.match(/\[FAIL\]\s*(.+?)\s*\(exit\s*\d+\)/);
    var phaseName = match ? match[1].trim() : '';

    var stepper = document.getElementById('phase-stepper');
    if (!stepper) return;

    if (phaseName) {
        var item = findPhaseItem(stepper, phaseName);
        if (item) {
            item.classList.remove('running', 'pending', 'done');
            item.classList.add('failed');
            var icon = item.querySelector('.phase-icon');
            if (icon) icon.textContent = '\u2717'; // ✗
        }
    }
}

function findPhaseItem(stepper, name) {
    var items = stepper.querySelectorAll('.phase-item');
    for (var i = 0; i < items.length; i++) {
        if (items[i].dataset.phase === name) return items[i];
    }
    return null;
}

function updatePhaseProgress() {
    var stepper = document.getElementById('phase-stepper');
    if (!stepper) return;
    var items = stepper.querySelectorAll('.phase-item');
    var done = stepper.querySelectorAll('.phase-item.done');
    if (items.length > 0) {
        var pct = Math.round((done.length / items.length) * 100);
        updateProgress(pct);
    }
}

function updateProgress(pct) {
    var fill = document.getElementById('progress-fill');
    if (fill) {
        fill.style.width = pct + '%';
    }
}

// ---------------------------------------------------------------------------
// Success / Error Panels
// ---------------------------------------------------------------------------

function showSuccessPanel(results) {
    var panel = document.getElementById('success-panel');
    var details = document.getElementById('success-details');
    if (!panel) return;
    panel.style.display = '';

    if (details) {
        details.innerHTML = '';
        if (results.length > 0) {
            var heading = document.createElement('h3');
            heading.textContent = 'Verification Results';
            heading.className = 'verification-heading';
            details.appendChild(heading);

            for (var i = 0; i < results.length; i++) {
                var row = document.createElement('div');
                row.className = 'verification-row';

                var icon = document.createElement('span');
                icon.className = 'verification-icon ' + (results[i].ok ? 'ok' : 'fail');
                icon.textContent = results[i].ok ? '\u2713' : '\u2717';

                var label = document.createElement('span');
                label.className = 'verification-label';
                label.textContent = results[i].name || results[i].check || 'Check';

                var detail = document.createElement('span');
                detail.className = 'verification-detail';
                detail.textContent = results[i].detail || results[i].message || '';

                row.appendChild(icon);
                row.appendChild(label);
                row.appendChild(detail);
                details.appendChild(row);
            }
        }
    }
}

function hideSuccessPanel() {
    var panel = document.getElementById('success-panel');
    if (panel) panel.style.display = 'none';
}

function showErrorPanel(msg) {
    var panel = document.getElementById('error-panel');
    var msgEl = document.getElementById('error-message');
    if (panel) panel.style.display = '';
    if (msgEl) msgEl.textContent = msg;
}

function hideErrorPanel() {
    var panel = document.getElementById('error-panel');
    if (panel) panel.style.display = 'none';
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

function cssEscape(str) {
    // Simple escape for use in CSS selector strings
    return str.replace(/[^a-zA-Z0-9_-]/g, function (c) {
        return '\\' + c.charCodeAt(0).toString(16).padStart(2, '0') + ' ';
    });
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

document.addEventListener('DOMContentLoaded', function () {
    buildStepDots();
    updateMoon(0);
    setBat('content');
    startMascotCycle();

    // Name validation
    var nameInput = document.getElementById('input-name');
    if (nameInput) {
        nameInput.addEventListener('input', validateName);
    }

    // Toggle handlers
    setupToggleHandlers();
});
