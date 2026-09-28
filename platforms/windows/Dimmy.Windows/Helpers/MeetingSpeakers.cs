using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// The speakers a diarized meeting was split into, as the core writes them to
/// <c>speakers.json</c> (see <c>core/src/diarize.rs</c>). Read-only here: a
/// rename goes through <c>dimmy_meeting_rename_speaker</c>, which rewrites
/// both this file and the labels in <c>transcripts.txt</c>.
/// </summary>
public sealed record MeetingSpeaker(
    string Id,
    string Name,
    string Band,
    double TalkSecs,
    IReadOnlyList<(double Start, double End)> Segments,
    int ColorIndex);

public static class MeetingSpeakers
{
    public const string FileName = "speakers.json";

    /// Speakers in id order (S1, S2 …); each keeps the colour of its position
    /// so the transcript label, the chip and the waveform lane always agree.
    /// Empty when the meeting was not diarized or the file is unreadable.
    public static List<MeetingSpeaker> Load(string? meetingDir)
    {
        var list = new List<MeetingSpeaker>();
        if (string.IsNullOrEmpty(meetingDir)) return list;
        var path = Path.Combine(meetingDir, FileName);
        if (!File.Exists(path)) return list;
        try
        {
            using var doc = JsonDocument.Parse(File.ReadAllText(path));
            foreach (var s in doc.RootElement.EnumerateArray())
            {
                var segs = new List<(double, double)>();
                if (s.TryGetProperty("segments", out var arr))
                    foreach (var seg in arr.EnumerateArray())
                        if (seg.GetArrayLength() == 2)
                            segs.Add((seg[0].GetDouble(), seg[1].GetDouble()));
                list.Add(new MeetingSpeaker(
                    s.GetProperty("id").GetString() ?? "",
                    s.GetProperty("name").GetString() ?? "",
                    s.TryGetProperty("band", out var b) ? b.GetString() ?? "" : "",
                    s.TryGetProperty("talk_secs", out var t) ? t.GetDouble() : 0,
                    segs,
                    list.Count));
            }
        }
        catch (Exception ex) when (ex is JsonException or IOException or InvalidOperationException or KeyNotFoundException)
        {
            return new List<MeetingSpeaker>();
        }
        return list;
    }

    /// Label → colour index, for the transcript renderer.
    public static Dictionary<string, int> ColorsByName(IEnumerable<MeetingSpeaker> speakers)
    {
        var map = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        foreach (var s in speakers) map[s.Name] = s.ColorIndex;
        return map;
    }

    // Eight hues far apart from each other and from the mic mint / system
    // violet track colours. Two shades each, as for the tracks: a light one
    // that reads on dark surfaces, a deep one that reads on light ones.
    private static readonly (uint Dark, uint Light)[] Palette =
    {
        (0xFF8AB4FF, 0xFF1F4FB3), // blue
        (0xFFFFC46B, 0xFF8A5A00), // amber
        (0xFFFF8FB8, 0xFFA3285A), // pink
        (0xFF6EE0E6, 0xFF0E6B70), // teal
        (0xFFFFA07A, 0xFFA84415), // orange
        (0xFFB8E07A, 0xFF4E6B12), // lime
        (0xFFE0B8FF, 0xFF6B2FA8), // orchid
        (0xFFE0B8A0, 0xFF7A4A30), // clay
    };

    /// ARGB of palette slot <paramref name="index"/> (wraps past eight).
    public static uint Argb(int index, bool darkTheme)
    {
        var (d, l) = Palette[((index % Palette.Length) + Palette.Length) % Palette.Length];
        return darkTheme ? d : l;
    }

    /// "1:37" / "12:05" — talk time on a chip.
    public static string FormatTalkTime(double secs)
    {
        var t = TimeSpan.FromSeconds(Math.Max(0, secs));
        return t.TotalHours >= 1 ? t.ToString(@"h\:mm\:ss") : t.ToString(@"m\:ss");
    }
}
