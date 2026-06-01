using System;
using System.Runtime.InteropServices;
using System.Threading;

namespace Dimmy.Windows.Services;

/// <summary>
/// Optional dedicated global hotkey that fires a ONE-SHOT command-mode
/// recording (transform the selection, or generate-and-insert when nothing
/// is selected) and then reverts to normal dictation. Independent of both
/// the Rust low-level hook (main dictation hotkey) and
/// <see cref="DictHotkeyService"/> — same Win32 RegisterHotKey + message-only
/// window approach, on its own background thread.
///
/// Win32 RegisterHotKey delivers key-DOWN only (MOD_NOREPEAT), so the
/// interaction is TOGGLE: first press starts the command recording, second
/// press (or the pill Stop button) ends it. Disabled by default — the user
/// opts in by setting <see cref="UiPreferences.CommandHotkey"/>; an empty
/// combo unregisters.
/// </summary>
public sealed class CommandHotkeyService : IDisposable
{
    private const int WM_HOTKEY = 0x0312;
    private const int WM_QUIT = 0x0012;

    // Distinct id from DictHotkeyService ("DCTK") so the two coexist.
    private const int HOTKEY_ID = 0x43_4D_44_4B; // "CMDK"

    private const uint MOD_NOREPEAT = 0x4000;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool RegisterHotKey(IntPtr hwnd, int id, uint mods, uint vk);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool UnregisterHotKey(IntPtr hwnd, int id);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowExW(
        uint dwExStyle,
        [MarshalAs(UnmanagedType.LPWStr)] string lpClassName,
        [MarshalAs(UnmanagedType.LPWStr)] string lpWindowName,
        uint dwStyle, int x, int y, int nWidth, int nHeight,
        IntPtr hWndParent, IntPtr hMenu, IntPtr hInstance, IntPtr lpParam);

    [DllImport("user32.dll")]
    private static extern bool DestroyWindow(IntPtr hwnd);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetMessageW(out MSG lpMsg, IntPtr hwnd, uint msgFilterMin, uint msgFilterMax);

    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref MSG lpMsg);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr DispatchMessageW(ref MSG lpMsg);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool PostThreadMessageW(uint idThread, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();

    private static readonly IntPtr HWND_MESSAGE = new IntPtr(-3);

    [StructLayout(LayoutKind.Sequential)]
    private struct MSG
    {
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int pt_x;
        public int pt_y;
    }

    /// <summary>Raised once per press (MOD_NOREPEAT). Fires on the pump
    /// thread — the consumer marshals onto the UI dispatcher.</summary>
    public event Action? Triggered;

    private Thread? _thread;
    private uint _threadId;
    private IntPtr _hwnd;
    private string _currentCombo = "";

    // Sent to the pump to re-bind (or, with vk=0, just unbind) the combo on
    // the hwnd-owning thread (RegisterHotKey is thread-affine).
    private const uint WM_REREGISTER = 0x0400 + 1;

    /// <summary>Register (or re-register) the command hotkey. An empty /
    /// unparseable combo UNREGISTERS — command mode then has no dedicated
    /// hotkey (the pill-menu toggle still works). Returns false when nothing
    /// is now bound.</summary>
    public bool Register(string? combo)
    {
        uint mods = 0, vk = 0;
        var parsed = !string.IsNullOrWhiteSpace(combo)
                     && TryParseCombo(combo!, out mods, out vk);
        _currentCombo = parsed ? combo! : "";

        if (_thread is not null)
        {
            PostThreadMessageW(_threadId, WM_REREGISTER, (IntPtr)(int)mods, (IntPtr)(int)vk);
            return parsed;
        }

        // No combo yet and nothing running → nothing to do (stay disabled).
        if (!parsed) return false;

        _thread = new Thread(() => RunMessagePump(mods, vk))
        {
            IsBackground = true,
            Name = "CommandHotkeyPump",
        };
        _thread.Start();
        return true;
    }

    private void RunMessagePump(uint initialMods, uint initialVk)
    {
        _threadId = GetCurrentThreadId();
        _hwnd = CreateWindowExW(0, "STATIC", "DimmyCommandHotkey", 0, 0, 0, 0, 0,
            HWND_MESSAGE, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        if (_hwnd == IntPtr.Zero)
        {
            App.Log("CommandHotkey: CreateWindowExW failed (err=" +
                Marshal.GetLastWin32Error() + ")", "Hotkey");
            return;
        }

        if (!RegisterHotKey(_hwnd, HOTKEY_ID, initialMods | MOD_NOREPEAT, initialVk))
        {
            App.Log("CommandHotkey: RegisterHotKey failed for initial combo (err=" +
                Marshal.GetLastWin32Error() + ")", "Hotkey");
            DestroyWindow(_hwnd);
            _hwnd = IntPtr.Zero;
            return;
        }
        App.Log($"CommandHotkey: registered '{_currentCombo}' (mods=0x{initialMods:X} vk=0x{initialVk:X})", "Hotkey");

        while (GetMessageW(out var msg, IntPtr.Zero, 0, 0) > 0)
        {
            if (msg.message == WM_HOTKEY && (int)msg.wParam == HOTKEY_ID)
            {
                try { Triggered?.Invoke(); }
                catch (Exception ex) { App.Log($"CommandHotkey handler exc: {ex.Message}", "Hotkey"); }
                continue;
            }
            if (msg.message == WM_REREGISTER)
            {
                uint newMods = (uint)(int)msg.wParam;
                uint newVk = (uint)(int)msg.lParam;
                UnregisterHotKey(_hwnd, HOTKEY_ID);
                if (newVk == 0)
                {
                    // Empty combo → leave it unbound (command mode disabled).
                    App.Log("CommandHotkey: unregistered (combo cleared)", "Hotkey");
                }
                else if (!RegisterHotKey(_hwnd, HOTKEY_ID, newMods | MOD_NOREPEAT, newVk))
                {
                    App.Log("CommandHotkey: re-register failed (err=" +
                        Marshal.GetLastWin32Error() + ")", "Hotkey");
                }
                else
                {
                    App.Log($"CommandHotkey: re-registered '{_currentCombo}' (mods=0x{newMods:X} vk=0x{newVk:X})", "Hotkey");
                }
                continue;
            }
            TranslateMessage(ref msg);
            DispatchMessageW(ref msg);
        }

        UnregisterHotKey(_hwnd, HOTKEY_ID);
        DestroyWindow(_hwnd);
        _hwnd = IntPtr.Zero;
        App.Log("CommandHotkey: pump exited", "Hotkey");
    }

    /// <summary>Same grammar as the dictation + dictionary hotkeys (a
    /// modifier plus a single key — modifier-only combos belong on the Rust
    /// hook, not a Win32 hotkey).</summary>
    public static bool TryParseCombo(string combo, out uint mods, out uint vk)
        => DictHotkeyParser.TryParse(combo, out mods, out vk);

    public void Dispose()
    {
        if (_thread is null) return;
        PostThreadMessageW(_threadId, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        _thread.Join(500);
        _thread = null;
    }
}
