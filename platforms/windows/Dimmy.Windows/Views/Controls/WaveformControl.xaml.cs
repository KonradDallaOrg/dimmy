using System;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;

namespace Dimmy.Windows.Views.Controls;

public sealed partial class WaveformControl : UserControl
{
    private readonly Border[] _bars;
    private readonly Ellipse[] _dots;
    private readonly double[] _barWeights = [0.3, 0.5, 0.7, 1.0, 0.7, 0.5, 0.3];
    private readonly double[] _dotWeights = [0.4, 0.7, 1.0, 0.7, 0.4];
    private readonly double[] _currentHeights;
    private readonly double[] _dotSizes;
    private readonly double[] _linePoints; // 9 sample points for polyline
    private readonly Random _rng = new();
    private DispatcherTimer? _timer;
    private bool _timerHandlerAttached;
    private Polyline? _polyline;

    private const double BarMinHeight = 3.0;
    private const double BarMaxHeight = 16.0;
    private const double DotMinSize = 3.0;
    private const double DotMaxSize = 10.0;

    public static readonly DependencyProperty AmplitudeProperty =
        DependencyProperty.Register(nameof(Amplitude), typeof(float),
            typeof(WaveformControl), new PropertyMetadata(0.0f));

    public float Amplitude
    {
        get => (float)GetValue(AmplitudeProperty);
        set => SetValue(AmplitudeProperty, value);
    }

    public static readonly DependencyProperty IsActiveProperty =
        DependencyProperty.Register(nameof(IsActive), typeof(bool),
            typeof(WaveformControl), new PropertyMetadata(false, OnIsActiveChanged));

    public bool IsActive
    {
        get => (bool)GetValue(IsActiveProperty);
        set => SetValue(IsActiveProperty, value);
    }

    public static readonly DependencyProperty StyleModeProperty =
        DependencyProperty.Register(nameof(StyleMode), typeof(string),
            typeof(WaveformControl), new PropertyMetadata("Bars", OnStyleModeChanged));

    public string StyleMode
    {
        get => (string)GetValue(StyleModeProperty);
        set => SetValue(StyleModeProperty, value);
    }

    public WaveformControl()
    {
        this.InitializeComponent();
        _bars = [Bar0, Bar1, Bar2, Bar3, Bar4, Bar5, Bar6];
        _dots = [Dot0, Dot1, Dot2, Dot3, Dot4];
        _currentHeights = new double[7];
        _dotSizes = new double[5];
        _linePoints = new double[9];
        for (int i = 0; i < 7; i++) _currentHeights[i] = BarMinHeight;
        for (int i = 0; i < 5; i++) _dotSizes[i] = DotMinSize;
        for (int i = 0; i < 9; i++) _linePoints[i] = 9.0; // center Y

        BuildPolyline();
    }

    private void BuildPolyline()
    {
        _polyline = new Polyline
        {
            Stroke = new SolidColorBrush(global::Windows.UI.Color.FromArgb(230, 255, 255, 255)),
            StrokeThickness = 2,
            StrokeLineJoin = PenLineJoin.Round,
            StrokeStartLineCap = PenLineCap.Round,
            StrokeEndLineCap = PenLineCap.Round,
        };
        // 9 points across the 40px width
        for (int i = 0; i < 9; i++)
            _polyline.Points.Add(new global::Windows.Foundation.Point(i * 5.0, 9.0));
        LineCanvas.Children.Add(_polyline);
    }

    private static void OnIsActiveChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        var control = (WaveformControl)d;
        if ((bool)e.NewValue) control.StartAnimation();
        else control.StopAnimation();
    }

    private static void OnStyleModeChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        var control = (WaveformControl)d;
        control.ApplyStyle();
    }

    private void ApplyStyle()
    {
        var mode = StyleMode ?? "Bars";

        // Show/hide panels
        BarsPanel.Visibility = Visibility.Collapsed;
        LineCanvas.Visibility = Visibility.Collapsed;
        DotsPanel.Visibility = Visibility.Collapsed;

        switch (mode)
        {
            case "Bars Center":
                BarsPanel.Visibility = Visibility.Visible;
                for (int i = 0; i < _bars.Length; i++)
                {
                    _bars[i].Width = 3;
                    _bars[i].CornerRadius = new CornerRadius(1.5);
                    _bars[i].VerticalAlignment = VerticalAlignment.Center;
                }
                break;
            case "Bars Round":
                BarsPanel.Visibility = Visibility.Visible;
                for (int i = 0; i < _bars.Length; i++)
                {
                    _bars[i].Width = 4;
                    _bars[i].CornerRadius = new CornerRadius(2);
                    _bars[i].VerticalAlignment = VerticalAlignment.Bottom;
                }
                break;
            case "Line":
                LineCanvas.Visibility = Visibility.Visible;
                break;
            case "Dots":
                DotsPanel.Visibility = Visibility.Visible;
                break;
            default: // "Bars"
                BarsPanel.Visibility = Visibility.Visible;
                for (int i = 0; i < _bars.Length; i++)
                {
                    _bars[i].Width = 3;
                    _bars[i].CornerRadius = new CornerRadius(1.5);
                    _bars[i].VerticalAlignment = VerticalAlignment.Bottom;
                }
                break;
        }
    }

    private void StartAnimation()
    {
        if (_timer is null)
            _timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 24) };
        if (!_timerHandlerAttached)
        {
            _timer.Tick += Timer_Tick;
            _timerHandlerAttached = true;
        }
        _timer.Start();
    }

    private void StopAnimation()
    {
        _timer?.Stop();
        for (int i = 0; i < _bars.Length; i++)
        {
            _currentHeights[i] = BarMinHeight;
            _bars[i].Height = BarMinHeight;
        }
        for (int i = 0; i < _dots.Length; i++)
        {
            _dotSizes[i] = DotMinSize;
            _dots[i].Width = DotMinSize;
            _dots[i].Height = DotMinSize;
        }
        if (_polyline != null)
            for (int i = 0; i < 9; i++)
            {
                _linePoints[i] = 9.0;
                _polyline.Points[i] = new global::Windows.Foundation.Point(i * 5.0, 9.0);
            }
    }

    private void Timer_Tick(object? sender, object e)
    {
        float amp = Math.Clamp(Amplitude, 0f, 1f);
        var mode = StyleMode ?? "Bars";

        if (mode == "Line")
            UpdateLine(amp);
        else if (mode == "Dots")
            UpdateDots(amp);
        else
            UpdateBars(amp);
    }

    private void UpdateBars(float amp)
    {
        for (int i = 0; i < _bars.Length; i++)
        {
            double jitter = 0.7 + _rng.NextDouble() * 0.6;
            double target = BarMinHeight + (amp * _barWeights[i] * jitter * (BarMaxHeight - BarMinHeight));
            target = Math.Clamp(target, BarMinHeight, BarMaxHeight);
            _currentHeights[i] += (target - _currentHeights[i]) * 0.4;
            _bars[i].Height = _currentHeights[i];
        }
    }

    private void UpdateLine(float amp)
    {
        if (_polyline == null) return;
        // 9 sample points, weights peak in center
        double[] weights = [0.2, 0.4, 0.6, 0.8, 1.0, 0.8, 0.6, 0.4, 0.2];
        for (int i = 0; i < 9; i++)
        {
            double jitter = 0.6 + _rng.NextDouble() * 0.8;
            double maxDeflection = 7.0; // max pixels from center
            double target = 9.0 + (amp * weights[i] * jitter * maxDeflection * (_rng.NextDouble() > 0.5 ? 1 : -1));
            target = Math.Clamp(target, 1.0, 17.0);
            _linePoints[i] += (target - _linePoints[i]) * 0.35;
            _polyline.Points[i] = new global::Windows.Foundation.Point(i * 5.0, _linePoints[i]);
        }
    }

    private void UpdateDots(float amp)
    {
        for (int i = 0; i < _dots.Length; i++)
        {
            double jitter = 0.7 + _rng.NextDouble() * 0.6;
            double target = DotMinSize + (amp * _dotWeights[i] * jitter * (DotMaxSize - DotMinSize));
            target = Math.Clamp(target, DotMinSize, DotMaxSize);
            _dotSizes[i] += (target - _dotSizes[i]) * 0.35;
            _dots[i].Width = _dotSizes[i];
            _dots[i].Height = _dotSizes[i];
            _dots[i].Opacity = 0.5 + (_dotSizes[i] / DotMaxSize) * 0.5;
        }
    }
}
