const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const W = 360;
const MICRO_W = 56;
const PILL_H = 32;
const REC_H = 64;
const BAR_COUNT = 28;
const BAR_W = 7;
const BAR_GAP = 2;

// LLM style -> dot color map
const STYLE_COLORS = {
  off: '#34d399',      // green — LLM off
  correct: '#2dd4bf',  // teal
  summarize: '#fbbf24', // amber
  elaborate: '#4ade80', // light green
  comprehensible: '#38bdf8', // sky blue
  professional: '#f472b6', // pink
  prompt: '#a78bfa',    // violet
  genz: '#e879f9',     // fuchsia
  boomer: '#f97316',   // orange-warm
  emoji: '#facc15',    // yellow
  acronyms: '#22d3ee', // cyan
  imbruttito: '#ef4444', // red
  custom: '#fb923c',   // orange
};

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
const shortcutLabel = document.getElementById('shortcut-label');
const shortcutRecordBtn = document.getElementById('shortcut-record-btn');
const shortcutModeSelect = document.getElementById('shortcut-mode-select');
const promptInput = document.getElementById('prompt-input');
const saveBtn = document.getElementById('save-btn');
const closeBtn = document.getElementById('close-btn');

const preprocessingCheckbox = document.getElementById('preprocessing-enabled');
const audioDebugCheckbox = document.getElementById('audio-debug-enabled');
const chunkStreamingCheckbox = document.getElementById('chunk-streaming-enabled');
const themeSelect = document.getElementById('theme-select');
const advancedSection = document.getElementById('advanced-section');
const advancedToggle = document.getElementById('advanced-toggle');

// LLM DOM
const llmStyleSelect = document.getElementById('llm-style-select');
const llmTranslateSelect = document.getElementById('llm-translate-select');
const llmToneSelect = document.getElementById('llm-tone-select');
const llmCustomPromptField = document.getElementById('llm-custom-prompt-field');
const llmCustomPrompt = document.getElementById('llm-custom-prompt');
const llmProviderSelect = document.getElementById('llm-provider-select');
const llmCustomEndpoint = document.getElementById('llm-custom-endpoint');
const llmApiUrlInput = document.getElementById('llm-api-url');
const llmApiModelInput = document.getElementById('llm-api-model');
const llmSameKeyCheckbox = document.getElementById('llm-same-key');
const llmKeyField = document.getElementById('llm-key-field');
const llmApiKeyInput = document.getElementById('llm-api-key');
const llmKeyHint = document.getElementById('llm-key-hint');
const llmLogCheckbox = document.getElementById('llm-log-enabled');

// === Theme ===
function applyTheme(theme) {
  document.documentElement.setAttribute('data-theme', theme);
  localStorage.setItem('dimmy-theme', theme);
}
// Apply saved theme immediately on load
applyTheme(localStorage.getItem('dimmy-theme') || 'auto');

themeSelect.addEventListener('change', () => applyTheme(themeSelect.value));

// Accessibility buttons (macOS)
const accessibilityGrantBtn = document.getElementById('accessibility-grant-btn');
if (accessibilityGrantBtn) {
  accessibilityGrantBtn.addEventListener('click', async () => {
    await invoke('prompt_accessibility');
  });
}
const accessibilityOpenBtn = document.getElementById('accessibility-open-btn');
if (accessibilityOpenBtn) {
  accessibilityOpenBtn.addEventListener('click', async () => {
    await invoke('open_accessibility_settings');
  });
}

let isRecording = false;
let waveformInterval = null;
let timerInterval = null;
let recordingStart = 0;
let chunkTexts = [];
let currentView = 'micro'; // 'micro' | 'pill' | 'rec' | 'settings'
let shrinkTimeout = null;
let energyHistory = [];
let waveformPending = false;
let peakAmplitude = 0.0001; // auto-scaling: tracks recent peak mic level

// LLM state
let llmEnabled = false;
let llmStyle = 'off';
let llmTone = 'none';
let styleFlashTimeout = null;

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
// LLM STYLE INDICATOR
// ========================
function updateStyleIndicator() {
  // Don't override recording/transcribing/error states
  if (dot.classList.contains('recording') ||
      dot.classList.contains('transcribing') ||
      dot.classList.contains('error') ||
      dot.classList.contains('llm-processing')) {
    return;
  }

  if (llmEnabled) {
    const color = STYLE_COLORS[llmStyle] || STYLE_COLORS.off;
    dot.style.setProperty('--style-color', color);
    dot.className = 'styled';
  } else {
    dot.style.removeProperty('--style-color');
    dot.className = '';
  }
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
  await loadLlmState();
  switchView('micro');
}
init();

async function loadLlmState() {
  try {
    const config = await invoke('get_config');
    llmStyle = config.llm_style || 'off';
    llmTone = config.llm_tone || 'none';
    const translateTo = config.llm_translate_to || 'none';
    llmEnabled = llmStyle !== 'off' || translateTo !== 'none';
    updateStyleIndicator();
  } catch (_) {}
}

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
// SCROLL-WHEEL CYCLING on pill (LLM style/tone)
// ========================
pill.addEventListener('wheel', async (e) => {
  // Only cycle when not recording and not in settings
  if (isRecording || currentView === 'settings') return;
  e.preventDefault();

  const direction = e.deltaY > 0 ? 1 : -1;

  try {
    if (e.ctrlKey) {
      // Ctrl+scroll = cycle tone
      const result = await invoke('cycle_llm_tone', { direction });
      llmTone = result.tone;
      showStatus(`tone: ${result.tone}`);
    } else {
      // Scroll = cycle style
      const result = await invoke('cycle_llm_style', { direction });
      llmStyle = result.style;
      llmEnabled = result.style !== 'off';
      showStatus(result.style);
    }
    updateStyleIndicator();
    // Flash status for 1.5s
    if (styleFlashTimeout) clearTimeout(styleFlashTimeout);
    styleFlashTimeout = setTimeout(() => {
      hideStatus();
      styleFlashTimeout = null;
    }, 1500);
  } catch (err) {
    console.error('cycle error:', err);
  }
}, { passive: false });

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
// LLM STATUS EVENTS
// ========================
listen('llm-status', (event) => {
  const { status, error } = event.payload;
  if (status === 'processing') {
    dot.className = 'llm-processing';
    showStatus('enhancing');
  } else if (status === 'done') {
    updateStyleIndicator();
  } else if (status === 'error') {
    console.error('LLM error:', error);
    // Brief error indication, then back to normal
    showStatus('llm err');
    setTimeout(() => updateStyleIndicator(), 2000);
  }
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
    setTimeout(() => { dot.className = ''; updateStyleIndicator(); hideStatus(); shrinkToMicro(5000); }, 4000);
    return;
  }

  isRecording = true;
  settingsBtn.disabled = true;
  dot.className = 'recording';
  showTimer();
  showStatus('rec');

  chunkTexts = [];
  energyHistory = [];
  peakAmplitude = 0.0001;
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
  const speakingSecs = (Date.now() - recordingStart) / 1000;

  dot.className = 'transcribing';
  showStatus('transcribing');

  try {
    let text = await invoke('stop_recording');
    isRecording = false;
    transcriptText.textContent = text;
    transcriptText.scrollLeft = transcriptText.scrollWidth;

    // LLM post-processing if enabled (style or translate active)
    if (llmEnabled) {
      dot.className = 'llm-processing';
      showStatus('enhancing');
      try {
        text = await invoke('process_with_llm', { text });
      } catch (llmErr) {
        console.error('LLM fallback:', llmErr);
        // text remains the original transcription
      }
      transcriptText.textContent = text;
      transcriptText.scrollLeft = transcriptText.scrollWidth;
    }

    dot.className = '';
    updateStyleIndicator();
    showStatus('pasting');

    try { await invoke('paste_text', { text }); } catch (_) {}

    // Accumulate KPI stats
    const wordCount = text.trim().split(/\s+/).filter(w => w.length > 0).length;
    if (wordCount > 0) {
      try { await invoke('update_stats', { words: wordCount, speakingSecs }); } catch (_) {}
    }

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
      updateStyleIndicator();
      hideStatus();
      settingsBtn.disabled = false;
      shrinkToMicro(5000);
    }, 4000);
  }
}

// ========================
// STATS FORMATTING
// ========================
function formatDuration(totalSecs) {
  if (totalSecs < 60) return Math.round(totalSecs) + 's';
  if (totalSecs < 3600) return (totalSecs / 60).toFixed(1) + ' min';
  return (totalSecs / 3600).toFixed(1) + ' hrs';
}

function formatTimeSaved(mins) {
  if (mins < 1) return Math.round(mins * 60) + 's';
  if (mins < 60) return mins.toFixed(1) + ' min';
  return (mins / 60).toFixed(1) + ' hrs';
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
    // Auto-scale: track peak with fast attack, slow decay (~3s to halve)
    if (amp > peakAmplitude) {
      peakAmplitude = amp;
    } else {
      peakAmplitude = Math.max(0.0001, peakAmplitude * 0.997);
    }
    const norm = Math.min(1.0, amp / peakAmplitude * 0.85);
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

// Per-provider key status (populated by openSettings)
let providerKeyFlags = {};

function getProviderFromUrl(url) {
  if (!url) return 'custom';
  if (url.includes('groq.com')) return 'groq';
  if (url.includes('openai.com')) return 'openai';
  if (url.includes('openrouter.ai')) return 'openrouter';
  if (url.includes('googleapis.com')) return 'gemini';
  if (url.includes('anthropic.com')) return 'anthropic';
  return 'custom';
}

function providerLabel(provider) {
  return { groq: 'Groq', openai: 'OpenAI', openrouter: 'OpenRouter', gemini: 'Gemini', anthropic: 'Anthropic', custom: 'Custom' }[provider] || provider;
}

function updateApiKeyHint(url) {
  const provider = getProviderFromUrl(url);
  const hasKey = providerKeyFlags['has_' + provider + '_key'];
  const name = providerLabel(provider);
  apiKeyInput.value = '';
  apiKeyInput.placeholder = hasKey ? `(${name} key saved) enter new to change` : 'sk-... or gsk_...';
  keyHint.textContent = hasKey ? `(${name} key saved)` : '';
}

function updateLlmKeyHint(url) {
  const provider = getProviderFromUrl(url);
  const hasKey = providerKeyFlags['has_llm_' + provider + '_key'];
  const name = providerLabel(provider);
  llmApiKeyInput.value = '';
  llmApiKeyInput.placeholder = hasKey ? `(${name} key saved) enter new to change` : 'Separate LLM API key...';
  if (llmKeyHint) {
    llmKeyHint.textContent = hasKey ? `(${name} key saved)` : '';
  }
}

async function openSettings() {
  try {
    const config = await invoke('get_config');

    // Store per-provider key flags
    providerKeyFlags = {
      has_groq_key: config.has_groq_key,
      has_openai_key: config.has_openai_key,
      has_gemini_key: config.has_gemini_key,
      has_custom_key: config.has_custom_key,
      has_llm_groq_key: config.has_llm_groq_key,
      has_llm_openai_key: config.has_llm_openai_key,
      has_llm_openrouter_key: config.has_llm_openrouter_key,
      has_llm_gemini_key: config.has_llm_gemini_key,
      has_llm_anthropic_key: config.has_llm_anthropic_key,
      has_llm_custom_key: config.has_llm_custom_key,
    };

    updateApiKeyHint(config.api_url);

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

    // Shortcut key
    shortcutLabel.textContent = config.shortcut_label || 'Press to set';
    shortcutRecordBtn.textContent = 'Change';

    // Shortcut mode
    shortcutModeSelect.value = config.shortcut_mode || 'toggle';

    // Theme
    themeSelect.value = localStorage.getItem('dimmy-theme') || 'auto';

    // Prompt / vocabulary
    promptInput.value = config.prompt || '';

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

    // Preprocessing
    preprocessingCheckbox.checked = config.preprocessing_enabled !== false;
    audioDebugCheckbox.checked = config.audio_debug_enabled || false;
    chunkStreamingCheckbox.checked = config.chunk_streaming_enabled !== false;

    // LLM settings
    llmStyleSelect.value = config.llm_style || 'off';
    llmTranslateSelect.value = config.llm_translate_to || 'none';
    toggleCustomPromptField();

    llmToneSelect.value = config.llm_tone || 'none';
    llmCustomPrompt.value = config.llm_custom_prompt || '';

    // LLM provider select
    let llmFound = false;
    for (const opt of llmProviderSelect.options) {
      if (opt.dataset.url === config.llm_api_url && opt.dataset.model === config.llm_api_model) {
        opt.selected = true;
        llmFound = true;
        break;
      }
    }
    if (!llmFound && config.llm_api_url) {
      llmProviderSelect.value = 'llm-custom';
      llmApiUrlInput.value = config.llm_api_url || '';
      llmApiModelInput.value = config.llm_api_model || '';
      llmCustomEndpoint.classList.remove('hide');
    } else {
      llmCustomEndpoint.classList.add('hide');
    }

    llmSameKeyCheckbox.checked = config.llm_use_same_key !== false;
    llmLogCheckbox.checked = config.llm_log_enabled !== false;
    toggleLlmKeyField();

    // Advanced section visibility
    applyAdvancedMode();

    updateLlmKeyHint(config.llm_api_url);

    // Populate stats
    const totalWords = config.stats_total_words || 0;
    const totalSpeakingSecs = config.stats_total_speaking_secs || 0;
    const typingTimeMins = totalWords / 40; // 40 WPM average
    const speakingTimeMins = totalSpeakingSecs / 60;
    const timeSavedMins = Math.max(0, typingTimeMins - speakingTimeMins);
    document.getElementById('stat-words').textContent = totalWords.toLocaleString();
    document.getElementById('stat-speaking').textContent = formatDuration(totalSpeakingSecs);
    document.getElementById('stat-saved').textContent = formatTimeSaved(timeSavedMins);

  } catch (err) {
    console.error('get_config:', err);
  }

  // Check accessibility permissions (macOS)
  try {
    const accessible = await invoke('check_accessibility');
    const warning = document.getElementById('accessibility-warning');
    if (!accessible) {
      warning.classList.remove('hide');
    } else {
      warning.classList.add('hide');
    }
  } catch (_) {}

  // Load version and check for updates
  const versionText = document.getElementById('version-text');
  const updateStatus = document.getElementById('update-status');
  try {
    const version = await invoke('get_version');
    versionText.textContent = `Dimmy v${version}`;
  } catch (_) {}

  // Async update check
  updateStatus.textContent = 'checking...';
  updateStatus.className = 'checking';
  (async () => {
    try {
      const newVersion = await invoke('check_for_update');
      if (newVersion) {
        updateStatus.textContent = `Update v${newVersion} available`;
        updateStatus.className = 'available';
        updateStatus.onclick = async () => {
          updateStatus.textContent = 'Installing...';
          updateStatus.className = 'installing';
          updateStatus.onclick = null;
          try {
            await invoke('install_update');
            updateStatus.textContent = 'Restart to apply';
          } catch (e) {
            updateStatus.textContent = `Update failed: ${e}`;
            updateStatus.className = 'error';
            console.error('install_update:', e);
          }
        };
      } else {
        updateStatus.textContent = 'Up to date';
        updateStatus.className = '';
      }
    } catch (e) {
      updateStatus.textContent = 'Update check failed';
      updateStatus.className = 'error';
      console.error('Update check failed:', e);
    }
  })();

  switchView('settings');
}

function closeSettings() {
  switchView('micro');
}

function toggleCustomPromptField() {
  if (llmStyleSelect.value === 'custom') {
    llmCustomPromptField.classList.remove('hide');
  } else {
    llmCustomPromptField.classList.add('hide');
  }
  resizeSettingsWindow();
}

function toggleLlmKeyField() {
  if (llmSameKeyCheckbox.checked) {
    llmKeyField.classList.add('hide');
  } else {
    llmKeyField.classList.remove('hide');
  }
  resizeSettingsWindow();
}

function resizeSettingsWindow() {
  if (currentView === 'settings') {
    requestAnimationFrame(() => {
      const container = document.getElementById('container');
      setWindowSizeWH(W, container.offsetHeight);
    });
  }
}

// LLM settings event listeners
llmStyleSelect.addEventListener('change', () => {
  toggleCustomPromptField();
  resizeSettingsWindow();
});
llmToneSelect.addEventListener('change', resizeSettingsWindow);
llmTranslateSelect.addEventListener('change', resizeSettingsWindow);
llmSameKeyCheckbox.addEventListener('change', toggleLlmKeyField);

llmProviderSelect.addEventListener('change', () => {
  if (llmProviderSelect.value === 'llm-custom') {
    llmCustomEndpoint.classList.remove('hide');
  } else {
    llmCustomEndpoint.classList.add('hide');
  }
  const opt = llmProviderSelect.options[llmProviderSelect.selectedIndex];
  const url = llmProviderSelect.value === 'llm-custom' ? llmApiUrlInput.value : (opt.dataset.url || '');
  updateLlmKeyHint(url);
  resizeSettingsWindow();
});

// ========================
// ADVANCED TOGGLE
// ========================
let advancedOpen = localStorage.getItem('dimmy-advanced') === '1';

function applyAdvancedMode() {
  if (advancedOpen) {
    advancedSection.classList.remove('hide');
    advancedToggle.textContent = 'Less';
  } else {
    advancedSection.classList.add('hide');
    advancedToggle.textContent = 'All settings';
  }
  resizeSettingsWindow();
}

advancedToggle.addEventListener('click', (e) => {
  e.preventDefault();
  advancedOpen = !advancedOpen;
  localStorage.setItem('dimmy-advanced', advancedOpen ? '1' : '0');
  applyAdvancedMode();
});

modelSelect.addEventListener('change', () => {
  if (modelSelect.value === 'custom') {
    customFields.classList.remove('hide');
  } else {
    customFields.classList.add('hide');
  }
  // Update API key hint for the newly selected provider
  const opt = modelSelect.options[modelSelect.selectedIndex];
  const url = modelSelect.value === 'custom' ? apiUrlInput.value : (opt.dataset.url || '');
  updateApiKeyHint(url);
  resizeSettingsWindow();
});

// ========================
// SHORTCUT RECORDING
// ========================
let shortcutRecording = false;
let shortcutPollInterval = null;
let shortcutAutoCancel = null;

shortcutRecordBtn.addEventListener('click', async () => {
  if (shortcutRecording) {
    // Cancel
    await invoke('cancel_shortcut_recording');
    shortcutRecordBtn.textContent = 'Change';
    shortcutRecording = false;
    if (shortcutPollInterval) { clearInterval(shortcutPollInterval); shortcutPollInterval = null; }
    if (shortcutAutoCancel) { clearTimeout(shortcutAutoCancel); shortcutAutoCancel = null; }
    return;
  }

  // Visual feedback BEFORE IPC to avoid race condition
  shortcutRecording = true;
  shortcutRecordBtn.textContent = 'Cancel';
  shortcutLabel.textContent = '2 modifiers (+ optional key)...';

  try {
    await invoke('start_shortcut_recording');
  } catch (err) {
    console.error('start_shortcut_recording failed:', err);
    shortcutLabel.textContent = 'Recording failed';
    shortcutRecordBtn.textContent = 'Change';
    shortcutRecording = false;
    return;
  }

  shortcutPollInterval = setInterval(async () => {
    try {
      const result = await invoke('poll_shortcut_recording');
      if (result.done) {
        clearInterval(shortcutPollInterval);
        shortcutPollInterval = null;
        if (shortcutAutoCancel) { clearTimeout(shortcutAutoCancel); shortcutAutoCancel = null; }
        shortcutRecording = false;
        shortcutLabel.textContent = result.label;
        shortcutRecordBtn.textContent = 'Change';
      }
    } catch (err) {
      console.error('poll_shortcut_recording error:', err);
      shortcutLabel.textContent = 'Error: ' + String(err).substring(0, 30);
    }
  }, 100);

  // Auto-cancel after 10s
  shortcutAutoCancel = setTimeout(() => {
    shortcutAutoCancel = null;
    if (shortcutRecording) {
      shortcutRecordBtn.click();
    }
  }, 10000);
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
  const prompt = promptInput.value;

  if (!apiKey) {
    const provider = getProviderFromUrl(apiUrl);
    const hasKey = providerKeyFlags['has_' + provider + '_key'];
    if (!hasKey) {
      apiKeyInput.focus();
      return;
    }
  }

  const preprocessingEnabled = preprocessingCheckbox.checked;
  const audioDebugEnabled = audioDebugCheckbox.checked;
  const chunkStreamingEnabled = chunkStreamingCheckbox.checked;

  // LLM fields — enabled is derived from style + translate
  const llmStyleVal = llmStyleSelect.value;
  const llmTranslateVal = llmTranslateSelect.value;
  const llmEnabledVal = llmStyleVal !== 'off' || llmTranslateVal !== 'none';
  const llmToneVal = llmToneSelect.value;
  const llmCustomPromptVal = llmCustomPrompt.value;
  const llmUseSameKey = llmSameKeyCheckbox.checked;
  const llmLogEnabled = llmLogCheckbox.checked;
  const llmApiKey = llmApiKeyInput.value.trim() || null;

  let llmApiUrl, llmApiModel;
  if (llmProviderSelect.value === 'llm-custom') {
    llmApiUrl = llmApiUrlInput.value.trim();
    llmApiModel = llmApiModelInput.value.trim();
  } else {
    const opt = llmProviderSelect.options[llmProviderSelect.selectedIndex];
    llmApiUrl = opt.dataset.url;
    llmApiModel = opt.dataset.model;
  }

  try {
    await invoke('set_config', {
      apiKey, apiUrl, apiModel, language, shortcutMode, selectedDevice, prompt,
      preprocessingEnabled: preprocessingEnabled,
      chunkStreamingEnabled: chunkStreamingEnabled,
      audioDebugEnabled: audioDebugEnabled,
      llmEnabled: llmEnabledVal,
      llmStyle: llmStyleVal,
      llmTone: llmToneVal,
      llmCustomPrompt: llmCustomPromptVal,
      llmTranslateTo: llmTranslateVal,
      llmApiUrl: llmApiUrl || null,
      llmApiModel: llmApiModel || null,
      llmUseSameKey: llmUseSameKey,
      llmLogEnabled: llmLogEnabled,
      llmApiKey: llmApiKey,
    });

    // Update local state
    llmEnabled = llmEnabledVal;
    llmStyle = llmStyleVal;
    llmTone = llmToneVal;
    // translate_to is persisted but not tracked locally (no pill indicator needed)
    updateStyleIndicator();

    const name = await invoke('get_audio_device');
    deviceName.textContent = name;
    deviceName.title = name;
    closeSettings();
  } catch (err) {
    console.error('save:', err);
    keyHint.textContent = 'Save failed: ' + err;
  }
});
