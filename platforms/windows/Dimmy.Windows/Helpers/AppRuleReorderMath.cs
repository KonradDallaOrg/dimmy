using System.Collections.Generic;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// Pure index math behind the AppRules manual drag-reorder. Extracted
/// out of <see cref="Dimmy.Windows.Views.SettingsWindow"/> so it's
/// unit-testable without spinning up a XAML host. The code-behind only
/// translates rendered <c>ListViewItem</c> bounds into the
/// <see cref="Slot"/> tuples this helper consumes.
///
/// See <c>docs/dev/known-bugs.md</c> WIN-003 for why the manual reorder
/// exists at all (WinUI 3 v3.1.7 built-in <c>CanReorderItems</c> hard-
/// crashes the renderer with E_UNEXPECTED).
/// </summary>
public static class AppRuleReorderMath
{
    /// <summary>One row's vertical bounds in the ListView's coordinate space.</summary>
    public readonly record struct Slot(double TopY, double BottomY);

    /// <summary>
    /// Find which insertion slot between rendered items the cursor is
    /// currently over.
    ///
    /// Returns:
    /// <list type="bullet">
    /// <item><c>RawTargetIdx</c> in <c>[0, slots.Count]</c>. <c>0</c>
    ///   means "insert above the first item"; <c>slots.Count</c>
    ///   means "drop after the last one".</item>
    /// <item><c>IndicatorY</c> — the Y coordinate where the
    ///   drop-indicator line should be drawn: top of slot
    ///   <c>RawTargetIdx</c>, or bottom of the last slot if the
    ///   cursor is past every slot. <paramref name="fallbackY"/> is
    ///   returned when <paramref name="slots"/> is empty.</item>
    /// </list>
    ///
    /// "Upper half of slot <c>i</c>" → insert above (target = i).
    /// "Lower half of slot <c>i</c>" → insert below (target = i + 1).
    /// Past all slots → target = slots.Count.
    /// </summary>
    public static (int RawTargetIdx, double IndicatorY) HitTest(
        double cursorY,
        IReadOnlyList<Slot> slots,
        double fallbackY)
    {
        if (slots == null || slots.Count == 0) return (0, fallbackY);
        for (int i = 0; i < slots.Count; i++)
        {
            var s = slots[i];
            double midY = s.TopY + (s.BottomY - s.TopY) / 2.0;
            if (cursorY < midY) return (i, s.TopY);
            if (cursorY < s.BottomY) return (i + 1, s.BottomY);
        }
        // Past all slots — anchor to the bottom of the last one.
        return (slots.Count, slots[slots.Count - 1].BottomY);
    }

    /// <summary>
    /// Convert a raw insertion index (0..N) to the index suitable for
    /// <see cref="System.Collections.ObjectModel.ObservableCollection{T}.Move"/>.
    ///
    /// <c>Move</c>'s semantics: <c>dstIdx</c> is the FINAL position the
    /// moved item will occupy. When <c>srcIdx &lt; rawTargetIdx</c>,
    /// removing src shifts the items between them down by one, so the
    /// desired final position is <c>rawTargetIdx - 1</c>.
    ///
    /// Returns <c>-1</c> when the move would be a no-op (src equals
    /// adjusted dst, or rawTargetIdx is invalid).
    /// </summary>
    public static int AdjustedDstIndex(int srcIdx, int rawTargetIdx)
    {
        if (rawTargetIdx < 0) return -1;
        int dst = rawTargetIdx;
        if (srcIdx < dst) dst -= 1;
        if (dst < 0) return -1;
        if (dst == srcIdx) return -1;
        return dst;
    }
}
