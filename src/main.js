const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
import { t, setLocale, detectLocale, applyTranslations } from './i18n.js';

// Container has 2px margin on each side for clean anti-aliased rounded corners
const MARGIN = 4; // 2px margin * 2 sides
const W = 360 + MARGIN;
const MICRO_W = 56 + MARGIN;
const PILL_H = 32 + MARGIN;
const REC_H = 64 + MARGIN;
const SETTINGS_W = 640 + MARGIN;
const SETTINGS_H = 700 + MARGIN;
const ONBOARDING_W = 348 + MARGIN;
const ONBOARDING_H = 480 + MARGIN;
const BAR_COUNT = 39;
const BAR_W = 7;
const BAR_GAP = 2;

// LLM style -> dot color map
const STYLE_COLORS = {
  off: '#41B0B1',      // teal — LLM off
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
const recBtn = document.getElementById('rec-btn');
const recIconPlay = document.getElementById('rec-icon-play');
const recIconStop = document.getElementById('rec-icon-stop');
const waveformInline = document.getElementById('waveform-inline');
const waveformInlineCtx = waveformInline.getContext('2d');
const compactModeCheckbox = document.getElementById('compact-mode-enabled');
const useKeyringCheckbox = document.getElementById('use-keyring');

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

// Drag the pill via Tauri's startDragging API (works reliably on all platforms
// including macOS where -webkit-app-region: drag can be flaky with Tauri 2)
pill.addEventListener('mousedown', async (e) => {
  // Only drag on left click, skip if clicking a button/input
  if (e.button !== 0) return;
  if (e.target.closest('button, input, select, textarea, a')) return;
  try {
    const win = window.__TAURI__.window.getCurrentWindow();
    await win.startDragging();
  } catch (_) {}
});

// Play/Stop button toggles recording
recBtn.addEventListener('click', async (e) => {
  e.stopPropagation();
  if (isRecording) await stopRecording(); else await startRecording();
});

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
let compactMode = false;

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

  // Pill-shaped for micro/pill, rectangular for rec/settings
  container.classList.toggle('expanded-mode', view === 'rec' || view === 'settings');

  if (view === 'micro') {
    pill.classList.add('micro');
    deviceName.classList.add('hide');
    timerEl.classList.add('hide');
    statusText.classList.add('hide');
    chunkDots.innerHTML = '';
    recBtn.classList.add('hide');
    setWindowSizeWH(MICRO_W, PILL_H);
  } else if (view === 'pill') {
    pill.classList.remove('micro');
    recBtn.classList.remove('hide');
    if (!isRecording) deviceName.classList.remove('hide');
    setWindowSizeWH(W, PILL_H);
  } else if (view === 'rec') {
    pill.classList.remove('micro');
    recBtn.classList.remove('hide');
    recPanel.classList.add('open');
    setWindowSizeWH(W, REC_H);
  } else if (view === 'settings') {
    pill.classList.remove('micro');
    settingsPanel.classList.add('open');
    setWindowSizeWH(SETTINGS_W, SETTINGS_H);
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
  compactMode = localStorage.getItem('dimmy-compact') === 'true';

  // Load locale — from config language if available, else system language
  try {
    const config = await invoke('get_config');
    const locale = detectLocale(config.language);
    await setLocale(locale);
  } catch (_) {
    await setLocale(detectLocale());
  }

  // Check if first launch — show onboarding (locale already loaded above)
  try {
    const needsOb = await invoke('needs_onboarding');
    if (needsOb) {
      await showOnboarding();
      return; // onboarding calls init() again when done
    }
  } catch (_) {}

  try {
    const name = await invoke('get_audio_device');
    deviceName.textContent = name;
    deviceName.title = name;
  } catch (_) {
    deviceName.textContent = t('pill.no_mic');
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
let isHovering = false;
const container = document.getElementById('container');

container.addEventListener('mouseenter', () => {
  isHovering = true;
  if (hoverTimeout) { clearTimeout(hoverTimeout); hoverTimeout = null; }
  if (currentView === 'micro' && !isRecording) {
    switchView('pill');
  }
});

container.addEventListener('mouseleave', () => {
  isHovering = false;
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
  if (currentView === 'pill' && !isRecording && !isHovering && !hoverTimeout && !shrinkTimeout) {
    switchView('micro');
  }
}, 5000);

// ========================
// SCROLL-WHEEL CYCLING on container (LLM style/tone)
// ========================
container.addEventListener('wheel', async (e) => {
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
    // Flash status for 3s
    if (styleFlashTimeout) clearTimeout(styleFlashTimeout);
    styleFlashTimeout = setTimeout(() => {
      hideStatus();
      styleFlashTimeout = null;
    }, 3000);
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
    showStatus(t('pill.err'));
  } else if (status === 'final') {
    addChunkDot(0, 'final');
    showStatus(t('pill.final'));
  }
});

listen('transcription-final', (event) => {
  transcriptText.textContent = event.payload.text;
  transcriptText.scrollLeft = transcriptText.scrollWidth;
  showStatus(t('pill.done'));
});

listen('final-chunk-progress', (event) => {
  const { current, total } = event.payload;
  if (total > 1) {
    transcriptText.textContent = t('pill.processing', { current, total });
    transcriptText.scrollLeft = transcriptText.scrollWidth;
  }
});

// ========================
// LLM STATUS EVENTS
// ========================
listen('llm-status', (event) => {
  const { status, error } = event.payload;
  if (status === 'processing') {
    dot.className = 'llm-processing';
    showStatus(t('pill.enhancing'));
  } else if (status === 'done') {
    dot.className = '';
    updateStyleIndicator();
  } else if (status === 'error') {
    console.error('LLM error:', error);
    // Brief error indication, then back to normal
    showStatus(t('pill.llm_err'));
    setTimeout(() => { dot.className = ''; updateStyleIndicator(); }, 2000);
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
  recBtn.classList.add('recording');
  recIconPlay.classList.add('hide');
  recIconStop.classList.remove('hide');
  dot.className = 'recording';
  container.classList.add('recording-active');
  showTimer();
  if (!compactMode) showStatus(t('pill.rec'));

  chunkTexts = [];
  energyHistory = [];
  peakAmplitude = 0.0001;
  transcriptText.textContent = '';
  transcriptText.setAttribute('data-placeholder', t('pill.listening'));
  chunkDots.innerHTML = '';

  if (compactMode) {
    // Compact: stay at pill height, show inline waveform
    waveformInline.classList.remove('hide');
    switchView('pill');
  } else {
    switchView('rec');
  }

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
        if (compactMode) {
          setupInlineCanvas();
        } else {
          setupCanvas();
        }
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
  container.classList.remove('recording-active');
  waveformInline.classList.add('hide');
  showStatus(t('pill.transcribing'));

  try {
    let text = await invoke('stop_recording');
    isRecording = false;
    transcriptText.textContent = text;
    transcriptText.scrollLeft = transcriptText.scrollWidth;

    // LLM post-processing if enabled (style or translate active)
    if (llmEnabled) {
      dot.className = 'llm-processing';
      showStatus(t('pill.enhancing'));
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
    showStatus(t('pill.pasting'));

    try { await invoke('paste_text', { text }); } catch (_) {}

    // Accumulate KPI stats
    const wordCount = text.trim().split(/\s+/).filter(w => w.length > 0).length;
    if (wordCount > 0) {
      try { await invoke('update_stats', { words: wordCount, speakingSecs }); } catch (_) {}
    }

    showStatus(t('pill.done'));
    setTimeout(() => {
      recBtn.classList.remove('recording');
      recIconPlay.classList.remove('hide');
      recIconStop.classList.add('hide');
      switchView('pill');
      hideTimer();
      hideStatus();
      settingsBtn.disabled = false;
      shrinkToMicro(5000);
    }, 2000);
  } catch (err) {
    isRecording = false;
    container.classList.remove('recording-active');
    waveformInline.classList.add('hide');
    dot.className = 'error';
    showStatus(String(err).substring(0, 30));
    setTimeout(() => {
      recBtn.classList.remove('recording');
      recIconPlay.classList.remove('hide');
      recIconStop.classList.add('hide');
      switchView('pill');
      hideTimer();
      dot.className = '';
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
  if (totalSecs < 60) return Math.round(totalSecs) + t('stats.seconds');
  if (totalSecs < 3600) return (totalSecs / 60).toFixed(1) + ' ' + t('stats.minutes');
  return (totalSecs / 3600).toFixed(1) + ' ' + t('stats.hours');
}

function formatTimeSaved(mins) {
  if (mins < 1) return Math.round(mins * 60) + t('stats.seconds');
  if (mins < 60) return mins.toFixed(1) + ' ' + t('stats.minutes');
  return (mins / 60).toFixed(1) + ' ' + t('stats.hours');
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

function setupInlineCanvas() {
  const rect = waveformInline.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  waveformInline.width = rect.width * dpr;
  waveformInline.height = rect.height * dpr;
  waveformInlineCtx.scale(dpr, dpr);
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
    // Keep enough history for whichever canvas is active
    const maxBars = compactMode ? 200 : BAR_COUNT;
    if (energyHistory.length > maxBars) {
      energyHistory = energyHistory.slice(energyHistory.length - maxBars);
    }
    drawBars();
  } catch (_) {}
  waveformPending = false;
}

function drawBars() {
  const canvas = compactMode ? waveformInline : waveformCanvas;
  const ctx = compactMode ? waveformInlineCtx : waveformCtx;
  const rect = canvas.getBoundingClientRect();
  const w = rect.width;
  const h = rect.height;

  ctx.clearRect(0, 0, w, h);

  // Compact mode: adapt bar sizing to available width
  const barW = compactMode ? 3 : BAR_W;
  const barGap = compactMode ? 1.5 : BAR_GAP;
  const barCount = compactMode ? Math.floor((w + barGap) / (barW + barGap)) : BAR_COUNT;

  // Trim history to bar count
  const history = energyHistory.slice(-barCount);

  const totalW = barCount * (barW + barGap) - barGap;
  // Compact: align bars to the right so new bars appear on the right edge
  // Normal: center bars in the canvas
  const offsetX = compactMode
    ? w - totalW + (barCount - history.length) * (barW + barGap)
    : (w - totalW) / 2;

  for (let i = 0; i < history.length; i++) {
    const val = history[i];
    const barH = Math.max(2, Math.floor(val * h));
    const x = offsetX + i * (barW + barGap);

    if (val >= 0.7) {
      ctx.fillStyle = '#5198C9';
    } else if (val > 0.05) {
      ctx.fillStyle = '#41B0B1';
    } else {
      ctx.fillStyle = '#1a4a4b';
    }

    ctx.beginPath();
    ctx.roundRect(x, h - barH, barW, barH, 1);
    ctx.fill();
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
  if (url.includes('deepgram.com')) return 'deepgram';
  if (url.includes('googleapis.com')) return 'gemini';
  if (url.includes('anthropic.com')) return 'anthropic';
  return 'custom';
}

function providerLabel(provider) {
  return { groq: 'Groq', openai: 'OpenAI', deepgram: 'Deepgram', openrouter: 'OpenRouter', gemini: 'Gemini', anthropic: 'Anthropic', custom: 'Custom' }[provider] || provider;
}

function updateApiKeyHint(url) {
  const provider = getProviderFromUrl(url);
  const hasKey = providerKeyFlags['has_' + provider + '_key'];
  const name = providerLabel(provider);
  apiKeyInput.value = '';
  apiKeyInput.placeholder = hasKey ? t('key_hint.saved_placeholder', { provider: name }) : 'sk-... or gsk_...';
  keyHint.textContent = hasKey ? t('key_hint.saved', { provider: name }) : '';
}

function updateLlmKeyHint(url) {
  const provider = getProviderFromUrl(url);
  const hasKey = providerKeyFlags['has_llm_' + provider + '_key'];
  const name = providerLabel(provider);
  llmApiKeyInput.value = '';
  llmApiKeyInput.placeholder = hasKey ? t('key_hint.saved_placeholder', { provider: name }) : t('key_hint.llm_placeholder');
  if (llmKeyHint) {
    llmKeyHint.textContent = hasKey ? t('key_hint.saved', { provider: name }) : '';
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
      has_deepgram_key: config.has_deepgram_key,
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
    defaultOpt.textContent = t('audio.system_default');
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
    shortcutLabel.textContent = config.shortcut_label || t('activation.press_to_set');
    shortcutRecordBtn.textContent = t('activation.change');

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

    // Compact mode
    compactModeCheckbox.checked = compactMode;

    // Key storage
    useKeyringCheckbox.checked = config.use_keyring || false;

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
    versionText.textContent = t('app.version', { version });
  } catch (_) {}

  // Async update check
  updateStatus.textContent = t('update.checking');
  updateStatus.className = 'checking';
  (async () => {
    try {
      const newVersion = await invoke('check_for_update');
      if (newVersion) {
        updateStatus.textContent = t('update.available', { version: newVersion });
        updateStatus.className = 'available';
        updateStatus.onclick = async () => {
          updateStatus.textContent = t('update.installing');
          updateStatus.className = 'installing';
          updateStatus.onclick = null;
          try {
            await invoke('install_update');
            updateStatus.textContent = t('update.restart');
          } catch (e) {
            updateStatus.textContent = t('update.failed');
            updateStatus.className = 'error';
            console.error('install_update:', e);
          }
        };
      } else {
        updateStatus.textContent = t('update.up_to_date');
        updateStatus.className = '';
      }
    } catch (e) {
      updateStatus.textContent = t('update.check_failed');
      updateStatus.className = 'error';
      console.error('Update check failed:', e);
    }
  })();

  // Reset to first tab
  document.querySelectorAll('#settings-nav .nav-item').forEach(b => b.classList.remove('active'));
  document.querySelectorAll('.settings-page').forEach(p => p.classList.add('hide'));
  const firstBtn = document.querySelector('#settings-nav .nav-item');
  if (firstBtn) firstBtn.classList.add('active');
  const firstPage = document.querySelector('.settings-page');
  if (firstPage) firstPage.classList.remove('hide');

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
  // Fixed height — content scrolls within the panel
  if (currentView === 'settings') {
    setWindowSizeWH(SETTINGS_W, SETTINGS_H);
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
// SETTINGS NAV — section switching
// ========================
document.querySelectorAll('#settings-nav .nav-item').forEach(btn => {
  btn.addEventListener('click', () => {
    document.querySelectorAll('#settings-nav .nav-item').forEach(b => b.classList.remove('active'));
    document.querySelectorAll('.settings-page').forEach(p => p.classList.add('hide'));
    btn.classList.add('active');
    const page = document.querySelector(`.settings-page[data-page="${btn.dataset.section}"]`);
    if (page) page.classList.remove('hide');
    document.getElementById('settings-pages').scrollTop = 0;
  });
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
    shortcutRecordBtn.textContent = t('activation.change');
    shortcutRecording = false;
    if (shortcutPollInterval) { clearInterval(shortcutPollInterval); shortcutPollInterval = null; }
    if (shortcutAutoCancel) { clearTimeout(shortcutAutoCancel); shortcutAutoCancel = null; }
    return;
  }

  // Visual feedback BEFORE IPC to avoid race condition
  shortcutRecording = true;
  shortcutRecordBtn.textContent = t('settings.cancel');
  shortcutLabel.textContent = t('activation.recording_hint');

  try {
    await invoke('start_shortcut_recording');
  } catch (err) {
    console.error('start_shortcut_recording failed:', err);
    shortcutLabel.textContent = t('activation.recording_failed');
    shortcutRecordBtn.textContent = t('activation.change');
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
        shortcutRecordBtn.textContent = t('activation.change');
      }
    } catch (err) {
      console.error('poll_shortcut_recording error:', err);
      shortcutLabel.textContent = t('error.generic', { error: String(err).substring(0, 30) });
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

document.getElementById('quit-btn').addEventListener('click', () => {
  window.__TAURI__.window.getCurrentWindow().close();
});

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
      useKeyring: useKeyringCheckbox.checked,
    });

    // Update local state
    compactMode = compactModeCheckbox.checked;
    localStorage.setItem('dimmy-compact', compactMode);
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
    keyHint.textContent = t('error.save_failed', { error: err });
  }
});

// ========================
// ONBOARDING (first launch only)
// ========================
const providerLinks = {
  groq:     { url: 'https://console.groq.com/keys',       name: 'Groq',    placeholder: 'gsk_...',  free: true },
  deepgram: { url: 'https://console.deepgram.com/',        name: 'Deepgram', placeholder: 'dg_...',  free: true },
  openai:   { url: 'https://platform.openai.com/api-keys', name: 'OpenAI',  placeholder: 'sk-...',  free: false },
  gemini:   { url: 'https://aistudio.google.com/apikey',   name: 'Gemini',  placeholder: 'AIza...', free: true },
};

async function showOnboarding() {
  const overlay = document.getElementById('onboarding-overlay');
  if (!overlay) return;

  // Show overlay and center window on screen
  overlay.classList.remove('hide');
  container.classList.add('expanded-mode');
  pill.classList.add('hide');
  await invoke('center_window', { w: ONBOARDING_W, h: ONBOARDING_H });

  // Allow dragging the onboarding window via the progress bar area
  const progressBar = overlay.querySelector('.ob-progress');
  progressBar.style.cursor = 'grab';
  progressBar.addEventListener('mousedown', async (e) => {
    if (e.button !== 0) return;
    try {
      const win = window.__TAURI__.window.getCurrentWindow();
      await win.startDragging();
    } catch (_) {}
  });

  const track = overlay.querySelector('.ob-track');
  const segments = overlay.querySelectorAll('.ob-seg');
  const counter = overlay.querySelector('.ob-counter');
  const backBtn = overlay.querySelector('.ob-back');
  const nextBtn = overlay.querySelector('.ob-next');
  const providerSelect = overlay.querySelector('#ob-provider');
  const apiKeyInput = overlay.querySelector('#ob-api-key');
  const providerHint = overlay.querySelector('#ob-provider-hint');

  let current = 0;
  const total = segments.length;

  function goTo(i) {
    current = i;
    track.style.transform = `translateX(-${current * 100}%)`;
    segments.forEach((seg, idx) => {
      seg.classList.remove('active', 'done');
      if (idx < current) seg.classList.add('done');
      if (idx === current) seg.classList.add('active');
    });
    counter.textContent = `${current + 1} / ${total}`;
    backBtn.classList.toggle('ob-invisible', current === 0);
    if (current === total - 1) nextBtn.textContent = t('onboarding.start');
    else if (current === total - 2) nextBtn.textContent = t('onboarding.done');
    else nextBtn.textContent = t('onboarding.continue');
  }

  function updateProviderHint() {
    const info = providerLinks[providerSelect.value];
    if (!info) return;
    const freeText = info.free ? ' ' + t('onboarding.api_key_hint_free') : '';
    providerHint.innerHTML =
      `${t('onboarding.api_key_hint_prefix')} <a href="${info.url}" target="_blank">${t('onboarding.api_key_hint_link', { provider: info.name })}</a>${freeText}`;
    apiKeyInput.placeholder = info.placeholder;
  }

  providerSelect.addEventListener('change', updateProviderHint);
  updateProviderHint();

  // Option card selection
  overlay.querySelectorAll('.ob-option-card').forEach(card => {
    card.addEventListener('click', () => {
      card.parentElement.querySelectorAll('.ob-option-card').forEach(c => c.classList.remove('selected'));
      card.classList.add('selected');
    });
  });

  // Style chip selection
  overlay.querySelectorAll('.ob-style-chip').forEach(chip => {
    chip.addEventListener('click', () => {
      chip.parentElement.querySelectorAll('.ob-style-chip').forEach(c => c.classList.remove('selected'));
      chip.classList.add('selected');
    });
  });

  nextBtn.addEventListener('click', async () => {
    if (current < total - 1) {
      goTo(current + 1);
    } else {
      await finishOnboarding();
    }
  });

  backBtn.addEventListener('click', () => {
    if (current > 0) goTo(current - 1);
  });

  async function finishOnboarding() {
    // Gather values
    const lang = overlay.querySelector('#ob-language').value;
    const provider = providerSelect.value;
    const apiKey = apiKeyInput.value.trim();
    const mode = overlay.querySelector('.ob-option-card.selected')?.dataset.mode || 'hold';
    const styleChip = overlay.querySelector('.ob-style-chip.selected');
    const style = styleChip?.dataset.style || 'off';
    const translateTo = overlay.querySelector('#ob-translate').value;

    // Map provider to URL + model
    const providerMap = {
      groq:     { url: 'https://api.groq.com/openai/v1/audio/transcriptions', model: 'whisper-large-v3-turbo' },
      deepgram: { url: 'https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true&punctuate=true&paragraphs=true', model: 'nova-3' },
      openai:   { url: 'https://api.openai.com/v1/audio/transcriptions', model: 'whisper-1' },
      gemini:   { url: 'https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent', model: 'gemini-2.5-flash' },
    };
    const prov = providerMap[provider] || providerMap.groq;

    try {
      await invoke('set_config', {
        apiKey: apiKey || null,
        apiUrl: prov.url,
        apiModel: prov.model,
        language: lang,
        shortcutMode: mode,
        shortcut: null,
        selectedDevice: null,
        prompt: '',
        llmEnabled: style !== 'off' || translateTo !== 'none' ? true : null,
        llmStyle: style !== 'off' ? style : null,
        llmTone: null,
        llmCustomPrompt: null,
        llmTranslateTo: translateTo !== 'none' ? translateTo : null,
        llmApiUrl: null,
        llmApiModel: null,
        llmUseSameKey: true,
        llmApiKey: null,
        llmLogEnabled: null,
        preprocessingEnabled: true,
        chunkStreamingEnabled: null,
        audioDebugEnabled: null,
        useKeyring: null,
      });
      await invoke('complete_onboarding');
    } catch (e) {
      console.error('onboarding save:', e);
    }

    // Hide onboarding, show app
    overlay.classList.add('hide');
    pill.classList.remove('hide');
    init(); // Re-init with saved config
  }

  goTo(0);
}
