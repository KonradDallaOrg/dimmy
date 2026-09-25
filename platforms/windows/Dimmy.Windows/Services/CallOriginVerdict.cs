using System.Collections.Generic;
using System.Linq;

namespace Dimmy.Windows.Services;

/// What the audio says about the call we are recording.
///
/// Pulled out of the tick so it can be asked questions without a dispatcher,
/// a WASAPI enumeration or a meeting: deciding to stop is the one decision
/// that costs the user something, and it should be examinable on its own.
public enum CallOriginState
{
    /// The call is going on.
    OnTheCall,

    /// The call is over: the app quit, or it let go of the microphone with
    /// nothing else to explain it.
    Ended,

    /// No session of that process at all, with the devices settled. Says
    /// nothing by itself; the caller counts it toward its backstop.
    Unseen,
}

/// The decision matrix, in one place. Full write-up and the measurements
/// behind every row: docs/dev/call-detection-matrix.md
public static class CallOriginVerdict
{
    private const int Active = 1; // AudioSessionState: Inactive 0, Active 1, Expired 2

    /// `someoneElseHoldsTheMic` is Windows' own answer - the privacy
    /// indicator's source - with Dimmy itself excluded, because Dimmy is
    /// holding the microphone precisely BECAUSE it is recording this call.
    ///
    /// `deviceChanging` is true between an audio device moving and the call
    /// being seen active again.
    public static CallOriginState Judge(
        uint originPid,
        IReadOnlyList<(string sessionId, uint pid, string endpointId, int state)> sessions,
        bool processAlive,
        bool someoneElseHoldsTheMic,
        bool deviceChanging)
    {
        // Row 1. Talking. Nothing else matters.
        if (sessions.Any(x => x.pid == originPid && x.state == Active))
            return CallOriginState.OnTheCall;

        // Row 2. The app is gone. No device change excuses this.
        if (originPid != 0 && !processAlive) return CallOriginState.Ended;

        if (sessions.Any(x => x.pid == originPid && x.state != Active))
        {
            // Rows 3 and 4. Every session idle. On its own that is a hangup
            // AND a device move - the two read identically, measured over a
            // whole evening of both. So it is a hangup only when neither
            // explanation is on the table:
            //
            //   - somebody other than us still holds the microphone: the call
            //     let go of THIS device and is capturing on another one;
            //   - a device is moving: at 23:13:48, mid-move on a Meet call,
            //     the registry listed NOBODY, not even the app on the call.
            //     Either signal alone stops the recording there. Both
            //     together get it right.
            if (someoneElseHoldsTheMic || deviceChanging) return CallOriginState.OnTheCall;
            return CallOriginState.Ended;
        }

        // Rows 5 and 6. No session of that process at all.
        return deviceChanging ? CallOriginState.OnTheCall : CallOriginState.Unseen;
    }
}

/// The same judgement, watched over time.
///
/// A device change cannot be inferred from a snapshot: turn a headset on and
/// off and you are back to the devices you began with, which looks settled
/// while one just moved. Measured 2026-09-25 22:11 - the recording stopped
/// 263 ms after the devices changed, and a second one started 2 s later. So
/// the move is remembered, and forgotten when the call is seen active again
/// on whatever device it landed on.
public sealed class CallOriginTracker
{
    private bool _deviceChanging;

    public bool DeviceChanging => _deviceChanging;

    /// Called when Dimmy's own audio engine reports the devices moved, or the
    /// set of capture endpoints changed under us.
    public void NoteDeviceChange() => _deviceChanging = true;

    public void Reset() => _deviceChanging = false;

    public CallOriginState Observe(
        uint originPid,
        IReadOnlyList<(string sessionId, uint pid, string endpointId, int state)> sessions,
        bool processAlive,
        bool someoneElseHoldsTheMic)
    {
        var verdict = CallOriginVerdict.Judge(
            originPid, sessions, processAlive, someoneElseHoldsTheMic, _deviceChanging);

        // Back on a device and talking: whatever moved has landed.
        if (verdict == CallOriginState.OnTheCall
            && sessions.Any(x => x.pid == originPid && x.state == 1))
        {
            _deviceChanging = false;
        }
        return verdict;
    }
}
