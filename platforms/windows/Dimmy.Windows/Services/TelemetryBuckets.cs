namespace Dimmy.Windows.Services;

/// <summary>
/// C# mirror of the bucket helpers in
/// <c>core/src/telemetry/sanitize.rs</c>. The Rust dispatcher
/// validates each prop against a categorical allowlist — we MUST
/// produce identical strings here so unknown values don't get
/// dropped to "unknown".
///
/// Privacy contract: telemetry events ship buckets, never the raw
/// counts/durations themselves. A precise count can identify the
/// only user with that specific number. Boundaries are PostHog-
/// dashboard-load-bearing — do not change without a coordinated
/// rename.
/// </summary>
public static class TelemetryBuckets
{
    public static string AudioSecs(double secs) => secs switch
    {
        < 30 => "lt_30",
        < 120 => "30_120",
        < 600 => "120_600",
        < 1800 => "600_1800",
        < 3600 => "1800_3600",
        _ => "ge_3600",
    };

    public static string ProcessingMs(long ms) => ms switch
    {
        < 500 => "lt_500",
        < 2_000 => "500_2000",
        < 10_000 => "2000_10000",
        < 60_000 => "10000_60000",
        _ => "ge_60000",
    };

    public static string WordCount(int n) => n switch
    {
        <= 0 => "0",
        <= 50 => "1_50",
        <= 200 => "51_200",
        <= 1000 => "201_1000",
        <= 5000 => "1001_5000",
        _ => "ge_5000",
    };

    public static string DictSize(int n) => n switch
    {
        <= 0 => "0",
        <= 5 => "1_5",
        <= 20 => "6_20",
        <= 100 => "21_100",
        _ => "ge_100",
    };

    public static string AppRules(int n) => n switch
    {
        <= 0 => "0",
        <= 5 => "1_5",
        <= 20 => "6_20",
        _ => "ge_20",
    };

    /// <summary>
    /// Bucket a raw model id ("claude-opus-4-7", "gpt-5-mini-…")
    /// to a stable family name so dashboards survive vendor
    /// version bumps. Mirrors the Rust `bucket_recap_model`.
    /// </summary>
    public static string RecapModel(string? model)
    {
        if (string.IsNullOrEmpty(model)) return "default";
        var lower = model.ToLowerInvariant();
        if (lower.Contains("opus")) return "opus";
        if (lower.Contains("sonnet")) return "sonnet";
        if (lower.Contains("haiku")) return "haiku";
        if (lower.Contains("gemini-2.5-pro") || lower.Contains("gemini-3-pro") || lower.Contains("gemini-3.1-pro")) return "gemini_pro";
        if (lower.Contains("gemini-2.5-flash") || lower.Contains("gemini-3-flash") || lower.Contains("gemini-3.1-flash")) return "gemini_flash";
        if (lower.Contains("gpt-5")) return "gpt_5";
        if (lower.Contains("gpt-4")) return "gpt_4";
        if (lower.Contains("llama")) return "llama";
        if (lower.Contains("gemma")) return "gemma";
        return "other";
    }

    /// <summary>
    /// Provider name from a recap-model URL or `provider` config
    /// field, mapped to the Rust allowlist. Returns "unset" for
    /// empty input. The Rust dispatcher rejects anything off this
    /// list — passing unknown lowers the event to "unknown" on the
    /// Rust side.
    /// </summary>
    public static string Provider(string? url)
    {
        if (string.IsNullOrEmpty(url)) return "unset";
        var lower = url.ToLowerInvariant();
        if (lower.Contains("groq.com")) return "groq";
        if (lower.Contains("openai.com")) return "openai";
        if (lower.Contains("anthropic.com")) return "anthropic";
        if (lower.Contains("googleapis.com")) return "gemini";
        if (lower.Contains("openrouter.ai")) return "openrouter";
        if (lower.Contains("localhost") || lower.Contains("127.0.0.1")) return "local";
        return "unset";
    }
}
