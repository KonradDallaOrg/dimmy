using System.Collections.Generic;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

/// <summary>
/// A loaded or Telegram audio keeps its own container instead of being
/// decoded to WAV. Three separate readers look the file up by extension
/// (waveform, playback, language detection) and a fourth lives in Rust, so
/// the order and the list are the contract between them.
/// </summary>
public class MeetingAudioTests
{
    [Fact]
    public void A_recorded_format_wins_over_a_loaded_one_in_the_same_folder()
    {
        // Same precedence as the Rust resolve_meeting_track: a real recording
        // writes .ogg, and that is the track to play.
        var present = new HashSet<string> { @"d\audio.m4a", @"d\audio.ogg" };
        Assert.Equal(@"d\audio.ogg", MeetingAudio.Resolve("d", "audio", present.Contains));
    }

    [Fact]
    public void A_telegram_m4a_is_found_when_it_is_the_only_track()
    {
        var present = new HashSet<string> { @"d\audio.m4a" };
        Assert.Equal(@"d\audio.m4a", MeetingAudio.Resolve("d", "audio", present.Contains));
    }

    [Fact]
    public void Per_band_tracks_resolve_the_same_way()
    {
        var present = new HashSet<string> { @"d\audio_mic.ogg", @"d\audio_system.wav" };
        Assert.Equal(@"d\audio_mic.ogg", MeetingAudio.Resolve("d", "audio_mic", present.Contains));
        Assert.Equal(@"d\audio_system.wav", MeetingAudio.Resolve("d", "audio_system", present.Contains));
        Assert.Null(MeetingAudio.Resolve("d", "audio_nothing", present.Contains));
    }

    [Fact]
    public void The_extension_list_matches_the_one_the_core_resolves()
    {
        // core/src/ffi.rs::resolve_meeting_track. A file the host saves and
        // the core cannot find back is a meeting that will not re-transcribe.
        Assert.Equal(
            new[] { "ogg", "wav", "m4a", "mp3", "aac", "flac", "mp4", "oga", "opus", "webm" },
            MeetingAudio.Extensions);
    }
}
