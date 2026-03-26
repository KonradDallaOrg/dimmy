using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

public class HotkeyParserTests
{
    [Fact]
    public void ParseShortcut_WinAlt_ReturnsModifiers()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("Win+Alt");
        Assert.True((modifiers & HotkeyParser.MOD_WIN) != 0);
        Assert.True((modifiers & HotkeyParser.MOD_ALT) != 0);
    }

    [Fact]
    public void ParseShortcut_CtrlShiftA_ReturnsCorrectValues()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("Ctrl+Shift+A");
        Assert.True((modifiers & HotkeyParser.MOD_CONTROL) != 0);
        Assert.True((modifiers & HotkeyParser.MOD_SHIFT) != 0);
        Assert.Equal(0x41u, vk); // VK_A
    }

    [Fact]
    public void ParseShortcut_EmptyString_ReturnsZero()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("");
        Assert.Equal(0u, modifiers);
        Assert.Equal(0u, vk);
    }

    [Fact]
    public void ParseShortcut_CaseInsensitive()
    {
        var (mod1, _) = HotkeyParser.ParseShortcut("win+alt");
        var (mod2, _) = HotkeyParser.ParseShortcut("Win+Alt");
        Assert.Equal(mod1, mod2);
    }

    // ── F-keys ──

    [Fact]
    public void ParseShortcut_FKey_ReturnsCorrectVK()
    {
        var (_, vk) = HotkeyParser.ParseShortcut("F5");
        Assert.Equal(0x74u, vk); // VK_F5 = 0x70 + 4
    }

    [Fact]
    public void ParseShortcut_AltC_ReturnsAltAndC()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("Alt+C");
        Assert.True((modifiers & HotkeyParser.MOD_ALT) != 0);
        Assert.Equal(0x43u, vk); // VK_C
    }

    [Fact]
    public void ParseShortcut_AllModifiers()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("Win+Ctrl+Alt+Shift+X");
        Assert.True((modifiers & HotkeyParser.MOD_WIN) != 0);
        Assert.True((modifiers & HotkeyParser.MOD_CONTROL) != 0);
        Assert.True((modifiers & HotkeyParser.MOD_ALT) != 0);
        Assert.True((modifiers & HotkeyParser.MOD_SHIFT) != 0);
        Assert.Equal(0x58u, vk); // VK_X
    }

    [Fact]
    public void ParseShortcut_NumberKey()
    {
        var (_, vk) = HotkeyParser.ParseShortcut("Ctrl+5");
        Assert.Equal(0x35u, vk); // VK_5 = '5'
    }

    [Fact]
    public void ParseShortcut_NamedKey_Space()
    {
        var (_, vk) = HotkeyParser.ParseShortcut("Ctrl+Space");
        Assert.Equal(0x20u, vk);
    }

    [Fact]
    public void ParseShortcut_NamedKey_Escape()
    {
        var (_, vk) = HotkeyParser.ParseShortcut("Escape");
        Assert.Equal(0x1Bu, vk);
    }

    [Fact]
    public void ParseShortcut_OnlyModifiers_NoVK()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("Win+Alt");
        Assert.True(modifiers != 0);
        Assert.Equal(0u, vk); // no VK, only modifiers
    }

    [Fact]
    public void ParseShortcut_NullString_ReturnsZero()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut(null!);
        Assert.Equal(0u, modifiers);
        Assert.Equal(0u, vk);
    }

    // ── F-key range ──

    [Theory]
    [InlineData("F1", 0x70u)]
    [InlineData("F12", 0x7Bu)]
    [InlineData("F24", 0x87u)]
    public void ParseShortcut_FKeys_CorrectVK(string key, uint expectedVk)
    {
        var (_, vk) = HotkeyParser.ParseShortcut(key);
        Assert.Equal(expectedVk, vk);
    }

    // ── Letter keys A-Z ──

    [Theory]
    [InlineData("A", 0x41u)]
    [InlineData("Z", 0x5Au)]
    [InlineData("a", 0x41u)] // case insensitive
    public void ParseShortcut_LetterKeys(string key, uint expectedVk)
    {
        var (_, vk) = HotkeyParser.ParseShortcut(key);
        Assert.Equal(expectedVk, vk);
    }

    // ── Named keys ──

    [Theory]
    [InlineData("Enter", 0x0Du)]
    [InlineData("Tab", 0x09u)]
    [InlineData("Backspace", 0x08u)]
    [InlineData("Delete", 0x2Eu)]
    [InlineData("Insert", 0x2Du)]
    [InlineData("Home", 0x24u)]
    [InlineData("End", 0x23u)]
    [InlineData("PageUp", 0x21u)]
    [InlineData("PageDown", 0x22u)]
    [InlineData("Up", 0x26u)]
    [InlineData("Down", 0x28u)]
    [InlineData("Left", 0x25u)]
    [InlineData("Right", 0x27u)]
    public void ParseShortcut_NamedKeys(string key, uint expectedVk)
    {
        var (_, vk) = HotkeyParser.ParseShortcut($"Ctrl+{key}");
        Assert.Equal(expectedVk, vk);
    }

    // ── Modifier aliases ──

    [Theory]
    [InlineData("Super")]
    [InlineData("Meta")]
    [InlineData("Cmd")]
    [InlineData("LeftWindows")]
    public void ParseShortcut_WinAliases(string alias)
    {
        var (modifiers, _) = HotkeyParser.ParseShortcut(alias);
        Assert.True((modifiers & HotkeyParser.MOD_WIN) != 0);
    }

    [Theory]
    [InlineData("Option")]
    [InlineData("Opt")]
    [InlineData("Menu")]
    public void ParseShortcut_AltAliases(string alias)
    {
        var (modifiers, _) = HotkeyParser.ParseShortcut(alias);
        Assert.True((modifiers & HotkeyParser.MOD_ALT) != 0);
    }

    // ── Whitespace handling ──

    [Fact]
    public void ParseShortcut_ExtraWhitespace_Trims()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("  Ctrl + Alt + A  ");
        Assert.True((modifiers & HotkeyParser.MOD_CONTROL) != 0);
        Assert.True((modifiers & HotkeyParser.MOD_ALT) != 0);
        Assert.Equal(0x41u, vk);
    }

    // ── Invalid/unknown key ──

    [Fact]
    public void ParseShortcut_UnknownKey_IgnoredGracefully()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("Ctrl+FooBar");
        Assert.True((modifiers & HotkeyParser.MOD_CONTROL) != 0);
        Assert.Equal(0u, vk); // FooBar not recognized → no VK
    }

    // ── Number keys ──

    [Theory]
    [InlineData("0", 0x30u)]
    [InlineData("9", 0x39u)]
    public void ParseShortcut_NumberKeys(string key, uint expectedVk)
    {
        var (_, vk) = HotkeyParser.ParseShortcut(key);
        Assert.Equal(expectedVk, vk);
    }

    // ── Preconditions (NSP) ──

    [Fact]
    public void ParseShortcut_WhitespaceOnly_ReturnsZero()
    {
        var (modifiers, vk) = HotkeyParser.ParseShortcut("   ");
        Assert.Equal(0u, modifiers);
        Assert.Equal(0u, vk);
    }

    [Fact]
    public void ParseShortcut_ModifiersOnly_VkIsZero()
    {
        // Postcondition: modifiers without VK is valid but VK=0
        var (modifiers, vk) = HotkeyParser.ParseShortcut("Ctrl+Alt+Shift");
        Assert.True(modifiers != 0, "Should have modifier flags");
        Assert.Equal(0u, vk);
    }

    [Fact]
    public void ParseShortcut_VkOnly_ModifiersZero()
    {
        // Just a key without modifiers
        var (modifiers, vk) = HotkeyParser.ParseShortcut("X");
        Assert.Equal(0u, modifiers);
        Assert.Equal(0x58u, vk);
    }
}
