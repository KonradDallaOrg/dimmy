using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
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

    private AppViewModel _appViewModel = new();
    private PillWindow? _pillWindow;
    private OnboardingWindow? _onboardingWindow;
    private HotkeyService? _hotkeyService;
    private TrayService? _trayService;
    private DispatcherQueue? _dispatcherQueue;

    // Must be stored as a field to prevent GC collection of the delegate
    private DimmyNative.EventCallback? _eventCallbackDelegate;

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
        _appViewModel.Language = settings.Language;
        _appViewModel.LlmStyle = settings.LlmStyle;
        _appViewModel.ShortcutMode = settings.ShortcutMode;
        _appViewModel.KeepInClipboard = settings.KeepInClipboard;

        if (_appViewModel.ShowInTaskbar != settings.ShowInTaskbar)
        {
            _appViewModel.ShowInTaskbar = settings.ShowInTaskbar;
            ApplyTaskbarVisibility();
        }

        if (_appViewModel.OverlayPosition != settings.OverlayPosition)
        {
            _appViewModel.OverlayPosition = settings.OverlayPosition;
            RepositionPill();
        }
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
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

    /// <summary>Show pill + register hotkey. Safe to call multiple times.</summary>
    public void ShowPillAndHotkey()
    {
        if (_pillWindow != null) return; // already shown

        _pillWindow = new PillWindow(_appViewModel);
        _pillWindow.Activate();

        _hotkeyService = new HotkeyService();
        _hotkeyService.HotkeyPressed += OnHotkeyPressed;
        var hwnd = WindowHelper.GetHwnd(_pillWindow);
        _hotkeyService.Register(hwnd, _appViewModel.Shortcut);
    }

    private void StartNormalMode()
    {
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
            if (File.Exists(path))
                json = File.ReadAllText(path);
        }
        catch { }

        // Fallback to FFI if file not found
        if (string.IsNullOrEmpty(json))
        {
            try { json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384); }
            catch { }
        }

        if (json == null) return;

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
            if (r.TryGetProperty("show_in_taskbar", out var sit))
                _appViewModel.ShowInTaskbar = sit.GetBoolean();
        }
        catch { }
    }

    private void OnHotkeyPressed()
    {
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            // Show pill if hidden — hotkey should always bring it back
            if (!IsPillVisible())
                ShowPill();

            if (_appViewModel.IsRecording)
            {
                // Stop recording → transcribe → LLM enhance → paste
                try
                {
                    var result = await Services.TranscriptionService.StopAndProcessAsync();
                    if (result.IsSuccess)
                    {
                        await TextInjectionService.PasteText(result.Text!, _appViewModel.KeepInClipboard);
                    }
                    else if (result.IsTimeout)
                    {
                        _appViewModel.SetError(result.Error!);
                    }
                    else if (_appViewModel.CurrentState == AppState.Transcribing)
                    {
                        _appViewModel.SetError("Empty transcription");
                    }
                }
                catch (Exception ex)
                {
                    _appViewModel.SetError(ex.Message);
                }
            }
            else
            {
                var result = DimmyNative.dimmy_start_recording();
                if (result == -1)
                    _appViewModel.SetError("No API key configured");
                else if (result < 0)
                    _appViewModel.SetError($"Recording failed ({result})");
            }
        });
    }

    private void ApplyTaskbarVisibility()
    {
        if (_pillWindow == null) return;
        var hwnd = WindowHelper.GetHwnd(_pillWindow);
        WindowHelper.SetTaskbarVisibility(hwnd, _appViewModel.ShowInTaskbar);
    }

    public void RepositionPill()
    {
        if (_pillWindow == null) return;
        WindowHelper.PositionByPreset(_pillWindow, _appViewModel.OverlayPosition, 240, 60);
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

    private void TogglePill()
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

    public void QuitApp() => Quit();

    private void Quit()
    {
        _hotkeyService?.Dispose();
        _trayService?.Dispose();
        DimmyNative.dimmy_shutdown();
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
