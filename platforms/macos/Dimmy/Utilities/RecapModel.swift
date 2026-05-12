import Foundation

// MARK: - Recap model auto-pick + curated picker list
//
// Mirrors the Win-side `MeetingWindow.PickRecapModel` + the
// `SettingsWindow` recap-model card. Only the model NAME is overridden —
// the URL + key still come from the user's main LLM config. So if the
// user picked Anthropic in Settings the recap uses Opus by default;
// with Gemini it uses 2.5 Pro; with everything else (Groq, OpenAI,
// Together, Fireworks, OpenRouter) we keep their configured model —
// those are usually the right call already.
//
// `pickRecapModel` reads `config.json` directly (instead of round-
// tripping AppState) so it can be called from non-MainActor contexts
// like the auto-recap worker thread without paying for a hop back.
// It checks the user's `recap_model_override` first; if empty, falls
// back to the auto-detect-from-URL heuristic.

func pickRecapModel() -> String {
    do {
        // Path comes from DimmyCore.shared.configDirURL so the staging
        // flavor (`dimmy-staging/`) is honoured. Hardcoding "dimmy"
        // here would make staging builds read a stale prod config —
        // same class of bug as the Win app_rules wipe (2026-05-12).
        guard let cfgURL = DimmyCore.shared.configDirURL?
            .appendingPathComponent("config.json")
        else { return "" }
        let data = try Data(contentsOf: cfgURL)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return ""
        }
        if let override = json["recap_model_override"] as? String,
           !override.trimmingCharacters(in: .whitespaces).isEmpty,
           override != RecapModelOption.autoKey {
            return override
        }
        guard let url = json["llm_api_url"] as? String else { return "" }
        let lower = url.lowercased()
        if lower.contains("anthropic.com") { return "claude-opus-4-7" }
        if lower.contains("googleapis.com") { return "gemini-2.5-pro" }
        return ""
    } catch {
        return ""
    }
}

// MARK: - Curated dropdown options

/// Mirror of the Win SettingsWindow.xaml recap-model ComboBox. Each
/// option pairs a user-facing label, the model id sent to the LLM, and
/// the provider category (used to render the right SF Symbol / icon).
/// The "Auto" option carries the empty string so writes through the
/// `recap_model_override` config field clear any prior pick.
struct RecapModelOption: Identifiable, Equatable {
    let id: String   // model id (also the config value); empty = Auto
    let label: String
    let provider: Provider

    enum Provider: String {
        case auto, anthropic, gemini, openai, local, custom
    }

    /// The value persisted in `recap_model_override` for the "Auto"
    /// option. Stored explicitly so we can round-trip cleanly even if
    /// a future version stores the empty string differently.
    static let autoKey = ""

    /// User-curated list. Order matters — same as Win.
    /// Local options carry a `local:` prefix in their id so the Rust
    /// FFI (`dimmy_llm_call_raw`) can distinguish from cloud model
    /// names. Cloud options stay bare-string for backward compatibility
    /// with existing `recap_model_override` values.
    static let curated: [RecapModelOption] = [
        .init(id: autoKey, label: "Auto (match LLM provider)", provider: .auto),

        .init(id: "claude-opus-4-7",   label: "Anthropic — Claude Opus 4.7 (best)",   provider: .anthropic),
        .init(id: "claude-sonnet-4-6", label: "Anthropic — Claude Sonnet 4.6 (balanced)", provider: .anthropic),
        .init(id: "claude-haiku-4-5",  label: "Anthropic — Claude Haiku 4.5 (fast)",  provider: .anthropic),

        .init(id: "gemini-3.1-pro",   label: "Google — Gemini 3.1 Pro (best)",   provider: .gemini),
        .init(id: "gemini-3.1-flash", label: "Google — Gemini 3.1 Flash (fast)", provider: .gemini),
        .init(id: "gemini-2.5-pro",   label: "Google — Gemini 2.5 Pro",          provider: .gemini),
        .init(id: "gemini-2.5-flash", label: "Google — Gemini 2.5 Flash",        provider: .gemini),

        .init(id: "gpt-5",      label: "OpenAI — GPT-5 (best)",          provider: .openai),
        .init(id: "gpt-5-mini", label: "OpenAI — GPT-5 mini (fast + cheap)", provider: .openai),
        .init(id: "gpt-4o",     label: "OpenAI — GPT-4o",                provider: .openai),

        // Local Gemma — on-device via llama.cpp Metal. No network, no
        // API key, transcript never leaves the Mac. Quality is below
        // flagship cloud models but adequate for personal meeting
        // recaps. Recap latency on M-series ~30-90 s for typical
        // 5-10k char transcripts.
        .init(id: "local:gemma-4-E4B-it-Q4_K_M.gguf",
              label: "Local — Gemma 4 E4B Q4 (best local, ~6GB VRAM)",
              provider: .local),
        .init(id: "local:gemma-4-E2B-it-Q4_K_M.gguf",
              label: "Local — Gemma 4 E2B Q4 (fast, 4GB VRAM)",
              provider: .local),
        .init(id: "local:gemma-4-E4B-it-Q8_0.gguf",
              label: "Local — Gemma 4 E4B Q8 (max quality, 10GB+ VRAM)",
              provider: .local),
    ]

    /// Find the curated option for a given config value, or fall through
    /// to the Custom branch (id = the raw value, label = the value).
    /// Used by the dropdown to render the current selection.
    static func resolve(_ value: String) -> RecapModelOption {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return curated[0] }
        if let hit = curated.first(where: { $0.id == trimmed }) { return hit }
        return .init(id: trimmed, label: "Custom — \(trimmed)", provider: .custom)
    }

    var iconName: String {
        switch provider {
        case .auto: return "sparkles"
        case .anthropic: return "a.circle.fill"
        case .gemini: return "diamond.fill"
        case .openai: return "circle.hexagongrid.fill"
        case .local: return "cpu"
        case .custom: return "wrench.adjustable"
        }
    }

    var iconColor: String {
        // Symbolic name; consumer maps to a SwiftUI Color.
        switch provider {
        case .auto: return "accent"
        case .anthropic: return "orange"
        case .gemini: return "blue"
        case .openai: return "green"
        case .local: return "purple"
        case .custom: return "gray"
        }
    }
}
