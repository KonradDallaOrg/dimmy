using System;
using System.IO;
using System.Runtime.InteropServices;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Services;

public class TrayService : IDisposable
{
    private readonly AppViewModel _vm;
    private readonly Action _onTogglePill;
    private readonly Action _onSettingsClick;
    private readonly Action _onQuitClick;
    private readonly Action? _onMeetingClick;
    private IntPtr _hwnd;
    private bool _iconAdded;

    // Win32 constants
    private const int WM_APP = 0x8000;
    private const int WM_TRAYICON = WM_APP + 1;
    private const int WM_LBUTTONUP = 0x0202;
    private const int WM_RBUTTONUP = 0x0205;
    private const int NIM_ADD = 0x00;
    private const int NIM_MODIFY = 0x01;
    private const int NIM_DELETE = 0x02;
    private const int NIF_MESSAGE = 0x01;
    private const int NIF_ICON = 0x02;
    private const int NIF_TIP = 0x04;
    private const int NIF_INFO = 0x10;
    private const int NOTIFYICON_VERSION_4 = 4;
    private const int NIM_SETVERSION = 0x04;

    // Menu constants
    private const uint MF_STRING = 0x00;
    private const uint MF_SEPARATOR = 0x0800;
    private const uint MF_CHECKED = 0x08;
    private const uint MF_GRAYED = 0x01;
    private const uint MF_DISABLED = 0x02;
    private const uint MF_OWNERDRAW = 0x0100;
    private const uint TPM_BOTTOMALIGN = 0x0020;
    private const uint TPM_LEFTALIGN = 0x0000;
    private const uint TPM_RETURNCMD = 0x0100;
    private const uint WM_MEASUREITEM = 0x002C;
    private const uint WM_DRAWITEM = 0x002B;

    private const int IDM_TOGGLE = 1;
    private const int IDM_SETTINGS = 2;
    private const int IDM_QUIT = 3;
    private const int IDM_MEETING = 4;
    private const int IDM_COMMAND = 5; // Command Mode toggle
    private const int IDM_STATUS = 99; // owner-drawn status item

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern bool Shell_NotifyIcon(int dwMessage, ref NOTIFYICONDATA lpData);

    [DllImport("user32.dll")]
    private static extern IntPtr LoadImage(IntPtr hInst, string name, uint type,
        int cx, int cy, uint fuLoad);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern uint RegisterWindowMessage(string lpString);

    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int nIndex);

    // Small-icon metrics — DPI-scaled by the OS for the process's DPI
    // awareness. SM_CXSMICON returns 16 at 100%, 24 at 150%, 32 at 200%.
    private const int SM_CXSMICON = 49;
    private const int SM_CYSMICON = 50;

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr hIcon);

    [DllImport("user32.dll")]
    private static extern IntPtr CreatePopupMenu();

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool AppendMenu(IntPtr hMenu, uint uFlags, nuint uIDNewItem, string? lpNewItem);

    [DllImport("user32.dll")]
    private static extern int TrackPopupMenu(IntPtr hMenu, uint uFlags, int x, int y,
        int nReserved, IntPtr hWnd, IntPtr prcRect);

    [DllImport("user32.dll")]
    private static extern bool DestroyMenu(IntPtr hMenu);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT lpPoint);

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }

    // GDI for owner-drawn menu items
    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateSolidBrush(uint crColor);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(IntPtr hObject);

    [DllImport("gdi32.dll")]
    private static extern IntPtr SelectObject(IntPtr hdc, IntPtr h);

    [DllImport("user32.dll")]
    private static extern int FillRect(IntPtr hDC, ref RECT lprc, IntPtr hbr);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int DrawText(IntPtr hDC, string lpchText, int cchText, ref RECT lprc, uint format);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateFont(int nHeight, int nWidth, int nEscapement, int nOrientation,
        int fnWeight, uint fdwItalic, uint fdwUnderline, uint fdwStrikeOut, uint fdwCharSet,
        uint fdwOutputPrecision, uint fdwClipPrecision, uint fdwQuality, uint fdwPitchAndFamily,
        string? lpszFace);

    [DllImport("gdi32.dll")]
    private static extern uint SetTextColor(IntPtr hdc, uint color);

    [DllImport("gdi32.dll")]
    private static extern uint SetBkMode(IntPtr hdc, int mode);

    [DllImport("gdi32.dll")]
    private static extern bool Ellipse(IntPtr hdc, int left, int top, int right, int bottom);

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct MEASUREITEMSTRUCT
    {
        public uint CtlType;
        public uint CtlID;
        public uint itemID;
        public uint itemWidth;
        public uint itemHeight;
        public IntPtr itemData;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct DRAWITEMSTRUCT
    {
        public uint CtlType;
        public uint CtlID;
        public uint itemID;
        public uint itemAction;
        public uint itemState;
        public IntPtr hwndItem;
        public IntPtr hDC;
        public RECT rcItem;
        public IntPtr itemData;
    }

    private const int TRANSPARENT = 1;

    private const uint IMAGE_ICON = 1;
    private const uint LR_LOADFROMFILE = 0x0010;
    private const uint LR_DEFAULTSIZE = 0x0040;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NOTIFYICONDATA
    {
        public int cbSize;
        public IntPtr hWnd;
        public uint uID;
        public uint uFlags;
        public uint uCallbackMessage;
        public IntPtr hIcon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)]
        public string szTip;
        public int dwState;
        public int dwStateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)]
        public string szInfo;
        public uint uVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)]
        public string szInfoTitle;
        public int dwInfoFlags;
        public Guid guidItem;
        public IntPtr hBalloonIcon;
    }

    // Window subclass for receiving tray messages
    private delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("comctl32.dll")]
    private static extern bool SetWindowSubclass(IntPtr hWnd, WndProcDelegate pfnSubclass,
        nuint uIdSubclass, nuint dwRefData);

    [DllImport("comctl32.dll")]
    private static extern bool RemoveWindowSubclass(IntPtr hWnd, WndProcDelegate pfnSubclass,
        nuint uIdSubclass);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);

    private WndProcDelegate? _wndProcDelegate;
    private IntPtr _hIcon;
    private string _currentIconPath = string.Empty;
    private string _currentTip = "Dimmy: Ready";

    // Broadcast message from explorer.exe when the taskbar is (re)created — fired
    // on first login AND after every explorer restart. Without re-adding our
    // NIM_ADD here, killing/restarting explorer wipes the tray entry until the
    // user relaunches Dimmy. Burned 2026-05-30 chasing icon-cache refresh.
    private uint _taskbarCreatedMsg;

    // Win32 standard setting-change broadcast. Windows fires this with
    // lParam = "ImmersiveColorSet" the moment the user toggles light/dark
    // in Settings → Personalization → Colors. We use it to flip the tray
    // icon's tone (black ↔ white silhouette) live, without waiting for
    // the next state transition.
    private const uint WM_SETTINGCHANGE = 0x001A;

    private Action? _onShowMenu;

    public TrayService(
        AppViewModel vm,
        Action onTogglePill,
        Action onSettingsClick,
        Action onQuitClick,
        Action? onMeetingClick = null)
    {
        _vm = vm;
        _onTogglePill = onTogglePill;
        _onSettingsClick = onSettingsClick;
        _onQuitClick = onQuitClick;
        _onMeetingClick = onMeetingClick;
    }

    /// <summary>Set the callback for showing the WinUI 3 context menu from the pill window.</summary>
    public void SetMenuCallback(Action onShowMenu) => _onShowMenu = onShowMenu;

    public void Initialize(IntPtr hwnd)
    {
        _hwnd = hwnd;

        // Subclass window to receive tray messages + the broadcast TaskbarCreated
        // message that fires after every explorer restart.
        _wndProcDelegate = TrayWndProc;
        SetWindowSubclass(_hwnd, _wndProcDelegate, 1, 0);
        _taskbarCreatedMsg = RegisterWindowMessage("TaskbarCreated");

        AddTrayIcon();
    }

    private void AddTrayIcon()
    {
        // Default state: idle / ready. Picks the variant whose tone
        // contrasts with the user's current taskbar chrome — white cloud
        // on a dark taskbar, black cloud on a light taskbar.
        _currentIconPath = ResolveIconPath(IdleIconForCurrentTaskbarTheme(), "dimmy-tray-idle.ico", "dimmy.ico");
        if (_hIcon != IntPtr.Zero) { DestroyIcon(_hIcon); _hIcon = IntPtr.Zero; }
        _hIcon = LoadTrayIconFromPath(_currentIconPath);

        var nid = MakeNid();
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = _hIcon;
        nid.szTip = _currentTip.Length > 127 ? _currentTip[..127] : _currentTip;

        _iconAdded = Shell_NotifyIcon(NIM_ADD, ref nid);
        System.Diagnostics.Debug.WriteLine($"[Tray] Shell_NotifyIcon ADD = {_iconAdded}, hIcon={_hIcon}, hwnd={_hwnd}");
    }

    // Overload for XamlRoot-based init (backwards compat)
    public void Initialize(Microsoft.UI.Xaml.XamlRoot xamlRoot)
    {
        // Get HWND from the pill window instead
        if (App.Instance != null)
        {
            var pillHwnd = GetPillHwnd();
            if (pillHwnd != IntPtr.Zero)
                Initialize(pillHwnd);
        }
    }

    private IntPtr GetPillHwnd()
    {
        // Access pill window HWND through App
        try
        {
            var field = typeof(App).GetField("_pillWindow", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance);
            if (field?.GetValue(App.Instance) is Microsoft.UI.Xaml.Window pillWindow)
                return Helpers.WindowHelper.GetHwnd(pillWindow);
        }
        catch { }
        return IntPtr.Zero;
    }

    private IntPtr TrayWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == WM_TRAYICON)
        {
            int eventId = (int)(lParam & 0xFFFF);
            switch (eventId)
            {
                case WM_LBUTTONUP:
                    _onTogglePill();
                    break;
                case WM_RBUTTONUP:
                    ShowContextMenu();
                    break;
            }
            return IntPtr.Zero;
        }

        if (_taskbarCreatedMsg != 0 && msg == _taskbarCreatedMsg)
        {
            // Explorer just (re)created the tray. Re-register our icon — the
            // previous NIM_ADD was lost with the old taskbar.
            _iconAdded = false;
            AddTrayIcon();
            return IntPtr.Zero;
        }

        if (msg == WM_SETTINGCHANGE && _iconAdded)
        {
            // lParam → null-terminated wide string naming the changed
            // setting class. We only care about ImmersiveColorSet, the
            // category that includes light/dark theme toggles. Filtering
            // here avoids re-loading + NIM_MODIFY on every unrelated
            // SystemParametersInfo broadcast.
            try
            {
                if (lParam != IntPtr.Zero)
                {
                    var name = Marshal.PtrToStringUni(lParam);
                    if (string.Equals(name, "ImmersiveColorSet", StringComparison.OrdinalIgnoreCase))
                    {
                        RefreshIconForTheme();
                    }
                }
            }
            catch
            {
                // Defensive: PtrToStringUni on a non-string lParam would
                // throw. Swallow — we'd just miss one tone flip, the next
                // state transition picks the right icon anyway.
            }
            // Fall through to DefSubclassProc so the system still gets to
            // propagate the broadcast to other subscribers.
        }

        return DefSubclassProc(hWnd, msg, wParam, lParam);
    }

    /// <summary>Re-pick the tray .ico for the current taskbar tone and swap
    /// it in via NIM_MODIFY. Preserves the current state slug — only the
    /// dark/light prefix changes. Cheap enough to call on every theme
    /// broadcast without debouncing.</summary>
    private void RefreshIconForTheme()
    {
        // Derive the state slug from the current icon path so the live
        // flip preserves whatever state the app is in (recording → keeps
        // the red dot, just swaps the cloud tone).
        var current = Path.GetFileName(_currentIconPath);
        string stateSlug = "idle";
        if (!string.IsNullOrEmpty(current))
        {
            // Filename shape: dimmy-tray-{theme}-{state}.ico OR legacy
            // dimmy-tray-{state}.ico — extract the trailing state slug.
            var stem = Path.GetFileNameWithoutExtension(current);
            var parts = stem.Split('-');
            if (parts.Length >= 2) stateSlug = parts[^1];
        }

        var themeSlug = Helpers.ThemeHelper.SystemTaskbarIsDark() ? "dark" : "light";
        var iconFile = $"dimmy-tray-{themeSlug}-{stateSlug}.ico";
        var path = ResolveIconPath(iconFile, $"dimmy-tray-{stateSlug}.ico", "dimmy.ico");
        if (string.IsNullOrEmpty(path) || path == _currentIconPath) return;

        var newHIcon = LoadTrayIconFromPath(path);
        if (newHIcon == IntPtr.Zero) return;

        var oldHIcon = _hIcon;
        _hIcon = newHIcon;
        _currentIconPath = path;

        var nid = MakeNid();
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = _hIcon;
        nid.szTip = _currentTip.Length > 127 ? _currentTip[..127] : _currentTip;
        Shell_NotifyIcon(NIM_MODIFY, ref nid);

        if (oldHIcon != IntPtr.Zero) DestroyIcon(oldHIcon);
    }

    private void ShowContextMenu()
    {
        // The WinUI 3 MenuFlyout the pill window builds is prettier
        // and matches the right-click-on-pill UX, but it requires a
        // FrameworkElement target with non-zero ActualWidth/Height
        // mounted in a live XamlRoot. When the user has hidden the
        // pill (taskbar-only mode → AppWindow.Hide()), ColorBorder
        // exists but is not laid out, so MenuFlyout.ShowAt silently
        // does nothing. Falling back to a native Win32 popup menu in
        // that case gives the user a working tray right-click without
        // having to first un-hide the pill.
        bool pillVisible = App.Instance?.IsPillVisible() ?? false;
        if (pillVisible)
        {
            _onShowMenu?.Invoke();
        }
        else
        {
            ShowWin32ContextMenu();
        }
    }

    /// <summary>
    /// Native Win32 popup menu — used as a fallback when the pill window
    /// is hidden so the user always has a working tray right-click. Less
    /// pretty than the WinUI 3 MenuFlyout (no glyphs, no submenus, no
    /// dark-mode accent) but always functional regardless of XamlRoot
    /// state. Delegates the menu commands back to the same callbacks
    /// the pill MenuFlyout invokes.
    /// </summary>
    private void ShowWin32ContextMenu()
    {
        var menu = CreatePopupMenu();
        if (menu == IntPtr.Zero) return;

        try
        {
            // Status line — disabled (informational), no command id
            // collision possible because IDM_STATUS is unique to this
            // entry. Bullet glyph is ASCII-safe so no font fallback
            // surprises in legacy GDI text rendering.
            string statusLabel = _vm.IsRecording ? "● Recording…" : "● Ready";
            AppendMenu(menu, MF_STRING | MF_GRAYED, (nuint)IDM_STATUS, statusLabel);
            AppendMenu(menu, MF_SEPARATOR, 0, null);

            // The pill is hidden when this code path runs, so the
            // toggle action will SHOW it. Phrase the label that way
            // — "Show Pill" reads better than "Show/Hide" when only
            // one direction is meaningful right now.
            AppendMenu(menu, MF_STRING, (nuint)IDM_TOGGLE, "Show Pill");
            if (_onMeetingClick != null)
            {
                AppendMenu(menu, MF_STRING, (nuint)IDM_MEETING, "Open Meeting…");
            }
            AppendMenu(menu, MF_STRING, (nuint)IDM_SETTINGS, "Settings…");
            // Command Mode toggle — checkmark reflects the current mode so
            // the tray (taskbar-only users) can flip dictation ↔ command
            // without opening the pill.
            AppendMenu(menu,
                MF_STRING | (_vm.CommandMode ? MF_CHECKED : 0),
                (nuint)IDM_COMMAND, "Command (edit selection)");
            AppendMenu(menu, MF_SEPARATOR, 0, null);
            AppendMenu(menu, MF_STRING, (nuint)IDM_QUIT, "Quit Dimmy");

            // Microsoft-recommended tray menu UX: SetForegroundWindow
            // before TrackPopupMenu so the menu has focus and dismisses
            // on click-outside. Without it the menu stays sticky.
            GetCursorPos(out var p);
            SetForegroundWindow(_hwnd);

            int cmd = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
                p.X, p.Y, 0, _hwnd, IntPtr.Zero);

            switch (cmd)
            {
                case IDM_TOGGLE:   _onTogglePill();   break;
                case IDM_MEETING:  _onMeetingClick?.Invoke(); break;
                case IDM_SETTINGS: _onSettingsClick(); break;
                case IDM_COMMAND:  _vm.CommandMode = !_vm.CommandMode; break;
                case IDM_QUIT:     _onQuitClick();    break;
                // 0 = dismissed without selection. IDM_STATUS is
                // grayed and never returnable here.
            }
        }
        finally
        {
            DestroyMenu(menu);
        }
    }

    /// <summary>Idle-state .ico for the user's current taskbar theme. Pulled
    /// out as a one-liner so both AddTrayIcon (initial draw) and UpdateState
    /// (state transitions) agree on the same source of truth.</summary>
    private static string IdleIconForCurrentTaskbarTheme()
    {
        var themeSlug = Helpers.ThemeHelper.SystemTaskbarIsDark() ? "dark" : "light";
        return $"dimmy-tray-{themeSlug}-idle.ico";
    }

    /// <summary>Resolve the first existing path among Assets/{name} or {name}
    /// alongside the EXE. Returns empty string if none of the candidates exist.</summary>
    private static string ResolveIconPath(params string[] fileNames)
    {
        var exeDir = AppContext.BaseDirectory;
        foreach (var fn in fileNames)
        {
            var p1 = Path.Combine(exeDir, "Assets", fn);
            if (File.Exists(p1)) return p1;
            var p2 = Path.Combine(exeDir, fn);
            if (File.Exists(p2)) return p2;
        }
        return string.Empty;
    }

    private IntPtr LoadTrayIconFromPath(string path)
    {
        // Request the DPI-correct small-icon size (16 @100%, 24 @150%,
        // 32 @200%) so LoadImage picks the matching frame out of the
        // multi-size .ico instead of forcing 16 and letting the shell
        // upscale a tiny bitmap.
        int sx = GetSystemMetrics(SM_CXSMICON);
        int sy = GetSystemMetrics(SM_CYSMICON);
        if (sx <= 0) sx = 16;
        if (sy <= 0) sy = 16;

        if (!string.IsNullOrEmpty(path) && File.Exists(path))
        {
            var h = LoadImage(IntPtr.Zero, path, IMAGE_ICON, sx, sy, LR_LOADFROMFILE);
            if (h != IntPtr.Zero) return h;
        }

        // Fallback: generate a tiny .ico and load it
        var fallback = EnsureFallbackIcon();
        if (fallback != null)
        {
            var h = LoadImage(IntPtr.Zero, fallback, IMAGE_ICON, 16, 16,
                LR_LOADFROMFILE | LR_DEFAULTSIZE);
            if (h != IntPtr.Zero) return h;
        }

        return IntPtr.Zero;
    }

    private static string? EnsureFallbackIcon()
    {
        try
        {
            var path = Path.Combine(Path.GetTempPath(), "dimmy_tray.ico");
            if (File.Exists(path)) return path;

            const int size = 16;
            var pixels = new byte[size * size * 4];
            byte cr = 0x41, cg = 0xB0, cb = 0xB1;
            double cx = 7.5, cy = 7.5, rad = 7.0;
            for (int y = 0; y < size; y++)
                for (int x = 0; x < size; x++)
                {
                    double dx = x - cx, dy = y - cy;
                    if (dx * dx + dy * dy <= rad * rad)
                    {
                        int i = (y * size + x) * 4;
                        pixels[i] = cb; pixels[i + 1] = cg; pixels[i + 2] = cr; pixels[i + 3] = 255;
                    }
                }

            using var ms = new MemoryStream();
            using var bw = new BinaryWriter(ms);
            bw.Write((short)0); bw.Write((short)1); bw.Write((short)1);
            bw.Write((byte)size); bw.Write((byte)size); bw.Write((byte)0); bw.Write((byte)0);
            bw.Write((short)1); bw.Write((short)32);
            int dataSize = 40 + pixels.Length;
            bw.Write(dataSize); bw.Write(22);
            bw.Write(40); bw.Write(size); bw.Write(size * 2);
            bw.Write((short)1); bw.Write((short)32);
            bw.Write(0); bw.Write(pixels.Length);
            bw.Write(0); bw.Write(0); bw.Write(0); bw.Write(0);
            for (int y = size - 1; y >= 0; y--)
                bw.Write(pixels, y * size * 4, size * 4);

            File.WriteAllBytes(path, ms.ToArray());
            return path;
        }
        catch { return null; }
    }

    public void UpdateState(string tooltip, string iconPath)
    {
        if (!_iconAdded) return;
        _currentTip = tooltip;
        var nid = MakeNid();
        nid.uFlags = NIF_TIP;
        nid.szTip = tooltip.Length > 127 ? tooltip[..127] : tooltip;
        Shell_NotifyIcon(NIM_MODIFY, ref nid);
    }

    /// <summary>Swap the tray icon to reflect the current app/meeting state —
    /// mirrors the macOS StatusBarController palette (white idle, red recording,
    /// blue transcribing, purple processing, green done, orange meeting paused).
    /// Meeting state takes precedence over dictation state because meetings can
    /// last for minutes/hours while the dictation state stays Idle the whole time.</summary>
    public void UpdateState(AppState state, bool meetingActive, bool meetingPaused, string? tooltip = null)
    {
        if (!_iconAdded) return;

        string stateSlug;
        string tip;
        if (meetingActive && meetingPaused) { stateSlug = "paused";       tip = "Dimmy — Meeting paused"; }
        else if (meetingActive)             { stateSlug = "recording";    tip = "Dimmy — Meeting recording"; }
        else
        {
            switch (state)
            {
                case AppState.Recording:    stateSlug = "recording";    tip = "Dimmy — Recording…"; break;
                case AppState.Transcribing: stateSlug = "transcribing"; tip = "Dimmy — Transcribing…"; break;
                case AppState.Processing:   stateSlug = "processing";   tip = "Dimmy — Processing…"; break;
                case AppState.Completing:   stateSlug = "completing";   tip = "Dimmy — Done"; break;
                default:                    stateSlug = "idle";         tip = "Dimmy — Ready"; break;
            }
        }
        if (tooltip != null) tip = tooltip;

        // Re-resolve the taskbar tone every UpdateState call — cheap
        // (one registry read) and means the icon flips automatically the
        // next time the app changes state after the user toggles theme.
        var themeSlug = Helpers.ThemeHelper.SystemTaskbarIsDark() ? "dark" : "light";
        var iconFile = $"dimmy-tray-{themeSlug}-{stateSlug}.ico";
        // Fallback chain: themed → legacy un-themed (dark only) → EXE icon.
        var path = ResolveIconPath(iconFile, $"dimmy-tray-{stateSlug}.ico", "dimmy.ico");
        if (path == _currentIconPath && tip == _currentTip) return; // no-op

        var newHIcon = LoadTrayIconFromPath(path);
        if (newHIcon == IntPtr.Zero) return; // keep current icon on failure

        var oldHIcon = _hIcon;
        _hIcon = newHIcon;
        _currentIconPath = path;
        _currentTip = tip;

        var nid = MakeNid();
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = _hIcon;
        nid.szTip = tip.Length > 127 ? tip[..127] : tip;
        Shell_NotifyIcon(NIM_MODIFY, ref nid);

        if (oldHIcon != IntPtr.Zero) DestroyIcon(oldHIcon);
    }

    private NOTIFYICONDATA MakeNid()
    {
        var nid = new NOTIFYICONDATA();
        nid.cbSize = Marshal.SizeOf<NOTIFYICONDATA>();
        nid.hWnd = _hwnd;
        nid.uID = 1;
        nid.szTip = "";
        nid.szInfo = "";
        nid.szInfoTitle = "";
        return nid;
    }

    public void Dispose()
    {
        if (_iconAdded)
        {
            var nid = MakeNid();
            Shell_NotifyIcon(NIM_DELETE, ref nid);
            _iconAdded = false;
        }
        if (_hIcon != IntPtr.Zero)
        {
            DestroyIcon(_hIcon);
            _hIcon = IntPtr.Zero;
        }
        if (_hwnd != IntPtr.Zero && _wndProcDelegate != null)
        {
            RemoveWindowSubclass(_hwnd, _wndProcDelegate, 1);
        }
        GC.SuppressFinalize(this);
    }
}
