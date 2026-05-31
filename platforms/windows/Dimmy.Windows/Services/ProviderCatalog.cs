using System;
using System.Collections.Generic;
using System.Linq;

namespace Dimmy.Windows.Services;

/// <summary>
/// Static catalog of the AI providers Dimmy can talk to, plus the data the
/// Providers settings page needs: a display mark, an accent colour, the EXACT
/// console URL where a user creates an API key (the deep-link that makes key
/// acquisition foolproof), and which of the three capabilities (STT / LLM /
/// Recap) each provider can serve.
///
/// This drives the new Providers &amp; Keys page. It is pure data — it does NOT
/// change the Rust core or the keystore. Keys still go through
/// dimmy_save_llm_provider_key; this just organises the UI around "one key per
/// provider" instead of scattering provider/key controls across pages.
///
/// Capability truth (matches core/src/provider.rs):
///   - Deepgram: STT only.
///   - Anthropic, OpenRouter, Together: LLM/Recap only (no STT endpoint).
///   - Groq, OpenAI, Gemini, Fireworks: STT + LLM + Recap.
///   - On-device: all three, no key.
/// </summary>
public sealed record ProviderInfo(
    string Id,
    string Name,
    string Mark,        // 2-letter logo mark
    string AccentHex,   // brand-ish accent for the mark chip
    string ConsoleUrl,  // exact "create an API key" page — empty = no key
    bool Stt,
    bool Llm,
    string GetKeyHint,  // 1-line "how to" shown in the add-key flow
    IReadOnlyList<ProviderModel> Models);

/// <summary>A representative model for a provider, with the capabilities it
/// serves. Used for the educational capability badges on the card.</summary>
public sealed record ProviderModel(string Name, bool Stt, bool Llm, bool Recap);

public static class ProviderCatalog
{
    /// <summary>Recap is an LLM operation — any LLM-capable provider can do it.</summary>
    public static bool Recap(this ProviderInfo p) => p.Llm;

    public static IReadOnlyList<ProviderInfo> All { get; } = new[]
    {
        new ProviderInfo("local", "On-device", "Lo", "#7C8CFF", "",
            true, true,
            "No key needed — runs fully on your machine, private and free.",
            new[]
            {
                new ProviderModel("Whisper / Parakeet (speech)", true, false, false),
                new ProviderModel("Gemma / Phi (rewrite & recap)", false, true, true),
            }),

        new ProviderInfo("groq", "Groq", "Gq", "#F55036", "https://console.groq.com/keys",
            true, true,
            "Sign up free, create an API key, paste it here. Free tier is plenty to start.",
            new[]
            {
                new ProviderModel("Whisper Large v3 turbo", true, false, false),
                new ProviderModel("Llama 3.3 70B", false, true, true),
            }),

        new ProviderInfo("openai", "OpenAI", "Ai", "#10A37F", "https://platform.openai.com/api-keys",
            true, true,
            "Create a key on the API keys page (needs a small balance for cloud STT).",
            new[]
            {
                new ProviderModel("Whisper-1 / gpt-4o-transcribe", true, false, false),
                new ProviderModel("GPT-4o / GPT-4o mini", false, true, true),
            }),

        new ProviderInfo("anthropic", "Anthropic", "An", "#D4A27F", "https://console.anthropic.com/settings/keys",
            false, true,
            "Create a key in the Anthropic console. Best for high-quality rewrite & recap.",
            new[]
            {
                new ProviderModel("Claude Haiku 4.5", false, true, true),
                new ProviderModel("Claude Sonnet / Opus", false, true, true),
            }),

        new ProviderInfo("gemini", "Google Gemini", "Ge", "#4285F4", "https://aistudio.google.com/apikey",
            true, true,
            "Get a free key in Google AI Studio — one click, no card required.",
            new[]
            {
                new ProviderModel("Gemini (speech)", true, false, false),
                new ProviderModel("Gemini 2.5 Flash / Pro", false, true, true),
            }),

        new ProviderInfo("deepgram", "Deepgram", "Dg", "#13EF93", "https://console.deepgram.com/",
            true, false,
            "Create a key in the Deepgram console — fast, accurate speech-to-text.",
            new[]
            {
                new ProviderModel("Nova (speech)", true, false, false),
            }),

        new ProviderInfo("openrouter", "OpenRouter", "Or", "#6566F1", "https://openrouter.ai/keys",
            false, true,
            "One key unlocks many models for rewrite & recap. Free tier available.",
            new[]
            {
                new ProviderModel("Any LLM (routed)", false, true, true),
            }),

        new ProviderInfo("fireworks", "Fireworks", "Fw", "#6B2FFF", "https://fireworks.ai/account/api-keys",
            true, true,
            "Create a key in the Fireworks dashboard.",
            new[]
            {
                new ProviderModel("Whisper (speech)", true, false, false),
                new ProviderModel("Llama / Qwen (rewrite & recap)", false, true, true),
            }),

        new ProviderInfo("together", "Together AI", "Tg", "#0F6FFF", "https://api.together.ai/settings/api-keys",
            false, true,
            "Create a key in the Together dashboard.",
            new[]
            {
                new ProviderModel("Llama / Qwen (rewrite & recap)", false, true, true),
            }),

        new ProviderInfo("custom", "Custom (OpenAI-compatible)", "Cu", "#9AA0AC", "",
            true, true,
            "Point Dimmy at any OpenAI-compatible endpoint. Enter the base URL + key.",
            new[]
            {
                new ProviderModel("Your endpoint", true, true, true),
            }),
    };

    public static ProviderInfo? ById(string id) =>
        All.FirstOrDefault(p => string.Equals(p.Id, id, StringComparison.OrdinalIgnoreCase));
}
