using System;
using System.IO;
using System.Runtime.InteropServices;
using Dimmy.Windows.Helpers;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Views;

/// <summary>
/// An always-minimized 1×1 window whose only purpose is to register
/// an HWND in the Windows taskbar. The taskbar button it produces:
///   - is the visual anchor for `ITaskbarList3.SetOverlayIcon` (state
///     dot) and `SetProgressState` (colored bar) — see TaskbarService,
///   - lets the user pin Dimmy to the taskbar (right-click → Pin),
///   - forwards left-clicks back to App.TogglePill so the button
///     behaves like the macOS Dock icon.
///
/// We start the window with `SW_SHOWMINNOACTIVE` (no focus steal, no
/// flash) and intercept `WM_SYSCOMMAND/SC_RESTORE` in the subclass
/// proc so the user clicking the taskbar button never actually
/// un-minimizes us — the window stays invisible, only the click event
/// reaches App.
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

    /// <summary>A button in the taskbar thumbnail toolbar was clicked, by id.
    /// The anchor owns the taskbar entry, so Explorer posts WM_COMMAND here and
    /// nowhere else — TaskbarService can register the buttons but cannot hear
    /// them.</summary>
    public event Action<int>? ThumbButtonClicked;

    /// <summary>Explorer created (or recreated, after a restart) our taskbar
    /// button. ThumbBarAddButtons is only legal from this point, and only
    /// ONCE per button set, so registration hangs off this.</summary>
    public event Action? TaskbarButtonCreated;

    public IntPtr Hwnd { get; }

    // Window subclass — must be retained as a field to prevent GC of the delegate.
    private readonly WndProcDelegate? _wndProcDelegate;

    private const uint WM_SYSCOMMAND = 0x0112;
    private const uint WM_COMMAND = 0x0111;
    // Explorer sends this in the HIWORD of WM_COMMAND wParam for a thumb button.
    private const int THBN_CLICKED = 0x1800;
    private uint _taskbarButtonCreatedMsg;
    private const int SC_RESTORE = 0xF120;

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    private const int SW_SHOWMINNOACTIVE = 7;
    private const int SW_HIDE = 0;
    private const int SW_MINIMIZE = 6;

    private delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("comctl32.dll")]
    private static extern bool SetWindowSubclass(IntPtr hWnd, WndProcDelegate pfnSubclass,
        nuint uIdSubclass, nuint dwRefData);

    [DllImport("comctl32.dll")]
    private static extern bool RemoveWindowSubclass(IntPtr hWnd, WndProcDelegate pfnSubclass,
        nuint uIdSubclass);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint RegisterWindowMessage(string lpString);

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

        // Shrink to 1×1. We don't move off-screen because Windows 11
        // treats extreme negative coords as "hidden" and may suppress
        // the taskbar entry — minimization is a stronger signal that
        // gives us the taskbar button reliably.
        var aw = WindowHelper.GetAppWindow(this);
        if (aw is not null)
            aw.Resize(new global::Windows.Graphics.SizeInt32(1, 1));

        // Force WS_EX_APPWINDOW so even if WinUI 3 default style would
        // hide us (e.g. tool window heuristics), we end up in the
        // taskbar.
        WindowHelper.SetTaskbarVisibility(Hwnd, true);

        TrySetWindowIcon();

        // Must be registered BEFORE the subclass can receive it.
        _taskbarButtonCreatedMsg = RegisterWindowMessage("TaskbarButtonCreated");

        _wndProcDelegate = AnchorWndProc;
        SetWindowSubclass(Hwnd, _wndProcDelegate, 1, 0);

        Closed += (_, _) =>
        {
            if (_wndProcDelegate is not null)
                RemoveWindowSubclass(Hwnd, _wndProcDelegate, 1);
        };
    }

    /// <summary>Show the window minimized so its taskbar entry appears
    /// without ever flashing on screen and without stealing focus.</summary>
    public void ActivateAnchor()
    {
        // SW_SHOWMINNOACTIVE = 7: shows window minimized, does not
        // activate it. This is the canonical Win32 way to "register
        // a window in the taskbar without disrupting the user."
        ShowWindow(Hwnd, SW_SHOWMINNOACTIVE);
    }

    private IntPtr AnchorWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        // Fires on first creation AND after an Explorer restart, which is why
        // the buttons are registered here rather than once at startup.
        if (msg != 0 && msg == _taskbarButtonCreatedMsg)
        {
            Dimmy.Windows.App.Log("TaskbarButtonCreated received", "Taskbar");
            try { TaskbarButtonCreated?.Invoke(); }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine(
                    $"[TaskbarAnchor] TaskbarButtonCreated handler threw: {ex.Message}");
            }
        }

        if (msg == WM_COMMAND && (wParam.ToInt64() >> 16 & 0xFFFF) == THBN_CLICKED)
        {
            int id = (int)(wParam.ToInt64() & 0xFFFF);
            try { ThumbButtonClicked?.Invoke(id); }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine(
                    $"[TaskbarAnchor] ThumbButtonClicked handler threw: {ex.Message}");
            }
            return IntPtr.Zero;
        }

        // When the user clicks our taskbar button, Windows sends
        // WM_SYSCOMMAND/SC_RESTORE to un-minimize us. We intercept
        // that, fire TaskbarClicked, and return 0 to suppress the
        // default restore — the window stays invisible.
        if (msg == WM_SYSCOMMAND)
        {
            // Low 4 bits are reserved; mask before comparing.
            int sc = (int)(wParam.ToInt64() & 0xFFF0);
            if (sc == SC_RESTORE)
            {
                try { TaskbarClicked?.Invoke(); }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine(
                        $"[TaskbarAnchor] TaskbarClicked handler threw: {ex.Message}");
                }
                // Re-minimize defensively in case some Explorer
                // version still tries to show us anyway.
                ShowWindow(Hwnd, SW_MINIMIZE);
                return IntPtr.Zero;
            }
        }
        return DefSubclassProc(hWnd, msg, wParam, lParam);
    }

    /// <summary>Set the taskbar button's icon to the gradient edge-to-edge
    /// EXE icon. The taskbar button is the user's primary surface and
    /// reads as "the app" — the gradient cloud is the brand mark. The
    /// monochrome white/black variants are reserved for the system tray
    /// where contrast against the system chrome is the constraint.</summary>
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
