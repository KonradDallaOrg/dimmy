using System;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Single point of truth for "which flavor of Dimmy is this" on the Win
/// side. Reads `dimmy_build_flavor()` from the Rust core once at startup
/// and caches it. Drives the single-instance mutex name (so a staging
/// build can run side-by-side with a prod install) and the UI watermark.
///
/// Empty / unset = prod build (default). Anything else flips the flavor
/// branch. The Rust build.rs panics on unrecognised flavor values, so
/// any non-empty value here is one we've explicitly authored.
/// </summary>
public static class BuildInfo
{
    private static readonly Lazy<string> _flavor = new(() =>
    {
        try
        {
            return DimmyNative.ReadBuffer(DimmyNative.dimmy_build_flavor, 64) ?? string.Empty;
        }
        catch
        {
            // FFI not available (e.g. unit-test host without dimmy_lib.dll).
            // Treat as prod — the affected paths fall back to existing
            // hardcoded names.
            return string.Empty;
        }
    });

    /// <summary>"" for prod, "staging" for staging.</summary>
    public static string Flavor => _flavor.Value;

    public static bool IsStaging => string.Equals(Flavor, "staging", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// Single-instance mutex name. Prod and staging use distinct mutex
    /// names so both can run simultaneously on the same machine without
    /// the second-launched flavor exiting silently.
    /// </summary>
    public static string SingleInstanceMutexName =>
        IsStaging ? @"Global\DimmySingleInstance.Staging"
                  : @"Global\DimmySingleInstance";

    /// <summary>Display label shown in the watermark badge.</summary>
    public static string FlavorLabel => IsStaging ? "STAGING" : string.Empty;

    /// <summary>
    /// Folder name under %APPDATA% that holds config.json + history.db
    /// + license.json + onboarding marker. MUST match the Rust core's
    /// `config_dir_name()`: `dimmy` for prod, `dimmy-staging` for
    /// staging — otherwise C# reads the wrong file, then writes UI
    /// state back through the FFI which overwrites the real
    /// Rust-owned file with empty defaults. Burned 2026-05-12 on
    /// app_rules loss after a staging install side-by-side a prod
    /// install. Use this everywhere — never hardcode "dimmy".
    /// </summary>
    public static string ConfigDirName => IsStaging ? "dimmy-staging" : "dimmy";

    /// <summary>
    /// Full path to the config-folder under %APPDATA%. Helper around
    /// <see cref="ConfigDirName"/> so callers don't repeat the
    /// GetFolderPath dance.
    /// </summary>
    public static string ConfigDirPath =>
        System.IO.Path.Combine(
            System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
            ConfigDirName);
}
