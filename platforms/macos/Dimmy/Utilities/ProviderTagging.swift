import Foundation

/// Pure helpers for matching STT and LLM endpoints to a provider tag.
///
/// Lifted out of `MacOutputPage` so the same logic can be exercised
/// from `SelfTests` without standing up a SwiftUI surface.
///
/// The mapping is intentionally URL-prefix based — that's how staging's
/// `LlmPreset.iconAssetName` already classifies things, and it keeps
/// the rule "same vendor key can power both STT and LLM" easy to read.
enum ProviderTagging {

    /// Returns the vendor tag derived from a Dimmy STT or LLM URL.
    /// Empty string for unknown / custom URLs.
    ///
    /// Recognised tags:
    ///   `groq`, `openai`, `openrouter`, `gemini`, `anthropic`,
    ///   `fireworks`, `together`, `deepgram`, `""` (unknown / custom).
    ///
    /// The synthetic `claude-code://` URL is mapped to `anthropic`
    /// because the underlying service is Anthropic — both URLs share
    /// the same vendor key namespace, just different auth paths.
    static func providerTag(forUrl url: String) -> String {
        if url.hasPrefix("claude-code://") { return "anthropic" }
        if url.contains("groq.com") { return "groq" }
        if url.contains("openai.com") { return "openai" }
        if url.contains("openrouter.ai") { return "openrouter" }
        if url.contains("googleapis.com") { return "gemini" }
        if url.contains("anthropic.com") { return "anthropic" }
        if url.contains("fireworks.ai") { return "fireworks" }
        if url.contains("together.xyz") || url.contains("together.ai") { return "together" }
        if url.contains("deepgram.com") { return "deepgram" }
        return ""
    }

    /// Should the Settings UI render the "Use same key as STT" toggle?
    ///
    /// True only when:
    ///   - Both URLs resolve to a known vendor (no empty tags), AND
    ///   - The two vendors match, AND
    ///   - The LLM is not using subscription auth (which has its own
    ///     credential — sharing an STT key wouldn't help).
    ///
    /// False if either provider is unknown / custom, or if they differ,
    /// or if the LLM is on subscription auth.
    static func sameKeyShouldShow(sttUrl: String, llmUrl: String, llmAuthMethod: String) -> Bool {
        let stt = providerTag(forUrl: sttUrl)
        let llm = providerTag(forUrl: llmUrl)
        guard !stt.isEmpty, stt == llm, llmAuthMethod != "subscription" else {
            return false
        }
        return true
    }

    /// Vendor tag for a `recap_model_override` value. The recap picker
    /// stores model IDs directly (e.g. `claude-opus-4-7`, `gemini-2.5-pro`,
    /// `gpt-5`, `local:gemma-4-...gguf`), not URLs — so we map by model
    /// id prefix.
    ///
    /// Returns one of: `anthropic` / `gemini` / `openai` / `local` /
    /// `""` (auto / unknown / custom).
    static func providerTag(forRecapModel modelId: String) -> String {
        let trimmed = modelId.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return "" }                 // Auto
        if trimmed.hasPrefix("local:") { return "local" }
        let lower = trimmed.lowercased()
        if lower.hasPrefix("claude") { return "anthropic" }
        if lower.hasPrefix("gemini") { return "gemini" }
        if lower.hasPrefix("gpt") || lower.hasPrefix("o1") || lower.hasPrefix("o3") {
            return "openai"
        }
        return ""
    }
}
