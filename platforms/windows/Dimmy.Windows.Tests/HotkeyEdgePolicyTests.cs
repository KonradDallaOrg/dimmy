using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests;

/// Which releases reach the app. The mode is read when the release arrives:
/// HotkeyService used to keep its own copy, Settings' auto-save never updated
/// it, and after switching to toggle every release still stopped the
/// recording the press had just started (2026-11-06, empty dictations).
public class HotkeyEdgePolicyTests
{
    [Theory]
    [InlineData("dictation")]
    [InlineData("command")]
    public void Push_to_talk_ends_on_release(string binding)
    {
        Assert.True(HotkeyEdgePolicy.DeliverRelease(binding, "hold"));
    }

    [Theory]
    [InlineData("dictation")]
    [InlineData("command")]
    public void Toggle_ignores_the_release(string binding)
    {
        Assert.False(HotkeyEdgePolicy.DeliverRelease(binding, "toggle"));
    }

    [Theory]
    [InlineData("hold")]
    [InlineData("toggle")]
    public void A_meeting_is_toggle_only_whatever_the_mode(string mode)
    {
        Assert.False(HotkeyEdgePolicy.DeliverRelease("meeting", mode));
    }

    [Fact]
    public void An_unknown_mode_is_toggle()
    {
        // config.json's default is toggle; never stop on release by accident.
        Assert.False(HotkeyEdgePolicy.DeliverRelease("dictation", ""));
    }
}
