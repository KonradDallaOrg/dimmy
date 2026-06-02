using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Text.Json;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

public record ProviderPreset(string Name, string Url, string DefaultModel);

public partial class SettingsViewModel : ObservableObject
{
    /// <summary>
    /// Temporary diagnostic logger for the app_rules persistence bug
    /// (2026-05-12). Appends to a dedicated file under config dir so
    /// the trail isn't drowned by other entries in ptt.log. Best-
    /// effort — silently ignores IO failures because diag is not
    /// load-bearing for the app to work.
    /// </summary>
    private static void DiagLog(string line)
    {
        try
        {
            var path = System.IO.Path.Combine(
                System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
                "dimmy", "app-rules-diag.log");
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            System.IO.File.AppendAllText(path,
                $"[{System.DateTime.Now:HH:mm:ss.fff}] {line}{System.Environment.NewLine}");
        }
        catch { }
    }

    public static readonly List<ProviderPreset> ProviderPresets =
    [
        new("Groq", "https://api.groq.com/openai/v1/audio/transcriptions", "whisper-large-v3-turbo"),
        new("Groq-v3", "https://api.groq.com/openai/v1/audio/transcriptions", "whisper-large-v3"),
        // "Groq-Distil" (distil-whisper-large-v3-en) removed
        // 2026-05-15 — decommissioned by Groq, returns
        // HTTP 400 model_decommissioned. Migration in
        // SettingsViewModel.LoadFromJson coerces saved
        // configs to whisper-large-v3-turbo.
        new("OpenAI", "https://api.openai.com/v1/audio/transcriptions", "whisper-1"),
        new("OpenAI-4o", "https://api.openai.com/v1/audio/transcriptions", "gpt-4o-transcribe"),
        new("OpenAI-4o-mini", "https://api.openai.com/v1/audio/transcriptions", "gpt-4o-mini-transcribe"),
        new("Deepgram", "https://api.deepgram.com/v1/listen", "nova-3"),
        new("Deepgram-Nova2", "https://api.deepgram.com/v1/listen", "nova-2"),
        // Gemini STT — Flash variants only. Pro models are slower
        // and more expensive than Flash without meaningful accuracy
        // gain for transcription; the user explicitly preferred fast
        // STT tier 2026-05-15. Pro stays available for LLM rewrite
        // + recap (interactive vs background workloads).
        new("Gemini-3.1-Flash-Lite", "https://generativelanguage.googleapis.com/v1beta/models", "gemini-3.1-flash-lite"),
        new("Gemini-3-Flash", "https://generativelanguage.googleapis.com/v1beta/models", "gemini-3-flash-preview"),
        new("Gemini-2.5-Flash", "https://generativelanguage.googleapis.com/v1beta/models", "gemini-2.5-flash"),
        // Legacy "Gemini" alias for back-compat → stable 2.5-flash.
        new("Gemini", "https://generativelanguage.googleapis.com/v1beta/models", "gemini-2.5-flash"),
        // Phase 1 cloud expansion (2026-05-04 benchmark drove the model picks)
        new("Fireworks", "https://audio-turbo.api.fireworks.ai/v1/audio/transcriptions", "whisper-v3-turbo"),
        new("Together-Parakeet", "https://api.together.xyz/v1/audio/transcriptions", "nvidia/parakeet-tdt-0.6b-v3"),
        new("Together-Whisper", "https://api.together.xyz/v1/audio/transcriptions", "openai/whisper-large-v3"),
        new("Custom", "", ""),
    ];

    public static readonly List<ProviderPreset> LlmProviderPresets =
    [
        // Curated cloud LLM presets — audit done 2026-05-15 against
        // each provider's live /models endpoint. Mirrored 1:1 on
        // Mac `LlmPreset.presets` + Linux LLM presets. Adding /
        // removing here MUST be reflected on both other platforms.
        //
        // ── Groq (free for moderate use, OpenAI-compatible) ────
        new("Groq", "https://api.groq.com/openai/v1/chat/completions", "llama-3.3-70b-versatile"),
        new("Groq-GptOss120b", "https://api.groq.com/openai/v1/chat/completions", "openai/gpt-oss-120b"),
        new("Groq-Llama4Scout", "https://api.groq.com/openai/v1/chat/completions", "meta-llama/llama-4-scout-17b-16e-instruct"),
        new("Groq-Llama8bInstant", "https://api.groq.com/openai/v1/chat/completions", "llama-3.1-8b-instant"),
        new("Groq-Qwen3-32b", "https://api.groq.com/openai/v1/chat/completions", "qwen/qwen3-32b"),
        // ── OpenAI ─────────────────────────────────────────────
        new("OpenAI", "https://api.openai.com/v1/chat/completions", "gpt-5-mini"),
        new("OpenAI-GPT5", "https://api.openai.com/v1/chat/completions", "gpt-5"),
        new("OpenAI-GPT51", "https://api.openai.com/v1/chat/completions", "gpt-5.1"),
        new("OpenAI-4o-mini", "https://api.openai.com/v1/chat/completions", "gpt-4o-mini"),
        // ── OpenRouter (free tiers for testing) ────────────────
        new("OpenRouter", "https://openrouter.ai/api/v1/chat/completions", "meta-llama/llama-3.3-70b-instruct:free"),
        new("OpenRouter-Deepseek", "https://openrouter.ai/api/v1/chat/completions", "deepseek/deepseek-r1:free"),
        // ── Gemini (full live-API spread, audit 2026-05-15) ────
        // 3.1 generation: only `-preview` and `-lite` variants exist.
        // 3 generation: only `-preview` variants exist.
        // 2.5 generation: plain names work (stable production tier).
        new("Gemini-3.1-Pro", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-3.1-pro-preview"),
        new("Gemini-3.1-Flash-Lite", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-3.1-flash-lite"),
        new("Gemini-3-Pro", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-3-pro-preview"),
        new("Gemini-3-Flash", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-3-flash-preview"),
        new("Gemini-2.5-Pro", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-2.5-pro"),
        new("Gemini-2.5-Flash", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-2.5-flash"),
        // Legacy "Gemini" alias for saved configs — falls to the
        // stable 2.5-flash tier. Older "Gemini-3.1-Pro" / -Flash
        // references in configs are migrated at load time.
        new("Gemini", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-2.5-flash"),
        // ── Anthropic ──────────────────────────────────────────
        new("Anthropic", "https://api.anthropic.com/v1/messages", "claude-haiku-4-5-20251001"),
        new("Anthropic-Sonnet", "https://api.anthropic.com/v1/messages", "claude-sonnet-4-6"),
        new("Anthropic-Opus", "https://api.anthropic.com/v1/messages", "claude-opus-4-7"),
        // The dedicated "Claude-Code" preset is gone — the same
        // routing now lives behind the Authentication radio group
        // in the Anthropic provider card (Subscription instead of
        // API key). Configs that still ship the legacy
        // claude-code://default URL are migrated in LoadConfig.
        // Phase 1 cloud expansion (2026-05-04, sensible model picks for filler-removal/smart-format)
        new("Fireworks", "https://api.fireworks.ai/inference/v1/chat/completions", "accounts/fireworks/models/kimi-k2p6"),
        new("Together-Llama", "https://api.together.xyz/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
        new("Together-Qwen", "https://api.together.xyz/v1/chat/completions", "Qwen/Qwen2.5-7B-Instruct-Turbo"),
        new("Custom", "", ""),
    ];

    public static readonly List<KeyValuePair<string, string>> Languages =
    [
        new("it", "Italiano"),
        new("en", "English"),
        new("es", "Español"),
        new("fr", "Français"),
        new("de", "Deutsch"),
        new("pt", "Português"),
    ];

    public static readonly string[] LlmStyles =
        ["off", "correct", "summarize", "elaborate", "comprehensible", "professional",
         "prompt", "genz", "boomer", "emoji", "acronyms", "imbruttito", "custom"];

    public static readonly string[] LlmTones =
        ["none", "formal", "friendly", "concise", "academic"];

    // XAML-bindable wrappers for static lists
    public List<KeyValuePair<string, string>> LanguageItems => Languages;
    public string[] LlmStyleItems => LlmStyles;
    public string[] LlmToneItems => LlmTones;

    /// <summary>Translation target list — same as Languages but with
    /// an explicit "" → "No translation" option as first item, since
    /// `llm_translate_to=""` in Rust means "keep transcript in source
    /// language, do not translate". The Settings dropdown previously
    /// used a parallel list with uppercase codes ("EN", "IT", "none")
    /// which mismatched the pill (lowercase) and required runtime
    /// normalisation in core. Now both UIs share this single list.</summary>
    public List<KeyValuePair<string, string>> TranslateToItems => TranslateTargets;
    public static readonly List<KeyValuePair<string, string>> TranslateTargets = new()
    {
        new("", "No translation"),
        new("it", "Italiano"),
        new("en", "English"),
        new("es", "Español"),
        new("fr", "Français"),
        new("de", "Deutsch"),
        new("pt", "Português"),
    };

    /// <summary>Map legacy `llm_translate_to` config values to the
    /// canonical lowercase ISO code (or "" for "no translation"). Old
    /// installs may have "EN"/"IT" or the literal string "none";
    /// normalise both so the new shared dropdown finds a match.
    /// Public for unit-testability from the Tests project.</summary>
    public static string NormaliseTranslateTo(string raw)
    {
        if (string.IsNullOrWhiteSpace(raw)) return "";
        var trimmed = raw.Trim().ToLowerInvariant();
        if (trimmed == "none") return "";
        // Accept any 2-letter code; let the LLM handle unknown ones
        // gracefully rather than silently dropping the user's choice.
        return trimmed;
    }
    public List<ProviderPreset> ProviderPresetItems => ProviderPresets;

    [ObservableProperty] private bool _isAdvanced;
    [ObservableProperty] private string _language = "";
    [ObservableProperty] private string _llmStyle = "off";
    [ObservableProperty] private string _llmTone = "none";
    [ObservableProperty] private string _apiUrl = "";
    [ObservableProperty] private string _apiModel = "";
    [ObservableProperty] private string _apiKey = "";
    [ObservableProperty] private bool _hasApiKey;
    [ObservableProperty] private string _prompt = "";
    /// <summary>User-curated vocabulary list — see core/src/lib.rs
    /// `compose_stt_prompt`. Loaded from / pushed to the Rust
    /// AppState via DictionaryService FFI; round-trips through
    /// dimmy_set_config_json too (the "user_dict" field on the
    /// snapshot is the same source of truth).</summary>
    public System.Collections.ObjectModel.ObservableCollection<string> UserDict { get; } = new();
    /// <summary>Hotkey for "add selected text to dictionary".
    /// Persisted to UiPreferences.DictHotkey (NOT config.json — it's
    /// a Win-only UI knob). Default Ctrl+Shift+D.</summary>
    [ObservableProperty] private string _dictHotkey = "ctrl+shift+d";
    /// <summary>Optional dedicated command-mode hotkey (one-shot). EMPTY =
    /// disabled; command mode still works via the pill-menu toggle. Persisted
    /// to UiPreferences.CommandHotkey (Win-only UI knob).</summary>
    [ObservableProperty] private string _commandHotkey = "";
    [ObservableProperty] private string _shortcut = "Win+Alt";
    [ObservableProperty] private string _shortcutMode = "toggle";
    [ObservableProperty] private string? _selectedDevice;
    [ObservableProperty] private List<string> _devices = [];
    [ObservableProperty] private bool _preprocessingEnabled = true;
    [ObservableProperty] private bool _chunkStreamingEnabled;
    [ObservableProperty] private bool _liveCaptionsEnabled = true;
    [ObservableProperty] private bool _callDetectEnabled = true;
    public ObservableCollection<string> CallDetectExcludedApps { get; } = new();
    [ObservableProperty] private bool _saveAudioInHistory = false;
    [ObservableProperty] private int _historyAudioKeepDays = 30;
    [ObservableProperty] private int _historyAudioMaxMb = 5_000;
    [ObservableProperty] private string _historySearchQuery = "";

    /// User-defined app rules. Round-tripped through config.json's
    /// `app_rules` array. The Rust core reads this list at LLM-enhance
    /// time and applies the first-match override to llm_style /
    /// llm_translate_to. Drag-reorder in the Settings UI maps to list
    /// order = priority.
    public ObservableCollection<AppRuleViewModel> AppRules { get; } = new();

    /// Result list for the History page. Populated lazily when the user
    /// navigates to that page (see SettingsWindow.LoadHistoryItems).
    public ObservableCollection<HistoryItemViewModel> HistoryItems { get; } = new();
    [ObservableProperty] private HistoryItemViewModel? _selectedHistoryItem;
    [ObservableProperty] private bool _useKeyring = false;
    [ObservableProperty] private bool _llmEnabled;
    [ObservableProperty] private string _llmApiUrl = "";
    [ObservableProperty] private string _llmApiModel = "";
    [ObservableProperty] private bool _llmUseSameKey = true;
    [ObservableProperty] private string _llmApiKey = "";
    [ObservableProperty] private bool _hasLlmKey;
    /// LLM authentication method — "api_key" (default, classic
    /// HTTP+key) or "subscription" (route via local `claude` CLI
    /// using the user's Anthropic Pro/Team/Max plan, no API credit
    /// consumed). The Settings RadioButton group binds here.
    [ObservableProperty] private string _llmAuthMethod = "api_key";
    /// Recap-specific auth override. Three values:
    ///   "" (default) — inherit from LlmAuthMethod
    ///   "api_key" — force HTTP+key for the recap regardless
    ///   "subscription" — force subprocess CLI for the recap regardless
    /// Lets a user run dictation rewrite via API key (fast, no
    /// subprocess cold-start tax) and meeting recap via the
    /// subscription (amortized across 30-90 s of inference).
    [ObservableProperty] private string _recapAuthMethod = "";
    /// <summary>
    /// Mirror of <see cref="LlmUseSameKey"/> at the recap layer. When
    /// the chosen recap model's vendor matches a vendor we already have
    /// a key for upstream (LLM-scope or STT-scope), should the recap
    /// reuse that key (true, default) or load a dedicated Recap-scope
    /// key the user types separately (false)?
    ///
    /// The toggle is visible in Settings → Output only when an upstream
    /// key for the derived recap vendor actually exists; otherwise it
    /// is hidden (the key field shows up directly because there's
    /// nothing to inherit from). Same pattern as the LLM section.
    /// </summary>
    [ObservableProperty] private bool _recapUseSameKey = true;
    /// <summary>
    /// Set by <c>UpdateRecapKeyCardVisibility</c> based on the
    /// `has_<recap_vendor>_recap_key` snapshot field. Drives the
    /// green ✓ badge + placeholder text on the Recap API key
    /// PasswordBox (mirror of <see cref="HasLlmKey"/>).
    /// </summary>
    [ObservableProperty] private bool _hasRecapKey;
    /// <summary>
    /// Recap-vendor key entered in the PasswordBox during this
    /// Settings session. Drained on Save / AutoSaveOnClose via
    /// `dimmy_save_llm_provider_key("recap", &lt;vendor&gt;, key)`.
    /// Never persisted in config.json (the Rust keystore is the
    /// single source of truth) — held here only between the user
    /// typing and the next save.
    /// </summary>
    [ObservableProperty] private string _recapApiKey = "";
    /// User override for the model ID used by the meeting recap LLM call.
    /// Empty = let PickRecapModel pick the provider-default flagship
    /// reasoning model (Opus 4.7 / Gemini 3.1 Pro / GPT-5).
    [ObservableProperty] private string _recapModelOverride = "";

    /// User-chosen meeting storage directory. Empty = default
    /// (&lt;config&gt;/meetings). Identity-class field: omitted from ToJson
    /// when empty so a transient VM can't wipe it; the folder picker /
    /// reset send explicit targeted updates.
    [ObservableProperty] private string _meetingStoragePath = "";

    // ── Notion integration ──────────────────────────────────────────
    /// UUID of the Notion page or database where meeting recaps land.
    /// Empty = no target picked yet (UI shows the onboarding panel).
    /// Set by the picker in the IntegrationsPanel.
    [ObservableProperty] private string _notionTargetId = "";
    /// "page" or "database" — drives the request shape sent to Notion.
    [ObservableProperty] private string _notionTargetKind = "";
    /// Display name of the picked target ("Meeting Notes",
    /// "Engineering log") shown in Settings as a confirmation.
    [ObservableProperty] private string _notionTargetTitle = "";
    /// When true, every meeting auto-sends to Notion at stop time.
    /// Default false — opt-in via explicit click. Save path goes through
    /// dimmy_set_config_json so the Rust core sees the toggle.
    [ObservableProperty] private bool _notionAutoSend;
    /// "Has the user pasted a token?" — drives the Connected/Not
    /// Connected status indicator on the Notion settings page. Sourced
    /// from `has_notion_token` field of dimmy_get_config_json snapshot
    /// at load time, kept in sync after the user pastes/clears the token
    /// via the dedicated FFI (token never round-trips through config).
    [ObservableProperty] private bool _hasNotionToken;

    /// Per-provider snapshot of "is an LLM key already stored?" — sourced from
    /// the `has_<provider>_llm_key` fields of `dimmy_get_config_json`. Used by
    /// the dropdown handler to refresh `HasLlmKey` (the green ✓ badge) without
    /// an FFI roundtrip when the user picks a different provider before saving.
    ///
    /// Bug burned 2026-05-18: the JSON field names here used to be the
    /// inverted order `has_llm_<provider>_key` which doesn't match what
    /// Rust emits in ffi.rs:1341 (`has_<provider>_llm_key`). Result: every
    /// `TryGetProperty` returned false, this dict was always all-false,
    /// `HasLlmKeyForUrl` always returned false, and the UI fell back to the
    /// session-active global flag — green check stayed on the wrong provider.
    private Dictionary<string, bool> _llmHasKeyByProvider = new();

    /// Per-provider snapshot of "is an STT key already stored?" — sourced from
    /// the `has_<provider>_key` fields of `dimmy_get_config_json` (STT scope
    /// flags, no `_llm` infix). Drives the green ✓ badge on STT provider
    /// dropdown changes. Same shape + same incident as `_llmHasKeyByProvider`
    /// — previously the STT side fell through to `has_key` (session-active
    /// global) so the green check was wrong any time the user switched
    /// providers without restarting.
    private Dictionary<string, bool> _sttHasKeyByProvider = new();

    /// Returns whether the LLM keystore has a key for the given provider URL.
    /// Mirrors `Provider::from_url` in `core/src/provider.rs` — keep in sync
    /// when adding providers.
    public bool HasLlmKeyForUrl(string url)
    {
        var key = LlmProviderKeyFromUrl(url);
        return _llmHasKeyByProvider.TryGetValue(key, out var v) && v;
    }

    /// Returns whether the STT keystore has a key for the given provider URL.
    /// Mirror of `HasLlmKeyForUrl`; STT-specific provider mapping (Deepgram +
    /// custom-URL fallback differ from the LLM list).
    public bool HasSttKeyForUrl(string url)
    {
        var key = SttProviderKeyFromUrl(url);
        return _sttHasKeyByProvider.TryGetValue(key, out var v) && v;
    }

    /// <summary>True if the encrypted keystore holds ANY key (STT or LLM) for the
    /// given vendor id (e.g. "groq", "deepgram"). Used by the Providers page to
    /// show a truthful "connected" status driven by real keys, wherever entered,
    /// rather than a cached UI mirror.</summary>
    public bool HasAnyKeyForProvider(string vendorId)
    {
        var k = (vendorId ?? string.Empty).ToLowerInvariant();
        return (_sttHasKeyByProvider.TryGetValue(k, out var s) && s)
            || (_llmHasKeyByProvider.TryGetValue(k, out var l) && l);
    }

    private static string LlmProviderKeyFromUrl(string url)
    {
        if (string.IsNullOrEmpty(url)) return "groq"; // matches Rust default
        if (url.Contains("groq.com")) return "groq";
        if (url.Contains("openai.com")) return "openai";
        if (url.Contains("openrouter.ai")) return "openrouter";
        if (url.Contains("googleapis.com")) return "gemini";
        if (url.Contains("anthropic.com")) return "anthropic";
        return "custom";
    }

    private static string SttProviderKeyFromUrl(string url)
    {
        if (string.IsNullOrEmpty(url)) return "groq"; // matches Rust default
        if (url.Contains("groq.com")) return "groq";
        if (url.Contains("openai.com")) return "openai";
        if (url.Contains("openrouter.ai")) return "openrouter";
        if (url.Contains("googleapis.com")) return "gemini";
        if (url.Contains("deepgram.com")) return "deepgram";
        return "custom";
    }
    [ObservableProperty] private string _llmCustomPrompt = "";
    [ObservableProperty] private string _llmTranslateTo = "";
    [ObservableProperty] private bool _llmLogEnabled;
    [ObservableProperty] private bool _audioDebugEnabled;
    [ObservableProperty] private bool _ggmlDebugLogging;

    /// <summary>If true, the floating pill is shown immediately at app
    /// startup. If false, Dimmy boots in "taskbar-only" mode — the
    /// pill stays hidden, recording state is surfaced only via the
    /// taskbar overlay icon (red dot + amplitude bar). Persisted in
    /// `ui_prefs.json`, not in Rust core's config.json.</summary>
    [ObservableProperty] private bool _pillShowOnStartup = true;

    /// <summary>If true, pressing the global hotkey while the pill is
    /// hidden re-shows it. If false, the hotkey records but the pill
    /// stays hidden — only the taskbar overlay reflects state.</summary>
    [ObservableProperty] private bool _pillShowOnHotkey = true;

    /// <summary>If true, Dimmy registers a button on the Windows taskbar
    /// (`TaskbarAnchorWindow`) with brand icon + amplitude overlay during
    /// recording + jump-list shortcuts. False hides the taskbar button
    /// while keeping system-tray + hotkey active. Persisted in
    /// `ui_prefs.json`. Default true.</summary>
    [ObservableProperty] private bool _showTaskbarIcon = true;

    // Telemetry — runtime-only for now (no persistence in config.json yet).
    // Initialised from DimmyNative state on viewmodel load; the on-change
    // partials forward toggles to the Rust core. Persistence is a separate
    // workstream (the Rust core needs config.telemetry_enabled fields).
    [ObservableProperty] private bool _telemetryEnabled = true;
    [ObservableProperty] private bool _crashReportsEnabled = true;

    /// Auto-refresh `HasApiKey` (the STT green ✓ badge) when the user
    /// switches providers via the URL dropdown, without waiting for a
    /// save+reload roundtrip. The per-provider dict was populated on the
    /// last `LoadFromJson`, so a URL change just rebinds to the new
    /// provider's entry. Mirror of the explicit `HasLlmKeyForUrl(url)`
    /// call inside the LLM provider dropdown handler — but as a
    /// partial-method it covers ALL paths that mutate `ApiUrl` (preset
    /// click, typed entry, programmatic save).
    partial void OnApiUrlChanged(string value)
    {
        HasApiKey = HasSttKeyForUrl(value);
    }

    partial void OnLlmApiUrlChanged(string value)
    {
        HasLlmKey = HasLlmKeyForUrl(value);
    }

    partial void OnTelemetryEnabledChanged(bool value)
    {
        try { Interop.DimmyNative.TelemetryEnabled = value; } catch { }
    }

    partial void OnCrashReportsEnabledChanged(bool value)
    {
        try { Interop.DimmyNative.CrashReportsEnabled = value; } catch { }
    }

    // Autostart — read at viewmodel load from the Rust core's actual
    // OS-level state (HKCU\…\Run presence on Windows). The on-change
    // partial forwards the new value through the FFI; if the FFI
    // throws (registry write denied etc.), we revert the property to
    // the actually-applied value so the UI matches reality. The
    // `_supressAutostartCallback` flag breaks the recursion that
    // would otherwise loop on the revert.
    [ObservableProperty] private bool _autostartEnabled;
    private bool _supressAutostartCallback;

    partial void OnAutostartEnabledChanged(bool value)
    {
        if (_supressAutostartCallback) return;
        try
        {
            Interop.DimmyNative.AutostartEnabled = value;
        }
        catch
        {
            var actual = Interop.DimmyNative.AutostartEnabled;
            if (actual != value)
            {
                _supressAutostartCallback = true;
                try { AutostartEnabled = actual; }
                finally { _supressAutostartCallback = false; }
            }
        }
    }
    // GPU known-bad surface (read-only in the UI; populated by LoadGpuStatus).
    [ObservableProperty] private bool _gpuKnownBad;
    [ObservableProperty] private string _gpuKnownBadSince = "";
    [ObservableProperty] private string _gpuKnownBadContext = "";
    [ObservableProperty] private bool _gpuFingerprintMatches;
    [ObservableProperty] private string _borderStyle = "Rainbow";
    [ObservableProperty] private string _waveformStyle = "Bars";
    [ObservableProperty] private string _overlayPosition = "Bottom Right";
    [ObservableProperty] private string _theme = "Default";
    [ObservableProperty] private bool _keepInClipboard;
    [ObservableProperty] private int _inputGainPercent = 100;
    [ObservableProperty] private string _audioSource = "mic";
    [ObservableProperty] private bool _showInTaskbar;
    [ObservableProperty] private string _sttMode = "cloud";
    [ObservableProperty] private string _localModel = "ggml-base-q8_0.bin";
    [ObservableProperty] private string _localSttBackend = "whisper";
    [ObservableProperty] private bool _fillerRemovalEnabled = true;
    [ObservableProperty] private string _llmMode = "cloud";
    [ObservableProperty] private string _localLlmModel = "gemma-4-E2B-it-Q4_K_M.gguf";
    [ObservableProperty] private long _statsTotalWords;
    [ObservableProperty] private double _statsTotalSpeakingSecs;

    // Time saved: typing ~40 WPM vs dictation ~150 WPM
    // saved = (words / 40 - words / 150) * 60 seconds
    public double TimeSavedEstimate => StatsTotalWords * (1.0 / 40 - 1.0 / 150) * 60;

    /// <summary>Human-friendly "12m 34s" / "3h 21m" / "—" formatting of
    /// TimeSavedEstimate, for the Home panel quick-stats card. Updated
    /// automatically when StatsTotalWords changes via OnStatsTotalWordsChanged
    /// below (CommunityToolkit.Mvvm partial).</summary>
    public string TimeSavedDisplay
    {
        get
        {
            var seconds = TimeSavedEstimate;
            if (seconds < 1) return "—";
            if (seconds < 60) return $"{(int)seconds}s";
            var minutes = (int)(seconds / 60);
            if (minutes < 60) return $"{minutes}m";
            var hours = minutes / 60;
            var rem = minutes % 60;
            return rem == 0 ? $"{hours}h" : $"{hours}h {rem}m";
        }
    }

    /// <summary>Human-friendly total speaking time for the Home stats card.
    /// Reads `stats_total_speaking_secs` accumulated by Rust on each
    /// successful transcription.</summary>
    public string SpeakingTimeDisplay
    {
        get
        {
            var seconds = StatsTotalSpeakingSecs;
            if (seconds < 1) return "—";
            if (seconds < 60) return $"{(int)seconds}s";
            var minutes = (int)(seconds / 60);
            if (minutes < 60) return $"{minutes}m";
            var hours = minutes / 60;
            var rem = minutes % 60;
            return rem == 0 ? $"{hours}h" : $"{hours}h {rem}m";
        }
    }

    partial void OnStatsTotalWordsChanged(long value)
    {
        OnPropertyChanged(nameof(TimeSavedEstimate));
        OnPropertyChanged(nameof(TimeSavedDisplay));
    }

    partial void OnStatsTotalSpeakingSecsChanged(double value)
    {
        OnPropertyChanged(nameof(SpeakingTimeDisplay));
    }

    private string _snapshotJson = "";
    // IsDirty must see EVERY field — even the ones gated out of the
    // default ToJson() (Notion/LLM/Recap blocks). Otherwise a user
    // editing only an LLM field would have IsDirty stay false → Save
    // button stays disabled → change silently lost. Snapshot is
    // captured with the same full-include flags at load time so the
    // comparison is symmetric.
    public bool IsDirty => ToJsonFull() != _snapshotJson;

    /// Full-include serialization — used ONLY for IsDirty tracking
    /// and snapshot capture. NEVER call this for FFI writes (it would
    /// re-introduce the wipe bug the includeLlm/includeRecap gates
    /// are designed to prevent).
    private string ToJsonFull() => ToJson(includeNotion: true, includeLlm: true, includeRecap: true);

    public void LoadFromJson(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var r = doc.RootElement;

            Language = r.TryGetProperty("language", out var lang) ? lang.GetString() ?? "" : "";
            LlmStyle = r.TryGetProperty("llm_style", out var style) ? style.GetString() ?? "off" : "off";
            LlmTone = r.TryGetProperty("llm_tone", out var tone) ? tone.GetString() ?? "none" : "none";
            ApiUrl = r.TryGetProperty("api_url", out var url) ? url.GetString() ?? "" : "";
            ApiModel = r.TryGetProperty("api_model", out var model) ? model.GetString() ?? "" : "";
            // Decommissioned-model migration (mirror of Rust
            // `migrate_decommissioned_models`). Each rule = one
            // upstream API regression audited 2026-05-15.
            if (ApiModel == "distil-whisper-large-v3-en"
                && ApiUrl.Contains("groq.com", StringComparison.OrdinalIgnoreCase))
            {
                // Groq decommissioned (HTTP 400 model_decommissioned).
                ApiModel = "whisper-large-v3-turbo";
            }
            if (ApiUrl.Contains("googleapis.com", StringComparison.OrdinalIgnoreCase))
            {
                if (ApiModel == "gemini-3.1-flash") ApiModel = "gemini-3.1-flash-lite";
                // STT Pro → Flash (Pro is LLM-only now, UX policy 2026-05-15).
                if (ApiModel == "gemini-3.1-pro" || ApiModel == "gemini-3.1-pro-preview")
                    ApiModel = "gemini-3.1-flash-lite";
                if (ApiModel == "gemini-3-pro-preview") ApiModel = "gemini-3-flash-preview";
                if (ApiModel == "gemini-2.5-pro") ApiModel = "gemini-2.5-flash";
            }
            // Per-provider STT key snapshot. Drives the green ✓ badge in real
            // time when the user switches providers in the dropdown. The
            // legacy `has_key` field (session-active global) is ignored — it
            // produced the "phantom key saved" bug where the badge stayed on
            // for any provider once any key existed in the session. The keys
            // here MUST match the Rust emitter exactly — see ffi.rs ~1330.
            _sttHasKeyByProvider = new Dictionary<string, bool>
            {
                ["groq"] = r.TryGetProperty("has_groq_key", out var hsg) && hsg.GetBoolean(),
                ["openai"] = r.TryGetProperty("has_openai_key", out var hso) && hso.GetBoolean(),
                ["openrouter"] = r.TryGetProperty("has_openrouter_key", out var hsor) && hsor.GetBoolean(),
                ["gemini"] = r.TryGetProperty("has_gemini_key", out var hsge) && hsge.GetBoolean(),
                ["deepgram"] = r.TryGetProperty("has_deepgram_key", out var hsd) && hsd.GetBoolean(),
                ["fireworks"] = r.TryGetProperty("has_fireworks_key", out var hsf) && hsf.GetBoolean(),
                ["together"] = r.TryGetProperty("has_together_key", out var hst) && hst.GetBoolean(),
                ["custom"] = r.TryGetProperty("has_custom_key", out var hsc) && hsc.GetBoolean(),
            };
            HasApiKey = HasSttKeyForUrl(ApiUrl);
            Prompt = r.TryGetProperty("prompt", out var prompt) ? prompt.GetString() ?? "" : "";
            Shortcut = r.TryGetProperty("shortcut", out var sc) ? sc.GetString() ?? "Win+Alt" : "Win+Alt";
            ShortcutMode = r.TryGetProperty("shortcut_mode", out var sm) ? sm.GetString() ?? "toggle" : "toggle";
            SelectedDevice = r.TryGetProperty("selected_device", out var dev) ? dev.GetString() : null;
            PreprocessingEnabled = !r.TryGetProperty("preprocessing_enabled", out var pe) || pe.GetBoolean();
            ChunkStreamingEnabled = r.TryGetProperty("chunk_streaming_enabled", out var cs) && cs.GetBoolean();
            LiveCaptionsEnabled = !r.TryGetProperty("live_captions_enabled", out var lce) || lce.GetBoolean();
            CallDetectEnabled = !r.TryGetProperty("call_detect_enabled", out var cde) || cde.GetBoolean();
            CallDetectExcludedApps.Clear();
            if (r.TryGetProperty("call_detect_excluded_apps", out var cdex)
                && cdex.ValueKind == System.Text.Json.JsonValueKind.Array)
            {
                foreach (var item in cdex.EnumerateArray())
                {
                    var s = item.GetString();
                    if (!string.IsNullOrEmpty(s))
                        CallDetectExcludedApps.Add(s);
                }
            }
            SaveAudioInHistory = r.TryGetProperty("save_audio_in_history", out var sah) && sah.GetBoolean();
            HistoryAudioKeepDays = r.TryGetProperty("history_audio_keep_days", out var hkd) ? hkd.GetInt32() : 30;
            HistoryAudioMaxMb = r.TryGetProperty("history_audio_max_mb", out var hmm) ? hmm.GetInt32() : 5_000;
            LoadAppRulesFromJson(r);
            UseKeyring = false;  // Always local encrypted file, ignore stored value
            LlmEnabled = r.TryGetProperty("llm_enabled", out var le) && le.GetBoolean();
            LlmApiUrl = r.TryGetProperty("llm_api_url", out var lu) ? lu.GetString() ?? "" : "";
            LlmApiModel = r.TryGetProperty("llm_api_model", out var lm) ? lm.GetString() ?? "" : "";
            // LLM-side decommissioned-model migration. Mirror of
            // Rust `migrate_decommissioned_models`.
            if (LlmApiUrl.Contains("googleapis.com", StringComparison.OrdinalIgnoreCase))
            {
                if (LlmApiModel == "gemini-3.1-flash") LlmApiModel = "gemini-3.1-flash-lite";
                if (LlmApiModel == "gemini-3.1-pro") LlmApiModel = "gemini-3.1-pro-preview";
            }
            if (LlmApiUrl.Contains("anthropic.com", StringComparison.OrdinalIgnoreCase)
                && LlmApiModel == "claude-sonnet-4-20250514")
            {
                // Upgrade dated Sonnet 4 to the named-tier successor.
                LlmApiModel = "claude-sonnet-4-6";
            }
            LlmUseSameKey = !r.TryGetProperty("llm_use_same_key", out var lsk) || lsk.GetBoolean();
            // Auth-method flag — "api_key" (default) or "subscription".
            // Migrate the legacy `claude-code://` URL scheme to the
            // explicit Anthropic + subscription pair so saved configs
            // from the first iteration land cleanly in the new UI.
            var savedAuth = r.TryGetProperty("llm_auth_method", out var lam)
                ? lam.GetString() ?? "api_key" : "api_key";
            if (LlmApiUrl.StartsWith("claude-code://", StringComparison.Ordinal))
            {
                LlmApiUrl = "https://api.anthropic.com/v1/messages";
                savedAuth = "subscription";
            }
            LlmAuthMethod = savedAuth == "subscription" ? "subscription" : "api_key";
            // Recap-specific override is a free string: "" = inherit,
            // or one of the two explicit values. The radio group
            // normalizes invalid input back to "" so the dispatcher
            // always sees a known token.
            var savedRecapAuth = r.TryGetProperty("recap_auth_method", out var ram)
                ? ram.GetString() ?? "" : "";
            RecapAuthMethod = savedRecapAuth switch
            {
                "api_key" or "subscription" => savedRecapAuth,
                _ => "",
            };
            RecapUseSameKey = !r.TryGetProperty("recap_use_same_key", out var rusk)
                || rusk.ValueKind != System.Text.Json.JsonValueKind.False;
            RecapModelOverride = r.TryGetProperty("recap_model_override", out var rmo)
                ? rmo.GetString() ?? "" : "";
            // Migrate stale Gemini recap ids saved by older builds — the bare
            // "gemini-3.1-pro" 404s ("model not found"); the valid id carries
            // the -preview suffix. Mirrors the LlmApiModel migration above.
            RecapModelOverride = RecapModelOverride switch
            {
                "gemini-3.1-pro" => "gemini-3.1-pro-preview",
                "gemini-3-1-pro" => "gemini-3.1-pro-preview",
                "gemini-3-pro" => "gemini-3-pro-preview",
                _ => RecapModelOverride,
            };
            MeetingStoragePath = r.TryGetProperty("meeting_storage_path", out var msp)
                ? msp.GetString() ?? "" : "";
            // Notion target + auto-send flag round-trip through config.
            // The token itself never appears in the config snapshot —
            // we read its presence via has_notion_token (set by the
            // Rust core when generating the snapshot) and load/store
            // the value via the dedicated FFI.
            NotionTargetId = r.TryGetProperty("notion_target_id", out var nti)
                ? nti.GetString() ?? "" : "";
            NotionTargetKind = r.TryGetProperty("notion_target_kind", out var ntk)
                ? ntk.GetString() ?? "" : "";
            NotionTargetTitle = r.TryGetProperty("notion_target_title", out var ntt)
                ? ntt.GetString() ?? "" : "";
            NotionAutoSend = r.TryGetProperty("notion_auto_send", out var nas) && nas.GetBoolean();
            HasNotionToken = r.TryGetProperty("has_notion_token", out var hnt) && hnt.GetBoolean();
            // Per-provider LLM key snapshot. JSON field names MUST match the
            // Rust emitter in ffi.rs:1341 — `has_<provider>_llm_key`, NOT the
            // inverted `has_llm_<provider>_key` that was here pre-2026-05-18.
            // The old names returned false for every TryGetProperty, the dict
            // was all-false, HasLlmKeyForUrl always returned false, and the
            // UI ended up reading the legacy `has_llm_key` global flag below
            // — same "phantom key saved" symptom as the STT side.
            _llmHasKeyByProvider = new Dictionary<string, bool>
            {
                ["groq"] = r.TryGetProperty("has_groq_llm_key", out var hlg) && hlg.GetBoolean(),
                ["openai"] = r.TryGetProperty("has_openai_llm_key", out var hlo) && hlo.GetBoolean(),
                ["anthropic"] = r.TryGetProperty("has_anthropic_llm_key", out var hla) && hla.GetBoolean(),
                ["gemini"] = r.TryGetProperty("has_gemini_llm_key", out var hlge) && hlge.GetBoolean(),
                ["openrouter"] = r.TryGetProperty("has_openrouter_llm_key", out var hlor) && hlor.GetBoolean(),
                ["fireworks"] = r.TryGetProperty("has_fireworks_llm_key", out var hlf) && hlf.GetBoolean(),
                ["together"] = r.TryGetProperty("has_together_llm_key", out var hlt) && hlt.GetBoolean(),
                ["custom"] = r.TryGetProperty("has_custom_llm_key", out var hlc) && hlc.GetBoolean(),
            };
            // Initial green ✓ for LLM — derived from the per-provider dict via
            // the current LLM URL, NOT from the legacy `has_llm_key` session
            // global (which stayed `true` for any provider once any LLM key
            // had been used in the session).
            HasLlmKey = HasLlmKeyForUrl(LlmApiUrl);
            LlmCustomPrompt = r.TryGetProperty("llm_custom_prompt", out var lcp) ? lcp.GetString() ?? "" : "";
            // Normalise legacy values: pre-V19 the dropdown used uppercase
            // codes ("EN", "IT") plus the string "none"; the pill used
            // lowercase ("en", "it"). Both surfaces now share lowercase
            // codes + "" for no translation, so collapse on read.
            LlmTranslateTo = NormaliseTranslateTo(
                r.TryGetProperty("llm_translate_to", out var lt) ? lt.GetString() ?? "" : ""
            );
            LlmLogEnabled = r.TryGetProperty("llm_log_enabled", out var lle) && lle.GetBoolean();
            AudioDebugEnabled = r.TryGetProperty("audio_debug_enabled", out var ade) && ade.GetBoolean();
            GgmlDebugLogging = r.TryGetProperty("ggml_debug_logging", out var gdl) && gdl.GetBoolean();
            // Telemetry toggles read directly from Rust core (runtime state),
            // not from the JSON — they're not persisted yet.
            try
            {
                TelemetryEnabled = Interop.DimmyNative.TelemetryEnabled;
                CrashReportsEnabled = Interop.DimmyNative.CrashReportsEnabled;
                // Autostart is read from the actual OS state (HKCU\…\Run on
                // Windows etc.), not from config.json — it's not a config
                // setting we own, it's an OS integration we observe.
                AutostartEnabled = Interop.DimmyNative.AutostartEnabled;
            }
            catch { /* DLL maybe missing in test/headless context */ }
            SttMode = r.TryGetProperty("stt_mode", out var sm2) ? sm2.GetString() ?? "cloud" : "cloud";
            LocalModel = r.TryGetProperty("local_model", out var lmod) ? lmod.GetString() ?? "ggml-base-q8_0.bin" : "ggml-base-q8_0.bin";
            LocalSttBackend = r.TryGetProperty("local_stt_backend", out var lsb) ? lsb.GetString() ?? "whisper" : "whisper";
            FillerRemovalEnabled = !r.TryGetProperty("filler_removal_enabled", out var fre) || fre.GetBoolean();
            LlmMode = r.TryGetProperty("llm_mode", out var llmm) ? llmm.GetString() ?? "cloud" : "cloud";
            LocalLlmModel = r.TryGetProperty("local_llm_model", out var llmod) ? llmod.GetString() ?? "gemma-4-E2B-it-Q4_K_M.gguf" : "gemma-4-E2B-it-Q4_K_M.gguf";
            BorderStyle = r.TryGetProperty("border_style", out var bs) ? bs.GetString() ?? "Rainbow" : "Rainbow";
            WaveformStyle = r.TryGetProperty("waveform_style", out var ws) ? ws.GetString() ?? "Bars" : "Bars";
            OverlayPosition = r.TryGetProperty("overlay_position", out var op) ? op.GetString() ?? "Bottom Right" : "Bottom Right";
            Theme = r.TryGetProperty("theme", out var pt) ? pt.GetString() ?? "Default" : "Default";
            KeepInClipboard = r.TryGetProperty("keep_in_clipboard", out var kc) && kc.GetBoolean();
            InputGainPercent = r.TryGetProperty("input_gain", out var ig) ? (int)(ig.GetDouble() * 100) : 100;
            AudioSource = r.TryGetProperty("audio_source", out var asrc) ? asrc.GetString() ?? "mic" : "mic";
            StatsTotalWords = r.TryGetProperty("stats_total_words", out var stw) ? stw.GetInt64() : 0;
            StatsTotalSpeakingSecs = r.TryGetProperty("stats_total_speaking_secs", out var sts) ? sts.GetDouble() : 0;

            if (r.TryGetProperty("devices", out var devArr) && devArr.ValueKind == JsonValueKind.Array)
            {
                var list = new List<string>();
                foreach (var d in devArr.EnumerateArray())
                    if (d.GetString() is string s) list.Add(s);
                Devices = list;
            }

            _snapshotJson = ToJsonFull();
        }
        catch (JsonException) { }
    }

    /// <summary>
    /// Serialize the Settings ViewModel to a JSON payload for
    /// <c>dimmy_set_config_json</c>.
    ///
    /// <para><b>Notion fields are EXCLUDED by default.</b> The Rust
    /// dispatcher treats an empty <c>notion_target_id</c> as
    /// "clear destination" — so if the VM had the field empty for
    /// any reason (race, partial load, schema migration), the
    /// generic Settings save would wipe a perfectly valid Notion
    /// destination from disk on every Save_Click. Burned 2026-05-13
    /// when a meeting recap kept failing with "destination not
    /// configured" right after a Settings save.</para>
    ///
    /// <para>To explicitly set or clear the Notion destination, the
    /// Notion picker dialog calls <c>ToJson(includeNotion: true)</c>
    /// AFTER assigning <c>NotionTargetId/Kind/Title</c>; the
    /// Disconnect path does the same. Generic Settings saves never
    /// touch these fields.</para>
    /// </summary>
    public string ToJson(bool includeNotion = false, bool includeLlm = false, bool includeRecap = false)
    {
        // Universal fields — preferences, cosmetics, debug toggles, audio
        // pipeline knobs. Safe to round-trip on EVERY save site because
        // they're either user preferences (re-pickable from any UI) or
        // booleans with sensible defaults. None of these is a "credential
        // / identity" field whose wipe would silently break the user.
        // STT identity fields (api_url, api_model, selected_device,
        // local_model) live BELOW this dict with the same `if-empty-omit`
        // protection used for ApiKey/LlmApiKey — see comment + 2026-05-16
        // incident below.
        var dict = new Dictionary<string, object?>
        {
            ["language"] = Language,
            ["llm_style"] = LlmStyle,
            ["llm_tone"] = LlmTone,
            ["prompt"] = Prompt,
            ["shortcut"] = Shortcut,
            ["shortcut_mode"] = ShortcutMode,
            ["preprocessing_enabled"] = PreprocessingEnabled,
            ["chunk_streaming_enabled"] = ChunkStreamingEnabled,
            ["live_captions_enabled"] = LiveCaptionsEnabled,
            ["call_detect_enabled"] = CallDetectEnabled,
            ["call_detect_excluded_apps"] = CallDetectExcludedApps.ToList(),
            ["save_audio_in_history"] = SaveAudioInHistory,
            ["history_audio_keep_days"] = HistoryAudioKeepDays,
            ["history_audio_max_mb"] = HistoryAudioMaxMb,
            ["use_keyring"] = false,  // Always local encrypted file
            // notion_target_{id,kind,title} are deliberately NOT in
            // the generic dict — see the includeNotion docstring above.
            // notion_auto_send IS safe to round-trip (it's a bool
            // toggle, not a load-bearing identifier).
            ["notion_auto_send"] = NotionAutoSend,
            ["llm_custom_prompt"] = LlmCustomPrompt,
            ["llm_translate_to"] = LlmTranslateTo,
            ["llm_log_enabled"] = LlmLogEnabled,
            ["audio_debug_enabled"] = AudioDebugEnabled,
            ["ggml_debug_logging"] = GgmlDebugLogging,
            ["stt_mode"] = SttMode,
            ["local_stt_backend"] = LocalSttBackend,
            ["filler_removal_enabled"] = FillerRemovalEnabled,
            ["border_style"] = BorderStyle,
            ["waveform_style"] = WaveformStyle,
            ["overlay_position"] = OverlayPosition,
            ["theme"] = Theme,
            ["keep_in_clipboard"] = KeepInClipboard,
            ["input_gain"] = InputGainPercent / 100.0,
            ["audio_source"] = AudioSource,
        };
        // ── Identity + credential fields — omit when empty ─────────────
        // Wipe protection for fields where empty VM means "transient
        // page state, don't touch disk" rather than "user wants to
        // clear this". The Rust side reads each key only if PRESENT,
        // so omitting preserves whatever was previously saved.
        // Incident 2026-05-16: a save triggered from the Recap page
        // (RecapModel_SelectionChanged) reached ToJson with a
        // partially-populated ViewModel; api_url + llm_api_url were
        // emitted as "" and the Rust core dutifully cleared them on
        // disk, requiring a manual config.json restore from a log
        // bisect. The pre-existing ApiKey/LlmApiKey lines below were
        // already on this pattern; this extends it to the URL +
        // model identity fields that PR #60 acknowledged as a
        // "follow-up" risk.
        if (!string.IsNullOrEmpty(ApiUrl)) dict["api_url"] = ApiUrl;
        if (!string.IsNullOrEmpty(ApiModel)) dict["api_model"] = ApiModel;
        if (!string.IsNullOrEmpty(SelectedDevice)) dict["selected_device"] = SelectedDevice;
        if (!string.IsNullOrEmpty(LocalModel)) dict["local_model"] = LocalModel;
        if (!string.IsNullOrEmpty(ApiKey)) dict["api_key"] = ApiKey;
        if (!string.IsNullOrEmpty(LlmApiKey)) dict["llm_api_key"] = LlmApiKey;
        // Meeting storage override — identity-class path. Omit when empty
        // so a transient VM never wipes the saved dir. An explicit clear
        // (Reset to default) is sent as a targeted {"meeting_storage_path":""}
        // by the folder-picker code-behind, not through this path.
        if (!string.IsNullOrEmpty(MeetingStoragePath)) dict["meeting_storage_path"] = MeetingStoragePath;
        if (includeLlm)
        {
            // Caller is the LLM-provider page Save / AutoSave — they own
            // the semantics of "the user touched the LLM config and wants
            // to persist exactly what's in the VM, even if empty". Other
            // callers MUST NOT pass true: a transient empty VM (window
            // just opened, fields not yet populated by LoadFromJson)
            // would otherwise wipe a valid LLM setup from disk. Pattern
            // mirrors the includeNotion fix shipped 2026-05-13 after the
            // exact same bug pattern destroyed Notion destinations.
            //
            // Defense in depth (same 2026-05-16 incident as the STT
            // fields above): even inside `includeLlm:true`, omit the
            // URL + model when empty. The transient-VM scenario that
            // triggered the STT wipe applies symmetrically here —
            // including `"llm_api_url": ""` would still clear the
            // saved provider on disk.
            if (!string.IsNullOrEmpty(LlmApiUrl)) dict["llm_api_url"] = LlmApiUrl;
            if (!string.IsNullOrEmpty(LlmApiModel)) dict["llm_api_model"] = LlmApiModel;
            dict["llm_use_same_key"] = LlmUseSameKey;
            dict["llm_auth_method"] = LlmAuthMethod;
            dict["llm_mode"] = LlmMode;
            if (!string.IsNullOrEmpty(LocalLlmModel)) dict["local_llm_model"] = LocalLlmModel;
            // `llm_enabled` is derived from `llm_style != "off"` — keep
            // it under the same gate so a non-LLM-page save can't
            // accidentally flip the kill switch.
            dict["llm_enabled"] = LlmStyle != "off";
        }
        if (includeRecap)
        {
            // Same rationale as includeLlm — the Recap section in
            // Settings → Output owns these fields; other call sites
            // MUST omit them to avoid wiping a valid recap setup.
            dict["recap_auth_method"] = RecapAuthMethod;
            dict["recap_use_same_key"] = RecapUseSameKey;
            dict["recap_model_override"] = RecapModelOverride;
        }
        if (includeNotion)
        {
            // Caller is the Notion picker/disconnect — they own the
            // semantics of "clearing means clear" and "setting means set".
            dict["notion_target_id"] = NotionTargetId;
            dict["notion_target_kind"] = NotionTargetKind;
            dict["notion_target_title"] = NotionTargetTitle;
        }

        // app_rules — serialized as a JSON array matching the Rust
        // `Vec<AppRule>` shape. Empty translate is encoded as null
        // (semantically distinct from "" which means "force off").
        var rules = new List<Dictionary<string, object?>>();
        foreach (var r in AppRules)
        {
            rules.Add(new Dictionary<string, object?>
            {
                ["match_pattern"] = r.MatchPattern,
                ["match_type"] = r.MatchType,
                ["llm_style"] = r.LlmStyle,
                ["llm_translate_to"] = string.IsNullOrEmpty(r.LlmTranslateTo) ? null : r.LlmTranslateTo,
                ["label"] = r.Label,
                ["enabled"] = r.Enabled,
            });
        }
        dict["app_rules"] = rules;
        DiagLog($"[AppRulesDiag] ToJson: serializing app_rules with {rules.Count} entries (ViewModel.AppRules.Count={AppRules.Count})");

        return JsonSerializer.Serialize(dict);
    }

    private void LoadAppRulesFromJson(JsonElement r)
    {
        AppRules.Clear();
        if (!r.TryGetProperty("app_rules", out var arr) || arr.ValueKind != JsonValueKind.Array)
        {
            DiagLog("[AppRulesDiag] LoadAppRulesFromJson: no app_rules key OR not array — AppRules left empty");
            return;
        }
        DiagLog($"[AppRulesDiag] LoadAppRulesFromJson: loading {arr.GetArrayLength()} rules from JSON");
        foreach (var el in arr.EnumerateArray())
        {
            var pattern = el.TryGetProperty("match_pattern", out var p) ? p.GetString() ?? "" : "";
            var matchType = el.TryGetProperty("match_type", out var mt) ? mt.GetString() ?? "process_name" : "process_name";
            var style = el.TryGetProperty("llm_style", out var s) ? s.GetString() ?? "off" : "off";
            string translate = "";
            if (el.TryGetProperty("llm_translate_to", out var tt) && tt.ValueKind == JsonValueKind.String)
                translate = tt.GetString() ?? "";
            var label = el.TryGetProperty("label", out var l) ? l.GetString() ?? "" : "";
            var enabled = !el.TryGetProperty("enabled", out var en) || en.GetBoolean();
            AppRules.Add(new AppRuleViewModel(pattern, matchType, style, translate, label, enabled));
        }
    }

    /// Drop the current rule list and load the v1 defaults bundled with
    /// the app. Used by the "Load defaults" button. Designed to be safe
    /// to call repeatedly — replaces, doesn't merge — because users who
    /// click it a second time after editing usually want a clean reset.
    /// Future versions will introduce a "Sync v2 defaults" button that
    /// merges by pattern, leaving custom edits alone.
    public void LoadAppRulesDefaults()
    {
        AppRules.Clear();
        foreach (var r in AppRulesDefaults.V1Windows)
            AppRules.Add(r);
    }

    /// <summary>
    /// Pull the current GPU known-bad state from Rust and populate the
    /// Gpu* properties so the Debug panel can render the status block.
    /// Safe to call from the UI thread; it's a single FFI read.
    /// </summary>
    public void LoadGpuStatus()
    {
        var json = Dimmy.Windows.Interop.DimmyNative.GpuGetStatus();
        if (string.IsNullOrEmpty(json)) return;
        try
        {
            using var doc = JsonDocument.Parse(json);
            var r = doc.RootElement;
            GpuKnownBad = r.TryGetProperty("known_bad", out var kb) && kb.GetBoolean();
            GpuKnownBadSince = r.TryGetProperty("timestamp", out var ts) && ts.ValueKind == JsonValueKind.String
                ? ts.GetString() ?? "" : "";
            GpuKnownBadContext = r.TryGetProperty("context", out var ctx) && ctx.ValueKind == JsonValueKind.String
                ? ctx.GetString() ?? "" : "";
            GpuFingerprintMatches = r.TryGetProperty("fingerprint_matches", out var fm)
                && fm.ValueKind == JsonValueKind.True;
        }
        catch (JsonException) { }
    }
}
