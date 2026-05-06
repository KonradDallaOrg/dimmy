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

    // Circle size for idle/completing states — matched to macOS (36pt)
    private const double CircleSize = 32;
    // Capsule height for recording/transcribing states
    private const double CapsuleHeight = 36;

    private const int WindowWidth = 240;
    private const int WindowHeight = 56;

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
    private const double AnimationDurationMs = 150;
    private const double AnimationFps = 60;
    private DispatcherTimer? _shapeAnimTimer;
    private double _animStartW, _animStartH, _animTargetW, _animTargetH;
    private global::Windows.UI.Color _animStartColor, _animTargetColor;
    private DateTime _animStartTime;
    private Action? _animOnComplete;
    // Track which panel is the NEW content (fades in) and which is OLD (fades out)
    private FrameworkElement? _animNewPanel;
    private FrameworkElement? _animOldPanel;
    // Last known rendered size — fallback when ActualWidth is stale after Width=NaN
    private double _lastKnownW;
    private double _lastKnownH;

    private void SetCircleShape()
    {
        ColorBorder.Width = CircleSize;
        ColorBorder.Height = CircleSize;
        PillBodyHost.Width = CircleSize;
        PillBodyHost.Height = CircleSize;
        var r = CircleSize / 2;
        ColorBorder.CornerRadius = new CornerRadius(r);
        PillInner.CornerRadius = new CornerRadius(r);
        _lastKnownW = CircleSize;
        _lastKnownH = CircleSize;
    }

    private void SetCapsuleShape()
    {
        ColorBorder.Width = double.NaN; // auto
        ColorBorder.Height = CapsuleHeight;
        PillBodyHost.Height = CapsuleHeight;
        var r = CapsuleHeight / 2;
        ColorBorder.CornerRadius = new CornerRadius(r);
        PillInner.CornerRadius = new CornerRadius(r);
    }

    private bool _animSkipBorderColor;

    /// <summary>
    /// Unified animation: smoothly interpolates size, border color, and content opacity.
    /// Pass skipBorderColor=true when rainbow or gradient brush manages the border externally.
    /// </summary>
    private void AnimateTransition(double targetW, double targetH,
        global::Windows.UI.Color targetBorderColor,
        FrameworkElement? newPanel = null, FrameworkElement? oldPanel = null,
        Action? onComplete = null, bool skipBorderColor = false)
    {
        _shapeAnimTimer?.Stop();

        // Force layout so ActualWidth/Height are fresh after Width=NaN
        ContentGrid.UpdateLayout();

        // Prefer explicit Width if set by a prior animation tick; fall back to ActualWidth;
        // last resort: _lastKnownW which is always updated by every tick and shape setter.
        _animStartW = (!double.IsNaN(ColorBorder.Width) && ColorBorder.Width > 0)
            ? ColorBorder.Width
            : ColorBorder.ActualWidth > 0
                ? ColorBorder.ActualWidth
                : _lastKnownW > 0 ? _lastKnownW : CircleSize;
        _animStartH = (!double.IsNaN(ColorBorder.Height) && ColorBorder.Height > 0)
            ? ColorBorder.Height
            : ColorBorder.ActualHeight > 0
                ? ColorBorder.ActualHeight
                : _lastKnownH > 0 ? _lastKnownH : CircleSize;
        _animTargetW = targetW;
        _animTargetH = targetH;
        _animSkipBorderColor = skipBorderColor;

        // Capture current border color
        _animStartColor = ColorBorder.BorderBrush is SolidColorBrush scb
            ? scb.Color
            : global::Windows.UI.Color.FromArgb(0, 0, 0, 0);
        _animTargetColor = targetBorderColor;

        _animNewPanel = newPanel;
        _animOldPanel = oldPanel;
        _animOnComplete = onComplete;
        _animStartTime = DateTime.Now;

        // Start new panel at 0 opacity, old panel at full
        if (_animNewPanel != null) _animNewPanel.Opacity = 0;
        if (_animOldPanel != null) _animOldPanel.Opacity = 1;

        if (_shapeAnimTimer == null)
        {
            _shapeAnimTimer = new DispatcherTimer
            {
                Interval = TimeSpan.FromMilliseconds(1000.0 / AnimationFps)
            };
            _shapeAnimTimer.Tick += ShapeAnimTick;
        }
        _shapeAnimTimer.Start();
    }

    private void ShapeAnimTick(object? sender, object e)
    {
        var elapsed = (DateTime.Now - _animStartTime).TotalMilliseconds;
        var t = Math.Min(1.0, elapsed / AnimationDurationMs);
        // Ease-out cubic
        var ease = 1.0 - Math.Pow(1.0 - t, 3);

        // Size
        var w = _animStartW + (_animTargetW - _animStartW) * ease;
        var h = _animStartH + (_animTargetH - _animStartH) * ease;
        var r = h / 2;

        ColorBorder.Width = w;
        ColorBorder.Height = h;
        PillBodyHost.Width = w;
        PillBodyHost.Height = h;
        ColorBorder.CornerRadius = new CornerRadius(r);
        PillInner.CornerRadius = new CornerRadius(r);
        _lastKnownW = w;
        _lastKnownH = h;

        // Border color interpolation (skip when rainbow/gradient manages the border)
        if (!_animSkipBorderColor)
        {
            var cA = (byte)Math.Clamp(_animStartColor.A + (int)((_animTargetColor.A - _animStartColor.A) * ease), 0, 255);
            var cR = (byte)Math.Clamp(_animStartColor.R + (int)((_animTargetColor.R - _animStartColor.R) * ease), 0, 255);
            var cG = (byte)Math.Clamp(_animStartColor.G + (int)((_animTargetColor.G - _animStartColor.G) * ease), 0, 255);
            var cB = (byte)Math.Clamp(_animStartColor.B + (int)((_animTargetColor.B - _animStartColor.B) * ease), 0, 255);
            ColorBorder.BorderBrush = new SolidColorBrush(
                global::Windows.UI.Color.FromArgb(cA, cR, cG, cB));
        }

        // Content crossfade
        if (_animNewPanel != null) _animNewPanel.Opacity = ease;
        if (_animOldPanel != null) _animOldPanel.Opacity = 1.0 - ease;

        if (t >= 1.0)
        {
            _shapeAnimTimer!.Stop();
            // Ensure final opacity
            if (_animNewPanel != null) _animNewPanel.Opacity = 1;
            if (_animOldPanel != null)
            {
                _animOldPanel.Opacity = 1; // reset
                _animOldPanel.Visibility = Visibility.Collapsed;
            }
            _animNewPanel = null;
            _animOldPanel = null;
            _animOnComplete?.Invoke();
            _animOnComplete = null;
        }
    }

    /// <summary>
    /// Animate to circle shape (idle/completing).
    /// </summary>
    private void AnimateToCircle(global::Windows.UI.Color borderColor,
        FrameworkElement? newPanel = null, FrameworkElement? oldPanel = null,
        bool skipBorderColor = false)
    {
        AnimateTransition(CircleSize, CircleSize, borderColor, newPanel, oldPanel,
            skipBorderColor: skipBorderColor);
    }

    /// <summary>
    /// Animate to capsule shape. Measures content to determine target width.
    /// </summary>
    private void AnimateToCapsule(global::Windows.UI.Color borderColor,
        FrameworkElement? newPanel = null, FrameworkElement? oldPanel = null,
        bool skipBorderColor = false)
    {
        // Measure desired capsule width
        var prevW = ColorBorder.Width;
        var prevH = ColorBorder.Height;

        // Temporarily set auto to measure
        ColorBorder.Width = double.NaN;
        ColorBorder.Height = CapsuleHeight;
        ContentGrid.UpdateLayout();
        var measuredW = ColorBorder.ActualWidth;

        // Restore — animation will start from current rendered size
        ColorBorder.Width = double.IsNaN(prevW) || prevW <= 0
            ? (_lastKnownW > 0 ? _lastKnownW : CircleSize) : prevW;
        ColorBorder.Height = double.IsNaN(prevH) || prevH <= 0
            ? (_lastKnownH > 0 ? _lastKnownH : CircleSize) : prevH;

        if (measuredW <= 0) measuredW = 120; // fallback

        AnimateTransition(measuredW, CapsuleHeight, borderColor, newPanel, oldPanel, () =>
        {
            // After animation: switch to auto width for responsive content
            ColorBorder.Width = double.NaN;
            PillBodyHost.Height = CapsuleHeight;
        }, skipBorderColor);
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
        global::Windows.UI.Color.FromArgb(160, 210, 210, 218);    // #A0D2D2DA — more transparent, lighter glass
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

        // Recording / Transcribing / Processing — always white to match waveform bars
        var activeFg = new SolidColorBrush(
            global::Windows.UI.Color.FromArgb(230, 255, 255, 255));
        var activeSecondary = new SolidColorBrush(
            global::Windows.UI.Color.FromArgb(119, 255, 255, 255));
        TimerText.Foreground = activeFg;
        StopIcon.Background = activeFg;
        TranscribingText.Foreground = activeFg;
        ChunkText.Foreground = activeSecondary;
        ProcessingText.Foreground = activeFg;
    }

    // ── Composition pill body (anti-aliased rounded rect) ──────
    private ShapeVisual? _pillBodyVisual;
    private CompositionRoundedRectangleGeometry? _pillBodyGeometry;
    private CompositionColorBrush? _pillBodyBrush;

    private void EnsurePillBodyVisual()
    {
        if (_pillBodyVisual != null) return;

        var compositor = ElementCompositionPreview.GetElementVisual(PillBodyHost).Compositor;

        _pillBodyGeometry = compositor.CreateRoundedRectangleGeometry();
        _pillBodyBrush = compositor.CreateColorBrush(
            global::Windows.UI.Color.FromArgb(255, 26, 26, 26));

        var shape = compositor.CreateSpriteShape(_pillBodyGeometry);
        shape.FillBrush = _pillBodyBrush;

        _pillBodyVisual = compositor.CreateShapeVisual();
        _pillBodyVisual.Shapes.Add(shape);

        ElementCompositionPreview.SetElementChildVisual(PillBodyHost, _pillBodyVisual);
    }

    private void UpdatePillBody()
    {
        if (_pillBodyVisual == null || _pillBodyGeometry == null || _pillBodyBrush == null) return;

        var w = ColorBorder.ActualWidth;
        var h = ColorBorder.ActualHeight;
        if (w <= 0 || h <= 0) return;

        // Inset by border thickness so the body sits inside the border ring
        const float inset = 2f; // matches BorderThickness
        var innerW = (float)w - inset * 2;
        var innerH = (float)h - inset * 2;
        var cr = Math.Max(0f, (float)ColorBorder.CornerRadius.TopLeft - inset);

        _pillBodyVisual.Size = new Vector2(innerW, innerH);
        _pillBodyVisual.Offset = new Vector3(inset, inset, 0);
        _pillBodyGeometry.Size = new Vector2(innerW, innerH);
        _pillBodyGeometry.CornerRadius = new Vector2(cr, cr);
        PillBodyHost.Width = w;
        PillBodyHost.Height = h;
    }

    private void SetPillBodyColor(global::Windows.UI.Color color)
    {
        EnsurePillBodyVisual();
        _pillBodyBrush?.Dispose();
        var compositor = _pillBodyVisual!.Compositor;
        _pillBodyBrush = compositor.CreateColorBrush(color);
        if (_pillBodyVisual.Shapes.Count > 0 &&
            _pillBodyVisual.Shapes[0] is CompositionSpriteShape shape)
            shape.FillBrush = _pillBodyBrush;
        UpdatePillBody();
    }

    // ── Composition DropShadow glow ────────────────────────────
    private SpriteVisual? _glowVisual;
    private DropShadow? _glowShadow;
    private CompositionRoundedRectangleGeometry? _glowGeometry;
    private CompositionSpriteShape? _glowShape;
    private ShapeVisual? _glowMaskShapeVisual;
    private global::Windows.UI.Color? _glowColor;
    private bool _glowSubtle;
    private const float GlowPad = 30; // extra space for blur spread

    private void EnsureGlowVisual()
    {
        if (_glowVisual != null) return;

        var compositor = ElementCompositionPreview.GetElementVisual(GlowHost).Compositor;

        // Ring shape (stroke only, no fill) — glow radiates outward from border,
        // interior stays clean. Thick stroke gives the shadow enough body.
        _glowGeometry = compositor.CreateRoundedRectangleGeometry();
        _glowShape = compositor.CreateSpriteShape(_glowGeometry);
        _glowShape.FillBrush = null;
        _glowShape.StrokeBrush = compositor.CreateColorBrush(
            global::Windows.UI.Color.FromArgb(255, 255, 255, 255));
        _glowShape.StrokeThickness = 8;

        _glowMaskShapeVisual = compositor.CreateShapeVisual();
        _glowMaskShapeVisual.Shapes.Add(_glowShape);

        var surface = compositor.CreateVisualSurface();
        surface.SourceVisual = _glowMaskShapeVisual;
        surface.SourceSize = new Vector2(1, 1);

        _glowShadow = compositor.CreateDropShadow();
        _glowShadow.Offset = Vector3.Zero;
        _glowShadow.BlurRadius = 20;
        _glowShadow.Opacity = 0;
        _glowShadow.Mask = compositor.CreateSurfaceBrush(surface);

        _glowVisual = compositor.CreateSpriteVisual();
        _glowVisual.Shadow = _glowShadow;

        ElementCompositionPreview.SetElementChildVisual(GlowHost, _glowVisual);
    }

    private void ApplyGlow()
    {
        if (_glowColor is not { } color || _glowShadow == null ||
            _glowVisual == null || _glowGeometry == null || _glowMaskShapeVisual == null) return;

        var w = ColorBorder.ActualWidth;
        var h = ColorBorder.ActualHeight;
        if (w <= 0 || h <= 0) return;

        var cr = (float)ColorBorder.CornerRadius.TopLeft;

        // The visual is padded so the blur has room to spread without clipping
        var totalW = (float)w + GlowPad * 2;
        var totalH = (float)h + GlowPad * 2;
        var totalSize = new Vector2(totalW, totalH);
        var pillSize = new Vector2((float)w, (float)h);

        _glowVisual.Size = totalSize;
        // Offset the visual so the pill shape is centered within the padded area
        _glowVisual.Offset = new Vector3(-GlowPad, -GlowPad, 0);

        // Geometry is pill-sized, offset to center within the padded visual
        _glowGeometry.Size = pillSize;
        _glowGeometry.Offset = new Vector2(GlowPad, GlowPad);
        _glowGeometry.CornerRadius = new Vector2(cr, cr);

        _glowMaskShapeVisual.Size = totalSize;

        // GlowHost matches pill size (positioned behind it)
        GlowHost.Width = w;
        GlowHost.Height = h;

        // Update mask surface
        if (_glowShadow.Mask is CompositionSurfaceBrush surfBrush &&
            surfBrush.Surface is CompositionVisualSurface surf)
        {
            surf.SourceSize = totalSize;
        }

        _glowShadow.Color = color;
        // Same blur for all states; opacity controls intensity
        _glowShadow.BlurRadius = 10;
        _glowShadow.Opacity = _glowSubtle ? 0.15f : 0.5f;
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
        UpdatePillBody();
    }

    private FrameworkElement? _currentVisiblePanel;

    /// <summary>Get the panel for the current state (before switching).</summary>
    private FrameworkElement? GetPanelForState(AppState state) => state switch
    {
        AppState.Idle => IdlePanel,
        AppState.Recording => RecordingPanel,
        AppState.Transcribing => TranscribingPanel,
        AppState.Processing => ProcessingPanel,
        AppState.Completing => CompletingPanel,
        AppState.Error => ErrorPanel,
        _ => null,
    };

    private global::Windows.UI.Color GetBorderColorForRecording() => _vm.BorderStyle switch
    {
        "Blue" => BorderBlue, "Green" => BorderGreen,
        "Purple" => BorderPurple, "Orange" => BorderOrange,
        _ => BorderBlue,
    };

    private void UpdateVisualState()
    {
        var oldPanel = _currentVisiblePanel;

        // Hide all panels EXCEPT the old one (it will fade out)
        foreach (var p in new FrameworkElement[] { IdlePanel, RecordingPanel, TranscribingPanel,
                                                    ProcessingPanel, CompletingPanel, ErrorPanel })
        {
            if (p != oldPanel) p.Visibility = Visibility.Collapsed;
        }

        _amplitudeTimer?.Stop();
        _recordingTimer?.Stop();

        LanguageLabel.Visibility = Visibility.Collapsed;
        ShortcutLabel.Visibility = Visibility.Collapsed;

        // Apply text colors for theme
        ApplyThemeColors();

        FrameworkElement? newPanel = GetPanelForState(_vm.CurrentState);

        // Make new panel visible (animation will fade it in from 0)
        if (newPanel != null && newPanel != oldPanel)
            newPanel.Visibility = Visibility.Visible;

        // If same panel (e.g. capsule→capsule), don't crossfade
        if (newPanel == oldPanel) oldPanel = null;

        _currentVisiblePanel = newPanel;

        switch (_vm.CurrentState)
        {
            case AppState.Idle:
                StyleDot.Fill = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
                RootGrid.Opacity = 1.0;
                SetPillBodyColor(IsGlass ? BgGlassIdle : BgDark);
                _rainbowTimer?.Stop();
                AnimateToCircle(global::Windows.UI.Color.FromArgb(0, 0, 0, 0), newPanel, oldPanel);
                UpdateGlow(ParseColor(_vm.LlmStyleColor), subtle: true);
                break;

            case AppState.Recording:
                RootGrid.Opacity = 1.0;
                SetPillBodyColor(IsGlass ? BgGlassActive : BgDark);
                StopButton.Visibility = Visibility.Visible;
                StartAmplitudePolling();
                _recordingStartTime = DateTime.Now;
                StartRecordingTimer();
                Waveform.IsActive = true;
                if (_vm.BorderStyle == "Rainbow")
                {
                    // Set rainbow brush first, then animate size only (skip border color)
                    ColorBorder.BorderBrush = _rainbowBrush;
                    StartRainbowAnimation();
                    AnimateToCapsule(BorderBlue, newPanel, oldPanel, skipBorderColor: true);
                }
                else
                {
                    _rainbowTimer?.Stop();
                    AnimateToCapsule(GetBorderColorForRecording(), newPanel, oldPanel);
                }
                UpdateGlow(GetBorderColorForRecording());
                break;

            case AppState.Transcribing:
                SetPillBodyColor(IsGlass ? BgGlassActive : BgDark);
                RootGrid.Opacity = 1.0;
                Waveform.IsActive = false;
                ChunkText.Text = _vm.ChunkTotal > 1 ? $"{_vm.ChunkCurrent}/{_vm.ChunkTotal}" : "";
                _rainbowTimer?.Stop();
                AnimateToCapsule(ColorTranscribing, newPanel, oldPanel);
                UpdateGlow(ColorTranscribing);
                break;

            case AppState.Processing:
                SetPillBodyColor(IsGlass ? BgGlassActive : BgDark);
                RootGrid.Opacity = 1.0;
                _rainbowTimer?.Stop();
                AnimateToCapsule(ParseColor(_vm.LlmStyleColor), newPanel, oldPanel);
                UpdateGlow(ParseColor(_vm.LlmStyleColor));
                break;

            case AppState.Completing:
                SetPillBodyColor(IsGlass ? BgGlassActive : BgDark);
                RootGrid.Opacity = 1.0;
                _rainbowTimer?.Stop();
                var completingColor = _vm.LlmStyle != "off"
                    ? ParseColor(_vm.LlmStyleColor)
                    : ColorCompleting;
                AnimateToCircle(completingColor, newPanel, oldPanel);
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
                ErrorText.Text = _vm.ErrorMessage;
                SetPillBodyColor(IsGlass ? BgGlassActive : BgDark);
                RootGrid.Opacity = 1.0;
                _rainbowTimer?.Stop();
                AnimateToCapsule(ColorError, newPanel, oldPanel);
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
            // Pill language label = output TRANSLATION target, NOT STT input.
            // Native (input) language is configured in Settings → General.
            // When no translation is set, show an em-dash so the scroll-wheel
            // hit target stays visible — otherwise users have nothing to roll
            // over to switch translation back on.
            LanguageLabel.Text = string.IsNullOrEmpty(_vm.LlmTranslateTo)
                ? "—"
                : _vm.LlmTranslateTo.ToUpperInvariant();
            ShortcutLabel.Text = _vm.Shortcut;
            LanguageLabel.Visibility = Visibility.Visible;
            ShortcutLabel.Visibility = Visibility.Visible;
            IdleContent.Margin = new Thickness(7, 0, 7, 0);
            // Hover keeps same transparent border — just animate size
            AnimateToCapsule(global::Windows.UI.Color.FromArgb(0, 0, 0, 0));
            UpdateGlow(ParseColor(_vm.LlmStyleColor), subtle: true);
        }
    }

    private void Pill_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (_vm.CurrentState == AppState.Idle)
        {
            RootGrid.Opacity = 1.0;
            LanguageLabel.Visibility = Visibility.Collapsed;
            ShortcutLabel.Visibility = Visibility.Collapsed;
            IdleContent.Margin = new Thickness(0);
            AnimateToCircle(global::Windows.UI.Color.FromArgb(0, 0, 0, 0));
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
    // Pill's scroll-wheel cycles `llm_translate_to`. The list MUST include
    // "" → "No translation" so users can scroll back to "off" — without it
    // the pill becomes a one-way ticket once translation is engaged.
    private static readonly System.Collections.Generic.List<System.Collections.Generic.KeyValuePair<string, string>> LangList = ViewModels.SettingsViewModel.TranslateTargets;

    private DateTime _lastScrollTime; // debounce for touchpad rapid-fire scroll
    private DispatcherTimer? _tooltipTimer;

    private void ShowScrollTooltip(string text)
    {
        ScrollTooltipText.Text = text;
        ScrollTooltip.Background = new SolidColorBrush(IsGlass
            ? global::Windows.UI.Color.FromArgb(230, 240, 240, 245)
            : global::Windows.UI.Color.FromArgb(230, 32, 32, 32));
        ScrollTooltipText.Foreground = new SolidColorBrush(IsGlass
            ? global::Windows.UI.Color.FromArgb(238, 30, 30, 30)
            : global::Windows.UI.Color.FromArgb(238, 255, 255, 255));
        ScrollTooltip.Visibility = Visibility.Visible;

        if (_tooltipTimer is null)
        {
            _tooltipTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1200) };
            _tooltipTimer.Tick += (_, _) =>
            {
                _tooltipTimer.Stop();
                ScrollTooltip.Visibility = Visibility.Collapsed;
            };
        }
        _tooltipTimer.Stop();
        _tooltipTimer.Start();
    }

    private void StyleDot_PointerWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        var delta = e.GetCurrentPoint(null).Properties.MouseWheelDelta;
        if (delta == 0) return;
        var now = DateTime.UtcNow;
        if ((now - _lastScrollTime).TotalMilliseconds < 250) { e.Handled = true; return; }
        _lastScrollTime = now;
        int idx = Array.IndexOf(LlmStyles, _vm.LlmStyle);
        if (idx < 0) idx = 0;
        idx = (idx + (delta > 0 ? -1 : 1) + LlmStyles.Length) % LlmStyles.Length;
        _vm.LlmStyle = LlmStyles[idx];
        StyleDot.Fill = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
        ShowScrollTooltip(_vm.LlmStyle == "off" ? "Off" : _vm.LlmStyle);
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
        var now = DateTime.UtcNow;
        if ((now - _lastScrollTime).TotalMilliseconds < 250) { e.Handled = true; return; }
        _lastScrollTime = now;
        // Pill cycles the OUTPUT translation target, NOT the STT input
        // language. Input language stays set in Settings → General
        // (`Language`); the pill writes to `llm_translate_to` so the
        // transcript gets translated to the picked language at LLM
        // post-process time.
        int idx = LangList.FindIndex(kv => kv.Key == _vm.LlmTranslateTo);
        if (idx < 0) idx = 0;
        idx = (idx + (delta > 0 ? -1 : 1) + LangList.Count) % LangList.Count;
        _vm.LlmTranslateTo = LangList[idx].Key;
        LanguageLabel.Text = string.IsNullOrEmpty(_vm.LlmTranslateTo)
            ? "—"
            : _vm.LlmTranslateTo.ToUpperInvariant();
        ShowScrollTooltip($"Translate to: {LangList[idx].Value}");
        // Single writer: only FFI, Rust saves to disk
        DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(
            new System.Collections.Generic.Dictionary<string, string> { ["llm_translate_to"] = _vm.LlmTranslateTo }));
        e.Handled = true;
    }

    // ── Actions ─────────────────────────────────────────────────────
    private void Pill_RightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        ShowContextMenu((FrameworkElement)sender, e.GetPosition((UIElement)sender));
        e.Handled = true;
    }

    /// <summary>Show a modern WinUI 3 context menu. Called from pill right-click and tray icon.</summary>
    public void ShowContextMenu(FrameworkElement? anchor = null, global::Windows.Foundation.Point? position = null)
    {
        var menu = new MenuFlyout();

        // Status
        var statusText = _vm.IsRecording ? "● Recording..." : "● Ready";
        var statusItem = new MenuFlyoutItem
        {
            Text = statusText,
            IsEnabled = false,
            Icon = new FontIcon
            {
                Glyph = "\uEA3B", // StatusCircleInner
                Foreground = new SolidColorBrush(_vm.IsRecording
                    ? global::Windows.UI.Color.FromArgb(255, 239, 68, 68)   // red
                    : global::Windows.UI.Color.FromArgb(255, 74, 222, 128)) // green
            }
        };
        menu.Items.Add(statusItem);
        menu.Items.Add(new MenuFlyoutSeparator());

        // Native input lang stays read-only (it's an STT setting, lives in Settings → Voice).
        var nativeLang = string.IsNullOrEmpty(_vm.Language) ? "(auto)" : _vm.Language;
        menu.Items.Add(new MenuFlyoutItem { Text = $"Native: {nativeLang}", IsEnabled = false });

        // Translate-to and Style become submenus so the user can change
        // them without opening the pill, scrolling the wheel, or going
        // to Settings. Same persistence path as the scroll handlers
        // (single writer: dimmy_set_config_json → Rust saves to disk).
        menu.Items.Add(BuildTranslateToSubmenu());
        menu.Items.Add(BuildStyleSubmenu());

        menu.Items.Add(new MenuFlyoutItem { Text = $"Shortcut: {_vm.Shortcut}", IsEnabled = false });
        menu.Items.Add(new MenuFlyoutSeparator());

        // Actions
        var toggleItem = new MenuFlyoutItem { Text = "Show/Hide Pill" };
        toggleItem.Click += (_, _) => App.Instance?.TogglePill();
        menu.Items.Add(toggleItem);

        var meetingItem = new MenuFlyoutItem { Text = "Open Meeting…" };
        meetingItem.Click += (_, _) => App.Instance?.OpenMeetingWindow();
        menu.Items.Add(meetingItem);

        var settingsItem = new MenuFlyoutItem { Text = "Settings..." };
        settingsItem.Click += (_, _) => App.Instance?.OpenSettingsWindow();
        menu.Items.Add(settingsItem);

        menu.Items.Add(new MenuFlyoutSeparator());

        var quitItem = new MenuFlyoutItem { Text = "Quit Dimmy" };
        quitItem.Click += (_, _) => App.Instance?.QuitApp();
        menu.Items.Add(quitItem);

        // Show at anchor or at center of pill
        var target = anchor ?? ColorBorder;
        var pos = position ?? new global::Windows.Foundation.Point(
            target.ActualWidth / 2, target.ActualHeight / 2);
        menu.ShowAt(target, pos);
    }

    private MenuFlyoutSubItem BuildTranslateToSubmenu()
    {
        var translateTo = string.IsNullOrEmpty(_vm.LlmTranslateTo) ? "(none)" : _vm.LlmTranslateTo;
        var sub = new MenuFlyoutSubItem { Text = $"Translate to: {translateTo}" };
        foreach (var kv in LangList)
        {
            var item = new ToggleMenuFlyoutItem
            {
                Text = string.IsNullOrEmpty(kv.Key) ? kv.Value : $"{kv.Value}",
                IsChecked = kv.Key == _vm.LlmTranslateTo,
            };
            var code = kv.Key;
            item.Click += (_, _) =>
            {
                _vm.LlmTranslateTo = code;
                LanguageLabel.Text = string.IsNullOrEmpty(code) ? "—" : code.ToUpperInvariant();
                DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(
                    new System.Collections.Generic.Dictionary<string, string> { ["llm_translate_to"] = code }));
            };
            sub.Items.Add(item);
        }
        return sub;
    }

    private MenuFlyoutSubItem BuildStyleSubmenu()
    {
        var style = _vm.LlmStyle == "off" ? "off" : _vm.LlmStyle;
        var sub = new MenuFlyoutSubItem { Text = $"Style: {style}" };
        foreach (var s in LlmStyles)
        {
            var item = new ToggleMenuFlyoutItem
            {
                Text = s,
                IsChecked = s == _vm.LlmStyle,
            };
            var styleValue = s;
            item.Click += (_, _) =>
            {
                _vm.LlmStyle = styleValue;
                StyleDot.Fill = new SolidColorBrush(ParseColor(_vm.LlmStyleColor));
                DimmyNative.dimmy_set_config_json(System.Text.Json.JsonSerializer.Serialize(
                    new System.Collections.Generic.Dictionary<string, object>
                    {
                        ["llm_style"] = styleValue,
                        ["llm_enabled"] = styleValue != "off"
                    }));
            };
            sub.Items.Add(item);
        }
        return sub;
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
