using System;
using System.Runtime.InteropServices;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

public static class TextInjectionService
{
    [DllImport("user32.dll")] private static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);

    private const int INPUT_KEYBOARD = 1;
    private const ushort VK_CONTROL = 0x11;
    private const ushort VK_V = 0x56;
    private const uint KEYEVENTF_KEYUP = 0x0002;

    [StructLayout(LayoutKind.Sequential)]
    private struct INPUT
    {
        public int type;
        public INPUTUNION u;
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct INPUTUNION
    {
        [FieldOffset(0)] public KEYBDINPUT ki;
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

    /// <summary>
    /// Paste text by:
    /// 1. Save current clipboard
    /// 2. Set text to clipboard
    /// 3. Send Ctrl+V
    /// 4. After 150ms, restore original clipboard
    /// </summary>
    public static async Task PasteText(string text)
    {
        // Save clipboard (using global::Windows.ApplicationModel.DataTransfer)
        var dataPackage = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
        string? previousText = null;
        try
        {
            var content = global::Windows.ApplicationModel.DataTransfer.Clipboard.GetContent();
            if (content.Contains(global::Windows.ApplicationModel.DataTransfer.StandardDataFormats.Text))
                previousText = await content.GetTextAsync();
        }
        catch { /* clipboard may be locked */ }

        // Set our text
        dataPackage.SetText(text);
        global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dataPackage);

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
        SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<INPUT>());

        // Restore after 150ms
        await Task.Delay(150);
        if (previousText != null)
        {
            var restore = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            restore.SetText(previousText);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(restore);
        }
    }

    private static INPUT MakeKeyDown(ushort vk) => new()
    {
        type = INPUT_KEYBOARD,
        u = new INPUTUNION { ki = new KEYBDINPUT { wVk = vk } }
    };

    private static INPUT MakeKeyUp(ushort vk) => new()
    {
        type = INPUT_KEYBOARD,
        u = new INPUTUNION { ki = new KEYBDINPUT { wVk = vk, dwFlags = KEYEVENTF_KEYUP } }
    };
}
