using System;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Services;

public class HotkeyService : IDisposable
{
    [DllImport("user32.dll")] private static extern bool RegisterHotKey(IntPtr hWnd, int id, uint fsModifiers, uint vk);
    [DllImport("user32.dll")] private static extern bool UnregisterHotKey(IntPtr hWnd, int id);

    // Window subclass for intercepting WM_HOTKEY
    private delegate IntPtr SUBCLASSPROC(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam, IntPtr uIdSubclass, IntPtr dwRefData);
    [DllImport("comctl32.dll")] private static extern bool SetWindowSubclass(IntPtr hWnd, SUBCLASSPROC pfnSubclass, IntPtr uIdSubclass, IntPtr dwRefData);
    [DllImport("comctl32.dll")] private static extern bool RemoveWindowSubclass(IntPtr hWnd, SUBCLASSPROC pfnSubclass, IntPtr uIdSubclass);
    [DllImport("comctl32.dll")] private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);

    private const uint WM_HOTKEY = 0x0312;

    public const uint MOD_ALT = 0x0001;
    public const uint MOD_CONTROL = 0x0002;
    public const uint MOD_SHIFT = 0x0004;
    public const uint MOD_WIN = 0x0008;
    public const uint MOD_NOREPEAT = 0x4000;

    private const int HOTKEY_ID = 0xD100; // Dimmy hotkey
    private IntPtr _hwnd;
    private bool _registered;
    private SUBCLASSPROC? _subclassProc; // prevent GC collection

    public event Action? HotkeyPressed;

    public void Register(IntPtr hwnd, string shortcut)
    {
        Unregister();
        _hwnd = hwnd;
        var (modifiers, vk) = ParseShortcut(shortcut);
        if (modifiers == 0 && vk == 0) return;

        // Install window subclass to intercept WM_HOTKEY messages
        _subclassProc = SubclassWndProc;
        SetWindowSubclass(_hwnd, _subclassProc, IntPtr.Zero, IntPtr.Zero);

        _registered = RegisterHotKey(_hwnd, HOTKEY_ID, modifiers | MOD_NOREPEAT, vk);
    }

    public void Unregister()
    {
        if (_registered && _hwnd != IntPtr.Zero)
        {
            UnregisterHotKey(_hwnd, HOTKEY_ID);
            if (_subclassProc != null)
                RemoveWindowSubclass(_hwnd, _subclassProc, IntPtr.Zero);
            _registered = false;
        }
    }

    private IntPtr SubclassWndProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam,
        IntPtr uIdSubclass, IntPtr dwRefData)
    {
        if (uMsg == WM_HOTKEY && wParam.ToInt32() == HOTKEY_ID)
        {
            HotkeyPressed?.Invoke();
            return IntPtr.Zero;
        }
        return DefSubclassProc(hWnd, uMsg, wParam, lParam);
    }

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
                    modifiers |= MOD_WIN; break;
                case "alt": case "option": case "opt":
                    modifiers |= MOD_ALT; break;
                case "ctrl": case "control":
                    modifiers |= MOD_CONTROL; break;
                case "shift":
                    modifiers |= MOD_SHIFT; break;
                default:
                    // Try single character (A-Z, 0-9)
                    if (lower.Length == 1)
                    {
                        char c = char.ToUpperInvariant(lower[0]);
                        if (c is >= 'A' and <= 'Z') vk = (uint)c;
                        else if (c is >= '0' and <= '9') vk = (uint)c;
                    }
                    break;
            }
        }

        return (modifiers, vk);
    }

    public void Dispose()
    {
        Unregister();
        GC.SuppressFinalize(this);
    }
}
