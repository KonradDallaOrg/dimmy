using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace Dimmy.Windows.Services;

/// <summary>
/// Secondary global hotkey dedicated to "add selected text to user
/// dictionary". Independent of the main dictation hotkey (which is
/// owned by the Rust low-level keyboard hook in <see cref="HotkeyService"/>)
/// so the two paths can't interfere — different OS subsystem, different
/// process objects.
///
/// Default trigger: <c>Ctrl+Shift+D</c>. Configurable via
/// <see cref="UiPreferences.DictHotkey"/> using the same combo grammar
/// the main hotkey uses (see <c>HotkeyParser</c>).
///
/// Implementation: classic Win32 <c>RegisterHotKey</c> + a message-only
/// window listening for <c>WM_HOTKEY</c>. Runs on its own background
/// thread so the message pump doesn't compete with the WinUI 3
/// dispatcher. On WM_HOTKEY the service raises the Triggered event on
/// the caller-supplied dispatcher; the consumer (App.OnLaunched) does
/// the clipboard read + Rust FFI add.
/// </summary>
public sealed class DictHotkeyService : IDisposable
{
    private const int WM_HOTKEY = 0x0312;
    private const int WM_CLOSE = 0x0010;
    private const int WM_DESTROY = 0x0002;
    private const int WM_QUIT = 0x0012;

    /// <summary>Single registered ID — we only own one combo. Picking a
    /// fixed value avoids the race where Register/Unregister overlap
    /// during reconfigure (the OS keys hotkeys by hwnd+id pair).</summary>
    private const int HOTKEY_ID = 0x44_43_54_4B; // "DCTK"

    // Modifier flags for RegisterHotKey
    private const uint MOD_ALT = 0x0001;
    private const uint MOD_CONTROL = 0x0002;
    private const uint MOD_SHIFT = 0x0004;
    private const uint MOD_WIN = 0x0008;
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

    // HWND_MESSAGE = special parent value that creates a message-only
    // window invisible to the user / taskbar / Alt-Tab. Perfect for a
    // pure-IPC sink with zero UI surface.
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

    /// <summary>Raised on the thread that calls <see cref="Register"/> —
    /// callers should marshal onto the UI dispatcher themselves before
    /// touching XAML state. Fires once per press (MOD_NOREPEAT is set
    /// so auto-repeat doesn't flood).</summary>
    public event Action? Triggered;

    private Thread? _thread;
    private uint _threadId;
    private IntPtr _hwnd;
    private string _currentCombo = "ctrl+shift+d";

    /// <summary>Register the hotkey (or re-register if a new combo is
    /// passed). Combo grammar matches <see cref="HotkeyParser"/>: tokens
    /// joined by '+', case-insensitive, modifiers (ctrl/shift/alt/win)
    /// + an optional single key (a-z, 0-9). Returns false on parse
    /// failure or OS register failure (the previous combo stays bound).</summary>
    public bool Register(string combo)
    {
        if (string.IsNullOrWhiteSpace(combo)) return false;
        if (!TryParseCombo(combo, out var mods, out var vk)) return false;
        _currentCombo = combo;

        if (_thread is not null)
        {
            // Existing thread — re-register via PostThreadMessage so the
            // unregister + register happen on the SAME thread that owns
            // the hwnd (RegisterHotKey is thread-affine).
            PostThreadMessageW(_threadId, WM_REREGISTER, (IntPtr)(int)mods, (IntPtr)(int)vk);
            return true;
        }

        // First-time setup: spawn the message pump thread.
        _thread = new Thread(() => RunMessagePump(mods, vk))
        {
            IsBackground = true,
            Name = "DictHotkeyPump",
        };
        _thread.Start();
        return true;
    }

    /// <summary>Custom WM_USER+ message ID for "re-register hotkey".
    /// Sent to the pump thread when Register() is called a second time
    /// — the pump unregisters the old combo and registers the new one
    /// without recreating the thread / hwnd.</summary>
    private const uint WM_REREGISTER = 0x0400 + 1;

    private void RunMessagePump(uint initialMods, uint initialVk)
    {
        _threadId = GetCurrentThreadId();
        // Create message-only window. We use "STATIC" as the window
        // class — it's a built-in user32 class that exists in every
        // process without needing to RegisterClassEx ourselves. The
        // HWND_MESSAGE parent makes it message-only (no rendering, no
        // user input, doesn't even appear in EnumWindows).
        _hwnd = CreateWindowExW(0, "STATIC", "DimmyDictHotkey", 0, 0, 0, 0, 0,
            HWND_MESSAGE, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        if (_hwnd == IntPtr.Zero)
        {
            App.Log("DictHotkey: CreateWindowExW failed (err=" +
                Marshal.GetLastWin32Error() + ")", "Hotkey");
            return;
        }

        if (!RegisterHotKey(_hwnd, HOTKEY_ID, initialMods | MOD_NOREPEAT, initialVk))
        {
            App.Log("DictHotkey: RegisterHotKey failed for initial combo (err=" +
                Marshal.GetLastWin32Error() + ")", "Hotkey");
            DestroyWindow(_hwnd);
            _hwnd = IntPtr.Zero;
            return;
        }
        App.Log($"DictHotkey: registered '{_currentCombo}' (mods=0x{initialMods:X} vk=0x{initialVk:X})", "Hotkey");

        // GetMessage returns 0 on WM_QUIT, -1 on error, +1 normally.
        while (GetMessageW(out var msg, IntPtr.Zero, 0, 0) > 0)
        {
            if (msg.message == WM_HOTKEY && (int)msg.wParam == HOTKEY_ID)
            {
                try { Triggered?.Invoke(); }
                catch (Exception ex) { App.Log($"DictHotkey handler exc: {ex.Message}", "Hotkey"); }
                continue;
            }
            if (msg.message == WM_REREGISTER)
            {
                uint newMods = (uint)(int)msg.wParam;
                uint newVk = (uint)(int)msg.lParam;
                UnregisterHotKey(_hwnd, HOTKEY_ID);
                if (!RegisterHotKey(_hwnd, HOTKEY_ID, newMods | MOD_NOREPEAT, newVk))
                {
                    App.Log("DictHotkey: re-register failed (err=" +
                        Marshal.GetLastWin32Error() + ")", "Hotkey");
                }
                else
                {
                    App.Log($"DictHotkey: re-registered '{_currentCombo}' (mods=0x{newMods:X} vk=0x{newVk:X})", "Hotkey");
                }
                continue;
            }
            TranslateMessage(ref msg);
            DispatchMessageW(ref msg);
        }

        UnregisterHotKey(_hwnd, HOTKEY_ID);
        DestroyWindow(_hwnd);
        _hwnd = IntPtr.Zero;
        App.Log("DictHotkey: pump exited", "Hotkey");
    }

    /// <summary>Parse a combo string like "ctrl+shift+d" into (mods, vk).
    /// Recognises ctrl / shift / alt / win modifiers + a single letter
    /// or digit key. Returns false on empty, unrecognised tokens, or
    /// missing key code (modifier-only is rejected — Win+Alt-style
    /// PTT belongs on the Rust hook, not on a Win32 hotkey).</summary>
    /// <summary>
    /// Re-uses <see cref="HotkeyParser"/> for combo parsing so the dict
    /// hotkey supports the same grammar (incl. F-keys, named keys) as
    /// the dictation hotkey, and gains all of its test coverage. The
    /// secondary check below enforces dict-specific semantics: BOTH a
    /// modifier AND a key are required (no modifier-only or key-only
    /// combos — those would either clash with normal typing or be
    /// undetectable to RegisterHotKey).
    /// </summary>
    public static bool TryParseCombo(string combo, out uint mods, out uint vk)
        => DictHotkeyParser.TryParse(combo, out mods, out vk);

    public void Dispose()
    {
        if (_thread is null) return;
        // Politely break the GetMessage loop on the pump thread.
        PostThreadMessageW(_threadId, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        _thread.Join(500);
        _thread = null;
    }
}
