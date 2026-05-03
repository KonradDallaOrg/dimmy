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
        DateTimeOffset CapturedAt)
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
        public string ToLogString() =>
            $"hwnd=0x{Hwnd.ToInt64():X} pid={Pid} proc='{ProcessName}' " +
            $"class='{ClassName}' title='{Truncate(WindowTitle, 80)}'";

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

        GetWindowThreadProcessId(hwnd, out uint pid);
        if (pid == 0)
            return CapturedTargetContext.Empty with { Hwnd = hwnd, CapturedAt = DateTimeOffset.UtcNow };

        var className = ReadClassName(hwnd);
        var title = ReadWindowTitle(hwnd);
        var (procName, exePath) = ReadProcessImage(pid);

        return new CapturedTargetContext(
            Hwnd: hwnd,
            Pid: (int)pid,
            ProcessName: procName,
            ExecutablePath: exePath,
            ClassName: className,
            WindowTitle: title,
            CapturedAt: DateTimeOffset.UtcNow);
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
