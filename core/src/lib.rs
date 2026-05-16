// `serde_json::json!{...}` in get_status grew past the default macro
// recursion limit (128) once Fireworks + Together were added to the
// provider matrix. Bump crate-wide so future provider additions don't
// silently break the build.
#![recursion_limit = "256"]

pub mod aec;
pub mod app_rules;
pub mod audio;
pub mod autostart;
pub mod chunked_stt;
pub mod claude_code;
pub mod dfn;
pub mod error;
pub mod ffi;
pub mod filler;
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
pub mod gpu_diag;
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
pub mod gpu_health;
pub mod history;
mod hotkey;
pub mod keystore;
/// Licensing — local-server PoC for the v2 architecture (see
/// `docs/dev/licensing-poc.md`). Always-available types + file I/O +
/// HTTP client; Ed25519 verify is gated behind `license-client`.
pub mod license;
pub mod llm;
pub mod local_llm;
pub mod local_stt;
/// Long-form meeting recording. Streams to disk so memory stays
/// bounded over multi-hour sessions and a `.recording` marker
/// enables crash recovery. Post-process pipeline (recap, actions,
/// optional translation) runs from `dimmy_meeting_stop` returning
/// the full transcript; the LLM call itself is driven from the C#/
/// Swift host through dimmy_process_with_llm to keep async runtime
/// concerns out of the meeting worker.
pub mod meeting;
/// Notion REST API client — sends meeting recap markdown to a user-
/// picked Notion page or database. Internal integration tokens, no
/// OAuth. Uses Notion's server-side markdown API (2026-02-26+) so we
/// don't ship a markdown→block-tree converter. See `notion.rs`.
pub mod notion;
/// Parakeet TDT v3 FP32 local STT via ONNX Runtime. Inference is
/// gated behind `local-stt-parakeet`; bundle download / presence
/// helpers are always available so the UI can render the "needs
/// download" state without a feature-gate dance.
pub mod parakeet;
/// Apple-Silicon-only Parakeet via FluidInference's CoreML bundle on
/// the Neural Engine. Inference behind `local-stt-parakeet-fluid`;
/// `bundle_present` / `download_bundle` always available. The module
/// itself compiles only on macOS arm64.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod parakeet_fluid;
pub mod preprocess;
pub mod process_loopback;
pub mod provider;
pub mod telemetry;
pub mod transcribe;

use audio::AudioCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub const DEFAULT_API_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
pub const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";
/// Default prompt guides Whisper to produce punctuated, well-formatted output.
/// Whisper mimics the style of this text — punctuation, capitalization, etc.
pub const DEFAULT_PROMPT: &str = "Hello, how are you? Fine, thanks! Today we'll discuss an interesting topic. Ciao, come stai? Bene, grazie! Oggi parliamo di un argomento interessante.";
pub const DEFAULT_LLM_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const DEFAULT_LLM_MODEL: &str = "llama-3.3-70b-versatile";
#[allow(dead_code)] // Used by native UI via FFI recording logic
const MAX_RECORDING_SECS: usize = 30 * 60; // 30 minutes hard cap
const MAX_LOG_BYTES: u64 = 1_048_576; // 1 MB log rotation threshold
/// Tail buffer: keep recording for this long after the user releases the hotkey.
/// Catches trailing audio when the user's finger lifts slightly before finishing
/// the last syllable. Same approach used by Discord (~200ms), TeamSpeak, Mumble.
/// 300ms is generous enough for dictation without feeling laggy.
#[allow(dead_code)] // Used by native UI via FFI stop logic
const STOP_TAIL_MS: u64 = 300;

/// Default shortcut: fn on macOS (simple, doesn't conflict),
/// Win+Alt on Windows/Linux (safe because Win+Alt isn't commonly used).
fn default_audio_source() -> String {
    "mic".to_string()
}

fn default_shortcut() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "fn"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "win+alt"
    }
}

/// Build flavor — "" (prod, default) or "staging". Embedded at compile
/// time via build.rs from the `DIMMY_BUILD_FLAVOR` env var. A staging
/// build surfaces a "STAGING" badge in the UI and uses the staging
/// licensing endpoint, but its config dir is now controlled
/// independently via `DIMMY_CONFIG_NAMESPACE` so a channel-prerelease
/// auto-update doesn't appear to wipe the user's data — see
/// `config_dir_name` below.
pub fn build_flavor() -> &'static str {
    option_env!("DIMMY_BUILD_FLAVOR").unwrap_or("")
}

/// True iff this binary was built with `DIMMY_BUILD_FLAVOR=staging`.
pub fn is_staging_build() -> bool {
    build_flavor() == "staging"
}

/// Compose the final initial-prompt / `prompt` form-field value for an
/// STT call from the user's free-form prompt + their curated user_dict
/// list. Both are user-controlled vocab biasing; we just join them
/// with `, ` so the STT model sees a single context string.
///
/// Behaviour:
///   - empty `prompt` + empty `user_dict` → `""` (caller skips param)
///   - empty `prompt` + non-empty dict → `"foo, bar, baz"`
///   - non-empty `prompt` + non-empty dict → `"<prompt>, foo, bar"`
///   - empty dict words are filtered (defensive — set_config_json
///     already drops them, but a hot-add could race)
///
/// The 224-token Whisper prompt limit is NOT enforced here — the
/// model truncates from the start; we trust the caller-side UI to
/// guide users away from pathological 500-word dicts (settings
/// shows a counter). At ~3 tokens/word avg, 60 words is the sweet
/// spot before truncation eats the user's free-form prompt.
pub fn compose_stt_prompt(prompt: &str, user_dict: &[String]) -> String {
    let dict_joined: String = user_dict
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    match (prompt.trim().is_empty(), dict_joined.is_empty()) {
        (true, true) => String::new(),
        (true, false) => dict_joined,
        (false, true) => prompt.to_string(),
        (false, false) => format!("{}, {}", prompt.trim(), dict_joined),
    }
}

/// Config dir name on disk. Controlled by the `DIMMY_CONFIG_NAMESPACE`
/// build-time env var (default `dimmy`). Decoupled from `build_flavor`
/// since 2026-05-16: a staging-flavored build that ships under the
/// same Velopack packId as prod (the channel-prerelease auto-update
/// case) MUST keep using the prod config dir, otherwise the user's
/// history / license / app-rules appear wiped every time Velopack
/// swaps in a prerelease build. Only the `staging-release.yml` tester
/// pipeline (different packId, truly side-by-side install) sets
/// `DIMMY_CONFIG_NAMESPACE=dimmy-staging` so its install can coexist
/// with prod on the same machine without stomping its files.
pub fn config_dir_name() -> &'static str {
    match option_env!("DIMMY_CONFIG_NAMESPACE") {
        Some(s) if !s.is_empty() => s,
        _ => "dimmy",
    }
}

pub fn config_dir_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|p| p.join(config_dir_name()))
}

/// Marker file path for onboarding completion.
/// Separate from config.json so deleting/resetting config doesn't re-trigger onboarding.
pub fn onboarding_marker_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join(".onboarding_done"))
}

/// Check if onboarding has been completed.
/// Returns true if: marker file exists, OR config.json exists (existing user upgrading).
/// This prevents the onboarding from appearing for existing users who upgrade to a
/// version that introduces onboarding — they already have a working config.
pub fn onboarding_completed() -> bool {
    // If marker exists, done
    if onboarding_marker_path().map(|p| p.exists()).unwrap_or(true) {
        return true;
    }
    // If config.json exists, this is an existing user — auto-mark as done
    if config_path().map(|p| p.exists()).unwrap_or(false) {
        log("Existing config.json found — skipping onboarding for upgrade user");
        let _ = mark_onboarding_done();
        return true;
    }
    false
}

/// Mark onboarding as completed by creating the marker file.
pub fn mark_onboarding_done() -> Result<(), String> {
    let path = onboarding_marker_path().ok_or("Cannot determine config directory")?;
    // Ensure config dir exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, "1").map_err(|e| e.to_string())?;
    assert!(
        path.exists(),
        "Onboarding marker file must exist after write"
    );
    Ok(())
}

pub fn config_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("config.json"))
}

pub fn log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("dimmy.log"))
}

#[allow(dead_code)]
fn transcription_debug_log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("transcription_debug.log"))
}

fn audio_debug_dir() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("audio_debug"))
}

/// Where meeting-mode sessions persist their on-disk artefacts
/// (`audio.wav` + `transcripts.txt` + `meta.json` + post-process
/// outputs). One sub-directory per meeting, named with a v4 UUID.
pub fn meetings_dir() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("meetings"))
}

/// Where opt-in history-audio WAVs live: one file per transcript row
/// named `<id>.wav`. Cleaned by age + size at startup; never written
/// when `save_audio_in_history` is false. Public so the FFI side can
/// reach it from the history-save path.
pub fn history_audio_dir() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("history_audio"))
}

/// Create a session directory for audio debug dumps and return its path.
pub fn create_debug_session_dir() -> Option<std::path::PathBuf> {
    let base = audio_debug_dir()?;
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let session_dir = base.join(&ts);
    std::fs::create_dir_all(&session_dir).ok()?;
    Some(session_dir)
}

/// Save WAV bytes to a file inside a debug session directory (fire-and-forget).
pub fn save_debug_wav(dir: &std::path::Path, filename: &str, wav_data: &[u8]) {
    let path = dir.join(filename);
    let _ = std::fs::write(&path, wav_data);
}

/// Save session metadata JSON to the debug directory.
#[allow(dead_code)]
fn save_debug_metadata(
    dir: &std::path::Path,
    sample_rate: u32,
    device: &Option<String>,
    preprocessing_on: bool,
    duration_secs: f64,
    chunk_count: u32,
) {
    let meta = serde_json::json!({
        "sample_rate": sample_rate,
        "device": device.as_deref().unwrap_or("default"),
        "preprocessing_enabled": preprocessing_on,
        "duration_secs": duration_secs,
        "chunk_count": chunk_count,
        "timestamp": chrono::Local::now().to_rfc3339(),
    });
    let path = dir.join("metadata.json");
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );
}

/// Append a line to the transcription debug log for chunk vs final comparison.
#[allow(dead_code)]
fn debug_transcription(msg: &str) {
    use std::io::Write;
    if let Some(path) = transcription_debug_log_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Rotate at 2 MB
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > 2_097_152 {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    let half = data.len() / 2;
                    let cut = data[half..]
                        .find('\n')
                        .map(|i| half + i + 1)
                        .unwrap_or(half);
                    let _ = std::fs::write(&path, &data[cut..]);
                }
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let _ = writeln!(f, "[{}] {}", ts, msg);
        }
    }
}

/// Write a log line to %APPDATA%/dimmy/dimmy.log (visible on Windows GUI apps)
pub fn log(msg: &str) {
    use std::io::Write;
    // NB: do NOT use eprintln!. On Windows when the host process (Velopack-
    // launched windowed C# app) has no stderr handle, eprintln! routes
    // through std::io::_eprint which `panic!`s on any non-BrokenPipe
    // write error. Hitting that panic from inside dimmy_init aborts the
    // whole DLL via __fastfail (extern "C" no-unwind boundary). Use a
    // best-effort writeln! that swallows errors instead.
    let _ = writeln!(std::io::stderr(), "{}", msg);
    if let Some(path) = log_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Rotate: if log exceeds MAX_LOG_BYTES, keep only the last half
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_LOG_BYTES {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    let half = data.len() / 2;
                    // Find the next newline after the halfway point to avoid splitting a line
                    let cut = data[half..]
                        .find('\n')
                        .map(|i| half + i + 1)
                        .unwrap_or(half);
                    let _ = std::fs::write(&path, &data[cut..]);
                }
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(f, "[{}] {}", ts, msg);
        }
    }
}

/// Non-sensitive config persisted to disk.
pub struct AppConfig {
    pub api_url: String,
    pub api_model: String,
    pub selected_device: Option<String>,
    pub language: String,
    pub shortcut_mode: String,
    pub shortcut: String,
    pub prompt: String,
    /// User-curated vocabulary list. Words / short phrases the user
    /// has flagged as misspelled in past transcripts and explicitly
    /// added — Wispr Flow-style "Add to dictionary" flow. Joined with
    /// the `prompt` field at STT time so both cloud (Whisper /
    /// Groq / OpenAI / Gemini's `prompt` form field) AND local
    /// Whisper.cpp (`initial_prompt` param) see the bias context.
    /// Stored as a flat list because order doesn't matter for STT
    /// biasing and the UI surfaces "Add" / "Remove" — no nesting.
    /// Parakeet local backend does NOT honor this in v1 (its NeMo
    /// TDT decoder has no public boost-words API in the ort wrapper
    /// we use); documented limitation, dict still applies to cloud
    /// and Whisper paths. Missing on-disk → empty Vec (see load
    /// fallback in parse_config).
    pub user_dict: Vec<String>,
    // LLM post-processing fields
    pub llm_enabled: bool,
    pub llm_style: llm::LlmStyle,
    pub llm_tone: llm::LlmTone,
    pub llm_custom_prompt: String,
    pub llm_translate_to: String,
    pub llm_api_url: String,
    pub llm_api_model: String,
    pub llm_use_same_key: bool,
    pub llm_log_enabled: bool,
    /// How the LLM dispatcher authenticates against the configured
    /// provider. Two values today:
    ///   - `"api_key"` (default) — send HTTP requests with the
    ///     provider's saved API key. Classic behaviour.
    ///   - `"subscription"` — route every LLM call through the
    ///     local `claude` CLI (`core/src/claude_code.rs`) using the
    ///     user's Anthropic Pro/Team/Max plan. Bypasses the URL
    ///     entirely; the model id is still honoured via `--model X`.
    ///
    /// The auth method is orthogonal to provider + model: a user
    /// can pick `Anthropic + claude-haiku-4-5 + subscription` to
    /// rewrite via subscription on Haiku, and `Anthropic + claude-
    /// opus-4-7 + subscription` for recap — same auth, different
    /// model, all without an API key. Stored as a string for
    /// forward-compat with future auth schemes (e.g. OAuth on
    /// other providers if they ever ship one).
    pub llm_auth_method: String,
    /// Auth method for the meeting recap call specifically. Separate
    /// knob from `llm_auth_method` so a user can run fast dictation
    /// rewrites via API key (subprocess cold-start would dominate
    /// the latency budget) AND run recap via the Anthropic
    /// subscription where the same overhead is amortized across
    /// 30-90 s of model inference.
    ///
    ///   - `""` (default) — inherit from `llm_auth_method`. Single-
    ///     knob users see exactly the behaviour they configured.
    ///   - `"api_key"` — force API-key HTTP for the recap regardless
    ///     of dictation auth.
    ///   - `"subscription"` — force subprocess CLI for the recap
    ///     regardless of dictation auth.
    pub recap_auth_method: String,
    /// When the recap model's vendor matches a vendor for which we
    /// already have an upstream key (`Llm` or `Stt` scope), should
    /// the recap reuse that key (true, default) or load a separate
    /// `Recap(vendor)` key from the keystore (false)?
    ///
    /// UI mirror of the existing `llm_use_same_key` toggle one layer
    /// up. Both toggles share the same shape: "Use my saved <Vendor>
    /// key" → ON pulls from upstream, OFF stores + reads a dedicated
    /// scope.
    ///
    /// Default `true` covers the 95% case where the user has one
    /// account per vendor. The OFF path supports advanced users who
    /// run two billing accounts on the same vendor (e.g. cheap key
    /// for dictation rewrite, premium key for recap deep-think).
    pub recap_use_same_key: bool,
    /// Model ID override for the meeting recap call (empty = use the
    /// provider-default flagship reasoning model picked by the C# side
    /// via PickRecapModel — claude-opus-4-7 / gemini-3-1-pro / gpt-5).
    /// Lets the user dial down to faster/cheaper models (e.g.
    /// claude-haiku-4-5, gemini-2-5-flash) when meeting recap quality
    /// matters less than turnaround time or cost.
    pub recap_model_override: String,
    pub chunk_streaming_enabled: bool,
    pub preprocessing_enabled: bool,
    pub audio_debug_enabled: bool,
    /// When true, forward `[ggml DEBUG]` lines to dimmy.log. Default false:
    /// per-tensor / per-layer dumps would balloon the log on every cold start.
    pub ggml_debug_logging: bool,
    pub use_keyring: bool,
    // Local STT fields
    pub stt_mode: String,    // "cloud" or "local"
    pub local_model: String, // e.g. "ggml-base-q8_0.bin"
    /// Which local STT backend to use when `stt_mode == "local"`. Either
    /// `"whisper"` (whisper.cpp via the `local-stt` feature, default) or
    /// `"parakeet"` (Parakeet TDT v3 FP32 via the `local-stt-parakeet`
    /// feature). Old configs default to `"whisper"` for compatibility.
    pub local_stt_backend: String,
    /// When true (and backend = parakeet), show a floating live-caption
    /// window below the pill while chunked transcription is running.
    /// Independent from `chunk_streaming_enabled` — a user can have
    /// the chunked engine on for faster final paste while keeping the
    /// caption window off if they find it visually distracting.
    pub live_captions_enabled: bool,
    /// When true, save the recorded WAV alongside the history row so
    /// the user can replay + view a waveform later. Default false for
    /// privacy + storage reasons. Honored by the history-save path in
    /// `dimmy_stop_recording`.
    pub save_audio_in_history: bool,
    /// Background cleanup horizon for history audio files. Files older
    /// than this many days are removed at startup (and on a periodic
    /// timer). 0 disables age-based cleanup; the size cap still applies.
    pub history_audio_keep_days: u32,
    /// Maximum total size of `<config>/history_audio/` in MB. When
    /// exceeded, the oldest files are deleted first until the size is
    /// back under this threshold. 0 disables size-based cleanup.
    pub history_audio_max_mb: u32,
    /// When a dictation recording exceeds this many seconds, the
    /// host (C#/Swift) should fire the meeting-style recap pipeline
    /// in addition to the dictation rewrite. 0 disables auto-recap.
    /// Read by TranscriptionService.StopAndProcessAsync; not used by
    /// Rust directly (the recap call is driven from the host because
    /// it shares the same dimmy_llm_call_raw FFI that the meeting
    /// window already uses).
    pub auto_recap_threshold_secs: u32,
    pub filler_removal_enabled: bool,
    // Local LLM fields
    pub llm_mode: String,        // "cloud" or "local"
    pub local_llm_model: String, // e.g. "gemma-4-E2B-it-Q4_K_M.gguf"
    // UI appearance fields (used by native frontends, opaque to Rust core)
    pub border_style: String,
    pub waveform_style: String,
    pub overlay_position: String,
    pub keep_in_clipboard: bool,
    /// Input gain (0.0-2.0, default 1.0). Attenuate hot mics (e.g. BT headsets).
    pub input_gain: f32,
    /// Loopback (system audio) makeup gain (0.5-4.0, default 2.0).
    /// WASAPI loopback captures post-volume samples — if Windows
    /// volume / per-app Volume Mixer / BT codec are conservative, the
    /// raw signal lands at ~0.1-0.3 peak, way below the mic which
    /// peaks at 0.5-0.9 on consonants. In Mix mode the mic dominates
    /// the sum and system audio sounds "bassissimo". This knob
    /// multiplies the loopback samples in the secondary stream
    /// callback BEFORE they hit the buffer / AEC ring; soft-clipped
    /// via tanh so peaks don't distort.
    pub loopback_gain: f32,
    /// Meeting chunk window in seconds. The meeting worker waits for
    /// this much new audio before firing one STT inference call.
    /// Longer chunks = better LLM context per segment, fewer API
    /// calls (cheaper on cloud), but later visibility of the live
    /// transcript. Range 5.0-60.0, default 15.0. Cloud providers
    /// accept up to ~25 MB so 60 s @ 16k mono int16 (~1.9 MB) is
    /// well within limits.
    pub meeting_chunk_secs: f32,
    /// Notion integration target — UUID of the parent page or database
    /// where each meeting recap lands. Empty string = no target
    /// configured (Settings UI shows the onboarding flow).
    pub notion_target_id: String,
    /// `"page"` or `"database"` — determines the request shape we send
    /// to `POST /v1/pages` (parent.page_id vs parent.database_id, plus
    /// the title-property name). Empty string → defaults to "page".
    pub notion_target_kind: String,
    /// Display name of the target — shown in Settings as "Recaps will
    /// land in: <title>". Pure UI sugar, never sent to the API.
    pub notion_target_title: String,
    /// When true, every meeting auto-sends to Notion at stop time.
    /// When false, the user clicks a "Send to Notion" button in the
    /// MeetingWindow Done view. Default false (opt-in by explicit
    /// click — same caution we apply to all data-egress features).
    pub notion_auto_send: bool,
    /// What to capture: "mic" (default), "system" (loopback — what's
    /// playing through the speakers), or "mix" (both summed in time).
    /// System / Mix are Windows-only for now; on Mac/Linux they fall
    /// back to mic with a log warning. AppConfig isn't serde-derived
    /// (manual JSON I/O); the default is applied in the loader at
    /// line ~665.
    pub audio_source: String,
    // Window position — bottom-right anchor in logical pixels
    pub window_anchor_right: Option<f64>,
    pub window_anchor_bottom: Option<f64>,
    // KPI stats — cumulative across sessions
    pub stats_total_words: u64,
    pub stats_total_speaking_secs: f64,
    /// Per-app context rules. The platform layer captures the foreground
    /// app's identifier at hotkey-down; if any rule matches, the matched
    /// rule's `llm_style` / `llm_translate_to` overrides the user's defaults
    /// for that single transcription. Empty list = no rules, default
    /// behavior preserved. Save/load JSON glue lives in `save_config_file`
    /// and `load_config_file` — `AppConfig` is not derive-Serde.
    pub app_rules: Vec<app_rules::AppRule>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            api_model: DEFAULT_MODEL.to_string(),
            selected_device: None,
            language: String::new(),
            shortcut_mode: "hold".to_string(),
            shortcut: if cfg!(target_os = "macos") {
                "cmd+option"
            } else {
                "win+alt"
            }
            .to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
            user_dict: Vec::new(),
            llm_enabled: false,
            llm_style: llm::LlmStyle::Off,
            llm_tone: llm::LlmTone::None,
            llm_custom_prompt: String::new(),
            llm_translate_to: "none".to_string(),
            llm_api_url: DEFAULT_LLM_URL.to_string(),
            llm_api_model: DEFAULT_LLM_MODEL.to_string(),
            llm_use_same_key: true,
            llm_log_enabled: false,
            llm_auth_method: "api_key".to_string(),
            recap_auth_method: String::new(),
            recap_use_same_key: true,
            recap_model_override: String::new(),
            chunk_streaming_enabled: false,
            preprocessing_enabled: true,
            audio_debug_enabled: false,
            ggml_debug_logging: false,
            use_keyring: false,
            stt_mode: if cfg!(target_os = "macos") {
                "local"
            } else {
                "cloud"
            }
            .to_string(),
            local_model: "ggml-base-q8_0.bin".to_string(),
            local_stt_backend: "whisper".to_string(),
            live_captions_enabled: true,
            save_audio_in_history: false,
            history_audio_keep_days: 30,
            history_audio_max_mb: 5_000,
            auto_recap_threshold_secs: 60,
            filler_removal_enabled: true,
            llm_mode: "cloud".to_string(),
            local_llm_model: local_llm::DEFAULT_LLM_MODEL.to_string(),
            border_style: "Rainbow".to_string(),
            waveform_style: "Bars".to_string(),
            overlay_position: "Bottom Right".to_string(),
            keep_in_clipboard: false,
            input_gain: 0.5,
            // 1.0 = no makeup (passthrough). The "system audio
            // bassissimo" complaint that justified earlier defaults
            // (1.5 / 2.0) turned out to be Volume-Mixer-dependent:
            // when Teams / browser sit at 70-100% the loopback
            // already lands at 0.7-1.0 peak and any boost saturates
            // tanh. When they're at 30-50% the user can dial this up
            // to 1.5 / 2.0 / 3.0 via config without recompiling. Tanh
            // soft-clip stays in the callback as a safety net for
            // user-boosted setups; at gain=1.0 it's effectively
            // identity for in-range samples.
            loopback_gain: 1.0,
            meeting_chunk_secs: 15.0,
            notion_target_id: String::new(),
            notion_target_kind: String::new(),
            notion_target_title: String::new(),
            notion_auto_send: false,
            audio_source: default_audio_source(),
            window_anchor_right: None,
            window_anchor_bottom: None,
            stats_total_words: 0,
            stats_total_speaking_secs: 0.0,
            app_rules: Vec::new(),
        }
    }
}

/// Save non-sensitive config to file (NO api_key — that goes to keyring ONLY)
pub fn save_config_file(cfg: &AppConfig) {
    // Preconditions: validate new config fields
    assert!(
        cfg.input_gain >= 0.0 && cfg.input_gain <= 2.0,
        "save_config_file: input_gain must be in [0.0, 2.0], got {}",
        cfg.input_gain
    );
    assert!(
        cfg.input_gain.is_finite(),
        "save_config_file: input_gain must be finite, got {}",
        cfg.input_gain
    );
    assert!(
        !cfg.border_style.is_empty(),
        "save_config_file: border_style must be non-empty"
    );
    assert!(
        !cfg.waveform_style.is_empty(),
        "save_config_file: waveform_style must be non-empty"
    );
    assert!(
        !cfg.overlay_position.is_empty(),
        "save_config_file: overlay_position must be non-empty"
    );

    if let Some(path) = config_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut json = serde_json::json!({
            "api_url": cfg.api_url,
            "api_model": cfg.api_model,
            "language": cfg.language,
            "shortcut_mode": cfg.shortcut_mode,
            "shortcut": cfg.shortcut,
            "prompt": cfg.prompt,
            "user_dict": cfg.user_dict,
            "llm_enabled": cfg.llm_enabled,
            "llm_style": cfg.llm_style.as_str(),
            "llm_tone": cfg.llm_tone.as_str(),
            "llm_custom_prompt": cfg.llm_custom_prompt,
            "llm_translate_to": cfg.llm_translate_to,
            "llm_api_url": cfg.llm_api_url,
            "llm_api_model": cfg.llm_api_model,
            "llm_use_same_key": cfg.llm_use_same_key,
            "llm_log_enabled": cfg.llm_log_enabled,
            "llm_auth_method": cfg.llm_auth_method,
            "recap_auth_method": cfg.recap_auth_method,
            "recap_use_same_key": cfg.recap_use_same_key,
            "recap_model_override": cfg.recap_model_override,
            "chunk_streaming_enabled": cfg.chunk_streaming_enabled,
            "preprocessing_enabled": cfg.preprocessing_enabled,
            "audio_debug_enabled": cfg.audio_debug_enabled,
            "ggml_debug_logging": cfg.ggml_debug_logging,
            "use_keyring": cfg.use_keyring,
            "stt_mode": cfg.stt_mode,
            "local_model": cfg.local_model,
            "local_stt_backend": cfg.local_stt_backend,
            "live_captions_enabled": cfg.live_captions_enabled,
            "save_audio_in_history": cfg.save_audio_in_history,
            "history_audio_keep_days": cfg.history_audio_keep_days,
            "history_audio_max_mb": cfg.history_audio_max_mb,
            "auto_recap_threshold_secs": cfg.auto_recap_threshold_secs,
            "filler_removal_enabled": cfg.filler_removal_enabled,
            "llm_mode": cfg.llm_mode,
            "local_llm_model": cfg.local_llm_model,
            "border_style": cfg.border_style,
            "waveform_style": cfg.waveform_style,
            "overlay_position": cfg.overlay_position,
            "keep_in_clipboard": cfg.keep_in_clipboard,
            "input_gain": cfg.input_gain,
            "loopback_gain": cfg.loopback_gain,
            "meeting_chunk_secs": cfg.meeting_chunk_secs,
            "notion_target_id": cfg.notion_target_id,
            "notion_target_kind": cfg.notion_target_kind,
            "notion_target_title": cfg.notion_target_title,
            "notion_auto_send": cfg.notion_auto_send,
            "audio_source": cfg.audio_source,
            "stats_total_words": cfg.stats_total_words,
            "stats_total_speaking_secs": cfg.stats_total_speaking_secs,
        });
        // app_rules / selected_device / anchors set outside the json! macro:
        // adding more fields to the macro pushes it past its
        // recursion-expansion limit.
        if let Ok(v) = serde_json::to_value(&cfg.app_rules) {
            json["app_rules"] = v;
        }
        if let Some(ref dev) = cfg.selected_device {
            json["selected_device"] = serde_json::json!(dev);
        }
        if let Some(r) = cfg.window_anchor_right {
            json["window_anchor_right"] = serde_json::json!(r);
        }
        if let Some(b) = cfg.window_anchor_bottom {
            json["window_anchor_bottom"] = serde_json::json!(b);
        }
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&json).unwrap_or_default(),
        );
    }
}

/// Load non-sensitive config from file. Missing LLM fields use defaults (backward compatible).
pub fn load_config_file() -> AppConfig {
    let defaults = AppConfig::default();
    if let Some(path) = config_path() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                let mut cfg = AppConfig {
                    api_url: v["api_url"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(DEFAULT_API_URL)
                        .to_string(),
                    api_model: v["api_model"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(DEFAULT_MODEL)
                        .to_string(),
                    selected_device: v["selected_device"].as_str().map(|s| s.to_string()),
                    language: v["language"].as_str().unwrap_or("").to_string(),
                    shortcut_mode: v["shortcut_mode"].as_str().unwrap_or("toggle").to_string(),
                    shortcut: v["shortcut"]
                        .as_str()
                        .unwrap_or(default_shortcut())
                        .to_string(),
                    prompt: v["prompt"].as_str().unwrap_or(DEFAULT_PROMPT).to_string(),
                    user_dict: v["user_dict"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default(),
                    llm_enabled: v["llm_enabled"].as_bool().unwrap_or(defaults.llm_enabled),
                    llm_style: llm::LlmStyle::from_str_lossy(
                        v["llm_style"].as_str().unwrap_or("off"),
                    ),
                    llm_tone: llm::LlmTone::from_str_lossy(
                        v["llm_tone"].as_str().unwrap_or("none"),
                    ),
                    llm_custom_prompt: v["llm_custom_prompt"]
                        .as_str()
                        .unwrap_or(&defaults.llm_custom_prompt)
                        .to_string(),
                    llm_translate_to: v["llm_translate_to"]
                        .as_str()
                        .unwrap_or(&defaults.llm_translate_to)
                        .to_string(),
                    llm_api_url: v["llm_api_url"]
                        .as_str()
                        .unwrap_or(&defaults.llm_api_url)
                        .to_string(),
                    llm_api_model: v["llm_api_model"]
                        .as_str()
                        .unwrap_or(&defaults.llm_api_model)
                        .to_string(),
                    llm_use_same_key: v["llm_use_same_key"]
                        .as_bool()
                        .unwrap_or(defaults.llm_use_same_key),
                    llm_log_enabled: v["llm_log_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.llm_log_enabled),
                    llm_auth_method: v["llm_auth_method"]
                        .as_str()
                        .unwrap_or(&defaults.llm_auth_method)
                        .to_string(),
                    recap_auth_method: v["recap_auth_method"]
                        .as_str()
                        .unwrap_or(&defaults.recap_auth_method)
                        .to_string(),
                    recap_use_same_key: v["recap_use_same_key"]
                        .as_bool()
                        .unwrap_or(defaults.recap_use_same_key),
                    recap_model_override: v["recap_model_override"]
                        .as_str()
                        .unwrap_or(&defaults.recap_model_override)
                        .to_string(),
                    chunk_streaming_enabled: v["chunk_streaming_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.chunk_streaming_enabled),
                    preprocessing_enabled: v["preprocessing_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.preprocessing_enabled),
                    audio_debug_enabled: v["audio_debug_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.audio_debug_enabled),
                    ggml_debug_logging: v["ggml_debug_logging"]
                        .as_bool()
                        .unwrap_or(defaults.ggml_debug_logging),
                    // Force use_keyring to false — local encrypted storage is the default.
                    // OS keyring is kept as read-only fallback for backward compatibility.
                    use_keyring: false,
                    stt_mode: v["stt_mode"]
                        .as_str()
                        .unwrap_or(&defaults.stt_mode)
                        .to_string(),
                    local_model: v["local_model"]
                        .as_str()
                        .unwrap_or(&defaults.local_model)
                        .to_string(),
                    local_stt_backend: v["local_stt_backend"]
                        .as_str()
                        .unwrap_or(&defaults.local_stt_backend)
                        .to_string(),
                    live_captions_enabled: v["live_captions_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.live_captions_enabled),
                    save_audio_in_history: v["save_audio_in_history"]
                        .as_bool()
                        .unwrap_or(defaults.save_audio_in_history),
                    history_audio_keep_days: v["history_audio_keep_days"]
                        .as_u64()
                        .map(|n| n as u32)
                        .unwrap_or(defaults.history_audio_keep_days),
                    history_audio_max_mb: v["history_audio_max_mb"]
                        .as_u64()
                        .map(|n| n as u32)
                        .unwrap_or(defaults.history_audio_max_mb),
                    auto_recap_threshold_secs: v["auto_recap_threshold_secs"]
                        .as_u64()
                        .map(|n| n as u32)
                        .unwrap_or(defaults.auto_recap_threshold_secs),
                    filler_removal_enabled: v["filler_removal_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.filler_removal_enabled),
                    llm_mode: v["llm_mode"]
                        .as_str()
                        .unwrap_or(&defaults.llm_mode)
                        .to_string(),
                    local_llm_model: v["local_llm_model"]
                        .as_str()
                        .unwrap_or(&defaults.local_llm_model)
                        .to_string(),
                    border_style: v["border_style"].as_str().unwrap_or("Rainbow").to_string(),
                    waveform_style: v["waveform_style"].as_str().unwrap_or("Bars").to_string(),
                    overlay_position: v["overlay_position"]
                        .as_str()
                        .unwrap_or("Bottom Right")
                        .to_string(),
                    keep_in_clipboard: v["keep_in_clipboard"].as_bool().unwrap_or(false),
                    input_gain: v["input_gain"]
                        .as_f64()
                        .unwrap_or(defaults.input_gain as f64)
                        as f32,
                    loopback_gain: v["loopback_gain"]
                        .as_f64()
                        .unwrap_or(defaults.loopback_gain as f64)
                        as f32,
                    meeting_chunk_secs: v["meeting_chunk_secs"]
                        .as_f64()
                        .unwrap_or(defaults.meeting_chunk_secs as f64)
                        as f32,
                    notion_target_id: v["notion_target_id"]
                        .as_str()
                        .unwrap_or(&defaults.notion_target_id)
                        .to_string(),
                    notion_target_kind: v["notion_target_kind"]
                        .as_str()
                        .unwrap_or(&defaults.notion_target_kind)
                        .to_string(),
                    notion_target_title: v["notion_target_title"]
                        .as_str()
                        .unwrap_or(&defaults.notion_target_title)
                        .to_string(),
                    notion_auto_send: v["notion_auto_send"]
                        .as_bool()
                        .unwrap_or(defaults.notion_auto_send),
                    audio_source: v["audio_source"]
                        .as_str()
                        .unwrap_or(&defaults.audio_source)
                        .to_string(),
                    window_anchor_right: v["window_anchor_right"].as_f64(),
                    window_anchor_bottom: v["window_anchor_bottom"].as_f64(),
                    stats_total_words: v["stats_total_words"].as_u64().unwrap_or(0),
                    stats_total_speaking_secs: v["stats_total_speaking_secs"]
                        .as_f64()
                        .unwrap_or(0.0),
                    app_rules: serde_json::from_value(v["app_rules"].clone()).unwrap_or_default(),
                };
                migrate_decommissioned_models(&mut cfg);
                return cfg;
            }
        }
    }
    defaults
}

/// In-place migration: rewrite STT model ids that the provider has
/// retired so the dispatcher doesn't HTTP-400 silently. Today: only
/// the Groq `distil-whisper-large-v3-en` model (decommissioned by
/// Groq 2026-05-15, returns `model_decommissioned`). Coerce to
/// `whisper-large-v3-turbo` (their fastest current English+multi).
///
/// Logged so users tracing dimmy.log can see the migration kicked in
/// once after upgrade.
fn migrate_decommissioned_models(cfg: &mut AppConfig) {
    // ── Groq STT: distil-whisper-large-v3-en decommissioned ────
    // Returns HTTP 400 {"code":"model_decommissioned"}.
    if cfg.api_model == "distil-whisper-large-v3-en" && cfg.api_url.contains("groq.com") {
        log(&format!(
            "[Config] migrating decommissioned Groq STT model '{}' → 'whisper-large-v3-turbo'",
            cfg.api_model
        ));
        cfg.api_model = "whisper-large-v3-turbo".to_string();
    }
    // ── Gemini STT/LLM: gemini-3.1-flash / gemini-3.1-pro never
    //    existed as plain ids (Google ships only `-lite` and
    //    `-preview` suffixes for 3.1). Returns HTTP 404. Coerce to
    //    the closest same-tier id that actually exists.
    if cfg.api_url.contains("googleapis.com") {
        // Plain 3.1 ids never existed → coerce to the closest
        // working same-family id.
        if cfg.api_model == "gemini-3.1-flash" {
            log("[Config] migrating Gemini STT 'gemini-3.1-flash' → 'gemini-3.1-flash-lite'");
            cfg.api_model = "gemini-3.1-flash-lite".to_string();
        }
        // STT Pro variants → Flash variants (UX policy 2026-05-15:
        // Pro is slower without accuracy gain for transcription).
        if cfg.api_model == "gemini-3.1-pro" || cfg.api_model == "gemini-3.1-pro-preview" {
            log("[Config] migrating Gemini Pro STT → 'gemini-3.1-flash-lite' (Pro = LLM-only now)");
            cfg.api_model = "gemini-3.1-flash-lite".to_string();
        }
        if cfg.api_model == "gemini-3-pro-preview" {
            log("[Config] migrating Gemini 3-pro STT → 'gemini-3-flash-preview'");
            cfg.api_model = "gemini-3-flash-preview".to_string();
        }
        if cfg.api_model == "gemini-2.5-pro" {
            log("[Config] migrating Gemini 2.5-pro STT → 'gemini-2.5-flash'");
            cfg.api_model = "gemini-2.5-flash".to_string();
        }
    }
    if cfg.llm_api_url.contains("googleapis.com") {
        if cfg.llm_api_model == "gemini-3.1-flash" {
            log("[Config] migrating Gemini LLM 'gemini-3.1-flash' → 'gemini-3.1-flash-lite'");
            cfg.llm_api_model = "gemini-3.1-flash-lite".to_string();
        }
        if cfg.llm_api_model == "gemini-3.1-pro" {
            log("[Config] migrating Gemini LLM 'gemini-3.1-pro' → 'gemini-3.1-pro-preview'");
            cfg.llm_api_model = "gemini-3.1-pro-preview".to_string();
        }
    }
    // ── Anthropic LLM: upgrade Sonnet 4 (May 2025 dated id) to
    //    Sonnet 4.6 (named-tier successor, October 2026). Old id
    //    still works server-side but the dropdown labels Sonnet
    //    entries as 4.6 so the config should match.
    if cfg.llm_api_url.contains("anthropic.com") && cfg.llm_api_model == "claude-sonnet-4-20250514"
    {
        log("[Config] upgrading Anthropic LLM 'claude-sonnet-4-20250514' → 'claude-sonnet-4-6'");
        cfg.llm_api_model = "claude-sonnet-4-6".to_string();
    }
}

/// Migrate from old "pai-voice" config/keyring to "dimmy" for existing users.
pub fn migrate_from_pai_voice() {
    let dimmy_dir = match config_dir_path() {
        Some(d) => d,
        None => return,
    };
    // If dimmy config dir already exists, skip migration
    if dimmy_dir.exists() {
        return;
    }
    let old_dir = match dirs::config_dir().map(|p| p.join("pai-voice")) {
        Some(d) => d,
        None => return,
    };
    if !old_dir.exists() {
        return;
    }
    log("Migrating from pai-voice to dimmy...");

    // Copy config and log files
    let _ = std::fs::create_dir_all(&dimmy_dir);
    for name in &["config.json", "pai-voice.log"] {
        let src = old_dir.join(name);
        if src.exists() {
            let dest_name = if *name == "pai-voice.log" {
                "dimmy.log"
            } else {
                name
            };
            let dest = dimmy_dir.join(dest_name);
            if let Err(e) = std::fs::copy(&src, &dest) {
                log(&format!("WARNING: failed to copy {}: {}", name, e));
            } else {
                log(&format!("Copied {} -> {}", src.display(), dest.display()));
            }
        }
    }

    // Migrate keyring entries from service "pai-voice" to "dimmy"
    let key_names = [
        "api-key-groq",
        "api-key-openai",
        "api-key-custom",
        "llm-key-groq",
        "llm-key-openai",
        "llm-key-custom",
        "api-key",
        "llm-api-key",
    ];
    for name in &key_names {
        if let Ok(old_entry) = keyring::Entry::new("pai-voice", name) {
            if let Ok(key) = old_entry.get_password() {
                if let Ok(new_entry) = keyring::Entry::new("dimmy", name) {
                    match new_entry.set_password(&key) {
                        Ok(()) => log(&format!("Migrated keyring entry: {}", name)),
                        Err(e) => log(&format!(
                            "WARNING: keyring migration failed for {}: {}",
                            name, e
                        )),
                    }
                }
            }
        }
    }
    log("Migration from pai-voice complete");
}

/// Migrate: if old config.json has api_key in plain text, move to secure storage and REMOVE from file
pub fn migrate_plaintext_key(store: &keystore::KeyStore, use_keyring: bool) {
    if let Some(path) = config_path() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(key) = v["api_key"].as_str() {
                    if !key.is_empty() {
                        let api_url = v["api_url"].as_str().unwrap_or(DEFAULT_API_URL);
                        let provider = Provider::from_url(api_url);
                        log(&format!(
                            "Migrating plaintext API key to secure storage (provider={})...",
                            provider
                        ));
                        match save_key_with_store(
                            store,
                            KeyringScope::Stt(provider),
                            key,
                            use_keyring,
                        ) {
                            Ok(()) => log("Key migrated to secure storage"),
                            Err(e) => log(&format!("WARNING: migration failed: {}", e)),
                        }
                        // Re-save config without the plaintext key
                        let cfg = load_config_file();
                        save_config_file(&cfg);
                        log("Plaintext key removed from config file");
                    }
                }
            }
        }
    }
}

// ── Per-provider secure key storage ─────────────────────────────────
// Keys are stored per-provider so switching providers doesn't lose keys.
// Key storage: routes through KeyStore (local encrypted file or OS keyring)

use provider::{KeyringScope, Provider};

/// Convenience wrappers that read use_keyring from AppState.
/// These maintain the same call signatures used throughout lib.rs.
pub fn save_key_with_store(
    store: &keystore::KeyStore,
    scope: KeyringScope,
    key: &str,
    use_keyring: bool,
) -> Result<(), String> {
    store.save_key(scope, key, use_keyring)
}

pub fn load_key_with_store(
    store: &keystore::KeyStore,
    scope: KeyringScope,
    use_keyring: bool,
) -> Option<String> {
    store.load_key(scope, use_keyring)
}

/// Migrate old single "api-key" and "llm-api-key" keyring entries to per-provider entries.
/// This handles the legacy format from before per-provider key storage.
pub fn migrate_keyring_to_per_provider(
    store: &keystore::KeyStore,
    api_url: &str,
    llm_api_url: &str,
    use_keyring: bool,
) {
    // Migrate transcription key
    if let Ok(entry) = keyring::Entry::new("dimmy", "api-key") {
        if let Ok(key) = entry.get_password() {
            let provider = Provider::from_url(api_url);
            log(&format!("Migrating old api-key to api-key-{}", provider));
            let _ = save_key_with_store(store, KeyringScope::Stt(provider), &key, use_keyring);
            store.delete_legacy_key("dimmy", "api-key");
        }
    }
    // Migrate LLM key
    if let Ok(entry) = keyring::Entry::new("dimmy", "llm-api-key") {
        if let Ok(key) = entry.get_password() {
            let provider = Provider::from_url(llm_api_url);
            log(&format!(
                "Migrating old llm-api-key to llm-key-{}",
                provider
            ));
            let _ = save_key_with_store(store, KeyringScope::Llm(provider), &key, use_keyring);
            store.delete_legacy_key("dimmy", "llm-api-key");
        }
    }
}

pub struct AppState {
    pub recording: Mutex<bool>,
    pub api_key: Mutex<Option<String>>,
    pub api_url: Mutex<String>,
    pub api_model: Mutex<String>,
    pub language: Mutex<String>,
    pub prompt: Mutex<String>, // Whisper style prompt (punctuation + vocabulary)
    /// User-curated vocabulary — see [`AppConfig::user_dict`].
    /// Mutated by `dimmy_user_dict_add` / `_remove` FFI; read by
    /// `compose_stt_prompt` which joins it with `prompt` at STT time.
    pub user_dict: Mutex<Vec<String>>,
    pub shortcut_mode: Mutex<String>, // "toggle" or "hold"
    pub shortcut: Mutex<String>,      // "win+alt", "ctrl+alt", "ctrl+shift"
    pub selected_device: Mutex<Option<String>>,
    pub audio_sample_rate: Mutex<u32>,
    pub transcript: Mutex<String>,
    pub audio_buffer: Arc<Mutex<Vec<f32>>>,
    /// Dedicated buffer for the loopback (system audio) stream when
    /// audio_source = Mix. Stays empty in Mic / System modes. meeting.rs
    /// reads both buffers and mixes per-sample so playback timing
    /// matches reality and sources are coherently combined.
    pub audio_buffer_secondary: Arc<Mutex<Vec<f32>>>,
    pub audio_tx: Mutex<Sender<AudioCommand>>,
    pub streaming_active: Arc<AtomicBool>,
    // LLM post-processing state
    pub llm_enabled: Mutex<bool>,
    pub llm_style: Mutex<llm::LlmStyle>,
    pub llm_tone: Mutex<llm::LlmTone>,
    pub llm_custom_prompt: Mutex<String>,
    pub llm_translate_to: Mutex<String>,
    pub llm_api_url: Mutex<String>,
    pub llm_api_model: Mutex<String>,
    pub llm_use_same_key: Mutex<bool>,
    pub llm_auth_method: Mutex<String>,
    pub recap_auth_method: Mutex<String>,
    pub recap_use_same_key: Mutex<bool>,
    pub recap_model_override: Mutex<String>,
    pub llm_api_key: Mutex<Option<String>>,
    pub llm_log_enabled: Mutex<bool>,
    pub chunk_streaming_enabled: Mutex<bool>,
    pub preprocessing_enabled: Mutex<bool>,
    pub audio_debug_enabled: Mutex<bool>,
    pub ggml_debug_logging: Mutex<bool>,
    pub use_keyring: Mutex<bool>,
    // Local STT state
    pub stt_mode: Mutex<String>,
    pub local_model: Mutex<String>,
    pub local_stt_backend: Mutex<String>,
    pub live_captions_enabled: Mutex<bool>,
    pub save_audio_in_history: Mutex<bool>,
    pub history_audio_keep_days: Mutex<u32>,
    pub history_audio_max_mb: Mutex<u32>,
    pub auto_recap_threshold_secs: Mutex<u32>,
    pub filler_removal_enabled: Mutex<bool>,
    // Local LLM state
    pub llm_mode: Mutex<String>,
    pub local_llm_model: Mutex<String>,
    // UI appearance (opaque to Rust, round-tripped for native frontends)
    pub border_style: Mutex<String>,
    pub waveform_style: Mutex<String>,
    pub overlay_position: Mutex<String>,
    pub keep_in_clipboard: Mutex<bool>,
    /// Input gain as AtomicU32 (f32 bits) — shared with audio capture thread
    pub input_gain: Arc<std::sync::atomic::AtomicU32>,
    pub loopback_gain: Arc<std::sync::atomic::AtomicU32>,
    pub meeting_chunk_secs: Mutex<f32>,
    /// Notion integration target — see [`AppConfig::notion_target_id`].
    pub notion_target_id: Mutex<String>,
    pub notion_target_kind: Mutex<String>,
    pub notion_target_title: Mutex<String>,
    pub notion_auto_send: Mutex<bool>,
    /// Audio source: "mic" | "system" | "mix". Read by dimmy_start_recording
    /// + meeting start to pick which AudioCommand::Start variant to send.
    pub audio_source: Mutex<String>,
    pub key_store: keystore::KeyStore,
    /// Path to current audio debug session directory (set during recording, cleared on stop)
    pub audio_debug_session_dir: Mutex<Option<std::path::PathBuf>>,
    /// Bottom-right anchor in logical pixels — persisted across restarts
    pub window_anchor: Mutex<Option<(f64, f64)>>,
    // KPI stats
    pub stats_total_words: Mutex<u64>,
    pub stats_total_speaking_secs: Mutex<f64>,
    // App-context rules (loaded at startup, mutated via FFI setter)
    pub app_rules: Mutex<Vec<crate::app_rules::AppRule>>,
    /// Snapshot of the foreground app at the most recent hotkey-down. The
    /// platform layer writes this via FFI before recording begins; the LLM
    /// post-process step reads it to resolve `app_rules`. Cleared after each
    /// transcription so a stale snapshot can't bleed into the next.
    pub current_app_context: Mutex<crate::app_rules::AppContext>,
    // Transcription history (SQLite-backed)
    pub history_store: Mutex<Option<crate::history::HistoryStore>>,
}

impl AppState {
    /// Create AppState from config file + keyring, without any UI framework.
    /// Used by native Linux UI and tests.
    pub fn new_standalone() -> Self {
        migrate_from_pai_voice();

        let file_cfg = load_config_file();
        let use_kr = file_cfg.use_keyring;
        let key_store = keystore::KeyStore::new();

        migrate_plaintext_key(&key_store, use_kr);
        migrate_keyring_to_per_provider(
            &key_store,
            &file_cfg.api_url,
            &file_cfg.llm_api_url,
            use_kr,
        );

        let transcription_provider = provider::Provider::from_url(&file_cfg.api_url);
        let llm_provider = provider::Provider::from_url(&file_cfg.llm_api_url);
        let stored_key = load_key_with_store(
            &key_store,
            provider::KeyringScope::Stt(transcription_provider),
            use_kr,
        );
        let stored_llm_key = load_key_with_store(
            &key_store,
            provider::KeyringScope::Llm(llm_provider),
            use_kr,
        );

        log(&format!(
            "Config loaded: url={}, model={}, device={:?}, provider={}, has_key={}, llm_provider={}, llm_enabled={}, llm_style={}",
            file_cfg.api_url, file_cfg.api_model, file_cfg.selected_device, transcription_provider,
            stored_key.is_some(), llm_provider, file_cfg.llm_enabled, file_cfg.llm_style.as_str()
        ));

        let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let audio_buffer_secondary = Arc::new(Mutex::new(Vec::<f32>::new()));
        let input_gain_atomic = Arc::new(std::sync::atomic::AtomicU32::new(
            file_cfg.input_gain.to_bits(),
        ));
        let loopback_gain_atomic = Arc::new(std::sync::atomic::AtomicU32::new(
            file_cfg.loopback_gain.to_bits(),
        ));
        let audio_tx = audio::spawn_audio_thread(
            audio_buffer.clone(),
            audio_buffer_secondary.clone(),
            input_gain_atomic.clone(),
            loopback_gain_atomic.clone(),
        );

        let state = AppState {
            recording: Mutex::new(false),
            api_key: Mutex::new(stored_key),
            api_url: Mutex::new(file_cfg.api_url),
            api_model: Mutex::new(file_cfg.api_model),
            language: Mutex::new(file_cfg.language),
            prompt: Mutex::new(file_cfg.prompt),
            user_dict: Mutex::new(file_cfg.user_dict),
            shortcut_mode: Mutex::new(file_cfg.shortcut_mode),
            shortcut: Mutex::new(file_cfg.shortcut),
            selected_device: Mutex::new(file_cfg.selected_device.clone()),
            audio_sample_rate: Mutex::new(audio::device_sample_rate(&file_cfg.selected_device)),
            transcript: Mutex::new(String::new()),
            audio_buffer,
            audio_buffer_secondary,
            audio_tx: Mutex::new(audio_tx),
            streaming_active: Arc::new(AtomicBool::new(false)),
            llm_enabled: Mutex::new(file_cfg.llm_enabled),
            llm_style: Mutex::new(file_cfg.llm_style),
            llm_tone: Mutex::new(file_cfg.llm_tone),
            llm_custom_prompt: Mutex::new(file_cfg.llm_custom_prompt),
            llm_translate_to: Mutex::new(file_cfg.llm_translate_to),
            llm_api_url: Mutex::new(file_cfg.llm_api_url),
            llm_api_model: Mutex::new(file_cfg.llm_api_model),
            llm_use_same_key: Mutex::new(file_cfg.llm_use_same_key),
            llm_auth_method: Mutex::new(file_cfg.llm_auth_method),
            recap_auth_method: Mutex::new(file_cfg.recap_auth_method),
            recap_use_same_key: Mutex::new(file_cfg.recap_use_same_key),
            recap_model_override: Mutex::new(file_cfg.recap_model_override),
            llm_api_key: Mutex::new(stored_llm_key),
            llm_log_enabled: Mutex::new(file_cfg.llm_log_enabled),
            chunk_streaming_enabled: Mutex::new(file_cfg.chunk_streaming_enabled),
            preprocessing_enabled: Mutex::new(file_cfg.preprocessing_enabled),
            audio_debug_enabled: Mutex::new(file_cfg.audio_debug_enabled),
            ggml_debug_logging: Mutex::new(file_cfg.ggml_debug_logging),
            use_keyring: Mutex::new(file_cfg.use_keyring),
            stt_mode: Mutex::new(file_cfg.stt_mode),
            local_model: Mutex::new(file_cfg.local_model),
            local_stt_backend: Mutex::new(file_cfg.local_stt_backend),
            live_captions_enabled: Mutex::new(file_cfg.live_captions_enabled),
            save_audio_in_history: Mutex::new(file_cfg.save_audio_in_history),
            history_audio_keep_days: Mutex::new(file_cfg.history_audio_keep_days),
            history_audio_max_mb: Mutex::new(file_cfg.history_audio_max_mb),
            auto_recap_threshold_secs: Mutex::new(file_cfg.auto_recap_threshold_secs),
            filler_removal_enabled: Mutex::new(file_cfg.filler_removal_enabled),
            llm_mode: Mutex::new(file_cfg.llm_mode),
            local_llm_model: Mutex::new(file_cfg.local_llm_model),
            border_style: Mutex::new(file_cfg.border_style),
            waveform_style: Mutex::new(file_cfg.waveform_style),
            overlay_position: Mutex::new(file_cfg.overlay_position),
            keep_in_clipboard: Mutex::new(file_cfg.keep_in_clipboard),
            input_gain: input_gain_atomic,
            loopback_gain: loopback_gain_atomic,
            meeting_chunk_secs: Mutex::new(file_cfg.meeting_chunk_secs),
            notion_target_id: Mutex::new(file_cfg.notion_target_id.clone()),
            notion_target_kind: Mutex::new(file_cfg.notion_target_kind.clone()),
            notion_target_title: Mutex::new(file_cfg.notion_target_title.clone()),
            notion_auto_send: Mutex::new(file_cfg.notion_auto_send),
            audio_source: Mutex::new(file_cfg.audio_source.clone()),
            key_store,
            audio_debug_session_dir: Mutex::new(None),
            window_anchor: Mutex::new(
                match (file_cfg.window_anchor_right, file_cfg.window_anchor_bottom) {
                    (Some(r), Some(b)) => Some((r, b)),
                    _ => None,
                },
            ),
            stats_total_words: Mutex::new(file_cfg.stats_total_words),
            stats_total_speaking_secs: Mutex::new(file_cfg.stats_total_speaking_secs),
            app_rules: Mutex::new(file_cfg.app_rules.clone()),
            current_app_context: Mutex::new(crate::app_rules::AppContext::default()),
            history_store: Mutex::new({
                let history_db = crate::config_dir_path()
                    .map(|p| p.join("history.db"))
                    .unwrap_or_else(|| std::path::PathBuf::from("history.db"));
                crate::history::HistoryStore::new(&history_db).ok()
            }),
        };

        // Postconditions
        assert!(
            !state.api_url.lock().unwrap().is_empty(),
            "api_url must not be empty after init"
        );
        assert!(
            !state.api_model.lock().unwrap().is_empty(),
            "api_model must not be empty after init"
        );

        state
    }
}

/// Build AppConfig by acquiring each mutex individually (one at a time, no overlapping locks).
/// Shared by all native UI consumers via FFI.
pub fn snapshot_config(state: &AppState) -> Result<AppConfig, String> {
    let api_url = state.api_url.lock().map_err(|e| e.to_string())?.clone();
    let api_model = state.api_model.lock().map_err(|e| e.to_string())?.clone();
    let selected_device = state
        .selected_device
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let language = state.language.lock().map_err(|e| e.to_string())?.clone();
    let shortcut_mode = state
        .shortcut_mode
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let shortcut = state.shortcut.lock().map_err(|e| e.to_string())?.clone();
    let prompt = state.prompt.lock().map_err(|e| e.to_string())?.clone();
    let user_dict = state.user_dict.lock().map_err(|e| e.to_string())?.clone();
    let llm_enabled = *state.llm_enabled.lock().map_err(|e| e.to_string())?;
    let llm_style = *state.llm_style.lock().map_err(|e| e.to_string())?;
    let llm_tone = *state.llm_tone.lock().map_err(|e| e.to_string())?;
    let llm_custom_prompt = state
        .llm_custom_prompt
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_translate_to = state
        .llm_translate_to
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_api_url = state.llm_api_url.lock().map_err(|e| e.to_string())?.clone();
    let llm_api_model = state
        .llm_api_model
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_use_same_key = *state.llm_use_same_key.lock().map_err(|e| e.to_string())?;
    let llm_log_enabled = *state.llm_log_enabled.lock().map_err(|e| e.to_string())?;
    let llm_auth_method = state
        .llm_auth_method
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let recap_auth_method = state
        .recap_auth_method
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let recap_use_same_key = *state.recap_use_same_key.lock().map_err(|e| e.to_string())?;
    let recap_model_override = state
        .recap_model_override
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let chunk_streaming_enabled = *state
        .chunk_streaming_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let preprocessing_enabled = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let audio_debug_enabled = *state
        .audio_debug_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let ggml_debug_logging = *state.ggml_debug_logging.lock().map_err(|e| e.to_string())?;
    let stt_mode = state.stt_mode.lock().map_err(|e| e.to_string())?.clone();
    let local_model = state.local_model.lock().map_err(|e| e.to_string())?.clone();
    let local_stt_backend = state
        .local_stt_backend
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let live_captions_enabled = *state
        .live_captions_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let save_audio_in_history = *state
        .save_audio_in_history
        .lock()
        .map_err(|e| e.to_string())?;
    let history_audio_keep_days = *state
        .history_audio_keep_days
        .lock()
        .map_err(|e| e.to_string())?;
    let history_audio_max_mb = *state
        .history_audio_max_mb
        .lock()
        .map_err(|e| e.to_string())?;
    let auto_recap_threshold_secs = *state
        .auto_recap_threshold_secs
        .lock()
        .map_err(|e| e.to_string())?;
    let filler_removal_enabled = *state
        .filler_removal_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let anchor = *state.window_anchor.lock().map_err(|e| e.to_string())?;

    Ok(AppConfig {
        api_url,
        api_model,
        selected_device,
        language,
        shortcut_mode,
        shortcut,
        prompt,
        user_dict,
        llm_enabled,
        llm_style,
        llm_tone,
        llm_custom_prompt,
        llm_translate_to,
        llm_api_url,
        llm_api_model,
        llm_use_same_key,
        llm_log_enabled,
        llm_auth_method,
        recap_auth_method,
        recap_use_same_key,
        recap_model_override,
        chunk_streaming_enabled,
        preprocessing_enabled,
        audio_debug_enabled,
        ggml_debug_logging,
        use_keyring: *state.use_keyring.lock().map_err(|e| e.to_string())?,
        stt_mode,
        local_model,
        local_stt_backend,
        live_captions_enabled,
        save_audio_in_history,
        history_audio_keep_days,
        history_audio_max_mb,
        auto_recap_threshold_secs,
        filler_removal_enabled,
        llm_mode: state.llm_mode.lock().map_err(|e| e.to_string())?.clone(),
        local_llm_model: state
            .local_llm_model
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        border_style: state
            .border_style
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        waveform_style: state
            .waveform_style
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        overlay_position: state
            .overlay_position
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        keep_in_clipboard: *state.keep_in_clipboard.lock().map_err(|e| e.to_string())?,
        input_gain: f32::from_bits(state.input_gain.load(Ordering::Relaxed)),
        loopback_gain: f32::from_bits(state.loopback_gain.load(Ordering::Relaxed)),
        meeting_chunk_secs: *state.meeting_chunk_secs.lock().map_err(|e| e.to_string())?,
        notion_target_id: state
            .notion_target_id
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        notion_target_kind: state
            .notion_target_kind
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        notion_target_title: state
            .notion_target_title
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        notion_auto_send: *state.notion_auto_send.lock().map_err(|e| e.to_string())?,
        audio_source: state
            .audio_source
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        window_anchor_right: anchor.map(|(r, _)| r),
        window_anchor_bottom: anchor.map(|(_, b)| b),
        stats_total_words: *state.stats_total_words.lock().map_err(|e| e.to_string())?,
        stats_total_speaking_secs: *state
            .stats_total_speaking_secs
            .lock()
            .map_err(|e| e.to_string())?,
        app_rules: state.app_rules.lock().map_err(|e| e.to_string())?.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_stt_prompt_empty_both() {
        assert_eq!(compose_stt_prompt("", &[]), "");
    }

    #[test]
    fn compose_stt_prompt_dict_only() {
        let dict = vec!["foo".to_string(), "bar".to_string()];
        assert_eq!(compose_stt_prompt("", &dict), "foo, bar");
    }

    #[test]
    fn compose_stt_prompt_prompt_only() {
        assert_eq!(
            compose_stt_prompt("a quick brown fox", &[]),
            "a quick brown fox"
        );
    }

    #[test]
    fn compose_stt_prompt_both_appends() {
        let dict = vec!["Notion".to_string(), "Velopack".to_string()];
        assert_eq!(
            compose_stt_prompt("technical context", &dict),
            "technical context, Notion, Velopack"
        );
    }

    #[test]
    fn compose_stt_prompt_filters_empty_dict_entries() {
        let dict = vec![
            "foo".to_string(),
            "".to_string(),
            "  ".to_string(),
            "bar".to_string(),
        ];
        assert_eq!(compose_stt_prompt("", &dict), "foo, bar");
    }

    #[test]
    fn compose_stt_prompt_trims_outer_prompt() {
        let dict = vec!["x".to_string()];
        // Leading/trailing whitespace in user prompt shouldn't leak
        // into the composed output — trimmed before the comma join.
        assert_eq!(compose_stt_prompt("  hello  ", &dict), "hello, x");
    }

    #[test]
    fn config_roundtrip_with_device() {
        // Use a temp dir to avoid polluting real config
        let tmp = std::env::temp_dir().join("dimmy-test-config");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");

        let cfg = serde_json::json!({
            "api_url": "https://test.example.com/v1/transcribe",
            "api_model": "test-model-v1",
            "language": "it",
            "selected_device": "Test Microphone"
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();

        assert_eq!(
            v["api_url"].as_str().unwrap(),
            "https://test.example.com/v1/transcribe"
        );
        assert_eq!(v["api_model"].as_str().unwrap(), "test-model-v1");
        assert_eq!(v["language"].as_str().unwrap(), "it");
        assert_eq!(v["selected_device"].as_str().unwrap(), "Test Microphone");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_roundtrip_no_device() {
        let tmp = std::env::temp_dir().join("dimmy-test-config2");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");

        let cfg = serde_json::json!({
            "api_url": DEFAULT_API_URL,
            "api_model": DEFAULT_MODEL,
            "language": "",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();

        assert_eq!(v["api_url"].as_str().unwrap(), DEFAULT_API_URL);
        assert_eq!(v["api_model"].as_str().unwrap(), DEFAULT_MODEL);
        assert!(
            v["selected_device"].is_null(),
            "No device should produce null"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_missing_file_returns_defaults() {
        let cfg = load_config_file();
        assert!(!cfg.api_url.is_empty(), "URL should not be empty");
        assert!(!cfg.api_model.is_empty(), "Model should not be empty");
    }

    #[test]
    fn save_config_no_panic_with_none_device() {
        let mut cfg = AppConfig::default();
        cfg.selected_device = None;
        save_config_file(&cfg);
    }

    #[test]
    fn save_config_no_panic_with_some_device() {
        let mut cfg = AppConfig::default();
        cfg.selected_device = Some("Mic".to_string());
        cfg.language = "it".to_string();
        save_config_file(&cfg);
    }

    #[test]
    fn config_dir_path_returns_some() {
        let p = config_dir_path();
        assert!(p.is_some(), "config_dir_path should return Some on any OS");
        let p = p.unwrap();
        assert!(p.ends_with("dimmy"), "Path should end with 'dimmy'");
    }

    #[test]
    fn config_path_ends_with_config_json() {
        let p = config_path();
        assert!(p.is_some());
        let p = p.unwrap();
        assert!(
            p.ends_with("config.json"),
            "Should end with config.json, got: {:?}",
            p
        );
    }

    #[test]
    fn log_path_ends_with_log_file() {
        let p = log_path();
        assert!(p.is_some());
        let p = p.unwrap();
        assert!(
            p.ends_with("dimmy.log"),
            "Should end with dimmy.log, got: {:?}",
            p
        );
    }

    #[test]
    fn log_writes_timestamped_line_to_file() {
        let tmp = std::env::temp_dir().join("dimmy-test-log");
        let _ = std::fs::create_dir_all(&tmp);
        let log_file = tmp.join("test.log");
        // Write directly using the same logic as log()
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
                .unwrap();
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            writeln!(f, "[{}] {}", ts, "test log message").unwrap();
        }
        let contents = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            contents.contains("test log message"),
            "Log should contain the message"
        );
        assert!(
            contents.contains("[20"),
            "Log should contain timestamp starting with [20xx"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_returns_valid_strings() {
        let cfg = load_config_file();
        assert!(
            cfg.api_url.starts_with("https://"),
            "URL should be HTTPS, got: {}",
            cfg.api_url
        );
        assert!(!cfg.api_model.is_empty(), "Model should not be empty");
    }

    #[test]
    fn save_config_creates_valid_json() {
        let mut cfg = AppConfig::default();
        cfg.api_url = "https://test.local/v1".to_string();
        cfg.api_model = "test-m".to_string();
        cfg.selected_device = Some("Dev1".into());
        cfg.language = "fr".to_string();
        save_config_file(&cfg);
        if let Some(path) = config_path() {
            let data = std::fs::read_to_string(&path).unwrap();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                assert!(v["api_url"].is_string(), "api_url should be a string");
                assert!(v["api_model"].is_string(), "api_model should be a string");
            }
            save_config_file(&AppConfig::default());
        }
    }

    #[test]
    fn migrate_plaintext_key_no_panic_without_key() {
        let store = keystore::KeyStore::new();
        migrate_plaintext_key(&store, false);
    }

    #[test]
    fn default_constants_are_valid() {
        assert!(
            DEFAULT_API_URL.starts_with("https://"),
            "API URL should use HTTPS"
        );
        assert!(
            !DEFAULT_MODEL.is_empty(),
            "Default model should not be empty"
        );
        assert!(
            DEFAULT_LLM_URL.starts_with("https://"),
            "LLM URL should use HTTPS"
        );
        assert!(
            !DEFAULT_LLM_MODEL.is_empty(),
            "Default LLM model should not be empty"
        );
    }

    #[test]
    fn llm_config_defaults() {
        let cfg = AppConfig::default();
        assert!(!cfg.llm_enabled, "LLM should be disabled by default");
        assert_eq!(cfg.llm_style, llm::LlmStyle::Off);
        assert_eq!(cfg.llm_tone, llm::LlmTone::None);
        assert!(cfg.llm_custom_prompt.is_empty());
        assert_eq!(cfg.llm_translate_to, "none");
        assert_eq!(cfg.llm_api_url, DEFAULT_LLM_URL);
        assert_eq!(cfg.llm_api_model, DEFAULT_LLM_MODEL);
        assert!(cfg.llm_use_same_key, "Should use same key by default");
    }

    #[test]
    fn llm_backward_compat_old_config() {
        // Simulate an old config file without LLM fields
        let tmp = std::env::temp_dir().join("dimmy-test-compat");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");

        let old_cfg = serde_json::json!({
            "api_url": "https://old.example.com/v1",
            "api_model": "old-model",
            "language": "de",
            "shortcut_mode": "hold",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&old_cfg).unwrap()).unwrap();

        // Manually parse to verify defaults apply
        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        let defaults = AppConfig::default();

        assert!(
            v["llm_enabled"].is_null(),
            "Old config shouldn't have llm_enabled"
        );
        assert_eq!(
            v["llm_enabled"].as_bool().unwrap_or(defaults.llm_enabled),
            false
        );
        assert_eq!(
            llm::LlmStyle::from_str_lossy(v["llm_style"].as_str().unwrap_or("off")),
            llm::LlmStyle::Off
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn onboarding_marker_path_returns_some() {
        let p = onboarding_marker_path();
        assert!(p.is_some(), "onboarding_marker_path should return Some");
        let p = p.unwrap();
        assert!(
            p.ends_with(".onboarding_done"),
            "Should end with .onboarding_done, got: {:?}",
            p
        );
        // Must be inside the dimmy config dir
        assert!(
            p.parent().unwrap().to_str().unwrap().contains("dimmy"),
            "Marker must be inside dimmy config dir"
        );
    }

    #[test]
    fn onboarding_marker_roundtrip() {
        // Use temp dir to avoid touching real marker
        let tmp = std::env::temp_dir().join("dimmy-test-onboarding");
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::create_dir_all(&tmp);
        let marker = tmp.join(".onboarding_done");

        // Before marker exists: not completed
        assert!(!marker.exists(), "Marker should not exist yet");

        // Create marker
        std::fs::write(&marker, "1").unwrap();
        assert!(marker.exists(), "Marker must exist after write");

        // Read marker — contents don't matter, existence is the signal
        assert!(marker.is_file(), "Marker must be a file");

        // Deleting config.json should NOT affect marker
        let fake_config = tmp.join("config.json");
        std::fs::write(&fake_config, "{}").unwrap();
        std::fs::remove_file(&fake_config).unwrap();
        assert!(marker.exists(), "Marker must survive config.json deletion");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn mark_onboarding_done_creates_file() {
        // This test uses the real config dir but is idempotent
        // (marking done when already done is a no-op)
        let result = mark_onboarding_done();
        assert!(result.is_ok(), "mark_onboarding_done should succeed");
        assert!(
            onboarding_completed(),
            "onboarding_completed must return true after marking done"
        );
    }

    #[test]
    fn onboarding_completed_is_idempotent() {
        // Calling mark_onboarding_done multiple times must not fail
        let r1 = mark_onboarding_done();
        let r2 = mark_onboarding_done();
        assert!(r1.is_ok());
        assert!(r2.is_ok());
        assert!(onboarding_completed());
    }

    // ── input_gain / UI field tests ──────────────────────────────────

    #[test]
    fn default_config_input_gain_is_half() {
        let cfg = AppConfig::default();
        assert!(
            (cfg.input_gain - 0.5).abs() < f32::EPSILON,
            "Default input_gain should be 0.5, got {}",
            cfg.input_gain
        );
    }

    #[test]
    fn default_config_ui_fields_non_empty() {
        let cfg = AppConfig::default();
        assert!(
            !cfg.border_style.is_empty(),
            "border_style must not be empty"
        );
        assert!(
            !cfg.waveform_style.is_empty(),
            "waveform_style must not be empty"
        );
        assert!(
            !cfg.overlay_position.is_empty(),
            "overlay_position must not be empty"
        );
    }

    #[test]
    fn load_config_defaults_input_gain_when_missing() {
        // Write a config file without input_gain field
        let tmp = std::env::temp_dir().join("dimmy-test-gain-missing");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");
        let cfg_json = serde_json::json!({
            "api_url": DEFAULT_API_URL,
            "api_model": DEFAULT_MODEL,
            "language": "en",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg_json).unwrap()).unwrap();

        // Parse manually the same way load_config_file does
        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        let gain = v["input_gain"].as_f64().unwrap_or(1.0) as f32;
        assert!(
            (gain - 1.0).abs() < f32::EPSILON,
            "Missing input_gain should default to 1.0, got {}",
            gain
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_reads_input_gain() {
        let tmp = std::env::temp_dir().join("dimmy-test-gain-present");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");
        let cfg_json = serde_json::json!({
            "api_url": DEFAULT_API_URL,
            "api_model": DEFAULT_MODEL,
            "language": "en",
            "input_gain": 0.75,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg_json).unwrap()).unwrap();

        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        let gain = v["input_gain"].as_f64().unwrap_or(1.0) as f32;
        assert!(
            (gain - 0.75).abs() < 0.001,
            "input_gain should be 0.75, got {}",
            gain
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_load_roundtrip_preserves_new_fields() {
        // Save a config with custom input_gain and UI fields, then load and verify
        // Combined into one test to avoid race conditions with parallel tests
        // sharing the same config file
        let mut cfg = AppConfig::default();
        cfg.input_gain = 0.42;
        cfg.border_style = "Solid".to_string();
        cfg.waveform_style = "Line".to_string();
        cfg.overlay_position = "Top Left".to_string();
        cfg.keep_in_clipboard = true;
        save_config_file(&cfg);

        let loaded = load_config_file();
        assert!(
            (loaded.input_gain - 0.42).abs() < 0.001,
            "Roundtrip input_gain should be 0.42, got {}",
            loaded.input_gain
        );
        assert_eq!(loaded.border_style, "Solid");
        assert_eq!(loaded.waveform_style, "Line");
        assert_eq!(loaded.overlay_position, "Top Left");
        assert!(loaded.keep_in_clipboard);

        // Restore default
        save_config_file(&AppConfig::default());
    }

    #[test]
    #[should_panic(expected = "input_gain must be in [0.0, 2.0]")]
    fn save_config_rejects_input_gain_above_max() {
        let mut cfg = AppConfig::default();
        cfg.input_gain = 3.0;
        save_config_file(&cfg);
    }

    #[test]
    #[should_panic(expected = "input_gain must be in [0.0, 2.0]")]
    fn save_config_rejects_negative_input_gain() {
        let mut cfg = AppConfig::default();
        cfg.input_gain = -0.5;
        save_config_file(&cfg);
    }

    #[test]
    fn new_standalone_creates_valid_state() {
        let state = AppState::new_standalone();
        assert!(!*state.recording.lock().unwrap());
        assert!(!state.api_url.lock().unwrap().is_empty());
        assert!(!state.api_model.lock().unwrap().is_empty());
        let gain_bits = state.input_gain.load(std::sync::atomic::Ordering::Relaxed);
        let gain = f32::from_bits(gain_bits);
        assert!(
            gain >= 0.0 && gain <= 2.0,
            "gain must be in [0.0, 2.0], got {}",
            gain
        );
        assert!(gain.is_finite(), "gain must be finite, got {}", gain);
    }

    /// Regression: `crate::log` must NEVER panic, regardless of the
    /// state of stdout/stderr or filesystem. The previous implementation
    /// used `eprintln!`, which on Windows windowed apps (Velopack-launched
    /// C# host with no stderr handle) routes through `std::io::_eprint`
    /// and panics on any non-BrokenPipe write error. Hitting that panic
    /// inside `dimmy_init` aborts the cdylib via __fastfail because the
    /// extern "C" boundary is no-unwind. The fix uses a best-effort
    /// `writeln!(io::stderr(), ...)` whose error is discarded.
    #[test]
    fn log_does_not_panic_on_any_input() {
        // Plain ASCII.
        log("test message");
        // Empty.
        log("");
        // Multiline.
        log("line one\nline two\nline three");
        // UTF-8 with multi-byte chars.
        log("emoji 🎙️ è à ü");
        // Long line — exercise rotation path if we happen to hit the cap.
        log(&"x".repeat(8192));
    }
}
