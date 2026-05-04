using System;
using System.Collections.Generic;
using System.Text.Json;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

public record ProviderPreset(string Name, string Url, string DefaultModel);

public partial class SettingsViewModel : ObservableObject
{
    public static readonly List<ProviderPreset> ProviderPresets =
    [
        new("Groq", "https://api.groq.com/openai/v1/audio/transcriptions", "whisper-large-v3-turbo"),
        new("Groq-v3", "https://api.groq.com/openai/v1/audio/transcriptions", "whisper-large-v3"),
        new("Groq-Distil", "https://api.groq.com/openai/v1/audio/transcriptions", "distil-whisper-large-v3-en"),
        new("OpenAI", "https://api.openai.com/v1/audio/transcriptions", "whisper-1"),
        new("OpenAI-4o", "https://api.openai.com/v1/audio/transcriptions", "gpt-4o-transcribe"),
        new("OpenAI-4o-mini", "https://api.openai.com/v1/audio/transcriptions", "gpt-4o-mini-transcribe"),
        new("Deepgram", "https://api.deepgram.com/v1/listen", "nova-3"),
        new("Deepgram-Nova2", "https://api.deepgram.com/v1/listen", "nova-2"),
        new("Gemini", "https://generativelanguage.googleapis.com/v1beta/models", "gemini-2.5-flash"),
        new("Gemini-Pro", "https://generativelanguage.googleapis.com/v1beta/models", "gemini-2.5-pro"),
        // Phase 1 cloud expansion (2026-05-04 benchmark drove the model picks)
        new("Fireworks", "https://audio-turbo.api.fireworks.ai/v1/audio/transcriptions", "whisper-v3-turbo"),
        new("Together-Parakeet", "https://api.together.xyz/v1/audio/transcriptions", "nvidia/parakeet-tdt-0.6b-v3"),
        new("Together-Whisper", "https://api.together.xyz/v1/audio/transcriptions", "openai/whisper-large-v3"),
        new("Custom", "", ""),
    ];

    public static readonly List<ProviderPreset> LlmProviderPresets =
    [
        new("Groq", "https://api.groq.com/openai/v1/chat/completions", "llama-3.3-70b-versatile"),
        new("OpenAI", "https://api.openai.com/v1/chat/completions", "gpt-4o-mini"),
        new("OpenRouter", "https://openrouter.ai/api/v1/chat/completions", "meta-llama/llama-3.3-70b-instruct:free"),
        new("OpenRouter-Deepseek", "https://openrouter.ai/api/v1/chat/completions", "deepseek/deepseek-r1:free"),
        new("Gemini", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-2.5-flash"),
        new("Anthropic", "https://api.anthropic.com/v1/messages", "claude-haiku-4-5-20251001"),
        new("Anthropic-Sonnet", "https://api.anthropic.com/v1/messages", "claude-sonnet-4-20250514"),
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
    [ObservableProperty] private string _shortcut = "Win+Alt";
    [ObservableProperty] private string _shortcutMode = "toggle";
    [ObservableProperty] private string? _selectedDevice;
    [ObservableProperty] private List<string> _devices = [];
    [ObservableProperty] private bool _preprocessingEnabled = true;
    [ObservableProperty] private bool _chunkStreamingEnabled;
    [ObservableProperty] private bool _useKeyring = false;
    [ObservableProperty] private bool _llmEnabled;
    [ObservableProperty] private string _llmApiUrl = "";
    [ObservableProperty] private string _llmApiModel = "";
    [ObservableProperty] private bool _llmUseSameKey = true;
    [ObservableProperty] private string _llmApiKey = "";
    [ObservableProperty] private bool _hasLlmKey;
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

    // Telemetry — runtime-only for now (no persistence in config.json yet).
    // Initialised from DimmyNative state on viewmodel load; the on-change
    // partials forward toggles to the Rust core. Persistence is a separate
    // workstream (the Rust core needs config.telemetry_enabled fields).
    [ObservableProperty] private bool _telemetryEnabled = true;
    [ObservableProperty] private bool _crashReportsEnabled = true;

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
    [ObservableProperty] private bool _showInTaskbar;
    [ObservableProperty] private string _sttMode = "cloud";
    [ObservableProperty] private string _localModel = "ggml-base-q8_0.bin";
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
    public bool IsDirty => ToJson() != _snapshotJson;

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
            HasApiKey = r.TryGetProperty("has_key", out var hk) && hk.GetBoolean();
            Prompt = r.TryGetProperty("prompt", out var prompt) ? prompt.GetString() ?? "" : "";
            Shortcut = r.TryGetProperty("shortcut", out var sc) ? sc.GetString() ?? "Win+Alt" : "Win+Alt";
            ShortcutMode = r.TryGetProperty("shortcut_mode", out var sm) ? sm.GetString() ?? "toggle" : "toggle";
            SelectedDevice = r.TryGetProperty("selected_device", out var dev) ? dev.GetString() : null;
            PreprocessingEnabled = !r.TryGetProperty("preprocessing_enabled", out var pe) || pe.GetBoolean();
            ChunkStreamingEnabled = r.TryGetProperty("chunk_streaming_enabled", out var cs) && cs.GetBoolean();
            UseKeyring = false;  // Always local encrypted file, ignore stored value
            LlmEnabled = r.TryGetProperty("llm_enabled", out var le) && le.GetBoolean();
            LlmApiUrl = r.TryGetProperty("llm_api_url", out var lu) ? lu.GetString() ?? "" : "";
            LlmApiModel = r.TryGetProperty("llm_api_model", out var lm) ? lm.GetString() ?? "" : "";
            LlmUseSameKey = !r.TryGetProperty("llm_use_same_key", out var lsk) || lsk.GetBoolean();
            HasLlmKey = r.TryGetProperty("has_llm_key", out var hlk) && hlk.GetBoolean();
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
            FillerRemovalEnabled = !r.TryGetProperty("filler_removal_enabled", out var fre) || fre.GetBoolean();
            LlmMode = r.TryGetProperty("llm_mode", out var llmm) ? llmm.GetString() ?? "cloud" : "cloud";
            LocalLlmModel = r.TryGetProperty("local_llm_model", out var llmod) ? llmod.GetString() ?? "gemma-4-E2B-it-Q4_K_M.gguf" : "gemma-4-E2B-it-Q4_K_M.gguf";
            BorderStyle = r.TryGetProperty("border_style", out var bs) ? bs.GetString() ?? "Rainbow" : "Rainbow";
            WaveformStyle = r.TryGetProperty("waveform_style", out var ws) ? ws.GetString() ?? "Bars" : "Bars";
            OverlayPosition = r.TryGetProperty("overlay_position", out var op) ? op.GetString() ?? "Bottom Right" : "Bottom Right";
            Theme = r.TryGetProperty("theme", out var pt) ? pt.GetString() ?? "Default" : "Default";
            KeepInClipboard = r.TryGetProperty("keep_in_clipboard", out var kc) && kc.GetBoolean();
            InputGainPercent = r.TryGetProperty("input_gain", out var ig) ? (int)(ig.GetDouble() * 100) : 100;
            StatsTotalWords = r.TryGetProperty("stats_total_words", out var stw) ? stw.GetInt64() : 0;
            StatsTotalSpeakingSecs = r.TryGetProperty("stats_total_speaking_secs", out var sts) ? sts.GetDouble() : 0;

            if (r.TryGetProperty("devices", out var devArr) && devArr.ValueKind == JsonValueKind.Array)
            {
                var list = new List<string>();
                foreach (var d in devArr.EnumerateArray())
                    if (d.GetString() is string s) list.Add(s);
                Devices = list;
            }

            _snapshotJson = ToJson();
        }
        catch (JsonException) { }
    }

    public string ToJson()
    {
        var dict = new Dictionary<string, object?>
        {
            ["language"] = Language,
            ["llm_style"] = LlmStyle,
            ["llm_tone"] = LlmTone,
            ["api_url"] = ApiUrl,
            ["api_model"] = ApiModel,
            ["prompt"] = Prompt,
            ["shortcut"] = Shortcut,
            ["shortcut_mode"] = ShortcutMode,
            ["selected_device"] = SelectedDevice,
            ["preprocessing_enabled"] = PreprocessingEnabled,
            ["chunk_streaming_enabled"] = ChunkStreamingEnabled,
            ["use_keyring"] = false,  // Always local encrypted file
            ["llm_enabled"] = LlmStyle != "off",
            ["llm_api_url"] = LlmApiUrl,
            ["llm_api_model"] = LlmApiModel,
            ["llm_use_same_key"] = LlmUseSameKey,
            ["llm_custom_prompt"] = LlmCustomPrompt,
            ["llm_translate_to"] = LlmTranslateTo,
            ["llm_log_enabled"] = LlmLogEnabled,
            ["audio_debug_enabled"] = AudioDebugEnabled,
            ["ggml_debug_logging"] = GgmlDebugLogging,
            ["stt_mode"] = SttMode,
            ["local_model"] = LocalModel,
            ["filler_removal_enabled"] = FillerRemovalEnabled,
            ["llm_mode"] = LlmMode,
            ["local_llm_model"] = LocalLlmModel,
            ["border_style"] = BorderStyle,
            ["waveform_style"] = WaveformStyle,
            ["overlay_position"] = OverlayPosition,
            ["theme"] = Theme,
            ["keep_in_clipboard"] = KeepInClipboard,
            ["input_gain"] = InputGainPercent / 100.0,
        };
        if (!string.IsNullOrEmpty(ApiKey)) dict["api_key"] = ApiKey;
        if (!string.IsNullOrEmpty(LlmApiKey)) dict["llm_api_key"] = LlmApiKey;

        return JsonSerializer.Serialize(dict);
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
