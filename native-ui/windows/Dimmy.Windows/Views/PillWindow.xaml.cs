using System;
using System.Numerics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;
using Microsoft.UI.Composition;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
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
    // Display AGC: tracks a smoothed peak to normalize amplitude for visual feedback.
    // Adapts like dagc — loud speech doesn't saturate, quiet speech is still visible.
    private float _displayPeak = 0.05f; // start with small value to avoid divide-by-zero
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

    private const int WindowWidth = 270;
    private const int WindowHeight = 74;

    public PillWindow(AppViewModel vm)
    {
        _vm = vm;
        this.InitializeComponent();
        Title = "Dimmy";
        BuildRainbowBrush();
        SetupWindow();
        _vm.PropertyChanged += Vm_PropertyChanged;
        Waveform.StyleMode = _vm.WaveformStyle;
        ColorBorder.SizeChanged += ColorBorder_SizeChanged;
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
        if (e.PropertyName == nameof(AppViewModel.CurrentState) || e.PropertyName == nameof(AppViewModel.BorderStyle)
            || e.PropertyName == nameof(AppViewModel.Theme))
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

    /// <summary>Border brush for active states (recording and beyond). Shows the user's chosen border color.</summary>
    private Brush GetActiveBorderBrush()
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

    // ── Glass / Glow ─────────────────────────────────────────────
    private bool IsGlass => _vm.Theme == "Light";

    // Dark: fully opaque dark  |  Glass: white-ish
    private static readonly global::Windows.UI.Color BgDark =
        global::Windows.UI.Color.FromArgb(255, 26, 26, 26);       // #FF1a1a1a
    private static readonly global::Windows.UI.Color BgGlassActive =
        global::Windows.UI.Color.FromArgb(255, 200, 200, 210);    // #FFC8C8D2 — fully opaque, border stays border
    private static readonly global::Windows.UI.Color BgGlassIdle =
        global::Windows.UI.Color.FromArgb(140, 200, 200, 210);    // #8CC8C8D2 — idle, more transparent

    private SolidColorBrush GetPillBackground(bool idle = false) =>
        new(IsGlass ? (idle ? BgGlassIdle : BgGlassActive) : BgDark);

    /// <summary>Swap text/icon foreground colors for dark vs glass (light) theme.</summary>
    private void ApplyThemeColors()
    {
        var textPrimary = IsGlass
            ? new SolidColorBrush(global::Windows.UI.Color.FromArgb(238, 30, 30, 30))   // dark text
            : new SolidColorBrush(global::Windows.UI.Color.FromArgb(238, 255, 255, 255)); // white text
        var textSecondary = IsGlass
            ? new SolidColorBrush(global::Windows.UI.Color.FromArgb(140, 30, 30, 30))
            : new SolidColorBrush(global::Windows.UI.Color.FromArgb(119, 255, 255, 255));

        // Idle
        LanguageLabel.Foreground = IsGlass
            ? new SolidColorBrush(global::Windows.UI.Color.FromArgb(170, 30, 30, 30))
            : new SolidColorBrush(global::Windows.UI.Color.FromArgb(170, 255, 255, 255));
        ShortcutLabel.Foreground = textSecondary;

        // Recording
        TimerText.Foreground = textPrimary;

        // Note: Recording/Transcribing/Processing text + waveform stay white —
        // they're inside the colored border area where white reads well on any theme.
    }

    // ── Composition DropShadow glow ────────────────────────────
    private SpriteVisual? _glowVisual;
    private DropShadow? _glowShadow;
    private CompositionRoundedRectangleGeometry? _glowGeometry;
    private global::Windows.UI.Color? _glowColor;
    private bool _glowSubtle;

    private void EnsureGlowVisual()
    {
        if (_glowVisual != null) return;

        var compositor = ElementCompositionPreview.GetElementVisual(GlowHost).Compositor;

        // Rounded rect geometry for shadow mask
        _glowGeometry = compositor.CreateRoundedRectangleGeometry();
        var shape = compositor.CreateSpriteShape(_glowGeometry);
        shape.FillBrush = compositor.CreateColorBrush(
            global::Windows.UI.Color.FromArgb(255, 255, 255, 255));

        // ShapeVisual renders the rounded rect → used as mask source
        var maskVisual = compositor.CreateShapeVisual();
        maskVisual.Shapes.Add(shape);

        // Render the shape to a surface for the shadow mask
        var surface = compositor.CreateVisualSurface();
        surface.SourceVisual = maskVisual;
        surface.SourceSize = new Vector2(1, 1); // updated in ApplyGlow

        _glowShadow = compositor.CreateDropShadow();
        _glowShadow.Offset = Vector3.Zero;
        _glowShadow.BlurRadius = 16;
        _glowShadow.Opacity = 0;
        _glowShadow.Mask = compositor.CreateSurfaceBrush(surface);

        // SpriteVisual hosts the shadow
        _glowVisual = compositor.CreateSpriteVisual();
        _glowVisual.Shadow = _glowShadow;

        ElementCompositionPreview.SetElementChildVisual(GlowHost, _glowVisual);
    }

    private CompositionVisualSurface? _glowSurface;
    private ShapeVisual? _glowMaskVisual;

    private void ApplyGlow()
    {
        if (_glowColor is not { } color || _glowShadow == null ||
            _glowVisual == null || _glowGeometry == null) return;

        var w = ColorBorder.ActualWidth;
        var h = ColorBorder.ActualHeight;
        if (w <= 0 || h <= 0) return;

        var cr = (float)ColorBorder.CornerRadius.TopLeft;
        var size = new Vector2((float)w, (float)h);

        _glowVisual.Size = size;
        _glowGeometry.Size = size;
        _glowGeometry.CornerRadius = new Vector2(cr, cr);
        GlowHost.Width = w;
        GlowHost.Height = h;

        // Update mask surface size to match
        if (_glowShadow.Mask is CompositionSurfaceBrush surfBrush &&
            surfBrush.Surface is CompositionVisualSurface surf)
        {
            surf.SourceSize = size;
            // Also update the mask shape visual size
            if (surf.SourceVisual is ShapeVisual sv)
                sv.Size = size;
        }

        _glowShadow.Color = color;
        _glowShadow.BlurRadius = _glowSubtle ? 12 : 22;
        _glowShadow.Opacity = _glowSubtle ? 0.5f : 0.8f;
    }

    private void UpdateGlow(global::Windows.UI.Color color, bool subtle = false)
    {
        EnsureGlowVisual();
        _glowColor = color;
        _glowSubtle = subtle;
        ApplyGlow();
    }

    private void HideGlow()
    {
        _glowColor = null;
        if (_glowShadow != null) _glowShadow.Opacity = 0;
    }

    private void ColorBorder_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_glowColor != null) ApplyGlow();
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

        // Apply text colors for theme
        ApplyThemeColors();

        switch (_vm.CurrentState)
        {
            case AppState.Idle:
                IdlePanel.Visibility = Visibility.Visible;
                StyleDot.Fill = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
                RootGrid.Opacity = 1.0;
                SetCircleShape();
                PillInner.Background = GetPillBackground(idle: true);
                // No colored border in idle — transparent in glass, dark in dark
                ColorBorder.Background = IsGlass
                    ? new SolidColorBrush(global::Windows.UI.Color.FromArgb(0, 0, 0, 0))
                    : new SolidColorBrush(BgDark);
                _rainbowTimer?.Stop();
                // Subtle glow in idle — uses LLM style color at low intensity
                UpdateGlow(ParseColor(_vm.LlmStyleColor), subtle: true);
                break;

            case AppState.Recording:
                RecordingPanel.Visibility = Visibility.Visible;
                RootGrid.Opacity = 1.0;
                PillInner.Background = GetPillBackground();
                StopButton.Visibility = Visibility.Visible;
                SetCapsuleShape();
                ColorBorder.Background = GetActiveBorderBrush();
                StartAmplitudePolling();
                _recordingStartTime = DateTime.Now;
                StartRecordingTimer();
                Waveform.IsActive = true;
                if (_vm.BorderStyle == "Rainbow") StartRainbowAnimation();
                else _rainbowTimer?.Stop();
                // Glow in recording: use the border color (first solid stop or blue)
                UpdateGlow(_vm.BorderStyle switch
                {
                    "Blue" => BorderBlue, "Green" => BorderGreen,
                    "Purple" => BorderPurple, "Orange" => BorderOrange,
                    _ => BorderBlue // rainbow → default blue glow
                });
                break;

            case AppState.Transcribing:
                TranscribingPanel.Visibility = Visibility.Visible;
                PillInner.Background = GetPillBackground();
                RootGrid.Opacity = 1.0;
                Waveform.IsActive = false;
                ChunkText.Text = _vm.ChunkTotal > 1 ? $"{_vm.ChunkCurrent}/{_vm.ChunkTotal}" : "";
                SetCapsuleShape();
                _rainbowTimer?.Stop();
                ColorBorder.Background = new SolidColorBrush(ColorTranscribing);
                UpdateGlow(ColorTranscribing);
                break;

            case AppState.Processing:
                ProcessingPanel.Visibility = Visibility.Visible;
                PillInner.Background = GetPillBackground();
                RootGrid.Opacity = 1.0;
                SetCapsuleShape();
                _rainbowTimer?.Stop();
                ColorBorder.Background = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
                UpdateGlow(ParseColor(_vm.LlmStyleColor));
                break;

            case AppState.Completing:
                CompletingPanel.Visibility = Visibility.Visible;
                PillInner.Background = GetPillBackground();
                RootGrid.Opacity = 1.0;
                SetCircleShape();
                _rainbowTimer?.Stop();
                var completingColor = _vm.LlmStyle != "off"
                    ? ParseColor(_vm.LlmStyleColor)
                    : ColorCompleting;
                ColorBorder.Background = new SolidColorBrush(completingColor);
                UpdateGlow(completingColor);
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
                PillInner.Background = GetPillBackground();
                RootGrid.Opacity = 1.0;
                SetCapsuleShape();
                _rainbowTimer?.Stop();
                ColorBorder.Background = new SolidColorBrush(ColorError);
                UpdateGlow(ColorError);
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
        _displayPeak = 0.05f; // reset AGC for each new recording
        if (_amplitudeTimer is null)
            _amplitudeTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 12) };
        if (!_amplitudeHandlerAttached)
        {
            _amplitudeTimer.Tick += (_, _) =>
            {
                var amp = DimmyNative.dimmy_get_amplitude();

                // Display AGC: smoothly track the peak level, then normalize against it.
                // - When loud: _displayPeak rises fast → normalized value stays <1.0
                // - When quiet: _displayPeak decays slowly → quiet speech still shows bars
                // Attack fast (0.3), release slow (0.005) — same principle as dagc.
                if (amp > _displayPeak)
                    _displayPeak += (amp - _displayPeak) * 0.3f;  // fast attack
                else
                    _displayPeak *= 0.995f;  // slow release (~1.4s to halve at 12Hz)

                // Floor to avoid dead bars when completely silent
                _displayPeak = Math.Max(_displayPeak, 0.01f);

                // Normalize: current amplitude relative to tracked peak
                var normalized = amp / _displayPeak;
                Waveform.Amplitude = Math.Clamp(normalized, 0f, 1f);
            };
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
            SetCapsuleShape();
            LanguageLabel.Text = string.IsNullOrEmpty(_vm.Language) ? "" : _vm.Language.ToUpperInvariant();
            ShortcutLabel.Text = _vm.Shortcut;
            if (!string.IsNullOrEmpty(LanguageLabel.Text))
                LanguageLabel.Visibility = Visibility.Visible;
            ShortcutLabel.Visibility = Visibility.Visible;
            IdleContent.Margin = new Thickness(10, 0, 10, 0);
            // Hover glow — slightly brighter than idle
            UpdateGlow(ParseColor(_vm.LlmStyleColor), subtle: true);
        }
    }

    private void Pill_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (_vm.CurrentState == AppState.Idle)
        {
            RootGrid.Opacity = 1.0;
            SetCircleShape();
            LanguageLabel.Visibility = Visibility.Collapsed;
            ShortcutLabel.Visibility = Visibility.Collapsed;
            IdleContent.Margin = new Thickness(0);
            // Back to subtle idle glow
            UpdateGlow(ParseColor(_vm.LlmStyleColor), subtle: true);
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
        // Single writer: only FFI, Rust saves to disk
        DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(
            new System.Collections.Generic.Dictionary<string, object>
            {
                ["llm_style"] = _vm.LlmStyle,
                ["llm_enabled"] = _vm.LlmStyle != "off"
            }));
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
        // Single writer: only FFI, Rust saves to disk
        DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(
            new System.Collections.Generic.Dictionary<string, string> { ["language"] = _vm.Language }));
        e.Handled = true;
    }

    // ── Actions ─────────────────────────────────────────────────────
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
