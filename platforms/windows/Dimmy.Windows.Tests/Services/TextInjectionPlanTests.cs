using System.Linq;
using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

/// <summary>
/// Tests for the SendCtrlC plan builder — the ordering rules that
/// load-bear on the dictionary-hotkey path. Real SendInput is a Win32
/// side-effect, so the plan builder was extracted as a pure function:
/// these tests exercise the rules in isolation.
///
/// The "351-char stale clipboard read" bug that drove this design
/// landed because synthetic Ctrl+C was sent while Shift was physically
/// held → app saw Ctrl+Shift+C → no copy → we read the previous
/// (stale) clipboard contents. The plan must release ANY held
/// modifier BEFORE issuing the Ctrl+C key pair, and it must do so in
/// a single SendInput batch (Win32 guarantees no-interleave inside
/// one call — separate calls were racing).
/// </summary>
public class TextInjectionPlanTests
{
    // Reference VKs (kept literal to avoid coupling tests to private
    // constants — the values themselves are part of the contract).
    private const ushort VK_LWIN = 0x5B;
    private const ushort VK_LSHIFT = 0xA0;
    private const ushort VK_LCONTROL = 0xA2;
    private const ushort VK_LMENU = 0xA4;
    private const ushort VK_CONTROL = 0x11;
    private const ushort VK_C = 0x43;

    // ── Baseline: no modifiers held ─────────────────────────────────

    [Fact]
    public void Plan_NoModsHeld_EmitsExactlyCtrlCPair()
    {
        var plan = TextInjectionService.PlanCtrlCBatch(System.Array.Empty<ushort>());
        Assert.Equal(4, plan.Count);
        Assert.Equal((VK_CONTROL, false), plan[0]);
        Assert.Equal((VK_C, false), plan[1]);
        Assert.Equal((VK_C, true), plan[2]);
        Assert.Equal((VK_CONTROL, true), plan[3]);
    }

    [Fact]
    public void Plan_NullHeld_TreatedAsEmpty()
    {
        // Defensive: callers pass `null` if they short-circuit the
        // GetAsyncKeyState scan (e.g. on test paths). Plan must not
        // throw — produces the baseline Ctrl+C batch.
        var plan = TextInjectionService.PlanCtrlCBatch(null!);
        Assert.Equal(4, plan.Count);
    }

    // ── Phantom-modifier release path (the bug we're guarding) ──────

    [Fact]
    public void Plan_OneHeldMod_IsReleasedBeforeCtrlC()
    {
        var plan = TextInjectionService.PlanCtrlCBatch(new ushort[] { VK_LSHIFT });
        Assert.Equal(5, plan.Count);
        // Shift release first.
        Assert.Equal((VK_LSHIFT, true), plan[0]);
        // Then the canonical Ctrl+C pair.
        Assert.Equal((VK_CONTROL, false), plan[1]);
        Assert.Equal((VK_C, false), plan[2]);
        Assert.Equal((VK_C, true), plan[3]);
        Assert.Equal((VK_CONTROL, true), plan[4]);
    }

    [Fact]
    public void Plan_MultipleHeldMods_AllReleasedBeforeCtrlC()
    {
        var held = new ushort[] { VK_LWIN, VK_LSHIFT, VK_LMENU };
        var plan = TextInjectionService.PlanCtrlCBatch(held);
        Assert.Equal(held.Length + 4, plan.Count);

        // Every release index must be < every Ctrl/C index. Index of
        // the first non-release entry == held.Length.
        for (int i = 0; i < held.Length; i++)
            Assert.True(plan[i].IsUp, $"plan[{i}] must be a release");
        Assert.False(plan[held.Length].IsUp);
        Assert.Equal(VK_CONTROL, plan[held.Length].Vk);
    }

    [Fact]
    public void Plan_ReleaseOrder_PreservesCallerOrder()
    {
        // Stable order matters when the user has explicit preferences
        // (e.g. always release Win first to avoid triggering the Start
        // menu). Caller controls — we just preserve.
        var held = new ushort[] { VK_LMENU, VK_LCONTROL, VK_LWIN };
        var plan = TextInjectionService.PlanCtrlCBatch(held);
        Assert.Equal((VK_LMENU, true), plan[0]);
        Assert.Equal((VK_LCONTROL, true), plan[1]);
        Assert.Equal((VK_LWIN, true), plan[2]);
    }

    // ── Negative-space invariants the dict path relies on ───────────

    [Fact]
    public void Plan_LastEntry_IsAlwaysCtrlUp()
    {
        // If Ctrl ever ends up still down at the end of the batch the
        // next user keypress is interpreted as a chord. Hard invariant.
        foreach (var held in new[] {
            System.Array.Empty<ushort>(),
            new ushort[] { VK_LSHIFT },
            new ushort[] { VK_LWIN, VK_LSHIFT, VK_LMENU, VK_LCONTROL },
        })
        {
            var plan = TextInjectionService.PlanCtrlCBatch(held);
            Assert.Equal((VK_CONTROL, true), plan[^1]);
        }
    }

    [Fact]
    public void Plan_EveryKey_HasBalancedDownAndUp()
    {
        // For every (vk, false) there must be a matching (vk, true).
        // Releases of held modifiers are unbalanced by design (we
        // never re-pressed them), so we only enforce balance on the
        // synthetic Ctrl+C events.
        var plan = TextInjectionService.PlanCtrlCBatch(new ushort[] { VK_LSHIFT });
        var downs = plan.Where(p => !p.IsUp).Select(p => p.Vk).ToHashSet();
        var ups = plan.Where(p => p.IsUp).Select(p => p.Vk).ToHashSet();
        // Ctrl and C both pressed down → both must come back up.
        Assert.Contains(VK_CONTROL, downs);
        Assert.Contains(VK_C, downs);
        Assert.Contains(VK_CONTROL, ups);
        Assert.Contains(VK_C, ups);
    }

    [Fact]
    public void Plan_CKeyDown_IsAfterCtrlDown()
    {
        // C-down must arrive after Ctrl-down — otherwise the target
        // app sees a bare "C" first, which lands as literal-text in
        // any focused TextBox (corrupting the user's selection).
        var plan = TextInjectionService.PlanCtrlCBatch(new ushort[] { VK_LSHIFT });
        int ctrlDownIdx = plan.ToList().FindIndex(p => p.Vk == VK_CONTROL && !p.IsUp);
        int cDownIdx = plan.ToList().FindIndex(p => p.Vk == VK_C && !p.IsUp);
        Assert.True(ctrlDownIdx < cDownIdx, "Ctrl-down must precede C-down");
    }

    [Fact]
    public void Plan_CKeyUp_IsBeforeCtrlUp()
    {
        // Releasing Ctrl before C means the app briefly sees a bare
        // C-keypress — same corruption risk.
        var plan = TextInjectionService.PlanCtrlCBatch(System.Array.Empty<ushort>());
        int ctrlUpIdx = plan.ToList().FindIndex(p => p.Vk == VK_CONTROL && p.IsUp);
        int cUpIdx = plan.ToList().FindIndex(p => p.Vk == VK_C && p.IsUp);
        Assert.True(cUpIdx < ctrlUpIdx, "C-up must precede Ctrl-up");
    }
}
