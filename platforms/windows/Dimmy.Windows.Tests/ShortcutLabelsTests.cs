using System.Text.Json;
using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests;

/// The shortcut row draws a stored combo as keys. Stored combos come in every
/// spelling Dimmy ever wrote, so the row must read all of them.
public class ShortcutLabelsTests
{
    [Theory]
    [InlineData("ctrl+shift+d", new[] { "Ctrl", "Shift", "D" })]
    [InlineData("Win+Alt", new[] { "Win", "Alt" })]
    [InlineData("alt+win", new[] { "Win", "Alt" })]
    [InlineData("win+ctrl+space", new[] { "Win", "Ctrl", "Space" })]
    [InlineData("ctrl+f5", new[] { "Ctrl", "F5" })]
    [InlineData("ctrl+num5", new[] { "Ctrl", "Num5" })]
    public void Core_grammar_reads_modifiers_first_in_windows_order(string combo, string[] expected)
    {
        Assert.Equal(expected, ShortcutLabels.FromCombo(combo));
    }

    [Theory]
    [InlineData("LeftWindows+Menu", new[] { "Win", "Alt" })]
    [InlineData("Control+Number1", new[] { "Ctrl", "1" })]
    [InlineData("Shift+Escape", new[] { "Shift", "Esc" })]
    [InlineData("cmd+opt", new[] { "Win", "Alt" })]
    public void Older_spellings_still_read(string combo, string[] expected)
    {
        Assert.Equal(expected, ShortcutLabels.FromCombo(combo));
    }

    [Theory]
    [InlineData("")]
    [InlineData(null)]
    [InlineData(" + ")]
    public void Nothing_set_is_no_keys(string? combo)
    {
        Assert.Empty(ShortcutLabels.FromCombo(combo));
    }

    [Fact]
    public void Recorder_event_payload_parses()
    {
        using var doc = JsonDocument.Parse("""
            {"target":"command","keys":["Ctrl","Shift","D"],"combo":"ctrl+shift+d",
             "can_save":false,"error":"Already used by the add-to-dictionary shortcut.",
             "warnings":["w1"],"mods":{"win":false,"ctrl":true,"alt":false,"shift":true,"fn":false},
             "key":"d","native_key":68}
            """);
        var v = RecorderVerdict.FromJson(doc.RootElement);
        Assert.Equal("command", v.Target);
        Assert.Equal(new[] { "Ctrl", "Shift", "D" }, v.Keys);
        Assert.Equal("ctrl+shift+d", v.Combo);
        Assert.False(v.CanSave);
        Assert.Equal("Already used by the add-to-dictionary shortcut.", v.Error);
        Assert.Equal(new[] { "w1" }, v.Warnings);
    }
}
