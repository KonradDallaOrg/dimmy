using System;
using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
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

    // Drag state
    private bool _isDragging;
    private global::Windows.Foundation.Point _dragStart;

    private const int PillWidth = 320;
    private const int PillHeight = 96;

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

        // Hide border decorations
        RainbowBorder.Visibility = Visibility.Collapsed;
        CompletionBorder.Visibility = Visibility.Collapsed;

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
                // Rainbow border
                RainbowBorder.Visibility = Visibility.Visible;
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
                CompletionBorder.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                // Auto-return to idle after 1.2s
                _completingTimer ??= new DispatcherTimer();
                _completingTimer.Interval = TimeSpan.FromMilliseconds(1200);
                _completingTimer.Tick += (s, e) =>
                {
                    _completingTimer.Stop();
                    _vm.SetState(AppState.Idle);
                };
                _completingTimer.Start();
                break;

            case AppState.Error:
                ErrorPanel.Visibility = Visibility.Visible;
                ErrorText.Text = _vm.ErrorMessage;
                RootGrid.Opacity = 1.0;
                // Auto-return to idle after 3s
                _errorTimer ??= new DispatcherTimer();
                _errorTimer.Interval = TimeSpan.FromSeconds(3);
                _errorTimer.Tick += (s, e) =>
                {
                    _errorTimer.Stop();
                    _vm.SetState(AppState.Idle);
                };
                _errorTimer.Start();
                break;
        }
    }

    private void StartAmplitudePolling()
    {
        _amplitudeTimer ??= new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 12) };
        _amplitudeTimer.Tick += (s, e) =>
        {
            Waveform.Amplitude = DimmyNative.dimmy_get_amplitude();
        };
        _amplitudeTimer.Start();
    }

    private void StartRecordingTimer()
    {
        _recordingTimer ??= new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        _recordingTimer.Tick += (s, e) =>
        {
            var elapsed = DateTime.Now - _recordingStartTime;
            TimerText.Text = $"{(int)elapsed.TotalMinutes:D2}:{elapsed.Seconds:D2}";
        };
        _recordingTimer.Start();
    }

    // ── Rainbow border animation ──────────────────────────────────────
    // 9 hue stops, full 360° rotation in 2.5 seconds (~30 FPS via 33 ms tick)
    private static readonly (double Offset, global::Windows.UI.Color Color)[] RainbowStops =
    [
        (0.000, ParseColor("#FF0000")),
        (0.125, ParseColor("#FF6600")),
        (0.250, ParseColor("#FFCC00")),
        (0.375, ParseColor("#33CC00")),
        (0.500, ParseColor("#00CCCC")),
        (0.625, ParseColor("#0066FF")),
        (0.750, ParseColor("#6600FF")),
        (0.875, ParseColor("#CC00CC")),
        (1.000, ParseColor("#FF0066")),
    ];

    private void StartRainbowAnimation()
    {
        // Build the gradient brush once
        if (_rainbowBrush is null)
        {
            _rainbowBrush = new LinearGradientBrush();
            foreach (var (offset, color) in RainbowStops)
            {
                _rainbowBrush.GradientStops.Add(new GradientStop { Offset = offset, Color = color });
            }
            RainbowBorder.Background = _rainbowBrush;
        }

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

        // Full rotation every 2.5 seconds → 144°/s
        var elapsed = (DateTime.UtcNow - _rainbowStartTime).TotalSeconds;
        var angleDeg = elapsed * 144.0 % 360.0;
        var angleRad = angleDeg * Math.PI / 180.0;

        // Convert angle to a unit-vector StartPoint / EndPoint centred on (0.5, 0.5)
        var cos = Math.Cos(angleRad);
        var sin = Math.Sin(angleRad);

        // Scale so the endpoint always reaches the edge of the [0,1]×[0,1] space
        var scale = 0.5 / Math.Max(Math.Abs(cos), Math.Abs(sin));
        var cx = 0.5;
        var cy = 0.5;

        _rainbowBrush.StartPoint = new global::Windows.Foundation.Point(cx - cos * scale, cy - sin * scale);
        _rainbowBrush.EndPoint   = new global::Windows.Foundation.Point(cx + cos * scale, cy + sin * scale);
    }

    private static global::Windows.UI.Color ParseColor(string hex)
    {
        hex = hex.TrimStart('#');
        byte r = Convert.ToByte(hex[0..2], 16);
        byte g = Convert.ToByte(hex[2..4], 16);
        byte b = Convert.ToByte(hex[4..6], 16);
        return global::Windows.UI.Color.FromArgb(255, r, g, b);
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

    private void Stop_Click(object sender, RoutedEventArgs e)
    {
        // Stop recording in toggle mode
        // The actual stop + transcribe is handled by HotkeyService
        DimmyNative.dimmy_cancel_recording();
    }
}
