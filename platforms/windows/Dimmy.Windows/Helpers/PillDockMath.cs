using System;
using System.Collections.Generic;
using System.Linq;

namespace Dimmy.Windows.Helpers;

public enum PillDock { None, Left, Right, Top, Bottom, TopLeft, TopRight, BottomLeft, BottomRight }

/// <summary>A rectangle in physical pixels, screen coordinates.</summary>
public readonly record struct ZoneRect(double Left, double Top, double Right, double Bottom);

/// <summary>Where one dock's zone is drawn while the pill is dragged.</summary>
public readonly record struct DockZone(PillDock Dock, ZoneRect Rect);

/// <summary>Where a dropped pill docks: a corner, the middle of an edge, or
/// nowhere (it stays where it was dropped). Pure, so the headless test project
/// covers it. The macOS mirror is <c>PillWindowController.dock(…)</c>: same
/// rule, same numbers in points.</summary>
public static class PillDockMath
{
    /// <summary>DIP between the visible pill's centre and an edge that still
    /// counts as "against" it: the pill itself ends up within ~45 DIP.</summary>
    public const double EdgeReach = 64;

    /// <summary>DIP either side of an edge's middle that docks to that middle.</summary>
    public const double MiddleBand = 120;

    /// <summary>DIP: the resting pill is a 36 DIP circle.</summary>
    public const double PillRadius = 18;

    /// <summary>DIP from a dock zone at which its outline appears during a
    /// drag: wide enough to show where the zone is before the pill is in it.</summary>
    public const double ApproachReach = 3 * EdgeReach;

    /// <summary>Where the pill's CENTRE has to be for each dock, the exact
    /// region <see cref="Pick"/> answers that dock for.</summary>
    private static IEnumerable<(PillDock Dock, ZoneRect Centre)> CentreZones(
        double waX, double waY, double waW, double waH, double scale)
    {
        double r = EdgeReach * scale, b = MiddleBand * scale;
        double right = waX + waW, bottom = waY + waH, mx = waX + waW / 2, my = waY + waH / 2;
        yield return (PillDock.TopLeft, new ZoneRect(waX, waY, waX + r, waY + r));
        yield return (PillDock.TopRight, new ZoneRect(right - r, waY, right, waY + r));
        yield return (PillDock.BottomLeft, new ZoneRect(waX, bottom - r, waX + r, bottom));
        yield return (PillDock.BottomRight, new ZoneRect(right - r, bottom - r, right, bottom));
        yield return (PillDock.Top, new ZoneRect(mx - b, waY, mx + b, waY + r));
        yield return (PillDock.Bottom, new ZoneRect(mx - b, bottom - r, mx + b, bottom));
        yield return (PillDock.Left, new ZoneRect(waX, my - b, waX + r, my + b));
        yield return (PillDock.Right, new ZoneRect(right - r, my - b, right, my + b));
    }

    /// <summary>The zones to draw: each centre zone grown by the pill's radius
    /// (the area the docked pill covers) and kept on screen.</summary>
    public static List<DockZone> Zones(double waX, double waY, double waW, double waH, double scale)
    {
        double g = PillRadius * scale;
        return CentreZones(waX, waY, waW, waH, scale)
            .Select(z => new DockZone(z.Dock, new ZoneRect(
                Math.Max(waX, z.Centre.Left - g), Math.Max(waY, z.Centre.Top - g),
                Math.Min(waX + waW, z.Centre.Right + g), Math.Min(waY + waH, z.Centre.Bottom + g))))
            .ToList();
    }

    /// <summary>The docks whose zone the pill (centre at cx, cy) is close
    /// enough to for its outline to show.</summary>
    public static List<PillDock> Near(double cx, double cy, double waX, double waY, double waW, double waH, double scale)
    {
        double reach = ApproachReach * scale;
        return CentreZones(waX, waY, waW, waH, scale)
            .Where(z =>
            {
                double dx = Math.Max(0, Math.Max(z.Centre.Left - cx, cx - z.Centre.Right));
                double dy = Math.Max(0, Math.Max(z.Centre.Top - cy, cy - z.Centre.Bottom));
                return Math.Sqrt(dx * dx + dy * dy) < reach;
            })
            .Select(z => z.Dock)
            .ToList();
    }

    /// <summary>All coordinates in physical pixels of one monitor; <paramref name="scale"/>
    /// is its DPI / 96. <paramref name="cx"/>, <paramref name="cy"/> are the
    /// visible pill's centre, not the window's: the window is larger and the
    /// pill sits against one of its sides when docked.</summary>
    public static PillDock Pick(double cx, double cy, double waX, double waY, double waW, double waH, double scale)
    {
        if (!(scale > 0) || !(waW > 0) || !(waH > 0))
            throw new ArgumentOutOfRangeException(nameof(scale), "work area and scale must be positive");
        double reach = EdgeReach * scale, band = MiddleBand * scale;
        double dl = cx - waX, dr = waX + waW - cx, dt = cy - waY, db = waY + waH - cy;

        bool nearX = Math.Min(dl, dr) < reach, nearY = Math.Min(dt, db) < reach;
        if (nearX && nearY)
            return (dt < db, dl < dr) switch
            {
                (true, true) => PillDock.TopLeft,
                (true, false) => PillDock.TopRight,
                (false, true) => PillDock.BottomLeft,
                _ => PillDock.BottomRight,
            };

        bool middleX = Math.Abs(cx - (waX + waW / 2)) < band;
        bool middleY = Math.Abs(cy - (waY + waH / 2)) < band;
        if (nearY && middleX) return dt < db ? PillDock.Top : PillDock.Bottom;
        if (nearX && middleY) return dl < dr ? PillDock.Left : PillDock.Right;
        return PillDock.None;
    }
}
