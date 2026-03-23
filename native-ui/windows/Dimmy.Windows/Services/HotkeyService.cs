using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.UI.Dispatching;

namespace Dimmy.Windows.Services;

/// <summary>
/// Global hotkey service using a dedicated hidden message-only window on its own thread.
/// WinUI 3 windows can swallow WM_HOTKEY messages, so we use a pure Win32 window instead.
/// </summary>
public class HotkeyService : IDisposable
{
    // ── Win32 imports ────────────────────────────────────────────────
    [DllImport("user32.dll")] private static extern bool RegisterHotKey(IntPtr hWnd, int id, uint fsModifiers, uint vk);
    [DllImport("user32.dll")] private static extern bool UnregisterHotKey(IntPtr hWnd, int id);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int vKey);
    [DllImport("user32.dll")] private static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern ushort RegisterClassW(ref WNDCLASS wc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowExW(uint exStyle, string className, string windowName,
        uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool GetMessageW(out MSG msg, IntPtr hWnd, uint filterMin, uint filterMax);
    [DllImport("user32.dll")] private static extern IntPtr DispatchMessageW(ref MSG msg);
    [DllImport("user32.dll")] private static extern IntPtr DefWindowProcW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] private static extern IntPtr SetTimer(IntPtr hWnd, IntPtr nIDEvent, uint uElapse, IntPtr lpTimerFunc);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr GetModuleHandleW(string? name);

    private delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    private struct MSG { public IntPtr hwnd; public uint message; public IntPtr wParam; public IntPtr lParam; public uint time; public int ptX, ptY; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WNDCLASS
    {
        public uint style;
        public WndProcDelegate lpfnWndProc;
        public int cbClsExtra, cbWndExtra;
        public IntPtr hInstance, hIcon, hCursor, hbrBackground;
        public string? lpszMenuName;
        public string lpszClassName;
    }

    // ── Constants ────────────────────────────────────────────────────
    private const uint WM_HOTKEY = 0x0312;
    private const uint WM_QUIT = 0x0012;
    private const uint WM_TIMER = 0x0113;
    private static readonly IntPtr HWND_MESSAGE = new(-3);
    private const int HOTKEY_ID = 0xD100;

    public const uint MOD_ALT = 0x0001;
    public const uint MOD_CONTROL = 0x0002;
    public const uint MOD_SHIFT = 0x0004;
    public const uint MOD_WIN = 0x0008;
    public const uint MOD_NOREPEAT = 0x4000;

    // VK codes for GetAsyncKeyState polling
    private const int VK_LWIN = 0x5B, VK_RWIN = 0x5C;
    private const int VK_LCONTROL = 0xA2, VK_RCONTROL = 0xA3;
    private const int VK_LMENU = 0xA4, VK_RMENU = 0xA5;
    private const int VK_LSHIFT = 0xA0, VK_RSHIFT = 0xA1;

    // ── State ────────────────────────────────────────────────────────
    private Thread? _thread;
    private IntPtr _hwnd;
    private WndProcDelegate? _wndProc; // prevent GC!
    private volatile bool _running;
    private readonly ManualResetEventSlim _ready = new(false);
    private readonly DispatcherQueue _dispatcher;

    private uint _registeredModifiers;
    private uint _registeredVk;

    // PTT polling on dedicated thread
    private Thread? _pttThread;
    private volatile bool _pttPolling;

    public event Action? HotkeyPressed;
    public event Action? HotkeyReleased;

    private static readonly string LogPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "dimmy", "ptt.log");

    private static void Log(string msg)
    {
        var line = $"[{DateTime.Now:HH:mm:ss.fff}] [Hotkey] {msg}";
        Console.WriteLine(line);
        Console.Out.Flush();
        try { File.AppendAllText(LogPath, line + Environment.NewLine); } catch { }
    }

    public HotkeyService(DispatcherQueue dispatcher)
    {
        _dispatcher = dispatcher;
    }

    public void Register(string shortcut)
    {
        // If already running, tear down and restart
        Dispose();

        var (modifiers, vk) = ParseShortcut(shortcut);
        _registeredModifiers = modifiers;
        _registeredVk = vk;

        if (modifiers == 0 && vk == 0)
        {
            Log($"Cannot register \"{shortcut}\" — no valid keys parsed");
            return;
        }

        _running = true;
        _ready.Reset();

        // Must store wndproc as field before thread starts
        _wndProc = HotkeyWndProc;

        _thread = new Thread(() => MessagePumpThread(modifiers, vk))
        {
            IsBackground = true,
            Name = "DimmyHotkeyPump"
        };
        _thread.SetApartmentState(ApartmentState.STA);
        _thread.Start();

        // Wait for the window to be created (max 3s)
        _ready.Wait(3000);
        Log($"Register(\"{shortcut}\") → mods=0x{modifiers:X}, vk=0x{vk:X}, hwnd={_hwnd}");
    }

    private void MessagePumpThread(uint modifiers, uint vk)
    {
        try
        {
            var hInstance = GetModuleHandleW(null);
            var cls = new WNDCLASS
            {
                lpfnWndProc = _wndProc!,
                hInstance = hInstance,
                lpszClassName = $"DimmyHotkey_{Environment.TickCount64}"
            };
            RegisterClassW(ref cls);

            _hwnd = CreateWindowExW(0, cls.lpszClassName, "", 0, 0, 0, 0, 0,
                HWND_MESSAGE, IntPtr.Zero, hInstance, IntPtr.Zero);

            if (_hwnd == IntPtr.Zero)
            {
                Log("FATAL: CreateWindowExW returned null!");
                _ready.Set();
                return;
            }

            bool result = RegisterHotKey(_hwnd, HOTKEY_ID, modifiers | MOD_NOREPEAT, vk);
            Log($"RegisterHotKey result={result} (thread={Environment.CurrentManagedThreadId})");

            // Heartbeat timer to prove the message pump is alive (every 5s)
            SetTimer(_hwnd, (IntPtr)1, 5000, IntPtr.Zero);

            _ready.Set();

            Log("Message pump entering GetMessageW loop");

            // Message pump — blocks until WM_QUIT
            while (_running && GetMessageW(out var msg, IntPtr.Zero, 0, 0))
            {
                DispatchMessageW(ref msg);
            }

            Log("Message pump exited");

            UnregisterHotKey(_hwnd, HOTKEY_ID);
            DestroyWindow(_hwnd);
            _hwnd = IntPtr.Zero;
        }
        catch (Exception ex)
        {
            Log($"MessagePump exception: {ex.Message}");
            _ready.Set();
        }
    }

    /// <summary>Whether PTT mode is active. Set by the caller before Register.</summary>
    public bool PttMode { get; set; }

    private int _heartbeatCount;

    private IntPtr HotkeyWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == WM_TIMER)
        {
            _heartbeatCount++;
            Log($"Pump alive (heartbeat #{_heartbeatCount})");
            return IntPtr.Zero;
        }

        if (msg == WM_HOTKEY)
        {
            Log($"WM_HOTKEY received! wParam=0x{wParam.ToInt64():X} expected=0x{HOTKEY_ID:X} PttMode={PttMode}");

            // CRITICAL: enqueue HotkeyPressed BEFORE starting PTT poll thread.
            bool enqueued = _dispatcher.TryEnqueue(() => HotkeyPressed?.Invoke());
            Log($"HotkeyPressed enqueued={enqueued}");

            if (PttMode)
                StartPttPolling();
            return IntPtr.Zero;
        }

        return DefWindowProcW(hWnd, msg, wParam, lParam);
    }

    // ── PTT polling via GetAsyncKeyState on dedicated thread ──────────
    /// <summary>Start polling for key release on a background thread. Fires HotkeyReleased via dispatcher.</summary>
    public void StartPttPolling()
    {
        StopPttPolling();
        _pttPolling = true;
        _pttThread = new Thread(PttPollLoop) { IsBackground = true, Name = "DimmyPttPoll" };
        _pttThread.Start();
        Log("PTT polling thread started");
    }

    public void StopPttPolling()
    {
        _pttPolling = false;
        _pttThread = null;
    }

    private void PttPollLoop()
    {
        // Tight poll loop — no dispatcher, no timer, just raw Win32
        Log($"PTT poll: checking keys mods=0x{_registeredModifiers:X} vk=0x{_registeredVk:X}");

        // Log initial key state
        bool modHeld = true, vkHeld = true;
        if ((_registeredModifiers & MOD_ALT) != 0)
            modHeld = IsDown(VK_LMENU) || IsDown(VK_RMENU);
        if (_registeredVk != 0)
            vkHeld = IsDown((int)_registeredVk);
        Log($"PTT poll initial state: mod_held={modHeld}, vk_held={vkHeld}");

        while (_pttPolling)
        {
            if (!AreHotkeyKeysHeld())
            {
                _pttPolling = false;
                Log("PTT keys released (poll thread)");
                bool rel = _dispatcher.TryEnqueue(() => HotkeyReleased?.Invoke());
                Log($"HotkeyReleased enqueued={rel}");
                return;
            }
            Thread.Sleep(15);
        }
        Log("PTT poll loop exited (cancelled)");
    }

    private bool AreHotkeyKeysHeld()
    {
        if ((_registeredModifiers & MOD_WIN) != 0 && !IsDown(VK_LWIN) && !IsDown(VK_RWIN))
            return false;
        if ((_registeredModifiers & MOD_ALT) != 0 && !IsDown(VK_LMENU) && !IsDown(VK_RMENU))
            return false;
        if ((_registeredModifiers & MOD_CONTROL) != 0 && !IsDown(VK_LCONTROL) && !IsDown(VK_RCONTROL))
            return false;
        if ((_registeredModifiers & MOD_SHIFT) != 0 && !IsDown(VK_LSHIFT) && !IsDown(VK_RSHIFT))
            return false;
        if (_registeredVk != 0 && !IsDown((int)_registeredVk))
            return false;
        return true;
    }

    private static bool IsDown(int vk) => (GetAsyncKeyState(vk) & 0x8000) != 0;

    // ── Shortcut parsing ─────────────────────────────────────────────
    public static (uint modifiers, uint vk) ParseShortcut(string shortcut)
    {
        if (string.IsNullOrWhiteSpace(shortcut)) return (0, 0);

        uint modifiers = 0;
        uint vk = 0;
        var parts = shortcut.Split('+', StringSplitOptions.TrimEntries);

        foreach (var part in parts)
        {
            var lower = part.ToLowerInvariant();
            switch (lower)
            {
                case "win": case "super": case "meta": case "cmd":
                case "leftwindows": case "rightwindows":
                    modifiers |= MOD_WIN; break;
                case "alt": case "option": case "opt":
                case "menu": case "leftmenu": case "rightmenu":
                    modifiers |= MOD_ALT; break;
                case "ctrl": case "control":
                case "leftcontrol": case "rightcontrol":
                    modifiers |= MOD_CONTROL; break;
                case "shift":
                case "leftshift": case "rightshift":
                    modifiers |= MOD_SHIFT; break;
                default:
                    // F-keys
                    if (lower.StartsWith("f") && int.TryParse(lower[1..], out int fNum) && fNum >= 1 && fNum <= 24)
                    {
                        vk = (uint)(0x6F + fNum);
                    }
                    // Single character A-Z, 0-9
                    else if (lower.Length == 1)
                    {
                        char c = char.ToUpperInvariant(lower[0]);
                        if (c is >= 'A' and <= 'Z') vk = (uint)c;
                        else if (c is >= '0' and <= '9') vk = (uint)c;
                    }
                    // Named keys
                    else
                    {
                        var mapped = lower switch
                        {
                            "space" => 0x20u,
                            "enter" or "return" => 0x0Du,
                            "tab" => 0x09u,
                            "escape" or "esc" => 0x1Bu,
                            "backspace" or "back" => 0x08u,
                            "delete" or "del" => 0x2Eu,
                            "insert" or "ins" => 0x2Du,
                            "home" => 0x24u,
                            "end" => 0x23u,
                            "pageup" or "pgup" => 0x21u,
                            "pagedown" or "pgdn" => 0x22u,
                            "up" => 0x26u,
                            "down" => 0x28u,
                            "left" => 0x25u,
                            "right" => 0x27u,
                            "number0" => 0x30u, "number1" => 0x31u, "number2" => 0x32u,
                            "number3" => 0x33u, "number4" => 0x34u, "number5" => 0x35u,
                            "number6" => 0x36u, "number7" => 0x37u, "number8" => 0x38u,
                            "number9" => 0x39u,
                            _ => 0u
                        };
                        if (mapped != 0) vk = mapped;
                    }
                    break;
            }
        }

        return (modifiers, vk);
    }

    public void Dispose()
    {
        StopPttPolling();
        _running = false;
        if (_hwnd != IntPtr.Zero)
            PostMessageW(_hwnd, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        _thread?.Join(2000);
        _thread = null;
        _hwnd = IntPtr.Zero;
        GC.SuppressFinalize(this);
    }
}
