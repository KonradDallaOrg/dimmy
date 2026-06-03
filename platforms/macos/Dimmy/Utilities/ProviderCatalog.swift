import Foundation

// MARK: - ProviderCatalog
//
// Mac port of `platforms/windows/Dimmy.Windows/Services/ProviderCatalog.cs`
// (commit fcf03f67 on `feat/settings-redesign`).
//
// Static catalog of the AI providers Dimmy can talk to, plus the data
// the Providers settings page needs: a display mark, an accent colour,
// the exact console URL where a user creates an API key (the deep-link
// that makes key acquisition foolproof), and the real, complete list
// of models that provider offers, matching the pickers in Voice input
// and Output. Not placeholders, if a model isn't pickable elsewhere
// it doesn't belong here either.
//
// This drives the Providers & keys page. It is pure data, it does NOT
// change the Rust core or the keystore. Keys still go through
// dimmy_save_llm_provider_key.
//
// Capability truth (matches core/src/provider.rs):
//   - Deepgram: STT only.
//   - Anthropic, OpenRouter: LLM/Recap only (no STT endpoint).
//   - Groq, OpenAI, Gemini, Fireworks, Together: STT + LLM + Recap
//   - On-device: all three, no key.

struct ProviderModel: Hashable {
    let name: String
    let stt: Bool
    let llm: Bool
    let recap: Bool
}

struct ProviderInfo: Hashable {
    let id: String
    let name: String
    let mark: String           // 2-letter logo mark (legacy fallback)
    let accentHex: String      // brand accent
    let consoleUrl: String     // exact "create an API key" page, empty = no key
    let stt: Bool
    let llm: Bool
    let getKeyHint: String     // 1-line "how to" shown in the add-key flow
    let models: [ProviderModel]
}

enum ProviderCatalog {
    // Capability shorthands for the model tables.
    private static func S(_ name: String) -> ProviderModel {
        ProviderModel(name: name, stt: true, llm: false, recap: false)
    }
    private static func L(_ name: String) -> ProviderModel {
        ProviderModel(name: name, stt: false, llm: true, recap: true)
    }
    private static func SLR(_ name: String) -> ProviderModel {
        ProviderModel(name: name, stt: true, llm: true, recap: true)
    }

    /// Vendors whose LLM/recap key the keystore FFI accepts. Anthropic
    /// and OpenRouter are LLM-only; Deepgram is not here (STT only);
    /// Custom and Local are configured elsewhere.
    private static let llmKeyVendors: Set<String> = [
        "groq", "openai", "anthropic", "gemini", "openrouter", "fireworks", "together",
    ]

    /// Vendors whose STT key the FFI accepts (mirrors
    /// Provider::supports_stt in core/src/provider.rs). Deepgram is
    /// here (STT only). Custom needs a URL so it's keyed on Voice/Output.
    private static let sttKeyVendors: Set<String> = [
        "groq", "openai", "gemini", "deepgram", "fireworks", "together",
    ]

    /// Recap is an LLM operation, any LLM-capable provider can do it.
    static func recap(_ p: ProviderInfo) -> Bool { p.llm }

    /// The keystore scopes the Providers page writes the single key
    /// into, matching what dimmy_save_llm_provider_key accepts per
    /// vendor. STT-capable vendors get "stt"; LLM-capable vendors get
    /// "llm" + "recap". Empty when the provider can't be keyed here
    /// (Custom needs a URL, On-device needs no key).
    static func keySaveScopes(_ p: ProviderInfo) -> [String] {
        var scopes: [String] = []
        if p.stt && sttKeyVendors.contains(p.id) { scopes.append("stt") }
        if p.llm && llmKeyVendors.contains(p.id) {
            scopes.append("llm")
            scopes.append("recap")
        }
        return scopes
    }

    /// True when a key for this provider can be saved from the
    /// Providers page (it has at least one FFI-accepted scope).
    /// Deepgram is now keyable (stt). Custom and On-device are not.
    static func isKeyableHere(_ p: ProviderInfo) -> Bool {
        !keySaveScopes(p).isEmpty
    }

    static let all: [ProviderInfo] = [
        ProviderInfo(
            id: "local", name: "On-device", mark: "Lo",
            accentHex: "#7C8CFF", consoleUrl: "",
            stt: true, llm: true,
            getKeyHint: "No key needed, runs fully on your machine, private and free. Models download on demand.",
            models: [
                S("Whisper Tiny, 42 MB"),
                S("Whisper Base, 78 MB, default"),
                S("Whisper Small, 181 MB"),
                S("Whisper Medium, 514 MB"),
                S("Whisper Large-v3-Turbo Q5, 574 MB"),
                S("Whisper Large-v3-Turbo Q8, 874 MB"),
                S("Whisper Large-v3 Q5, 1.1 GB"),
                S("Distil-Large-v3.5 Q8 EN, 818 MB"),
                S("Distil-Large-v3.5 Q5 EN, 538 MB"),
                S("Parakeet TDT v3, 2.5 GB"),
                L("Gemma 4 E2B Q4, 3.1 GB"),
                L("Gemma 4 E2B Q5, 3.7 GB"),
                L("Gemma 4 E4B Q3, 4.1 GB"),
                L("Gemma 4 E4B Q4, 5.0 GB"),
                L("Gemma 4 E4B Q8, 8.2 GB"),
                L("Phi-4 Mini Q4, 2.5 GB, default"),
            ]
        ),

        ProviderInfo(
            id: "groq", name: "Groq", mark: "Gq",
            accentHex: "#F55036",
            consoleUrl: "https://console.groq.com/keys",
            stt: true, llm: true,
            getKeyHint: "Sign up free, create an API key, paste it here. Free tier is plenty to start.",
            models: [
                S("whisper-large-v3-turbo, free"),
                S("whisper-large-v3, free"),
                L("llama-3.3-70b, free"),
                L("gpt-oss-120b, top quality"),
                L("llama-4-scout-17b, MoE, balanced"),
                L("llama-3.1-8b, instant"),
                L("qwen3-32b, multilingual"),
            ]
        ),

        ProviderInfo(
            id: "openai", name: "OpenAI", mark: "Ai",
            accentHex: "#10A37F",
            consoleUrl: "https://platform.openai.com/api-keys",
            stt: true, llm: true,
            getKeyHint: "Create a key on the API keys page (needs a small balance for cloud STT).",
            models: [
                S("whisper-1"),
                S("gpt-4o-transcribe"),
                S("gpt-4o-mini-transcribe"),
                L("gpt-5.5, latest"),
                L("gpt-5.4-mini, fast"),
                L("gpt-5.4-nano, fastest"),
                L("gpt-5.1"),
                L("gpt-5"),
                L("gpt-5-mini"),
                L("gpt-5-nano, fastest"),
                L("gpt-4o, legacy"),
                L("gpt-4o-mini, legacy"),
                L("o3, reasoning, deep"),
                L("o3-mini, reasoning, fast"),
            ]
        ),

        ProviderInfo(
            id: "anthropic", name: "Anthropic", mark: "An",
            accentHex: "#D4A27F",
            consoleUrl: "https://console.anthropic.com/settings/keys",
            stt: false, llm: true,
            getKeyHint: "Create a key in the Anthropic console. Best for high-quality rewrite and recap.",
            models: [
                L("claude-opus-4.8, top"),
                L("claude-opus-4.7"),
                L("claude-sonnet-4.6, balanced"),
                L("claude-haiku-4.5, fast"),
            ]
        ),

        ProviderInfo(
            id: "gemini", name: "Google Gemini", mark: "Ge",
            accentHex: "#4285F4",
            consoleUrl: "https://aistudio.google.com/apikey",
            stt: true, llm: true,
            getKeyHint: "Get a free key in Google AI Studio, one click, no card required.",
            models: [
                L("gemini-3.5-flash, newest fast"),
                L("gemini-3.1-pro-preview, top"),
                SLR("gemini-3.1-flash-lite, fast"),
                SLR("gemini-3-flash-preview"),
                L("gemini-2.5-pro, stable top"),
                SLR("gemini-2.5-flash, stable fast"),
            ]
        ),

        ProviderInfo(
            id: "deepgram", name: "Deepgram", mark: "Dg",
            accentHex: "#13EF93",
            consoleUrl: "https://console.deepgram.com/",
            stt: true, llm: false,
            getKeyHint: "Create a key in the Deepgram console, fast and accurate speech-to-text.",
            models: [
                S("nova-3"),
                S("nova-2"),
            ]
        ),

        ProviderInfo(
            id: "openrouter", name: "OpenRouter", mark: "Or",
            accentHex: "#6566F1",
            consoleUrl: "https://openrouter.ai/keys",
            stt: false, llm: true,
            getKeyHint: "One key unlocks many models for rewrite and recap. Free tier available.",
            models: [
                L("llama-3.3-70b, free"),
                L("deepseek-r1, free"),
            ]
        ),

        ProviderInfo(
            id: "fireworks", name: "Fireworks", mark: "Fw",
            accentHex: "#6B2FFF",
            consoleUrl: "https://fireworks.ai/account/api-keys",
            stt: true, llm: true,
            getKeyHint: "Create a key in the Fireworks dashboard.",
            models: [
                S("whisper-v3-turbo"),
                L("kimi-k2"),
            ]
        ),

        ProviderInfo(
            id: "together", name: "Together AI", mark: "Tg",
            accentHex: "#0F6FFF",
            consoleUrl: "https://api.together.ai/settings/api-keys",
            stt: true, llm: true,
            getKeyHint: "Create a key in the Together dashboard.",
            models: [
                S("parakeet-tdt-0.6b-v3"),
                S("whisper-large-v3"),
                L("llama-3.3-70b"),
                L("qwen-2.5-7b"),
            ]
        ),

        ProviderInfo(
            id: "custom", name: "Custom (OpenAI-compatible)", mark: "Cu",
            accentHex: "#9AA0AC", consoleUrl: "",
            stt: true, llm: true,
            getKeyHint: "Point Dimmy at any OpenAI-compatible endpoint. Enter the base URL and key on the Voice input or Output page.",
            models: [
                SLR("Any OpenAI-compatible model you configure"),
            ]
        ),
    ]

    static func byId(_ id: String) -> ProviderInfo? {
        all.first { $0.id.caseInsensitiveCompare(id) == .orderedSame }
    }
}
