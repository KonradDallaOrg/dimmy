namespace Dimmy.Windows.Services;

/// <summary>Decides whether an update may end this process right now.
///
/// Applying an update swaps the EXE and re-spawns, so every capture
/// thread dies mid-write. The audio written so far survives — the sinks
/// flush continuously, which is the whole point of THE AUDIO RULE — but
/// the recording is cut short, never finalized, `meta.json` keeps no
/// duration, `.recording` is left behind and the meeting is orphaned
/// with no recap.
///
/// Measured on a real meeting 2026-09-04: recording started 13:50:14,
/// Velopack applied a staged update at 13:56:18, and 6 minutes of a
/// conversation ended up as an orphan. Velopack arms this by spawning
/// `Update.exe apply --waitPid &lt;us&gt;`, which lies in wait for the
/// process to exit for ANY reason — a clean quit, a crash, a kill. So
/// the guard has to sit at the moment of ARMING, not at the moment of
/// exit: once armed, we no longer control what happens.</summary>
public static class UpdateGate
{
    /// <summary>`meetingActiveRc` is the return of
    /// `dimmy_meeting_is_active()`: 1 active, 0 idle, negative on a lock
    /// failure.
    ///
    /// Only an explicit 0 permits the update. A lock failure — or any
    /// value we did not expect — counts as recording: when we cannot
    /// tell whether audio is being captured, the safe answer is the one
    /// that cannot destroy it. An update always waits; a lost meeting
    /// never comes back.</summary>
    public static bool MayEndProcess(int meetingActiveRc) => meetingActiveRc == 0;
}
