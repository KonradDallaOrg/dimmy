using System;
using System.Runtime.InteropServices;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using WinRT;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// A SystemBackdrop that makes the window background fully transparent.
/// Only XAML content is visible; the rest is see-through to the desktop.
///
/// Technique: DwmEnableBlurBehindWindow enables DWM composition on the window,
/// then we set a zero-alpha CompositionColorBrush as the SystemBackdrop via
/// ICompositionSupportsSystemBackdrop. WM_ERASEBKGND is handled to fill the
/// client area with black (which DWM composites as transparent when blur-behind
/// is enabled).
/// </summary>
public class TransparentBackdrop : SystemBackdrop
{
    private global::Windows.UI.Composition.CompositionColorBrush? _brush;
    private IntPtr _hwnd;
    private IntPtr _backgroundBrush;
    private SubclassProc? _subclassDelegate;

    /// <summary>Set the HWND before the backdrop connects (call from Window constructor).</summary>
    public IntPtr Hwnd { get; set; }

    protected override void OnTargetConnected(ICompositionSupportsSystemBackdrop connectedTarget, XamlRoot xamlRoot)
    {
        try
        {
            // Use the HWND set externally, or try to get it from XamlRoot
            if (Hwnd != IntPtr.Zero)
                _hwnd = Hwnd;
            else
            {
                // Convert AppWindowId to HWND
                var appWindowId = xamlRoot.ContentIslandEnvironment.AppWindowId;
                _hwnd = (IntPtr)Microsoft.UI.Win32Interop.GetWindowFromWindowId(
                    new Microsoft.UI.WindowId { Value = appWindowId.Value });
            }

            System.Diagnostics.Debug.WriteLine($"[TransparentBackdrop] HWND = {_hwnd}");

            // Install window subclass to handle WM_ERASEBKGND
            _subclassDelegate = WndProc;
            SetWindowSubclass(_hwnd, _subclassDelegate, UIntPtr.Zero, IntPtr.Zero);

            // Enable DWM blur-behind with a tiny region
            ConfigureDwm(_hwnd);

            // Get the Microsoft.UI compositor, then cast to Windows.UI.Composition.Compositor
            var muiCompositor = Microsoft.UI.Xaml.Media.CompositionTarget.GetCompositorForCurrentThread();
            var compositor = muiCompositor.As<global::Windows.UI.Composition.Compositor>();

            // Create a fully transparent composition brush
            _brush = compositor.CreateColorBrush(global::Windows.UI.Color.FromArgb(0, 0, 0, 0));
            connectedTarget.SystemBackdrop = _brush;

            base.OnTargetConnected(connectedTarget, xamlRoot);

            // Clear the background immediately
            var hdc = GetDC(_hwnd);
            ClearBackground(_hwnd, hdc);
            ReleaseDC(_hwnd, hdc);

            System.Diagnostics.Debug.WriteLine("[TransparentBackdrop] Connected successfully");
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[TransparentBackdrop] ERROR: {ex.GetType().Name}: {ex.Message}");
            System.Diagnostics.Debug.WriteLine(ex.StackTrace);
            // Don't crash — just fall back to default opaque background
            try { base.OnTargetConnected(connectedTarget, xamlRoot); } catch { }
        }
    }

    protected override void OnTargetDisconnected(ICompositionSupportsSystemBackdrop disconnectedTarget)
    {
        // Clean up subclass
        if (_subclassDelegate != null)
        {
            RemoveWindowSubclass(_hwnd, _subclassDelegate, UIntPtr.Zero);
            _subclassDelegate = null;
        }

        // Clean up brush
        var backdrop = disconnectedTarget.SystemBackdrop;
        disconnectedTarget.SystemBackdrop = null;
        backdrop?.Dispose();
        _brush?.Dispose();
        _brush = null;

        // Clean up GDI brush
        if (_backgroundBrush != IntPtr.Zero)
        {
            DeleteObject(_backgroundBrush);
            _backgroundBrush = IntPtr.Zero;
        }

        base.OnTargetDisconnected(disconnectedTarget);
    }

    protected override void OnDefaultSystemBackdropConfigurationChanged(
        ICompositionSupportsSystemBackdrop target, XamlRoot xamlRoot)
    {
        if (target != null)
            base.OnDefaultSystemBackdropConfigurationChanged(target, xamlRoot);
    }

    private static void ConfigureDwm(IntPtr hwnd)
    {
        // Extend glass frame into entire client area — required for transparency.
        // Use explicit 1-pixel margins instead of -1 ("sheet of glass") to avoid
        // DWM adding a shadow around the glass frame.
        var margins = new MARGINS { left = 1, right = 1, top = 1, bottom = 1 };
        DwmExtendFrameIntoClientArea(hwnd, ref margins);

        // Enable blur-behind with a tiny (essentially empty) region.
        // This tells DWM to composite the window, making the black-filled
        // client area transparent, without adding a visible blur or shadow.
        var rgn = CreateRectRgn(-2, -2, -1, -1);
        try
        {
            var bb = new DWM_BLURBEHIND
            {
                dwFlags = DWM_BB_ENABLE | DWM_BB_BLURREGION,
                fEnable = true,
                hRgnBlur = rgn,
                fTransitionOnMaximized = false,
            };
            DwmEnableBlurBehindWindow(hwnd, ref bb);
        }
        finally
        {
            if (rgn != IntPtr.Zero)
                DeleteObject(rgn);
        }
    }

    private bool ClearBackground(IntPtr hwnd, IntPtr hdc)
    {
        if (GetClientRect(hwnd, out var rect))
        {
            if (_backgroundBrush == IntPtr.Zero)
                _backgroundBrush = CreateSolidBrush(0); // Black = RGB(0,0,0)
            FillRect(hdc, ref rect, _backgroundBrush);
            return true;
        }
        return false;
    }

    private IntPtr WndProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam,
        UIntPtr uIdSubclass, IntPtr dwRefData)
    {
        const uint WM_ERASEBKGND = 0x0014;
        const uint WM_DWMCOMPOSITIONCHANGED = 0x031E; // 798

        if (uMsg == WM_ERASEBKGND)
        {
            if (ClearBackground(hWnd, wParam))
                return (IntPtr)1;
        }
        else if (uMsg == WM_DWMCOMPOSITIONCHANGED)
        {
            // Re-apply DWM settings if composition changes (e.g., RDP session)
            ConfigureDwm(hWnd);
            return IntPtr.Zero;
        }

        return DefSubclassProc(hWnd, uMsg, wParam, lParam);
    }

    // -- P/Invoke declarations --

    private delegate IntPtr SubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam,
        IntPtr lParam, UIntPtr uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll", SetLastError = true)]
    private static extern bool SetWindowSubclass(IntPtr hWnd, SubclassProc pfnSubclass,
        UIntPtr uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll")]
    private static extern bool RemoveWindowSubclass(IntPtr hWnd, SubclassProc pfnSubclass,
        UIntPtr uIdSubclass);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg,
        IntPtr wParam, IntPtr lParam);

    [DllImport("dwmapi.dll")]
    private static extern int DwmExtendFrameIntoClientArea(IntPtr hwnd, ref MARGINS margins);

    [DllImport("dwmapi.dll")]
    private static extern int DwmEnableBlurBehindWindow(IntPtr hwnd, ref DWM_BLURBEHIND pBlurBehind);

    [DllImport("user32.dll")]
    private static extern IntPtr GetDC(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern int ReleaseDC(IntPtr hWnd, IntPtr hDC);

    [DllImport("user32.dll")]
    private static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    private static extern int FillRect(IntPtr hDC, ref RECT lprc, IntPtr hbr);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateRectRgn(int x1, int y1, int x2, int y2);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateSolidBrush(uint crColor);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(IntPtr hObject);

    // -- Native structs --

    [StructLayout(LayoutKind.Sequential)]
    private struct MARGINS { public int left, right, top, bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct DWM_BLURBEHIND
    {
        public uint dwFlags;
        public bool fEnable;
        public IntPtr hRgnBlur;
        public bool fTransitionOnMaximized;
    }

    private const uint DWM_BB_ENABLE = 0x00000001;
    private const uint DWM_BB_BLURREGION = 0x00000002;
}
