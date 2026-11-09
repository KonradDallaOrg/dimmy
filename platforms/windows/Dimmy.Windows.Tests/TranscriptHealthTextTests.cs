using System;
using System.IO;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

/// A failed meeting must never read as a silent one (2026-11-09: Nemotron was
/// refused for memory and the window said "VAD may have removed all audio").
public class TranscriptHealthTextTests
{
    [Fact]
    public void FailedMeetingNamesTheCauseNotTheVad()
    {
        var text = TranscriptHealthText.EmptyTranscript("failed", "not_enough_memory", "nemotron");
        Assert.Contains("not enough free memory to load Nemotron", text);
        Assert.DoesNotContain("VAD", text);
    }

    [Fact]
    public void SilentAndOldMeetingsAreToldApart()
    {
        Assert.Contains("no speech", TranscriptHealthText.EmptyTranscript("silent", null, "whisper"));
        // Recorded before meta.json carried a transcript status: claim nothing.
        var old = TranscriptHealthText.EmptyTranscript(null, null, null);
        Assert.DoesNotContain("speech", old);
        Assert.Contains("Regenerate transcript", old);
    }

    [Fact]
    public void ReadsTheStatusTheCoreWrites()
    {
        var dir = Path.Combine(Path.GetTempPath(), "dimmy-th-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        try
        {
            File.WriteAllText(Path.Combine(dir, "meta.json"),
                "{\"id\":\"x\",\"transcript\":{\"status\":\"failed\",\"engine\":\"cloud\",\"reason\":\"auth\",\"windows_text\":0}}");
            Assert.Equal(("failed", "auth", "cloud"), TranscriptHealthText.ReadMeta(dir));
            Assert.Contains("refused the API key", TranscriptHealthText.EmptyTranscriptFor(dir));
        }
        finally { Directory.Delete(dir, true); }
    }

    [Fact]
    public void EveryReasonCodeHasItsOwnSentence()
    {
        string[] codes = { "not_enough_memory", "model_missing", "no_api_key", "auth", "network", "rate_limit", "engine_error" };
        var seen = new System.Collections.Generic.HashSet<string>();
        foreach (var c in codes)
            Assert.True(seen.Add(TranscriptHealthText.Cause("whisper", c)), c);
        Assert.DoesNotContain("—", TranscriptHealthText.UnavailableBody("nemotron", "not_enough_memory"));
    }
}
