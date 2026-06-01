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

    /// <summary>If true, Dimmy registers a button on the Windows taskbar
    /// (`TaskbarAnchorWindow`) with the brand icon, the amplitude overlay
    /// during recording, and the right-click jump-list shortcuts. If
    /// false, the anchor window stays hidden and Dimmy is reachable only
    /// via the system tray + global hotkey. Default true — most users
    /// want the taskbar affordance because of the amplitude bar feedback
    /// during recording. Live-applied: toggling the switch shows/hides
    /// the entry without restart.</summary>
    public bool ShowTaskbarIcon { get; set; } = true;

    /// <summary>Last email the user entered in the pre-checkout / activate
    /// modal. Persisted across Sign out so the user doesn't have to re-type
    /// it every time. Distinct from license.json (which Sign out drops):
    /// this is just a UX convenience pre-fill, no auth weight.</summary>
    public string? BuyerEmail { get; set; }

    /// <summary>Settings-window theme: "Default" (system / auto), "Light",
    /// or "Dark". Lives here instead of config.json because the Rust core
    /// has no Theme field — pushing it via dimmy_set_config_json silently
    /// drops on Rust's re-serialise, which is the root cause of the
    /// "set Light → reverts to Auto on reopen" bug.</summary>
    public string Theme { get; set; } = "Default";

    /// <summary>Auto-update channel: "stable" (default — Latest GitHub
    /// release only) or "prerelease" (also offers staging-native builds
    /// off the staging-latest tag). The user picks in Settings → About;
    /// UpdateService reads it on every BackgroundCheckAsync to decide
    /// whether to set GithubSource.prerelease=true.</summary>
    public string UpdateChannel { get; set; } = "stable";

    /// <summary>Provider ids the user has connected (saved a key for) via the
    /// Providers &amp; keys page. UI-state mirror used to show the "connected"
    /// badge on reload — the encrypted keystore is the source of truth for the
    /// key itself, this just remembers which cards to light up. Seeded from the
    /// current STT/LLM provider on first build so existing setups appear
    /// connected. Win-only display state, hence here and not in config.json.</summary>
    public System.Collections.Generic.List<string> ConnectedProviders { get; set; } = new();

    /// <summary>Global hotkey for "add selected text to user dictionary"
    /// (Wispr Flow-style). Same combo grammar as the main hotkey
    /// (ctrl/shift/alt/win + single letter). Default Ctrl+Shift+D.
    /// Editable in Settings → Voice input → Dictionary section. Lives
    /// here rather than config.json because it's a Win-only UI knob
    /// — the Rust core has no opinion on which key adds to the dict,
    /// only on what's IN the dict.</summary>
    public string DictHotkey { get; set; } = "ctrl+shift+d";

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
