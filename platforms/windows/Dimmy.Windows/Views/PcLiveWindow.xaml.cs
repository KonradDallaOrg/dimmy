using System;
using System.Runtime.InteropServices;
using Dimmy.Windows.Helpers;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace Dimmy.Windows.Views;

/// <summary>
/// EXPERIMENT (exp/pc-actions): the live card for PC commands. One instance,
/// top right of the display under the cursor, fixed size, never takes the
/// focus (WS_EX_NOACTIVATE, like DictToastWindow). <see cref="Update"/> runs
/// on every routed partial; <see cref="Finish"/> shows the outcome and closes
/// the card a few seconds later.
/// </summary>
public sealed partial class PcLiveWindow : Window
{
    private const int WidthDip = 380;
    private const int HeightDip = 236;
    private const int MarginDip = 12;

    private static PcLiveWindow? _current;
    private readonly DispatcherTimer _closeTimer;

    /// <summary>The bar is half full at the live threshold: past the middle,
    /// the router is sure enough to act.</summary>
    public static void Update(string heard, string intent, double margin, double threshold, string plan, string state)
    {
        var w = Ensure();
        w.HeardText.Text = string.IsNullOrEmpty(heard) ? "…" : $"\"{heard}\"";
        w.IntentText.Text = string.IsNullOrEmpty(intent) ? "…" : intent;
        var full = threshold > 0 ? threshold * 2 : 0.07;
        w.MarginBar.Value = Math.Clamp(margin / full * 100, 0, 100);
        w.MarginText.Text = $"{margin:0.000}  (soglia {threshold:0.000})";
        w.PlanText.Text = string.IsNullOrEmpty(plan) ? "non ancora" : plan;
        w.StateText.Text = state;
        w._closeTimer.Stop();
    }

    /// <summary>The command ran (or didn't): show it, then close.</summary>
    public static void Finish(string plan, string state)
    {
        var w = Ensure();
        if (!string.IsNullOrEmpty(plan)) w.PlanText.Text = plan;
        w.StateText.Text = state;
        w._closeTimer.Stop();
        w._closeTimer.Start();
    }

    private static PcLiveWindow Ensure()
    {
        if (_current != null) return _current;
        _current = new PcLiveWindow();
        WindowHelper.GetAppWindow(_current)?.Show(activateWindow: false);
        return _current;
    }

    private PcLiveWindow()
    {
        InitializeComponent();
        Title = "Dimmy PC commands";
        Root.RequestedTheme = ThemeHelper.ResolvedElementTheme();
        SystemBackdrop = new DesktopAcrylicBackdrop();
        SetupWindow();
        Root.Loaded += (_, _) => Place();
        _closeTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(4) };
        _closeTimer.Tick += (_, _) => Close();
        Closed += (_, _) =>
        {
            _closeTimer.Stop();
            if (ReferenceEquals(_current, this)) _current = null;
        };
    }

    private void SetupWindow()
    {
        var appWindow = WindowHelper.GetAppWindow(this)!;
        if (appWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsResizable = false;
            presenter.IsMaximizable = false;
            presenter.IsMinimizable = false;
            presenter.IsAlwaysOnTop = true;
            presenter.SetBorderAndTitleBar(true, false);
        }
        appWindow.IsShownInSwitchers = false;
        var hwnd = WindowHelper.GetHwnd(this);
        long ex = GetWindowLongPtr(hwnd, GWL_EXSTYLE).ToInt64();
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, new IntPtr(ex | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW));
        int round = DWMWCP_ROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref round, sizeof(int));
        appWindow.Move(new global::Windows.Graphics.PointInt32(-32000, -32000));
    }

    private void Place()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow is null) return;
        double scale = Root.XamlRoot?.RasterizationScale ?? 1.0;
        GetCursorPos(out var cursor);
        var area = DisplayArea.GetFromPoint(new global::Windows.Graphics.PointInt32(cursor.X, cursor.Y), DisplayAreaFallback.Primary);
        var work = area.WorkArea;
        int w = (int)Math.Round(WidthDip * scale);
        int h = (int)Math.Round(HeightDip * scale);
        int m = (int)Math.Round(MarginDip * scale);
        appWindow.MoveAndResize(new global::Windows.Graphics.RectInt32(work.X + work.Width - w - m, work.Y + m, w, h));
    }

    private const int GWL_EXSTYLE = -20;
    private const long WS_EX_NOACTIVATE = 0x08000000, WS_EX_TOOLWINDOW = 0x00000080;
    private const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
    private const int DWMWCP_ROUND = 2;

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT p);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern IntPtr SetWindowLongPtr(IntPtr hwnd, int index, IntPtr value);

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);
}
