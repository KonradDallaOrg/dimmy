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

    private static readonly string PttLogPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "dimmy", "ptt.log");

    private static void PttLog(string msg) => Log(msg, "PTT");

    /// Public diagnostic logger callable from any window for ad-hoc
    /// debugging. Routes to the same ptt.log so output is one stream.
    public static void Log(string msg, string tag = "Dimmy")
    {
        var line = $"[{DateTime.Now:HH:mm:ss.fff}] [{tag}] {msg}";
        Console.WriteLine(line);
        Console.Out.Flush();
        try { File.AppendAllText(PttLogPath, line + Environment.NewLine); } catch { }
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
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // Set the AppUserModelID FIRST. Windows derives taskbar
        // grouping + jump-list ownership from the AUMI of the process
        // at the moment a window first registers in the taskbar; if
        // we set it later, the jump list we register doesn't bind to
        // our taskbar entry and right-click shows nothing custom.
        JumpListService.SetProcessAumi();

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
        var pipeCommand = TryGetPipeCommandFromArgs();
        if (pipeCommand is not null)
        {
            CommandPipeServer.TrySendCommand(pipeCommand);
            Environment.Exit(0);
            return;
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

            // 2b. Caption window — chunked transcriber emits stt_chunk
            // events from the Rust core; we route them through the
            // shared AppViewModel.SttChunkReceived hook.
            _appViewModel.SttChunkReceived += OnSttChunkReceived;

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
        _hotkeyService.PttMode = _appViewModel.ShortcutMode == "hold";
        _hotkeyService.Register(_appViewModel.Shortcut);
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

            // Wire WinUI 3 context menu from pill window
            var pill = _pillWindow as Views.PillWindow;
            _trayService.SetMenuCallback(() =>
                _dispatcherQueue?.TryEnqueue(() => pill?.ShowContextMenu()));
        }

        InitTaskbarAnchor();
        InitCommandPipeAndJumpList();

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
        if (e.PropertyName != nameof(AppViewModel.CurrentState)) return;
        var state = _appViewModel.CurrentState;
        _dispatcherQueue?.TryEnqueue(() => _taskbarService?.UpdateState(state));
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
            && e.PropertyName != nameof(AppViewModel.PillShowOnStartup))
            return;
        _uiPrefs.PillShowOnHotkey = _appViewModel.PillShowOnHotkey;
        _uiPrefs.PillShowOnStartup = _appViewModel.PillShowOnStartup;
        _uiPrefs.Save();
    }

    private void OnboardingWindow_Closed(object sender, WindowEventArgs args)
    {
        // Marker is written explicitly once the user commits to a model choice
        // (see OnboardingWindow.PersistModelChoice). Closing without choosing
        // leaves the marker absent → onboarding shows again next launch.
        StartNormalMode();
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
            _captionWindow.Activate();
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
            var configDir = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
            var path = Path.Combine(configDir, "dimmy", "config.json");
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

                if (_appViewModel.IsRecording && !_stopInProgress)
                    await StopAndProcess();
                else if (!_appViewModel.IsBusy && !_stopInProgress)
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

    private async Task StopAndProcess()
    {
        if (_stopInProgress)
        {
            PttLog("StopAndProcess: already in progress, ignoring");
            return;
        }
        _stopInProgress = true;
        try
        {
            PttLog("StopAndProcess: calling dimmy_stop_recording...");
            var result = await Services.TranscriptionService.StopAndProcessAsync();
            PttLog($"StopAndProcess: IsSuccess={result.IsSuccess}, IsEmpty={result.IsEmpty}, IsTimeout={result.IsTimeout}, Text={result.Text?.Length ?? 0} chars, Error={result.Error}");
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
                    PttLog($"PASTE pre: FOCUS DRIFT — target was 0x{target.Hwnd.ToInt64():X} '{target.WindowTitle}', now 0x{prePaste.Hwnd.ToInt64():X} '{prePaste.WindowTitle}' (proc='{prePaste.ProcessName}')");
                else
                    PttLog($"PASTE pre: foreground unchanged ({prePaste.ToLogString()})");

                PttLog($"StopAndProcess: pasting text ({result.Text!.Length} chars)");
                await TextInjectionService.PasteText(result.Text!, _appViewModel.KeepInClipboard);
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

    public void QuitApp() => Quit();

    private void Quit()
    {
        _hotkeyService?.Dispose();
        _trayService?.Dispose();
        _commandPipe?.Dispose();
        _appViewModel.PropertyChanged -= OnAppViewModelPropertyChangedForTaskbar;
        _taskbarService?.Dispose();
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

    private static bool IsOnboardingComplete()
    {
        var configDir = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        var marker = Path.Combine(configDir, "dimmy", ".onboarding_done");
        return File.Exists(marker);
    }

    public static void MarkOnboardingComplete()
    {
        var configDir = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        var dimmyDir = Path.Combine(configDir, "dimmy");
        Directory.CreateDirectory(dimmyDir);
        File.WriteAllText(Path.Combine(dimmyDir, ".onboarding_done"), "1");
    }
}
