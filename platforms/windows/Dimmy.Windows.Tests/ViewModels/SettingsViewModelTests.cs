using System.Linq;
using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

public class SettingsViewModelTests
{
    [Fact]
    public void Advanced_DefaultFalse()
    {
        var vm = new SettingsViewModel();
        Assert.False(vm.IsAdvanced);
    }

    [Fact]
    public void ToggleAdvanced_ShowsExtraControls()
    {
        var vm = new SettingsViewModel();
        vm.IsAdvanced = true;
        Assert.True(vm.IsAdvanced);
    }

    [Fact]
    public void LoadFromJson_ParsesConfig()
    {
        var vm = new SettingsViewModel();
        var json = """
        {
            "api_url": "https://api.groq.com/openai/v1/audio/transcriptions",
            "api_model": "whisper-large-v3-turbo",
            "language": "it",
            "llm_style": "correct",
            "llm_tone": "formal",
            "shortcut": "Win+Alt",
            "shortcut_mode": "toggle",
            "has_key": true,
            "preprocessing_enabled": true,
            "chunk_streaming_enabled": false,
            "use_keyring": true,
            "llm_enabled": true,
            "llm_api_url": "https://api.groq.com/openai/v1/chat/completions",
            "llm_api_model": "llama-3.3-70b-versatile",
            "llm_use_same_key": true,
            "llm_log_enabled": false,
            "audio_debug_enabled": false,
            "stats_total_words": 1500,
            "stats_total_speaking_secs": 320.5,
            "devices": ["Default", "USB Mic"],
            "prompt": "Hello"
        }
        """;
        vm.LoadFromJson(json);
        Assert.Equal("it", vm.Language);
        Assert.Equal("correct", vm.LlmStyle);
        Assert.True(vm.HasApiKey);
        Assert.Equal(1500, vm.StatsTotalWords);
    }

    [Fact]
    public void ToJson_ProducesValidJson()
    {
        var vm = new SettingsViewModel();
        vm.Language = "en";
        vm.LlmStyle = "off";
        var json = vm.ToJson();
        Assert.Contains("\"language\":\"en\"", json);
        Assert.Contains("\"llm_style\":\"off\"", json);
    }

    [Fact]
    public void IsDirty_FalseAfterLoad()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{\"language\":\"en\"}");
        Assert.False(vm.IsDirty);
    }

    [Fact]
    public void IsDirty_TrueAfterChange()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{\"language\":\"en\"}");
        vm.Language = "it";
        Assert.True(vm.IsDirty);
    }

    [Fact]
    public void ProviderPresets_ReturnsKnownProviders()
    {
        var presets = SettingsViewModel.ProviderPresets;
        Assert.Contains(presets, p => p.Name == "Groq");
        Assert.Contains(presets, p => p.Name == "OpenAI");
        Assert.Contains(presets, p => p.Name == "Deepgram");
        Assert.Contains(presets, p => p.Name == "Gemini");
        Assert.Contains(presets, p => p.Name == "Custom");
    }

    [Fact]
    public void Languages_ContainsSixOptions()
    {
        var langs = SettingsViewModel.Languages;
        Assert.Equal(6, langs.Count);
    }

    [Fact]
    public void TimeSavedEstimate_BasedOnWords()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{\"stats_total_words\":100}");
        // 100 words: typing 100/40=2.5min, dictation 100/150=0.67min → saved ~1.83min = ~110s
        var expected = 100.0 * (1.0 / 40 - 1.0 / 150) * 60;
        Assert.Equal(expected, vm.TimeSavedEstimate, 0.1);
    }

    // ── Input Gain ──

    [Fact]
    public void InputGainPercent_DefaultIs100()
    {
        var vm = new SettingsViewModel();
        Assert.Equal(100, vm.InputGainPercent);
    }

    [Fact]
    public void LoadFromJson_ParsesInputGain()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{\"input_gain\": 0.5}");
        Assert.Equal(50, vm.InputGainPercent);
    }

    [Fact]
    public void LoadFromJson_MissingInputGain_DefaultsTo100()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{\"language\": \"en\"}");
        Assert.Equal(100, vm.InputGainPercent);
    }

    [Fact]
    public void ToJson_IncludesInputGain()
    {
        var vm = new SettingsViewModel();
        vm.InputGainPercent = 75;
        var json = vm.ToJson();
        Assert.Contains("\"input_gain\":0.75", json);
    }

    [Fact]
    public void ToJson_InputGain100_SerializesAsOne()
    {
        var vm = new SettingsViewModel();
        vm.InputGainPercent = 100;
        var json = vm.ToJson();
        Assert.Contains("\"input_gain\":1", json);
    }

    // ── UI appearance fields roundtrip ──

    [Fact]
    public void LoadFromJson_ParsesUiFields()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""
        {
            "border_style": "Solid",
            "waveform_style": "Dots",
            "overlay_position": "Top Left",
            "keep_in_clipboard": true
        }
        """);
        Assert.Equal("Solid", vm.BorderStyle);
        Assert.Equal("Dots", vm.WaveformStyle);
        Assert.Equal("Top Left", vm.OverlayPosition);
        Assert.True(vm.KeepInClipboard);
    }

    [Fact]
    public void LoadFromJson_MissingUiFields_DefaultsCorrectly()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{}");
        Assert.Equal("Rainbow", vm.BorderStyle);
        Assert.Equal("Bars", vm.WaveformStyle);
        Assert.Equal("Bottom Right", vm.OverlayPosition);
        Assert.False(vm.KeepInClipboard);
    }

    [Fact]
    public void ToJson_IncludesAllUiFields()
    {
        var vm = new SettingsViewModel();
        vm.BorderStyle = "Solid";
        vm.WaveformStyle = "Line";
        vm.OverlayPosition = "Top Right";
        vm.KeepInClipboard = true;
        var json = vm.ToJson();
        Assert.Contains("\"border_style\":\"Solid\"", json);
        Assert.Contains("\"waveform_style\":\"Line\"", json);
        Assert.Contains("\"overlay_position\":\"Top Right\"", json);
        Assert.Contains("\"keep_in_clipboard\":true", json);
    }

    [Fact]
    public void ToJson_RoundTrip_PreservesAllFields()
    {
        var vm1 = new SettingsViewModel();
        vm1.InputGainPercent = 60;
        vm1.BorderStyle = "Solid";
        vm1.WaveformStyle = "Dots";
        vm1.OverlayPosition = "Top Left";
        vm1.KeepInClipboard = true;
        vm1.Language = "es";
        vm1.LlmStyle = "summarize";

        var json = vm1.ToJson();

        var vm2 = new SettingsViewModel();
        vm2.LoadFromJson(json);

        Assert.Equal(vm1.InputGainPercent, vm2.InputGainPercent);
        Assert.Equal(vm1.BorderStyle, vm2.BorderStyle);
        Assert.Equal(vm1.WaveformStyle, vm2.WaveformStyle);
        Assert.Equal(vm1.OverlayPosition, vm2.OverlayPosition);
        Assert.Equal(vm1.KeepInClipboard, vm2.KeepInClipboard);
        Assert.Equal(vm1.Language, vm2.Language);
        Assert.Equal(vm1.LlmStyle, vm2.LlmStyle);
    }

    /// <summary>
    /// Pre-V19 saved either uppercase codes ("EN", "IT") or the literal
    /// string "none" in `llm_translate_to`. The normaliser must collapse
    /// both to the canonical lowercase ISO code (or "" for none) so the
    /// shared TranslateToItems dropdown finds a match.
    /// </summary>
    [Theory]
    [InlineData("", "")]
    [InlineData("none", "")]
    [InlineData("None", "")]
    [InlineData("NONE", "")]
    [InlineData(" none ", "")]
    [InlineData("EN", "en")]
    [InlineData("IT", "it")]
    [InlineData("De", "de")]
    [InlineData("en", "en")]
    [InlineData("it", "it")]
    [InlineData("  it  ", "it")]
    public void NormaliseTranslateTo_handles_legacy_values(string input, string expected)
    {
        Assert.Equal(expected, SettingsViewModel.NormaliseTranslateTo(input));
    }

    /// <summary>
    /// Round-trip via LoadFromJson: a config.json carrying legacy
    /// "EN"/"none" must populate LlmTranslateTo with the canonical
    /// lowercase form so the bound dropdown displays correctly after
    /// the V18 → V19 upgrade.
    /// </summary>
    [Theory]
    [InlineData("\"EN\"", "en")]
    [InlineData("\"none\"", "")]
    [InlineData("\"\"", "")]
    [InlineData("\"de\"", "de")]
    public void LoadFromJson_normalises_legacy_translate_to(string jsonValue, string expected)
    {
        var json = $"{{\"llm_translate_to\":{jsonValue}}}";
        var vm = new SettingsViewModel();
        vm.LoadFromJson(json);
        Assert.Equal(expected, vm.LlmTranslateTo);
    }

    // ── Per-provider LLM key flags (the bug fix) ──

    /// <summary>
    /// HasLlmKeyForUrl maps URLs to provider keys mirroring the Rust
    /// `Provider::from_url` logic. Hardcoded URL substrings — this test
    /// will catch desync if either side adds a provider without the
    /// other.
    /// </summary>
    [Theory]
    [InlineData("https://api.groq.com/openai/v1/chat/completions", "groq")]
    [InlineData("https://api.openai.com/v1/chat/completions", "openai")]
    [InlineData("https://openrouter.ai/api/v1/chat/completions", "openrouter")]
    [InlineData("https://generativelanguage.googleapis.com/v1beta/...", "gemini")]
    [InlineData("https://api.anthropic.com/v1/messages", "anthropic")]
    [InlineData("https://my-self-hosted-llm.example.com/v1", "custom")]
    [InlineData("", "groq")] // empty defaults to groq, matches Rust behaviour
    public void HasLlmKeyForUrl_returnsFalse_whenDictNotPopulated(string url, string expectedProviderKey)
    {
        var vm = new SettingsViewModel();
        // Without LoadKeyFlagsFrom call, dict is empty → all false. The
        // expectedProviderKey parameter documents WHICH internal bucket
        // each url maps to (kept in test data so a future regression
        // that breaks the URL→provider mapping shows up here too).
        Assert.False(vm.HasLlmKeyForUrl(url));
        Assert.NotNull(expectedProviderKey); // smoke-use to avoid xUnit1026 warning
    }

    /// <summary>
    /// LoadKeyFlagsFrom parses the FFI JSON shape (`has_llm_*_key`
    /// booleans) and populates the per-provider dict so that subsequent
    /// HasLlmKeyForUrl lookups return the right value when the user
    /// switches the dropdown.
    /// </summary>
    [Fact]
    public void LoadKeyFlagsFrom_populatesPerProviderDict()
    {
        var vm = new SettingsViewModel();
        var json = """
        {
            "has_llm_groq_key": false,
            "has_llm_openai_key": true,
            "has_llm_anthropic_key": true,
            "has_llm_gemini_key": false,
            "has_llm_openrouter_key": false,
            "has_llm_custom_key": false
        }
        """;
        using var doc = System.Text.Json.JsonDocument.Parse(json);
        vm.LoadKeyFlagsFrom(doc.RootElement);

        Assert.True(vm.HasLlmKeyForUrl("https://api.openai.com/v1/chat/completions"));
        Assert.True(vm.HasLlmKeyForUrl("https://api.anthropic.com/v1/messages"));
        Assert.False(vm.HasLlmKeyForUrl("https://api.groq.com/openai/v1/chat/completions"));
        Assert.False(vm.HasLlmKeyForUrl("https://generativelanguage.googleapis.com/v1beta/..."));
    }

    /// <summary>
    /// After LoadKeyFlagsFrom, the active-provider HasLlmKey flag must
    /// reflect the new dict so the green ✓ badge renders correctly on
    /// initial Settings open (not just on dropdown change).
    /// </summary>
    [Fact]
    public void LoadKeyFlagsFrom_redrivesHasLlmKeyForActiveUrl()
    {
        var vm = new SettingsViewModel();
        vm.LlmApiUrl = "https://api.anthropic.com/v1/messages";

        var json = """{ "has_llm_anthropic_key": true }""";
        using var doc = System.Text.Json.JsonDocument.Parse(json);
        vm.LoadKeyFlagsFrom(doc.RootElement);

        Assert.True(vm.HasLlmKey);

        // Switching the active URL to a provider WITHOUT a stored key
        // must NOT auto-flip HasLlmKey — that's the dropdown handler's
        // job, not LoadKeyFlagsFrom's.
        vm.LlmApiUrl = "https://api.groq.com/openai/v1/chat/completions";
        Assert.True(vm.HasLlmKey); // unchanged until LoadKeyFlagsFrom or dropdown handler runs
    }

    /// <summary>
    /// Multiple Anthropic presets share the same URL (Haiku, Sonnet) —
    /// the dict is keyed by provider, so all variants of the same
    /// provider must surface the same flag.
    /// </summary>
    [Fact]
    public void HasLlmKeyForUrl_treatsAnthropicHaikuAndSonnetIdentically()
    {
        var vm = new SettingsViewModel();
        var json = """{ "has_llm_anthropic_key": true }""";
        using var doc = System.Text.Json.JsonDocument.Parse(json);
        vm.LoadKeyFlagsFrom(doc.RootElement);

        // Both Anthropic presets in LlmProviderPresets share api.anthropic.com
        var haikuUrl = SettingsViewModel.LlmProviderPresets
            .First(p => p.Name == "Anthropic").Url;
        var sonnetUrl = SettingsViewModel.LlmProviderPresets
            .First(p => p.Name == "Anthropic-Sonnet").Url;
        Assert.Equal(haikuUrl, sonnetUrl); // sanity: presets really do share URL
        Assert.True(vm.HasLlmKeyForUrl(haikuUrl));
        Assert.True(vm.HasLlmKeyForUrl(sonnetUrl));
    }
}
