using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace Dimmy.Windows.Services;

/// <summary>
/// Captures a snapshot of the foreground window at hotkey-press time.
/// Two callers, single primitive:
///   1) Diagnostic for the Notepad++ paste bug — comparing the snapshot
///      taken at press to a fresh GetForegroundWindow() at paste time
///      tells us whether focus drifted (Win+Alt menu activation, Game
///      Bar overlay, etc.) between recording start and paste.
///   2) Feature: feeds `app_rules` matcher in core via
///      `dimmy_set_app_context` so the LLM enhance step can apply a
///      per-app style override (Slack=casual, VS Code=technical, ...).
///
/// Privacy boundary:
///   - process_name, class_name, AUMID → categorical, OK to send to Rust
///   - window_title, executable_path → may contain PII (file paths, email
///     subjects, usernames). Kept LOCAL to ptt.log only, never crosses
///     FFI, never reaches PostHog/Sentry.
/// </summary>
public static class AppContextCapture
{
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern int GetClassNameW(IntPtr hWnd, StringBuilder buf, int bufLen);

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextW(IntPtr hWnd, StringBuilder buf, int bufLen);

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextLengthW(IntPtr hWnd);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint desiredAccess, bool inherit, uint processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern bool QueryFullProcessImageNameW(
        IntPtr hProcess, uint dwFlags, StringBuilder lpExeName, ref uint lpdwSize);

    /// PROCESS_QUERY_LIMITED_INFORMATION — minimal rights, works
    /// cross-elevation when target is elevated and we are not.
    /// Process.MainModule throws Win32Exception in that scenario.
    private const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;

    [StructLayout(LayoutKind.Sequential)]
    private struct GUITHREADINFO
    {
        public int cbSize;
        public uint flags;
        public IntPtr hwndActive;
        public IntPtr hwndFocus;     // ← the actual focused control inside the active window
        public IntPtr hwndCapture;
        public IntPtr hwndMenuOwner;
        public IntPtr hwndMoveSize;
        public IntPtr hwndCaret;
        public RECT rcCaret;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int left, top, right, bottom; }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetGUIThreadInfo(uint idThread, ref GUITHREADINFO lpgui);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int vKey);

    private const int VK_LWIN = 0x5B;
    private const int VK_RWIN = 0x5C;
    private const int VK_MENU = 0x12;     // Alt
    private const int VK_LMENU = 0xA4;
    private const int VK_RMENU = 0xA5;
    private const int VK_CONTROL = 0x11;
    private const int VK_SHIFT = 0x10;

    /// <summary>
    /// Snapshot of the foreground window — what the user was looking at
    /// when they pressed the hotkey. Populated once at press; consumed
    /// at paste for restore + by app_rules in core for style routing.
    /// All string fields default to "" (never null) so JSON serialisation
    /// and equality comparisons are predictable.
    /// </summary>
    public sealed record CapturedTargetContext(
        IntPtr Hwnd,
        int Pid,
        string ProcessName,
        string ExecutablePath,
        string ClassName,
        string WindowTitle,
        DateTimeOffset CapturedAt,
        IntPtr HwndFocus = default,
        string KeyState = "")
    {
        public static readonly CapturedTargetContext Empty = new(
            IntPtr.Zero, 0, "", "", "", "", DateTimeOffset.MinValue);

        public bool IsEmpty => Hwnd == IntPtr.Zero && Pid == 0;

        /// JSON shape consumed by `dimmy_set_app_context`. Never includes
        /// title/path — those stay local to ptt.log per privacy contract.
        public string ToCoreJson() =>
            JsonSerializer.Serialize(new
            {
                process_name = ProcessName,
                bundle_id = "",
                wm_class = "",
            });

        /// Single-line summary safe for ptt.log (includes title for debug).
        public string ToLogString()
        {
            var s = $"hwnd=0x{Hwnd.ToInt64():X} pid={Pid} proc='{ProcessName}' " +
                    $"class='{ClassName}' title='{Truncate(WindowTitle, 80)}'";
            if (HwndFocus != IntPtr.Zero && HwndFocus != Hwnd)
                s += $" focus=0x{HwndFocus.ToInt64():X}";
            if (!string.IsNullOrEmpty(KeyState))
                s += $" mods=[{KeyState}]";
            return s;
        }

        private static string Truncate(string s, int max) =>
            s.Length <= max ? s : s.Substring(0, max - 1) + "…";
    }

    /// <summary>
    /// Take a snapshot of whatever window currently has foreground.
    /// Best-effort: any sub-call failure (process exited, access denied,
    /// secure desktop) yields an empty field rather than a thrown
    /// exception — the hot path must never break recording.
    /// </summary>
    public static CapturedTargetContext SnapshotForeground()
    {
        var hwnd = GetForegroundWindow();
        if (hwnd == IntPtr.Zero)
            return CapturedTargetContext.Empty;

        var threadId = GetWindowThreadProcessId(hwnd, out uint pid);
        if (pid == 0)
            return CapturedTargetContext.Empty with { Hwnd = hwnd, CapturedAt = DateTimeOffset.UtcNow };

        var className = ReadClassName(hwnd);
        var title = ReadWindowTitle(hwnd);
        var (procName, exePath) = ReadProcessImage(pid);

        // Focus child within the foreground window. Different from the
        // top-level HWND when the menu bar / titlebar / a child control
        // has focus instead of the document area. ALT-up notoriously
        // shifts this to the menu bar in legacy Win32 apps.
        IntPtr hwndFocus = IntPtr.Zero;
        try
        {
            var gti = new GUITHREADINFO { cbSize = Marshal.SizeOf<GUITHREADINFO>() };
            if (GetGUIThreadInfo(threadId, ref gti))
                hwndFocus = gti.hwndFocus;
        }
        catch { /* best-effort */ }

        // Snapshot the modifier-key state at this exact moment. Stuck
        // Win/Alt at PASTE time would explain SendInput Ctrl+V being
        // re-interpreted as Win+Ctrl+V or Alt+Ctrl+V (which goes to
        // OS-level keybindings rather than the focused app).
        var keyState = SnapshotModifierState();

        return new CapturedTargetContext(
            Hwnd: hwnd,
            Pid: (int)pid,
            ProcessName: procName,
            ExecutablePath: exePath,
            ClassName: className,
            WindowTitle: title,
            CapturedAt: DateTimeOffset.UtcNow,
            HwndFocus: hwndFocus,
            KeyState: keyState);
    }

    private static string SnapshotModifierState()
    {
        var down = new System.Collections.Generic.List<string>();
        if ((GetAsyncKeyState(VK_LWIN) & 0x8000) != 0) down.Add("LWin");
        if ((GetAsyncKeyState(VK_RWIN) & 0x8000) != 0) down.Add("RWin");
        if ((GetAsyncKeyState(VK_LMENU) & 0x8000) != 0) down.Add("LAlt");
        if ((GetAsyncKeyState(VK_RMENU) & 0x8000) != 0) down.Add("RAlt");
        if ((GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0) down.Add("Ctrl");
        if ((GetAsyncKeyState(VK_SHIFT) & 0x8000) != 0) down.Add("Shift");
        return string.Join(",", down);
    }

    private static string ReadClassName(IntPtr hwnd)
    {
        var sb = new StringBuilder(256);
        var len = GetClassNameW(hwnd, sb, sb.Capacity);
        return len > 0 ? sb.ToString() : "";
    }

    private static string ReadWindowTitle(IntPtr hwnd)
    {
        var len = GetWindowTextLengthW(hwnd);
        if (len <= 0) return "";
        var sb = new StringBuilder(len + 1);
        GetWindowTextW(hwnd, sb, sb.Capacity);
        return sb.ToString();
    }

    private static (string procName, string exePath) ReadProcessImage(uint pid)
    {
        // Prefer QueryFullProcessImageName — works across elevation when
        // we have at least PROCESS_QUERY_LIMITED_INFORMATION.
        var hProcess = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        if (hProcess == IntPtr.Zero)
        {
            // Fallback: managed Process API. Will throw under elevation
            // mismatch, in which case we just degrade to empty fields.
            try
            {
                using var p = Process.GetProcessById((int)pid);
                var name = (p.ProcessName ?? "").ToLowerInvariant();
                if (!name.EndsWith(".exe")) name += ".exe";
                return (name, "");
            }
            catch
            {
                return ("", "");
            }
        }
        try
        {
            var sb = new StringBuilder(1024);
            uint cap = (uint)sb.Capacity;
            if (QueryFullProcessImageNameW(hProcess, 0, sb, ref cap))
            {
                var path = sb.ToString();
                var fileName = System.IO.Path.GetFileName(path).ToLowerInvariant();
                return (fileName, path);
            }
            return ("", "");
        }
        finally
        {
            CloseHandle(hProcess);
        }
    }
}
