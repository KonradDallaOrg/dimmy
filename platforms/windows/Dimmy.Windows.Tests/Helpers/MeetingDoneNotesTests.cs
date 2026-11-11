using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

/// <summary>
/// The shared meeting stop (MeetingStopService) replaced the window's own
/// stop, and with it two things the Done view showed: "⚠ audio incomplete"
/// beside the duration, and "Open Recap settings" on a recap the user can
/// fix there. The stop and the recap now leave a note per meeting folder;
/// the Done view reads it.
/// </summary>
public class MeetingDoneNotesTests
{
    [Fact]
    public void A_meeting_with_incomplete_audio_says_so_beside_its_metadata()
    {
        var notes = new MeetingDoneNotes();
        notes.AudioIncomplete(@"C:\m\a");
        Assert.Equal("12 min · 2026-11-09 · ⚠ audio incomplete", notes.MetaLine("12 min · 2026-11-09", @"C:\m\a"));
    }

    [Fact]
    public void Another_meeting_keeps_its_metadata_as_it_is()
    {
        var notes = new MeetingDoneNotes();
        notes.AudioIncomplete(@"C:\m\a");
        Assert.Equal("12 min", notes.MetaLine("12 min", @"C:\m\b"));
    }

    [Theory]
    [InlineData(-5, true)]
    [InlineData(-6, true)]
    [InlineData(-7, true)]
    [InlineData(-3, false)]
    [InlineData(-4, false)]
    [InlineData(-8, false)]
    public void Only_a_recap_failure_settings_can_fix_offers_the_settings(int rc, bool offered)
    {
        Assert.Equal(offered, MeetingDoneNotes.FixableInRecapSettings(rc));
    }

    [Fact]
    public void A_failed_recap_offers_the_settings_until_a_recap_succeeds()
    {
        var notes = new MeetingDoneNotes();
        notes.RecapFailed(@"C:\m\a", fixableInSettings: true);
        Assert.True(notes.OffersRecapSettings(@"C:\m\a"));
        notes.RecapSaved(@"C:\m\a");
        Assert.False(notes.OffersRecapSettings(@"C:\m\a"));
    }

    [Fact]
    public void A_failure_settings_cannot_fix_offers_nothing()
    {
        var notes = new MeetingDoneNotes();
        notes.RecapFailed(@"C:\m\a", fixableInSettings: false);
        Assert.False(notes.OffersRecapSettings(@"C:\m\a"));
    }

    [Fact]
    public void The_folder_is_matched_whatever_its_case_or_trailing_slash()
    {
        var notes = new MeetingDoneNotes();
        notes.AudioIncomplete(@"C:\M\A\");
        Assert.EndsWith("⚠ audio incomplete", notes.MetaLine("x", @"c:\m\a"));
    }
}
