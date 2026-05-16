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
    /// + license.json + onboarding marker. Resolved at startup from
    /// the Rust core via `dimmy_config_dir_name()` — DO NOT derive
    /// from <see cref="IsStaging"/> here, the two are decoupled since
    /// 2026-05-16 (a flavor=staging build that ships under the prod
    /// packId keeps the prod config dir so channel-prerelease
    /// auto-updates don't appear to wipe user data). The previous
    /// flavor-derived value caused the 2026-05-16 onboarding-restart
    /// bug: C# read `dimmy-staging/.onboarding_done` (absent) while
    /// Rust wrote to `dimmy/.onboarding_done` (present) and the
    /// wizard re-launched on every start. Use this everywhere —
    /// never hardcode "dimmy" or "dimmy-staging".
    /// </summary>
    private static readonly Lazy<string> _configDirName = new(() =>
    {
        try
        {
            var name = DimmyNative.ReadBuffer(DimmyNative.dimmy_config_dir_name, 64);
            return string.IsNullOrEmpty(name) ? "dimmy" : name!;
        }
        catch
        {
            // FFI not available (unit-test host without dimmy_lib.dll).
            // Fall back to prod default — staging tester install can't
            // happen without the FFI anyway.
            return "dimmy";
        }
    });
    public static string ConfigDirName => _configDirName.Value;

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
