using System;
using Microsoft.Win32;

namespace Dimmy.Windows.Services;

/// <summary>
/// Registers Dimmy as the handler for `dimmy://` URLs in the user's
/// registry hive (HKCU — no admin needed). Idempotent; safe to call
/// on every launch.
///
/// When a magic link in an activation email opens `dimmy://activate?code=…`,
/// Windows finds the registration here and launches `Dimmy.Windows.exe`
/// with the URL as its command-line argument. App.OnLaunched picks it
/// up via Environment.GetCommandLineArgs() and dispatches to the
/// licensing flow.
///
/// Velopack installs land in `%LOCALAPPDATA%\Programs\Dimmy\…` and
/// the EXE path is stable across upgrades (Velopack rewrites the
/// shortcut on update, the registry entry stays valid).
///
/// See `docs/dev/licensing-prod.md` for the full activation flow.
/// </summary>
public static class UrlSchemeRegistrar
{
    private const string Scheme = "dimmy";

    /// <summary>Register `dimmy://` if not already registered, or
    /// update the command path if it changed (e.g. install moved).</summary>
    public static void EnsureRegistered()
    {
        try
        {
            var exePath = Environment.ProcessPath
                ?? System.Diagnostics.Process.GetCurrentProcess().MainModule?.FileName;
            if (string.IsNullOrEmpty(exePath))
            {
                System.Diagnostics.Debug.WriteLine("[UrlScheme] cannot resolve EXE path");
                return;
            }

            // The shell command Windows runs when an external caller
            // navigates to dimmy://… . Quoted EXE path + "%1" placeholder
            // for the URL — same shape every browser/email-client URL
            // scheme uses.
            var command = $"\"{exePath}\" \"%1\"";

            // Skip the registry round-trip if the existing command
            // already matches what we'd write.
            using (var existing = Registry.CurrentUser.OpenSubKey(
                $@"Software\Classes\{Scheme}\shell\open\command", writable: false))
            {
                if (existing?.GetValue("") is string current && current == command)
                {
                    return;
                }
            }

            using var rootKey = Registry.CurrentUser.CreateSubKey(
                $@"Software\Classes\{Scheme}");
            rootKey.SetValue("", "URL:Dimmy activation"); // (Default) — friendly name
            rootKey.SetValue("URL Protocol", "");          // marks this key as a URL scheme

            using var commandKey = Registry.CurrentUser.CreateSubKey(
                $@"Software\Classes\{Scheme}\shell\open\command");
            commandKey.SetValue("", command);

            System.Diagnostics.Debug.WriteLine(
                $"[UrlScheme] registered dimmy:// → {exePath}");
        }
        catch (Exception ex)
        {
            // Best-effort. If the registry is locked-down or the user
            // is on a managed corporate machine where HKCU\Classes is
            // restricted, the magic link won't work — but the
            // paste-token fallback (Settings → License → "Paste code")
            // still does. Log + continue.
            System.Diagnostics.Debug.WriteLine($"[UrlScheme] failed: {ex.Message}");
        }
    }

    /// <summary>Parse a `dimmy://activate?code=…` (or `?token=…`) URL
    /// and return the extracted activation code or token. Returns null
    /// if the URL isn't shaped as expected.
    ///
    /// `code`  — short single-use activation code from the magic link.
    /// `token` — full signed JWT (paste-fallback path: user copy-pastes
    ///           the entire token from the email body).</summary>
    public static (string? Code, string? Token) ParseActivationUrl(string url)
    {
        if (string.IsNullOrWhiteSpace(url)) return (null, null);
        if (!Uri.TryCreate(url, UriKind.Absolute, out var uri)) return (null, null);
        if (!string.Equals(uri.Scheme, Scheme, StringComparison.OrdinalIgnoreCase))
            return (null, null);
        if (!string.Equals(uri.Host, "activate", StringComparison.OrdinalIgnoreCase))
            return (null, null);

        var query = uri.Query.TrimStart('?');
        string? code = null, token = null;
        foreach (var pair in query.Split('&', StringSplitOptions.RemoveEmptyEntries))
        {
            var eq = pair.IndexOf('=');
            if (eq <= 0) continue;
            var k = pair.Substring(0, eq);
            var v = Uri.UnescapeDataString(pair.Substring(eq + 1));
            if (k.Equals("code", StringComparison.OrdinalIgnoreCase)) code = v;
            else if (k.Equals("token", StringComparison.OrdinalIgnoreCase)) token = v;
        }
        return (code, token);
    }
}
