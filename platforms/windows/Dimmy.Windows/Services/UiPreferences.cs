using System;
using System.IO;
using System.Text.Json;

namespace Dimmy.Windows.Services;

/// <summary>
/// Tiny JSON-backed store for UI-only Windows preferences that don't
/// belong in the Rust core's `config.json` (which is reserved for
/// cross-platform settings — see CLAUDE.md "single-writer" rule).
///
/// Today this is just the pill-visibility toggles that decide whether
/// the floating pill should auto-show on launch / on hotkey, which
/// only matters now that the taskbar overlay icon provides the same
/// state feedback. If we add more Win-only knobs (e.g. "show in
/// taskbar"), they go here too.
///
/// Stored at `%APPDATA%\dimmy\ui_prefs.json` next to the existing
/// onboarding marker. Missing file → defaults; parse errors → defaults
/// (best-effort, never throws to the caller).
/// </summary>
public sealed class UiPreferences
{
    /// <summary>If true, pressing the global hotkey while the pill is
    /// hidden re-shows it (legacy behaviour). If false, the pill stays
    /// hidden even during recording — only the taskbar overlay icon
    /// signals state. Default true so we don't change behaviour for
    /// users who never visit the toggle.</summary>
    public bool PillShowOnHotkey { get; set; } = true;

    /// <summary>If true, the pill appears as soon as the app finishes
    /// starting up. If false, the app boots in "taskbar only" mode and
    /// the pill stays hidden until the user explicitly toggles it.
    /// Default true.</summary>
    public bool PillShowOnStartup { get; set; } = true;

    private static string PrefsPath
    {
        get
        {
            var dir = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy");
            return Path.Combine(dir, "ui_prefs.json");
        }
    }

    public static UiPreferences Load()
    {
        try
        {
            var path = PrefsPath;
            if (!File.Exists(path)) return new UiPreferences();
            var json = File.ReadAllText(path);
            return JsonSerializer.Deserialize<UiPreferences>(json) ?? new UiPreferences();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[UiPreferences] load failed: {ex.Message}");
            return new UiPreferences();
        }
    }

    public void Save()
    {
        try
        {
            var path = PrefsPath;
            var dir = Path.GetDirectoryName(path);
            if (!string.IsNullOrEmpty(dir))
                Directory.CreateDirectory(dir);
            var json = JsonSerializer.Serialize(this, new JsonSerializerOptions
            {
                WriteIndented = true,
            });
            File.WriteAllText(path, json);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[UiPreferences] save failed: {ex.Message}");
        }
    }
}
