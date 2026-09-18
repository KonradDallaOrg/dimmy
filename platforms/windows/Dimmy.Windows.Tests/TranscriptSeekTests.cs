using System;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

/// Scrolling the transcript moves the playhead. Which turn the playhead lands
/// on is this mapping, and when it picks the wrong one the waveform and the
/// text read as two unrelated things. Mirror of the Mac
/// `TranscriptSeekTests.testSeekLandsOnTheLineBeingSpoken` for the opposite
/// direction.
public class TranscriptSeekTests
{
    // A transcript of five turns, 40 px apart, as laid out in the scroller.
    private static readonly double[] Offsets = { 0, 40, 80, 120, 160 };
    private static double At(int i) => Offsets[i];

    [Fact]
    public void ScrollingToATurnPicksThatTurn()
    {
        Assert.Equal(2, TranscriptSeek.IndexAtOffset(Offsets.Length, At, 80));
        Assert.Equal(4, TranscriptSeek.IndexAtOffset(Offsets.Length, At, 160));
    }

    [Fact]
    public void AViewportInsideATurnStaysOnIt()
    {
        // Half-way down turn 2: still turn 2, not the one below.
        Assert.Equal(2, TranscriptSeek.IndexAtOffset(Offsets.Length, At, 119));
    }

    [Fact]
    public void ScrolledPastTheEndStaysOnTheLastTurn()
    {
        Assert.Equal(4, TranscriptSeek.IndexAtOffset(Offsets.Length, At, 10_000));
    }

    [Fact]
    public void AboveTheFirstTurnFallsBackToIt()
    {
        // Same fallback as the seek → scroll direction (`hit ??= anchors[0]`).
        Assert.Equal(0, TranscriptSeek.IndexAtOffset(Offsets.Length, At, -50));
    }

    [Fact]
    public void NoTurnsMeansNothingToSeekTo()
    {
        Assert.Equal(-1, TranscriptSeek.IndexAtOffset(0, _ => 0, 12));
    }

    [Fact]
    public void AnUnresolvedLayoutDoesNotThrowAwayWhatWasMeasured()
    {
        double AtWithGap(int i) => i >= 3 ? double.NaN : Offsets[i];
        Assert.Equal(2, TranscriptSeek.IndexAtOffset(Offsets.Length, AtWithGap, 200));
    }
}
