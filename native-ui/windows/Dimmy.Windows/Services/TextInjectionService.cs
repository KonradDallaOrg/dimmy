using System;
using System.Runtime.InteropServices;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

public static class TextInjectionService
{
    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);

    [DllImport("user32.dll")]
    private static extern bool OpenClipboard(IntPtr hWndNewOwner);

    [DllImport("user32.dll")]
    private static extern bool CloseClipboard();

    [DllImport("user32.dll")]
    private static extern bool EmptyClipboard();

    [DllImport("user32.dll")]
    private static extern IntPtr SetClipboardData(uint uFormat, IntPtr hMem);

    [DllImport("user32.dll")]
    private static extern IntPtr GetClipboardData(uint uFormat);

    [DllImport("user32.dll")]
    private static extern bool IsClipboardFormatAvailable(uint format);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GlobalAlloc(uint uFlags, UIntPtr dwBytes);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GlobalLock(IntPtr hMem);

    [DllImport("kernel32.dll")]
    private static extern bool GlobalUnlock(IntPtr hMem);

    [DllImport("kernel32.dll")]
    private static extern UIntPtr GlobalSize(IntPtr hMem);

    private const uint CF_UNICODETEXT = 13;
    private const uint GMEM_MOVEABLE = 0x0002;

    private const int INPUT_KEYBOARD = 1;
    private const ushort VK_CONTROL = 0x11;
    private const ushort VK_V = 0x56;
    private const uint KEYEVENTF_KEYUP = 0x0002;

    // Correct struct layout for SendInput on x64
    [StructLayout(LayoutKind.Sequential)]
    private struct INPUT
    {
        public uint type;
        public INPUTUNION u;
    }

    // Union must be sized to the largest member (MOUSEINPUT = 32 bytes on x64)
    [StructLayout(LayoutKind.Explicit)]
    private struct INPUTUNION
    {
        [FieldOffset(0)] public KEYBDINPUT ki;
        [FieldOffset(0)] public MOUSEINPUT mi;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KEYBDINPUT
    {
        public ushort wVk;
        public ushort wScan;
        public uint dwFlags;
        public uint time;
        public IntPtr dwExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MOUSEINPUT
    {
        public int dx;
        public int dy;
        public uint mouseData;
        public uint dwFlags;
        public uint time;
        public IntPtr dwExtraInfo;
    }

    /// <summary>
    /// Paste text using Win32 clipboard + SendInput(Ctrl+V).
    /// Same approach as the Tauri version: save clipboard → set text → Ctrl+V → restore.
    /// </summary>
    public static async Task PasteText(string text, bool keepInClipboard = false)
    {
        // Save current clipboard text (only if we plan to restore it)
        string? previousText = null;
        if (!keepInClipboard && OpenClipboard(IntPtr.Zero))
        {
            try
            {
                if (IsClipboardFormatAvailable(CF_UNICODETEXT))
                {
                    var hData = GetClipboardData(CF_UNICODETEXT);
                    if (hData != IntPtr.Zero)
                    {
                        var ptr = GlobalLock(hData);
                        if (ptr != IntPtr.Zero)
                        {
                            previousText = Marshal.PtrToStringUni(ptr);
                            GlobalUnlock(hData);
                        }
                    }
                }
            }
            finally { CloseClipboard(); }
        }

        // Set our text to clipboard
        SetClipboardText(text);

        // Small delay for clipboard to settle
        await Task.Delay(50);

        // Send Ctrl+V
        var inputs = new INPUT[]
        {
            MakeKeyDown(VK_CONTROL),
            MakeKeyDown(VK_V),
            MakeKeyUp(VK_V),
            MakeKeyUp(VK_CONTROL),
        };
        uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<INPUT>());
        System.Diagnostics.Debug.WriteLine($"SendInput sent {sent} of {inputs.Length} events, struct size={Marshal.SizeOf<INPUT>()}");

        // Restore after 150ms (only if keepInClipboard is false)
        if (!keepInClipboard)
        {
            await Task.Delay(150);
            if (previousText != null)
            {
                SetClipboardText(previousText);
            }
        }
    }

    private static void SetClipboardText(string text)
    {
        if (!OpenClipboard(IntPtr.Zero)) return;
        try
        {
            EmptyClipboard();
            var chars = text.ToCharArray();
            var bytes = (chars.Length + 1) * 2; // UTF-16 + null terminator
            var hGlobal = GlobalAlloc(GMEM_MOVEABLE, (UIntPtr)bytes);
            if (hGlobal == IntPtr.Zero) return;

            var ptr = GlobalLock(hGlobal);
            if (ptr != IntPtr.Zero)
            {
                Marshal.Copy(chars, 0, ptr, chars.Length);
                // Null terminator
                Marshal.WriteInt16(ptr + chars.Length * 2, 0);
                GlobalUnlock(hGlobal);
            }
            SetClipboardData(CF_UNICODETEXT, hGlobal);
            // Don't free hGlobal — clipboard owns it now
        }
        finally { CloseClipboard(); }
    }

    private static INPUT MakeKeyDown(ushort vk) => new()
    {
        type = (uint)INPUT_KEYBOARD,
        u = new INPUTUNION { ki = new KEYBDINPUT { wVk = vk } }
    };

    private static INPUT MakeKeyUp(ushort vk) => new()
    {
        type = (uint)INPUT_KEYBOARD,
        u = new INPUTUNION { ki = new KEYBDINPUT { wVk = vk, dwFlags = KEYEVENTF_KEYUP } }
    };
}
