using System;
using System.Text.Json;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Usage statistics and crash reports consent. The core saves the answer
/// (<c>analytics_consent</c>, <c>crash_reports_consent</c>) and sends nothing
/// until it is a yes; this only asks.
/// </summary>
public static class StatsConsent
{
    /// <summary>True when the user has not answered yet.</summary>
    public static bool NeedsAsking()
    {
        try
        {
            var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 65536);
            if (string.IsNullOrEmpty(json)) return false;
            using var doc = JsonDocument.Parse(json);
            return !doc.RootElement.TryGetProperty("analytics_consent", out var v)
                || v.ValueKind == JsonValueKind.Null;
        }
        catch (Exception ex)
        {
            App.Log($"consent check failed: {ex.Message}", "Telemetry");
            return false;
        }
    }

    public static void Answer(bool yes)
    {
        DimmyNative.TelemetryEnabled = yes;
        DimmyNative.CrashReportsEnabled = yes;
    }
}
