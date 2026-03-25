using System;

namespace Dimmy.Windows.Services;

/// <summary>
/// Pure shortcut parsing logic — no WinUI or Win32 dependencies.
/// Extracted from HotkeyService for testability on headless CI.
/// </summary>
public static class HotkeyParser
{
    public const uint MOD_ALT = 0x0001;
    public const uint MOD_CONTROL = 0x0002;
    public const uint MOD_SHIFT = 0x0004;
    public const uint MOD_WIN = 0x0008;
    public const uint MOD_NOREPEAT = 0x4000;

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
}
