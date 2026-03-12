# Negative Space Programming — Refactoring Plan

## Filosofia

Ogni fase **elimina** una classe di bug rendendo gli stati illegali non-rappresentabili nel type system.
Ogni fase compila e i test passano prima di procedere alla successiva.

---

## Fase 0 — Typed Errors (fondamenta)

**Cosa elimina:** errori `String` opachi, impossibili da matchare o gestire selettivamente.

### 0.1 — Crea `src-tauri/src/error.rs`

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DimmyError {
    // Audio
    #[error("audio: {0}")]
    Audio(#[from] AudioError),

    // Transcription
    #[error("transcribe: {0}")]
    Transcribe(#[from] TranscribeError),

    // LLM
    #[error("llm: {0}")]
    Llm(#[from] LlmError),

    // Config
    #[error("config: {0}")]
    Config(String),

    // State
    #[error("invalid state: {0}")]
    InvalidState(String),

    // Platform
    #[error("platform: {0}")]
    Platform(String),
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("no input device available")]
    NoDevice,

    #[error("device '{0}' not found")]
    DeviceNotFound(String),

    #[error("capture failed: {0}")]
    Capture(String),

    #[error("encoding failed: {0}")]
    Encode(String),
}

#[derive(Debug, Error)]
pub enum TranscribeError {
    #[error("no API key configured for {0}")]
    NoApiKey(String),  // provider name

    #[error("HTTP {status}: {body}")]
    Api { status: u16, body: String },

    #[error("empty transcription")]
    Empty,

    #[error("request failed: {0}")]
    Network(String),

    #[error("insecure URL (HTTPS required): {0}")]
    InsecureUrl(String),
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("HTTP {status}: {body}")]
    Api { status: u16, body: String },

    #[error("request failed: {0}")]
    Network(String),

    #[error("no API key for LLM provider {0}")]
    NoApiKey(String),
}

// Tauri richiede Serialize per gli errori dei comandi
impl serde::Serialize for DimmyError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
```

### 0.2 — Aggiungi `thiserror` a Cargo.toml

```toml
thiserror = "2"
```

### 0.3 — Migra un modulo alla volta

Ordine: `audio.rs` → `transcribe.rs` → `llm.rs` → `lib.rs` (comandi Tauri).

Ogni file:
1. Cambia `Result<T, String>` → `Result<T, AudioError>` (o equivalente)
2. Rimuovi tutti i `.map_err(|e| e.to_string())`
3. Usa `?` con le conversioni `From` automatiche di thiserror
4. I comandi Tauri restituiscono `Result<T, DimmyError>` — Tauri serializza via il trait `Serialize`

**Test:** `cargo build` + `cargo test` dopo ogni file migrato.

---

## Fase 1 — Provider Enums (STT + LLM)

**Cosa elimina:** branching runtime su stringhe URL, provider custom non validati, errori di typo nei nomi provider.

### 1.1 — Crea `src-tauri/src/provider.rs`

```rust
/// STT provider — determina formato richiesta/risposta
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SttProvider {
    Groq,
    OpenAI,
    Deepgram,
    Gemini,
    Custom,
}

/// LLM provider — determina formato richiesta/risposta
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    Groq,
    OpenAI,
    OpenRouter,
    Gemini,
    Anthropic,
    Custom,
}

/// URL validato — HTTPS obbligatorio (tranne localhost)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecureUrl(String);

impl SecureUrl {
    pub fn new(url: &str) -> Result<Self, TranscribeError> {
        let u = url.trim();
        if u.starts_with("https://") ||
           u.starts_with("http://localhost") ||
           u.starts_with("http://127.0.0.1") ||
           u.starts_with("http://[::1]") {
            Ok(Self(u.to_string()))
        } else {
            Err(TranscribeError::InsecureUrl(u.to_string()))
        }
    }

    pub fn as_str(&self) -> &str { &self.0 }

    pub fn provider(&self) -> SttProvider {
        if self.0.contains("groq.com") { SttProvider::Groq }
        else if self.0.contains("openai.com") { SttProvider::OpenAI }
        else if self.0.contains("deepgram.com") { SttProvider::Deepgram }
        else if self.0.contains("googleapis.com") { SttProvider::Gemini }
        else { SttProvider::Custom }
    }

    pub fn llm_provider(&self) -> LlmProvider {
        if self.0.contains("groq.com") { LlmProvider::Groq }
        else if self.0.contains("openai.com") { LlmProvider::OpenAI }
        else if self.0.contains("openrouter.ai") { LlmProvider::OpenRouter }
        else if self.0.contains("googleapis.com") { LlmProvider::Gemini }
        else if self.0.contains("anthropic.com") { LlmProvider::Anthropic }
        else { LlmProvider::Custom }
    }
}
```

### 1.2 — Refactora `transcribe.rs` con trait dispatch

```rust
pub async fn transcribe_audio(
    url: &SecureUrl,
    model: &str,
    api_key: &str,
    wav_data: &[u8],
    language: &str,
    prompt: &str,
) -> Result<String, TranscribeError> {
    match url.provider() {
        SttProvider::Deepgram => transcribe_deepgram(url, api_key, wav_data, language).await,
        SttProvider::Gemini   => transcribe_gemini(url, api_key, wav_data, language, prompt).await,
        _                     => transcribe_openai_compat(url, model, api_key, wav_data, language, prompt).await,
    }
}
```

Ogni funzione privata gestisce il proprio formato — niente if/else nidificati.

### 1.3 — Refactora `llm.rs` con match su LlmProvider

```rust
pub async fn process_text(
    url: &SecureUrl,
    model: &str,
    api_key: &str,
    system_prompt: &str,
    text: &str,
) -> Result<String, LlmError> {
    let max_tokens = std::cmp::max(512, (text.len() as f64 * 0.75 * 3.0).ceil() as u32);
    match url.llm_provider() {
        LlmProvider::Anthropic => call_anthropic(url, model, api_key, system_prompt, text, max_tokens).await,
        _                      => call_openai_compat(url, model, api_key, system_prompt, text, max_tokens).await,
    }
}
```

### 1.4 — Aggiorna `lib.rs`

- `api_url: Mutex<String>` → `api_url: Mutex<SecureUrl>`
- `llm_api_url: Mutex<String>` → `llm_api_url: Mutex<SecureUrl>`
- `url_to_provider()` → rimossa (il metodo è su `SecureUrl`)
- Il keyring usa `url.provider()` / `url.llm_provider()` per la chiave

**Test:** `cargo build` + `cargo test`. Il check HTTPS è ora nel costruttore di `SecureUrl` — impossibile passare HTTP.

---

## Fase 2 — LLM Style & Tone Enums

**Cosa elimina:** stringhe style/tone non valide, match su stringhe nei prompt, cicli che saltano indici.

### 2.1 — In `llm.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmStyle {
    Off, Correct, Summarize, Elaborate, Comprehensible,
    Professional, Prompt, GenZ, Boomer, Emoji,
    Acronyms, Imbruttito, Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmTone {
    None, Formal, Friendly, Concise, Academic,
}

impl LlmStyle {
    pub const ALL: &[Self] = &[
        Self::Off, Self::Correct, Self::Summarize, Self::Elaborate,
        Self::Comprehensible, Self::Professional, Self::Prompt,
        Self::GenZ, Self::Boomer, Self::Emoji, Self::Acronyms,
        Self::Imbruttito, Self::Custom,
    ];

    pub fn cycle(self, direction: i32) -> Self {
        let i = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        let len = Self::ALL.len() as i32;
        Self::ALL[((i as i32 + direction).rem_euclid(len)) as usize]
    }

    /// Istruzione per il system prompt — None = nessun processing
    pub fn instruction(&self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Correct => Some("Fix grammar, spelling, and remove filler words..."),
            Self::Summarize => Some("Condense into key points..."),
            // ... ogni variante ha il suo testo, match exhaustive
            Self::Custom => None, // usa custom_prompt
        }
    }
}
```

### 2.2 — Aggiorna `lib.rs`

- `llm_style: Mutex<String>` → `llm_style: Mutex<LlmStyle>`
- `llm_tone: Mutex<String>` → `llm_tone: Mutex<LlmTone>`
- `cycle_llm_style()` usa `style.cycle(direction)` — niente vettore di stringhe
- `build_system_prompt()` usa `style.instruction()` — match exhaustive, aggiungi stile → errore compilazione se non gestisci

**Test:** `cargo build` + `cargo test`. Aggiungere un nuovo LlmStyle senza instruction() → **errore di compilazione**.

---

## Fase 3 — App Phase State Machine

**Cosa elimina:** stati inconsistenti (recording=true senza audio_stream, transcribing senza buffer, ecc.), lock ordering bugs.

### 3.1 — Definisci le fasi

```rust
/// Le uniche fasi possibili dell'applicazione.
/// Ogni fase possiede i dati che le servono — impossibile accedere
/// a dati di un'altra fase.
pub enum AppPhase {
    Idle,
    Recording {
        start: Instant,
        buffer: Arc<Mutex<Vec<f32>>>,
        sample_rate: u32,
        streaming_active: Arc<AtomicBool>,
        chunk_index: u32,
    },
    Transcribing {
        wav_data: Vec<u8>,
        start: Instant,
    },
    Processing {
        raw_text: String,
        start: Instant,
    },
}
```

### 3.2 — Riduci `AppState`

```rust
pub struct AppState {
    // === Fase corrente — un solo Mutex per lo stato mutevole ===
    phase: Mutex<AppPhase>,

    // === Config — cambia solo in settings, mai durante recording ===
    config: Mutex<AppConfig>,

    // === Infrastruttura — vive per tutta la sessione ===
    audio_tx: Sender<AudioCommand>,
    window_anchor: Mutex<Option<(f64, f64)>>,
}
```

Da ~30 Mutex a ~3. La `AppConfig` struct raccoglie tutto ciò che oggi è sparso:

```rust
pub struct AppConfig {
    pub stt: SttConfig,
    pub llm: LlmConfig,
    pub audio: AudioConfig,
    pub shortcut: ShortcutConfig,
    pub ui: UiConfig,
    pub stats: Stats,
}

pub struct SttConfig {
    pub url: SecureUrl,
    pub model: String,
    pub language: String,
    pub prompt: String,
    pub chunk_streaming: bool,
}

pub struct LlmConfig {
    pub style: LlmStyle,
    pub tone: LlmTone,
    pub translate_to: Option<String>,   // None = niente traduzione
    pub custom_prompt: String,
    pub url: SecureUrl,
    pub model: String,
    pub use_same_key: bool,
    pub log_enabled: bool,
}

pub struct AudioConfig {
    pub selected_device: Option<String>,
    pub sample_rate: u32,
    pub preprocessing: bool,
    pub debug_enabled: bool,
    pub debug_session_dir: Option<PathBuf>,
}

pub struct ShortcutConfig {
    pub mode: ShortcutMode,  // enum Toggle | Hold
    pub combo: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShortcutMode { Toggle, Hold }

pub struct Stats {
    pub total_words: u64,
    pub total_speaking_secs: f64,
}
```

### 3.3 — Transizioni di fase con validazione

```rust
impl AppPhase {
    /// Transizione Idle → Recording. Fallisce se non siamo in Idle.
    pub fn start_recording(
        &mut self,
        buffer: Arc<Mutex<Vec<f32>>>,
        sample_rate: u32,
        streaming: Arc<AtomicBool>,
    ) -> Result<(), DimmyError> {
        match self {
            AppPhase::Idle => {
                *self = AppPhase::Recording {
                    start: Instant::now(),
                    buffer,
                    sample_rate,
                    streaming_active: streaming,
                    chunk_index: 0,
                };
                Ok(())
            }
            _ => Err(DimmyError::InvalidState("already recording".into())),
        }
    }

    /// Transizione Recording → Transcribing. Ritorna i dati audio.
    pub fn stop_recording(&mut self) -> Result<(Vec<u8>, Instant), DimmyError> {
        match std::mem::replace(self, AppPhase::Idle) {
            AppPhase::Recording { buffer, sample_rate, start, streaming_active, .. } => {
                streaming_active.store(false, Ordering::Relaxed);
                let samples = buffer.lock().map_err(|e| DimmyError::InvalidState(e.to_string()))?;
                let wav = audio::encode_wav(&samples, sample_rate)?;
                *self = AppPhase::Transcribing { wav_data: wav, start };
                Ok((/* wav_data per transcribe */, start))
            }
            other => {
                *self = other; // rimetti lo stato
                Err(DimmyError::InvalidState("not recording".into()))
            }
        }
    }
}
```

### 3.4 — Migra i comandi Tauri

Ogni comando:
1. Locka `phase`
2. Chiama il metodo di transizione
3. Se serve config, locka `config` (ordine fisso: phase → config, mai invertito → niente deadlock)

Esempio `start_recording`:
```rust
#[tauri::command]
async fn start_recording(state: State<'_, AppState>, app: AppHandle) -> Result<(), DimmyError> {
    let config = state.config.lock()?.clone();
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let streaming = Arc::new(AtomicBool::new(config.stt.chunk_streaming));

    // Avvia cattura audio
    state.audio_tx.send(AudioCommand::Start {
        device: config.audio.selected_device.clone(),
        buffer: buffer.clone(),
    })?;

    // Transizione di fase
    state.phase.lock()?.start_recording(buffer, config.audio.sample_rate, streaming.clone())?;

    // Spawna chunk streaming task se abilitato
    if config.stt.chunk_streaming {
        spawn_chunk_streaming(app, state.inner().clone(), streaming);
    }

    Ok(())
}
```

**Test:** `cargo build` + `cargo test`. Chiamare `stop_recording` senza `start_recording` → `Err(InvalidState)` garantito dal type system.

---

## Fase 4 — Keyring Newtype

**Cosa elimina:** API key vuote/dimenticate, confusione tra STT key e LLM key.

### 4.1 — Newtype per le credenziali

```rust
/// API key validata (non vuota, non whitespace-only)
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: &str) -> Option<Self> {
        let k = key.trim();
        if k.is_empty() { None } else { Some(Self(k.to_string())) }
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

/// Prefisso keyring per tipo di chiave
pub enum KeyringScope {
    Stt(SttProvider),
    Llm(LlmProvider),
}

impl KeyringScope {
    pub fn entry_name(&self) -> String {
        match self {
            Self::Stt(p) => format!("api-key-{}", p.as_str()),
            Self::Llm(p) => format!("llm-key-{}", p.as_str()),
        }
    }
}
```

### 4.2 — Funzioni keyring tipizzate

```rust
fn store_key(scope: KeyringScope, key: &ApiKey) -> Result<(), DimmyError> { ... }
fn load_key(scope: KeyringScope) -> Result<Option<ApiKey>, DimmyError> { ... }
fn has_key(scope: KeyringScope) -> bool { ... }
```

Niente più `format!("api-key-{}", url_to_provider(url))` sparsi in lib.rs.

**Test:** `cargo build` + `cargo test`. Passare stringa vuota come API key → `None` a compile-time-ish (costruttore).

---

## Fase 5 — Audio Pipeline Tipizzato

**Cosa elimina:** buffer raw passati senza sample rate, preprocessing opzionale gestito con if.

### 5.1 — Tipi per audio

```rust
/// Audio raw dalla cattura (sample rate originale, mono)
pub struct RawAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Audio preprocessato (VAD, AGC) al sample rate originale
pub struct ProcessedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Audio pronto per STT (16kHz, WAV-encoded)
pub struct WavPayload {
    pub data: Vec<u8>,
    pub duration_secs: f32,
}

impl RawAudio {
    pub fn preprocess(self, enabled: bool) -> ProcessedAudio { ... }
}

impl ProcessedAudio {
    pub fn to_wav(self) -> Result<WavPayload, AudioError> {
        let resampled = downsample_to_16k(&self.samples, self.sample_rate);
        let duration = resampled.len() as f32 / 16000.0;
        let data = encode_wav(&resampled, 16000)?;
        Ok(WavPayload { data, duration_secs: duration })
    }
}
```

### 5.2 — Pipeline diventa una catena tipizzata

```rust
// Prima (lib.rs, ~40 righe di logica sparsa):
let processed = if preprocessing { preprocess(&buf, sr) } else { buf };
let resampled = downsample_to_16k(&processed, sr);
let wav = encode_wav(&resampled, 16000)?;

// Dopo (una riga, tipi garantiscono l'ordine):
let wav = RawAudio { samples: buf, sample_rate: sr }
    .preprocess(config.audio.preprocessing)
    .to_wav()?;
```

Impossibile passare audio non-resampled a `transcribe_audio()` perche' accetta solo `WavPayload`.

**Test:** `cargo build` + `cargo test`.

---

## Ordine di esecuzione

| Fase | File toccati | Righe stimate | Rischio |
|------|-------------|---------------|---------|
| 0 — Typed Errors | nuovo `error.rs`, poi tutti | ~150 nuove, ~80 rimosse | Basso — meccanico |
| 1 — Provider Enums | nuovo `provider.rs`, `transcribe.rs`, `llm.rs`, `lib.rs` | ~120 nuove, ~60 rimosse | Medio — tocca API calls |
| 2 — Style/Tone Enums | `llm.rs`, `lib.rs` | ~80 nuove, ~50 rimosse | Basso — locale |
| 3 — Phase State Machine | `lib.rs` (grosso) | ~200 nuove, ~300 rimosse | **Alto** — cuore dell'app |
| 4 — Keyring Newtype | `lib.rs` | ~60 nuove, ~40 rimosse | Basso — meccanico |
| 5 — Audio Pipeline | `audio.rs`, `preprocess.rs`, `lib.rs` | ~80 nuove, ~60 rimosse | Medio — tocca audio flow |

**Risultato netto stimato: ~350 righe in MENO** rispetto a oggi. Il codice si accorcia perche' spariscono i check difensivi e i match su stringhe.

## Regole

1. **Un commit per sotto-fase** (0.1, 0.2, 0.3, 1.1, ...) — ogni commit compila
2. **Non toccare hotkey.rs** — troppo platform-specific, il negative space non aggiunge valore li'
3. **Non toccare il frontend** — le firme dei comandi Tauri restano compatibili (stessi nomi, stessi parametri JSON)
4. **Backward compat config** — il `Deserialize` degli enum ha `#[serde(rename_all = "lowercase")]` che matcha le stringhe esistenti nel config.json
5. **Test esistenti devono passare** dopo ogni sotto-fase
