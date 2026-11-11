using System;
using System.Collections.Concurrent;
using System.IO;

namespace Dimmy.Windows.Helpers;

/// <summary>What a meeting's stop and recap left for its Done view, per
/// meeting folder, for this run of the app: the audio came out incomplete,
/// or the recap failed in a way the Recap settings can fix. The shared stop
/// (MeetingStopService) and the recap (MeetingPostProcessService) write it;
/// the window reads it when it shows that meeting. Not in meta.json: the
/// window's own stop only ever showed both right after the stop, and the
/// toast and the recorded failure reason stay either way.</summary>
public sealed class MeetingDoneNotes
{
    public static MeetingDoneNotes Shared { get; } = new();

    private readonly ConcurrentDictionary<string, bool> _audioIncomplete = new(StringComparer.OrdinalIgnoreCase);
    private readonly ConcurrentDictionary<string, bool> _recapFixable = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>The recap errors that point at the recap configuration
    /// (model mismatch, auth, rate limit). Others (-3/-4/-8) would send the
    /// user to the wrong fix.</summary>
    public static bool FixableInRecapSettings(int rc) => rc is -5 or -6 or -7;

    public void AudioIncomplete(string dir) => _audioIncomplete[Key(dir)] = true;

    public void RecapFailed(string dir, bool fixableInSettings) => _recapFixable[Key(dir)] = fixableInSettings;

    public void RecapSaved(string dir) => _recapFixable.TryRemove(Key(dir), out _);

    public bool OffersRecapSettings(string dir) => _recapFixable.TryGetValue(Key(dir), out var f) && f;

    public string MetaLine(string subtitle, string dir)
        => _audioIncomplete.ContainsKey(Key(dir)) ? subtitle + " · ⚠ audio incomplete" : subtitle;

    private static string Key(string dir)
    {
        if (string.IsNullOrWhiteSpace(dir)) throw new ArgumentException("meeting folder is empty", nameof(dir));
        return Path.TrimEndingDirectorySeparator(dir);
    }
}
