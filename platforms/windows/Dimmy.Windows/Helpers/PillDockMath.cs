using System;

namespace Dimmy.Windows.Helpers;

public enum PillDock { None, Left, Right, Top, Bottom, TopLeft, TopRight, BottomLeft, BottomRight }

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
