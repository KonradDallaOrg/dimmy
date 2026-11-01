using System.Linq;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

public class LiveWaveformLayoutTests
{
    private const double SampleMs = 83, Pitch = 6, Width = 600, Slack = 2;

    private static double OnScreen(long k, double elapsedMs, double originMs) =>
        LiveWaveformLayout.BarX(k, SampleMs, Pitch, Width, Slack, originMs)
        + LiveWaveformLayout.LayerX(elapsedMs, SampleMs, Pitch, originMs);

    [Fact]
    public void ANewBarIsLaidPastTheRightEdge() =>
        Assert.True(OnScreen(100, 100 * SampleMs, 0) >= Width + Pitch);

    [Fact]
    public void ABarMovesAtConstantSpeed()
    {
        double a = OnScreen(100, 100 * SampleMs, 0);
        double b = OnScreen(100, 101 * SampleMs, 0);
        double c = OnScreen(100, 102 * SampleMs, 0);
        Assert.Equal(-Pitch, b - a, 6);
        Assert.Equal(-Pitch, c - b, 6);
    }

    [Fact]
    public void RestartingTheLayerDoesNotMoveABarOnScreen()
    {
        const double now = 3_600_123;
        Assert.Equal(OnScreen(43000, now, 0), OnScreen(43000, now, now), 6);
    }

    [Fact]
    public void OldestBarIsPastTheLeftEdgeWhenReused()
    {
        int n = LiveWaveformLayout.Capacity(Width, Pitch, Slack, 20, 240);
        long newest = 1000;
        long reused = newest - n;
        Assert.True(OnScreen(reused, newest * SampleMs, 0) + 4 <= 0);
    }

    [Fact]
    public void ARingNeverGivesTwoLiveSamplesTheSameRectangle()
    {
        var used = Enumerable.Range(0, 50).Select(i => LiveWaveformLayout.RingIndex(3 - i, 50)).ToList();
        Assert.Equal(50, used.Distinct().Count());
        Assert.All(used, i => Assert.InRange(i, 0, 49));
    }
}
