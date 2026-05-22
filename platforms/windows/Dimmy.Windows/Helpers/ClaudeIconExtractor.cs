using System;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using Dimmy.Windows.Services;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// Extracts Claude Desktop's installed app icon for use in Dimmy's
/// Settings → Integrations card. The Microsoft Store / MSIX build of
/// Claude ships a set of PNG logos under its install dir's
/// `Assets/` subfolder (Square44x44Logo, Square150x150Logo, …). We
/// pick the largest available square PNG, copy it to the user's
/// config dir, and bind the Settings card Image to that path.
///
/// Falling back to our bundled SVG (Assets/Providers/claude-desktop.svg)
/// when extraction fails — covers the "user only has Squirrel install"
/// or "ACL refused" cases.
/// </summary>
internal static class ClaudeIconExtractor
{
    /// <summary>
    /// Try to find a Claude app icon on disk and cache it. Returns the
    /// cached path on success, null on any failure (caller falls back
    /// to bundled SVG). Cheap to call repeatedly — if the cache file
    /// already exists and the Claude install timestamp hasn't moved,
    /// returns the cached path without re-extracting.
    /// </summary>
    public static async Task<string?> TryExtractAsync()
    {
        try
        {
            return await Task.Run(() => TryExtract());
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeIconExtractor exc: {ex.Message}", "ClaudeDesktop");
            return null;
        }
    }

    /// <summary>
    /// Resolve Claude Desktop's full AUMID (Application User Model
    /// ID) by enumerating the package's app entries. The AUMID is
    /// `<PackageFamilyName>!<ApplicationId>` and the application id
    /// portion is NOT discoverable from the family name alone — it
    /// depends on what Anthropic wrote in their AppxManifest. We let
    /// WinRT tell us instead of guessing. Returns null on any
    /// failure; caller falls back to opening Explorer at the install
    /// dir (visible-but-useless) or to nothing.
    /// </summary>
    public static async Task<string?> ResolveAumidAsync()
    {
        try
        {
            var pm = new global::Windows.Management.Deployment.PackageManager();
            var pkgs = pm.FindPackagesForUser(
                string.Empty, "Claude_pzs8sxrjxfjjc");
            var pkg = pkgs?
                .OrderByDescending(p => p.Id?.Version.Major ?? 0)
                .ThenByDescending(p => p.Id?.Version.Minor ?? 0)
                .ThenByDescending(p => p.Id?.Version.Build ?? 0)
                .ThenByDescending(p => p.Id?.Version.Revision ?? 0)
                .FirstOrDefault();
            if (pkg == null) return null;
            var entries = await pkg.GetAppListEntriesAsync();
            // Most packages ship a single tile entry; if more we pick
            // the first — the Start-menu launch target.
            var entry = entries?.FirstOrDefault();
            return entry?.AppUserModelId;
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeIconExtractor AUMID resolve failed: {ex.Message}", "ClaudeDesktop");
            return null;
        }
    }

    private static string? TryExtract()
    {
        var cacheDir = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            BuildInfo.ConfigDirName,
            "cache");
        Directory.CreateDirectory(cacheDir);
        var cached = Path.Combine(cacheDir, "claude-desktop-icon.png");

        // Resolve the MSIX install location via the WinRT
        // PackageManager. From an unpackaged process this requires
        // the `windows.management.deployment` capability, which the
        // Microsoft.WindowsAppSDK targets enable by default.
        string? installDir;
        try
        {
            var pm = new global::Windows.Management.Deployment.PackageManager();
            var pkgs = pm.FindPackagesForUser(
                string.Empty, "Claude_pzs8sxrjxfjjc");
            // Pick the highest version when multiple are present
            // (Windows keeps older versions during update windows).
            var pkg = pkgs?
                .OrderByDescending(p => p.Id?.Version.Major ?? 0)
                .ThenByDescending(p => p.Id?.Version.Minor ?? 0)
                .ThenByDescending(p => p.Id?.Version.Build ?? 0)
                .ThenByDescending(p => p.Id?.Version.Revision ?? 0)
                .FirstOrDefault();
            installDir = pkg?.InstalledLocation?.Path;
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeIconExtractor PackageManager failed: {ex.Message}", "ClaudeDesktop");
            return null;
        }
        if (string.IsNullOrEmpty(installDir)) return null;

        // Cache invalidation: if the cached file is newer than the
        // pack's install dir, reuse it. We use install dir mtime as a
        // proxy for "Claude was updated".
        var installInfo = new DirectoryInfo(installDir);
        if (File.Exists(cached))
        {
            var cachedInfo = new FileInfo(cached);
            if (cachedInfo.LastWriteTimeUtc >= installInfo.LastWriteTimeUtc)
            {
                return cached;
            }
        }

        // Walk the Assets dir looking for the best square logo. MSIX
        // stores multiple scales — pick the largest variant we can
        // read so the card renders crisp on hi-DPI displays.
        var assetsDir = Path.Combine(installDir, "Assets");
        if (!Directory.Exists(assetsDir)) return null;
        var candidate = Directory.EnumerateFiles(assetsDir, "*Logo*.png")
            .Where(p =>
            {
                var name = Path.GetFileName(p);
                // Prefer the bigger ones; LargeTile + Square150 work
                // well at 40px UI render. Avoid the tiny 16/24/30
                // variants which blur when upscaled.
                return name.Contains("Square150x150", StringComparison.OrdinalIgnoreCase)
                    || name.Contains("LargeTile", StringComparison.OrdinalIgnoreCase)
                    || name.Contains("Square71x71", StringComparison.OrdinalIgnoreCase)
                    || name.Contains("StoreLogo", StringComparison.OrdinalIgnoreCase);
            })
            // Prefer scale-200 (hi-DPI) when present.
            .OrderByDescending(p => p.Contains("scale-200"))
            .ThenByDescending(p => new FileInfo(p).Length)
            .FirstOrDefault();
        if (candidate == null) return null;

        try
        {
            File.Copy(candidate, cached, overwrite: true);
            return cached;
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeIconExtractor copy failed: {ex.Message}", "ClaudeDesktop");
            return null;
        }
    }
}
