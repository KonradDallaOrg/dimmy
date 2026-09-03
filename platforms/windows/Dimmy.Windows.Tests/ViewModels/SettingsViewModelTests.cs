using System;
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
            "has_groq_key": true,
            "preprocessing_enabled": true,
            "chunk_streaming_enabled": false,
            "use_keyring": true,
            "llm_enabled": true,
            "llm_api_url": "https://api.groq.com/openai/v1/chat/completions",
            "llm_api_model": "openai/gpt-oss-120b",
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

    /// <summary>
    /// REGRESSION GUARD (2026-05-13): generic <c>ToJson()</c> must
    /// NOT include the Notion target fields. Rust treats an empty
    /// <c>notion_target_id</c> as "clear destination", so any save
    /// from a non-Integration page with a transient-empty VM was
    /// wiping a valid destination. Fix shipped on
    /// <c>feat/anthropic-subscription-login</c>; this test pins it.
    /// </summary>
    [Fact]
    public void ToJson_DefaultDoesNotIncludeNotionTargetFields()
    {
        var vm = new SettingsViewModel
        {
            NotionTargetId = "abc-123-page-id",
            NotionTargetKind = "page",
            NotionTargetTitle = "My Workspace",
        };
        var json = vm.ToJson(); // default: includeNotion = false
        Assert.DoesNotContain("notion_target_id", json);
        Assert.DoesNotContain("notion_target_kind", json);
        Assert.DoesNotContain("notion_target_title", json);
        // notion_auto_send IS safe to round-trip (bool toggle, not a
        // load-bearing identifier).
        Assert.Contains("notion_auto_send", json);
    }

    /// <summary>
    /// Counterpart to the regression guard: the Notion picker
    /// confirm / Disconnect path passes <c>includeNotion: true</c>
    /// and the fields MUST be present in the output then.
    /// </summary>
    [Fact]
    public void ToJson_IncludeNotionTrue_ContainsNotionTargetFields()
    {
        var vm = new SettingsViewModel
        {
            NotionTargetId = "abc-123-page-id",
            NotionTargetKind = "page",
            NotionTargetTitle = "My Workspace",
        };
        var json = vm.ToJson(includeNotion: true);
        Assert.Contains("\"notion_target_id\":\"abc-123-page-id\"", json);
        Assert.Contains("\"notion_target_kind\":\"page\"", json);
        Assert.Contains("\"notion_target_title\":\"My Workspace\"", json);
    }

    // ── LLM wipe guard (2026-05-15) ─────────────────────────────────────
    // Same pattern as Notion: when a non-LLM-page save fires with a
    // transient-empty VM, ToJson() default MUST NOT emit llm_api_url /
    // llm_api_model / etc. — otherwise the Rust core sees `"": ""` and
    // wipes a valid LLM provider config from disk. Found 2026-05-15
    // when the user's `llm_api_url` had been silently emptied: the
    // Settings Save path wrote the VM's (empty) state and zero'd the
    // file.

    [Fact]
    public void ToJson_DefaultDoesNotIncludeLlmIdentityFields()
    {
        var vm = new SettingsViewModel
        {
            LlmApiUrl = "https://api.anthropic.com/v1/messages",
            LlmApiModel = "claude-opus-4-7",
            LlmUseSameKey = true,
            LlmAuthMethod = "subscription",
            LlmMode = "cloud",
            LocalLlmModel = "gemma-4-E2B-it-Q4_K_M.gguf",
        };
        var json = vm.ToJson(); // default: includeLlm = false
        Assert.DoesNotContain("\"llm_api_url\"", json);
        Assert.DoesNotContain("\"llm_api_model\"", json);
        Assert.DoesNotContain("\"llm_use_same_key\"", json);
        Assert.DoesNotContain("\"llm_auth_method\"", json);
        Assert.DoesNotContain("\"llm_mode\"", json);
        Assert.DoesNotContain("\"local_llm_model\"", json);
        // llm_enabled is derived from llm_style and lives under the
        // same gate — otherwise a non-LLM save could flip the kill
        // switch when style happens to be "off".
        Assert.DoesNotContain("\"llm_enabled\"", json);
        // BUT llm_style stays default-emitted: it's a user preference
        // (the rewrite style chip), re-pickable from any UI, not an
        // identity field. Wiping it would only force re-pick, not
        // brick a provider.
        Assert.Contains("\"llm_style\"", json);
    }

    [Fact]
    public void ToJson_IncludeLlmTrue_ContainsAllLlmIdentityFields()
    {
        var vm = new SettingsViewModel
        {
            LlmApiUrl = "https://api.anthropic.com/v1/messages",
            LlmApiModel = "claude-opus-4-7",
            LlmUseSameKey = true,
            LlmAuthMethod = "subscription",
            LlmMode = "cloud",
            LocalLlmModel = "gemma-4-E2B-it-Q4_K_M.gguf",
            LlmStyle = "correct",
        };
        var json = vm.ToJson(includeLlm: true);
        Assert.Contains("\"llm_api_url\":\"https://api.anthropic.com/v1/messages\"", json);
        Assert.Contains("\"llm_api_model\":\"claude-opus-4-7\"", json);
        Assert.Contains("\"llm_use_same_key\":true", json);
        Assert.Contains("\"llm_auth_method\":\"subscription\"", json);
        Assert.Contains("\"llm_mode\":\"cloud\"", json);
        Assert.Contains("\"local_llm_model\":\"gemma-4-E2B-it-Q4_K_M.gguf\"", json);
        // llm_enabled flips true when style != "off".
        Assert.Contains("\"llm_enabled\":true", json);
    }

    [Fact]
    public void ToJson_IncludeLlmTrue_OmitsEmptyIdentityFields()
    {
        // 2026-05-16: behaviour changed from "explicit empty wipes" to
        // "if-empty-omit" as defense-in-depth against transient-empty
        // ViewModels destroying saved config. A user who really wants
        // to wipe an LLM provider doesn't have a Disconnect button in
        // the current UI; if/when one is added, the right shape is a
        // dedicated `forceClearLlm:true` parameter, NOT empty-string-
        // as-clear (which can't distinguish "user really wants to
        // wipe" from "VM not yet loaded").
        //
        // Rationale + incident: see CLAUDE.md "Decision tree — Save
        // anything in C# Settings → ToJson" and PR #73.
        var vm = new SettingsViewModel
        {
            LlmApiUrl = "",
            LlmApiModel = "",
            LlmAuthMethod = "api_key",
        };
        var json = vm.ToJson(includeLlm: true);
        Assert.DoesNotContain("\"llm_api_url\":\"\"", json);
        Assert.DoesNotContain("\"llm_api_model\":\"\"", json);
        // Non-identity LLM fields under includeLlm still emit so the
        // user can flip llm_mode / auth_method even on a fresh page
        // without losing the empty-identity safeguard.
        Assert.Contains("\"llm_auth_method\":\"api_key\"", json);
        Assert.Contains("\"llm_mode\":\"cloud\"", json);
    }

    [Fact]
    public void ToJson_DefaultOmitsEmptySttIdentityFields()
    {
        // Mirror of the LLM-side protection — api_url / api_model /
        // selected_device / local_model are no longer in the universal
        // dict; they emit only when non-empty. Burned 2026-05-16:
        // PR #60 gated llm_*/recap_* but left STT identity in the
        // universal dict, so any per-field save (e.g. NotionAutoSend
        // toggle) wiped api_url when the VM was transient-empty.
        var vm = new SettingsViewModel
        {
            ApiUrl = "",
            ApiModel = "",
            SelectedDevice = "",
            LocalModel = "",
        };
        var json = vm.ToJson();
        Assert.DoesNotContain("\"api_url\":\"\"", json);
        Assert.DoesNotContain("\"api_model\":\"\"", json);
        Assert.DoesNotContain("\"selected_device\":\"\"", json);
        Assert.DoesNotContain("\"local_model\":\"\"", json);
    }

    [Fact]
    public void ToJson_DefaultEmitsPopulatedSttIdentityFields()
    {
        // Sanity counterpart to the omit test — when fields ARE set,
        // they MUST round-trip on every save (the user reaches Save
        // expecting their STT config to persist).
        var vm = new SettingsViewModel
        {
            ApiUrl = "https://api.groq.com/openai/v1/audio/transcriptions",
            ApiModel = "whisper-large-v3-turbo",
        };
        var json = vm.ToJson();
        Assert.Contains("\"api_url\":\"https://api.groq.com/openai/v1/audio/transcriptions\"", json);
        Assert.Contains("\"api_model\":\"whisper-large-v3-turbo\"", json);
    }

    [Fact]
    public void ToJson_DefaultDoesNotIncludeRecapIdentityFields()
    {
        var vm = new SettingsViewModel
        {
            RecapAuthMethod = "subscription",
            RecapUseSameKey = false,
            RecapModelOverride = "gemini-3.1-pro",
        };
        var json = vm.ToJson(); // default: includeRecap = false
        Assert.DoesNotContain("\"recap_auth_method\"", json);
        Assert.DoesNotContain("\"recap_use_same_key\"", json);
        Assert.DoesNotContain("\"recap_model_override\"", json);
    }

    [Fact]
    public void ToJson_IncludeRecapTrue_ContainsAllRecapIdentityFields()
    {
        var vm = new SettingsViewModel
        {
            RecapAuthMethod = "subscription",
            RecapUseSameKey = false,
            RecapModelOverride = "gemini-3.1-pro",
        };
        var json = vm.ToJson(includeRecap: true);
        Assert.Contains("\"recap_auth_method\":\"subscription\"", json);
        Assert.Contains("\"recap_use_same_key\":false", json);
        Assert.Contains("\"recap_model_override\":\"gemini-3.1-pro\"", json);
    }

    [Fact]
    public void RecapUseSameKey_defaults_to_true()
    {
        var vm = new SettingsViewModel();
        Assert.True(vm.RecapUseSameKey);
    }

    [Fact]
    public void LoadFromJson_parses_recap_use_same_key_false()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""{"recap_use_same_key":false}""");
        Assert.False(vm.RecapUseSameKey);
    }

    [Fact]
    public void LoadFromJson_missing_recap_use_same_key_defaults_true()
    {
        // Forward compat: a config without the field (older app
        // version) must land on the default ON, not silently OFF.
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""{"recap_auth_method":""}""");
        Assert.True(vm.RecapUseSameKey);
    }

    [Fact]
    public void IsDirty_FiresWhenRecapUseSameKeyFlips()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""{"recap_use_same_key":true}""");
        Assert.False(vm.IsDirty);
        vm.RecapUseSameKey = false;
        Assert.True(vm.IsDirty);
    }

    [Fact]
    public void LoadThenDefaultSave_PreservesLlmAndRecapByOmission()
    {
        // The "wipe protection" round-trip: a config on disk has valid
        // LLM + Recap. The Settings UI loads it. A NON-LLM/NON-RECAP
        // save fires (e.g. user toggled theme). The default ToJson()
        // must omit llm_* + recap_* entirely so the Rust core, seeing
        // missing fields, preserves the on-disk state. Before this
        // fix, ToJson() emitted `"llm_api_url": ""` and the Rust core
        // wrote the empty string → permanent wipe.
        var src = """
            {
              "llm_api_url": "https://api.anthropic.com/v1/messages",
              "llm_api_model": "claude-opus-4-7",
              "llm_auth_method": "subscription",
              "recap_model_override": "gemini-3.1-pro-preview",
              "language": "it"
            }
            """;
        var vm = new SettingsViewModel();
        vm.LoadFromJson(src);
        // VM has the values internally:
        Assert.Equal("https://api.anthropic.com/v1/messages", vm.LlmApiUrl);
        Assert.Equal("gemini-3.1-pro-preview", vm.RecapModelOverride);
        // But default ToJson() OMITS them — Rust core preserves disk state.
        var json = vm.ToJson();
        Assert.DoesNotContain("llm_api_url", json);
        Assert.DoesNotContain("recap_model_override", json);
        // Universal field round-trips:
        Assert.Contains("\"language\":\"it\"", json);
    }

    [Fact]
    public void IsDirty_FiresWhenLlmFieldsChangeEvenThoughDefaultToJsonOmitsThem()
    {
        // IsDirty MUST see ALL fields — otherwise a user editing only
        // LlmApiUrl would have Save stay disabled, lose the change.
        // The fix: ToJsonFull() (includeLlm:true, includeRecap:true,
        // includeNotion:true) is used by snapshot capture + IsDirty
        // comparison.
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""{ "llm_api_url": "https://api.anthropic.com/v1/messages" }""");
        Assert.False(vm.IsDirty);
        vm.LlmApiUrl = "https://api.openai.com/v1/chat/completions";
        Assert.True(vm.IsDirty);
    }

    [Fact]
    public void IsDirty_FiresWhenRecapModelChanges()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""{ "recap_model_override": "claude-opus-4-7" }""");
        Assert.False(vm.IsDirty);
        vm.RecapModelOverride = "claude-sonnet-4-6";
        Assert.True(vm.IsDirty);
    }

    /// <summary>
    /// Migration regression guard: a legacy config that still
    /// carries the synthetic <c>claude-code://default</c> URL must
    /// land in the new schema as Anthropic + subscription. Without
    /// this migration the Authentication radio would default to
    /// API key and the user's saved subscription preference would
    /// be silently lost.
    /// </summary>
    [Fact]
    public void LoadFromJson_MigratesClaudeCodeUrlToAnthropicSubscription()
    {
        var vm = new SettingsViewModel();
        var json = """
        {
            "llm_api_url": "claude-code://default",
            "llm_api_model": "claude-opus-4-7",
            "llm_auth_method": "api_key"
        }
        """;
        vm.LoadFromJson(json);
        Assert.Equal("https://api.anthropic.com/v1/messages", vm.LlmApiUrl);
        Assert.Equal("subscription", vm.LlmAuthMethod);
        // Model untouched — the migration only rewrites URL + auth.
        Assert.Equal("claude-opus-4-7", vm.LlmApiModel);
    }

    /// <summary>
    /// LoadFromJson reads the explicit <c>llm_auth_method</c> field
    /// when present (no migration needed). Default for legacy
    /// configs that lack the field is "api_key" (classic HTTP path).
    /// </summary>
    [Fact]
    public void LoadFromJson_ReadsExplicitAuthMethodAndDefaultsToApiKey()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""{"llm_api_url":"https://api.anthropic.com/v1/messages","llm_auth_method":"subscription"}""");
        Assert.Equal("subscription", vm.LlmAuthMethod);

        var vm2 = new SettingsViewModel();
        vm2.LoadFromJson("""{"llm_api_url":"https://api.anthropic.com/v1/messages"}""");
        Assert.Equal("api_key", vm2.LlmAuthMethod);
    }

    /// <summary>
    /// RecapAuthMethod accepts only "" (inherit), "api_key", or
    /// "subscription". A malformed config value must normalise to
    /// "" so the Rust dispatcher never sees an unknown token.
    /// </summary>
    [Fact]
    public void LoadFromJson_NormalisesUnknownRecapAuthMethodToInherit()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("""{"recap_auth_method":"garbage_value"}""");
        Assert.Equal("", vm.RecapAuthMethod);

        var vm2 = new SettingsViewModel();
        vm2.LoadFromJson("""{"recap_auth_method":"subscription"}""");
        Assert.Equal("subscription", vm2.RecapAuthMethod);

        var vm3 = new SettingsViewModel();
        vm3.LoadFromJson("""{"recap_auth_method":"api_key"}""");
        Assert.Equal("api_key", vm3.RecapAuthMethod);
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
        // Presets are now derived from the single-source model catalog, so the
        // synthetic Name is "{provider}-{model}" rather than the bare provider
        // word. Custom is always present (kept in code). The cloud providers
        // appear only when the embedded catalog actually loaded — in a unit
        // test host without dimmy_lib on the load path the catalog is empty and
        // only Custom remains. Catalog *content* is verified by the Rust
        // catalog::tests + the Tier-A live test; here we just check the wiring,
        // so the cloud assertions are gated on the catalog being available.
        var presets = SettingsViewModel.ProviderPresets;
        Assert.Contains(presets, p => p.Name == "Custom");
        if (presets.Count > 1)
        {
            Assert.Contains(presets, p => p.Url.Contains("groq.com"));
            Assert.Contains(presets, p => p.Url.Contains("openai.com"));
            Assert.Contains(presets, p => p.Url.Contains("deepgram.com"));
            Assert.Contains(presets, p => p.Url.Contains("generativelanguage.googleapis.com"));
        }
    }

    [Fact]
    public void Languages_StartWithAutoDetect_AndCoverCoreLanguages()
    {
        var langs = SettingsViewModel.Languages;
        // Auto-detect ("" code) is the first entry. NOTE: Auto-detect is
        // reliable only with cloud STT; local whisper auto-detection is
        // unreliable (empty output + confident misdetection), which the
        // combo's info tip calls out.
        Assert.Equal("", langs[0].Key);
        Assert.Equal("Auto-detect", langs[0].Value);
        Assert.Contains(langs, l => l.Key == "it");
        Assert.Contains(langs, l => l.Key == "en");
        Assert.Contains(langs, l => l.Key == "zh");
        // Expanded world-language list (count is intentionally not pinned so
        // adding a language does not break the test).
        Assert.True(langs.Count >= 10, "expected the expanded language list");
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

    // ── Recap-model override (meeting picker) ─────────────────────────
    // Coverage for the dropdown landed in commit 4e8e611 / re-applied
    // 5f4b918 after a debug-roundtrip revert. The view-model carries
    // the chosen model id (or "" for "auto"); the field has to survive
    // a load/save round-trip so a user pinning Opus 4.7 doesn't lose
    // their pick on next config reload.

    [Fact]
    public void RecapModelOverride_defaults_to_empty()
    {
        var vm = new SettingsViewModel();
        Assert.Equal("", vm.RecapModelOverride);
    }

    [Fact]
    public void LoadFromJson_parses_recap_model_override()
    {
        var vm = new SettingsViewModel();
        vm.LoadFromJson("{\"recap_model_override\":\"claude-opus-4-7\"}");
        Assert.Equal("claude-opus-4-7", vm.RecapModelOverride);
    }

    [Fact]
    public void LoadFromJson_recap_model_override_missing_keeps_empty()
    {
        var vm = new SettingsViewModel();
        vm.RecapModelOverride = "claude-opus-4-7"; // some prior value
        vm.LoadFromJson("{}");
        Assert.Equal("", vm.RecapModelOverride);
    }

    [Fact]
    public void ToJson_includes_recap_model_override()
    {
        // recap_* fields are gated behind includeRecap:true since the
        // 2026-05-15 wipe-protection fix. The Recap section in Settings
        // → Output owns these fields; this test pins the "explicit
        // emit" path used by Save_Click / AutoSaveOnClose.
        var vm = new SettingsViewModel { RecapModelOverride = "gemini-3.1-pro" };
        var json = vm.ToJson(includeRecap: true);
        Assert.Contains("\"recap_model_override\"", json);
        Assert.Contains("gemini-3.1-pro", json);
    }

    [Theory]
    [InlineData("")]
    [InlineData("claude-opus-4-7")]
    [InlineData("claude-sonnet-4-6")]
    [InlineData("claude-haiku-4-5-20251001")]
    [InlineData("gemini-3.1-pro-preview")]
    [InlineData("gemini-2.5-pro")]
    [InlineData("gemini-2.5-flash")]
    [InlineData("gpt-5")]
    [InlineData("gpt-4o")]
    [InlineData("custom-model-id-from-future")]
    public void RecapModelOverride_round_trips_through_json(string modelId)
    {
        // The dropdown's curated tags + the Custom escape hatch must all
        // round-trip cleanly. Whatever the user picks, that's exactly
        // what should land in config.json and come back out on reload.
        // Pass includeRecap:true to simulate the Settings → Save path
        // (the only site that's allowed to persist recap_* fields).
        var vm = new SettingsViewModel { RecapModelOverride = modelId };
        var json = vm.ToJson(includeRecap: true);
        var vm2 = new SettingsViewModel();
        vm2.LoadFromJson(json);
        Assert.Equal(modelId, vm2.RecapModelOverride);
    }

    [Theory]
    [InlineData("gemini-3.1-pro", "gemini-3.1-pro-preview")]
    [InlineData("gemini-3-1-pro", "gemini-3.1-pro-preview")]
    [InlineData("gemini-3-pro", "gemini-3-pro-preview")]
    [InlineData("gpt-5", "gpt-5")] // valid ids pass through untouched
    [InlineData("gemini-3.1-pro-preview", "gemini-3.1-pro-preview")]
    public void LoadFromJson_migrates_stale_gemini_recap_ids(string stored, string expected)
    {
        // Older builds saved bare Gemini ids ("gemini-3.1-pro") that 404 on
        // the live endpoint ("models/gemini-3.1-pro is not found"). LoadFromJson
        // migrates them to the valid -preview form so the recap call resolves.
        var vm = new SettingsViewModel();
        vm.LoadFromJson($"{{\"recap_model_override\":\"{stored}\"}}");
        Assert.Equal(expected, vm.RecapModelOverride);
    }

    // ── Audio-source dead config field (always-mix architecture) ──
    // Commit 4e8e611 dropped the AudioSource radio buttons from
    // Settings, but the field stays in config.json for backward read
    // compat. The view-model still serialises it as a string the
    // Rust core ignores at runtime.

    [Fact]
    public void AudioSource_default_does_not_break_load()
    {
        var vm = new SettingsViewModel();
        // Old configs predating always-mix have audio_source="mic"
        vm.LoadFromJson("{\"audio_source\":\"mic\"}");
        Assert.Equal("mic", vm.AudioSource);
        // And still serialise it on save so a downgrade is non-destructive
        var json = vm.ToJson();
        Assert.Contains("\"audio_source\"", json);
    }

    [Theory]
    [InlineData("mic")]
    [InlineData("system")]
    [InlineData("mix")]
    public void AudioSource_round_trips_through_json(string source)
    {
        var vm = new SettingsViewModel { AudioSource = source };
        var json = vm.ToJson();
        var vm2 = new SettingsViewModel();
        vm2.LoadFromJson(json);
        Assert.Equal(source, vm2.AudioSource);
    }

    // ── Recap rc → user-facing message (Phase 3 UI feedback) ──
    // Pinned categorical mapping so the user message never echoes
    // API response bodies (which could contain transcript text via
    // 4xx error payloads). Each rc maps to exactly one branch — if
    // a new rc lands in core/src/ffi.rs::dimmy_llm_call_raw, this
    // test catches the missing case via the fallback branch.

    [Fact]
    public void RecapRcToUserMessage_unknown_rc_uses_fallback()
    {
        var msg = Dimmy.Windows.Helpers.MeetingRecapHelpers
            .RecapRcToUserMessage(-99, "auto");
        Assert.Contains("-99", msg);
        Assert.Contains("dimmy.log", msg);
    }

    // ── Recap rc → telemetry category ──
    // `meeting.recap_completed` used to carry `success: false` and nothing
    // else, so a 37% failure rate (21 of 57 over 60 days, measured
    // 2026-09-03) was visible but never explicable. These buckets are the
    // same vocabulary the Rust sanitizer emits, so the two ends aggregate
    // together.

    [Theory]
    [InlineData(-2, "no_api_key")]
    [InlineData(-4, "model_load")]
    [InlineData(-5, "not_found")]
    [InlineData(-6, "auth")]
    [InlineData(-7, "rate_limit")]
    [InlineData(-8, "network")]
    [InlineData(-9, "too_large")]
    [InlineData(-10, "truncated")]
    [InlineData(-11, "refusal")]
    public void RecapRcToCategory_maps_each_documented_rc(int rc, string expected)
    {
        Assert.Equal(expected,
            Dimmy.Windows.Helpers.MeetingRecapHelpers.RecapRcToCategory(rc));
    }

    [Fact]
    public void RecapRcToCategory_is_categorical_never_free_text()
    {
        // The whole point is that nothing provider-supplied escapes here:
        // an unmapped rc must degrade to a fixed bucket, never to the code
        // or a message that could carry transcript text.
        var cat = Dimmy.Windows.Helpers.MeetingRecapHelpers.RecapRcToCategory(-99);
        Assert.Equal("unknown", cat);
        Assert.DoesNotContain("-99", cat);
    }

    [Theory]
    [InlineData(-2, "key")]
    [InlineData(-3, "HTTP")]
    [InlineData(-4, "Local")]
    [InlineData(-6, "key")]
    [InlineData(-7, "rate")]
    [InlineData(-8, "Network")]
    [InlineData(-9, "too large")]
    public void RecapRcToUserMessage_named_categories_have_text(int rc, string keyword)
    {
        var msg = Dimmy.Windows.Helpers.MeetingRecapHelpers
            .RecapRcToUserMessage(rc, "claude-opus-4-7");
        Assert.NotEmpty(msg);
        Assert.Contains(keyword, msg, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void RecapRcToUserMessage_minus_five_includes_model_hint()
    {
        // -5 ("model not found") gives the user the picker label
        // so they immediately know which model id needs replacing.
        var msg = Dimmy.Windows.Helpers.MeetingRecapHelpers
            .RecapRcToUserMessage(-5, "gemini-3.1-pro");
        Assert.Contains("gemini-3.1-pro", msg);
        Assert.Contains("Settings", msg);
    }

    [Fact]
    public void RecapRcToUserMessage_minus_five_with_empty_override_says_auto()
    {
        // When the user has "Auto" picked, the modelOverride arg
        // is "" — show a readable "auto" hint instead of an empty
        // ' '...
        var msg = Dimmy.Windows.Helpers.MeetingRecapHelpers
            .RecapRcToUserMessage(-5, "");
        Assert.Contains("auto", msg);
    }

    [Fact]
    public void RecapRcToUserMessage_never_echoes_caller_supplied_body()
    {
        // SECURITY: the helper takes ONLY the rc + the user's curated
        // model id. There is no way to inject HTTP response body text.
        // This test pins the signature — if someone later adds an
        // overload that takes a body string, this test still passes
        // for the legacy overload AND the security review forces
        // them to think about the new overload.
        var msg = Dimmy.Windows.Helpers.MeetingRecapHelpers
            .RecapRcToUserMessage(-6, "claude-opus-4-7");
        // Should NOT contain anything that looks like an Anthropic
        // 401 body (which would be the natural -6 cause).
        Assert.DoesNotContain("authentication_error", msg);
        Assert.DoesNotContain("x-api-key", msg);
    }
}
