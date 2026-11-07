using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

/// The part of the pill window that takes clicks: the pill and its glow, plus
/// the tip while it shows. Everything else in the (much larger) window must let
/// clicks reach the app underneath.
public class PillHitRegionTests
{
    [Fact]
    public void The_pill_grows_by_its_glow_and_keeps_its_round_shape()
    {
        // 36 DIP circle at (102,26) in a 240x88 window, glow 20, 100% scale.
        var shapes = PillHitRegion.Compute(new DipRect(102, 26, 36, 36), glow: 20, tip: null, scale: 1.0);
        var pill = Assert.Single(shapes);
        Assert.Equal(new RegionShape(82, 6, 158, 82, 76), pill);
    }

    [Fact]
    public void A_capsule_rounds_on_its_short_side()
    {
        var shapes = PillHitRegion.Compute(new DipRect(40, 24, 160, 40), glow: 10, tip: null, scale: 1.0);
        Assert.Equal(new RegionShape(30, 14, 210, 74, 60), Assert.Single(shapes));
    }

    [Fact]
    public void Scaling_converts_dip_to_pixels_and_never_cuts_inward()
    {
        // 125%: 102.4 -> floor, right edge -> ceil, so the glow is never clipped.
        var s = Assert.Single(PillHitRegion.Compute(new DipRect(102.3, 26, 36, 36), glow: 20, tip: null, scale: 1.25));
        Assert.Equal(102, s.Left);   // floor((102.3-20)*1.25) = floor(102.875)
        Assert.Equal(198, s.Right);  // ceil((102.3+36+20)*1.25) = ceil(197.875)
    }

    [Fact]
    public void The_tip_joins_the_region_while_it_shows()
    {
        var shapes = PillHitRegion.Compute(new DipRect(102, 40, 36, 36), glow: 10,
            tip: new DipRect(80, 2, 80, 24), scale: 1.0);
        Assert.Equal(2, shapes.Count);
        Assert.Equal(new RegionShape(80, 2, 160, 26, 0), shapes[1]);
    }

    [Fact]
    public void Nothing_laid_out_yet_means_no_region()
    {
        Assert.Empty(PillHitRegion.Compute(new DipRect(0, 0, 0, 0), glow: 20, tip: null, scale: 1.0));
    }
}
