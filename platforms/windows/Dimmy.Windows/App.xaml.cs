using System;
using System.IO;
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
    private PillWindow? _pillWindow;
    private OnboardingWindow? _onboardingWindow;
    private HotkeyService? _hotkeyService;
    private TrayService? _trayService;
    private DispatcherQueue? _dispatcherQueue;

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

    private static void PttLog(string msg)
    {
        var line = $"[{DateTime.Now:HH:mm:ss.fff}] [PTT] {msg}";
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
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // Single-instance guard: exit immediately if another Dimmy is already running
        _singleInstanceMutex = new Mutex(true, @"Global\DimmySingleInstance", out bool createdNew);
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

            // 3. Load config into ViewModel
            LoadConfigIntoViewModel();



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
            onQuitClick: Quit);

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
    }

    private void OnboardingWindow_Closed(object sender, WindowEventArgs args)
    {
        MarkOnboardingComplete();
        StartNormalMode();
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
            PttLog($"LoadConfig: shortcut={_appViewModel.Shortcut}, mode={_appViewModel.ShortcutMode}");
        }
        catch (Exception ex) { PttLog($"LoadConfig: parse error: {ex.Message}"); }
    }

    private void OnHotkeyPressed()
    {
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            // Show pill if hidden — hotkey should always bring it back
            if (!IsPillVisible())
                ShowPill();

            if (_appViewModel.ShortcutMode == "hold")
            {
                // PTT: press starts recording
                if (!_appViewModel.IsBusy && !_pttStarted)
                {
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
                    _appViewModel.SuppressRecordingStarted = false; // ensure Rust event is accepted
                    var result = DimmyNative.dimmy_start_recording();
                    if (result == -1)
                        _appViewModel.SetError("No API key configured");
                    else if (result < 0)
                        _appViewModel.SetError($"Recording failed ({result})");
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
                PttLog($"StopAndProcess: pasting text ({result.Text!.Length} chars)");
                await TextInjectionService.PasteText(result.Text!, _appViewModel.KeepInClipboard);
                // Show completing state (checkmark) AFTER paste — PillWindow timer returns to Idle
                _appViewModel.SetState(AppState.Completing);
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
        _pillWindow.Activate();
        _trayService?.UpdateState("Dimmy — Ready", "");
    }

    private bool IsPillVisible()
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

    private void OpenSettings()
    {
        // Only allow one settings window at a time
        if (_settingsWindow != null)
        {
            try { _settingsWindow.Activate(); return; }
            catch { _settingsWindow = null; }
        }
        _settingsWindow = new SettingsWindow();
        _settingsWindow.Closed += (_, _) => _settingsWindow = null;
        _settingsWindow.Activate();
    }

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

    private static void MarkOnboardingComplete()
    {
        var configDir = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        var dimmyDir = Path.Combine(configDir, "dimmy");
        Directory.CreateDirectory(dimmyDir);
        File.WriteAllText(Path.Combine(dimmyDir, ".onboarding_done"), "1");
    }
}
