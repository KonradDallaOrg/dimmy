using System;
using System.Text.Json;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Terms of Service / EULA acceptance. The core owns the version
/// (<c>dimmy_terms_version</c>) and the stored answer
/// (<c>terms_accepted_version</c>); this only asks and records. Accepting is
/// a plain config write, so the single-writer rule holds and the core ignores
/// an empty value: no save from here or from Settings can un-accept.
/// </summary>
public static class TermsAcceptance
{
    public const string Url = "https://dimmy.app/terms/";

    /// <summary>True when the user already accepted the CURRENT terms version.</summary>
    public static bool IsCurrent()
    {
        try
        {
            var current = DimmyNative.TermsVersion();
            if (string.IsNullOrEmpty(current)) return true; // no version to ask about
            var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 65536);
            if (string.IsNullOrEmpty(json)) return false;
            using var doc = JsonDocument.Parse(json);
            return doc.RootElement.TryGetProperty("terms_accepted_version", out var v)
                && v.GetString() == current;
        }
        catch (Exception ex)
        {
            App.Log($"terms check failed: {ex.Message}", "Terms");
            return false;
        }
    }

    /// <summary>Record that the user accepted the current terms version.</summary>
    public static void Accept()
    {
        var current = DimmyNative.TermsVersion();
        if (string.IsNullOrEmpty(current)) return;
        var json = JsonSerializer.Serialize(new { terms_accepted_version = current });
        DimmyNative.dimmy_set_config_json(json);
    }
}
