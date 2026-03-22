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
    private static extern int DwmExtendFrameIntoClientArea(IntPtr hwnd, ref MARGINS margins);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter,
        int X, int Y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int nIndex);

    [StructLayout(LayoutKind.Sequential)]
    private struct MARGINS { public int left, right, top, bottom; }

    private static readonly IntPtr HWND_TOPMOST = new(-1);
    private const uint SWP_NOMOVE = 0x0002;
    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_SHOWWINDOW = 0x0040;
    private const int SM_CXSCREEN = 0;
    private const int SM_CYSCREEN = 1;

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

    public static void SetTopmost(Window window)
    {
        var hwnd = GetHwnd(window);
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    }

    public static void EnableTransparency(Window window)
    {
        var hwnd = GetHwnd(window);
        var margins = new MARGINS { left = -1, right = -1, top = -1, bottom = -1 };
        DwmExtendFrameIntoClientArea(hwnd, ref margins);
    }

    public static void PositionBottomRight(Window window, int width, int height, int margin = 100)
    {
        var screenW = GetSystemMetrics(SM_CXSCREEN);
        var screenH = GetSystemMetrics(SM_CYSCREEN);
        var appWindow = GetAppWindow(window);
        appWindow?.MoveAndResize(new global::Windows.Graphics.RectInt32(
            screenW - width - margin,
            screenH - height - margin,
            width, height));
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
}
