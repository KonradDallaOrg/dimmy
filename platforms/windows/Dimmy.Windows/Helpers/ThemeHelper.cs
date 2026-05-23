using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Microsoft.Win32;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// Single source of truth for "what theme should this UI element show?".
///
/// Resolution order:
///   1. UiPreferences.Theme — user's explicit Settings choice
///      ("Light"|"Dark") wins.
///   2. UiPreferences.Theme = "Default" (or empty / unparseable)
///      → fall back to the Windows AppsUseLightTheme registry value.
///
/// We deliberately never trust just `Application.RequestedTheme` because
/// it tracks the system theme at process-start and does not honour our
/// user-pref override.
///
/// All callers (XAML root windows, pill MenuFlyout items, jump-list ICO
/// generation) should ask this helper instead of reading the registry
/// or the prefs file directly — keeps theme decisions consistent across
/// every surface the user sees.
/// </summary>
public static class ThemeHelper
{
    /// <summary>The user's effective theme as a WinUI ElementTheme.
    /// Use for `RequestedTheme` on FrameworkElements.</summary>
    public static ElementTheme ResolvedElementTheme()
    {
        return ResolvedIsDark() ? ElementTheme.Dark : ElementTheme.Light;
    }

    /// <summary>True iff the effective theme is Dark. Use this when you
    /// need a boolean (e.g. picking a tone for a baked-bitmap ICO).</summary>
    public static bool ResolvedIsDark()
    {
        try
        {
            var pref = Services.UiPreferences.Load().Theme;
            if (string.Equals(pref, "Light", System.StringComparison.OrdinalIgnoreCase)) return false;
            if (string.Equals(pref, "Dark", System.StringComparison.OrdinalIgnoreCase)) return true;
        }
        catch
        {
            // UiPreferences load failures fall through to system theme —
            // the user still gets a sensible default rather than a crash.
        }
        return SystemIsDark();
    }

    /// <summary>
    /// The Windows accent colour as a SolidColorBrush, picked for the
    /// user's resolved theme (UiPreferences override → system fallback).
    ///
    /// Mirrors what `AccentFillColorDefaultBrush` does internally:
    /// SystemAccentColorDark2 on Light theme (saturated, readable on
    /// light surface), SystemAccentColorLight2 on Dark theme (lighter,
    /// readable on dark surface). Use this from C# code-behind anywhere
    /// you'd otherwise fetch `Application.Current.Resources["AccentFill…"]` —
    /// that lookup ignores per-window RequestedTheme overrides and gives
    /// the wrong shade when the user has flipped Dimmy's theme away from
    /// the system default.
    ///
    /// Returns a fresh SolidColorBrush instance; callers may share it
    /// across multiple visual elements. NOT live-tracking — if you need
    /// the brush to update on theme change, re-call this and re-assign.
    /// </summary>
    public static SolidColorBrush ResolvedAccentBrush()
    {
        try
        {
            var key = ResolvedIsDark() ? "SystemAccentColorLight2" : "SystemAccentColorDark2";
            if (Application.Current.Resources.TryGetValue(key, out var v) && v is global::Windows.UI.Color c)
                return new SolidColorBrush(c);
        }
        catch { }
        // Last-resort fallback — same hex as Win11 default user accent.
        return new SolidColorBrush(global::Windows.UI.Color.FromArgb(0xFF, 0x00, 0x78, 0xD4));
    }

    /// <summary>Read the current Windows app theme from the registry.
    /// `AppsUseLightTheme = 0` means dark; missing/1 means light. Returns
    /// false (light) on read failure — most users have light as their
    /// fall-through default.</summary>
    public static bool SystemIsDark()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            if (key?.GetValue("AppsUseLightTheme") is int v)
                return v == 0;
        }
        catch
        {
            // Registry access can fail under sandboxed contexts; light
            // fallback keeps icons visible on the most common system
            // configuration rather than vanishing into a dark menu.
        }
        return false;
    }
}
