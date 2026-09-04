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
/// "stable" (Latest GitHub release only) vs "prerelease" (stable + rc
/// builds from release.yml via GithubSource(prerelease: true)). Both
/// channels only ever serve PROD-flavor builds (license.dimmy.app +
/// Stripe Live). staging-native builds are deliberately NOT
/// discoverable here — staging-auto-update.yml withholds the Velopack
/// manifest so a paying prod user can't auto-update into a
/// flavor=staging binary (Stripe Test, different license pubkey).
/// Burned 2026-06-16. See CLAUDE.md "Release pipelines".
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
    /// exceptions (logged) — never throws to the caller, but reports
    /// what happened via the returned <see cref="UpdateCheckResult"/>
    /// so the button can render a concrete answer. The background loop
    /// discards the result.</summary>
    public async Task<UpdateCheckResult> CheckAndDownloadAsync(CancellationToken ct = default)
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
                return new UpdateCheckResult(UpdateCheckOutcome.NoLicense, "", null);
            }

            var prefs = UiPreferences.Load();
            // A staging install (packId Dimmy-Staging, Velopack channel
            // "staging") lives entirely on prerelease GitHub releases — the
            // prod "Latest" release is a different packId, so prerelease=false
            // would find nothing and staging would never auto-update. Force
            // prerelease=true for staging so a downloaded staging build keeps
            // itself current with no user action ("scarico e basta"). It can
            // NEVER pick up a prod build: packId AND channel both differ, so
            // Velopack filters prod releases out. The channel selector in
            // Settings is a prod-only control.
            bool prerelease = BuildInfo.IsStaging
                || string.Equals(prefs.UpdateChannel, "prerelease",
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
                return new UpdateCheckResult(UpdateCheckOutcome.DevBuild, "", null);
            }

            var info = await _manager.CheckForUpdatesAsync().ConfigureAwait(false);
            if (info is null)
            {
                App.Log($"no update (channel={prefs.UpdateChannel}, current={_manager.CurrentVersion})", "Update");
                return new UpdateCheckResult(
                    UpdateCheckOutcome.UpToDate,
                    _manager.CurrentVersion?.ToString() ?? "",
                    null);
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
            return new UpdateCheckResult(
                UpdateCheckOutcome.UpdateReady,
                info.TargetFullRelease.Version.ToString(),
                null);
        }
        catch (OperationCanceledException) { throw; }
        catch (Exception ex)
        {
            App.Log($"CheckAndDownload exc: {ex.GetType().Name}: {ex.Message}", "Update");
            return new UpdateCheckResult(UpdateCheckOutcome.Failed, "", ex.Message);
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
        if (!MayEndProcess(out int rc))
        {
            App.Log($"apply+restart REFUSED: meeting recording (rc={rc})", "Update");
            return;
        }
        var version = _pendingUpdate.TargetFullRelease.Version.ToString();
        App.Log($"applying update v{version} + restart", "Update");

        // Write a one-shot "I just updated" marker so the next launch
        // can surface a confirmation toast — users complained that
        // post-update the app vanished without telling them whether
        // the new version actually came up. Read + deleted by
        // OnLaunched after the UI is wired.
        WriteUpdatePendingMarker(version);

        try
        {
            // restartArgs: explicit Array.Empty<string>() — Velopack's
            // default is to forward the CURRENT process's argv as the
            // new process's argv. If the old Dimmy was launched via a
            // jump-list shortcut (`--command open-settings`), that arg
            // is forwarded to the new process, which then tries to
            // pipe-forward to a sibling that doesn't exist (Velopack
            // just killed it) and exits silently. Burned on v0.6.37
            // first-install. Empty array = clean argv = normal startup.
            _manager.ApplyUpdatesAndRestart(_pendingUpdate, restartArgs: System.Array.Empty<string>());
        }
        catch (Exception ex)
        {
            App.Log($"ApplyAndRestart exc: {ex.GetType().Name}: {ex.Message}", "Update");
        }
    }

    /// <summary>
    /// Persist "the apply happened" so the next launch can show a
    /// confirmation toast. Marker file lives alongside the existing
    /// config so we don't proliferate locations. Best-effort — IO
    /// failure here must not block the apply.
    /// </summary>
    private static void WriteUpdatePendingMarker(string version)
    {
        try
        {
            var path = System.IO.Path.Combine(BuildInfo.ConfigDirPath, "update-pending.txt");
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            System.IO.File.WriteAllText(path, version);
        }
        catch (System.Exception ex)
        {
            App.Log($"WriteUpdatePendingMarker exc: {ex.GetType().Name}: {ex.Message}", "Update");
        }
    }

    /// <summary>
    /// Read + clear the post-update marker written by
    /// <see cref="WriteUpdatePendingMarker"/>. Returns the applied
    /// version string if the previous run finished an update, or
    /// null if no marker (normal startup). Always deletes the marker
    /// — even on null return — so a marker leaked from a partial
    /// crash doesn't replay forever.
    /// </summary>
    public static string? ConsumeUpdatePendingMarker()
    {
        try
        {
            var path = System.IO.Path.Combine(BuildInfo.ConfigDirPath, "update-pending.txt");
            if (!System.IO.File.Exists(path)) return null;
            var v = System.IO.File.ReadAllText(path).Trim();
            try { System.IO.File.Delete(path); } catch { }
            return string.IsNullOrEmpty(v) ? null : v;
        }
        catch { return null; }
    }

    /// <summary>Apply at next launch instead of now — Velopack stages
    /// the swap and exits cleanly without restarting. Used when the
    /// user picks "Skip this time" at quit time but we still want
    /// the update in place for the next session.</summary>
    public void ApplyOnExit()
    {
        if (_manager is null || _pendingUpdate is null) return;
        // WaitExitThenApplyUpdates spawns `Update.exe apply --waitPid <us>`,
        // which then lies in wait for this process to exit for ANY reason —
        // a clean quit, a crash, an external kill — and applies the update.
        // That is why the guard belongs HERE and not at the exit: once armed
        // we no longer control what happens. A meeting killed this way
        // survives as playable audio but is cut short, never finalized and
        // left orphaned. Burned 2026-09-04. The update simply waits for the
        // next quit instead; it is one launch late, which costs nothing.
        if (!MayEndProcess(out int rc))
        {
            App.Log($"apply-on-exit NOT armed: meeting recording (rc={rc})", "Update");
            return;
        }
        try
        {
            _manager.WaitExitThenApplyUpdates(_pendingUpdate, silent: true, restart: false);
        }
        catch (Exception ex)
        {
            App.Log($"ApplyOnExit exc: {ex.GetType().Name}: {ex.Message}", "Update");
        }
    }

    /// <summary>Ask the core whether a meeting is recording. A failure to
    /// read the state is treated as "recording" by
    /// <see cref="UpdateGate.MayEndProcess"/> — see the rationale there —
    /// and so is a throwing P/Invoke.</summary>
    private static bool MayEndProcess(out int rc)
    {
        try { rc = Interop.DimmyNative.dimmy_meeting_is_active(); }
        catch { rc = -1; }
        return UpdateGate.MayEndProcess(rc);
    }

    /// <summary>True when an update is downloaded but must wait because a
    /// meeting is recording. Lets the quit path skip the "apply now?"
    /// dialog rather than ask a question whose answer it would ignore.</summary>
    public bool IsBlockedByActiveMeeting => IsUpdateReady && !MayEndProcess(out _);
}
