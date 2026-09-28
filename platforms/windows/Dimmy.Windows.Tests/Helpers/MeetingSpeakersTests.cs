using System.IO;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

public class MeetingSpeakersTests
{
    private static string TempMeetingDir(string? speakersJson)
    {
        var dir = Path.Combine(Path.GetTempPath(), "dimmy-speakers-" + System.Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        if (speakersJson != null) File.WriteAllText(Path.Combine(dir, MeetingSpeakers.FileName), speakersJson);
        return dir;
    }

    // The exact shape core/src/diarize.rs writes (serde: tuples as arrays).
    private const string CoreJson = """
        [
          { "id": "S1", "name": "Marco", "band": "system", "talk_secs": 97.9,
            "segments": [[0.64, 12.1], [24.6, 28.4]] },
          { "id": "S2", "name": "Speaker 2", "band": "system", "talk_secs": 81.0,
            "segments": [[28.44, 30.9]] }
        ]
        """;

    [Fact]
    public void Reads_the_file_the_core_writes()
    {
        var dir = TempMeetingDir(CoreJson);
        var list = MeetingSpeakers.Load(dir);
        Assert.Equal(2, list.Count);
        Assert.Equal(("S1", "Marco", "system"), (list[0].Id, list[0].Name, list[0].Band));
        Assert.Equal(2, list[0].Segments.Count);
        Assert.Equal((24.6, 28.4), list[0].Segments[1]);
        Assert.Equal(new[] { 0, 1 }, new[] { list[0].ColorIndex, list[1].ColorIndex });
        Directory.Delete(dir, true);
    }

    [Fact]
    public void An_undiarized_or_broken_meeting_has_no_speakers()
    {
        var none = TempMeetingDir(null);
        var broken = TempMeetingDir("{ not json");
        Assert.Empty(MeetingSpeakers.Load(none));
        Assert.Empty(MeetingSpeakers.Load(broken));
        Assert.Empty(MeetingSpeakers.Load(null));
        Directory.Delete(none, true);
        Directory.Delete(broken, true);
    }

    [Fact]
    public void Colours_follow_the_label_case_insensitively()
    {
        var dir = TempMeetingDir(CoreJson);
        var map = MeetingSpeakers.ColorsByName(MeetingSpeakers.Load(dir));
        Assert.Equal(0, map["marco"]);
        Assert.Equal(1, map["Speaker 2"]);
        Directory.Delete(dir, true);
    }

    [Fact]
    public void Palette_wraps_and_differs_per_theme()
    {
        Assert.Equal(MeetingSpeakers.Argb(0, true), MeetingSpeakers.Argb(8, true));
        Assert.NotEqual(MeetingSpeakers.Argb(0, true), MeetingSpeakers.Argb(0, false));
        Assert.NotEqual(MeetingSpeakers.Argb(0, true), MeetingSpeakers.Argb(1, true));
    }

    [Theory]
    [InlineData(97.9, "1:37")]
    [InlineData(3725, "1:02:05")]
    [InlineData(-3, "0:00")]
    public void Talk_time_reads_like_a_clock(double secs, string expected) =>
        Assert.Equal(expected, MeetingSpeakers.FormatTalkTime(secs));
}
