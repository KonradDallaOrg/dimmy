namespace Dimmy.Windows.Services;

/// <summary>
/// Actionable next-step per failure category for the "Transcription
/// failed" toast. Categories are the Rust telemetry sanitizer's buckets
/// (core/src/telemetry/sanitize.rs error_category), carried on the
/// structured `error` event. Pure so the mapping is unit-testable
/// (the test project links this file; it cannot link the WinUI-bound
/// DictNotificationService).
/// </summary>
public static class DictFailureHints
{
    public static string For(string category) => category switch
    {
        "auth" => "Check your API key in Settings, Providers and keys.",
        "rate_limit" => "Provider rate limit hit. Wait a moment and retry.",
        "network" or "timeout" => "Check your internet connection.",
        "model_load" => "The local model failed to load. Re-download it in Settings.",
        _ => "Details are in dimmy.log (Settings, Advanced).",
    };
}
