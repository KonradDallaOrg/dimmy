using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Views;

public sealed partial class PillWindow : Window
{
    private readonly AppViewModel _vm;
    private DispatcherTimer? _amplitudeTimer;
    private DispatcherTimer? _recordingTimer;
    private DateTime _recordingStartTime;
    private DispatcherTimer? _completingTimer;
    private DispatcherTimer? _errorTimer;
    private DispatcherTimer? _rainbowTimer;
    private LinearGradientBrush? _rainbowBrush;
    private DateTime _rainbowStartTime;

    private bool _amplitudeHandlerAttached;
    private bool _recordingHandlerAttached;
    private bool _completingHandlerAttached;
    private bool _errorHandlerAttached;

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT lpPoint);
    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }
    private bool _isDragging;
    private POINT _dragStartScreen;
    private global::Windows.Graphics.PointInt32 _windowStartPos;

    // Circle size for idle/completing states (Width = Height = perfect circle)
    private const double CircleSize = 36;
    // Capsule height for recording/transcribing states
    private const double CapsuleHeight = 40;

    private const int WindowWidth = 240;
    private const int WindowHeight = 60;

    public PillWindow(AppViewModel vm)
    {
        _vm = vm;
        this.InitializeComponent();
        Title = "Dimmy";
        BuildRainbowBrush();
        SetupWindow();
        _vm.PropertyChanged += Vm_PropertyChanged;
        Waveform.StyleMode = _vm.WaveformStyle;
        UpdateVisualState();
    }

    private void SetupWindow()
    {
        ExtendsContentIntoTitleBar = true;

        // Set up transparent backdrop with the window's HWND
        var backdrop = new Helpers.TransparentBackdrop();
        backdrop.Hwnd = WindowHelper.GetHwnd(this);
        this.SystemBackdrop = backdrop;

        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Resize(new global::Windows.Graphics.SizeInt32(WindowWidth, WindowHeight));

        if (appWindow?.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsResizable = false;
            presenter.SetBorderAndTitleBar(false, false);
            presenter.IsAlwaysOnTop = true;
        }

        WindowHelper.EnableTransparency(this);
        WindowHelper.PositionBottomRight(this, WindowWidth, WindowHeight);
    }

    private void Vm_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(AppViewModel.CurrentState) || e.PropertyName == nameof(AppViewModel.BorderStyle))
            DispatcherQueue.TryEnqueue(UpdateVisualState);
        if (e.PropertyName == nameof(AppViewModel.LlmStyle))
            DispatcherQueue.TryEnqueue(() => StyleDot.Fill = new SolidColorBrush(ParseColor(_vm.LlmStyleColor)));
        if (e.PropertyName == nameof(AppViewModel.WaveformStyle))
            DispatcherQueue.TryEnqueue(() => Waveform.StyleMode = _vm.WaveformStyle);
    }

    // ── Shape helpers ───────────────────────────────────────────────
    private void SetCircleShape()
    {
        // Force Width = Height → perfect circle
        ColorBorder.Width = CircleSize;
        ColorBorder.Height = CircleSize;
        // CornerRadius = half the size → perfect round
        var r = CircleSize / 2;
        ColorBorder.CornerRadius = new CornerRadius(r);
        PillInner.CornerRadius = new CornerRadius(r - 2); // minus border padding
    }

    private void SetCapsuleShape()
    {
        // Fixed height, auto width → capsule/stadium
        ColorBorder.Width = double.NaN; // auto
        ColorBorder.Height = CapsuleHeight;
        // CornerRadius = exactly half the height → perfect stadium ends
        var r = CapsuleHeight / 2;
        ColorBorder.CornerRadius = new CornerRadius(r);
        PillInner.CornerRadius = new CornerRadius(r - 2);
    }

    // ── State colors ────────────────────────────────────────────────
    private static readonly global::Windows.UI.Color ColorTranscribing =
        global::Windows.UI.Color.FromArgb(255, 56, 189, 248);
    private static readonly global::Windows.UI.Color ColorProcessing =
        global::Windows.UI.Color.FromArgb(255, 168, 139, 250);
    private static readonly global::Windows.UI.Color ColorCompleting =
        global::Windows.UI.Color.FromArgb(255, 74, 222, 128);
    private static readonly global::Windows.UI.Color ColorError =
        global::Windows.UI.Color.FromArgb(255, 239, 68, 68);

    // Border style solid colors
    private static readonly global::Windows.UI.Color BorderBlue =
        global::Windows.UI.Color.FromArgb(255, 56, 189, 248);
    private static readonly global::Windows.UI.Color BorderGreen =
        global::Windows.UI.Color.FromArgb(255, 74, 222, 128);
    private static readonly global::Windows.UI.Color BorderPurple =
        global::Windows.UI.Color.FromArgb(255, 168, 139, 250);
    private static readonly global::Windows.UI.Color BorderOrange =
        global::Windows.UI.Color.FromArgb(255, 251, 146, 39);

    private Brush GetIdleBorderBrush()
    {
        return _vm.BorderStyle switch
        {
            "Blue" => new SolidColorBrush(BorderBlue),
            "Green" => new SolidColorBrush(BorderGreen),
            "Purple" => new SolidColorBrush(BorderPurple),
            "Orange" => new SolidColorBrush(BorderOrange),
            "None" => new SolidColorBrush(global::Windows.UI.Color.FromArgb(255, 60, 60, 60)),
            _ => _rainbowBrush!, // Rainbow default
        };
    }

    private void UpdateVisualState()
    {
        // Hide all panels
        IdlePanel.Visibility = Visibility.Collapsed;
        RecordingPanel.Visibility = Visibility.Collapsed;
        TranscribingPanel.Visibility = Visibility.Collapsed;
        ProcessingPanel.Visibility = Visibility.Collapsed;
        CompletingPanel.Visibility = Visibility.Collapsed;
        ErrorPanel.Visibility = Visibility.Collapsed;

        _amplitudeTimer?.Stop();
        _recordingTimer?.Stop();

        LanguageLabel.Visibility = Visibility.Collapsed;
        ShortcutLabel.Visibility = Visibility.Collapsed;
        GearButton.Visibility = Visibility.Collapsed;

        switch (_vm.CurrentState)
        {
            case AppState.Idle:
                IdlePanel.Visibility = Visibility.Visible;
                StyleDot.Fill = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
                RootGrid.Opacity = 1.0;
                SetCircleShape();
                ColorBorder.Background = GetIdleBorderBrush();
                if (_vm.BorderStyle == "Rainbow") StartRainbowAnimation();
                else _rainbowTimer?.Stop();
                break;

            case AppState.Recording:
                RecordingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                StopButton.Visibility = _vm.ShortcutMode == "toggle"
                    ? Visibility.Visible : Visibility.Collapsed;
                SetCapsuleShape();
                ColorBorder.Background = GetIdleBorderBrush();
                StartAmplitudePolling();
                _recordingStartTime = DateTime.Now;
                StartRecordingTimer();
                Waveform.IsActive = true;
                if (_vm.BorderStyle == "Rainbow") StartRainbowAnimation();
                else _rainbowTimer?.Stop();
                break;

            case AppState.Transcribing:
                TranscribingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                Waveform.IsActive = false;
                ChunkText.Text = _vm.ChunkTotal > 1 ? $"{_vm.ChunkCurrent}/{_vm.ChunkTotal}" : "";
                SetCapsuleShape();
                _rainbowTimer?.Stop();
                ColorBorder.Background = new SolidColorBrush(ColorTranscribing);
                break;

            case AppState.Processing:
                ProcessingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                SetCapsuleShape();
                _rainbowTimer?.Stop();
                // Use LLM style color so user sees which enhancement is active
                ColorBorder.Background = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
                break;

            case AppState.Completing:
                CompletingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                SetCircleShape();
                _rainbowTimer?.Stop();
                // Green checkmark, but border shows LLM style color if enhancement was used
                ColorBorder.Background = _vm.LlmStyle != "off"
                    ? new SolidColorBrush(ParseColor(_vm.LlmStyleColor))
                    : new SolidColorBrush(ColorCompleting);
                if (_completingTimer is null)
                    _completingTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1200) };
                if (!_completingHandlerAttached)
                {
                    _completingTimer.Tick += (_, _) => { _completingTimer.Stop(); _vm.SetState(AppState.Idle); };
                    _completingHandlerAttached = true;
                }
                _completingTimer.Start();
                break;

            case AppState.Error:
                ErrorPanel.Visibility = Visibility.Visible;
                ErrorText.Text = _vm.ErrorMessage;
                RootGrid.Opacity = 1.0;
                SetCapsuleShape();
                _rainbowTimer?.Stop();
                ColorBorder.Background = new SolidColorBrush(ColorError);
                if (_errorTimer is null)
                    _errorTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(3) };
                if (!_errorHandlerAttached)
                {
                    _errorTimer.Tick += (_, _) => { _errorTimer.Stop(); _vm.SetState(AppState.Idle); };
                    _errorHandlerAttached = true;
                }
                _errorTimer.Start();
                break;
        }
    }

    // ── Timers ──────────────────────────────────────────────────────
    private void StartAmplitudePolling()
    {
        if (_amplitudeTimer is null)
            _amplitudeTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 12) };
        if (!_amplitudeHandlerAttached)
        {
            _amplitudeTimer.Tick += (_, _) => Waveform.Amplitude = DimmyNative.dimmy_get_amplitude();
            _amplitudeHandlerAttached = true;
        }
        _amplitudeTimer.Start();
    }

    private void StartRecordingTimer()
    {
        if (_recordingTimer is null)
            _recordingTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        if (!_recordingHandlerAttached)
        {
            _recordingTimer.Tick += (_, _) =>
            {
                var elapsed = DateTime.Now - _recordingStartTime;
                TimerText.Text = $"{(int)elapsed.TotalMinutes:D2}:{elapsed.Seconds:D2}";
            };
            _recordingHandlerAttached = true;
        }
        _recordingTimer.Start();
    }

    // ── Rainbow ─────────────────────────────────────────────────────
    private static readonly (double Offset, string Hex)[] RainbowStops =
    [
        (0.000, "#FF4D4D"), (0.125, "#FF6633"), (0.250, "#FFB84D"),
        (0.375, "#49F249"), (0.500, "#66E0FF"), (0.625, "#4D7AFF"),
        (0.750, "#9966FF"), (0.875, "#E066FF"), (1.000, "#FF4D8C"),
    ];

    private void BuildRainbowBrush()
    {
        _rainbowBrush = new LinearGradientBrush();
        foreach (var (offset, hex) in RainbowStops)
            _rainbowBrush.GradientStops.Add(new GradientStop { Offset = offset, Color = ParseColor(hex) });
    }

    private void StartRainbowAnimation()
    {
        _rainbowStartTime = DateTime.UtcNow;
        if (_rainbowTimer is null)
        {
            _rainbowTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 30) };
            _rainbowTimer.Tick += (_, _) =>
            {
                if (_rainbowBrush is null) return;
                var elapsed = (DateTime.UtcNow - _rainbowStartTime).TotalSeconds;
                var angleRad = (elapsed * 144.0 % 360.0) * Math.PI / 180.0;
                var cos = Math.Cos(angleRad);
                var sin = Math.Sin(angleRad);
                var scale = 0.5 / Math.Max(Math.Abs(cos), Math.Abs(sin));
                _rainbowBrush.StartPoint = new global::Windows.Foundation.Point(0.5 - cos * scale, 0.5 - sin * scale);
                _rainbowBrush.EndPoint = new global::Windows.Foundation.Point(0.5 + cos * scale, 0.5 + sin * scale);
            };
        }
        _rainbowTimer.Start();
    }

    private static global::Windows.UI.Color ParseColor(string hex)
    {
        hex = hex.TrimStart('#');
        byte a = 255, r, g, b;
        if (hex.Length == 8)
        {
            a = Convert.ToByte(hex[0..2], 16); r = Convert.ToByte(hex[2..4], 16);
            g = Convert.ToByte(hex[4..6], 16); b = Convert.ToByte(hex[6..8], 16);
        }
        else
        {
            r = Convert.ToByte(hex[0..2], 16); g = Convert.ToByte(hex[2..4], 16);
            b = Convert.ToByte(hex[4..6], 16);
        }
        return global::Windows.UI.Color.FromArgb(a, r, g, b);
    }

    // ── Hover ───────────────────────────────────────────────────────
    private void Pill_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        if (_vm.CurrentState == AppState.Idle)
        {
            RootGrid.Opacity = 0.95;
            // Expand to capsule to show info
            SetCapsuleShape();
            LanguageLabel.Text = string.IsNullOrEmpty(_vm.Language) ? "" : _vm.Language.ToUpperInvariant();
            ShortcutLabel.Text = _vm.Shortcut;
            if (!string.IsNullOrEmpty(LanguageLabel.Text))
                LanguageLabel.Visibility = Visibility.Visible;
            ShortcutLabel.Visibility = Visibility.Visible;
            GearButton.Visibility = Visibility.Visible;

            IdleContent.Margin = new Thickness(14, 0, 14, 0);
        }
    }

    private void Pill_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (_vm.CurrentState == AppState.Idle)
        {
            RootGrid.Opacity = 1.0;
            // Shrink back to circle
            SetCircleShape();
            LanguageLabel.Visibility = Visibility.Collapsed;
            ShortcutLabel.Visibility = Visibility.Collapsed;
            GearButton.Visibility = Visibility.Collapsed;

            IdleContent.Margin = new Thickness(0);
        }
    }

    // ── Drag ────────────────────────────────────────────────────────
    private void Pill_PointerPressed(object sender, PointerRoutedEventArgs e)
    {
        _isDragging = true;
        GetCursorPos(out _dragStartScreen);
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow != null) _windowStartPos = appWindow.Position;
        ((UIElement)sender).CapturePointer(e.Pointer);
    }

    private void Pill_PointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_isDragging) return;
        GetCursorPos(out var current);
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Move(new global::Windows.Graphics.PointInt32(
            _windowStartPos.X + current.X - _dragStartScreen.X,
            _windowStartPos.Y + current.Y - _dragStartScreen.Y));
    }

    private void Pill_PointerReleased(object sender, PointerRoutedEventArgs e)
    {
        _isDragging = false;
        ((UIElement)sender).ReleasePointerCapture(e.Pointer);
    }

    // ── Scroll to cycle settings ────────────────────────────────────
    private static readonly string[] LlmStyles = ViewModels.SettingsViewModel.LlmStyles;
    private static readonly System.Collections.Generic.List<System.Collections.Generic.KeyValuePair<string, string>> LangList = ViewModels.SettingsViewModel.Languages;

    private void StyleDot_PointerWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        var delta = e.GetCurrentPoint(null).Properties.MouseWheelDelta;
        if (delta == 0) return;
        int idx = Array.IndexOf(LlmStyles, _vm.LlmStyle);
        if (idx < 0) idx = 0;
        idx = (idx + (delta > 0 ? -1 : 1) + LlmStyles.Length) % LlmStyles.Length;
        _vm.LlmStyle = LlmStyles[idx];
        StyleDot.Fill = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
        SaveFieldToConfig("llm_style", _vm.LlmStyle);
        SaveFieldToConfig("llm_enabled", _vm.LlmStyle != "off" ? "true" : "false");
        var dict = new System.Collections.Generic.Dictionary<string, object>
        {
            ["llm_style"] = _vm.LlmStyle,
            ["llm_enabled"] = _vm.LlmStyle != "off"
        };
        DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(dict));
        e.Handled = true;
    }

    private void LanguageLabel_PointerWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        var delta = e.GetCurrentPoint(null).Properties.MouseWheelDelta;
        if (delta == 0) return;
        int idx = LangList.FindIndex(kv => kv.Key == _vm.Language);
        if (idx < 0) idx = 0;
        idx = (idx + (delta > 0 ? -1 : 1) + LangList.Count) % LangList.Count;
        _vm.Language = LangList[idx].Key;
        LanguageLabel.Text = _vm.Language.ToUpperInvariant();
        SaveFieldToConfig("language", _vm.Language);
        DimmyNative.dimmy_set_config_json(
            System.Text.Json.JsonSerializer.Serialize(
                new System.Collections.Generic.Dictionary<string, string> { ["language"] = _vm.Language }));
        e.Handled = true;
    }

    /// <summary>Save a single field to config.json (UI-side merge).</summary>
    private static void SaveFieldToConfig(string key, object value)
    {
        try
        {
            var configDir = System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData);
            var path = System.IO.Path.Combine(configDir, "dimmy", "config.json");
            var dict = new System.Collections.Generic.Dictionary<string, object?>();
            if (System.IO.File.Exists(path))
            {
                using var doc = System.Text.Json.JsonDocument.Parse(System.IO.File.ReadAllText(path));
                foreach (var prop in doc.RootElement.EnumerateObject())
                {
                    dict[prop.Name] = prop.Value.ValueKind switch
                    {
                        System.Text.Json.JsonValueKind.String => prop.Value.GetString(),
                        System.Text.Json.JsonValueKind.Number => prop.Value.GetDouble(),
                        System.Text.Json.JsonValueKind.True => true,
                        System.Text.Json.JsonValueKind.False => false,
                        _ => null
                    };
                }
            }
            dict[key] = value;
            var json = System.Text.Json.JsonSerializer.Serialize(dict,
                new System.Text.Json.JsonSerializerOptions { WriteIndented = true });
            System.IO.File.WriteAllText(path, json);
        }
        catch { }
    }

    // ── Actions ─────────────────────────────────────────────────────
    private void Settings_Click(object sender, RoutedEventArgs e)
    {
        App.Instance?.OpenSettingsWindow();
    }

    private void Pill_RightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        var menu = new MenuFlyout();

        var settingsItem = new Microsoft.UI.Xaml.Controls.MenuFlyoutItem { Text = "Settings..." };
        settingsItem.Click += (_, _) => App.Instance?.OpenSettingsWindow();
        menu.Items.Add(settingsItem);

        menu.Items.Add(new Microsoft.UI.Xaml.Controls.MenuFlyoutSeparator());

        var hideItem = new Microsoft.UI.Xaml.Controls.MenuFlyoutItem { Text = "Hide" };
        hideItem.Click += (_, _) => App.Instance?.HidePill();
        menu.Items.Add(hideItem);

        menu.ShowAt((FrameworkElement)sender, e.GetPosition((UIElement)sender));
        e.Handled = true;
    }

    private async void Stop_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var result = await Services.TranscriptionService.StopAndProcessAsync();
            if (result.IsSuccess)
                await TextInjectionService.PasteText(result.Text!, _vm.KeepInClipboard);
            else if (result.IsTimeout)
                _vm.SetError(result.Error!);
            else
                _vm.SetError("Empty transcription");
        }
        catch (Exception ex) { _vm.SetError(ex.Message); }
    }
}
