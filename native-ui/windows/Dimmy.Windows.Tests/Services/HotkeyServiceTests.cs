using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

public class HotkeyServiceTests
{
    [Fact]
    public void ParseShortcut_WinAlt_ReturnsModifiers()
    {
        var (modifiers, vk) = HotkeyService.ParseShortcut("Win+Alt");
        Assert.True((modifiers & HotkeyService.MOD_WIN) != 0);
        Assert.True((modifiers & HotkeyService.MOD_ALT) != 0);
    }

    [Fact]
    public void ParseShortcut_CtrlShiftA_ReturnsCorrectValues()
    {
        var (modifiers, vk) = HotkeyService.ParseShortcut("Ctrl+Shift+A");
        Assert.True((modifiers & HotkeyService.MOD_CONTROL) != 0);
        Assert.True((modifiers & HotkeyService.MOD_SHIFT) != 0);
        Assert.Equal(0x41, vk); // VK_A
    }

    [Fact]
    public void ParseShortcut_EmptyString_ReturnsZero()
    {
        var (modifiers, vk) = HotkeyService.ParseShortcut("");
        Assert.Equal(0u, modifiers);
        Assert.Equal(0u, vk);
    }

    [Fact]
    public void ParseShortcut_CaseInsensitive()
    {
        var (mod1, _) = HotkeyService.ParseShortcut("win+alt");
        var (mod2, _) = HotkeyService.ParseShortcut("Win+Alt");
        Assert.Equal(mod1, mod2);
    }
}
