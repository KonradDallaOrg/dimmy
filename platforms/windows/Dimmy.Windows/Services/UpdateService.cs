using System;
using System.Threading;
using System.Threading.Tasks;
using Velopack;
using Velopack.Sources;

namespace Dimmy.Windows.Services;

/// <summary>
/// Auto-update orchestrator for Windows builds.
///
/// Wraps <see cref="Velopack.UpdateManager"/> and follows the
/// "Mixed" UX picked 2026-05-11:
///
///   1. Background check ~5 s after launch (don't block startup).
///   2. If an update exists → silent download in background.
///   3. Surface readiness (UpdateReady event) so Settings → About and
///      the taskbar overlay can react.
///   4. At natural quit time, App shows a small "Apply now?" dialog;
///      Apply hands off to Velopack which swaps files + restarts.
///
/// No admin prompt — Velopack writes to %LOCALAPPDATA%\Dimmy where
/// the install lives, and the per-user installer path is already
/// user-writable. The user-facing flow is described in the
/// 2026-05-11 design conversation: "Mixed" scenario.
///
/// Channel selection lives in <see cref="UiPreferences.UpdateChannel"/>:
/// "stable" (Latest GitHub release only) vs "prerelease" (also offers
/// staging-native builds via GithubSource(prerelease: true)).
///
/// Failure modes are silent by design — no network, no GitHub auth,
/// or a malformed release manifest must NOT block the user from
/// using Dimmy. All exceptions land in ptt.log under tag "Update".
/// </summary>
public sealed class UpdateService
{
    public static UpdateService? Instance { get; private set; }

    private const string RepoUrl = "https://github.com/KonradDallaOrg/dimmy";
    /// <summary>Delay before first background check. Long enough that
    /// the foreground UI is painted and any pill animation has
    /// stabilised — under 10 s so the user sees the indicator on the
    /// same "session" they launched, not minutes later.</summary>
    private static readonly TimeSpan FirstCheckDelay = TimeSpan.FromSeconds(5);

    /// <summary>How often we re-check while the app stays running.
    /// 6 hours is the GitHub Releases convention: long enough not to
    /// hit unauth rate limits (60/hr per IP, more than enough for a
    /// single-user app), short enough that a user who keeps Dimmy
    /// open all day still picks up an update pushed at 9 am.</summary>
    private static readonly TimeSpan ReCheckInterval = TimeSpan.FromHours(6);

    private UpdateManager? _manager;
    private UpdateInfo? _pendingUpdate;
    private CancellationTokenSource? _cts;

    /// <summary>True iff an update has been checked, found newer than
    /// the running version, and successfully downloaded. The quit-time
    /// dialog and Settings card both gate on this.</summary>
    public bool IsUpdateReady => _pendingUpdate is not null;

    /// <summary>Version of the downloaded update, or empty if none.
    /// Used by the Settings card and the quit dialog to label what's
    /// being applied.</summary>
    public string PendingVersion =>
        _pendingUpdate?.TargetFullRelease?.Version?.ToString() ?? "";

    /// <summary>Fires on the UI dispatcher thread when an update is
    /// downloaded and ready to apply. Settings + taskbar overlay
    /// subscribe to refresh their UI; safe to fire multiple times
    /// (re-checks may surface a newer version).</summary>
    public event EventHandler? UpdateReady;

    public static void EnsureCreated()
    {
        Instance ??= new UpdateService();
    }

    private UpdateService()
    {
        // When the user redeems / activates a license, re-poll
        // immediately instead of waiting up to 6 h for the next tick.
        // Symmetric on Clear() — if scope drops, the next tick simply
        // skips, no need to react synchronously.
        LicenseService.LicenseChanged += () =>
        {
            if (_cts is null) return;
            App.Log("license changed — forcing update re-check", "Update");
            _ = CheckAndDownloadAsync();
        };
    }

    /// <summary>Schedule the first background check after FirstCheckDelay
    /// + periodic re-checks every ReCheckInterval. Idempotent — calling
    /// twice doesn't double-schedule.</summary>
    public void StartBackgroundLoop()
    {
        if (_cts is not null) return;
        _cts = new CancellationTokenSource();
        _ = Task.Run(() => LoopAsync(_cts.Token));
        App.Log("started background update loop", "Update");
    }

    public void Stop()
    {
        _cts?.Cancel();
        _cts = null;
    }

    private async Task LoopAsync(CancellationToken ct)
    {
        try
        {
            await Task.Delay(FirstCheckDelay, ct).ConfigureAwait(false);
            while (!ct.IsCancellationRequested)
            {
                try
                {
                    await CheckAndDownloadAsync(ct).ConfigureAwait(false);
                }
                catch (Exception ex)
                {
                    App.Log($"check failed: {ex.GetType().Name}: {ex.Message}", "Update");
                }
                await Task.Delay(ReCheckInterval, ct).ConfigureAwait(false);
            }
        }
        catch (TaskCanceledException) { /* stop requested, normal */ }
    }

    /// <summary>One pass of check + download. Public so the Settings
    /// "Check for updates now" button can force-run it. Swallows all
    /// exceptions (logged) — never throws to the caller.</summary>
    public async Task CheckAndDownloadAsync(CancellationToken ct = default)
    {
        try
        {
            // License gate — auto-update is a paid feature
            // (LicenseService.ScopeNames.AutoUpdate). Source / dev
            // builds without an embedded pubkey see HasScope=true for
            // every scope, so this is a no-op locally. Free users on
            // a signed prod build silently skip the entire flow — no
            // poll, no notification, no Settings UI — as the user
            // requested 2026-05-11 ("Nulla — silenzioso"). Re-runs
            // automatically when LicenseService.LicenseChanged fires
            // (e.g. after a Redeem succeeds).
            if (!LicenseService.HasScope(LicenseService.ScopeNames.AutoUpdate))
            {
                App.Log("auto_update scope absent — skipping check (license-gated)", "Update");
                return;
            }

            var prefs = UiPreferences.Load();
            bool prerelease = string.Equals(prefs.UpdateChannel, "prerelease",
                StringComparison.OrdinalIgnoreCase);

            // Recreate manager each time so the prerelease flag tracks
            // the user's pref without an app restart. Velopack's
            // GithubSource is lightweight, no auth, just hits /releases.
            _manager = new UpdateManager(new GithubSource(RepoUrl, accessToken: null, prerelease: prerelease));

            // IsInstalled is FALSE for unpackaged dev builds — Velopack
            // won't update a binary it doesn't own (no metadata to
            // diff against). Skip silently rather than logging an
            // error on every dev session.
            if (!_manager.IsInstalled)
            {
                App.Log("dev build / no Velopack metadata — skipping check", "Update");
                return;
            }

            var info = await _manager.CheckForUpdatesAsync().ConfigureAwait(false);
            if (info is null)
            {
                App.Log($"no update (channel={prefs.UpdateChannel}, current={_manager.CurrentVersion})", "Update");
                return;
            }

            App.Log($"update available: {info.TargetFullRelease.Version}; downloading", "Update");
            await _manager.DownloadUpdatesAsync(info, cancelToken: ct).ConfigureAwait(false);
            _pendingUpdate = info;
            App.Log($"update downloaded: v{info.TargetFullRelease.Version}", "Update");

            // Fire on the UI dispatcher so subscribers don't have to
            // re-marshal. Best-effort — if no dispatcher is captured
            // (early startup race), the event still fires synchronously
            // and subscribers handle dispatch themselves.
            App.Instance?.RunOnUI(() => UpdateReady?.Invoke(this, EventArgs.Empty));
        }
        catch (OperationCanceledException) { throw; }
        catch (Exception ex)
        {
            App.Log($"CheckAndDownload exc: {ex.GetType().Name}: {ex.Message}", "Update");
        }
    }

    /// <summary>Apply the downloaded update and relaunch Dimmy. This
    /// blocks the calling thread briefly while Velopack spawns its
    /// updater process; the current process exits before the new
    /// one launches. Safe to call only when <see cref="IsUpdateReady"/>
    /// is true.</summary>
    public void ApplyAndRestart()
    {
        if (_manager is null || _pendingUpdate is null)
        {
            App.Log("ApplyAndRestart called with no pending update", "Update");
            return;
        }
        App.Log($"applying update v{_pendingUpdate.TargetFullRelease.Version} + restart", "Update");
        try
        {
            _manager.ApplyUpdatesAndRestart(_pendingUpdate);
        }
        catch (Exception ex)
        {
            App.Log($"ApplyAndRestart exc: {ex.GetType().Name}: {ex.Message}", "Update");
        }
    }

    /// <summary>Apply at next launch instead of now — Velopack stages
    /// the swap and exits cleanly without restarting. Used when the
    /// user picks "Skip this time" at quit time but we still want
    /// the update in place for the next session.</summary>
    public void ApplyOnExit()
    {
        if (_manager is null || _pendingUpdate is null) return;
        try
        {
            _manager.WaitExitThenApplyUpdates(_pendingUpdate, silent: true, restart: false);
        }
        catch (Exception ex)
        {
            App.Log($"ApplyOnExit exc: {ex.GetType().Name}: {ex.Message}", "Update");
        }
    }
}
