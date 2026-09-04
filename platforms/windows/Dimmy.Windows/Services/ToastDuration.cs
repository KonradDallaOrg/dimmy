using System;

namespace Dimmy.Windows.Services;

/// <summary>
/// How long a toast stays on screen, derived from how long it takes to read.
///
/// Every toast showed for a flat 3 s until 2026-09-04. That was chosen for
/// the dictionary toasts it was written for ("Added to dictionary") and is
/// right for them. It is wrong for the ones added since, which carry the
/// user's next step:
///
///   "Transcription is falling behind. Your recording is safe and continues
///    normally. Try Parakeet or a smaller model in Settings, Transcription.
///    You can also regenerate the transcript from the audio after the
///    meeting."
///
/// That is 34 words. At 3 s it disappeared before it could be read — and it
/// is precisely the toast whose whole purpose is to be acted on. Reported
/// from a live meeting, not from a hypothesis.
///
/// Pure and separate from the window so it can be tested without WinUI.
/// </summary>
public static class ToastDuration
{
    /// <summary>What the shortest toast needs. Also the floor for everything
    /// else — a toast is never shown for less than this however short.</summary>
    public const double MinSecs = 3.0;

    /// <summary>Beyond this it is not a toast any more. A message that needs
    /// longer belongs somewhere the user can return to.</summary>
    public const double MaxSecs = 12.0;

    /// <summary>Deliberately below the ~250 wpm of focused reading: a toast
    /// is glanced at while doing something else.</summary>
    public const double WordsPerMinute = 180.0;

    /// <summary>Time to notice the toast and look at it, before reading
    /// starts. Without this the count is of a reader already staring at the
    /// right corner of the screen, which nobody is.</summary>
    public const double NoticeSecs = 1.2;

    /// <summary>Seconds to show a toast with this title and body.</summary>
    public static double For(string? title, string? body)
    {
        int words = CountWords(title) + CountWords(body);
        double read = words / WordsPerMinute * 60.0;
        double total = NoticeSecs + read;
        if (total < MinSecs) return MinSecs;
        if (total > MaxSecs) return MaxSecs;
        return total;
    }

    private static int CountWords(string? s)
    {
        if (string.IsNullOrWhiteSpace(s)) return 0;
        int n = 0;
        bool inWord = false;
        foreach (char c in s)
        {
            if (char.IsWhiteSpace(c)) { inWord = false; }
            else if (!inWord) { inWord = true; n++; }
        }
        return n;
    }
}
