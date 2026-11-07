using System;
using System.Collections.Generic;

namespace Dimmy.Windows.Helpers;

/// <summary>A rectangle in DIP, relative to the window's client area.</summary>
public readonly record struct DipRect(double X, double Y, double Width, double Height);

/// <summary>One piece of a window region, in physical pixels relative to the
/// window. <see cref="Round"/> is the corner ellipse size (0 = square).</summary>
public readonly record struct RegionShape(int Left, int Top, int Right, int Bottom, int Round);

/// <summary>The part of the pill window that takes clicks. The window is
/// several times larger than the pill (room for the hover capsule, the
/// vertical dock, the tip); a transparent pixel of it still takes the click,
/// so without a region the area around the pill was dead for the apps
/// underneath. Pure, so the headless test project covers it.</summary>
public static class PillHitRegion
{
    /// <summary>The pill grown by its glow (the drop shadow's blur radius, so
    /// the glow is never clipped), rounded on its short side; plus the tip's
    /// rectangle while it shows. Rounding is outward on every edge.</summary>
    public static List<RegionShape> Compute(DipRect pill, double glow, DipRect? tip, double scale)
    {
        if (!(scale > 0) || glow < 0)
            throw new ArgumentOutOfRangeException(nameof(scale), "scale must be positive, glow non-negative");
        var shapes = new List<RegionShape>();
        if (pill.Width <= 0 || pill.Height <= 0) return shapes;

        var outer = new DipRect(pill.X - glow, pill.Y - glow, pill.Width + 2 * glow, pill.Height + 2 * glow);
        int round = (int)Math.Ceiling(Math.Min(outer.Width, outer.Height) * scale);
        shapes.Add(ToPixels(outer, scale, round));
        if (tip is { Width: > 0, Height: > 0 } t)
            shapes.Add(ToPixels(t, scale, 0));
        return shapes;
    }

    private static RegionShape ToPixels(DipRect r, double scale, int round) => new(
        (int)Math.Floor(r.X * scale),
        (int)Math.Floor(r.Y * scale),
        (int)Math.Ceiling((r.X + r.Width) * scale),
        (int)Math.Ceiling((r.Y + r.Height) * scale),
        round);
}
