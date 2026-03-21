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
    public void TimeSavedEstimate_Is3xSpeakingTime()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{\"stats_total_speaking_secs\":100.0}");
        Assert.Equal(300.0, vm.TimeSavedEstimate, 0.1);
    }
}
