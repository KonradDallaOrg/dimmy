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
    /// release only) or "prerelease" (stable + rc builds from
    /// release.yml). Both are PROD-flavor only; staging-native builds
    /// are NOT offered here (staging-auto-update.yml withholds the
    /// Velopack manifest by design — burned 2026-06-16). The user picks
    /// in Settings → About; UpdateService reads it on every
    /// BackgroundCheckAsync to decide whether to set
    /// GithubSource.prerelease=true.</summary>
    public string UpdateChannel { get; set; } = "stable";

    /// <summary>Global hotkey for "add selected text to user dictionary"
    /// (Wispr Flow-style). Same combo grammar as the main hotkey
    /// (ctrl/shift/alt/win + single letter). Default Ctrl+Shift+D.
    /// Editable in Settings → Voice input → Dictionary section. Lives
    /// here rather than config.json because it's a Win-only UI knob
    /// — the Rust core has no opinion on which key adds to the dict,
    /// only on what's IN the dict.</summary>
    public string DictHotkey { get; set; } = "ctrl+shift+d";

    /// <summary>Optional dedicated global hotkey that fires a ONE-SHOT
    /// command-mode recording (then reverts to normal output), as opposed
    /// to the pill-menu toggle which is a sticky mode. Same combo grammar
    /// as the dictation + dictionary hotkeys. EMPTY by default — command
    /// mode works via the menu toggle out of the box; the dedicated hotkey
    /// is opt-in so we never grab a global key the user didn't ask for.
    /// Win-only UI knob, hence here and not in config.json.</summary>
    public string CommandHotkey { get; set; } = "";

    /// <summary>If true, Dimmy pins its system-tray icon to the always-visible
    /// notification area (next to wifi / volume / clock) by setting IsPromoted
    /// on its Win11 NotifyIconSettings entry, so the user doesn't have to drag
    /// it out of the overflow flyout manually. Default false — opt-in, and we
    /// never demote on a default-off startup so a manual Windows pin is left
    /// alone. Best-effort: the registry surface is unsupported and may need
    /// re-applying after some Windows updates.</summary>
    public bool TrayIconAlwaysVisible { get; set; } = false;

    /// <summary>Optional folder to also copy each finished recap.md into, as
    /// `&lt;title&gt; (&lt;meeting-id&gt;).md`. Lets the user point at an
    /// Obsidian vault or a Google Drive / Dropbox / OneDrive sync folder so
    /// recaps land in their notes / cloud for free (no OAuth). Empty =
    /// disabled. Win-only convenience, hence here and not in config.json.</summary>
    public string RecapExportFolder { get; set; } = "";

    // Namespace-aware: the config dir comes from the Rust core via
    // BuildInfo.ConfigDirPath (dimmy_config_dir_name FFI). NEVER hardcode
    // "dimmy" — a staging install (dimmy-staging) keeps its OWN prefs.
    // Hardcoding "dimmy" leaked the prod install's prefs (ConnectedProviders,
    // RecapExportFolder, UpdateChannel, DictHotkey) into the staging app —
    // the Providers page showed prod's connected vendors while staging's
    // keystore only had two (flavor != config dir, since 2026-05-16).
    private static string PrefsPath =>
        Path.Combine(BuildInfo.ConfigDirPath, "ui_prefs.json");

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
