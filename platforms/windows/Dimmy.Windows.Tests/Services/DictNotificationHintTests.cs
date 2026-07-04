using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

/// <summary>
/// Pins the category-to-hint mapping behind the "Transcription failed"
/// toast. The categories are the Rust telemetry sanitizer's buckets
/// (core/src/telemetry/sanitize.rs error_category) — if either side
/// renames a bucket, the toast silently degrades to the generic hint
/// and the user loses the actionable next step (the whole point of
/// the 2026-07-04 fix: four Groq HTTP 403 retried blind).
/// Mapping lives in DictFailureHints (pure, linkable here).
/// </summary>
public class DictNotificationHintTests
{
    [Theory]
    [InlineData("auth")]
    public void AuthFailuresPointAtTheApiKey(string category)
    {
        Assert.Contains("API key", DictFailureHints.For(category));
    }

    [Fact]
    public void RateLimitSaysRetry()
    {
        Assert.Contains("retry", DictFailureHints.For("rate_limit"));
    }

    [Theory]
    [InlineData("network")]
    [InlineData("timeout")]
    public void ConnectivityFailuresPointAtTheConnection(string category)
    {
        Assert.Contains("connection", DictFailureHints.For(category));
    }

    [Fact]
    public void UnknownCategoriesFallBackToTheLog()
    {
        Assert.Contains("dimmy.log", DictFailureHints.For("something_new"));
        Assert.Contains("dimmy.log", DictFailureHints.For(""));
    }

    [Fact]
    public void NoEmDashesInUserFacingHints()
    {
        // Hard rule: no em-dashes in UI copy (feedback_no_em_dashes_in_ui_copy).
        foreach (var c in new[] { "auth", "rate_limit", "network", "timeout", "model_load", "x" })
            Assert.DoesNotContain("—", DictFailureHints.For(c));
    }
}
