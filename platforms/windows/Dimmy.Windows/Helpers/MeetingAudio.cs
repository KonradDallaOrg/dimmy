using System;
using System.IO;
using System.Linq;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// Where a meeting's audio lives on disk, and what a loaded file is saved as.
///
/// <para>A loaded or Telegram file keeps its OWN container rather than being
/// decoded to WAV: an hour of m4a is a tenth of the WAV, and the decode pass
/// cost minutes on the very files people share from a phone. Everything
/// downstream already copes - peaks go through the Rust decoder, the media
/// player handles m4a/mp3 natively, and the Rust re-transcribe resolver knows
/// the same list.</para>
/// </summary>
public static class MeetingAudio
{
    /// Recorded formats first (that is what a real meeting writes), then the
    /// containers a loaded file arrives in. Same order as the Rust
    /// `resolve_meeting_track`, so host and core agree on which file wins when
    /// a folder somehow holds two.
    public static readonly string[] Extensions =
        { "ogg", "wav", "m4a", "mp3", "aac", "flac", "mp4" };

    /// <summary>The name a loaded file is saved under inside the meeting
    /// folder: `audio.` plus its own extension. Anything we cannot name
    /// falls back to `audio.wav`, which is what the caller used to write
    /// unconditionally.</summary>
    public static string FileNameForSource(string? sourcePath)
    {
        var ext = Path.GetExtension(sourcePath ?? "").TrimStart('.').ToLowerInvariant();
        return Extensions.Contains(ext) ? $"audio.{ext}" : "audio.wav";
    }

    /// <summary>Resolve `baseName` (audio / audio_mic / audio_system) to the
    /// file on disk, or null. `exists` is injected so the rule is testable
    /// without touching a disk.</summary>
    public static string? Resolve(string dir, string baseName, Func<string, bool>? exists = null)
    {
        if (string.IsNullOrEmpty(dir) || string.IsNullOrEmpty(baseName)) return null;
        exists ??= File.Exists;
        foreach (var ext in Extensions)
        {
            var path = Path.Combine(dir, $"{baseName}.{ext}");
            if (exists(path)) return path;
        }
        return null;
    }
}
