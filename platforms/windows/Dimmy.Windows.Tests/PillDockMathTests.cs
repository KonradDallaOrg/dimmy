using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

/// Where a dropped pill docks. Distances are from the visible pill's centre,
/// in DIP; the work area is 1920x1040 at the origin unless stated.
public class PillDockMathTests
{
    private const double W = 1920, H = 1040;

    private static PillDock Pick(double cx, double cy, double scale = 1.0) =>
        PillDockMath.Pick(cx, cy, 0, 0, W * scale, H * scale, scale);

    [Fact]
    public void Dropped_against_a_corner_docks_to_it()
    {
        Assert.Equal(PillDock.BottomRight, Pick(W - 30, H - 30));
        Assert.Equal(PillDock.TopLeft, Pick(30, 30));
    }

    [Fact]
    public void Dropped_against_the_middle_of_an_edge_docks_to_it()
    {
        Assert.Equal(PillDock.Bottom, Pick(W / 2 + 80, H - 30));
        Assert.Equal(PillDock.Left, Pick(30, H / 2 - 100));
    }

    [Fact]
    public void A_pill_a_hand_away_from_the_edge_stays_where_it_was_dropped()
    {
        // 150 DIP from the bottom: used to dock (reach was 200 px).
        Assert.Equal(PillDock.None, Pick(W / 2, H - 150));
        Assert.Equal(PillDock.None, Pick(W - 150, H - 150));
    }

    [Fact]
    public void Along_an_edge_away_from_its_middle_and_corners_stays_put()
    {
        // At the bottom edge, 400 DIP left of centre: used to be "middle"
        // (15% of the width each side).
        Assert.Equal(PillDock.None, Pick(W / 2 - 400, H - 30));
    }

    [Fact]
    public void Reach_is_in_dip_so_a_scaled_monitor_behaves_the_same()
    {
        // 150% scaling: the same physical gesture, 1.5x the pixels.
        Assert.Equal(PillDock.BottomRight, Pick((W - 30) * 1.5, (H - 30) * 1.5, 1.5));
        Assert.Equal(PillDock.None, Pick(W / 2 * 1.5, (H - 150) * 1.5, 1.5));
    }

    [Fact]
    public void Work_area_offset_is_respected()
    {
        // A second monitor to the right of the first, taskbar on top.
        var d = PillDockMath.Pick(1920 + 30, 48 + 30, 1920, 48, 1920, 1032, 1.0);
        Assert.Equal(PillDock.TopLeft, d);
    }
}
