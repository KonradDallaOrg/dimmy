using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dimmy.Windows.Views.Controls;

public sealed partial class WaveformControl : UserControl
{
    private readonly Border[] _bars;
    private readonly double[] _barWeights = [0.3, 0.5, 0.7, 1.0, 0.7, 0.5, 0.3];
    private readonly double[] _currentHeights;
    private readonly Random _rng = new();
    private DispatcherTimer? _timer;
    private bool _timerHandlerAttached;

    private const double BarMinHeight = 3.0;
    private const double BarMaxHeight = 16.0;
    private const double Smoothing = 0.4; // Balance between reactive and smooth

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

    public WaveformControl()
    {
        this.InitializeComponent();
        _bars = [Bar0, Bar1, Bar2, Bar3, Bar4, Bar5, Bar6];
        _currentHeights = new double[7];
        for (int i = 0; i < 7; i++) _currentHeights[i] = BarMinHeight;
    }

    private static void OnIsActiveChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        var control = (WaveformControl)d;
        if ((bool)e.NewValue) control.StartAnimation();
        else control.StopAnimation();
    }

    private void StartAnimation()
    {
        if (_timer is null)
        {
            _timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(1000.0 / 24) };
        }
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
        // Reset bars to minimum
        for (int i = 0; i < _bars.Length; i++)
        {
            _currentHeights[i] = BarMinHeight;
            _bars[i].Height = BarMinHeight;
        }
    }

    private void Timer_Tick(object? sender, object e)
    {
        float amp = Math.Clamp(Amplitude, 0f, 1f);

        for (int i = 0; i < _bars.Length; i++)
        {
            // Target height: amplitude * weight * max, with jitter range 0.7-1.3
            double jitter = 0.7 + _rng.NextDouble() * 0.6; // 0.7–1.3
            double target = BarMinHeight + (amp * _barWeights[i] * jitter * (BarMaxHeight - BarMinHeight));
            target = Math.Clamp(target, BarMinHeight, BarMaxHeight);

            // Smooth interpolation
            _currentHeights[i] += (target - _currentHeights[i]) * Smoothing;
            _bars[i].Height = _currentHeights[i];
        }
    }
}
