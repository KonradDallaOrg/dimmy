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
        Assert.Equal(0x41u, vk); // VK_A
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

    // ── F-keys ──

    [Fact]
    public void ParseShortcut_FKey_ReturnsCorrectVK()
    {
        var (_, vk) = HotkeyService.ParseShortcut("F5");
        Assert.Equal(0x74u, vk); // VK_F5 = 0x70 + 4
    }

    [Fact]
    public void ParseShortcut_AltC_ReturnsAltAndC()
    {
        var (modifiers, vk) = HotkeyService.ParseShortcut("Alt+C");
        Assert.True((modifiers & HotkeyService.MOD_ALT) != 0);
        Assert.Equal(0x43u, vk); // VK_C
    }

    [Fact]
    public void ParseShortcut_AllModifiers()
    {
        var (modifiers, vk) = HotkeyService.ParseShortcut("Win+Ctrl+Alt+Shift+X");
        Assert.True((modifiers & HotkeyService.MOD_WIN) != 0);
        Assert.True((modifiers & HotkeyService.MOD_CONTROL) != 0);
        Assert.True((modifiers & HotkeyService.MOD_ALT) != 0);
        Assert.True((modifiers & HotkeyService.MOD_SHIFT) != 0);
        Assert.Equal(0x58u, vk); // VK_X
    }

    [Fact]
    public void ParseShortcut_NumberKey()
    {
        var (_, vk) = HotkeyService.ParseShortcut("Ctrl+5");
        Assert.Equal(0x35u, vk); // VK_5 = '5'
    }

    [Fact]
    public void ParseShortcut_NamedKey_Space()
    {
        var (_, vk) = HotkeyService.ParseShortcut("Ctrl+Space");
        Assert.Equal(0x20u, vk);
    }

    [Fact]
    public void ParseShortcut_NamedKey_Escape()
    {
        var (_, vk) = HotkeyService.ParseShortcut("Escape");
        Assert.Equal(0x1Bu, vk);
    }

    [Fact]
    public void ParseShortcut_OnlyModifiers_NoVK()
    {
        var (modifiers, vk) = HotkeyService.ParseShortcut("Win+Alt");
        Assert.True(modifiers != 0);
        Assert.Equal(0u, vk); // no VK, only modifiers
    }

    [Fact]
    public void ParseShortcut_NullString_ReturnsZero()
    {
        var (modifiers, vk) = HotkeyService.ParseShortcut(null!);
        Assert.Equal(0u, modifiers);
        Assert.Equal(0u, vk);
    }
}
