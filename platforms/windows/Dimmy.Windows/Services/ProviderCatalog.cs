using System;
using System.Collections.Generic;
using System.Linq;

namespace Dimmy.Windows.Services;

/// <summary>
/// Static catalog of the AI providers Dimmy can talk to, plus the data the
/// Providers settings page needs: a display mark, an accent colour, the EXACT
/// console URL where a user creates an API key (the deep-link that makes key
/// acquisition foolproof), and the REAL, COMPLETE list of models that provider
/// offers — the exact same models the user can pick on the Voice input /
/// Output / recap pickers (see ProviderPresets + LlmProviderPresets in
/// SettingsViewModel.cs and the ComboBoxes in SettingsWindow.xaml). Not
/// placeholders — if a model isn't pickable elsewhere it doesn't belong here.
///
/// This drives the Providers &amp; Keys page. It is pure data — it does NOT
/// change the Rust core or the keystore. Keys still go through
/// dimmy_save_llm_provider_key.
///
/// Capability truth (matches core/src/provider.rs):
///   - Deepgram: STT only.
///   - Anthropic, OpenRouter: LLM/Recap only (no STT endpoint).
///   - Groq, OpenAI, Gemini, Fireworks, Together: STT + LLM + Recap
///     (Together exposes parakeet/whisper STT in the Win picker).
///   - On-device: all three, no key.
/// </summary>
public sealed record ProviderInfo(
    string Id,
    string Name,
    string Mark,        // 2-letter logo mark (legacy; real SVG logo drives the UI now)
    string AccentHex,   // brand-ish accent
    string ConsoleUrl,  // exact "create an API key" page — empty = no key
    bool Stt,
    bool Llm,
    string GetKeyHint,  // 1-line "how to" shown in the add-key flow
    IReadOnlyList<ProviderModel> Models);

/// <summary>A model the user can actually select for this provider, with the
/// task(s) it serves. Name mirrors the picker label so it's recognisable.</summary>
public sealed record ProviderModel(string Name, bool Stt, bool Llm, bool Recap);

public static class ProviderCatalog
{
    /// <summary>Recap is an LLM operation — any LLM-capable provider can do it.</summary>
    public static bool Recap(this ProviderInfo p) => p.Llm;

    /// <summary>Vendors whose LLM/recap key the keystore FFI accepts — those with
    /// a completions endpoint. Anthropic/OpenRouter are LLM-only; Deepgram is not
    /// here (STT-only); Custom/Local are configured elsewhere.</summary>
    private static readonly HashSet<string> LlmKeyVendors = new(StringComparer.OrdinalIgnoreCase)
    {
        "groq", "openai", "anthropic", "gemini", "openrouter", "fireworks", "together",
    };

    /// <summary>Vendors whose STT key the FFI accepts — those with a cloud speech
    /// endpoint (mirrors Provider::supports_stt in core/src/provider.rs). Deepgram
    /// is here (STT-only). Custom needs a URL so it's keyed on Voice/Output.</summary>
    private static readonly HashSet<string> SttKeyVendors = new(StringComparer.OrdinalIgnoreCase)
    {
        "groq", "openai", "gemini", "deepgram", "fireworks", "together",
    };

    /// <summary>The keystore scopes the Providers page writes the single key into,
    /// matching what dimmy_save_llm_provider_key accepts per vendor. STT-capable
    /// vendors get "stt"; LLM-capable vendors get "llm"+"recap". Empty when the
    /// provider can't be keyed here (Custom needs a URL, On-device needs no key).</summary>
    public static IReadOnlyList<string> KeySaveScopes(this ProviderInfo p)
    {
        var scopes = new List<string>();
        if (p.Stt && SttKeyVendors.Contains(p.Id)) scopes.Add("stt");
        if (p.Llm && LlmKeyVendors.Contains(p.Id)) { scopes.Add("llm"); scopes.Add("recap"); }
        return scopes;
    }

    /// <summary>True when a key for this provider can be saved from the Providers
    /// page (it has at least one FFI-accepted scope). Deepgram is now keyable
    /// (stt). Custom (arbitrary endpoint) and On-device are not.</summary>
    public static bool IsKeyableHere(this ProviderInfo p) => p.KeySaveScopes().Count > 0;

    // Capability shorthands for the local/custom rows, which aren't in the
    // single-source catalog (local models have download/bundle semantics;
    // custom is a free-form endpoint).
    private static ProviderModel S(string name) => new(name, true, false, false);   // speech
    private static ProviderModel L(string name) => new(name, false, true, true);    // rewrite + recap
    private static ProviderModel SLR(string name) => new(name, true, true, true);   // all three

    // Cloud providers' model rows are DERIVED from the single-source catalog so
    // this reference list can never disagree with the actual pickers. The
    // task flags come straight from the catalog; the label gets the tier as a
    // short qualifier when present.
    private static IReadOnlyList<ProviderModel> Cat(string providerId)
    {
        var p = ModelCatalog.ById(providerId);
        if (p == null) return Array.Empty<ProviderModel>();
        return p.Models.Select(m => new ProviderModel(
            string.IsNullOrEmpty(m.Tier) ? m.Label : $"{m.Label} · {m.Tier}",
            m.Tasks.Contains("stt"),
            m.Tasks.Contains("llm"),
            m.Tasks.Contains("recap"))).ToList();
    }

    public static IReadOnlyList<ProviderInfo> All { get; } = new[]
    {
        new ProviderInfo("local", "On-device", "Lo", "#7C8CFF", "",
            true, true,
            "No key needed — runs fully on your machine, private and free. Models download on demand.",
            new[]
            {
                // Local STT — whisper.cpp sizes + Parakeet (core/src/local_stt.rs, parakeet.rs).
                S("Whisper Tiny · 42 MB"),
                S("Whisper Base · 78 MB · default"),
                S("Whisper Small · 181 MB"),
                S("Whisper Medium · 514 MB"),
                S("Whisper Large-v3-Turbo Q5 · 574 MB"),
                S("Whisper Large-v3-Turbo Q8 · 874 MB"),
                S("Whisper Large-v3 Q5 · 1.1 GB"),
                S("Distil-Large-v3.5 Q8 · EN · 818 MB"),
                S("Distil-Large-v3.5 Q5 · EN · 538 MB"),
                S("Parakeet TDT v3 · 2.5 GB"),
                // Local LLM — every downloadable Gemma + Phi (core/src/local_llm.rs).
                L("Gemma 4 E2B Q4 · 3.1 GB"),
                L("Gemma 4 E2B Q5 · 3.7 GB"),
                L("Gemma 4 E4B Q3 · 4.1 GB"),
                L("Gemma 4 E4B Q4 · 5.0 GB"),
                L("Gemma 4 E4B Q8 · 8.2 GB"),
                L("Phi-4 Mini Q4 · 2.5 GB · default"),
            }),

        new ProviderInfo("groq", "Groq", "Gq", "#F55036", "https://console.groq.com/keys",
            true, true,
            "Sign up free, create an API key, paste it here. Free tier is plenty to start.",
            Cat("groq")),

        new ProviderInfo("openai", "OpenAI", "Ai", "#10A37F", "https://platform.openai.com/api-keys",
            true, true,
            "Create a key on the API keys page (needs a small balance for cloud STT).",
            Cat("openai")),

        new ProviderInfo("anthropic", "Anthropic", "An", "#D4A27F", "https://console.anthropic.com/settings/keys",
            false, true,
            "Create a key in the Anthropic console. Best for high-quality rewrite & recap.",
            Cat("anthropic")),

        new ProviderInfo("gemini", "Google Gemini", "Ge", "#4285F4", "https://aistudio.google.com/apikey",
            true, true,
            "Get a free key in Google AI Studio — one click, no card required.",
            Cat("gemini")),

        new ProviderInfo("deepgram", "Deepgram", "Dg", "#13EF93", "https://console.deepgram.com/",
            true, false,
            "Create a key in the Deepgram console — fast, accurate speech-to-text.",
            Cat("deepgram")),

        new ProviderInfo("openrouter", "OpenRouter", "Or", "#6566F1", "https://openrouter.ai/keys",
            false, true,
            "One key unlocks many models for rewrite & recap. Free tier available.",
            Cat("openrouter")),

        new ProviderInfo("fireworks", "Fireworks", "Fw", "#6B2FFF", "https://fireworks.ai/account/api-keys",
            true, true,
            "Create a key in the Fireworks dashboard.",
            Cat("fireworks")),

        new ProviderInfo("together", "Together AI", "Tg", "#0F6FFF", "https://api.together.ai/settings/api-keys",
            true, true,
            "Create a key in the Together dashboard.",
            Cat("together")),

        new ProviderInfo("custom", "Custom (OpenAI-compatible)", "Cu", "#9AA0AC", "",
            true, true,
            "Point Dimmy at any OpenAI-compatible endpoint. Enter the base URL + key on the Voice input / Output pages.",
            new[]
            {
                SLR("Any OpenAI-compatible model you configure"),
            }),
    };

    public static ProviderInfo? ById(string id) =>
        All.FirstOrDefault(p => string.Equals(p.Id, id, StringComparison.OrdinalIgnoreCase));
}
