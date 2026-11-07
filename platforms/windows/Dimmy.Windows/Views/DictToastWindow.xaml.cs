using System;
using System.Runtime.InteropServices;
using Dimmy.Windows.Helpers;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace Dimmy.Windows.Views;

/// <summary>
/// Self-dismissing toast, laid out like a Windows 11 notification: bottom
/// right above the taskbar, acrylic, DWM corners and shadow, as tall as its
/// text. It never takes the focus: the user is typing in another app (the
/// dictionary hotkey, command mode) and must keep the keyboard. The first
/// version called Activate() and took it; <c>Show(activateWindow: false)</c>
/// alone is not enough either (a process that just started, or just got a
/// key through the hook, may set the foreground), so the window carries
/// WS_EX_NOACTIVATE, the Win32 style for exactly this. One at a time: a new
/// toast replaces the one on screen. Closes after long enough to read it
/// (<see cref="Services.ToastDuration"/>) or on click.
///
/// Why not <c>Microsoft.Windows.AppNotifications</c>: Dimmy ships
/// unpackaged via Velopack, without the registered ToastActivator CLSID
/// those need, and Show() silently swallows the toast.
/// </summary>
public sealed partial class DictToastWindow : Window
{
    private const int WidthDip = 364;
    private const int MarginDip = 12;

    private static DictToastWindow? _current;
    private readonly DispatcherTimer _closeTimer;

    /// <summary>Show a toast, replacing the one on screen. UI thread only.</summary>
    public static void Present(string title, string body)
    {
        _current?.Close();
        var toast = new DictToastWindow(title, body);
        _current = toast;
        WindowHelper.GetAppWindow(toast)?.Show(activateWindow: false);
    }

    private DictToastWindow(string title, string body)
    {
        InitializeComponent();
        Title = "Dimmy notification";
        TitleText.Text = title;
        BodyText.Text = body;
        // A toast has no parent whose theme would flow down: take the
        // user's choice explicitly. The acrylic follows the root's theme.
        Root.RequestedTheme = ThemeHelper.ResolvedElementTheme();
        SystemBackdrop = new DesktopAcrylicBackdrop();
        SetupWindow();

        // The size depends on the text and on the monitor's scale, which is
        // only known once the window is up: it opens off screen and empty,
        // and Root.Loaded sizes it, places it, then adds the card (which
        // plays the entrance transition).
        Root.Children.Remove(Card);
        Root.Loaded += (_, _) => PlaceAndReveal();

        Root.PointerPressed += (_, _) => Close();
        Closed += (_, _) =>
        {
            _closeTimer?.Stop();
            if (ReferenceEquals(_current, this)) _current = null;
        };

        // Long enough to READ it: a flat 3 s suited four-word dictionary
        // toasts, not "Transcription is falling behind ... try Parakeet"
        // (reported 2026-09-04).
        _closeTimer = new DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(Services.ToastDuration.For(title, body)),
        };
        _closeTimer.Tick += (_, _) => Close();
        _closeTimer.Start();
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
            // Border on, title bar off: Windows 11 then draws the rounded
            // corners, the hairline and the shadow, as for its own flyouts.
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

    private double _wantHeightDip;
    private int _corrections;
    private bool _revealed;

    private void PlaceAndReveal()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow is null) return;
        double scale = Root.XamlRoot?.RasterizationScale ?? 1.0;
        var pad = Root.Padding;
        Card.Measure(new global::Windows.Foundation.Size(WidthDip - pad.Left - pad.Right, double.PositiveInfinity));
        _wantHeightDip = Card.DesiredSize.Height + pad.Top + pad.Bottom;

        // What the frame takes from the outer size depends on the border
        // style and Windows version, so no constants: size, then let
        // Root.SizeChanged say how much room XAML really got and correct by
        // the difference (it arrives after the resize, not during it).
        Root.SizeChanged += OnRootSized;
        appWindow.Resize(new global::Windows.Graphics.SizeInt32(
            (int)Math.Ceiling(WidthDip * scale), (int)Math.Ceiling(_wantHeightDip * scale)));
        // Never stay hidden off screen if no size change comes back.
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, async () =>
        {
            await System.Threading.Tasks.Task.Delay(1000);
            Reveal();
        });
    }

    private void OnRootSized(object sender, SizeChangedEventArgs e)
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow is null || _revealed) return;
        double scale = Root.XamlRoot?.RasterizationScale ?? 1.0;
        int dw = (int)Math.Round((WidthDip - e.NewSize.Width) * scale);
        int dh = (int)Math.Round((_wantHeightDip - e.NewSize.Height) * scale);
        if ((dw != 0 || dh != 0) && ++_corrections <= 3)
        {
            var s = appWindow.Size;
            appWindow.Resize(new global::Windows.Graphics.SizeInt32(s.Width + dw, s.Height + dh));
            return;
        }
        Reveal();
    }

    private void Reveal()
    {
        if (_revealed) return;
        _revealed = true;
        Root.SizeChanged -= OnRootSized;
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow is null) return;
        double scale = Root.XamlRoot?.RasterizationScale ?? 1.0;

        // Bottom right of the work area (taskbar excluded) of the monitor
        // with the mouse, where the system shows its notifications. The
        // margin is measured from the VISIBLE frame: the outer rectangle
        // includes Windows' invisible resize borders.
        GetCursorPos(out var cursor);
        var area = DisplayArea.GetFromPoint(new global::Windows.Graphics.PointInt32(cursor.X, cursor.Y), DisplayAreaFallback.Primary);
        var wa = area.WorkArea;
        int margin = (int)Math.Round(MarginDip * scale);
        var hwnd = WindowHelper.GetHwnd(this);
        GetWindowRect(hwnd, out var outer);
        int right = 0, bottom = 0;
        if (DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, out var visible, Marshal.SizeOf<RECT>()) == 0)
        {
            right = outer.Right - visible.Right;
            bottom = outer.Bottom - visible.Bottom;
        }
        var size = appWindow.Size;
        appWindow.Move(new global::Windows.Graphics.PointInt32(
            wa.X + wa.Width - size.Width + right - margin, wa.Y + wa.Height - size.Height + bottom - margin));
        Root.Children.Add(Card);
    }

    private const int DWMWA_EXTENDED_FRAME_BOUNDS = 9;

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hwnd, out RECT r);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out RECT value, int size);

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
