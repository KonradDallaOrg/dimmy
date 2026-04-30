using System;
using System.IO;
using System.Runtime.InteropServices;
using Dimmy.Windows.Helpers;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Views;

/// <summary>
/// An invisible 1×1 window positioned off-screen whose only purpose
/// is to register an HWND in the Windows taskbar. The taskbar button
/// it produces:
///   - is the visual anchor for `ITaskbarList3.SetOverlayIcon` (state
///     dot) and `SetProgressState` (colored bar) — see TaskbarService,
///   - lets the user pin Dimmy to the taskbar (right-click → Pin),
///   - forwards left-click activations back to App.TogglePill so the
///     button behaves like the macOS Dock icon.
///
/// The actual UI for the app stays in PillWindow + SettingsWindow;
/// this is purely a presence tile.
/// </summary>
public sealed partial class TaskbarAnchorWindow : Window
{
    /// <summary>Raised when the user clicks the taskbar button.
    /// Subscribers typically toggle pill visibility. Named distinctly
    /// from `Window.Activated` (the inherited XAML event) so callers
    /// can't accidentally hook the wrong one.</summary>
    public event Action? TaskbarClicked;

    public IntPtr Hwnd { get; }

    // Window subclass — must be retained as a field to prevent GC of the delegate.
    private readonly WndProcDelegate? _wndProcDelegate;

    private const uint WM_ACTIVATE = 0x0006;
    private const int WA_INACTIVE = 0;

    private delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("comctl32.dll")]
    private static extern bool SetWindowSubclass(IntPtr hWnd, WndProcDelegate pfnSubclass,
        nuint uIdSubclass, nuint dwRefData);

    [DllImport("comctl32.dll")]
    private static extern bool RemoveWindowSubclass(IntPtr hWnd, WndProcDelegate pfnSubclass,
        nuint uIdSubclass);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int SendMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern IntPtr LoadImage(IntPtr hInst, string name, uint type,
        int cx, int cy, uint fuLoad);

    private const uint WM_SETICON = 0x0080;
    private const int ICON_SMALL = 0;
    private const int ICON_BIG = 1;
    private const uint IMAGE_ICON = 1;
    private const uint LR_LOADFROMFILE = 0x0010;
    private const uint LR_DEFAULTSIZE = 0x0040;

    public TaskbarAnchorWindow()
    {
        InitializeComponent();
        Title = "Dimmy";
        Hwnd = WindowHelper.GetHwnd(this);

        // Park off-screen at a guaranteed-not-visible coordinate. The
        // taskbar entry stays even though the window itself is never
        // user-visible — Windows registers the entry on window create,
        // not on first paint.
        var aw = WindowHelper.GetAppWindow(this);
        if (aw is not null)
        {
            aw.Resize(new global::Windows.Graphics.SizeInt32(1, 1));
            aw.Move(new global::Windows.Graphics.PointInt32(-32000, -32000));
        }

        // Force the taskbar to show this window (in case any platform
        // default suppresses it). Without WS_EX_APPWINDOW the chrome-
        // less WinUI 3 windows can be elided from the taskbar.
        WindowHelper.SetTaskbarVisibility(Hwnd, true);

        TrySetWindowIcon();

        _wndProcDelegate = AnchorWndProc;
        SetWindowSubclass(Hwnd, _wndProcDelegate, 1, 0);

        Closed += (_, _) =>
        {
            if (_wndProcDelegate is not null)
                RemoveWindowSubclass(Hwnd, _wndProcDelegate, 1);
        };
    }

    /// <summary>Show the window so its taskbar entry appears, but
    /// without stealing focus from whatever the user is doing.</summary>
    public void ActivateAnchor() => WindowHelper.ShowWithoutActivating(this);

    private IntPtr AnchorWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        // WM_ACTIVATE fires when the user clicks the taskbar button
        // (Windows tries to activate our window). We capture it,
        // forward to App.TogglePill via the event, and let the
        // default proc finish so Windows doesn't get confused.
        if (msg == WM_ACTIVATE)
        {
            int activeFlag = (int)(wParam.ToInt64() & 0xFFFF);
            if (activeFlag != WA_INACTIVE)
            {
                try { TaskbarClicked?.Invoke(); }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"[TaskbarAnchor] TaskbarClicked handler threw: {ex.Message}");
                }
            }
        }
        return DefSubclassProc(hWnd, msg, wParam, lParam);
    }

    /// <summary>Set the taskbar button's icon to dimmy.ico if present.
    /// Falls back silently to the WinUI default icon otherwise — the
    /// overlay state dots still render either way.</summary>
    private void TrySetWindowIcon()
    {
        var exeDir = AppContext.BaseDirectory;
        var paths = new[]
        {
            Path.Combine(exeDir, "Assets", "dimmy.ico"),
            Path.Combine(exeDir, "dimmy.ico"),
        };
        foreach (var path in paths)
        {
            if (!File.Exists(path)) continue;
            var hIconBig = LoadImage(IntPtr.Zero, path, IMAGE_ICON, 32, 32,
                LR_LOADFROMFILE | LR_DEFAULTSIZE);
            var hIconSmall = LoadImage(IntPtr.Zero, path, IMAGE_ICON, 16, 16,
                LR_LOADFROMFILE | LR_DEFAULTSIZE);
            if (hIconBig != IntPtr.Zero)
                SendMessage(Hwnd, WM_SETICON, ICON_BIG, hIconBig);
            if (hIconSmall != IntPtr.Zero)
                SendMessage(Hwnd, WM_SETICON, ICON_SMALL, hIconSmall);
            return;
        }
    }
}
