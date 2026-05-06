using System;
using System.Runtime.InteropServices;
using System.Text;

namespace Dimmy.Windows.Helpers;

/// Captures the foreground app's executable name (e.g. "slack.exe") at
/// hotkey-down. Fed to the Rust core via dimmy_set_app_context so that
/// LLM-enhance can resolve user-defined app_rules — e.g. force "imbruttito"
/// style in Slack and "professional" in Outlook without the user having
/// to switch the global pref every time.
///
/// Best-effort: any Win32 failure (UAC-elevated foreground, exotic
/// shells) returns an empty string and the caller treats it as "no rule
/// matches" — same path as the no-foreground case.
public static class AppContextCapture
{
    public readonly record struct ForegroundApp(string ProcessName, string ExePath)
    {
        public bool HasValue => !string.IsNullOrEmpty(ProcessName);
        public static ForegroundApp Empty => new("", "");
    }

    /// Returns the basename of the foreground window's process executable
    /// (e.g. "slack.exe", "code.exe"), lowercased. Empty string when the
    /// foreground window can't be identified.
    public static string GetForegroundProcessName()
        => GetForegroundApp().ProcessName;

    /// Returns both the basename and the full executable path of the
    /// foreground process. The full path lets us extract the real
    /// taskbar icon via SHGetFileInfo (see IconExtractor).
    public static ForegroundApp GetForegroundApp()
    {
        try
        {
            var hwnd = GetForegroundWindow();
            if (hwnd == IntPtr.Zero) return ForegroundApp.Empty;

            uint pid = 0;
            GetWindowThreadProcessId(hwnd, out pid);
            if (pid == 0) return ForegroundApp.Empty;

            // PROCESS_QUERY_LIMITED_INFORMATION (0x1000) is enough for
            // QueryFullProcessImageName and works against UAC-elevated
            // processes from a non-elevated caller; PROCESS_QUERY_INFORMATION
            // would deny.
            var hProc = OpenProcess(0x1000, false, pid);
            if (hProc == IntPtr.Zero) return ForegroundApp.Empty;

            try
            {
                var sb = new StringBuilder(1024);
                int len = sb.Capacity;
                if (!QueryFullProcessImageName(hProc, 0, sb, ref len))
                    return ForegroundApp.Empty;
                var path = sb.ToString(0, len);
                if (string.IsNullOrEmpty(path)) return ForegroundApp.Empty;
                var name = System.IO.Path.GetFileName(path);
                return new ForegroundApp(name?.ToLowerInvariant() ?? "", path);
            }
            finally
            {
                CloseHandle(hProc);
            }
        }
        catch
        {
            return ForegroundApp.Empty;
        }
    }

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint dwDesiredAccess, bool bInheritHandle, uint dwProcessId);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryFullProcessImageName(IntPtr hProcess, uint dwFlags, StringBuilder lpExeName, ref int lpdwSize);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(IntPtr hObject);
}
