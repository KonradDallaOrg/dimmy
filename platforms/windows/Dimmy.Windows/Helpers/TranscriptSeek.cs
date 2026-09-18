using System;

namespace Dimmy.Windows.Helpers;

/// Maps a scrolled transcript back onto the audio timeline. The Done view
/// already scrolls the transcript when the audio is seeked; this is the other
/// direction — the turn sitting at the top of the reading pane is the one the
/// playhead should be on.
public static class TranscriptSeek
{
    /// <summary>Index of the last turn whose vertical offset is at or above
    /// <paramref name="y"/> (the top of the viewport), i.e. the turn the reader
    /// is looking at. Returns -1 when there are no turns, and 0 when the
    /// viewport sits above the first one. <paramref name="offsetAt"/> must be
    /// non-decreasing — turns are laid out in reading order — which is what
    /// lets this binary-search instead of measuring every paragraph.</summary>
    public static int IndexAtOffset(int count, Func<int, double> offsetAt, double y)
    {
        if (offsetAt == null) throw new ArgumentNullException(nameof(offsetAt));
        if (count <= 0) return -1;

        int lo = 0, hi = count - 1, best = 0;
        while (lo <= hi)
        {
            int mid = lo + (hi - lo) / 2;
            double off = offsetAt(mid);
            // A paragraph whose layout has not resolved yet measures as NaN.
            // Answering with the best match so far beats answering with 0.
            if (double.IsNaN(off)) return best;
            if (off <= y) { best = mid; lo = mid + 1; }
            else hi = mid - 1;
        }
        return best;
    }
}
