using System;
using System.Text;
using System.Threading.Tasks;
using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
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

    // Track whether tick handlers have been attached (prevents duplicate handler bug → SEHException)
    private bool _amplitudeHandlerAttached;
    private bool _recordingHandlerAttached;
    private bool _completingHandlerAttached;
    private bool _errorHandlerAttached;

    // Drag state
    private bool _isDragging;
    private global::Windows.Foundation.Point _dragStart;

    private const int PillWidth = 260;
    private const int PillHeight = 72; // 36px pill + 12px padding top/bottom + extra for border

    public PillWindow(AppViewModel vm)
    {
        _vm = vm;
        this.InitializeComponent();
        Title = "Dimmy";

        // Configure transparent, borderless, always-on-top
        SetupWindow();

        // Subscribe to state changes
        _vm.PropertyChanged += Vm_PropertyChanged;

        // Initial UI
        UpdateVisualState();
    }

    private void SetupWindow()
    {
        ExtendsContentIntoTitleBar = true;
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow.Resize(new global::Windows.Graphics.SizeInt32(PillWidth, PillHeight));

        // Borderless, no taskbar entry
        if (appWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsResizable = false;
            presenter.SetBorderAndTitleBar(false, false);
            presenter.IsAlwaysOnTop = true;
        }

        WindowHelper.EnableTransparency(this);
        WindowHelper.PositionBottomRight(this, PillWidth, PillHeight);
    }

    private void Vm_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(AppViewModel.CurrentState))
        {
            DispatcherQueue.TryEnqueue(UpdateVisualState);
        }
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

        // Stop timers
        _amplitudeTimer?.Stop();
        _recordingTimer?.Stop();
        _rainbowTimer?.Stop();

        // Reset OuterBorder to default (other states: no padding, no border, no background)
        OuterBorder.Padding = new Thickness(0);
        OuterBorder.Background = null;
        OuterBorder.BorderBrush = null;
        OuterBorder.BorderThickness = new Thickness(0);

        // Hide hover labels
        LanguageLabel.Visibility = Visibility.Collapsed;
        ShortcutLabel.Visibility = Visibility.Collapsed;

        switch (_vm.CurrentState)
        {
            case AppState.Idle:
                IdlePanel.Visibility = Visibility.Visible;
                // Style dot color
                var color = _vm.LlmStyleColor;
                StyleDot.Fill = new SolidColorBrush(ParseColor(color));
                DeviceText.Text = _vm.DeviceName;
                // Low opacity when idle
                RootGrid.Opacity = 0.5;
                // Idle border: subtle white 12% opacity, 0.5px
                OuterBorder.BorderThickness = new Thickness(0.5);
                OuterBorder.BorderBrush = new SolidColorBrush(
                    global::Windows.UI.Color.FromArgb(31, 255, 255, 255)); // white 12%
                break;

            case AppState.Recording:
                RecordingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                // Show stop button in toggle mode
                StopButton.Visibility = _vm.ShortcutMode == "toggle"
                    ? Visibility.Visible : Visibility.Collapsed;
                // Start amplitude polling (~12 FPS)
                StartAmplitudePolling();
                // Start timer
                _recordingStartTime = DateTime.Now;
                StartRecordingTimer();
                // Waveform active
                Waveform.IsActive = true;
                // Rainbow border — Padding=2 creates the 2px gap, gradient Background fills it
                OuterBorder.Padding = new Thickness(2);
                StartRainbowAnimation();
                break;

            case AppState.Transcribing:
                TranscribingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                Waveform.IsActive = false;
                ChunkText.Text = _vm.ChunkTotal > 1
                    ? $"{_vm.ChunkCurrent}/{_vm.ChunkTotal}"
                    : "";
                break;

            case AppState.Processing:
                ProcessingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                break;

            case AppState.Completing:
                CompletingPanel.Visibility = Visibility.Visible;
                // Green completion border: Padding=0, BorderThickness=1.5, green 40%
                OuterBorder.Padding = new Thickness(0);
                OuterBorder.BorderBrush = new SolidColorBrush(
                    global::Windows.UI.Color.FromArgb(102, 74, 222, 128)); // #4ade80 @ 40%
                OuterBorder.BorderThickness = new Thickness(1.5);
                RootGrid.Opacity = 1.0;
                // Auto-return to idle after 1.2s
                if (_completingTimer is null)
                {
                    _completingTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1200) };
                }
                if (!_completingHandlerAttached)
                {
                    _completingTimer.Tick += CompletingTimer_Tick;
                    _completingHandlerAttached = true;
                }
                _completingTimer.Start();
                break;

            case AppState.Error:
                ErrorPanel.Visibility = Visibility.Visible;
                ErrorText.Text = _vm.ErrorMessage;
                RootGrid.Opacity = 1.0;
                // Auto-return to idle after 3s
                if (_errorTimer is null)
                {
                    _errorTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(3) };
                }
                if (!_errorHandlerAttached)
                {
                    _errorTimer.Tick += ErrorTimer_Tick;
                    _errorHandlerAttached = true;
                }
                _errorTimer.Start();
                break;
        }
    }

    private void CompletingTimer_Tick(object? sender, object e)
    {
        _completingTimer?.Stop();
        _vm.SetState(AppState.Idle);
    }

    private void ErrorTimer_Tick(object? sender, object e)
    {
        _errorTimer?.Stop();
        _vm.SetState(AppState.Idle);
    }

    private void StartAmplitudePolling()
    {
        if (_amplitudeTimer is null)
        {
            _amplitudeTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 12) };
        }
        if (!_amplitudeHandlerAttached)
        {
            _amplitudeTimer.Tick += AmplitudeTimer_Tick;
            _amplitudeHandlerAttached = true;
        }
        _amplitudeTimer.Start();
    }

    private void AmplitudeTimer_Tick(object? sender, object e)
    {
        Waveform.Amplitude = DimmyNative.dimmy_get_amplitude();
    }

    private void StartRecordingTimer()
    {
        if (_recordingTimer is null)
        {
            _recordingTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        }
        if (!_recordingHandlerAttached)
        {
            _recordingTimer.Tick += RecordingTimer_Tick;
            _recordingHandlerAttached = true;
        }
        _recordingTimer.Start();
    }

    private void RecordingTimer_Tick(object? sender, object e)
    {
        var elapsed = DateTime.Now - _recordingStartTime;
        TimerText.Text = $"{(int)elapsed.TotalMinutes:D2}:{elapsed.Seconds:D2}";
    }

    // ── Rainbow border animation ──────────────────────────────────────
    // macOS HSV colors converted to hex (9 stops from AngularGradient)
    private static readonly (double Offset, string Hex)[] RainbowHexStops =
    [
        (0.000, "#FF4D4D"),   // hue 0.0, sat 0.7, bri 1.0
        (0.125, "#FF6633"),   // hue 0.08, sat 0.8, bri 1.0
        (0.250, "#FFB84D"),   // hue 0.15, sat 0.7, bri 1.0
        (0.375, "#49F249"),   // hue 0.35, sat 0.7, bri 0.95
        (0.500, "#66E0FF"),   // hue 0.52, sat 0.6, bri 1.0
        (0.625, "#4D7AFF"),   // hue 0.62, sat 0.7, bri 1.0
        (0.750, "#9966FF"),   // hue 0.75, sat 0.6, bri 1.0
        (0.875, "#E066FF"),   // hue 0.85, sat 0.6, bri 1.0
        (1.000, "#FF4D8C"),   // hue 0.95, sat 0.7, bri 1.0
    ];

    private void StartRainbowAnimation()
    {
        // Build the gradient brush once (lazy — avoid calling ParseColor in static init)
        if (_rainbowBrush is null)
        {
            _rainbowBrush = new LinearGradientBrush();
            foreach (var (offset, hex) in RainbowHexStops)
            {
                _rainbowBrush.GradientStops.Add(new GradientStop { Offset = offset, Color = ParseColor(hex) });
            }
        }
        OuterBorder.Background = _rainbowBrush;

        _rainbowStartTime = DateTime.UtcNow;

        if (_rainbowTimer is null)
        {
            _rainbowTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 30) };
            _rainbowTimer.Tick += RainbowTimer_Tick;
        }
        _rainbowTimer.Start();
    }

    private void RainbowTimer_Tick(object? sender, object e)
    {
        if (_rainbowBrush is null) return;

        // Full rotation every 2.5 seconds -> 144 deg/s
        var elapsed = (DateTime.UtcNow - _rainbowStartTime).TotalSeconds;
        var angleDeg = elapsed * 144.0 % 360.0;
        var angleRad = angleDeg * Math.PI / 180.0;

        // Convert angle to a unit-vector StartPoint / EndPoint centred on (0.5, 0.5)
        var cos = Math.Cos(angleRad);
        var sin = Math.Sin(angleRad);

        // Scale so the endpoint always reaches the edge of the [0,1]x[0,1] space
        var scale = 0.5 / Math.Max(Math.Abs(cos), Math.Abs(sin));
        var cx = 0.5;
        var cy = 0.5;

        _rainbowBrush.StartPoint = new global::Windows.Foundation.Point(cx - cos * scale, cy - sin * scale);
        _rainbowBrush.EndPoint   = new global::Windows.Foundation.Point(cx + cos * scale, cy + sin * scale);
    }

    private static global::Windows.UI.Color ParseColor(string hex)
    {
        hex = hex.TrimStart('#');
        byte a = 255;
        byte r, g, b;
        if (hex.Length == 8)
        {
            a = Convert.ToByte(hex[0..2], 16);
            r = Convert.ToByte(hex[2..4], 16);
            g = Convert.ToByte(hex[4..6], 16);
            b = Convert.ToByte(hex[6..8], 16);
        }
        else
        {
            r = Convert.ToByte(hex[0..2], 16);
            g = Convert.ToByte(hex[2..4], 16);
            b = Convert.ToByte(hex[4..6], 16);
        }
        return global::Windows.UI.Color.FromArgb(a, r, g, b);
    }

    // ── Hover opacity + info labels ──────────────────────────────────
    private void Pill_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        if (_vm.CurrentState == AppState.Idle)
        {
            RootGrid.Opacity = 0.95;
            LanguageLabel.Text = string.IsNullOrEmpty(_vm.Language) ? "" : _vm.Language.ToUpperInvariant();
            ShortcutLabel.Text = _vm.Shortcut;
            if (!string.IsNullOrEmpty(LanguageLabel.Text))
                LanguageLabel.Visibility = Visibility.Visible;
            ShortcutLabel.Visibility = Visibility.Visible;
        }
    }

    private void Pill_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (_vm.CurrentState == AppState.Idle)
        {
            RootGrid.Opacity = 0.5;
            LanguageLabel.Visibility = Visibility.Collapsed;
            ShortcutLabel.Visibility = Visibility.Collapsed;
        }
    }

    // ── Drag support ─────────────────────────────────────────────────
    private void Pill_PointerPressed(object sender, PointerRoutedEventArgs e)
    {
        _isDragging = true;
        _dragStart = e.GetCurrentPoint(null).Position;
        ((UIElement)sender).CapturePointer(e.Pointer);
    }

    private void Pill_PointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_isDragging) return;
        var current = e.GetCurrentPoint(null).Position;
        var dx = current.X - _dragStart.X;
        var dy = current.Y - _dragStart.Y;

        var appWindow = WindowHelper.GetAppWindow(this);
        var pos = appWindow.Position;
        appWindow.Move(new global::Windows.Graphics.PointInt32(
            pos.X + (int)dx, pos.Y + (int)dy));

        _dragStart = current;
    }

    private void Pill_PointerReleased(object sender, PointerRoutedEventArgs e)
    {
        _isDragging = false;
        ((UIElement)sender).ReleasePointerCapture(e.Pointer);
        // TODO: save position to config
    }

    // ── Button handlers ──────────────────────────────────────────────
    private void Settings_Click(object sender, RoutedEventArgs e)
    {
        var settingsWindow = new SettingsWindow();
        settingsWindow.Activate();
    }

    private async void Stop_Click(object sender, RoutedEventArgs e)
    {
        // Stop recording and trigger transcription (same flow as hotkey stop)
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
}
