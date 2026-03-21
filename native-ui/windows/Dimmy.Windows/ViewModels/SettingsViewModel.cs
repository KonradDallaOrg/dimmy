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
        new("OpenAI", "https://api.openai.com/v1/audio/transcriptions", "whisper-1"),
        new("Deepgram", "https://api.deepgram.com/v1/listen", "nova-3"),
        new("Gemini", "https://generativelanguage.googleapis.com/v1beta/models", "gemini-2.0-flash"),
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
    [ObservableProperty] private bool _useKeyring = true;
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
    [ObservableProperty] private long _statsTotalWords;
    [ObservableProperty] private double _statsTotalSpeakingSecs;

    public double TimeSavedEstimate => StatsTotalSpeakingSecs * 3;

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
            UseKeyring = !r.TryGetProperty("use_keyring", out var uk) || uk.GetBoolean();
            LlmEnabled = r.TryGetProperty("llm_enabled", out var le) && le.GetBoolean();
            LlmApiUrl = r.TryGetProperty("llm_api_url", out var lu) ? lu.GetString() ?? "" : "";
            LlmApiModel = r.TryGetProperty("llm_api_model", out var lm) ? lm.GetString() ?? "" : "";
            LlmUseSameKey = !r.TryGetProperty("llm_use_same_key", out var lsk) || lsk.GetBoolean();
            HasLlmKey = r.TryGetProperty("has_llm_key", out var hlk) && hlk.GetBoolean();
            LlmCustomPrompt = r.TryGetProperty("llm_custom_prompt", out var lcp) ? lcp.GetString() ?? "" : "";
            LlmTranslateTo = r.TryGetProperty("llm_translate_to", out var lt) ? lt.GetString() ?? "" : "";
            LlmLogEnabled = r.TryGetProperty("llm_log_enabled", out var lle) && lle.GetBoolean();
            AudioDebugEnabled = r.TryGetProperty("audio_debug_enabled", out var ade) && ade.GetBoolean();
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
            ["use_keyring"] = UseKeyring,
            ["llm_enabled"] = LlmEnabled,
            ["llm_api_url"] = LlmApiUrl,
            ["llm_api_model"] = LlmApiModel,
            ["llm_use_same_key"] = LlmUseSameKey,
            ["llm_custom_prompt"] = LlmCustomPrompt,
            ["llm_translate_to"] = LlmTranslateTo,
            ["llm_log_enabled"] = LlmLogEnabled,
            ["audio_debug_enabled"] = AudioDebugEnabled,
        };
        if (!string.IsNullOrEmpty(ApiKey)) dict["api_key"] = ApiKey;
        if (!string.IsNullOrEmpty(LlmApiKey)) dict["llm_api_key"] = LlmApiKey;

        return JsonSerializer.Serialize(dict);
    }
}
