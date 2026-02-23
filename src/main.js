const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const W = 300;
const MICRO_W = 56;
const PILL_H = 32;
const REC_H = 64;
const BAR_COUNT = 28;
const BAR_W = 7;
const BAR_GAP = 2;

// DOM
const pill = document.getElementById('pill');
const dot = document.getElementById('dot');
const deviceName = document.getElementById('device-name');
const timerEl = document.getElementById('timer');
const statusText = document.getElementById('status-text');
const settingsBtn = document.getElementById('settings-btn');
const chunkDots = document.getElementById('chunk-dots');

const recPanel = document.getElementById('rec-panel');
const waveformCanvas = document.getElementById('waveform');
const waveformCtx = waveformCanvas.getContext('2d');
const transcriptText = document.getElementById('transcript-text');

const settingsPanel = document.getElementById('settings-panel');
const deviceSelect = document.getElementById('device-select');
const languageSelect = document.getElementById('language-select');
const modelSelect = document.getElementById('model-select');
const customFields = document.getElementById('custom-fields');
const apiUrlInput = document.getElementById('api-url');
const apiModelInput = document.getElementById('api-model');
const apiKeyInput = document.getElementById('api-key');
const keyHint = document.getElementById('key-hint');
const shortcutModeSelect = document.getElementById('shortcut-mode-select');
const saveBtn = document.getElementById('save-btn');
const closeBtn = document.getElementById('close-btn');

let isRecording = false;
let waveformInterval = null;
let timerInterval = null;
let recordingStart = 0;
let chunkTexts = [];
let currentView = 'micro'; // 'micro' | 'pill' | 'rec' | 'settings'
let shrinkTimeout = null;
let energyHistory = [];
let waveformPending = false;

// ========================
// RESIZE — Rust-side
// ========================
async function setWindowSizeWH(w, h) {
  try {
    await invoke('resize_window', { w, h });
  } catch (e) {
    console.error('resize_window failed:', e);
  }
}

function switchView(view) {
  if (shrinkTimeout) { clearTimeout(shrinkTimeout); shrinkTimeout = null; }

  recPanel.classList.remove('open');
  settingsPanel.classList.remove('open');
  currentView = view;

  if (view === 'micro') {
    pill.classList.add('micro');
    deviceName.classList.add('hide');
    timerEl.classList.add('hide');
    statusText.classList.add('hide');
    chunkDots.innerHTML = '';
    setWindowSizeWH(MICRO_W, PILL_H);
  } else if (view === 'pill') {
    pill.classList.remove('micro');
    deviceName.classList.remove('hide');
    setWindowSizeWH(W, PILL_H);
  } else if (view === 'rec') {
    pill.classList.remove('micro');
    recPanel.classList.add('open');
    setWindowSizeWH(W, REC_H);
  } else if (view === 'settings') {
    pill.classList.remove('micro');
    settingsPanel.classList.add('open');
    requestAnimationFrame(() => {
      const container = document.getElementById('container');
      setWindowSizeWH(W, container.offsetHeight);
    });
  }
}

// Shrink to micro after delay
function shrinkToMicro(delay) {
  if (shrinkTimeout) clearTimeout(shrinkTimeout);
  shrinkTimeout = setTimeout(() => {
    if (!isRecording && currentView === 'pill') {
      switchView('micro');
    }
  }, delay);
}

// ========================
// INIT
// ========================
async function init() {
  try {
    const name = await invoke('get_audio_device');
    deviceName.textContent = name;
    deviceName.title = name;
  } catch (_) {
    deviceName.textContent = 'No mic';
  }
  switchView('micro');
}
init();

// ========================
// HOVER — expand micro on mouse enter, shrink back on leave
// ========================
let hoverTimeout = null;
const container = document.getElementById('container');

container.addEventListener('mouseenter', () => {
  if (hoverTimeout) { clearTimeout(hoverTimeout); hoverTimeout = null; }
  if (currentView === 'micro' && !isRecording) {
    switchView('pill');
  }
});

container.addEventListener('mouseleave', () => {
  if (hoverTimeout) { clearTimeout(hoverTimeout); hoverTimeout = null; }
  if (currentView === 'pill' && !isRecording) {
    hoverTimeout = setTimeout(() => {
      hoverTimeout = null;
      if (currentView === 'pill' && !isRecording) {
        switchView('micro');
      }
    }, 800);
  }
});

// Fallback: if somehow stuck in pill after hover, auto-shrink
setInterval(() => {
  if (currentView === 'pill' && !isRecording && !hoverTimeout && !shrinkTimeout) {
    switchView('micro');
  }
}, 5000);

// ========================
// SHORTCUTS
// ========================
listen('shortcut-start', async () => { if (!isRecording) await startRecording(); });
listen('shortcut-stop', async () => { if (isRecording) await stopRecording(); });

// ========================
// STREAMING EVENTS
// ========================
listen('transcription-chunk', (event) => {
  const { index, text } = event.payload;
  chunkTexts[index - 1] = text;
  const visible = chunkTexts.filter(Boolean).slice(-2).join(' ');
  transcriptText.textContent = visible;
  transcriptText.scrollLeft = transcriptText.scrollWidth;
  updateChunkDot(index, 'done');
});

listen('chunk-status', (event) => {
  const { index, status } = event.payload;
  if (status === 'sending') {
    addChunkDot(index, 'sending');
    showStatus(`#${index}`);
  } else if (status === 'error') {
    updateChunkDot(index, 'error');
    showStatus('err');
  } else if (status === 'final') {
    addChunkDot(0, 'final');
    showStatus('final');
  }
});

listen('transcription-final', (event) => {
  transcriptText.textContent = event.payload.text;
  transcriptText.scrollLeft = transcriptText.scrollWidth;
  showStatus('done');
});

// ========================
// CHUNK DOTS — in pill row
// ========================
function addChunkDot(i, cls) {
  let el = document.getElementById(`c${i}`);
  if (!el) {
    el = document.createElement('div');
    el.id = `c${i}`;
    chunkDots.appendChild(el);
  }
  el.className = `chunk-dot ${cls}`;
}

function updateChunkDot(i, cls) {
  const el = document.getElementById(`c${i}`);
  if (el) el.className = `chunk-dot ${cls}`;
}

// ========================
// STATUS
// ========================
function showStatus(msg) {
  statusText.textContent = msg;
  statusText.classList.remove('hide');
}
function hideStatus() { statusText.classList.add('hide'); }
function showTimer() { timerEl.classList.remove('hide'); deviceName.classList.add('hide'); }
function hideTimer() { timerEl.classList.add('hide'); deviceName.classList.remove('hide'); }

// ========================
// RECORDING
// ========================
async function startRecording() {
  try {
    await invoke('start_recording');
  } catch (err) {
    dot.className = 'error';
    showStatus(String(err).substring(0, 30));
    setTimeout(() => { dot.className = ''; hideStatus(); shrinkToMicro(5000); }, 4000);
    return;
  }

  isRecording = true;
  settingsBtn.disabled = true;
  dot.className = 'recording';
  showTimer();
  showStatus('rec');

  chunkTexts = [];
  energyHistory = [];
  transcriptText.textContent = '';
  chunkDots.innerHTML = '';

  switchView('rec');

  recordingStart = Date.now();
  updateTimerDisplay();
  timerInterval = setInterval(updateTimerDisplay, 1000);

  // Delay canvas setup: window resize is async (Rust IPC), so wait for
  // two rAF frames + a short timeout to ensure the DOM has reflowed at
  // the correct 300px width. Without this, canvas measures the old micro
  // size (56px) and bars appear zoomed/oversized.
  setTimeout(() => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        setupCanvas();
        waveformInterval = setInterval(pollWaveform, 30);
      });
    });
  }, 50);
}

async function stopRecording() {
  if (timerInterval) { clearInterval(timerInterval); timerInterval = null; }
  if (waveformInterval) { clearInterval(waveformInterval); waveformInterval = null; }

  dot.className = 'transcribing';
  showStatus('transcribing');

  try {
    const text = await invoke('stop_recording');
    isRecording = false;
    transcriptText.textContent = text;
    transcriptText.scrollLeft = transcriptText.scrollWidth;
    dot.className = '';
    showStatus('pasting');

    try { await invoke('paste_text', { text }); } catch (_) {}

    showStatus('done');
    setTimeout(() => {
      switchView('pill');
      hideTimer();
      hideStatus();
      settingsBtn.disabled = false;
      shrinkToMicro(5000);
    }, 2000);
  } catch (err) {
    isRecording = false;
    dot.className = 'error';
    showStatus(String(err).substring(0, 30));
    setTimeout(() => {
      switchView('pill');
      hideTimer();
      dot.className = '';
      hideStatus();
      settingsBtn.disabled = false;
      shrinkToMicro(5000);
    }, 4000);
  }
}

// ========================
// TIMER
// ========================
function updateTimerDisplay() {
  const s = Math.floor((Date.now() - recordingStart) / 1000);
  timerEl.textContent =
    String(Math.floor(s / 60)).padStart(2, '0') + ':' +
    String(s % 60).padStart(2, '0');
}

// ========================
// WAVEFORM
// ========================
function setupCanvas() {
  const rect = waveformCanvas.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  waveformCanvas.width = rect.width * dpr;
  waveformCanvas.height = rect.height * dpr;
  waveformCtx.scale(dpr, dpr);
}

// PS1-style: one RMS energy reading per tick, scrolling bar history
async function pollWaveform() {
  if (waveformPending) return;
  waveformPending = true;
  try {
    const amp = await invoke('get_amplitude');
    const norm = Math.min(1.0, amp * 8);
    energyHistory.push(norm);
    if (energyHistory.length > BAR_COUNT) {
      energyHistory = energyHistory.slice(energyHistory.length - BAR_COUNT);
    }
    drawBars();
  } catch (_) {}
  waveformPending = false;
}

function drawBars() {
  const rect = waveformCanvas.getBoundingClientRect();
  const w = rect.width;
  const h = rect.height;

  waveformCtx.clearRect(0, 0, w, h);

  const totalW = BAR_COUNT * (BAR_W + BAR_GAP) - BAR_GAP;
  const offsetX = (w - totalW) / 2;

  for (let i = 0; i < BAR_COUNT; i++) {
    const val = i < energyHistory.length ? energyHistory[i] : 0;
    const barH = Math.max(2, Math.floor(val * h));
    const x = offsetX + i * (BAR_W + BAR_GAP);

    if (val >= 0.7) {
      waveformCtx.fillStyle = '#818cf8';
    } else if (val > 0.05) {
      waveformCtx.fillStyle = '#6366f1';
    } else {
      waveformCtx.fillStyle = '#312e81';
    }

    waveformCtx.beginPath();
    waveformCtx.roundRect(x, h - barH, BAR_W, barH, 1);
    waveformCtx.fill();
  }
}

// ========================
// SETTINGS
// ========================
async function openSettings() {
  try {
    const config = await invoke('get_config');
    apiKeyInput.value = '';
    apiKeyInput.placeholder = config.has_key ? '(secured) enter new to change' : 'sk-... or gsk_...';
    keyHint.textContent = config.has_key ? '(saved securely)' : '';

    // Populate device dropdown
    deviceSelect.innerHTML = '';
    const defaultOpt = document.createElement('option');
    defaultOpt.value = '';
    defaultOpt.textContent = 'System Default';
    deviceSelect.appendChild(defaultOpt);
    if (config.devices) {
      for (const dev of config.devices) {
        const opt = document.createElement('option');
        opt.value = dev;
        opt.textContent = dev;
        if (config.selected_device === dev) opt.selected = true;
        deviceSelect.appendChild(opt);
      }
    }

    // Language
    languageSelect.value = config.language || '';

    // Shortcut mode
    shortcutModeSelect.value = config.shortcut_mode || 'toggle';

    // Model select
    let found = false;
    for (const opt of modelSelect.options) {
      if (opt.dataset.url === config.api_url && opt.dataset.model === config.api_model) {
        opt.selected = true;
        found = true;
        break;
      }
    }
    if (!found) {
      modelSelect.value = 'custom';
      apiUrlInput.value = config.api_url || '';
      apiModelInput.value = config.api_model || '';
      customFields.classList.remove('hide');
    } else {
      customFields.classList.add('hide');
    }
  } catch (err) {
    console.error('get_config:', err);
  }
  switchView('settings');
}

function closeSettings() {
  switchView('micro');
}

modelSelect.addEventListener('change', () => {
  if (modelSelect.value === 'custom') {
    customFields.classList.remove('hide');
  } else {
    customFields.classList.add('hide');
  }
  requestAnimationFrame(() => {
    const container = document.getElementById('container');
    setWindowSizeWH(W, container.offsetHeight);
  });
});

settingsBtn.addEventListener('click', (e) => {
  e.stopPropagation();
  if (currentView === 'settings') {
    closeSettings();
  } else {
    openSettings();
  }
});

closeBtn.addEventListener('click', closeSettings);

saveBtn.addEventListener('click', async () => {
  let apiUrl, apiModel;
  if (modelSelect.value === 'custom') {
    apiUrl = apiUrlInput.value.trim();
    apiModel = apiModelInput.value.trim();
    if (!apiUrl || !apiModel) { apiUrlInput.focus(); return; }
  } else {
    const opt = modelSelect.options[modelSelect.selectedIndex];
    apiUrl = opt.dataset.url;
    apiModel = opt.dataset.model;
  }

  const apiKey = apiKeyInput.value.trim() || null;
  const selectedDevice = deviceSelect.value || null;
  const language = languageSelect.value;
  const shortcutMode = shortcutModeSelect.value;

  if (!apiKey) {
    try {
      const config = await invoke('get_config');
      if (!config.has_key) {
        apiKeyInput.focus();
        return;
      }
    } catch (_) {}
  }

  try {
    await invoke('set_config', { apiKey, apiUrl, apiModel, language, shortcutMode, selectedDevice });
    const name = await invoke('get_audio_device');
    deviceName.textContent = name;
    deviceName.title = name;
    closeSettings();
  } catch (err) {
    console.error('save:', err);
    keyHint.textContent = 'Save failed: ' + err;
  }
});
