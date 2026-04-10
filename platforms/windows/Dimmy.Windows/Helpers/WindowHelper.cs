using System;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using WinRT.Interop;

namespace Dimmy.Windows.Helpers;

public static class WindowHelper
{
    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int dwAttribute, ref uint pvAttribute, int cbAttribute);

    private const int DWMWA_NCRENDERING_POLICY = 2;
    private const uint DWMNCRP_DISABLED = 1; // disable DWM non-client rendering (removes shadow)
    private const int DWMWA_BORDER_COLOR = 34;
    private const uint DWMWA_COLOR_NONE = 0xFFFFFFFE;
    private const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
    private const uint DWMWCP_DONOTROUND = 1;

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter,
        int X, int Y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int nIndex);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    private const int SW_SHOWNOACTIVATE = 4;

    [DllImport("user32.dll")]
    private static extern nint GetWindowLongPtr(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll")]
    private static extern nint SetWindowLongPtr(IntPtr hWnd, int nIndex, nint dwNewLong);

    [DllImport("user32.dll")]
    private static extern nint GetClassLongPtr(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll")]
    private static extern nint SetClassLongPtr(IntPtr hWnd, int nIndex, nint dwNewLong);

    private const int GCL_STYLE = -26;
    private const nint CS_DROPSHADOW = 0x00020000;

    private const int GWL_STYLE = -16;
    private const int GWL_EXSTYLE = -20;
    private static readonly nint WS_POPUP = (nint)0x80000000L;
    private const nint WS_CAPTION = 0x00C00000;
    private const nint WS_THICKFRAME = 0x00040000;
    private const nint WS_SYSMENU = 0x00080000;
    private const nint WS_OVERLAPPEDWINDOW = WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | 0x00010000 | 0x00020000;
    private const nint WS_EX_TOOLWINDOW = 0x00000080;
    private const nint WS_EX_APPWINDOW = 0x00040000;
    private const nint WS_EX_NOREDIRECTIONBITMAP = 0x00200000;
    private const nint WS_EX_LAYERED = 0x00080000;
    private const nint WS_EX_NOACTIVATE = 0x08000000;

    [DllImport("user32.dll")]
    private static extern bool SetLayeredWindowAttributes(IntPtr hwnd, uint crKey, byte bAlpha, uint dwFlags);
    private const uint LWA_ALPHA = 0x00000002;

    private static readonly IntPtr HWND_TOPMOST = new(-1);
    private const uint SWP_NOMOVE = 0x0002;
    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_SHOWWINDOW = 0x0040;
    private const int SM_CXSCREEN = 0;
    private const int SM_CYSCREEN = 1;

    // Monitor APIs for work-area clamping
    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromWindow(IntPtr hwnd, uint dwFlags);

    [DllImport("user32.dll", CharSet = CharSet.Auto)]
    private static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFO lpmi);

    private const uint MONITOR_DEFAULTTONEAREST = 2;

    [StructLayout(LayoutKind.Sequential)]
    private struct MONITORINFO
    {
        public uint cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public uint dwFlags;
    }

    public static IntPtr GetHwnd(Window window) =>
        WindowNative.GetWindowHandle(window);

    public static AppWindow? GetAppWindow(Window window)
    {
        try
        {
            var hwnd = GetHwnd(window);
            var windowId = Win32Interop.GetWindowIdFromWindow(hwnd);
            return AppWindow.GetFromWindowId(windowId);
        }
        catch
        {
            return null;
        }
    }

    /// <summary>Show a window without stealing focus from the active app.</summary>
    public static void ShowWithoutActivating(Window window)
    {
        var hwnd = GetHwnd(window);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }

    public static void SetTaskbarVisibility(IntPtr hwnd, bool showInTaskbar)
    {
        var exStyle = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        if (showInTaskbar)
        {
            exStyle |= WS_EX_APPWINDOW;
            exStyle &= ~WS_EX_TOOLWINDOW;
        }
        else
        {
            exStyle &= ~WS_EX_APPWINDOW;
            exStyle |= WS_EX_TOOLWINDOW;
        }
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, exStyle);
    }

    public static void SetTopmost(Window window)
    {
        var hwnd = GetHwnd(window);
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    }

    /// <summary>
    /// Configures the window as an overlay: tool window (no taskbar entry),
    /// borderless, no shadow. Transparency is handled by TransparentBackdrop
    /// set as Window.SystemBackdrop in XAML or code-behind.
    /// </summary>
    public static void EnableTransparency(Window window)
    {
        var hwnd = GetHwnd(window);

        // 1. Replace overlapped window style with popup (no border chrome)
        var style = GetWindowLongPtr(hwnd, GWL_STYLE);
        style &= ~WS_OVERLAPPEDWINDOW;
        style |= WS_POPUP;
        SetWindowLongPtr(hwnd, GWL_STYLE, style);

        // 2. Tool window (no taskbar), no redirection bitmap, layered, no-activate
        var exStyle = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        exStyle |= WS_EX_TOOLWINDOW;
        exStyle |= WS_EX_NOREDIRECTIONBITMAP;
        exStyle |= WS_EX_LAYERED;           // layered → DWM skips shadow
        exStyle |= WS_EX_NOACTIVATE;        // never steal focus from active app
        exStyle &= ~WS_EX_APPWINDOW;
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, exStyle);

        // 3. Set layered window to fully opaque — we only want the style flag
        //    to suppress DWM shadow, not to change actual opacity
        SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);

        // 4. Remove CS_DROPSHADOW from window class style
        var classStyle = GetClassLongPtr(hwnd, GCL_STYLE);
        classStyle &= ~CS_DROPSHADOW;
        SetClassLongPtr(hwnd, GCL_STYLE, classStyle);

        // 5. Disable DWM non-client rendering (removes window shadow)
        uint ncrPolicy = DWMNCRP_DISABLED;
        DwmSetWindowAttribute(hwnd, DWMWA_NCRENDERING_POLICY, ref ncrPolicy, sizeof(uint));

        // 6. Remove Windows 11 DWM border color
        uint borderColor = DWMWA_COLOR_NONE;
        DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, ref borderColor, sizeof(uint));

        // 7. Disable Windows 11 rounded corners (their rendering carries a shadow)
        uint cornerPref = DWMWCP_DONOTROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref cornerPref, sizeof(uint));

        // 8. Force redraw with new styles
        SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | 0x0020 /*SWP_FRAMECHANGED*/ | SWP_SHOWWINDOW);
    }

    [DllImport("user32.dll")]
    private static extern bool SystemParametersInfo(uint uiAction, uint uiParam, ref RECT pvParam, uint fWinIni);
    private const uint SPI_GETWORKAREA = 0x0030;

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    public static void PositionBottomRight(Window window, int width, int height, int margin = 20)
    {
        // Use work area (excludes taskbar) instead of full screen
        var workArea = new RECT();
        SystemParametersInfo(SPI_GETWORKAREA, 0, ref workArea, 0);
        var x = workArea.Right - width - margin;
        var y = workArea.Bottom - height - margin;
        var appWindow = GetAppWindow(window);
        appWindow?.MoveAndResize(new global::Windows.Graphics.RectInt32(x, y, width, height));
    }

    public static void PositionAtAnchor(Window window, int width, int height,
        double anchorRight, double anchorBottom)
    {
        var screenW = GetSystemMetrics(SM_CXSCREEN);
        var screenH = GetSystemMetrics(SM_CYSCREEN);
        var x = screenW - width - (int)anchorRight;
        var y = screenH - height - (int)anchorBottom;
        x = Math.Max(0, Math.Min(x, screenW - width));
        y = Math.Max(0, Math.Min(y, screenH - height));
        var appWindow = GetAppWindow(window);
        appWindow?.MoveAndResize(new global::Windows.Graphics.RectInt32(x, y, width, height));
    }

    public static void PositionByPreset(Window window, string preset, int width, int height, int margin = 20)
    {
        var workArea = new RECT();
        SystemParametersInfo(SPI_GETWORKAREA, 0, ref workArea, 0);

        int x, y;
        switch (preset)
        {
            case "Top Left":
                x = workArea.Left + margin;
                y = workArea.Top + margin;
                break;
            case "Top Right":
                x = workArea.Right - width - margin;
                y = workArea.Top + margin;
                break;
            case "Bottom Left":
                x = workArea.Left + margin;
                y = workArea.Bottom - height - margin;
                break;
            case "Bottom Center":
                x = (workArea.Left + workArea.Right) / 2 - width / 2;
                y = workArea.Bottom - height - margin;
                break;
            default: // "Bottom Right"
                x = workArea.Right - width - margin;
                y = workArea.Bottom - height - margin;
                break;
        }

        var appWindow = GetAppWindow(window);
        appWindow?.Move(new global::Windows.Graphics.PointInt32(x, y));
    }

    /// <summary>
    /// Clamps the given window position so the window stays entirely within the
    /// work area (excludes taskbar) of the monitor nearest to the window.
    /// </summary>
    public static global::Windows.Graphics.PointInt32 ClampToWorkArea(
        Window window, int x, int y, int width, int height)
    {
        var hwnd = GetHwnd(window);
        var monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        var mi = new MONITORINFO { cbSize = (uint)Marshal.SizeOf<MONITORINFO>() };
        if (GetMonitorInfo(monitor, ref mi))
        {
            var work = mi.rcWork;
            x = Math.Max(work.Left, Math.Min(x, work.Right - width));
            y = Math.Max(work.Top, Math.Min(y, work.Bottom - height));
        }
        return new global::Windows.Graphics.PointInt32(x, y);
    }
}
