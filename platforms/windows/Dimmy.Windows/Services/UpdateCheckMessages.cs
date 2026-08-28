namespace Dimmy.Windows.Services;

/// <summary>What one check-and-download pass concluded. The background
/// loop ignores this; the Settings "Check for updates" button needs it
/// to tell the user something concrete instead of leaving the UI
/// unchanged when the answer is "you're already current".</summary>
public enum UpdateCheckOutcome
{
    /// <summary>Poll succeeded, running version is the newest one on the channel.</summary>
    UpToDate,
    /// <summary>Newer version found AND downloaded; ready to apply.</summary>
    UpdateReady,
    /// <summary>auto_update license scope absent — nothing was polled.</summary>
    NoLicense,
    /// <summary>Unpackaged dev build; Velopack owns no metadata to diff against.</summary>
    DevBuild,
    /// <summary>Network / GitHub / manifest failure.</summary>
    Failed,
}

/// <summary>Outcome of one pass, with the version string when one is
/// relevant (the pending version for UpdateReady, the running version
/// for UpToDate) and the error text for Failed.</summary>
public readonly record struct UpdateCheckResult(
    UpdateCheckOutcome Outcome,
    string Version,
    string? Error);

/// <summary>
/// Maps a check outcome to the line the About page shows under the
/// "Check for updates" button. Pure so the mapping is unit-testable —
/// the test project links this file; it cannot link the
/// Velopack-bound UpdateService or the WinUI-bound SettingsWindow.
/// Same shape as <see cref="DictFailureHints"/>.
///
/// <c>Ok</c> picks the checkmark vs the warning glyph: the distinction
/// users need at a glance is "nothing to do" vs "something went
/// wrong", not the five internal outcome codes.
/// </summary>
public static class UpdateCheckMessages
{
    /// <summary>Label for the channel the check ran against, so
    /// "you're on the latest" says WHICH latest. A pre-release user
    /// shown a stable version number would read it as a stuck check.</summary>
    public static string ChannelLabel(string? channel) =>
        channel == "prerelease" ? "stable + pre-release" : "stable";

    public static (bool Ok, string Message) For(
        UpdateCheckOutcome outcome, string version, string? channel)
    {
        var chan = ChannelLabel(channel);
        return outcome switch
        {
            UpdateCheckOutcome.UpdateReady => (true,
                string.IsNullOrEmpty(version)
                    ? "An update is downloaded and ready to install."
                    : $"Dimmy v{version} downloaded and ready to install."),
            UpdateCheckOutcome.UpToDate => (true,
                string.IsNullOrEmpty(version)
                    ? $"You're on the latest {chan} version."
                    : $"You're on the latest {chan} version (v{version})."),
            UpdateCheckOutcome.DevBuild => (false,
                "This is a development build - in-app updates only work in an installed copy."),
            UpdateCheckOutcome.NoLicense => (false,
                "In-app updates need an active plan."),
            _ => (false,
                "Couldn't reach the update server. Check your connection and try again."),
        };
    }
}
