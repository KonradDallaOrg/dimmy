using System.Collections.Generic;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

/// <summary>
/// Unit coverage for the App-rule manual drag-reorder math.
///
/// Why this exists: WinUI 3 v3.1.7's built-in <c>ListView.CanReorderItems</c>
/// hard-crashes the renderer (see <c>docs/dev/known-bugs.md</c> WIN-003).
/// We replaced it with a manual pointer-driven implementation; the
/// pure index math behind it is the only part we can unit-test
/// without spinning up a XAML host. The pointer-flow itself
/// (PointerPressed → captured drag → release) needs Tier-3 manual
/// sweep — UIA3 / FlaUI doesn't drive captured-pointer drag reliably.
/// </summary>
public class AppRuleReorderMathTests
{
    // Three slots, 40 px tall each, no gaps:
    //   slot 0 → y in [0, 40)
    //   slot 1 → y in [40, 80)
    //   slot 2 → y in [80, 120)
    private static readonly IReadOnlyList<AppRuleReorderMath.Slot> ThreeSlots = new[]
    {
        new AppRuleReorderMath.Slot(0, 40),
        new AppRuleReorderMath.Slot(40, 80),
        new AppRuleReorderMath.Slot(80, 120),
    };

    // ── HitTest ────────────────────────────────────────────────────

    [Fact]
    public void HitTest_empty_slots_returns_zero_and_fallback()
    {
        var (raw, y) = AppRuleReorderMath.HitTest(50, new AppRuleReorderMath.Slot[0], fallbackY: 99);
        Assert.Equal(0, raw);
        Assert.Equal(99, y);
    }

    [Fact]
    public void HitTest_upper_half_of_first_slot_targets_above()
    {
        var (raw, y) = AppRuleReorderMath.HitTest(5, ThreeSlots, fallbackY: 200);
        Assert.Equal(0, raw);
        Assert.Equal(0, y); // top of slot 0
    }

    [Fact]
    public void HitTest_exactly_at_midline_counts_as_lower_half()
    {
        // mid of slot 0 is 20 — we use < midY for "upper half", so
        // y == 20 is lower half ⇒ insert below ⇒ raw=1, line at bottomY.
        var (raw, y) = AppRuleReorderMath.HitTest(20, ThreeSlots, fallbackY: 200);
        Assert.Equal(1, raw);
        Assert.Equal(40, y);
    }

    [Fact]
    public void HitTest_lower_half_of_middle_slot_targets_below()
    {
        var (raw, y) = AppRuleReorderMath.HitTest(70, ThreeSlots, fallbackY: 200);
        Assert.Equal(2, raw);
        Assert.Equal(80, y); // bottom of slot 1
    }

    [Fact]
    public void HitTest_past_last_slot_returns_count_and_last_bottom()
    {
        var (raw, y) = AppRuleReorderMath.HitTest(200, ThreeSlots, fallbackY: 9999);
        Assert.Equal(3, raw);            // slots.Count
        Assert.Equal(120, y);            // bottom of last rendered slot,
        // NOT the fallback — fallback is only used when slots is empty.
    }

    [Theory]
    [InlineData(0, 0, 0)]                // top edge of first slot → above
    [InlineData(39.999, 1, 40)]          // just before bottom of slot 0 (lower half) → below
    [InlineData(40, 1, 40)]              // exact top of slot 1, upper half of slot 1 → above
    [InlineData(60, 2, 80)]              // exact midline of slot 1, lower half → below
    public void HitTest_boundary_cases(double cursorY, int expectedRaw, double expectedY)
    {
        var (raw, y) = AppRuleReorderMath.HitTest(cursorY, ThreeSlots, fallbackY: 200);
        Assert.Equal(expectedRaw, raw);
        Assert.Equal(expectedY, y);
    }

    // ── AdjustedDstIndex ───────────────────────────────────────────

    [Fact]
    public void AdjustedDst_drop_above_when_src_is_below_no_shift()
    {
        // [A, B, C] → drag C (src=2) to top (rawTarget=0). Final dst=0
        // ([C, A, B]). src=2 > rawTarget=0 ⇒ NO shift.
        Assert.Equal(0, AppRuleReorderMath.AdjustedDstIndex(srcIdx: 2, rawTargetIdx: 0));
    }

    [Fact]
    public void AdjustedDst_drop_below_when_src_is_above_shifts()
    {
        // [A, B, C] → drag A (src=0) past B (rawTarget=2). Final dst=1
        // ([B, A, C]). src=0 < rawTarget=2 ⇒ shift by 1.
        Assert.Equal(1, AppRuleReorderMath.AdjustedDstIndex(srcIdx: 0, rawTargetIdx: 2));
    }

    [Fact]
    public void AdjustedDst_drop_at_end_with_src_above()
    {
        // [A, B, C, D] → drag B (src=1) to end (rawTarget=4). Final dst=3
        // ([A, C, D, B]). src=1 < 4 ⇒ shift to 3.
        Assert.Equal(3, AppRuleReorderMath.AdjustedDstIndex(srcIdx: 1, rawTargetIdx: 4));
    }

    [Fact]
    public void AdjustedDst_dropping_in_own_lower_half_is_noop()
    {
        // [A, B, C] → drag B (src=1) into B's own lower half
        // (rawTarget=2 = "below B"). After shift dst=1 = src ⇒ -1.
        Assert.Equal(-1, AppRuleReorderMath.AdjustedDstIndex(srcIdx: 1, rawTargetIdx: 2));
    }

    [Fact]
    public void AdjustedDst_dropping_at_own_upper_edge_is_noop()
    {
        // [A, B, C] → drag B (src=1) into B's own upper half
        // (rawTarget=1 = "above B"). NO shift (src not < rawTarget).
        // dst=1 = src ⇒ -1.
        Assert.Equal(-1, AppRuleReorderMath.AdjustedDstIndex(srcIdx: 1, rawTargetIdx: 1));
    }

    [Fact]
    public void AdjustedDst_negative_rawTarget_returns_neg1()
    {
        Assert.Equal(-1, AppRuleReorderMath.AdjustedDstIndex(srcIdx: 0, rawTargetIdx: -1));
    }

    [Theory]
    [InlineData(0, 1, -1)] // adjacent (src=0, raw=1, dst becomes 0 = src) ⇒ noop
    [InlineData(0, 0, -1)] // raw above src=0 (raw=0, dst=0=src) ⇒ noop
    [InlineData(2, 3, -1)] // raw just below src=2 (rawTarget=3, no shift, dst=3 ≠ src=2)
    public void AdjustedDst_adjacency_cases(int srcIdx, int rawTargetIdx, int expected)
    {
        // Last InlineData is intentionally NOT a noop — drop just past
        // src is a 1-step move down. The two -1 cases are the genuine
        // self-drops.
        if (expected == -1)
            Assert.Equal(-1, AppRuleReorderMath.AdjustedDstIndex(srcIdx, rawTargetIdx));
        else
            Assert.NotEqual(-1, AppRuleReorderMath.AdjustedDstIndex(srcIdx, rawTargetIdx));
    }

    // ── End-to-end: HitTest → AdjustedDstIndex chain ───────────────

    [Fact]
    public void EndToEnd_drag_first_to_below_last()
    {
        // [A, B, C] in ThreeSlots, src=A=0. Cursor at y=200 (past last).
        var (raw, _) = AppRuleReorderMath.HitTest(200, ThreeSlots, fallbackY: 999);
        Assert.Equal(3, raw);
        int dst = AppRuleReorderMath.AdjustedDstIndex(srcIdx: 0, rawTargetIdx: raw);
        // src=0 < rawTarget=3 ⇒ dst=2 ([B, C, A]).
        Assert.Equal(2, dst);
    }

    [Fact]
    public void EndToEnd_drag_last_above_first()
    {
        // src=C=2. Cursor at y=5 (upper half of slot 0).
        var (raw, _) = AppRuleReorderMath.HitTest(5, ThreeSlots, fallbackY: 999);
        Assert.Equal(0, raw);
        int dst = AppRuleReorderMath.AdjustedDstIndex(srcIdx: 2, rawTargetIdx: raw);
        // src=2 > rawTarget=0 ⇒ no shift ⇒ dst=0 ([C, A, B]).
        Assert.Equal(0, dst);
    }
}
