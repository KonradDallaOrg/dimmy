using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

/// <summary>
/// Tests for the dict-add hotkey combo validator. The parser delegates
/// to HotkeyParser for tokenisation; these tests cover the
/// dict-specific invariant (both a modifier AND a key required) and
/// confirm the default combo "ctrl+shift+d" round-trips to the
/// modifiers + VK that RegisterHotKey expects.
/// </summary>
public class DictHotkeyParserTests
{
    // ── Default combo round-trip ────────────────────────────────────

    [Fact]
    public void TryParse_DefaultCombo_ParsesCtrlShiftD()
    {
        Assert.True(DictHotkeyParser.TryParse("ctrl+shift+d", out var mods, out var vk));
        Assert.True((mods & HotkeyParser.MOD_CONTROL) != 0);
        Assert.True((mods & HotkeyParser.MOD_SHIFT) != 0);
        Assert.Equal(0u, mods & HotkeyParser.MOD_ALT);
        Assert.Equal(0u, mods & HotkeyParser.MOD_WIN);
        Assert.Equal(0x44u, vk); // VK_D
    }

    [Fact]
    public void TryParse_IsCaseInsensitive()
    {
        Assert.True(DictHotkeyParser.TryParse("Ctrl+Shift+D", out var m1, out var k1));
        Assert.True(DictHotkeyParser.TryParse("CTRL+SHIFT+D", out var m2, out var k2));
        Assert.True(DictHotkeyParser.TryParse("ctrl+shift+d", out var m3, out var k3));
        Assert.Equal(m1, m2);
        Assert.Equal(m2, m3);
        Assert.Equal(k1, k2);
        Assert.Equal(k2, k3);
    }

    // ── Dict-specific invariant: BOTH modifier AND key required ─────

    [Fact]
    public void TryParse_ModifierOnly_Rejected()
    {
        // Ctrl+Shift alone would clash with normal modifier presses —
        // RegisterHotKey requires a primary key, and the dict path
        // can't disambiguate "user pressed Ctrl to copy" from "user
        // pressed Ctrl as a hotkey".
        Assert.False(DictHotkeyParser.TryParse("ctrl+shift", out _, out _));
    }

    [Fact]
    public void TryParse_KeyOnly_Rejected()
    {
        // A bare letter would intercept normal typing — catastrophic.
        Assert.False(DictHotkeyParser.TryParse("d", out _, out _));
    }

    [Fact]
    public void TryParse_Empty_Rejected()
    {
        Assert.False(DictHotkeyParser.TryParse("", out var m, out var v));
        Assert.Equal(0u, m);
        Assert.Equal(0u, v);
    }

    [Fact]
    public void TryParse_Whitespace_Rejected()
    {
        Assert.False(DictHotkeyParser.TryParse("   ", out _, out _));
    }

    [Fact]
    public void TryParse_Null_Rejected()
    {
        // Defensive: callers occasionally read combo from a config
        // field that hasn't been initialised yet. Asserting null is
        // safe to pass.
        Assert.False(DictHotkeyParser.TryParse(null!, out var m, out var v));
        Assert.Equal(0u, m);
        Assert.Equal(0u, v);
    }

    // ── Alternative combos users actually pick ──────────────────────

    [Theory]
    [InlineData("alt+d", HotkeyParser.MOD_ALT, 0x44u)]
    [InlineData("win+d", HotkeyParser.MOD_WIN, 0x44u)]
    [InlineData("ctrl+alt+v", HotkeyParser.MOD_CONTROL | HotkeyParser.MOD_ALT, 0x56u)]
    [InlineData("ctrl+shift+alt+space", HotkeyParser.MOD_CONTROL | HotkeyParser.MOD_SHIFT | HotkeyParser.MOD_ALT, 0x20u)]
    [InlineData("ctrl+f12", HotkeyParser.MOD_CONTROL, 0x7Bu)]
    public void TryParse_ValidCombos_RoundTrip(string combo, uint expectedMods, uint expectedVk)
    {
        Assert.True(DictHotkeyParser.TryParse(combo, out var mods, out var vk));
        Assert.Equal(expectedMods, mods & (HotkeyParser.MOD_CONTROL | HotkeyParser.MOD_SHIFT
                                          | HotkeyParser.MOD_ALT | HotkeyParser.MOD_WIN));
        Assert.Equal(expectedVk, vk);
    }

    // ── Alias coverage (sanity that ctrl/control are interchangeable) ─

    [Fact]
    public void TryParse_ControlAlias_EquivalentToCtrl()
    {
        Assert.True(DictHotkeyParser.TryParse("control+shift+d", out var m1, out var v1));
        Assert.True(DictHotkeyParser.TryParse("ctrl+shift+d", out var m2, out var v2));
        Assert.Equal(m1, m2);
        Assert.Equal(v1, v2);
    }

    [Fact]
    public void TryParse_PlusInsideToken_GarbageRejected()
    {
        // "ctrl+shift+" would tokenise to ["ctrl", "shift", ""] —
        // HotkeyParser ignores the empty token, so we fall through to
        // mods!=0 but vk==0 → rejected by the invariant. Belt-and-
        // braces: this scenario is what happens if the user manually
        // edits the config and trails the string.
        Assert.False(DictHotkeyParser.TryParse("ctrl+shift+", out _, out _));
    }
}
