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

    // ── Rows 2b and 4b: the call's playback and media (2026-11-02) ──

    /// Measured 16:52:52: microphone switched from Teams' own menu. Every
    /// Teams capture session Inactive, the registry naming nobody but
    /// Dimmy, no device event - and Teams still playing the call. The old
    /// matrix stopped the recording here and a new one started 1.1 s later.
    [Fact]
    public void SwitchingTheMicrophoneInsideTheAppIsNotAHangup()
        => Assert.Equal(CallOriginState.OnTheCall, CallOriginVerdict.Judge(
            42, Sessions((42, "{headset}", Inactive), (42, "{laptop}", Inactive)),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: false,
            callStillPlaying: true));

    /// Mid-switch the session can be missing altogether.
    [Fact]
    public void NoSessionWhileTheCallStillPlaysIsInTransit()
        => Assert.Equal(CallOriginState.OnTheCall, CallOriginVerdict.Judge(
            42, Sessions(),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: false,
            callStillPlaying: true));

    /// Measured 17:05:03-17:05:13: playback stopped, 9 of 11 UDP sockets
    /// closed, then the microphone was released.
    [Fact]
    public void HangingUpWithPlaybackAndMediaGoneEnds()
        => Assert.Equal(CallOriginState.Ended, CallOriginVerdict.Judge(
            42, Sessions((42, "{headset}", Inactive)),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: false,
            callStillPlaying: false, callMediaClosed: true));

    /// A browser keeps playing another tab after the call: the media
    /// closing still ends it.
    [Fact]
    public void ClosedMediaEndsTheCallEvenIfTheAppKeepsPlaying()
        => Assert.Equal(CallOriginState.Ended, CallOriginVerdict.Judge(
            42, Sessions((42, "{headset}", Inactive)),
            processAlive: true, someoneElseHoldsTheMic: true, deviceChanging: false,
            callStillPlaying: true, callMediaClosed: true));

    /// Talking wins over everything, closed media included.
    [Fact]
    public void ClosedMediaDoesNotEndACallThatIsCapturing()
        => Assert.Equal(CallOriginState.OnTheCall, CallOriginVerdict.Judge(
            42, Sessions((42, "{headset}", Active)),
            processAlive: true, someoneElseHoldsTheMic: false, deviceChanging: false,
            callStillPlaying: false, callMediaClosed: true));

    // ── CallMediaSockets.Closed ──────────────────────────────────────
    private static List<(uint, ushort)> Ports(params ushort[] ports)
    {
        var list = new List<(uint, ushort)>();
        foreach (var p in ports) list.Add((33224, p));
        return list;
    }

    /// 17:05:05: 11 sockets while talking, 2 left (the two Teams holds all day).
    [Fact]
    public void HangupClosesTheMediaSockets()
        => Assert.True(CallMediaSockets.Closed(
            Ports(50005, 50014, 50023, 50024, 50045, 50051, 50054, 50058, 55832, 50086, 64966),
            Ports(50086, 64966)));

    /// 17:04:50: the same 11 sockets across the in-app switch.
    [Fact]
    public void AnInAppSwitchKeepsTheMediaSockets()
    {
        var talking = Ports(50005, 50014, 50023, 50024, 50045, 50051, 50054, 50058, 55832, 50086, 64966);
        Assert.False(CallMediaSockets.Closed(talking, talking));
    }

    /// A call relayed over TCP has only the app's everyday sockets: two of
    /// them closing is churn, not a hangup.
    [Fact]
    public void ChurnOnAFewEverydaySocketsIsNotAHangup()
        => Assert.False(CallMediaSockets.Closed(Ports(50086, 64966), Ports()));

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
