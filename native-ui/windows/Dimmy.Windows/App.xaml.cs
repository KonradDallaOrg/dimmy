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

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _dispatcherQueue = DispatcherQueue.GetForCurrentThread();

        // 1. Initialize Rust core
        int result = DimmyNative.dimmy_init();
        if (result != 0)
        {
            System.Diagnostics.Debug.WriteLine("FATAL: dimmy_init() returned " + result);
            Exit();
            return;
        }

        // 2. Register event callback
        _eventCallbackDelegate = OnNativeEvent;
        DimmyNative.dimmy_set_event_callback(_eventCallbackDelegate);

        // 3. Load config into ViewModel
        LoadConfigIntoViewModel();

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

    private void StartNormalMode()
    {
        _pillWindow = new PillWindow(_appViewModel);
        _pillWindow.Activate();

        _trayService = new TrayService(
            vm: _appViewModel,
            onTogglePill: TogglePill,
            onSettingsClick: OpenSettings,
            onQuitClick: Quit);

        _hotkeyService = new HotkeyService();
        _hotkeyService.HotkeyPressed += OnHotkeyPressed;
        var hwnd = WindowHelper.GetHwnd(_pillWindow);
        _hotkeyService.Register(hwnd, _appViewModel.Shortcut);
    }

    private void OnboardingWindow_Closed(object sender, WindowEventArgs args)
    {
        MarkOnboardingComplete();
        StartNormalMode();
    }

    private void OnNativeEvent(IntPtr jsonPtr)
    {
        var json = DimmyNative.MarshalEventJson(jsonPtr);
        _dispatcherQueue?.TryEnqueue(() => _appViewModel.HandleEvent(json));
    }

    private void LoadConfigIntoViewModel()
    {
        var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384);
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
        }
        catch { }
    }

    private void OnHotkeyPressed()
    {
        _dispatcherQueue?.TryEnqueue(async () =>
        {
            if (_appViewModel.IsRecording)
            {
                // Stop recording + transcribe on background thread
                var text = await Task.Run(() =>
                {
                    var buf = new byte[65536];
                    int len = DimmyNative.dimmy_stop_recording(buf, buf.Length);
                    return len > 0 ? Encoding.UTF8.GetString(buf, 0, len) : null;
                });
                if (!string.IsNullOrEmpty(text))
                {
                    await TextInjectionService.PasteText(text);
                }
            }
            else
            {
                DimmyNative.dimmy_start_recording();
            }
        });
    }

    private void TogglePill()
    {
        if (_pillWindow == null) return;
        var appWindow = WindowHelper.GetAppWindow(_pillWindow);
        if (appWindow.IsVisible) appWindow.Hide();
        else _pillWindow.Activate();
    }

    private void OpenSettings()
    {
        var settings = new SettingsWindow();
        settings.Activate();
    }

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
        var config = Path.Combine(configDir, "dimmy", "config.json");
        return File.Exists(marker) || File.Exists(config);
    }

    private static void MarkOnboardingComplete()
    {
        var configDir = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        var dimmyDir = Path.Combine(configDir, "dimmy");
        Directory.CreateDirectory(dimmyDir);
        File.WriteAllText(Path.Combine(dimmyDir, ".onboarding_done"), "1");
    }
}
