using System.Collections.Generic;
using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests;

/// When a recording we started should stop. One test per row of the decision
/// matrix in docs/dev/call-detection-matrix.md; every number and every case
/// below came off a real log on 2026-09-25.
///
/// Stopping is the expensive mistake. A recording that stops halfway leaves
/// two files where there was one conversation, and the second starts from
/// silence. So a reading that could mean "the call ended" is only believed
/// when nothing else can explain it.
public class CallOriginVerdictTests
{
    private const int Inactive = 0;
    private const int Active = 1;

    private static List<(string, uint, string, int)> Sessions(params (uint pid, string ep, int state)[] rows)
    {
        var list = new List<(string, uint, string, int)>();
        foreach (var r in rows) list.Add(($"sess-{r.pid}@{r.ep}", r.pid, r.ep, r.state));
        return list;
    }

    // ── Row 1: a session is Active ──────────────────────────────────
    [Fact]
    public void AnActiveSessionMeansTheCallIsStillGoing()
        => Assert.Equal(CallOriginState.OnTheCall, CallOriginVerdict.Judge(
            42, Sessions((42, "{speakers}", Active)),
            processAlive: true, someoneElseHoldsTheMic: true, deviceChanging: false));

    // ── Row 2: the process is gone ──────────────────────────────────
    [Fact]
    public void ClosingTheAppEndsIt()
        => Assert.Equal(CallOriginState.Ended, CallOriginVerdict.Judge(
            42, Sessions(),
            processAlive: false, someoneElseHoldsTheMic: false, deviceChanging: false));

    /// A device change must never swallow a quit.
    [Fact]
    public void QuittingTheAppEndsItEvenMidMove()
        => Assert.Equal(CallOriginState.Ended, CallOriginVerdict.Judge(
            42, Sessions(),
            processAlive: false, someoneElseHoldsTheMic: true, deviceChanging: true));

    // ── Row 3: everything idle, nothing explains it ─────────────────
    /// Measured 23:12:48, 23:13:25 and 23:13:56 - three hangups, three
    /// identical pictures: every session Inactive and the only app left
    /// holding the microphone is Dimmy itself.
    [Fact]
    public void HangingUpEndsIt()
        => Assert.Equal(CallOriginState.Ended, CallOriginVerdict.Judge(
            42, Sessions((42, "{speakers}", Inactive), (42, "{headset}", Inactive)),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: false));

    // ── Row 4: idle, but something explains it ──────────────────────
    /// The call let go of this device and is capturing on another one.
    /// Windows still lists it as holding the microphone.
    [Fact]
    public void StillHoldingTheMicrophoneIsNotAHangup()
        => Assert.Equal(CallOriginState.OnTheCall, CallOriginVerdict.Judge(
            42, Sessions((42, "{speakers}", Inactive)),
            processAlive: true, someoneElseHoldsTheMic: true, deviceChanging: false));

    /// THE CASE THAT GOT AWAY. Measured 23:13:48 during a Meet call, while a
    /// device was changing: every session Inactive AND the registry listed
    /// nobody at all - not even the app on the call. Believing either signal
    /// on its own stops the recording here. Only both together get it right.
    [Fact]
    public void AnEmptyMicrophoneListDuringADeviceChangeIsNotAHangup()
        => Assert.Equal(CallOriginState.OnTheCall, CallOriginVerdict.Judge(
            42, Sessions((42, "{speakers}", Inactive)),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: true));

    // ── Rows 5 and 6: no session of that process at all ─────────────
    [Fact]
    public void NoSessionWithTheDevicesSettledConcludesNothing()
        => Assert.Equal(CallOriginState.Unseen, CallOriginVerdict.Judge(
            42, Sessions(),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: false));

    [Fact]
    public void NoSessionMidMoveMeansItIsInTransit()
        => Assert.Equal(CallOriginState.OnTheCall, CallOriginVerdict.Judge(
            42, Sessions(),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: true));

    // ── The sequences, as they happen ───────────────────────────────

    /// Measured 23:11:15 to 23:12:48: one call, four device changes, ONE
    /// recording. Headset on, headset off, Teams switched from headset to
    /// Realtek and back.
    [Fact]
    public void AHeadsetGoingOnAndOffMidCallKeepsOneRecording()
    {
        var t = new CallOriginTracker();
        Assert.Equal(CallOriginState.OnTheCall,
            t.Observe(42, Sessions((42, "{speakers}", Active)), true, true));

        // Headset on: the old device goes idle, the registry still has it.
        t.NoteDeviceChange();
        Assert.Equal(CallOriginState.OnTheCall,
            t.Observe(42, Sessions((42, "{speakers}", Inactive)), true, true));
        Assert.Equal(CallOriginState.OnTheCall,
            t.Observe(42, Sessions((42, "{headset}", Active)), true, true));

        // Headset off: back to the devices it started on, which is why
        // comparing against the starting set could never work.
        t.NoteDeviceChange();
        Assert.Equal(CallOriginState.OnTheCall,
            t.Observe(42, Sessions((42, "{headset}", Inactive)), true, false));
        Assert.Equal(CallOriginState.OnTheCall,
            t.Observe(42, Sessions((42, "{speakers}", Active)), true, true));

        // And now a real hangup, with the devices settled.
        Assert.Equal(CallOriginState.Ended,
            t.Observe(42, Sessions((42, "{speakers}", Inactive)), true, false));
    }

    /// The window a device change opens has to close, or one headset switch
    /// would make the recording immortal.
    [Fact]
    public void SeeingTheCallActiveAgainClosesTheDeviceChangeWindow()
    {
        var t = new CallOriginTracker();
        t.NoteDeviceChange();
        Assert.True(t.DeviceChanging);
        t.Observe(42, Sessions((42, "{headset}", Active)), true, true);
        Assert.False(t.DeviceChanging);
    }
}
