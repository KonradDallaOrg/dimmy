using System;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;
using Dimmy.Windows.ViewModels;
using Dimmy.Windows.Views;

namespace Dimmy.Windows;

public partial class App : Application
{
    public static App? Instance { get; private set; }

    // Single-instance mutex — prevents multiple Dimmy processes
    private static Mutex? _singleInstanceMutex;

    private AppViewModel _appViewModel = new();
    /// Exposes the shared view-model so secondary windows (Settings,
    /// Onboarding) can subscribe to FFI-routed events that the App-level
    /// callback already de-duplicates and dispatches onto the UI thread.
    public AppViewModel AppViewModel => _appViewModel;

    /// <summary>Dispatch an action onto the UI thread. Safe to call
    /// from any thread; no-op if the dispatcher hasn't been captured
    /// yet (very early startup). Used by background services
    /// (UpdateService, etc.) that need to raise events without
    /// hardcoding their own DispatcherQueue resolution.</summary>
    public void RunOnUI(Action action) => _dispatcherQueue?.TryEnqueue(() => action());

    /// <summary>Re-register the dictionary global hotkey with a new
    /// combo. Called by Settings → Voice input → Add-to-dictionary
    /// shortcut after the user picks a new combination. No-op if the
    /// service hasn't been created yet (very early startup race).</summary>
    public void ReregisterDictHotkey(string combo)
    {
        try { _dictHotkey?.Register(combo); }
        catch (Exception ex) { Log($"ReregisterDictHotkey exc: {ex.Message}", "Hotkey"); }
    }

    /// <summary>Re-bind the optional dedicated command-mode hotkey after the
    /// user edits it in Settings. An empty combo disables it. Runs on the same
    /// Rust low-level hook as the dictation hotkey (so it supports toggle +
    /// PTT and every combo incl. modifier-only). No-op until the service
    /// exists.</summary>
    public void ReregisterCommandHotkey(string combo)
    {
        try { _hotkeyService?.SetCommandShortcut(combo); }
        catch (Exception ex) { Log($"ReregisterCommandHotkey exc: {ex.Message}", "Hotkey"); }
    }

    /// <summary>Re-bind the optional meeting start/stop hotkey after the user
    /// edits it in Settings. Empty disables it. Toggle-only. No-op until the
    /// service exists.</summary>
    public void ReregisterMeetingHotkey(string combo)
    {
        try { _hotkeyService?.SetMeetingShortcut(combo); }
        catch (Exception ex) { Log($"ReregisterMeetingHotkey exc: {ex.Message}", "Hotkey"); }
    }

    private void OnMeetingHotkeyPressed()
    {
        _dispatcherQueue?.TryEnqueue(async () => await ToggleMeetingFromShortcutAsync());
    }

    /// <summary>Toggle meeting recording from the meeting hotkey OR a menu
    /// action — the single consent-gated meeting entry point for those paths.
    /// Active meeting → stop + recap (via the pill's existing pipeline).
    /// Idle → mandatory consent modal, then start recording in the BACKGROUND
    /// (the pill + taskbar reflect it via the `meeting_state` event; the Meeting
    /// window is NOT opened). Ignored while a dictation is in flight: the two
    /// captures share the cpal buffer and the core has no guard for that
    /// direction (it only blocks dictation while a meeting is active, rc -7).</summary>
    public async Task ToggleMeetingFromShortcutAsync(string source = "hotkey")
    {
        try
        {
            if (DimmyNative.dimmy_meeting_is_active() == 1)
            {
                PttLog("Meeting hotkey: stopping active meeting (+recap)");
                try { DimmyNative.dimmy_track_meeting_action(source); } catch { }
                if (_pillWindow != null)
                    await _pillWindow.StopMeetingFromPillAsync();
                return;
            }
            if (_appViewModel.IsRecording || _pttStarted)
            {
                PttLog("Meeting hotkey ignored — a dictation is in progress");
                _appViewModel.SetError("Finish the dictation first");
                return;
            }
            var lang = System.Globalization.CultureInfo.CurrentUICulture.TwoLetterISOLanguageName;
            // ConsentFlow self-hosts a correctly-sized window — no XamlRoot needed.
            if (!await Services.ConsentFlow.ConfirmAndAnnounceAsync(null, lang))
            {
                PttLog("Meeting hotkey: consent declined");
                return;
            }
            var buf = new byte[256];
            int rc = DimmyNative.dimmy_meeting_start(buf, buf.Length);
            if (rc <= 0)
            {
                PttLog($"Meeting hotkey: start failed rc={rc}");
                _appViewModel.SetError($"Meeting start failed ({rc})");
                return;
            }
            PttLog("Meeting hotkey: recording started (background)");
            // Pin the recap intent for THIS meeting. A background/shortcut
            // meeting has no per-meeting "Generate recap" checkbox (that UI
            // only lives in the meeting window), so the stop path reads
            // AppViewModel.MeetingGenerateRecap — if we don't set it here it
            // keeps a STALE value from an earlier window-started meeting and
            // the recap is silently skipped at stop. Default to recap on.
            _appViewModel.MeetingGenerateRecap = true;
            try { DimmyNative.dimmy_track_meeting_action(source); } catch { }
            // Pill + taskbar flip to recording via the meeting_state event.
        }
        catch (Exception ex) { PttLog($"ToggleMeetingFromShortcut exc: {ex.Message}"); }
    }

    /// <summary>Core hit the 5 min soft threshold on a single dictation:
    /// nudge toward Meeting mode without interrupting the recording.</summary>
    private void OnDictationLongWarning()
    {
        try { Services.DictNotificationService.ShowLongDictationWarning(); }
        catch (Exception ex) { PttLog($"OnDictationLongWarning exc: {ex.Message}"); }
    }

    /// <summary>Core hit the 10 min hard cap on a single dictation. Stop it
    /// like a normal hotkey stop (transcribes + pastes what was said, which
    /// bounds the otherwise-unbounded dictation buffer), then nudge toward
    /// Meeting mode. Runs on the UI thread (HandleEvent is dispatched there).</summary>
    private async void OnDictationMaxDuration()
    {
        try
        {
            PttLog("dictation.max_duration — auto-stopping the runaway dictation");
            if (_appViewModel.IsRecording || _pttStarted)
                await StopAndProcess();
            _pttStarted = false;
            Services.DictNotificationService.ShowLongDictationCapped();
        }
        catch (Exception ex) { PttLog($"OnDictationMaxDuration exc: {ex.Message}"); }
    }

    /// <summary>Host the recording-consent dialog in a properly sized window
    /// and return the user's decision. The pill is too small to host a
    /// ContentDialog — the consent text rendered clipped/illegible (burned
    /// 2026-06-24). If a large window (Meeting/Settings) is already open we
    /// reuse its XamlRoot; otherwise we spin up a transient centered host
    /// window that closes as soon as the dialog does, so meeting-from-hotkey
    /// still records in the background without leaving a window open.</summary>
    private async Task<bool> RunConsentDialogAsync(string lang)
    {
        var existing = _meetingWindow?.Content?.XamlRoot
            ?? _settingsWindow?.Content?.XamlRoot;
        if (existing != null)
            return await Services.ConsentFlow.ConfirmAndAnnounceAsync(existing, lang);

        var host = new Microsoft.UI.Xaml.Window { Title = "Recording notice" };
        host.Content = new Microsoft.UI.Xaml.Controls.Grid
        {
            Background = (Microsoft.UI.Xaml.Media.Brush?)
                Application.Current.Resources["ApplicationPageBackgroundThemeBrush"],
            RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
        };
        try
        {
            var aw = host.AppWindow;
            const int w = 560, h = 540;
            var da = Microsoft.UI.Windowing.DisplayArea.GetFromWindowId(
                aw.Id, Microsoft.UI.Windowing.DisplayAreaFallback.Primary);
            int x = da.WorkArea.X + (da.WorkArea.Width - w) / 2;
            int y = da.WorkArea.Y + (da.WorkArea.Height - h) / 2;
            aw.MoveAndResize(new global::Windows.Graphics.RectInt32(x, y, w, h));
            if (aw.Presenter is Microsoft.UI.Windowing.OverlappedPresenter p)
            {
                p.IsResizable = false;
                p.IsMaximizable = false;
                p.IsMinimizable = false;
                p.IsAlwaysOnTop = true;
            }
        }
        catch (Exception ex) { PttLog($"consent host size exc: {ex.Message}"); }

        var ready = new TaskCompletionSource();
        ((Microsoft.UI.Xaml.FrameworkElement)host.Content).Loaded += (_, __) => ready.TrySetResult();
        host.Activate();
        await ready.Task;
        try
        {
            return await Services.ConsentFlow.ConfirmAndAnnounceAsync(host.Content.XamlRoot, lang);
        }
        finally
        {
            host.Close();
        }
    }

    /// <summary>Menu action "Start/Stop Dictation": toggle a dictation. If
    /// recording → stop+process; else behaves exactly like pressing the
    /// dictation shortcut (OnHotkeyPressed handles the meeting-active gate,
    /// hold/toggle mode, pill visibility + rc handling).</summary>
    public void MenuToggleDictation()
    {
        if (_appViewModel.IsRecording || _pttStarted)
            _dispatcherQueue?.TryEnqueue(async () => await StopAndProcess());
        else
            OnHotkeyPressed();
    }

    /// <summary>Menu action "Command (next dictation)": arm a ONE-SHOT command
    /// for the next dictation (no recording starts now — you then trigger
    /// dictation normally and that one transforms your selection). Mirrors the
    /// user's "solo per stavolta, con lo stesso shortcut della dettatura".</summary>
    public void MenuArmCommandOneShot()
    {
        _dispatcherQueue?.TryEnqueue(() =>
        {
            if (DimmyNative.dimmy_meeting_is_active() != 0)
            {
                PttLog("arm-command ignored — meeting recording in progress");
                return;
            }
            _appViewModel.CommandOneShot = true;
            PttLog("Command armed for the next dictation (menu)");
        });
    }

    private PillWindow? _pillWindow;
    private CaptionWindow? _captionWindow;
    private MeetingWindow? _meetingWindow;
    private OnboardingWindow? _onboardingWindow;
    private HotkeyService? _hotkeyService;
    private TrayService? _trayService;
    private TaskbarAnchorWindow? _taskbarAnchor;
    private TaskbarService? _taskbarService;
    private CommandPipeServer? _commandPipe;
    private UiPreferences _uiPrefs = new();
    private DispatcherQueue? _dispatcherQueue;
    private DictHotkeyService? _dictHotkey;
    private CallNudgeWindow? _callNudgeWindow;
    private Services.CallDetectionService? _callDetection;

    /// <summary>Set on launch if `dimmy://activate?…` was the trigger
    /// AND no running instance was reachable to forward to. Picked up
    /// inside StartNormalMode after the pipe server is online.</summary>
    private string? _pendingActivationPayload;

    // Must be stored as a field to prevent GC collection of the delegate
    private DimmyNative.EventCallback? _eventCallbackDelegate;

    // PTT: tracks that WE started recording (don't wait for Rust event)
    private volatile bool _pttStarted;
    // PTT: set by release handler if it fires before/during recording start
    private volatile bool _pendingStop;
    private volatile bool _stopInProgress;
    // Toggle debounce: ignore presses within 300ms of last action
    private long _lastToggleMs;
    // Separate debounce for the command hotkey so a recent dictation toggle
    // can't swallow a command press (and vice versa).
    private long _lastCommandToggleMs;

    private static readonly string PttLogPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "dimmy", "ptt.log");

    private static void PttLog(string msg) => Log(msg, "PTT");

    // ptt.log rotation cap. Every hotkey event / FFI callback appends here,
    // and before 2026-07-02 the file grew unbounded (7.7 MB observed on a
    // real machine — audit blocker). Mirrors the Rust core's dimmy.log
    // rotation (core/src/lib.rs): over the cap → keep the newest half,
    // cut at a line boundary.
    private const long MaxPttLogBytes = 1_048_576; // 1 MB
    private static int _logCallsSinceSizeCheck;

    /// Public diagnostic logger callable from any window for ad-hoc
    /// debugging. Routes to the same ptt.log so output is one stream.
    public static void Log(string msg, string tag = "Dimmy")
    {
        var line = $"[{DateTime.Now:HH:mm:ss.fff}] [{tag}] {msg}";
        Console.WriteLine(line);
        Console.Out.Flush();
        try
        {
            // Stat the file every 64th call — a per-append stat is cheap but
            // pointless at this cadence, and 64 lines (~6 KB) of overshoot
            // past the 1 MB cap is immaterial.
            if (Interlocked.Increment(ref _logCallsSinceSizeCheck) % 64 == 1)
            {
                Helpers.LogRotation.TrimToHalfIfOver(PttLogPath, MaxPttLogBytes);
            }
            File.AppendAllText(PttLogPath, line + Environment.NewLine);
        }
        catch { }
    }

    public App()
    {
        Instance = this;
        this.InitializeComponent();
    }

    public void ReloadConfig()
    {
        LoadConfigIntoViewModel();
    }

    /// <summary>Apply settings directly from the SettingsViewModel (avoids Rust roundtrip for UI-only fields).</summary>
    public void ApplySettings(ViewModels.SettingsViewModel settings)
    {
        _appViewModel.BorderStyle = settings.BorderStyle;
        _appViewModel.WaveformStyle = settings.WaveformStyle;
        _appViewModel.Theme = settings.Theme;
        _appViewModel.Language = settings.Language;
        _appViewModel.LlmTranslateTo = settings.LlmTranslateTo;
        _appViewModel.LlmStyle = settings.LlmStyle;
        _appViewModel.KeepInClipboard = settings.KeepInClipboard;

        _appViewModel.ShortcutMode = settings.ShortcutMode;
        if (_hotkeyService != null)
            _hotkeyService.PttMode = settings.ShortcutMode == "hold";

        // Always re-register hotkey — ReloadConfig() may have already updated
        // _appViewModel.Shortcut, so comparing would miss the change.
        _appViewModel.Shortcut = settings.Shortcut;
        _hotkeyService?.Register(_appViewModel.Shortcut);

        if (_appViewModel.OverlayPosition != settings.OverlayPosition)
        {
            _appViewModel.OverlayPosition = settings.OverlayPosition;
            RepositionPill();
        }

        // Win-only pill visibility prefs — copy back into the live
        // AppViewModel so the next hotkey press / startup honours the
        // new value. The PropertyChanged handler in OnLaunched persists
        // them to ui_prefs.json automatically.
        _appViewModel.PillShowOnStartup = settings.PillShowOnStartup;
        _appViewModel.PillShowOnHotkey = settings.PillShowOnHotkey;
        _appViewModel.ShowTaskbarIcon = settings.ShowTaskbarIcon;
        _appViewModel.TrayIconAlwaysVisible = settings.TrayIconAlwaysVisible;
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // Set the AppUserModelID FIRST. Windows derives taskbar
        // grouping + jump-list ownership from the AUMI of the process
        // at the moment a window first registers in the taskbar; if
        // we set it later, the jump list we register doesn't bind to
        // our taskbar entry and right-click shows nothing custom.
        JumpListService.SetProcessAumi();

        // One-time cleanup of the retired app-rules-diag.log (the
        // "temporary" 2026-05-12 diag logger grew unbounded on users'
        // disks — audit 2026-07-02; the logger itself is deleted).
        try
        {
            File.Delete(Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy", "app-rules-diag.log"));
        }
        catch { }

        // Register the dimmy:// custom URL scheme in HKCU\Classes so
        // activation magic-link emails can deep-link into the app.
        // No admin needed; idempotent.
        UrlSchemeRegistrar.EnsureRegistered();

        // If launched via a `dimmy://activate?…` URL, normalise to a
        // pipe command and either:
        //   (a) forward to a running Dimmy via the command pipe and
        //       exit (so the user doesn't see a second window flash),
        //   (b) keep the payload on our process so the still-to-be-
        //       constructed Dimmy can dispatch it once StartNormalMode
        //       brings the pipe + UI online.
        var activationPayload = TryGetActivationPipeCommand();
        if (activationPayload is not null)
        {
            // Hand off foreground rights to the running instance BEFORE
            // forwarding the pipe command. Without this, the running
            // instance's SetForegroundWindow call (post-activation,
            // when popping Settings → License) silently no-ops because
            // Windows only lets the *currently foreground* process
            // promote arbitrary windows. We are foreground briefly here
            // (this transient instance was launched by the OS in
            // response to the dimmy:// click), so we transfer the
            // promote-window right to the running PID, then exit.
            try
            {
                var running = System.Diagnostics.Process
                    .GetProcessesByName("Dimmy.Windows")
                    .FirstOrDefault(p => p.Id != Environment.ProcessId);
                if (running is not null)
                {
                    AllowSetForegroundWindow((uint)running.Id);
                }
            }
            catch { /* best-effort handoff; activation still works without it */ }

            if (CommandPipeServer.TrySendCommand(activationPayload))
            {
                Environment.Exit(0);
                return;
            }
        }
        // If a payload exists but we couldn't forward (no running
        // instance), stash it for HandleForwardedCommand to pick up
        // after StartNormalMode initialises the pipe server.
        _pendingActivationPayload = activationPayload;

        // Ensure a Start-menu shortcut with the matching AUMI exists.
        // Velopack creates one in production; in dev we make a "Dimmy
        // (Dev).lnk" so Windows 11 is willing to display the custom
        // jump-list entries on the taskbar button right-click.
        JumpListService.EnsureStartMenuShortcut();

        // Forward jump-list shortcuts BEFORE the single-instance guard.
        // Each jump-list entry re-launches Dimmy.Windows.exe with
        // `--command <name>`; we forward to the running instance via
        // named pipe and exit immediately, so the user sees no second
        // window flash.
        //
        // If TrySendCommand returns false there's no running instance
        // to forward to — we MUST fall through to normal startup
        // instead of exiting silently. Two scenarios this guards:
        //   1. Velopack post-update relaunch: ApplyUpdatesAndRestart
        //      defaults to forwarding the old process's argv as
        //      restartArgs. If the old Dimmy was started via jumplist
        //      (`--command open-settings`), the new one inherits the
        //      same arg, sees no running sibling, used to Exit(0) =
        //      app vanishes after update.
        //   2. User clicks jumplist while Dimmy has just crashed or
        //      shut down — they expect Dimmy to come up, not silently
        //      die. Same code path now stashes the command and lets
        //      normal startup proceed.
        // After init, `_pendingActivationPayload` style stash lets
        // HandleForwardedCommand pick it up once the pipe server is
        // up (mirrors the dimmy:// URL fallback pattern just above).
        var pipeCommand = TryGetPipeCommandFromArgs();
        if (pipeCommand is not null)
        {
            if (CommandPipeServer.TrySendCommand(pipeCommand))
            {
                Environment.Exit(0);
                return;
            }
            _pendingActivationPayload = pipeCommand;
        }

        // Single-instance guard: exit immediately if another Dimmy is already running.
        // Mutex name is flavor-aware (BuildInfo.SingleInstanceMutexName) so a
        // staging install can coexist with a prod install on the same machine
        // without the second launcher exiting silently.
        _singleInstanceMutex = new Mutex(true, BuildInfo.SingleInstanceMutexName, out bool createdNew);
        if (!createdNew)
        {
            // Another instance exists — just exit silently
            Environment.Exit(0);
            return;
        }

        _dispatcherQueue = DispatcherQueue.GetForCurrentThread();

        try
        {
            // 1. Initialize Rust core
            int result = DimmyNative.dimmy_init();
            if (result != 0)
            {
                System.Diagnostics.Debug.WriteLine("FATAL: dimmy_init() returned " + result);
                // Show a window anyway so app doesn't silently die
                var errorWin = new OnboardingWindow();
                errorWin.Activate();
                return;
            }

            // 2. Register event callback
            _eventCallbackDelegate = OnNativeEvent;
            DimmyNative.dimmy_set_event_callback(_eventCallbackDelegate);
            // Wire AppViewModel's optional log delegate through to
            // App.Log so diagnostic lines from the VM (e.g.
            // "meeting_state event: …") land in ptt.log. Kept as an
            // injection point so the VM stays cross-compilable into
            // Dimmy.Windows.Tests without taking App as a hard dep.
            ViewModels.AppViewModel.Log = (msg, tag) => Log(msg, tag);

            // 2b. Caption window — chunked transcriber emits stt_chunk
            // events from the Rust core; we route them through the
            // shared AppViewModel.SttChunkReceived hook.
            _appViewModel.SttChunkReceived += OnSttChunkReceived;

            // 2c. Realtime streaming dictation (Deepgram): each finalised
            // segment is typed at the cursor as the user speaks. The final
            // paste at stop is suppressed for these sessions (see
            // StopAndProcess) so the text isn't injected twice.
            _appViewModel.StreamingSegmentFinalized += OnStreamingSegmentFinalized;

            // 2d. Long-dictation guardrail (core-driven, event-based). A pill
            // dictation buffers unbounded audio (no draining worker), so the
            // core emits a soft warning at 5 min and a hard cap at 10 min.
            _appViewModel.DictationLongWarning += OnDictationLongWarning;
            _appViewModel.DictationMaxDurationReached += OnDictationMaxDuration;

            // 3. Load config into ViewModel
            LoadConfigIntoViewModel();

            // 3b. Load Win-only UI prefs (pill visibility toggles + theme).
            // Theme MUST be applied to the AppViewModel here at startup —
            // PillWindow reads _vm.Theme to decide glass-vs-dark in its
            // first render. Without this line the pill defaulted to
            // "Default" and only refreshed when the user opened Settings
            // (which triggered ApplySettings → AppViewModel.Theme write
            // → PillWindow PropertyChanged → re-render). Bug surfaced
            // 2026-05-08: "all'avvio la pill non prende il tema settato".
            _uiPrefs = UiPreferences.Load();
            _appViewModel.PillShowOnHotkey = _uiPrefs.PillShowOnHotkey;
            _appViewModel.PillShowOnStartup = _uiPrefs.PillShowOnStartup;
            _appViewModel.ShowTaskbarIcon = _uiPrefs.ShowTaskbarIcon;
            _appViewModel.TrayIconAlwaysVisible = _uiPrefs.TrayIconAlwaysVisible;
            _appViewModel.Theme = _uiPrefs.Theme;
            _appViewModel.PropertyChanged += OnUiPrefsRelevantPropertyChanged;



        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"FFI init error: {ex.GetType().Name}: {ex.Message}");
            // Continue without FFI — at least show the UI
        }

        // 4. Check onboarding
        bool onboardingDone = IsOnboardingComplete();

        try
        {
            if (!onboardingDone)
            {
                _onboardingWindow = new OnboardingWindow();
                _onboardingWindow.Closed += OnboardingWindow_Closed;
                _onboardingWindow.Activate();
            }
            else
            {
                StartNormalMode();
            }
        }
        catch (Exception ex)
        {
            var msg = $"FATAL UI: {ex.GetType().Name}: {ex.Message}\n{ex.StackTrace}";
            PttLog(msg);
            System.Diagnostics.Debug.WriteLine(msg);
            try { System.IO.File.WriteAllText(
                System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy", "crash.log"), msg); } catch { }
        }
    }

    /// <summary>Apply a shortcut + mode chosen in onboarding to the LIVE
    /// hotkey immediately. ShowPillAndHotkey early-returns when the pill is
    /// already up (re-run from Settings), so it would never re-register the
    /// new combo on its own — this does it explicitly. Safe before the
    /// HotkeyService exists (first run): it just primes the AppViewModel and
    /// ShowPillAndHotkey registers it when it creates the service.</summary>
    public void ApplyOnboardingShortcut(string shortcut, string mode)
    {
        if (!string.IsNullOrWhiteSpace(shortcut))
            _appViewModel.Shortcut = shortcut;
        if (!string.IsNullOrWhiteSpace(mode))
            _appViewModel.ShortcutMode = mode;
        if (_hotkeyService != null)
        {
            _hotkeyService.PttMode = _appViewModel.ShortcutMode == "hold";
            _hotkeyService.Register(_appViewModel.Shortcut);
        }
    }

    /// <summary>Show pill + register hotkey. Safe to call multiple times.</summary>
    public void ShowPillAndHotkey()
    {
        if (_pillWindow != null) return; // already shown

        _pillWindow = new PillWindow(_appViewModel);
        _pillWindow.Activate();

        var hwnd = WindowHelper.GetHwnd(_pillWindow);

        // Force hide from taskbar — belt-and-suspenders with EnableTransparency's TOOLWINDOW
        WindowHelper.SetTaskbarVisibility(hwnd, false);

        // Respect the user's "taskbar-only" choice: if they've turned
        // off PillShowOnStartup, hide the pill immediately after we've
        // registered it. We can't skip creation entirely because the
        // hotkey + transcription flow still needs the pill object —
        // we just keep its window invisible.
        if (!_appViewModel.PillShowOnStartup)
            HidePill();

        _hotkeyService = new HotkeyService(_dispatcherQueue!);
        _hotkeyService.HotkeyPressed += OnHotkeyPressed;
        _hotkeyService.HotkeyReleased += OnHotkeyReleased;
        _hotkeyService.CommandHotkeyPressed += OnCommandHotkeyPressed;
        _hotkeyService.CommandHotkeyReleased += OnCommandHotkeyReleased;
        _hotkeyService.MeetingHotkeyPressed += OnMeetingHotkeyPressed;
        _hotkeyService.PttMode = _appViewModel.ShortcutMode == "hold";
        _hotkeyService.Register(_appViewModel.Shortcut);
        // Optional dedicated command-mode hotkey (empty = disabled). Runs on
        // the same hook, so it inherits toggle/PTT + every combo.
        _hotkeyService.SetCommandShortcut(_uiPrefs.CommandHotkey);
        // Optional meeting start/stop hotkey (empty = disabled). Toggle-only.
        _hotkeyService.SetMeetingShortcut(_uiPrefs.MeetingHotkey);
    }

    private void StartNormalMode()
    {
        // Check audio device health before starting
        CheckAudioHealth();

        ShowPillAndHotkey();

        _trayService = new TrayService(
            vm: _appViewModel,
            onTogglePill: TogglePill,
            onSettingsClick: OpenSettings,
            onQuitClick: Quit,
            onMeetingClick: OpenMeetingWindow);

        // Initialize tray icon with the pill window's HWND
        if (_pillWindow != null)
        {
            var hwnd = WindowHelper.GetHwnd(_pillWindow);
            _trayService.Initialize(hwnd);

            // Opt-in only: pin the tray icon to the always-visible area if the
            // user asked for it. We never demote on a default-off startup so a
            // manual Windows pin is left untouched.
            if (_uiPrefs.TrayIconAlwaysVisible)
                _trayService.SetPromoted(true);

            // Wire WinUI 3 context menu from pill window
            var pill = _pillWindow as Views.PillWindow;
            _trayService.SetMenuCallback(() =>
                _dispatcherQueue?.TryEnqueue(() => pill?.ShowContextMenu()));
        }

        InitTaskbarAnchor();
        InitCommandPipeAndJumpList();
        InitCallDetection();

        // If the launch came from a `dimmy://activate?…` URL but no
        // running instance was around to handle it, we stashed the
        // payload pre-mutex. Dispatch it now that the pipe server
        // (and the rest of the UI) is up.
        if (_pendingActivationPayload is not null)
        {
            HandleForwardedCommand(_pendingActivationPayload);
            _pendingActivationPayload = null;
        }

        // Best-effort refresh — if we have a license, bump last_online_check
        // server-side so the soft-suspend grace clock stays accurate. Errors
        // are silent (offline / server unreachable / no license) — the
        // existing on-disk token continues to work either way.
        _ = Task.Run(async () =>
        {
            try
            {
                var s = Dimmy.Windows.Services.LicenseService.GetStatus();
                if (s.Kind is "TrialActive" or "Active" or "Suspended")
                {
                    var r = await Dimmy.Windows.Services.LicenseService.RefreshAsync();
                    if (!r.Ok)
                        PttLog($"[license] launch refresh: {r.Error}");
                    else
                        PttLog("[license] launch refresh ok");
                }
            }
            catch (Exception ex)
            {
                PttLog($"[license] launch refresh error: {ex.Message}");
            }
        });
    }

    /// <summary>
    /// Stand up the named-pipe server that listens for forwarded
    /// jump-list commands, and (re)register the Windows jump list so
    /// the user gets right-click access on the taskbar icon to:
    /// toggle pill, open settings, switch style, switch translate-to,
    /// quit.
    ///
    /// The two pieces are coupled: the jump-list shortcuts re-launch
    /// our EXE with `--command X`, and OnLaunched (above) forwards
    /// that to the running instance via this pipe. So the pipe must
    /// be up before the jump list goes live, but in practice the user
    /// can't click an entry that fast — order is purely defensive.
    /// </summary>
    private void InitCommandPipeAndJumpList()
    {
        try
        {
            _commandPipe = new CommandPipeServer(HandleForwardedCommand);
            _commandPipe.Start();
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] CommandPipe start failed: {ex.Message}");
        }

        try { JumpListService.Register(); }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] JumpList register failed: {ex.Message}");
        }

        // Auto-update: Velopack wrapper kicks off ~5 s after startup
        // and re-checks every 6 h while Dimmy stays open. Surfaces an
        // UpdateReady event the Settings card + taskbar overlay
        // subscribe to. No admin prompt; writes to %LOCALAPPDATA%.
        try
        {
            UpdateService.EnsureCreated();
            UpdateService.Instance?.StartBackgroundLoop();

            // Post-update confirmation. If the previous run finished a
            // Velopack apply-and-restart, it left an `update-pending.txt`
            // marker. Consume + surface a toast so the user sees an
            // explicit "yes, you're on the new version". The marker is
            // single-shot (deleted on read) so a leaked file from a
            // partial crash doesn't replay forever.
            var justInstalled = UpdateService.ConsumeUpdatePendingMarker();
            if (justInstalled is not null)
            {
                Log($"post-update toast: v{justInstalled}", "Update");
                // Marshal to UI dispatcher so the toast window opens on
                // the right thread — OnLaunched runs there already, but
                // future refactors may not.
                RunOnUI(() => DictNotificationService.ShowUpdateInstalled(justInstalled));
            }
        }
        catch (Exception ex)
        {
            Log($"UpdateService start failed: {ex.Message}", "Update");
        }

        // Custom-dictionary hotkey — a SECOND global hotkey via Win32
        // RegisterHotKey, independent of the Rust low-level hook that
        // owns the main dictation combo. Default Ctrl+Shift+D, user-
        // configurable in Settings → Voice input. On press: simulate
        // Ctrl+C silently, read the clipboard, push the word into the
        // Rust user_dict (which persists to config.json + applies as
        // STT bias on every future transcribe).
        try
        {
            _dictHotkey = new DictHotkeyService();
            _dictHotkey.Triggered += OnDictHotkeyTriggered;
            _dictHotkey.Register(_uiPrefs.DictHotkey);
        }
        catch (Exception ex)
        {
            Log($"DictHotkey start failed: {ex.Message}", "Hotkey");
        }

        // The dedicated command-mode hotkey is wired in ShowPillAndHotkey
        // (CommandHotkeyPressed/Released on the shared Rust hook) + bound via
        // SetCommandShortcut. No separate OS hotkey object — it lives on the
        // same low-level hook as dictation, so it inherits toggle/PTT + every
        // combo (incl. modifier-only like Win+Alt).
    }

    /// <summary>Command-mode hotkey PRESS — mirror of <see cref="OnHotkeyPressed"/>
    /// but flags the recording as a ONE-SHOT command (CommandOneShot routes the
    /// stop to the transform/generate path and paints the pill amber; it
    /// self-clears in StopAndCommandTransform). Honours the SAME ShortcutMode
    /// as dictation: in PTT mode this press starts and the release stops; in
    /// toggle mode this press starts and the next press stops.</summary>
    private void OnCommandHotkeyPressed()
    {
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            try
            {
                if (DimmyNative.dimmy_meeting_is_active() != 0)
                {
                    PttLog("command hotkey ignored — meeting recording in progress");
                    return;
                }

                if (!IsPillVisible() && _appViewModel.PillShowOnHotkey)
                    ShowPill();

                if (_appViewModel.ShortcutMode == "hold")
                {
                    // PTT: press starts a one-shot command recording.
                    if (!_appViewModel.IsBusy && !_pttStarted)
                    {
                        CaptureAndPushAppContext();
                        // Grab the selection NOW (pristine focus, no keys) so
                        // hold mode never fights the synthetic Ctrl+C.
                        BeginCommandSelectionCapture();
                        _pendingStop = false;
                        _pttStarted = true;
                        _appViewModel.SuppressRecordingStarted = false;
                        _appViewModel.CommandOneShot = true;
                        PttLog("Command hotkey (PTT): starting recording...");
                        var result = DimmyNative.dimmy_start_recording();
                        if (result == -1)
                        {
                            _pttStarted = false;
                            _appViewModel.CommandOneShot = false;
                            _appViewModel.SetError("No API key configured");
                        }
                        else if (result == -7)
                        {
                            _pttStarted = false;
                            _appViewModel.CommandOneShot = false;
                            PttLog("Command hotkey suppressed: meeting recording active (rc=-7)");
                        }
                        else if (result < 0)
                        {
                            _pttStarted = false;
                            _appViewModel.CommandOneShot = false;
                            _appViewModel.SetError($"Recording failed ({result})");
                        }
                        else if (_pendingStop)
                        {
                            PttLog("Command hotkey (PTT): pending stop after start — stopping");
                            _pttStarted = false;
                            await StopAndProcess();
                        }
                        else
                        {
                            PttLog("Command hotkey (PTT): recording started OK");
                        }
                    }
                }
                else
                {
                    // Toggle: press toggles the command recording on/off.
                    var now = Environment.TickCount64;
                    if (now - _lastCommandToggleMs < 300)
                    {
                        PttLog($"Command toggle debounce: {now - _lastCommandToggleMs}ms < 300ms, ignoring");
                        return;
                    }
                    _lastCommandToggleMs = now;

                    if (_appViewModel.IsRecording && !_stopInProgress)
                    {
                        _appViewModel.SuppressRecordingStarted = true;
                        PttLog("Command hotkey (toggle): stopping → command transform");
                        await StopAndProcess();
                    }
                    else if (!_appViewModel.IsBusy && !_stopInProgress)
                    {
                        CaptureAndPushAppContext();
                        BeginCommandSelectionCapture();
                        _appViewModel.SuppressRecordingStarted = false;
                        _appViewModel.CommandOneShot = true;
                        var result = DimmyNative.dimmy_start_recording();
                        if (result == -1)
                        {
                            _appViewModel.CommandOneShot = false;
                            _appViewModel.SetError("No API key configured");
                        }
                        else if (result == -7)
                        {
                            _appViewModel.CommandOneShot = false;
                            PttLog("Command toggle suppressed: meeting recording active (rc=-7)");
                        }
                        else if (result == -2)
                        {
                            PttLog("Command toggle race: start returned -2 (already recording) — treating as stop");
                            await StopAndProcess();
                        }
                        else if (result < 0)
                        {
                            _appViewModel.CommandOneShot = false;
                            _appViewModel.SetError($"Recording failed ({result})");
                        }
                        else
                        {
                            PttLog("Command hotkey (toggle): one-shot recording started");
                            _appViewModel.SetState(AppState.Recording);
                        }
                    }
                }
            }
            catch (Exception ex)
            {
                PttLog($"Command hotkey press handler exc: {ex.Message}");
            }
        });
    }

    /// <summary>Command-mode hotkey RELEASE — only delivered in PTT mode
    /// (HotkeyService gates Released on PttMode). Mirror of
    /// <see cref="OnHotkeyReleased"/>: ends the held command recording and
    /// routes the stop to the transform/generate path via the still-set
    /// CommandOneShot flag.</summary>
    private void OnCommandHotkeyReleased()
    {
        _pendingStop = true;
        _pttStarted = false;
        PttLog("Command key released — enqueueing stop");
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            _appViewModel.SuppressRecordingStarted = true;
            PttLog($"Command release handler executing, state={_appViewModel.CurrentState}");
            await StopAndProcess();
            if (_appViewModel.CurrentState == AppState.Recording)
            {
                PttLog("Command release: force-resetting to Idle");
                _appViewModel.SetState(AppState.Idle);
            }
        });
    }

    /// <summary>Add-to-dictionary handler — fires on the DictHotkey
    /// pump thread, marshals to UI thread to read clipboard and call
    /// the FFI. Sequence: simulate Ctrl+C → wait 80 ms for the host
    /// app to honour the copy → read text from clipboard → trim →
    /// call <c>dimmy_user_dict_add</c>. Logs every step into ptt.log
    /// (tag "Dict") so the user can diagnose "I pressed it but
    /// nothing happened" cases.</summary>
    private void OnDictHotkeyTriggered()
    {
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            // Deterministic clipboard-update detection via
            // GetClipboardSequenceNumber: capture seq, do the work,
            // poll until seq increments. No arbitrary wall-clock
            // delays — the poll exits the same millisecond the
            // target app finishes its copy handler. The 200/500 ms
            // ceilings are safety nets for unresponsive apps
            // (frozen Electron, sandbox refusing copy), NOT
            // "wait this long because we hope it's done".
            const string Sentinel = "__DIMMY_DICT_PROBE_v1__";
            const int SeqPollIntervalMs = 1;
            const int SetContentMaxWaitMs = 200;
            const int CopyMaxWaitMs = 500;

            string? previousText = null;
            try
            {
                // Stash existing clipboard text so we can restore at
                // the end (probe shouldn't clobber the user's
                // paste buffer).
                try
                {
                    var prev = global::Windows.ApplicationModel.DataTransfer.Clipboard.GetContent();
                    if (prev.Contains(global::Windows.ApplicationModel.DataTransfer.StandardDataFormats.Text))
                        previousText = await prev.GetTextAsync();
                }
                catch { }

                // ── Phase 1: write sentinel + wait for seq bump ──
                uint preSet = Services.TextInjectionService.ClipboardSequenceNumber();
                var probe = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
                probe.SetText(Sentinel);
                global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(probe);
                if (!await WaitForSequenceBumpAsync(preSet, SetContentMaxWaitMs, SeqPollIntervalMs))
                {
                    Log("DictHotkey: sentinel write didn't bump clipboard seq — abort", "Dict");
                    return;
                }

                // ── Phase 2: atomic Ctrl+C ── (SendCtrlC batches
                // phantom-modifier releases + Ctrl+C in one
                // SendInput call — no race, no inter-phase sleep).
                uint preCopy = Services.TextInjectionService.ClipboardSequenceNumber();
                Services.TextInjectionService.SendCtrlC();

                // ── Phase 3: wait for the app to fulfill copy ──
                if (!await WaitForSequenceBumpAsync(preCopy, CopyMaxWaitMs, SeqPollIntervalMs))
                {
                    Log("DictHotkey: app didn't update clipboard (no selection / password field / frozen)", "Dict");
                    RestoreClipboard(previousText);
                    return;
                }

                var dp = global::Windows.ApplicationModel.DataTransfer.Clipboard.GetContent();
                if (!dp.Contains(global::Windows.ApplicationModel.DataTransfer.StandardDataFormats.Text))
                {
                    Log("DictHotkey: clipboard non-text after copy — restoring", "Dict");
                    RestoreClipboard(previousText);
                    return;
                }
                var raw = await dp.GetTextAsync();
                if (raw == Sentinel)
                {
                    // seq bumped to a different sentinel? Race —
                    // someone else wrote then we wrote again.
                    // Treat as no-op.
                    Log("DictHotkey: clipboard still sentinel after seq bump (race?)", "Dict");
                    RestoreClipboard(previousText);
                    return;
                }
                var text = (raw ?? "").Trim();
                if (string.IsNullOrEmpty(text))
                {
                    Log("DictHotkey: clipboard text empty after trim", "Dict");
                    RestoreClipboard(previousText);
                    return;
                }
                // Cap at 100 chars — covers long names ("Software
                // Engineer Senior Manager", "Centro Studi Erickson
                // Trento") while still catching paragraph grabs.
                if (text.Length > 100)
                {
                    Log($"DictHotkey: rejected too-long selection ({text.Length} chars) — use Settings for phrases", "Dict");
                    RestoreClipboard(previousText);
                    return;
                }
                int rc = Services.DictionaryService.Add(text);
                Log($"DictHotkey: add '{text}' rc={rc}", "Dict");
                // Visible feedback: rc 0 = added, 1 = already present.
                // Both show a toast so the user knows the hotkey
                // worked (no silent failures from their perspective).
                switch (rc)
                {
                    case 0:
                        Services.DictNotificationService.ShowAdded(text);
                        Interop.DimmyNative.TrackEvent(
                            "user_dict.word_added", new { source = "hotkey" });
                        break;
                    case 1: Services.DictNotificationService.ShowAlreadyPresent(text); break;
                }
                RestoreClipboard(previousText);
            }
            catch (Exception ex)
            {
                Log($"DictHotkey handler exc: {ex.Message}", "Dict");
                RestoreClipboard(previousText);
            }
        });
    }

    /// <summary>Poll <see cref="Services.TextInjectionService.ClipboardSequenceNumber"/>
    /// at <paramref name="intervalMs"/> ms until it differs from
    /// <paramref name="baselineSeq"/> or <paramref name="timeoutMs"/>
    /// elapses. Returns true on bump, false on timeout. The poll loop
    /// is yield-friendly (uses Task.Delay, not Thread.Sleep) so the
    /// dispatcher queue stays responsive while we wait.</summary>
    private static async System.Threading.Tasks.Task<bool> WaitForSequenceBumpAsync(
        uint baselineSeq, int timeoutMs, int intervalMs)
    {
        var sw = System.Diagnostics.Stopwatch.StartNew();
        while (sw.ElapsedMilliseconds < timeoutMs)
        {
            uint now = Services.TextInjectionService.ClipboardSequenceNumber();
            // seq=0 from GetClipboardSequenceNumber means the caller
            // lacks the right to read; treat as "polling unsupported"
            // and bail with timeout — caller falls back gracefully.
            if (now == 0) return false;
            if (now != baselineSeq) return true;
            await System.Threading.Tasks.Task.Delay(intervalMs);
        }
        return false;
    }

    /// <summary>Best-effort restore of whatever text the user had on
    /// the clipboard before our probe overwrote it. Silent on failure
    /// — clipboard contention happens, and the cost of NOT restoring
    /// is just one Ctrl+V missing its expected text, recoverable by
    /// the user.</summary>
    private static void RestoreClipboard(string? previousText)
    {
        if (previousText is null) return;
        try
        {
            var dp = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(previousText);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
        }
        catch { }
    }

    /// <summary>Dispatcher for the line received over the command
    /// pipe (sent by a transient `--command X` instance). Always
    /// marshals onto the UI thread before touching view-models or
    /// FFI — pipe callbacks fire on a thread-pool worker.</summary>
    private void HandleForwardedCommand(string command)
    {
        _dispatcherQueue?.TryEnqueue(() =>
        {
            try
            {
                if (command == "toggle-pill") { TogglePill(); return; }
                if (command == "open-settings") { OpenSettings(); return; }
                if (command.StartsWith("open-settings:", StringComparison.Ordinal))
                {
                    // dimmy://settings/<tag> deep link — open Settings
                    // and navigate to the named nav tag (e.g. "license").
                    var tag = command["open-settings:".Length..];
                    OpenSettingsWindowAt(tag);
                    return;
                }
                if (command == "open-meeting") { OpenMeetingWindow(); return; }
                // Recording actions (jumplist mirrors the global shortcuts).
                if (command == "start-dictation") { MenuToggleDictation(); return; }
                if (command == "arm-command") { MenuArmCommandOneShot(); return; }
                if (command == "toggle-meeting") { _ = ToggleMeetingFromShortcutAsync("jumplist"); return; }
                if (command == "quit") { Quit(); return; }
                if (command.StartsWith("set-style:", StringComparison.Ordinal))
                {
                    var style = command["set-style:".Length..];
                    _appViewModel.LlmStyle = style;
                    DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(
                        new System.Collections.Generic.Dictionary<string, object>
                        {
                            ["llm_style"] = style,
                            ["llm_enabled"] = style != "off",
                        }));
                    return;
                }
                if (command.StartsWith("set-translate:", StringComparison.Ordinal))
                {
                    var code = command["set-translate:".Length..];
                    _appViewModel.LlmTranslateTo = code;
                    DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(
                        new System.Collections.Generic.Dictionary<string, string>
                        {
                            ["llm_translate_to"] = code,
                        }));
                    return;
                }
                if (command.StartsWith("activate-code:", StringComparison.Ordinal))
                {
                    var code = command["activate-code:".Length..];
                    PttLog($"[license] activate-code received (len={code.Length})");
                    _ = Task.Run(async () =>
                    {
                        bool ok = false;
                        try
                        {
                            var r = await Dimmy.Windows.Services.LicenseService
                                .RedeemAsync(code, Environment.MachineName);
                            ok = r.Ok;
                            PttLog(r.Ok
                                ? "[license] activated via dimmy:// scheme"
                                : $"[license] activation failed: {r.Error}");
                        }
                        catch (Exception ex)
                        {
                            PttLog($"[license] activate-code error: {ex.Message}");
                        }
                        finally
                        {
                            Dimmy.Windows.Services.LicenseService.NotifyChanged();
                            // Surface confirmation: pop Settings → License so the user
                            // sees the result. Without this, activation is silent and
                            // they don't know whether the magic-link click landed.
                            if (ok)
                            {
                                _dispatcherQueue?.TryEnqueue(() =>
                                {
                                    try { OpenSettingsWindowAt("license"); }
                                    catch (Exception ex)
                                    {
                                        PttLog($"[license] OpenSettingsWindowAt failed: {ex.Message}");
                                    }
                                });
                            }
                        }
                    });
                    return;
                }
                if (command.StartsWith("activate-token:", StringComparison.Ordinal))
                {
                    var token = command["activate-token:".Length..];
                    PttLog($"[license] activate-token received (len={token.Length}) — pre-signed token paste not yet supported");
                    return;
                }
                System.Diagnostics.Debug.WriteLine($"[App] unknown forwarded command: {command}");
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[App] HandleForwardedCommand: {ex.Message}");
            }
        });
    }

    /// <summary>Parse `--command <name>` from the process command line.
    /// Returns null if the flag isn't present (= normal launch).
    /// `Environment.GetCommandLineArgs()` is the only reliable source
    /// for WinUI 3 packaged apps — `LaunchActivatedEventArgs` strips
    /// args in some shell-launch paths.</summary>
    private static string? TryGetPipeCommandFromArgs()
    {
        var args = Environment.GetCommandLineArgs();
        for (int i = 1; i < args.Length - 1; i++)
        {
            if (args[i] == "--command")
                return args[i + 1];
        }
        return null;
    }

    /// <summary>Walk the command-line args looking for a `dimmy://`
    /// URL (Windows passes the URL as a single argv entry per the
    /// `"%1"` registered command). Convert to a pipe-command payload
    /// so the rest of the dispatch flow (HandleForwardedCommand)
    /// doesn't have to know about URL parsing.</summary>
    private static string? TryGetActivationPipeCommand()
    {
        var args = Environment.GetCommandLineArgs();
        for (int i = 1; i < args.Length; i++)
        {
            var raw = args[i];
            if (string.IsNullOrEmpty(raw)) continue;
            if (!raw.StartsWith("dimmy://", StringComparison.OrdinalIgnoreCase)) continue;

            // First try the activation flow (license magic links).
            var (code, token) = UrlSchemeRegistrar.ParseActivationUrl(raw);
            if (code is not null) return $"activate-code:{code}";
            if (token is not null) return $"activate-token:{token}";

            // Then fall through to host-only "open this surface" routes.
            // dimmy://meeting        -> open the Meeting window
            // dimmy://settings       -> open Settings (default tab)
            // dimmy://settings/license -> open Settings on the License tab
            // The pipe command IDs match HandleForwardedCommand's switch
            // (line 410-ish in this file). Add a new host here +
            // matching case there to expose more deeplinks.
            if (Uri.TryCreate(raw, UriKind.Absolute, out var uri)
                && string.Equals(uri.Scheme, "dimmy", StringComparison.OrdinalIgnoreCase))
            {
                var host = uri.Host?.ToLowerInvariant();
                switch (host)
                {
                    case "meeting":
                        return "open-meeting";
                    case "settings":
                        var path = uri.AbsolutePath?.Trim('/').ToLowerInvariant();
                        if (!string.IsNullOrEmpty(path))
                            return $"open-settings:{path}";
                        return "open-settings";
                }
            }
        }
        return null;
    }

    /// <summary>
    /// Stand up the off-screen anchor window + ITaskbarList3 service
    /// so the Windows taskbar gets a Dimmy button with state-colored
    /// overlay dots — the closest Windows analogue to the macOS menu
    /// bar status icon. Subscribes to AppViewModel.CurrentState so the
    /// overlay updates in realtime as recording transitions through
    /// the pipeline.
    /// </summary>
    private void InitTaskbarAnchor()
    {
        try
        {
            _taskbarAnchor = new TaskbarAnchorWindow();
            _taskbarAnchor.TaskbarClicked += OnTaskbarAnchorClicked;
            // Stamp our AUMI on the anchor window's property store so
            // Windows 11 binds the jump list to this taskbar entry.
            // The process-wide AUMI alone (set in OnLaunched) is
            // sometimes insufficient on Win11 for unpackaged apps.
            JumpListService.SetWindowAumi(_taskbarAnchor.Hwnd);
            // Honour the user's UiPreferences.ShowTaskbarIcon — the
            // window object always exists (TaskbarService routes
            // amplitude / state updates through its HWND), but its
            // AppWindow stays hidden when the user opted out.
            if (_appViewModel.ShowTaskbarIcon)
                _taskbarAnchor.ActivateAnchor();

            _taskbarService = new TaskbarService(_taskbarAnchor.Hwnd);
            // Reflect any state we already have (Idle on first launch).
            _taskbarService.UpdateState(_appViewModel.CurrentState);

            _appViewModel.PropertyChanged += OnAppViewModelPropertyChangedForTaskbar;
        }
        catch (Exception ex)
        {
            // Taskbar polish — never let a failure here take down the
            // app. Tray + pill keep working without it.
            System.Diagnostics.Debug.WriteLine($"[App] InitTaskbarAnchor failed: {ex.Message}");
        }
    }

    private void OnAppViewModelPropertyChangedForTaskbar(object? sender,
        System.ComponentModel.PropertyChangedEventArgs e)
    {
        var name = e.PropertyName;
        if (name != nameof(AppViewModel.CurrentState)
            && name != nameof(AppViewModel.MeetingActive)
            && name != nameof(AppViewModel.MeetingPaused)) return;

        var state = _appViewModel.CurrentState;
        var mActive = _appViewModel.MeetingActive;
        var mPaused = _appViewModel.MeetingPaused;
        _dispatcherQueue?.TryEnqueue(() =>
        {
            if (name == nameof(AppViewModel.CurrentState))
                _taskbarService?.UpdateState(state);
            _trayService?.UpdateState(state, mActive, mPaused);
        });
    }

    private void OnTaskbarAnchorClicked()
    {
        // The taskbar button was clicked — toggle pill visibility, just
        // like the tray icon does on left-click.
        _dispatcherQueue?.TryEnqueue(TogglePill);
    }

    /// <summary>Persist the UI-only Windows preferences (pill
    /// visibility toggles) on every change. Cheap — single small JSON
    /// file write under %APPDATA%\dimmy\.</summary>
    private void OnUiPrefsRelevantPropertyChanged(object? sender,
        System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(AppViewModel.PillShowOnHotkey)
            && e.PropertyName != nameof(AppViewModel.PillShowOnStartup)
            && e.PropertyName != nameof(AppViewModel.ShowTaskbarIcon)
            && e.PropertyName != nameof(AppViewModel.TrayIconAlwaysVisible))
            return;
        _uiPrefs.PillShowOnHotkey = _appViewModel.PillShowOnHotkey;
        _uiPrefs.PillShowOnStartup = _appViewModel.PillShowOnStartup;
        _uiPrefs.ShowTaskbarIcon = _appViewModel.ShowTaskbarIcon;
        _uiPrefs.TrayIconAlwaysVisible = _appViewModel.TrayIconAlwaysVisible;
        _uiPrefs.Save();

        // Live-apply the taskbar-anchor visibility on toggle. The window
        // object stays around either way (TaskbarService updates funnel
        // through it), but its AppWindow is hidden so the taskbar entry
        // and amplitude overlay disappear. Toggling back re-activates.
        if (e.PropertyName == nameof(AppViewModel.ShowTaskbarIcon))
            ApplyTaskbarAnchorVisibility();

        // Live-apply the tray pin. Both directions on an explicit toggle:
        // turning it off demotes (the user chose to unpin), which differs
        // from the default-off startup that leaves manual pins alone.
        if (e.PropertyName == nameof(AppViewModel.TrayIconAlwaysVisible))
            _trayService?.SetPromoted(_appViewModel.TrayIconAlwaysVisible);
    }

    /// <summary>Show or hide the TaskbarAnchorWindow per the current
    /// `_appViewModel.ShowTaskbarIcon`. Safe to call before the anchor
    /// is constructed — early returns until InitTaskbarAnchor has run.</summary>
    private void ApplyTaskbarAnchorVisibility()
    {
        if (_taskbarAnchor is null) return;
        try
        {
            if (_appViewModel.ShowTaskbarIcon)
            {
                // ActivateAnchor also handles the "minimised, no focus
                // steal" semantics — same code path used at first launch.
                _taskbarAnchor.ActivateAnchor();
            }
            else
            {
                var aw = Helpers.WindowHelper.GetAppWindow(_taskbarAnchor);
                aw?.Hide();
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] ApplyTaskbarAnchorVisibility failed: {ex.Message}");
        }
    }

    private void OnboardingWindow_Closed(object sender, WindowEventArgs args)
    {
        // Marker is written explicitly once the user commits to a model choice
        // (see OnboardingWindow.PersistModelChoice). Closing without choosing
        // leaves the marker absent → onboarding shows again next launch.
        StartNormalMode();
    }

    /// Re-open the welcome guide on demand from Settings → About. The app
    /// is already in normal mode (pill + hotkey + tray live), so unlike the
    /// first-launch path this MUST NOT call StartNormalMode again — that
    /// would stack a second TrayService / taskbar anchor. The wizard's
    /// DetectPriorState recognises already-downloaded models and an existing
    /// saved key, so a re-run never re-downloads or asks for a key the user
    /// already has. On close we pull the (possibly changed) config back into
    /// the running UI and re-register the hotkey so a new model / shortcut
    /// applies without a restart.
    public void RelaunchOnboarding()
    {
        if (_onboardingWindow is not null)
        {
            _onboardingWindow.Activate();
            return;
        }
        _onboardingWindow = new OnboardingWindow();
        _onboardingWindow.Closed += OnboardingRerunClosed;
        _onboardingWindow.Activate();
    }

    private void OnboardingRerunClosed(object sender, WindowEventArgs args)
    {
        _onboardingWindow = null;
        // The wizard already persisted any choice via dimmy_set_config_json
        // (single-writer merge preserves untouched keys). Ensure the marker
        // exists, then refresh the live view model + hotkey from the core.
        MarkOnboardingComplete();
        try
        {
            ReloadConfig();
            _hotkeyService?.Register(_appViewModel.Shortcut);
            if (_hotkeyService != null)
                _hotkeyService.PttMode = _appViewModel.ShortcutMode == "hold";
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[Onboarding rerun] refresh failed: {ex.Message}");
        }
    }

    /// Subtitle-style routing of stt_chunk events: the caption window
    /// shows a FIFO of the last N chunk deltas (currently 2), centered
    /// at the bottom of the primary display. The cumulative text used
    /// for the final paste is owned upstream — only the rolling on-
    /// screen subtitles are managed here. AppViewModel still tracks
    /// the cumulative for any other consumer that wants it.
    private string _lastCumulative = "";
    private void OnSttChunkReceived(string cumulative, bool isFinal)
    {
        if (!_appViewModel.LiveCaptionsEnabled) return;

        if (_captionWindow == null)
        {
            _captionWindow = new CaptionWindow();
            // Never Activate(): the caption overlay is movie-subtitle
            // chrome, it must NOT steal focus from the user's target
            // app (otherwise the final Ctrl+V paste lands on this
            // borderless transparent window and the text disappears).
            // Mac parity: CaptionWindowController uses an NSPanel with
            // `.nonactivatingPanel` + `orderFrontRegardless` — same
            // intent, different API.
            Helpers.WindowHelper.ShowWithoutActivating(_captionWindow);
        }

        // Compute the per-chunk delta from the cumulative diff. The
        // Rust core also sends a `delta` field via the stt_chunk
        // payload, but routing it would mean changing AppViewModel's
        // signature — this keeps the FIFO logic compact and self-
        // contained on the C# side.
        string delta;
        if (!string.IsNullOrEmpty(_lastCumulative)
            && cumulative.StartsWith(_lastCumulative, StringComparison.Ordinal))
        {
            delta = cumulative.Substring(_lastCumulative.Length).Trim();
        }
        else
        {
            delta = cumulative.Trim();
        }
        _lastCumulative = cumulative;

        if (!string.IsNullOrEmpty(delta))
        {
            _captionWindow.PushChunk(delta);
        }
        _captionWindow.PositionAtScreenBottom();

        if (!isFinal)
        {
            _captionWindow.Show();
        }
        else
        {
            // Hide after ~1.2 s so the user gets a final glance, then
            // reset state for the next recording.
            var dq = _dispatcherQueue;
            _ = System.Threading.Tasks.Task.Delay(1200).ContinueWith(_ =>
            {
                dq?.TryEnqueue(() =>
                {
                    _captionWindow?.Hide();
                    _captionWindow?.Reset();
                    _appViewModel.LiveCaptionText = "";
                    _lastCumulative = "";
                });
            });
        }
    }

    /// <summary>Type one finalised streaming-dictation segment at the
    /// cursor. Runs on the UI thread (HandleEvent is dispatched there).
    /// Uses Unicode SendInput rather than clipboard paste so repeated
    /// per-segment injection doesn't thrash the user's clipboard. A
    /// trailing space keeps segments separated as they accrue.</summary>
    private void OnStreamingSegmentFinalized(string segment)
    {
        if (string.IsNullOrWhiteSpace(segment)) return;
        try
        {
            TextInjectionService.TypeUnicodeText(segment.TrimEnd() + " ");
        }
        catch (Exception ex)
        {
            PttLog($"StreamingSegment: inject EXC: {ex.Message}");
        }
    }

    private void OnNativeEvent(IntPtr jsonPtr)
    {
        try
        {
            var json = DimmyNative.MarshalEventJson(jsonPtr);
            _dispatcherQueue?.TryEnqueue(() => _appViewModel.HandleEvent(json));
        }
        catch
        {
            // Defensive: never let FFI callback crash the app
        }
    }

    private void LoadConfigIntoViewModel()
    {
        // Always read from config.json file — it's the complete source of truth.
        string? json = null;
        try
        {
            var path = Path.Combine(Services.BuildInfo.ConfigDirPath, "config.json");
            PttLog($"LoadConfig: looking for {path}, exists={File.Exists(path)}");
            if (File.Exists(path))
                json = File.ReadAllText(path);
        }
        catch (Exception ex) { PttLog($"LoadConfig: file read error: {ex.Message}"); }

        // Fallback to FFI if file not found
        if (string.IsNullOrEmpty(json))
        {
            PttLog("LoadConfig: file empty/missing, falling back to FFI");
            try { json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384); }
            catch (Exception ex) { PttLog($"LoadConfig: FFI error: {ex.Message}"); }
        }

        if (json == null) { PttLog("LoadConfig: no config available"); return; }

        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var r = doc.RootElement;
            if (r.TryGetProperty("shortcut", out var sc))
                _appViewModel.Shortcut = sc.GetString() ?? "Win+Alt";
            if (r.TryGetProperty("shortcut_mode", out var sm))
                _appViewModel.ShortcutMode = sm.GetString() ?? "toggle";
            if (r.TryGetProperty("language", out var lang))
                _appViewModel.Language = lang.GetString() ?? "";
            if (r.TryGetProperty("llm_translate_to", out var trans))
                _appViewModel.LlmTranslateTo = trans.GetString() ?? "";
            if (r.TryGetProperty("llm_style", out var style))
                _appViewModel.LlmStyle = style.GetString() ?? "off";
            if (r.TryGetProperty("selected_device", out var dev))
                _appViewModel.DeviceName = dev.GetString() ?? "";
            if (r.TryGetProperty("border_style", out var bs))
                _appViewModel.BorderStyle = bs.GetString() ?? "Rainbow";
            if (r.TryGetProperty("waveform_style", out var ws))
                _appViewModel.WaveformStyle = ws.GetString() ?? "Bars";
            if (r.TryGetProperty("overlay_position", out var op))
                _appViewModel.OverlayPosition = op.GetString() ?? "Bottom Right";
            if (r.TryGetProperty("keep_in_clipboard", out var kc))
                _appViewModel.KeepInClipboard = kc.GetBoolean();
            if (r.TryGetProperty("theme", out var pt))
                _appViewModel.Theme = pt.GetString() ?? "Default";
            // live_captions_enabled — defaults to true if absent (old configs).
            // Drives whether OnSttChunkReceived shows the floating caption
            // window. Independent of chunk_streaming_enabled.
            _appViewModel.LiveCaptionsEnabled =
                !r.TryGetProperty("live_captions_enabled", out var lce) || lce.GetBoolean();
            // call_detect_enabled — default true. The Rust state machine
            // is the source of truth for what fires; this mirror is just
            // a fast no-op gate in OnCallDetected for the in-flight
            // tick after a toggle-off (config round-trip hasn't reached
            // Rust yet).
            _appViewModel.CallDetectEnabled =
                !r.TryGetProperty("call_detect_enabled", out var cde) || cde.GetBoolean();
            if (r.TryGetProperty("call_detect_excluded_apps", out var cdex)
                && cdex.ValueKind == System.Text.Json.JsonValueKind.Array)
            {
                _appViewModel.CallDetectExcludedApps.Clear();
                foreach (var item in cdex.EnumerateArray())
                {
                    var s = item.GetString();
                    if (!string.IsNullOrEmpty(s))
                        _appViewModel.CallDetectExcludedApps.Add(s);
                }
            }
            // Reconfigure the service so a runtime toggle takes effect
            // without restarting the app.
            _callDetection?.SetEnabled(_appViewModel.CallDetectEnabled);
            PttLog($"LoadConfig: shortcut={_appViewModel.Shortcut}, mode={_appViewModel.ShortcutMode}");
        }
        catch (Exception ex) { PttLog($"LoadConfig: parse error: {ex.Message}"); }
    }

    /// Snapshot the foreground app's process name and push it to the
    /// Rust core. The Rust matcher uses it later (LLM-enhance time) to
    /// resolve any user-defined app_rules. Empty string is fine — Rust
    /// treats it as "no rule matches" and falls back to user defaults.
    /// Snapshot of the foreground window taken at the moment the user
    /// pressed the hotkey. Captured once in CaptureAndPushAppContext
    /// and consumed in the PASTE phase to detect "focus drift" (window
    /// switched between record and paste — paste lands somewhere
    /// unexpected). Cleared after PASTE so the next press starts fresh.
    private Helpers.AppContextCapture.CapturedTargetContext? _targetContext;

    /// <summary>Command Mode's selection, grabbed at PRESS time via UIA
    /// (no keyboard, no clipboard) while focus + selection are pristine —
    /// the reliable path for Outlook/Word/native edit controls and safe in
    /// hold mode. Awaited in <see cref="StopAndCommandTransform"/>; null
    /// result there falls back to the synthetic-Ctrl+C grab for Electron
    /// apps. Started on a background thread so it never delays recording.</summary>
    private Task<string?>? _pendingCommandSelection;

    /// <summary>Kick off the pristine-focus UIA selection read for a command
    /// recording. Fire-and-forget on the thread pool (MTA); the result is
    /// awaited at stop. Safe to call even if the app has no UIA text — it
    /// just resolves to null.</summary>
    private void BeginCommandSelectionCapture()
    {
        try
        {
            _pendingCommandSelection = Task.Run(() => Services.UiaSelectionReader.TryGetFocusedSelection());
        }
        catch (Exception ex)
        {
            _pendingCommandSelection = null;
            PttLog($"BeginCommandSelectionCapture EXC: {ex.Message}");
        }
    }

    private void CaptureAndPushAppContext()
    {
        try
        {
            var snap = Helpers.AppContextCapture.SnapshotForeground();
            _targetContext = snap.IsEmpty ? null : snap;
            PttLog($"PRESS target: {snap.ToLogString()}");
            // ToCoreJson stays privacy-safe (process_name only — no
            // window title, no exe path crosses FFI). Mac/Linux bundle
            // ids stay empty here.
            var rc = DimmyNative.dimmy_set_app_context(snap.ToCoreJson());
            // Fire-and-forget icon extraction: SHGetFileInfo on the exe
            // path → PNG cache. Future Settings → App Rules renders
            // pull the cached PNG instead of a hand-rolled SVG.
            if (!snap.IsEmpty && !string.IsNullOrEmpty(snap.ExecutablePath))
            {
                var exePath = snap.ExecutablePath;
                System.Threading.Tasks.Task.Run(() =>
                    Helpers.IconExtractor.EnsureCachedFromExePath(exePath));
            }
            // Always log the captured value so diagnosing "rules don't
            // match" only requires reading ptt.log: empty = capture
            // failed (UAC-elevated foreground, exotic shell), non-empty
            // = what we sent to Rust for the resolve() lookup.
            Log($"captured process='{snap.ProcessName}' rc={rc}", "AppCtx");
        }
        catch (Exception ex)
        {
            PttLog($"app context capture failed: {ex.Message}");
        }
    }

    private void OnHotkeyPressed()
    {
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            // Gate: if a meeting recording is active, swallow the hotkey
            // entirely. Starting a parallel dictation would corrupt the
            // shared cpal audio buffer (both writers append to the same
            // Vec<f32>). User gets visible feedback via the meeting
            // window's pulsing red dot — no toast needed.
            if (DimmyNative.dimmy_meeting_is_active() != 0)
            {
                PttLog("hotkey ignored — meeting recording in progress");
                return;
            }

            // Show pill if hidden — but only if the user hasn't opted
            // into "taskbar-only" mode. With PillShowOnHotkey=false the
            // recording status is conveyed exclusively via the taskbar
            // overlay icon (red dot + amplitude bar) and the pill is
            // never auto-resurrected.
            if (!IsPillVisible() && _appViewModel.PillShowOnHotkey)
                ShowPill();

            if (_appViewModel.ShortcutMode == "hold")
            {
                // PTT: press starts recording
                if (!_appViewModel.IsBusy && !_pttStarted)
                {
                    // Snapshot foreground app BEFORE start_recording —
                    // by the time Rust applies app_rules at LLM-enhance
                    // time the focus may have moved to Dimmy itself or
                    // wherever the paste landed.
                    CaptureAndPushAppContext();
                    _pendingStop = false; // clear before starting
                    _pttStarted = true;
                    _appViewModel.SuppressRecordingStarted = false; // allow recording_started event
                    PttLog("Starting recording...");
                    var result = DimmyNative.dimmy_start_recording();
                    if (result == -1)
                    {
                        _pttStarted = false;
                        _appViewModel.SetError("No API key configured");
                    }
                    else if (result == -7)
                    {
                        // Meeting recording is active — silent suppress per
                        // user spec. The pill stays at idle, no error toast,
                        // ptt.log gets the diagnostic line so it's debuggable
                        // if the user wonders why the hotkey didn't engage.
                        _pttStarted = false;
                        PttLog("PTT hotkey suppressed: meeting recording active (rc=-7)");
                    }
                    else if (result < 0)
                    {
                        _pttStarted = false;
                        _appViewModel.SetError($"Recording failed ({result})");
                    }
                    else if (_pendingStop)
                    {
                        // Release happened while we were starting — stop immediately
                        PttLog("Pending stop detected after start — stopping immediately");
                        _pttStarted = false;
                        await StopAndProcess();
                    }
                    else
                    {
                        PttLog("Recording started OK");
                    }
                }
            }
            else
            {
                // Toggle mode: press toggles recording on/off
                var now = Environment.TickCount64;
                if (now - _lastToggleMs < 300)
                {
                    PttLog($"Toggle debounce: {now - _lastToggleMs}ms < 300ms, ignoring");
                    return;
                }
                _lastToggleMs = now;

                if (_stopInProgress)
                {
                    // A stop is already running (whisper still processing a
                    // long dictation). Without this explicit arm the press
                    // falls through BOTH branches below — IsRecording can
                    // still be true while _stopInProgress is true — and is
                    // silently lost, so the user mashes the key and spawns
                    // concurrent stops (the race that drops the paste). Ack
                    // + no-op; the in-flight stop owns delivery.
                    PttLog("Toggle ignored: stop already in progress");
                    return;
                }
                if (_appViewModel.IsRecording)
                {
                    // Flip to Transcribing IMMEDIATELY so the pill leaves the
                    // red Recording dot the instant the user stops. whisper can
                    // take a couple seconds; a still-red pill is exactly what
                    // makes the user re-press (→ the concurrent-stop race that
                    // loses the paste). SetState also clears IsRecording.
                    _appViewModel.SetState(AppState.Transcribing);
                    await StopAndProcess();
                }
                else if (!_appViewModel.IsBusy)
                {
                    CaptureAndPushAppContext();
                    _appViewModel.SuppressRecordingStarted = false; // ensure Rust event is accepted
                    var result = DimmyNative.dimmy_start_recording();
                    if (result == -1)
                        _appViewModel.SetError("No API key configured");
                    else if (result == -7)
                    {
                        // Meeting in progress — silent no-op, log only.
                        PttLog("Toggle suppressed: meeting recording active (rc=-7)");
                    }
                    else if (result == -2)
                    {
                        // Race: Rust thinks it's already recording (a previous
                        // start that we initiated is still spinning up the
                        // audio stream and hasn't fired recording_started yet
                        // → ViewModel still shows IsRecording=false → we
                        // mistook this press as a "start" instead of "stop").
                        // Auto-recover: treat as the intended stop. This is
                        // the difference between a frustrating "Recording
                        // failed (-2)" toast and the user's intent.
                        PttLog("Toggle race: dimmy_start_recording returned -2 (already recording) — treating as stop");
                        await StopAndProcess();
                    }
                    else if (result < 0)
                        _appViewModel.SetError($"Recording failed ({result})");
                    else
                    {
                        // Optimistic state update: Rust is now recording, but
                        // the recording_started event roundtrip can take ~1s
                        // (audio stream build time). Without this, a quick
                        // second toggle press during the build window would
                        // see IsRecording=false and try to start AGAIN — the
                        // -2 race above. Setting state here closes the gap.
                        _appViewModel.SetState(AppState.Recording);
                    }
                }
            }
        });
    }

    private void OnHotkeyReleased()
    {
        _pendingStop = true; // signal pressed handler in case it hasn't finished yet
        _pttStarted = false;
        PttLog("Key released — enqueueing stop");
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            // Suppress late "recording_started" Rust callbacks that arrive after we stop
            _appViewModel.SuppressRecordingStarted = true;
            PttLog($"Release handler executing, state={_appViewModel.CurrentState}");
            await StopAndProcess();
            // Belt-and-suspenders: PTT release MUST end recording state
            if (_appViewModel.CurrentState == AppState.Recording)
            {
                PttLog("Force-resetting to Idle (state was still Recording after StopAndProcess)");
                _appViewModel.SetState(AppState.Idle);
            }
        });
    }

    /// <summary>The single dictation stop→transcribe→paste pipeline. Callable
    /// from the pill Stop button (<see cref="Views.PillWindow"/>) as well as
    /// the hotkey paths so BOTH go through the same focus-restore
    /// (<c>_targetContext</c>), the same <c>_stopInProgress</c> re-entry guard,
    /// and the same Completing→Idle transition. The caller flips the UI to
    /// Transcribing first (immediacy); this method owns everything after.</summary>
    internal async Task StopAndProcess()
    {
        if (_stopInProgress)
        {
            PttLog("StopAndProcess: already in progress, ignoring");
            return;
        }
        _stopInProgress = true;
        try
        {
            // Command Mode branch: instead of dictating, transform the
            // user's selected text with what they just spoke (or generate +
            // insert when nothing is selected). Two ways in: the sticky
            // pill-menu toggle (CommandMode) OR a one-shot fired by the
            // dedicated command hotkey (CommandOneShot).
            if (_appViewModel.CommandMode || _appViewModel.CommandOneShot)
            {
                await StopAndCommandTransform();
                return;
            }

            PttLog("StopAndProcess: calling dimmy_stop_recording...");
            var result = await Services.TranscriptionService.StopAndProcessAsync();
            PttLog($"StopAndProcess: IsSuccess={result.IsSuccess}, IsEmpty={result.IsEmpty}, IsSkipped={result.IsSkipped}, IsTimeout={result.IsTimeout}, Text={result.Text?.Length ?? 0} chars, Error={result.Error}");
            if (result.IsSkipped)
            {
                // A concurrent stop (pill Stop, or a mashed toggle) won the
                // Rust guard and owns the transcript + delivery. Do NOTHING:
                // don't paste, don't touch state — the winner drives the UI to
                // Completing/Idle. The finally releases our _stopInProgress.
                PttLog("StopAndProcess: concurrent stop already handling (rc -9) — no-op");
                return;
            }
            if (result.IsSuccess)
            {
                // Focus drift diagnostic: did the foreground window
                // change between PRESS (CaptureAndPushAppContext) and
                // now (PASTE)? If so the user clicked away during the
                // STT/LLM round-trip and our Ctrl+V will land in the
                // wrong app — log it explicitly so debugging "paste
                // disappeared" doesn't require ptt.log archaeology.
                var prePaste = Helpers.AppContextCapture.SnapshotForeground();
                var target = _targetContext;
                if (target == null)
                    PttLog($"PASTE pre: no target snapshot recorded; current fg={prePaste.ToLogString()}");
                else if (prePaste.Hwnd != target.Hwnd)
                {
                    PttLog($"PASTE pre: FOCUS DRIFT — target was 0x{target.Hwnd.ToInt64():X} '{target.WindowTitle}', now 0x{prePaste.Hwnd.ToInt64():X} '{prePaste.WindowTitle}' (proc='{prePaste.ProcessName}')");
                    // Defense-in-depth: restore the recorded target as
                    // foreground before SendInput, otherwise Ctrl+V
                    // lands wherever focus drifted (CaptionWindow,
                    // Toast notification, Teams popup, etc.). Same
                    // topmost-toggle pattern Settings/Meeting windows
                    // use to defeat Win11's foreground lock (see
                    // ForegroundSettingsWindow / OpenMeetingWindow).
                    if (target.Hwnd != IntPtr.Zero)
                    {
                        try
                        {
                            ShowWindow(target.Hwnd, SW_RESTORE);
                            SetWindowPos(target.Hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                            SetWindowPos(target.Hwnd, HWND_NOTOPMOST, 0, 0, 0, 0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                            bool ok = SetForegroundWindow(target.Hwnd);
                            // 40 ms gives the foreground change time
                            // to land before SendInput; without it
                            // the OS can still route the synthetic
                            // Ctrl+V to the old foreground in flight.
                            await System.Threading.Tasks.Task.Delay(40);
                            var post = Helpers.AppContextCapture.SnapshotForeground();
                            PttLog($"PASTE pre: foreground-restore SetForegroundWindow={ok}, now=0x{post.Hwnd.ToInt64():X} '{post.ProcessName}'");
                        }
                        catch (Exception ex)
                        {
                            PttLog($"PASTE pre: foreground-restore EXC: {ex.Message}");
                        }
                    }
                }
                else
                    PttLog($"PASTE pre: foreground unchanged ({prePaste.ToLogString()})");

                // Streaming dictation already typed each segment at the
                // cursor live, so skip the final paste — pasting the whole
                // transcript again would duplicate it. The text still flows
                // to history via the Rust core's normal save path.
                if (_appViewModel.StreamingDictationActive)
                {
                    PttLog($"StopAndProcess: streaming session — segments already injected, skipping final paste ({result.Text!.Length} chars)");
                }
                else
                {
                    PttLog($"StopAndProcess: pasting text ({result.Text!.Length} chars)");
                    await TextInjectionService.PasteText(result.Text!, _appViewModel.KeepInClipboard);
                }
                // Show completing state (checkmark) AFTER paste — PillWindow timer returns to Idle
                _appViewModel.SetState(AppState.Completing);
                _targetContext = null; // consumed
            }
            else if (result.IsTimeout)
            {
                _appViewModel.SetError(result.Error!);
            }
            else
            {
                // Empty transcription — always reset to idle
                PttLog("StopAndProcess: empty result, resetting to Idle");
                _appViewModel.SetState(AppState.Idle);
            }
        }
        catch (Exception ex)
        {
            PttLog($"StopAndProcess: EXCEPTION {ex.GetType().Name}: {ex.Message}");
            _appViewModel.SetError(ex.Message);
        }
        finally
        {
            _stopInProgress = false;
        }
    }

    /// <summary>Command Mode stop path: grab the user's selected text,
    /// transform it with the spoken instruction, paste the result over
    /// the selection. Falls back to plain dictation when nothing was
    /// selected. Shares the foreground-restore + paste primitives with
    /// the normal dictation path.</summary>
    private async Task StopAndCommandTransform()
    {
        try
        {
            // Bring the recorded target back to the foreground BEFORE the
            // Ctrl+C grab and the paste — both must land in the same app
            // the user was selecting in, not whatever stole focus during
            // the STT round-trip.
            await RestoreTargetForegroundAsync();

            // Prefer the pristine-focus UIA read taken at PRESS time (no
            // keyboard/clipboard, reliable in Outlook/Word/native controls).
            // Only fall back to the synthetic-Ctrl+C grab when UIA yielded
            // nothing — Electron apps (VS Code, Slack) need the Ctrl+C path.
            string? selection = null;
            if (_pendingCommandSelection != null)
            {
                try { selection = await _pendingCommandSelection; } catch { selection = null; }
                _pendingCommandSelection = null;
                PttLog($"CommandMode: UIA pre-capture = {(selection == null ? "(none)" : $"{selection.Length} chars")}");
            }
            if (selection == null)
            {
                selection = await Services.SelectionCaptureService.CaptureAsync();
                PttLog($"CommandMode: Ctrl+C fallback = {(selection == null ? "(none)" : $"{selection.Length} chars")}");
            }

            var result = await Services.TranscriptionService.StopAndCommandAsync(selection);

            if (result.IsEmptyTranscript)
            {
                PttLog("CommandMode: empty transcript — idle");
                _appViewModel.SetState(AppState.Idle);
                return;
            }
            if (!result.IsSuccess)
            {
                PttLog($"CommandMode: failed — {result.Error}");
                _appViewModel.SetError(result.Error!);
                return;
            }

            // Re-assert foreground (the transform round-trip can take
            // seconds; focus may have drifted again) then paste. With a
            // selection the paste REPLACES it; with no selection there's
            // nothing highlighted so the same paste INSERTS the generated
            // text at the cursor — the behaviour the user expects for both.
            await RestoreTargetForegroundAsync();
            PttLog($"CommandMode: pasting result ({result.Text!.Length} chars)");
            await TextInjectionService.PasteText(result.Text!, _appViewModel.KeepInClipboard);
            _appViewModel.SetState(AppState.Completing);
            _targetContext = null;
        }
        catch (Exception ex)
        {
            PttLog($"CommandMode: EXCEPTION {ex.GetType().Name}: {ex.Message}");
            _appViewModel.SetError(ex.Message);
        }
        finally
        {
            // A one-shot (dedicated-hotkey) command always reverts to normal
            // output afterwards. The sticky menu toggle (CommandMode) is
            // left untouched, so it stays on across commands as intended.
            _appViewModel.CommandOneShot = false;
        }
    }

    /// <summary>Restore the recorded PTT target (`_targetContext`) to the
    /// foreground using the topmost-toggle trick that defeats Win11's
    /// foreground lock. No-op when there's no recorded target or it's
    /// already in front. Mirrors the inline restore in StopAndProcess —
    /// extracted so Command Mode can reuse it for both its Ctrl+C grab
    /// and its paste.</summary>
    private async Task RestoreTargetForegroundAsync()
    {
        var target = _targetContext;
        if (target == null || target.Hwnd == IntPtr.Zero) return;
        var cur = Helpers.AppContextCapture.SnapshotForeground();
        if (cur.Hwnd == target.Hwnd) return;
        try
        {
            ShowWindow(target.Hwnd, SW_RESTORE);
            SetWindowPos(target.Hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            SetWindowPos(target.Hwnd, HWND_NOTOPMOST, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            SetForegroundWindow(target.Hwnd);
            await Task.Delay(40);
        }
        catch (Exception ex)
        {
            PttLog($"RestoreTargetForeground EXC: {ex.Message}");
        }
    }

    public void RepositionPill()
    {
        if (_pillWindow == null) return;
        WindowHelper.PositionByPreset(_pillWindow, _appViewModel.OverlayPosition, 240, 56);
    }

    public void HidePill()
    {
        if (_pillWindow == null) return;
        var appWindow = WindowHelper.GetAppWindow(_pillWindow);
        appWindow?.Hide();
        _trayService?.UpdateState("Dimmy — Hidden (hotkey still active)", "");
    }

    public void ShowPill()
    {
        if (_pillWindow == null) return;
        WindowHelper.ShowWithoutActivating(_pillWindow);
        _trayService?.UpdateState("Dimmy — Ready", "");
    }

    public bool IsPillVisible()
    {
        if (_pillWindow == null) return false;
        var appWindow = WindowHelper.GetAppWindow(_pillWindow);
        return appWindow?.IsVisible ?? false;
    }

    public void TogglePill()
    {
        if (IsPillVisible()) HidePill();
        else ShowPill();
    }

    private SettingsWindow? _settingsWindow;

    public void OpenSettingsWindow() => OpenSettings();

    /// Open the dedicated MeetingWindow (or activate it if already
    /// open). Triggered from the jump-list "Meetings" entry and from
    // ── Call auto-detect wiring ──────────────────────────────────────

    private void InitCallDetection()
    {
        Log($"InitCallDetection ENTRY: _callDetection={(_callDetection == null ? "null" : "set")} _dispatcherQueue={(_dispatcherQueue == null ? "null" : "set")} CallDetectEnabled={_appViewModel.CallDetectEnabled}", "CallDetect");
        try
        {
            if (_callDetection != null || _dispatcherQueue == null)
            {
                Log("InitCallDetection EARLY-RETURN", "CallDetect");
                return;
            }
            _callDetection = new Services.CallDetectionService(_dispatcherQueue);
            _callDetection.SetEnabled(_appViewModel.CallDetectEnabled);
            _callDetection.Start();
            _appViewModel.CallDetected += OnCallDetected;
            _appViewModel.CallEnded += OnCallEnded;
            _appViewModel.CallStopSuggested += OnCallStopSuggested;
            Log("Call detection initialised", "CallDetect");
        }
        catch (Exception ex)
        {
            Log($"InitCallDetection EXC: {ex.Message}", "CallDetect");
        }
    }

    private void OnCallDetected(string? appId, long sinceSeconds)
    {
        _dispatcherQueue?.TryEnqueue(() =>
        {
            try
            {
                // If user has the master toggle off in the running VM
                // but the Rust state already fired (race between config
                // round-trip and the in-flight tick), swallow silently.
                if (!_appViewModel.CallDetectEnabled) return;

                EnsureCallNudgeWindow();
                Log($"call_detected: app={appId ?? "<none>"} since={sinceSeconds}s", "CallDetect");
                _callNudgeWindow!.ShowFor(appId);
            }
            catch (Exception ex)
            {
                Log($"OnCallDetected EXC: {ex.Message}", "CallDetect");
            }
        });
    }

    private void OnCallEnded(string? appId)
    {
        _dispatcherQueue?.TryEnqueue(() =>
        {
            try
            {
                _callNudgeWindow?.Hide();
            }
            catch (Exception ex)
            {
                Log($"OnCallEnded EXC: {ex.Message}", "CallDetect");
            }
        });
    }

    private void OnNudgeRecord(string? appId)
    {
        try
        {
            DimmyNative.dimmy_call_signal_response(appId, "record_now");
            StartMeetingFromCallDetect();
        }
        catch (Exception ex)
        {
            Log($"OnNudgeRecord EXC: {ex.Message}", "CallDetect");
        }
    }

    private void OnNudgeNotNow(string? appId)
    {
        try { DimmyNative.dimmy_call_signal_response(appId, "not_now"); }
        catch (Exception ex) { Log($"OnNudgeNotNow EXC: {ex.Message}", "CallDetect"); }
    }

    private void OnNudgeNever(string? appId)
    {
        try
        {
            DimmyNative.dimmy_call_signal_response(appId, "never");
            // Rust persisted the exclusion to disk via save_config_file.
            // Refresh the VM's mirror so Settings reflects it immediately.
            _appViewModel.RefreshCallDetectExclusions();
        }
        catch (Exception ex)
        {
            Log($"OnNudgeNever EXC: {ex.Message}", "CallDetect");
        }
    }

    private void OnNudgeTimeout(string? appId)
    {
        try { DimmyNative.dimmy_call_signal_response(appId, "timeout"); }
        catch (Exception ex) { Log($"OnNudgeTimeout EXC: {ex.Message}", "CallDetect"); }
    }

    /// Lazily construct the CallNudgeWindow + wire ALL handlers (both
    /// detection-mode and stop-suggestion-mode). Centralised so the
    /// two entry points (`call_detected` event and `meeting.stop_suggested`
    /// event) can't accidentally diverge in which subset of events they
    /// hook up.
    private void EnsureCallNudgeWindow()
    {
        if (_callNudgeWindow != null) return;
        _callNudgeWindow = new CallNudgeWindow();
        _callNudgeWindow.RecordRequested += OnNudgeRecord;
        _callNudgeWindow.NotNowRequested += OnNudgeNotNow;
        _callNudgeWindow.NeverRequested += OnNudgeNever;
        _callNudgeWindow.TimedOut += OnNudgeTimeout;
        _callNudgeWindow.StopAndRecapRequested += OnNudgeStopAndRecap;
        _callNudgeWindow.KeepRecordingRequested += OnNudgeKeepRecording;
        _callNudgeWindow.StopTimedOut += OnNudgeStopTimeout;
    }

    private void OnCallStopSuggested(string? appId, long inactiveForSecs)
    {
        _dispatcherQueue?.TryEnqueue(() =>
        {
            try
            {
                if (!_appViewModel.CallDetectEnabled) return;
                // Conditions (recording is ours, meeting is active, mic
                // has been silent past the threshold) are enforced by
                // call_detector::handle_inactive — here we just paint.
                EnsureCallNudgeWindow();
                Log($"meeting.stop_suggested: app={appId ?? "<none>"} inactive_for={inactiveForSecs}s", "CallDetect");
                _callNudgeWindow!.ShowStopSuggestion(appId);
            }
            catch (Exception ex)
            {
                Log($"OnCallStopSuggested EXC: {ex.Message}", "CallDetect");
            }
        });
    }

    private void OnNudgeStopAndRecap(string? appId)
    {
        try
        {
            DimmyNative.dimmy_call_signal_response(appId, "stop_and_recap");
            // Reuse the pill's existing stop-meeting + recap pipeline
            // (visual feedback + post-process + sidebar refresh). Pill
            // must be up because the precondition for emitting
            // StopSuggested is "we accepted a call_detected nudge",
            // which always brings the pill into Transcribing-capable
            // state.
            if (_pillWindow is Views.PillWindow pill)
            {
                _ = pill.StopMeetingFromPillAsync();
            }
            else
            {
                Log("OnNudgeStopAndRecap: pill window unavailable — meeting stop skipped", "CallDetect");
            }
        }
        catch (Exception ex)
        {
            Log($"OnNudgeStopAndRecap EXC: {ex.Message}", "CallDetect");
        }
    }

    private void OnNudgeKeepRecording(string? appId)
    {
        try { DimmyNative.dimmy_call_signal_response(appId, "keep_recording"); }
        catch (Exception ex) { Log($"OnNudgeKeepRecording EXC: {ex.Message}", "CallDetect"); }
    }

    private void OnNudgeStopTimeout(string? appId)
    {
        try { DimmyNative.dimmy_call_signal_response(appId, "stop_timeout"); }
        catch (Exception ex) { Log($"OnNudgeStopTimeout EXC: {ex.Message}", "CallDetect"); }
    }

    /// Kick off a meeting recording from the call-detect nudge.
    /// Mirrors MeetingWindow.Start_Click's happy path but doesn't
    /// require the window to be open already — closes the loop
    /// between detection and recording with a single user click.
    private async void StartMeetingFromCallDetect()
    {
        try
        {
            // If a meeting is already active (e.g. user clicked
            // Record on a re-emit), don't start a second one —
            // just surface the existing window.
            if (DimmyNative.dimmy_meeting_is_active() == 1)
            {
                Log("StartMeetingFromCallDetect: meeting already active, opening window", "CallDetect");
                OpenMeetingWindow();
                return;
            }

            // Open the window and route the start THROUGH its own Start flow.
            // The window owns a valid XamlRoot (once loaded) for the mandatory
            // consent modal AND does all the recording UI wiring (state flip,
            // polling, app-context). Starting the meeting in the core directly
            // from here left the just-opened window stuck on idle. Wait for the
            // XamlRoot to go live first (null right after Activate).
            OpenMeetingWindow();
            if (_meetingWindow == null)
            {
                Log("StartMeetingFromCallDetect: no meeting window", "CallDetect");
                return;
            }
            await _meetingWindow.EnsureDialogRootAsync();
            bool started = await _meetingWindow.StartFromCallDetectAsync();
            if (!started)
            {
                Log("StartMeetingFromCallDetect: not started (declined or failed)", "CallDetect");
                return;
            }
            Log("StartMeetingFromCallDetect: meeting started via window", "CallDetect");
            // Bind the originating WASAPI session id so the
            // CallDetectionService meeting-tick can stop the meeting
            // the moment the call ends — no amplitude-based silence
            // heuristic, no false positives during quiet stretches.
            // Best-effort: if no live emitted session matches
            // (race with session ending instantly), the service
            // falls back to the amplitude path.
            var bound = _callDetection?.MarkMeetingOriginFromCurrentSession() ?? false;
            Log($"StartMeetingFromCallDetect: origin_bound={bound}", "CallDetect");
            OpenMeetingWindow();
        }
        catch (Exception ex)
        {
            Log($"StartMeetingFromCallDetect EXC: {ex.Message}", "CallDetect");
        }
    }

    /// the Settings home → Meeting card.
    /// Called by PillWindow.StopMeetingFromPillAsync after the recap
    /// pipeline successfully writes recap.md / actions to disk. If a
    /// MeetingWindow is open, dispatches it to refresh its history
    /// sidebar and auto-select the just-completed meeting so the user
    /// sees the recap cards populated without having to click around.
    /// No-op when no MeetingWindow is open — the artefacts are on
    /// disk and visible next time it's opened.
    public void NotifyMeetingRecapSaved(string dir)
    {
        try
        {
            var w = _meetingWindow;
            if (w == null || string.IsNullOrEmpty(dir)) return;
            w.DispatcherQueue?.TryEnqueue(() =>
            {
                try { w.RefreshAndSelectDir(dir); }
                catch (Exception ex)
                {
                    Log($"NotifyMeetingRecapSaved dispatch exc: {ex.Message}", "Meeting");
                }
            });
        }
        catch (Exception ex)
        {
            Log($"NotifyMeetingRecapSaved exc: {ex.Message}", "Meeting");
        }
    }

    public void OpenMeetingWindow()
    {
        Log("OpenMeetingWindow called", "Meeting");
        try
        {
            if (_meetingWindow == null)
            {
                _meetingWindow = new MeetingWindow();
                _meetingWindow.Closed += (_, __) => _meetingWindow = null;
            }
            _meetingWindow.Activate();
            var hwnd = WindowHelper.GetHwnd(_meetingWindow);
            if (hwnd != IntPtr.Zero)
            {
                // Same topmost-toggle the Settings window uses — the
                // bare SetForegroundWindow loses to Win11's foreground
                // lock when the URL-launched transient process forwards
                // the command via pipe. Restore-if-minimised + promote
                // wins reliably.
                ShowWindow(hwnd, SW_RESTORE);
                SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                SetForegroundWindow(hwnd);
            }
            Log($"OpenMeetingWindow activated + foregrounded, hwnd={hwnd}", "Meeting");
        }
        catch (Exception ex)
        {
            Log($"OpenMeetingWindow EXC: {ex}", "Meeting");
        }
    }

    /// Open Settings and navigate to the named nav tag (e.g. "license").
    /// Used post-activation to surface confirmation without forcing the
    /// user to find the License panel manually.
    public void OpenSettingsWindowAt(string tag)
    {
        OpenSettings();
        _settingsWindow?.NavigateToTag(tag);
        ForegroundSettingsWindow();
    }

    private void OpenSettings()
    {
        // Only allow one settings window at a time
        if (_settingsWindow != null)
        {
            try
            {
                _settingsWindow.Activate();
                // Activate() alone doesn't reliably bring the window to
                // foreground when the calling process (tray icon thread,
                // pipe IPC handler) isn't already the foreground process.
                // The topmost-toggle in ForegroundSettingsWindow is the
                // workaround pattern that does — apply it on every open.
                ForegroundSettingsWindow();
                return;
            }
            catch { _settingsWindow = null; }
        }
        _settingsWindow = new SettingsWindow();
        _settingsWindow.Closed += (_, _) => _settingsWindow = null;
        _settingsWindow.Activate();
        ForegroundSettingsWindow();
    }

    /// <summary>
    /// Force the Settings window to the foreground via Win32 — `Activate()`
    /// alone is unreliable when the calling process isn't the current
    /// foreground process (typical for our case: dimmy:// URL clicked in
    /// browser, browser is foreground, our running instance receives the
    /// command via pipe and tries to surface).
    ///
    /// The trick is the topmost-toggle pattern: briefly mark the window
    /// HWND_TOPMOST then immediately undo to NOTOPMOST. SetForegroundWindow
    /// is paired with the AllowSetForegroundWindow handoff that the
    /// transient (URL-launched) instance issues before forwarding via pipe
    /// — see TryGetActivationPipeCommand path below.
    /// </summary>
    private void ForegroundSettingsWindow()
    {
        if (_settingsWindow is null) return;
        try
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_settingsWindow);
            // Restore if minimised, then promote.
            ShowWindow(hwnd, SW_RESTORE);
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            SetForegroundWindow(hwnd);
        }
        catch (Exception ex)
        {
            PttLog($"[license] ForegroundSettingsWindow failed: {ex.Message}");
        }
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter,
        int X, int Y, int cx, int cy, uint uFlags);

    [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
    private static extern bool AllowSetForegroundWindow(uint dwProcessId);

    private const int SW_RESTORE = 9;
    private static readonly IntPtr HWND_TOPMOST = new(-1);
    private static readonly IntPtr HWND_NOTOPMOST = new(-2);
    private const uint SWP_NOMOVE = 0x0002;
    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_NOACTIVATE = 0x0010;

    private void CheckAudioHealth()
    {
        try
        {
            var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_check_audio_health, 4096);
            if (json == null) return;

            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var r = doc.RootElement;

            bool hasDevices = r.TryGetProperty("has_devices", out var hd) && hd.GetBoolean();
            bool canOpen = r.TryGetProperty("can_open_stream", out var co) && co.GetBoolean();
            bool selectedAvailable = r.TryGetProperty("selected_available", out var sa) && sa.GetBoolean();
            string? error = r.TryGetProperty("error", out var err) && err.ValueKind != System.Text.Json.JsonValueKind.Null
                ? err.GetString() : null;

            if (!hasDevices)
            {
                _appViewModel.SetError("No microphone found");
            }
            else if (!canOpen)
            {
                var msg = error ?? "Microphone unavailable";
                _appViewModel.SetError(msg);
                System.Diagnostics.Debug.WriteLine($"[AudioHealth] WARNING: {msg}");
            }
            else if (!selectedAvailable)
            {
                // Selected device gone — will fall back to default, just log
                System.Diagnostics.Debug.WriteLine("[AudioHealth] Selected device not found, will use default");
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[AudioHealth] Check failed: {ex.Message}");
        }
    }

    public async void QuitApp() => await QuitAsync();

    /// <summary>Sync facade for callers that can't await (tray menu
    /// callback delegate, jump-list command pipe). Fire-and-forget —
    /// the UI thread continues normally while the async path tears
    /// services down. Equivalent to QuitApp(); the rename keeps the
    /// public surface intuitive for external callers.</summary>
    private void Quit() => _ = QuitAsync();

    private async Task QuitAsync()
    {
        // Auto-update prompt at quit — only if an update was already
        // downloaded in the background AND we still have a XamlRoot to
        // host the dialog on. The "Mixed" UX picked 2026-05-11: silent
        // download + ask before apply. ApplyAndRestart never returns
        // (Velopack swaps the EXE and re-spawns); ApplyOnExit stages
        // the swap so the next normal launch picks up the new version.
        if (UpdateService.Instance?.IsUpdateReady == true)
        {
            var apply = await AskApplyUpdateAsync();
            if (apply == true)
            {
                UpdateService.Instance.ApplyAndRestart();
                return; // never reached — Velopack exits the process
            }
            // Skip / no-dialog-possible: still stage so next launch picks up.
            UpdateService.Instance.ApplyOnExit();
            Interop.DimmyNative.TrackEvent("update.apply_deferred");
        }

        _hotkeyService?.Dispose();
        _dictHotkey?.Dispose();
        _trayService?.Dispose();
        _commandPipe?.Dispose();
        _appViewModel.PropertyChanged -= OnAppViewModelPropertyChangedForTaskbar;
        _taskbarService?.Dispose();
        _callDetection?.Dispose();
        try { _callNudgeWindow?.Close(); } catch { }
        try { _taskbarAnchor?.Close(); } catch { }

        // Cancel any active recording before shutdown to release microphone
        try
        {
            if (_appViewModel.IsRecording || _pttStarted)
            {
                DimmyNative.dimmy_cancel_recording();
                _pttStarted = false;
            }
        }
        catch { }

        DimmyNative.dimmy_shutdown();

        _singleInstanceMutex?.ReleaseMutex();
        _singleInstanceMutex?.Dispose();
        Exit();
    }

    /// <summary>Ask the user "Apply update now or skip?" via a
    /// ContentDialog anchored on whatever XamlRoot is currently
    /// available (pill window or taskbar anchor). Returns true for
    /// Apply, false for Skip, null when no XamlRoot is available
    /// (quit-from-tray with no visible window) — in which case
    /// callers should stage the update for next launch silently.</summary>
    private async Task<bool?> AskApplyUpdateAsync()
    {
        var anchor = (FrameworkElement?)_pillWindow?.Content
                  ?? (FrameworkElement?)_taskbarAnchor?.Content;
        if (anchor?.XamlRoot is null) return null;
        try
        {
            var version = UpdateService.Instance?.PendingVersion ?? "";
            var dlg = new Microsoft.UI.Xaml.Controls.ContentDialog
            {
                RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
                Title = "Update ready",
                Content = string.IsNullOrEmpty(version)
                    ? "A Dimmy update is downloaded.\nApply now? Dimmy will close and reopen on the new version."
                    : $"Dimmy v{version} is downloaded.\nApply now? Dimmy will close and reopen on the new version.",
                PrimaryButtonText = "Apply & restart",
                CloseButtonText = "Skip this time",
                DefaultButton = Microsoft.UI.Xaml.Controls.ContentDialogButton.Primary,
                XamlRoot = anchor.XamlRoot,
            };
            var result = await dlg.ShowAsync();
            return result == Microsoft.UI.Xaml.Controls.ContentDialogResult.Primary;
        }
        catch (Exception ex)
        {
            Log($"AskApplyUpdate dialog failed: {ex.Message}", "Update");
            return null;
        }
    }

    private static bool IsOnboardingComplete()
    {
        var marker = Path.Combine(Services.BuildInfo.ConfigDirPath, ".onboarding_done");
        return File.Exists(marker);
    }

    public static void MarkOnboardingComplete()
    {
        var dimmyDir = Services.BuildInfo.ConfigDirPath;
        Directory.CreateDirectory(dimmyDir);
        File.WriteAllText(Path.Combine(dimmyDir, ".onboarding_done"), "1");
    }
}
