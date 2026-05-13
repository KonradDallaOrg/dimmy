import Foundation

// MARK: - TelemetryBuckets
//
// Swift mirror of `platforms/windows/Dimmy.Windows/Services/TelemetryBuckets.cs`
// and `core/src/telemetry/sanitize.rs`. The Rust dispatcher validates
// each prop against a categorical allowlist — we MUST produce
// identical strings here so unknown values don't degrade to "unknown".
//
// Privacy contract: telemetry events ship buckets, never the raw
// counts/durations. A precise number can identify the only user
// with that specific value. Boundaries are PostHog-dashboard-load-
// bearing — do not change without a coordinated rename across Rust
// + Win + Mac.

enum TelemetryBuckets {
    static func audioSecs(_ secs: Double) -> String {
        switch secs {
        case ..<30: return "lt_30"
        case ..<120: return "30_120"
        case ..<600: return "120_600"
        case ..<1800: return "600_1800"
        case ..<3600: return "1800_3600"
        default: return "ge_3600"
        }
    }

    static func processingMs(_ ms: Int64) -> String {
        switch ms {
        case ..<500: return "lt_500"
        case ..<2000: return "500_2000"
        case ..<10000: return "2000_10000"
        case ..<60000: return "10000_60000"
        default: return "ge_60000"
        }
    }

    static func wordCount(_ n: Int) -> String {
        switch n {
        case ..<1: return "0"
        case 1...50: return "1_50"
        case 51...200: return "51_200"
        case 201...1000: return "201_1000"
        case 1001...5000: return "1001_5000"
        default: return "ge_5000"
        }
    }

    static func dictSize(_ n: Int) -> String {
        switch n {
        case ..<1: return "0"
        case 1...5: return "1_5"
        case 6...20: return "6_20"
        case 21...100: return "21_100"
        default: return "ge_100"
        }
    }

    static func appRules(_ n: Int) -> String {
        switch n {
        case ..<1: return "0"
        case 1...5: return "1_5"
        case 6...20: return "6_20"
        default: return "ge_20"
        }
    }

    /// Family-bucket a recap model id so dashboards survive vendor
    /// version bumps. Mirrors `TelemetryBuckets.RecapModel` (Win) +
    /// `bucket_recap_model` (Rust).
    static func recapModel(_ model: String?) -> String {
        guard let model, !model.isEmpty else { return "default" }
        let l = model.lowercased()
        if l.contains("opus") { return "opus" }
        if l.contains("sonnet") { return "sonnet" }
        if l.contains("haiku") { return "haiku" }
        if l.contains("gemini-2.5-pro") || l.contains("gemini-3-pro") || l.contains("gemini-3.1-pro") {
            return "gemini_pro"
        }
        if l.contains("gemini-2.5-flash") || l.contains("gemini-3-flash") || l.contains("gemini-3.1-flash") {
            return "gemini_flash"
        }
        if l.contains("gpt-5") { return "gpt_5" }
        if l.contains("gpt-4") { return "gpt_4" }
        if l.contains("llama") { return "llama" }
        if l.contains("gemma") { return "gemma" }
        return "other"
    }

    /// Provider tag from a recap-model URL or provider config field.
    /// Maps to the Rust allowlist; anything off-list degrades to
    /// "unset" so PostHog never sees a free-form value.
    static func provider(_ url: String?) -> String {
        guard let url, !url.isEmpty else { return "unset" }
        let l = url.lowercased()
        if l.contains("groq.com") { return "groq" }
        if l.contains("openai.com") { return "openai" }
        if l.contains("anthropic.com") { return "anthropic" }
        if l.contains("googleapis.com") { return "gemini" }
        if l.contains("openrouter.ai") { return "openrouter" }
        if l.contains("localhost") || l.contains("127.0.0.1") { return "local" }
        return "unset"
    }
}
