using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Threading;

namespace Dimmy.Windows.Services;

/// Tells us WHEN somebody starts or stops using the microphone, without
/// asking anyone.
///
/// Windows already tracks this: it is what draws the microphone indicator in
/// the system tray, and it lives in the registry under
/// CapabilityAccessManager\ConsentStore\microphone — one subkey per app, with
/// LastUsedTimeStart and a LastUsedTimeStop that is 0 while the app still
/// holds the device. A registry key can be waited on with
/// RegNotifyChangeKeyValue, so this costs a blocked thread and nothing else.
///
/// Measured against the 4 Hz WASAPI poll it replaces as a trigger
/// (2026-09-25, Teams and a browser call):
///
///     Teams   start  event 12:24:45.926   poll 12:24:46.131   (+205 ms)
///     Teams   stop   event 12:25:29.540   poll 12:25:29.772   (+232 ms)
///     Chrome  start  event 12:28:57.198   poll 12:28:57.460   (+262 ms)
///
/// The event wins on every edge, which is the point: this is not a cheaper
/// way to be slower.
///
/// It does NOT replace the WASAPI sampling. The registry names the app
/// (a package family name like MSTeams_8wekyb3d8bbwe, or an exe path when
/// the app is not packaged) and never a process id or a session instance id,
/// which is what the call state machine is built on. So this decides WHEN to
/// look, and CallDetectionService still decides WHAT it sees.
///
/// Browsers open and close the device briefly when they check permission —
/// observed 2.4 s before the real call started. Callers must not treat a
/// single edge as a call; the promote-after-N-samples rule already handles it.
internal sealed class MicUsageWatcher : IDisposable
{
    private const string SubKey =
        @"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    private static readonly IntPtr HKEY_CURRENT_USER = new(unchecked((int)0x80000001));
    private const int KEY_NOTIFY = 0x0010;
    private const int KEY_READ = 0x20019;
    private const int REG_NOTIFY_CHANGE_NAME = 0x00000001;
    private const int REG_NOTIFY_CHANGE_LAST_SET = 0x00000004;
    // Without THREAD_AGNOSTIC the registration dies with the thread that made
    // it, and the wait never wakes again.
    private const int REG_NOTIFY_THREAD_AGNOSTIC = 0x10000000;

    [DllImport("advapi32.dll", CharSet = CharSet.Unicode)]
    private static extern int RegOpenKeyEx(IntPtr hKey, string subKey, int options, int sam, out IntPtr result);
    [DllImport("advapi32.dll")]
    private static extern int RegCloseKey(IntPtr hKey);
    [DllImport("advapi32.dll")]
    private static extern int RegNotifyChangeKeyValue(
        IntPtr hKey, bool watchSubtree, int notifyFilter, IntPtr hEvent, bool asynchronous);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateEvent(IntPtr attributes, bool manualReset, bool initialState, string? name);
    [DllImport("kernel32.dll")]
    private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll")]
    private static extern bool ResetEvent(IntPtr handle);
    [DllImport("kernel32.dll")]
    private static extern bool SetEvent(IntPtr handle);
    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    /// Raised on a background thread whenever the set of apps holding the
    /// microphone may have changed. The bool says whether anyone holds it now.
    public event Action<bool>? Changed;

    private IntPtr _key = IntPtr.Zero;
    private IntPtr _signal = IntPtr.Zero;
    private IntPtr _stop = IntPtr.Zero;
    private Thread? _thread;
    private volatile bool _disposed;

    /// True when at least one app currently holds the microphone. Cheap: one
    /// registry read, no COM, no enumeration of audio endpoints.
    public static bool AnyoneUsingMic() => UsersOfMic().Count > 0;

    /// Which apps hold the microphone right now. The name is a package family
    /// name for packaged apps and an exe path for the rest, so it is a hint
    /// for logs, not a key to match processes on.
    public static List<string> UsersOfMic()
    {
        var inUse = new List<string>();
        foreach (var path in new[] { SubKey + @"\NonPackaged", SubKey })
        {
            try
            {
                using var root = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(path);
                if (root == null) continue;
                foreach (var name in root.GetSubKeyNames())
                {
                    using var app = root.OpenSubKey(name);
                    if (app?.GetValue("LastUsedTimeStart") == null) continue;
                    // Windows writes 0 into the stop time while the app still
                    // holds the device, and the real timestamp when it lets go.
                    if (Convert.ToInt64(app.GetValue("LastUsedTimeStop") ?? 0L) == 0L)
                    {
                        inUse.Add(name);
                    }
                }
            }
            catch { /* a privacy key we cannot read must not break detection */ }
        }
        return inUse;
    }

    /// Is anybody OTHER THAN US holding the microphone?
    ///
    /// Dimmy is always in that list while it records, because it is recording
    /// - asking "is the microphone free" would answer its own question. What
    /// the call detector needs to know is whether the app on the call still
    /// has it, and the cheapest correct way to ask that is "is there anyone
    /// but me", which needs no mapping from a process to a registry name.
    /// Those names are a package family for packaged apps
    /// (MSTeams_8wekyb3d8bbwe) and an exe path for the rest
    /// (C:#Program Files#Google#Chrome#Application#chrome.exe), and guessing
    /// that mapping is how this would go wrong quietly.
    public static bool SomeoneElseUsingMic()
    {
        var mine = OwnRegistryName();
        foreach (var name in UsersOfMic())
        {
            if (!string.Equals(name, mine, StringComparison.OrdinalIgnoreCase)) return true;
        }
        return false;
    }

    /// Our own exe as the privacy key spells it: the full path with the
    /// separators replaced by '#'.
    private static string OwnRegistryName()
    {
        try
        {
            var path = Environment.ProcessPath;
            return string.IsNullOrEmpty(path) ? "" : path.Replace('\\', '#');
        }
        catch { return ""; }
    }

    public bool Start()
    {
        if (_thread != null) return true;
        if (RegOpenKeyEx(HKEY_CURRENT_USER, SubKey, 0, KEY_NOTIFY | KEY_READ, out _key) != 0
            || _key == IntPtr.Zero)
        {
            App.Log("cannot open the microphone privacy key — staying on the timer", "MicWatch");
            return false;
        }
        _signal = CreateEvent(IntPtr.Zero, true, false, null);
        _stop = CreateEvent(IntPtr.Zero, true, false, null);
        if (_signal == IntPtr.Zero || _stop == IntPtr.Zero) return false;

        _thread = new Thread(Loop)
        {
            IsBackground = true,
            Name = "MicUsageWatcher",
        };
        _thread.Start();
        return true;
    }

    private void Loop()
    {
        var handles = new[] { _signal, _stop };
        while (!_disposed)
        {
            ResetEvent(_signal);
            const int filter = REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET
                               | REG_NOTIFY_THREAD_AGNOSTIC;
            if (RegNotifyChangeKeyValue(_key, true, filter, _signal, true) != 0) break;

            var which = WaitForMultipleObjects(2, handles, false, 0xFFFFFFFF);
            if (_disposed || which != 0) break;
            try { Changed?.Invoke(AnyoneUsingMic()); }
            catch (Exception ex) { App.Log($"handler threw: {ex.Message}", "MicWatch"); }
        }
    }

    [DllImport("kernel32.dll")]
    private static extern uint WaitForMultipleObjects(
        uint count, IntPtr[] handles, bool waitAll, uint milliseconds);

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        if (_stop != IntPtr.Zero) SetEvent(_stop);
        _thread?.Join(2000);
        _thread = null;
        if (_key != IntPtr.Zero) { RegCloseKey(_key); _key = IntPtr.Zero; }
        if (_signal != IntPtr.Zero) { CloseHandle(_signal); _signal = IntPtr.Zero; }
        if (_stop != IntPtr.Zero) { CloseHandle(_stop); _stop = IntPtr.Zero; }
    }
}
