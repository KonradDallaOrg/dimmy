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
    // Order of precedence (mirrors Win MeetingWindow.PickRecapModel):
    //   1. Explicit `recap_model_override` — with stale Gemini id
    //      migration so an old saved value doesn't 404 at recap time.
    //   2. Auto: walk the user's connected providers (LLM scope OR
    //      Recap scope OR keystore booleans) and pick the best flagship
    //      among them: Anthropic Opus > OpenAI GPT-5 > Gemini 3.1 Pro
    //      Preview. The Win side reaches the SAME hierarchy via
    //      `dimmy_get_config_json` flags — Mac reads them off the same
    //      FFI snapshot here.
    //   3. Empty — Rust falls back to `llm_api_model` (whatever the
    //      user's dictation model is). Honest errors then guide if the
    //      tier can't handle the recap size.
    //
    // CRITICAL: the `has_<vendor>_*key` flags live in the LIVE FFI
    // snapshot (`dimmy_get_config_json`), NOT in the config.json FILE
    // — the Rust core computes them and strips them on save. Reading
    // the file directly (the old behaviour) would see all-false and
    // Auto would pick nothing. Mirror of the Win fix (eefd3b49).
    let snapshot = DimmyCore.shared.getConfig() ?? [:]

    if let override = snapshot["recap_model_override"] as? String,
       !override.trimmingCharacters(in: .whitespaces).isEmpty,
       override != RecapModelOption.autoKey {
        return migrateRecapModelId(override.trimmingCharacters(in: .whitespaces))
    }

    func boolFlag(_ key: String) -> Bool {
        (snapshot[key] as? Bool) == true
    }

    if boolFlag("has_anthropic_llm_key") || boolFlag("has_anthropic_recap_key") {
        return "claude-opus-4-8"
    }
    if boolFlag("has_openai_key") || boolFlag("has_openai_llm_key") || boolFlag("has_openai_recap_key") {
        return "gpt-5.5"
    }
    if boolFlag("has_gemini_key") || boolFlag("has_gemini_llm_key") || boolFlag("has_gemini_recap_key") {
        return "gemini-3.1-pro-preview"
    }
    // No premium provider connected → return empty so the Rust core
    // uses the user's `llm_api_model`. With no LLM at all the call
    // fails with an honest error, which is preferable to a silent
    // success of a path that was always going to fail.
    return ""
}

/// Migrate stale recap model ids saved by older builds to their current
/// valid form. Mirror of the Win `MigrateRecapModelId`. Same fix as the
/// `LlmApiModel` migration: the recap dropdown switched the Gemini Pro
/// id to the `-preview` suffix, so a config that still carries the
/// bare id 404s with "models/gemini-3.1-pro is not found".
func migrateRecapModelId(_ id: String) -> String {
    switch id {
    case "gemini-3.1-pro":   return "gemini-3.1-pro-preview"
    case "gemini-3-1-pro":   return "gemini-3.1-pro-preview"
    case "gemini-3-pro":     return "gemini-3-pro-preview"
    default: return id
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

        // Anthropic. Opus 4.8 is the current flagship (May 2026) and
        // is API-compatible with the 4.7 adaptive-thinking path; 4.7
        // remains callable for users pinned to it. Sonnet 4.6 and
        // Haiku 4.5 are still the current Sonnet / Haiku tier — no
        // newer point release exists at the time of this curation.
        .init(id: "claude-opus-4-8",   label: "Anthropic — Claude Opus 4.8 (best)",     provider: .anthropic),
        .init(id: "claude-opus-4-7",   label: "Anthropic — Claude Opus 4.7",            provider: .anthropic),
        .init(id: "claude-sonnet-4-6", label: "Anthropic — Claude Sonnet 4.6 (balanced)", provider: .anthropic),
        .init(id: "claude-haiku-4-5",  label: "Anthropic — Claude Haiku 4.5 (fast)",    provider: .anthropic),

        // Gemini 3.1 ships as preview-channel only — the bare `gemini-3.1-pro`
        // string 404s with "models/gemini-3.1-pro is not found"; the working
        // id is `-preview` suffixed. `migrateRecapModelId` covers stale
        // saved configs but the dropdown itself must save the right id.
        .init(id: "gemini-3.1-pro-preview",   label: "Google — Gemini 3.1 Pro (newest top)",   provider: .gemini),
        .init(id: "gemini-3.1-flash-lite",    label: "Google — Gemini 3.1 Flash (newest fast)", provider: .gemini),
        .init(id: "gemini-3-pro-preview",     label: "Google — Gemini 3 Pro",                  provider: .gemini),
        .init(id: "gemini-2.5-pro",           label: "Google — Gemini 2.5 Pro (stable)",       provider: .gemini),
        .init(id: "gemini-2.5-flash",         label: "Google — Gemini 2.5 Flash (stable fast)", provider: .gemini),

        // OpenAI. GPT-5.5 (Apr 2026) is the flagship; GPT-5.4 + 5.4-mini
        // (Mar 2026) are kept for users who prefer that tier or want
        // cheaper recap. Legacy gpt-5 / gpt-5-mini are still callable
        // for back-compat with saved configs. Drop-in compatible with
        // the existing /v1/chat/completions path.
        .init(id: "gpt-5.5",      label: "OpenAI — GPT-5.5 (best)",            provider: .openai),
        .init(id: "gpt-5.4",      label: "OpenAI — GPT-5.4",                   provider: .openai),
        .init(id: "gpt-5.4-mini", label: "OpenAI — GPT-5.4 mini (fast + cheap)", provider: .openai),
        .init(id: "gpt-5",        label: "OpenAI — GPT-5",                     provider: .openai),
        .init(id: "gpt-5-mini",   label: "OpenAI — GPT-5 mini",                provider: .openai),

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

    /// Asset Catalog imageset name in `Assets.xcassets/Providers/` for
    /// the real vendor logo. Empty when we don't have a vendor asset
    /// (auto / local / custom). The picker UI falls back to
    /// `iconName` (SF Symbol) for those.
    var assetName: String {
        switch provider {
        case .anthropic: return "anthropic"
        case .gemini: return "gemini"
        case .openai: return "openai"
        case .auto, .local, .custom: return ""
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
