using System;
using System.IO;

namespace Dimmy.Windows.Services;

/// Best-effort "also save the recap into my notes folder" export. After a
/// meeting's recap.md is persisted to its meeting dir, if the user has set
/// a <see cref="UiPreferences.RecapExportFolder"/> (Settings → Meetings),
/// copy recap.md there as `&lt;sanitized-title&gt; (&lt;meeting-id&gt;).md`.
///
/// Pointing the folder at an Obsidian vault or a Google Drive / Dropbox /
/// OneDrive sync folder gives free cloud + notes sync with no OAuth. The
/// meeting-id suffix keeps a REGENERATE of the same meeting overwriting its
/// own file, while never clobbering a different meeting that happens to share
/// a title.
///
/// The filename stem is produced by the pure, unit-tested
/// <see cref="Helpers.MeetingRecapHelpers.SanitizeRecapFileName"/>; this
/// service is the (untestable) IO shell around it.
public static class RecapExportService
{
    /// Copy `&lt;dir&gt;/recap.md` into the configured export folder, named
    /// from the meeting title. No-op — and NEVER throws — when no folder is
    /// set, recap.md is missing, or anything goes wrong (USB unplugged, cloud
    /// folder offline, permissions). The recap pipeline must not break because
    /// an external folder is unavailable.
    public static void TryExport(string dir, string? title)
    {
        try
        {
            if (string.IsNullOrEmpty(dir)) return;
            var folder = UiPreferences.Load().RecapExportFolder;
            if (string.IsNullOrWhiteSpace(folder)) return;

            var recapPath = Path.Combine(dir, "recap.md");
            if (!File.Exists(recapPath)) return;

            Directory.CreateDirectory(folder);
            var stem = Helpers.MeetingRecapHelpers.SanitizeRecapFileName(title);
            var meetingId = Path.GetFileName(dir.TrimEnd('\\', '/'));
            var fileName = string.IsNullOrEmpty(meetingId)
                ? $"{stem}.md"
                : $"{stem} ({meetingId}).md";
            var dest = Path.Combine(folder, fileName);
            File.Copy(recapPath, dest, overwrite: true);
            App.Log($"recap exported to {dest}", "RecapExport");
        }
        catch (Exception ex)
        {
            App.Log($"recap export failed: {ex.Message}", "RecapExport");
        }
    }
}
