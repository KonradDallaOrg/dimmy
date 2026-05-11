namespace Dimmy.Windows.Services;

/// <summary>
/// Pure validation wrapper for the dict-add hotkey combo. Delegates the
/// actual tokenisation to <see cref="HotkeyParser"/> (same grammar as
/// the main dictation hotkey) and tacks on the dict-specific invariant:
/// the combo MUST have both a modifier AND a key. Extracted to a
/// freestanding file because <see cref="DictHotkeyService"/> pulls in
/// Win32 P/Invoke + WinUI <c>App.Log</c>, neither of which compiles in
/// the headless xUnit test project.
/// </summary>
public static class DictHotkeyParser
{
    /// <summary>
    /// Parse + validate. Returns true only when the combo has at least
    /// one modifier and exactly one main key. Modifier-only combos
    /// (Ctrl+Shift) and key-only combos (D alone) are rejected because
    /// RegisterHotKey cannot bind them safely without colliding with
    /// regular typing or modifier press detection.
    /// </summary>
    public static bool TryParse(string combo, out uint modifiers, out uint vk)
    {
        (modifiers, vk) = HotkeyParser.ParseShortcut(combo ?? string.Empty);
        return modifiers != 0 && vk != 0;
    }
}
