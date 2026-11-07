using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Helpers;

/// <summary>The dock zones drawn while the pill is dragged: faint when the
/// pill comes close, stronger when dropping it there would dock it. One small
/// window per zone, the plainest Win32 for a translucent shape: a solid class
/// brush, uniform alpha (SetLayeredWindowAttributes), a rounded window region.
/// Click-through and never activated, so the drag and the apps under it are
/// untouched. Win32 only changes when a zone's state does, not on every move.</summary>
internal sealed class DockZoneOverlay : IDisposable
{
    private enum ZoneState { Hidden, Near, Active }

    private const byte NearAlpha = 60;
    private const byte ActiveAlpha = 130;
    // #38BDF8, the pill's "transcribing" blue, as a COLORREF (0x00BBGGRR).
    private const uint ZoneColor = 0x00F8BD38;
    private const string ClassName = "DimmyDockZone";

    private readonly Dictionary<PillDock, IntPtr> _windows = new();
    private readonly Dictionary<PillDock, ZoneState> _states = new();
    private readonly Dictionary<PillDock, ZoneRect> _rects = new();
    private static bool _classRegistered;

    /// <summary>Lay the zones out for one monitor's work area (physical px).
    /// All hidden until <see cref="Show"/>.</summary>
    public void Layout(IReadOnlyList<DockZone> zones, double scale)
    {
        EnsureClass();
        int radius = (int)Math.Round(12 * scale);
        foreach (var z in zones)
        {
            if (_rects.TryGetValue(z.Dock, out var old) && old == z.Rect) continue;
            _rects[z.Dock] = z.Rect;
            int x = (int)Math.Floor(z.Rect.Left), y = (int)Math.Floor(z.Rect.Top);
            int w = (int)Math.Ceiling(z.Rect.Right) - x, h = (int)Math.Ceiling(z.Rect.Bottom) - y;
            if (!_windows.TryGetValue(z.Dock, out var hwnd))
            {
                hwnd = CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                    ClassName, "", WS_POPUP, x, y, w, h, IntPtr.Zero, IntPtr.Zero, GetModuleHandleW(null), IntPtr.Zero);
                if (hwnd == IntPtr.Zero) continue;
                _windows[z.Dock] = hwnd;
                _states[z.Dock] = ZoneState.Hidden;
            }
            else
            {
                SetWindowPos(hwnd, IntPtr.Zero, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            // The system owns the region once it is set.
            SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2), true);
        }
    }

    /// <summary>Show the zones the pill is near; <paramref name="active"/> is
    /// the one it would dock to if dropped now. Kept just below the pill.</summary>
    public void Show(ICollection<PillDock> near, PillDock active, IntPtr pillHwnd)
    {
        foreach (var (dock, hwnd) in _windows)
        {
            var want = dock == active ? ZoneState.Active : near.Contains(dock) ? ZoneState.Near : ZoneState.Hidden;
            if (_states[dock] == want) continue;
            _states[dock] = want;
            if (want == ZoneState.Hidden)
            {
                ShowWindow(hwnd, SW_HIDE);
                continue;
            }
            SetLayeredWindowAttributes(hwnd, 0, want == ZoneState.Active ? ActiveAlpha : NearAlpha, LWA_ALPHA);
            SetWindowPos(hwnd, pillHwnd, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    public void HideAll() => Show(Array.Empty<PillDock>(), PillDock.None, IntPtr.Zero);

    public void Dispose()
    {
        foreach (var hwnd in _windows.Values) DestroyWindow(hwnd);
        _windows.Clear();
        _states.Clear();
        _rects.Clear();
    }

    private static void EnsureClass()
    {
        if (_classRegistered) return;
        var wc = new WNDCLASSEXW
        {
            cbSize = (uint)Marshal.SizeOf<WNDCLASSEXW>(),
            // No managed window procedure: the default one paints the class
            // brush and that is all these windows ever do.
            lpfnWndProc = GetProcAddress(GetModuleHandleW("user32.dll"), "DefWindowProcW"),
            hInstance = GetModuleHandleW(null),
            hbrBackground = CreateSolidBrush(ZoneColor),
            lpszClassName = ClassName,
        };
        _classRegistered = RegisterClassExW(ref wc) != 0;
    }

    private const uint WS_POPUP = 0x80000000;
    private const uint WS_EX_LAYERED = 0x00080000;
    private const uint WS_EX_TRANSPARENT = 0x00000020;
    private const uint WS_EX_TOOLWINDOW = 0x00000080;
    private const uint WS_EX_NOACTIVATE = 0x08000000;
    private const uint WS_EX_TOPMOST = 0x00000008;
    private const uint LWA_ALPHA = 0x2;
    private const int SW_HIDE = 0;
    private const uint SWP_NOSIZE = 0x0001, SWP_NOMOVE = 0x0002, SWP_NOZORDER = 0x0004,
        SWP_NOACTIVATE = 0x0010, SWP_SHOWWINDOW = 0x0040;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WNDCLASSEXW
    {
        public uint cbSize;
        public uint style;
        public IntPtr lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        public string? lpszMenuName;
        public string lpszClassName;
        public IntPtr hIconSm;
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern ushort RegisterClassExW(ref WNDCLASSEXW wc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowExW(uint exStyle, string cls, string title, uint style,
        int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")]
    private static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int w, int h, uint flags);
    [DllImport("user32.dll")]
    private static extern bool SetLayeredWindowAttributes(IntPtr hwnd, uint key, byte alpha, uint flags);
    [DllImport("user32.dll")]
    private static extern int SetWindowRgn(IntPtr hwnd, IntPtr rgn, bool redraw);
    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateRoundRectRgn(int x1, int y1, int x2, int y2, int w, int h);
    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateSolidBrush(uint color);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr GetModuleHandleW(string? name);
    [DllImport("kernel32.dll", CharSet = CharSet.Ansi)]
    private static extern IntPtr GetProcAddress(IntPtr module, string name);
}
