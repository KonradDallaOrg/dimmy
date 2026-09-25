using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Runtime.InteropServices;
using Microsoft.UI.Dispatching;

using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// Fast-poll capture-session watcher with session-instance-id
/// one-shot semantics. Mirrors the approach Notion + most other
/// "detect a meeting started" apps take on Windows — because
/// `IAudioSessionNotification` (the event-driven alternative) only
/// fires for sessions created via the legacy `IAudioSessionManager
/// ::GetAudioSessionControl` path. ALL modern apps (Teams, Zoom,
/// browser meetings) open their streams via `IAudioClient::Initialize`,
/// which bypasses the notification source. Confirmed empirically
/// 2026-05-22: registering OnSessionCreated and joining 3 Teams
/// calls produced zero callbacks.
///
/// Polling at 4 Hz (250 ms) keeps perceived latency low enough that
/// the nudge appears within a beat of joining a call, while still
/// costing ~50 µs per tick (WASAPI enumeration is cheap). The
/// session-instance-id tracking is what makes it FEEL event-driven
/// to the user: emit once per new GUID, never again for the same
/// GUID, re-emit when the GUID disappears from the active set
/// (== call ended) and a new one appears (== next call).
///
/// Generic app discovery — there is NO hardcoded list of "VoIP apps".
/// Whatever exe owns a new session becomes the cooldown / exclusion
/// key. The `SystemExesToIgnore` filter keeps Windows itself out
/// (audiodg, dwm, ourselves). The user's "Not now" / "Never"
/// interactions populate per-app behaviour from there.
internal sealed class CallDetectionService : IDisposable
{
    private readonly DispatcherQueue _dispatcher;
    private DispatcherQueueTimer? _timer;
    private bool _disposed;
    private bool _isEnabled;

    /// Active session-instance-ids we've already emitted a
    /// `call_signal(1, exe)` for. Cleared on disappearance from the
    /// enumeration → next call by the same app gets a new GUID and
    /// re-emits.
    private readonly Dictionary<string, string> _emittedSessions =
        new(StringComparer.Ordinal); // sessionId -> exe

    /// Sessions seen for the first time, awaiting one confirm tick
    /// before we promote them. Filters out ~50 ms Windows
    /// notification chirps that briefly grab the mic.
    private readonly Dictionary<string, int> _pendingSessions =
        new(StringComparer.Ordinal); // sessionId -> tick-count seen
    private const int PromoteAfterTicks = 1; // 250 ms × 1 = 250 ms confirm

    /// Last exe we emitted a positive call_signal for. The
    /// meeting-active branch reuses it so cooldown keys stay stable
    /// across the recording.
    private string? _lastEmittedExe;

    /// Session-instance-id of the originating call when the user
    /// accepted the "Record now" nudge. Set by
    /// `MarkMeetingOriginFromCurrentSession()` and consumed in the
    /// meeting-active tick branch: as long as this session is still
    /// alive in the enumeration, the call is on-going; the moment
    /// it disappears we know the call ended (deterministic, no
    /// silence-heuristic). Cleared on meeting-stop transition.
    private string? _meetingOriginSessionId;
    /// True iff the active meeting was started in response to a
    /// call-detect nudge (vs the user opening the meeting window
    /// manually). When true, the OnTick meeting branch trusts the
    /// session-id signal exclusively; when false it falls back to
    /// the amplitude heuristic. Mutually exclusive — no overlap.
    private bool _meetingDrivenByCallDetect;
    /// Previous-tick value of `meetingActive` so we can detect the
    /// active→inactive edge and clear the origin tracker once a
    /// recording ends (regardless of whether stop was suggested by
    /// us or initiated manually).
    private bool _prevMeetingActive;

    /// Exe names (lowercase, no `.exe`) that own audio capture without
    /// representing a real call. Filter, not whitelist — anything not
    /// in here is treated as a candidate. Dimmy itself MUST be here
    /// or the pill's own cpal mic stream would self-nudge.
    private static readonly HashSet<string> SystemExesToIgnore =
        new(StringComparer.OrdinalIgnoreCase)
        {
            "audiodg",
            "svchost",
            "dwm",
            "csrss",
            "winlogon",
            "smss",
            "runtimebroker",
            "applicationframehost",
            "shellexperiencehost",
            "searchhost",
            "searchindexer",
            "searchapp",
            "systemsettings",
            "ctfmon",
            "explorer",
            "dimmy",
            "dimmy.windows",
        };

    private int _logSuppressCounter;

    public CallDetectionService(DispatcherQueue dispatcher)
    {
        _dispatcher = dispatcher ?? throw new ArgumentNullException(nameof(dispatcher));
    }

    /// How long the 4 Hz sampling keeps running after the last app lets go of
    /// the microphone. The stop path draws its conclusion from what it can no
    /// longer see, so it needs samples AFTER the call ended, not just before.
    private static readonly TimeSpan IdleGrace = TimeSpan.FromSeconds(5);
    private MicUsageWatcher? _micWatcher;
    private DateTime? _micFreeSince;

    public void Start()
    {
        if (_timer != null) return;
        _isEnabled = true;
        _timer = _dispatcher.CreateTimer();
        _timer.Interval = TimeSpan.FromMilliseconds(250);
        _timer.Tick += OnTick;

        // The sampling itself stays exactly as it was — the promote rule needs
        // two sightings 250 ms apart, and the stop path needs to watch the
        // origin disappear. What changes is WHEN it runs: the registry tells
        // us the moment somebody picks the microphone up, so there is no
        // reason to enumerate audio endpoints four times a second through the
        // hours when nobody is on a call. Measured cost of a sample on this
        // machine: 4-6 ms typical, 50 ms worst seen, on the UI thread.
        _micWatcher = new MicUsageWatcher();
        _micWatcher.Changed += OnMicUsageChanged;
        if (_micWatcher.Start())
        {
            // Somebody may already be on a call when Dimmy starts. One read,
            // not a timer.
            if (MicUsageWatcher.AnyoneUsingMic())
            {
                StartSampling("a call is already in progress");
            }
            else
            {
                SignalMicFree("nobody is holding the microphone at startup");
                try { DimmyNative.dimmy_call_mic_confirmed_free(); } catch { }
                App.Log("started — idle, waiting for the microphone to be picked up", "CallDetect");
            }
        }
        else
        {
            // The watcher is an optimisation, never a dependency. If the
            // privacy key cannot be watched we behave exactly like before.
            _micWatcher.Dispose();
            _micWatcher = null;
            _timer.Start();
            App.Log("started (4 Hz poll, no mic watcher)", "CallDetect");
        }
    }

    /// Dimmy's own audio engine reports the devices moved. It watches the
    /// default input and output for its OWN capture, so it is the same fact
    /// arriving from a different door - and it arrives about a second and a
    /// half before the call's session goes idle, measured three times over
    /// on 2026-09-25 (21:57:31 then 21:57:33, 22:11:45 then 22:11:46,
    /// 22:37:33 then 22:37:35).
    public void NoteAudioDeviceChanged(string trigger)
    {
        _dispatcher.TryEnqueue(() =>
        {
            if (_disposed) return;
            _originTracker.NoteDeviceChange();
            App.Log($"audio devices moved ({trigger}) — an idle session means nothing until it lands",
                "CallDetect");
        });
    }

    private void OnMicUsageChanged(bool anyoneUsing)
    {
        // Raised on the watcher thread; everything below touches state the
        // dispatcher owns.
        _dispatcher.TryEnqueue(() =>
        {
            if (_disposed || !_isEnabled) return;
            if (anyoneUsing)
            {
                _micFreeSince = null;
                StartSampling("microphone picked up");
            }
            else if (_micFreeSince == null)
            {
                // Do NOT stop here. The stop path concludes the call is over
                // by no longer seeing the origin, which takes samples it has
                // not taken yet.
                // Deliberately NOT signalling the core here. It already
                // learns the microphone is free from the tick, which sends
                // signal(0) when the sessions disappear - once per call, at
                // the end of it.
                //
                // Signalling on this edge as well restarted the core's
                // release clock, and that clock is what decides whether the
                // next call is a new call or the tail of the one just
                // stopped: it wants ten quiet seconds. The registry emits
                // spurious free/busy edges - a browser probing permission,
                // Teams reopening its session - and one of those landing
                // shortly before a real call reset the count to zero and got
                // the call thrown away. Measured 2026-09-25: free at
                // 18:04:18, free AGAIN at 18:04:29.280, call at 18:04:29.965,
                // suppressed 0.7 s into a window that had already run 11.7.
                _micFreeSince = DateTime.UtcNow;
                // The fact, not an inference: Windows says nobody holds the
                // microphone. This RELEASES the hold taken after a stop
                // instead of restarting a clock, so an extra edge is
                // harmless and hanging up then dialling again two seconds
                // later records.
                try { DimmyNative.dimmy_call_mic_confirmed_free(); } catch { }
            }
        });
    }

    /// Tell the core the machine is quiet.
    ///
    /// This is load-bearing, not housekeeping. The detector refuses to hand
    /// auto-record a call it never saw START — a call already under way when
    /// Dimmy arrived is somebody else's, not ours to grab — and it decides
    /// that from having seen the microphone free at least once. The old
    /// unconditional 4 Hz poll delivered that as a side effect, by watching
    /// sessions disappear. Gating the poll removed the side effect, and the
    /// first real call of a session was then suppressed as "already running":
    /// measured 2026-09-25, a Teams meeting produced `new session: ms-teams`
    /// and no nudge at all.
    ///
    /// The registry is better evidence than the poll ever was. It says the
    /// microphone is free; the poll only ever inferred it from an enumeration
    /// that came back empty.
    private static void SignalMicFree(string why)
    {
        try
        {
            DimmyNative.dimmy_call_signal(0, null);
            App.Log($"microphone free — {why}", "CallDetect");
        }
        catch (Exception ex)
        {
            App.Log($"idle signal failed: {ex.Message}", "CallDetect");
        }
    }

    private void StartSampling(string why)
    {
        if (_timer == null || _timer.IsRunning) return;
        _timer.Start();
        App.Log($"sampling at 4 Hz — {why}", "CallDetect");
    }

    private void StopSamplingIfIdle()
    {
        if (_timer == null || !_timer.IsRunning) return;
        if (_micFreeSince == null || DateTime.UtcNow - _micFreeSince.Value < IdleGrace) return;
        if (App.Instance?.AppViewModel.MeetingActive == true) return;
        _timer.Stop();
        _micFreeSince = null;
        App.Log("microphone free — sampling stopped", "CallDetect");
    }

    public void Stop()
    {
        _isEnabled = false;
        if (_micWatcher != null)
        {
            _micWatcher.Changed -= OnMicUsageChanged;
            _micWatcher.Dispose();
            _micWatcher = null;
        }
        if (_timer != null)
        {
            _timer.Stop();
            _timer.Tick -= OnTick;
            _timer = null;
        }
        _emittedSessions.Clear();
        _pendingSessions.Clear();
        _micFreeSince = null;
    }

    public void SetEnabled(bool enabled)
    {
        _isEnabled = enabled;
    }

    /// Called by the host right after the user accepted a "Record
    /// now" nudge and the meeting actually started. Picks the
    /// currently-emitted session-id (matching `_lastEmittedExe`)
    /// and stamps it as the meeting origin so the meeting-active
    /// tick branch can detect call termination by watching for that
    /// id's disappearance.
    ///
    /// Returns true iff an origin was bound. If false (no live
    /// emitted session matched), the meeting falls back to the
    /// amplitude-silence heuristic.
    public bool MarkMeetingOriginFromCurrentSession()
    {
        if (string.IsNullOrEmpty(_lastEmittedExe)) return false;
        var origin = _emittedSessions
            .FirstOrDefault(kv => kv.Value == _lastEmittedExe);
        if (string.IsNullOrEmpty(origin.Key)) return false;
        _meetingOriginSessionId = origin.Key;
        var originRow = _tickSessions.FirstOrDefault(x => x.sessionId == origin.Key);
        _meetingOriginEndpointId = originRow.endpointId;
        _originTracker.Reset();
        _meetingOriginPid = originRow.pid;
        _originEndpointAwayLogged = false;
        _meetingDrivenByCallDetect = true;
        App.Log($"meeting origin bound: exe={_lastEmittedExe} id=…{TailOf(origin.Key)}", "CallDetect");
        return true;
    }

    /// Adopt a call as the meeting origin WHILE a meeting is already
    /// active — the common flow where the user starts recording first and
    /// joins the call after (so the meeting-start binding ran before any
    /// call existed). Samples capture sessions; the first real call app
    /// (not Dimmy itself, not a system process) capturing the mic becomes
    /// the origin and the Rust stop-path is armed. Idempotent; only acts
    /// while `_meetingOriginSessionId` is still null.
    private void TryAdoptCallOriginDuringMeeting()
    {
        try
        {
            // Discovery only ever cares about sessions that are actually
            // capturing right now.
            var live = _tickSessions.Where(x => x.state == (int)AudioSessionState.Active).ToList();
            int ownPid = Environment.ProcessId;
            foreach (var (sessionId, pid, endpointId, _) in live)
            {
                if (pid == ownPid) continue; // Dimmy's own meeting mic capture
                var exe = ResolveProcessExeName(pid);
                if (string.IsNullOrEmpty(exe) || SystemExesToIgnore.Contains(exe)) continue;
                _emittedSessions[sessionId] = exe;
                _lastEmittedExe = exe;
                _meetingOriginSessionId = sessionId;
                _meetingOriginEndpointId = endpointId;
                _originTracker.Reset();
                _meetingOriginPid = pid;
                _originEndpointAwayLogged = false;
                _meetingDrivenByCallDetect = true;
                try { DimmyNative.dimmy_call_meeting_started_external(); } catch { }
                App.Log($"adopted call origin mid-meeting: exe={exe} pid={pid} id=…{TailOf(sessionId)}", "CallDetect");
                return;
            }
        }
        catch (Exception ex)
        {
            App.Log($"adopt-origin failed: {ex.Message}", "CallDetect");
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        Stop();
    }

    /// Amplitude floor for "audibly active" during a meeting. Live
    /// mic + loopback peaks below this are treated as silence by the
    /// stop-suggestion gate.
    private const float MeetingAmpFloor = 0.02f;

    // What a tick actually costs, measured rather than asserted. The 4 Hz
    // cadence is only defensible if the work is negligible, and "WASAPI
    // enumeration is cheap" was a claim nobody had checked. Reported in the
    // heartbeat that already runs, so this adds a timestamp read per tick
    // and no timer of its own.
    private long _tickCostTotalUs;
    private long _tickCostMaxUs;
    private int _tickCostCount;

    private volatile bool _sampleInFlight;

    /// The timer no longer does the work; it asks for it.
    ///
    /// Enumerating the capture endpoints and their sessions costs 4-6 ms
    /// typically and was measured at 50 ms, once at 228 ms, per sample. Four
    /// times a second on the UI thread, that is three dropped frames in a bad
    /// sample and a visibly stuttering overlay during the exact minutes a
    /// call is running. COM enumeration has no business on the thread that
    /// draws the pill.
    ///
    /// What crosses back is a plain list. Every decision still runs on the
    /// dispatcher, in order, against dispatcher-owned state - the promote
    /// counters, the emitted set, the meeting origin - so none of the
    /// reasoning changed, only where the expensive part happens.
    private void OnTick(DispatcherQueueTimer sender, object args)
    {
        if (!_isEnabled || _disposed) return;
        // A sample that overruns its 250 ms slot must not have another one
        // stacked behind it. Skipping is right: the next tick is 250 ms away
        // and the state it would read is the state this one is already
        // reading.
        if (_sampleInFlight) return;
        _sampleInFlight = true;
        var t0 = System.Diagnostics.Stopwatch.GetTimestamp();

        System.Threading.Tasks.Task.Run(() =>
        {
            List<(string sessionId, uint pid, string endpointId, int state)>? sampled = null;
            var endpointsChanged = false;
            try { sampled = SampleActiveCaptureSessions(out endpointsChanged); }
            catch (Exception ex) { App.Log($"sample failed: {ex.Message}", "CallDetect"); }

            var changed = endpointsChanged;
            var rows = sampled;
            _dispatcher.TryEnqueue(() =>
            {
                try
                {
                    if (rows != null && !_disposed) OnTickCore(rows, changed);
                }
                catch (Exception ex)
                {
                    App.Log($"call-detection tick failed: {ex.Message}", "CallDetect");
                }
                finally
                {
                    StopSamplingIfIdle();
                    var us = (System.Diagnostics.Stopwatch.GetTimestamp() - t0)
                        * 1_000_000 / System.Diagnostics.Stopwatch.Frequency;
                    _tickCostTotalUs += us;
                    if (us > _tickCostMaxUs) _tickCostMaxUs = us;
                    _tickCostCount++;
                    _sampleInFlight = false;
                }
            });
        });
    }

    private void OnTickCore(
        List<(string sessionId, uint pid, string endpointId, int state)> sampled,
        bool endpointsChanged)
    {
        if (!_isEnabled) return;
        var app = App.Instance;
        bool meetingActive = app?.AppViewModel.MeetingActive == true;
        bool dictationActive = app?.AppViewModel.IsRecording == true && !meetingActive;

        if (dictationActive)
        {
            try { DimmyNative.dimmy_call_signal(0, null); } catch { }
            return;
        }

        // Did the set of capture endpoints just change under us?
        //
        // Computed ONCE here, for the whole tick, because every branch
        // below draws a conclusion from which sessions it can see and
        // every one of them is wrong while the device is churning. The
        // first version of this guard sat inside the discovery branch
        // only, so a Bluetooth profile flip still reached the STOP path:
        // the origin session vanished with its endpoint, the meeting was
        // stopped as though the call had ended, and the reappearance two
        // seconds later started a new one. That is the "opens several
        // recordings, stops and starts at random" the user hit at 12:40.
        _tickSessions = sampled;
        if (endpointsChanged)
        {
            // This is the only place that knows a device just came or went.
            // Tell the tracker, because the reading that follows - the call's
            // session going idle on the device it is leaving - is
            // indistinguishable from a hangup without it.
            _originTracker.NoteDeviceChange();
            App.Log("endpoint set changed — skipping this sample", "CallDetect");
            return;
        }

        // Detect meeting active→inactive transition and reset
        // origin tracking. Belt-and-braces: meeting-stop can be
        // triggered by the stop-suggestion popup, the pill, the
        // meeting window close button, or the jump-list, and we
        // want exactly one well-defined edge to clear the state.
        if (_prevMeetingActive && !meetingActive)
        {
            _meetingOriginSessionId = null;
            _meetingOriginEndpointId = null;
            _originTracker.Reset();
            _meetingOriginPid = 0;
            _originMissingSince = null;
            _meetingDrivenByCallDetect = false;
        }
        // Meeting just started WITHOUT a bound origin — i.e. a manual start
        // from the meeting window / pill, not via a "Record now" nudge. If
        // a call is currently detected, adopt its session as the origin so
        // closing the call still fires the stop-suggestion. Without this,
        // manually-started meetings only had the weaker amplitude heuristic
        // and never suggested stop when the call (e.g. Teams) ended.
        else if (!_prevMeetingActive && meetingActive && _meetingOriginSessionId == null)
        {
            if (MarkMeetingOriginFromCurrentSession())
            {
                // Tell the Rust call-state machine this manually-started
                // meeting counts as "ours", so signal_session_ended will
                // actually suggest stop when the bound call ends (otherwise
                // recording_active_from_us stays false → NoChange → no popup).
                try { DimmyNative.dimmy_call_meeting_started_external(); } catch { }
            }
        }
        _prevMeetingActive = meetingActive;

        if (meetingActive)
        {
            try
            {
                if (_meetingOriginSessionId == null)
                {
                    // Meeting active but no call origin yet — typically the
                    // user started recording FIRST and joined the call after,
                    // so the meeting-start binding ran before any call existed.
                    // Keep watching: adopt the call session the moment it shows
                    // up so leaving the call still suggests stop.
                    TryAdoptCallOriginDuringMeeting();
                }
                if (_meetingDrivenByCallDetect && _meetingOriginSessionId != null)
                {
                    // Session-id-driven stop. Re-enumerate, look for
                    // the origin id; the moment it disappears, fire
                    // the authoritative stop signal once. No
                    // amplitude check — silence-during-call is NOT
                    // a stop signal in this branch (a user might be
                    // listening intently for minutes).
                    // Is the call still up? Asked of the PROCESS, not of one
                    // session id: changing the audio device inside Teams moves
                    // its capture to a different endpoint under a new session,
                    // and the call never paused. Any active capture from that
                    // process, anywhere, means the call is running.
                    // MEASUREMENT (2026-09-25): a session is Active,
                    // Inactive or Expired, and we have been collapsing the
                    // last two into "not Active". The docs say Inactive means
                    // the client stopped its stream - a hangup - while
                    // Expired means the session was invalidated, typically
                    // because the device went away. If that holds, the device
                    // case needs no correlation with anything: the state says
                    // it. Logged until we know.
                    var picture = string.Join(" ", _tickSessions
                        .Where(x => x.pid == _meetingOriginPid)
                        .Select(x => $"{StateName(x.state)}@{TailOf(x.endpointId)}"));
                    if (picture != _lastOriginPicture)
                    {
                        // MEASUREMENT: the sessions look the same whether the
                        // call moved device or hung up - both read Inactive,
                        // and only what happens next tells them apart, which
                        // is a clock. Windows answers the question directly
                        // and per app: does that app still hold the
                        // microphone? A call switching device is still
                        // capturing, on the other endpoint. A call that hung
                        // up is not. Printing the whole holder list rather
                        // than matching a name, because the registry names a
                        // package family for packaged apps and an exe path
                        // for the rest, and guessing that mapping is how this
                        // would go wrong quietly.
                        var holders = string.Join(", ", MicUsageWatcher.UsersOfMic());
                        App.Log($"origin sessions: [{picture}] alive={ProcessAlive(_meetingOriginPid)} "
                            + $"deviceChanging={_originTracker.DeviceChanging} endpoints={_lastEndpointIds.Count} "
                            + $"micHeldBy=[{holders}]",
                            "CallDetect");
                        _lastOriginPicture = picture;
                    }

                    var verdict = _originTracker.Observe(
                        _meetingOriginPid,
                        _tickSessions,
                        ProcessAlive(_meetingOriginPid),
                        MicUsageWatcher.SomeoneElseUsingMic());

                    if (verdict == CallOriginState.OnTheCall)
                    {
                        if (_originMissingSince != null)
                            App.Log("origin session back — it had moved, not ended", "CallDetect");
                        _originMissingSince = null;
                    }
                    else if (verdict == CallOriginState.Ended)
                    {
                        App.Log("the call let go of the microphone and nothing else explains it — call over",
                            "CallDetect");
                        FireSessionEnded();
                    }
                    else if (_originMissingSince == null)
                    {
                        // No session at all from that process: mid-move, or the
                        // endpoint churning. Start counting, conclude nothing.
                        _originMissingSince = DateTime.UtcNow;
                    }
                    else if (DateTime.UtcNow - _originMissingSince.Value >= OriginGoneConfirm)
                    {
                        // Backstop only. Every ordinary ending is answered
                        // above without a clock; this covers a process we
                        // cannot see and a session object that never
                        // reappears, so a recording can never run for ever
                        // on a signal that went missing.
                        App.Log("origin absent past the backstop — call over", "CallDetect");
                        FireSessionEnded();
                    }
                }
                else
                {
                    // Fallback amplitude path: meeting was started
                    // manually (no origin session-id), use mic+sys
                    // silence heuristic as before.
                    float micAmp = DimmyNative.dimmy_get_amplitude();
                    float sysAmp = DimmyNative.dimmy_get_loopback_amplitude();
                    bool micOk = micAmp > MeetingAmpFloor;
                    bool sysOk = sysAmp > MeetingAmpFloor;
                    DimmyNative.dimmy_call_signal(micOk ? 1 : 0, _lastEmittedExe);
                    DimmyNative.dimmy_call_signal_sys(sysOk ? 1 : 0, _lastEmittedExe);
                }
            }
            catch (Exception ex)
            {
                App.Log($"meeting-tick failed: {ex.Message}", "CallDetect");
            }
            return;
        }

        // Pre-meeting: poll all active capture endpoints, collect
        // (session-instance-id, pid) pairs for active sessions,
        // then apply one-shot logic.
        try
        {
            var live = _tickSessions.Where(x => x.state == (int)AudioSessionState.Active).ToList();
            var liveIds = new HashSet<string>(live.Select(s => s.sessionId), StringComparer.Ordinal);

            // 1. Cleanup: any emitted session that's no longer alive
            // = call ended. Drop from set so the next session by the
            // same exe re-emits.
            var disappeared = _emittedSessions.Where(kv => !liveIds.Contains(kv.Key))
                .Select(kv => kv).ToList();
            foreach (var kv in disappeared)
            {
                _emittedSessions.Remove(kv.Key);
                App.Log($"session ended: exe={kv.Value} id=…{TailOf(kv.Key)}", "CallDetect");
                if (_lastEmittedExe == kv.Value) _lastEmittedExe = null;
            }
            if (disappeared.Count > 0)
            {
                try { DimmyNative.dimmy_call_signal(0, null); } catch { }
            }
            // Pending list cleanup too — drop disappeared.
            var pendingGone = _pendingSessions.Keys.Where(k => !liveIds.Contains(k)).ToList();
            foreach (var k in pendingGone) _pendingSessions.Remove(k);

            // 2a. Already-emitted sessions: keep re-signalling
            // signal(1, exe) on every tick. The Rust state machine
            // is level-triggered — it uses `mic_active_since` to
            // gate the `min_active_secs` threshold AND it needs
            // continuous signal(1) calls to honour stop-suggestion
            // semantics during meeting mode. Edge-triggered
            // (one-and-done) would make detection_emitted fire only
            // through luck and leave the meeting branch starved.
            foreach (var (sessionId, _, _, _) in live)
            {
                if (_emittedSessions.TryGetValue(sessionId, out var emittedExe)
                    && !string.IsNullOrEmpty(emittedExe)
                    && !SystemExesToIgnore.Contains(emittedExe))
                {
                    try { DimmyNative.dimmy_call_signal(1, emittedExe); } catch { }
                    break; // single live session is the canonical case
                }
            }

            // 2b. Look for first non-system non-emitted candidate.
            // Emit ONE per tick (Rust state machine + nudge UI are
            // single-session — multiple parallel calls aren't a
            // supported scenario yet).
            foreach (var (sessionId, pid, endpointId, _) in live)
            {
                if (_emittedSessions.ContainsKey(sessionId)) continue;
                if (!_pendingSessions.TryGetValue(sessionId, out var ticksSeen))
                {
                    _pendingSessions[sessionId] = 1;
                    continue;
                }
                if (ticksSeen < PromoteAfterTicks)
                {
                    _pendingSessions[sessionId] = ticksSeen + 1;
                    continue;
                }
                // Confirmed — resolve exe, filter, emit.
                var exe = ResolveProcessExeName(pid);
                if (string.IsNullOrEmpty(exe)) continue;
                if (SystemExesToIgnore.Contains(exe))
                {
                    // Don't keep re-promoting on every tick. Mark as
                    // emitted with an empty-exe sentinel so we ignore
                    // it for the rest of its lifetime.
                    _emittedSessions[sessionId] = exe;
                    _pendingSessions.Remove(sessionId);
                    continue;
                }
                _emittedSessions[sessionId] = exe;
                _pendingSessions.Remove(sessionId);
                _lastEmittedExe = exe;
                try
                {
                    // rc 1 = the core emitted call_detected, 0 = it swallowed
                    // the signal. Discarding rc made a suppressed call look
                    // exactly like a call nobody saw, and the only way to tell
                    // them apart was to guess. When it is swallowed, ask the
                    // detector why and write it down.
                    var rc = DimmyNative.dimmy_call_signal(1, exe);
                    App.Log($"new session: exe={exe} pid={pid} id=…{TailOf(sessionId)} rc={rc}", "CallDetect");
                    if (rc == 0)
                    {
                        App.Log($"signal swallowed — detector says {DimmyNative.CallDetectorState() ?? "<no state>"}",
                            "CallDetect");
                    }
                }
                catch (Exception ex)
                {
                    App.Log($"signal(1, {exe}) failed: {ex.Message}", "CallDetect");
                }
                break; // one emit per tick
            }

            // Heartbeat every ~30 s (120 ticks @ 250 ms).
            if (++_logSuppressCounter >= 120)
            {
                _logSuppressCounter = 0;
                var avgUs = _tickCostCount > 0 ? _tickCostTotalUs / _tickCostCount : 0;
                App.Log($"heartbeat: live={live.Count} emitted={_emittedSessions.Count} " +
                        $"pending={_pendingSessions.Count} tick avg={avgUs}us max={_tickCostMaxUs}us " +
                        $"over {_tickCostCount} ticks", "CallDetect");
                _tickCostTotalUs = 0; _tickCostMaxUs = 0; _tickCostCount = 0;
            }
        }
        catch (Exception ex)
        {
            App.Log($"call-detection tick failed: {ex.Message}", "CallDetect");
        }
    }

    /// Enumerate every active capture endpoint and collect
    /// `(session-instance-id, pid)` for every session currently in
    /// AudioSessionState.Active. Multi-endpoint coverage is critical
    /// for BT-HFP rigs: when the user switches to a Bluetooth
    /// headset, Teams moves its session to that endpoint and the
    /// previous default goes quiet.
    /// The capture endpoints the last sample was taken from.
    ///
    /// Load-bearing, not diagnostics. We enumerate DEVICE_STATE_ACTIVE
    /// endpoints only, so an endpoint that leaves that state takes every
    /// session on it out of our view at once — and a Bluetooth headset
    /// leaves it every time it flips between A2DP and HFP, which with a
    /// phone paired to the same headset happens constantly. Teams never
    /// dropped anything; we simply stopped being able to see it. Measured
    /// 2026-09-24: 28 disappear/reappear cycles for the same PID and the
    /// same session id, from 0.26 s to 13 s apart, several of which
    /// started a recording of a call that had never stopped.
    private static HashSet<string> _lastEndpointIds = new(StringComparer.Ordinal);

    /// The one sample taken this tick. Every branch reads it instead of
    /// re-enumerating: three enumerations per tick could disagree with
    /// each other mid-profile-switch, which is precisely the state this
    /// whole guard exists to survive.
    private List<(string sessionId, uint pid, string endpointId, int state)> _tickSessions = new();

    /// The capture endpoint the origin session was found on.
    ///
    /// The stop path asks "is the origin session still there?" and a
    /// Bluetooth profile switch answers no — not because the call ended
    /// but because the endpoint left DEVICE_STATE_ACTIVE and took every
    /// session on it out of view. Remembering the endpoint turns that
    /// into a question we can actually answer: a session missing while
    /// its endpoint is ALSO missing is unexplained, not over.
    private string? _meetingOriginEndpointId;
    /// The capture devices that existed when this call was bound. An idle
    /// session only means "hung up" while this still matches: a device coming
    /// or going makes the same reading mean "moved".
    /// AudioSessionState: Inactive = 0, Active = 1, Expired = 2. Measured
    /// 2026-09-25 - a headset switch reads Inactive, not Expired, so the
    /// state alone does not tell a device move from a hangup.
    private static string StateName(int state) => state switch
    {
        0 => "Inactive",
        1 => "Active",
        2 => "Expired",
        _ => $"state{state}",
    };

    private readonly CallOriginTracker _originTracker = new();
    /// What the origin's sessions looked like last tick, so the log records
    /// state CHANGES and not four lines a second.
    private string _lastOriginPicture = "";

    /// Log the "endpoint away" verdict once per absence, not four times
    /// a second for as long as the headset takes to switch profile.
    private bool _originEndpointAwayLogged;

    /// When the origin session was first missing. Null while it is there.
    private DateTime? _originMissingSince;

    /// The process whose call we are recording. The call is up while THIS
    /// process is capturing, wherever it captures from — which is what
    /// makes an in-app device change a non-event instead of a hangup.
    private uint _meetingOriginPid;

    /// How long the origin session must stay missing before we call the
    /// call over.
    ///
    /// NOT a guess. Changing the audio device inside Teams moves its
    /// capture session between endpoints, and for that moment the session
    /// is simply gone — every endpoint stays alive, so nothing else marks
    /// it as a device event. Measured over the whole log on 2026-09-24:
    /// 23 absences that turned out to be a move, from 0.23 s to 31.12 s.
    /// Sixty seconds is double the longest.
    ///
    /// The asymmetry is the argument. Waiting too long costs a recording
    /// that runs a minute past the end of the call. Waiting too little
    /// costs what the user actually hit: the stop fires, the session comes
    /// back a quarter of a second later, detection is no longer suppressed
    /// by an active meeting, and a new recording starts — five times over.
    /// Backstop for an absence nothing else explains.
    ///
    /// Not the normal path any more: a process that exits, and a process
    /// that releases the microphone while keeping its session, are both
    /// answered immediately above. This only covers a session that
    /// vanishes and never comes back from a process we cannot inspect, so
    /// that a recording can never run for ever on a missing signal.
    /// Sixty seconds is double the longest absence ever observed that
    /// turned out to be a device move (31.12 s, measured 2026-09-24).
    private static readonly TimeSpan OriginGoneConfirm = TimeSpan.FromSeconds(60);

    /// `endpointsChanged` is true when the set of active capture endpoints
    /// differs from the previous sample. When it does, sessions that
    /// vanished or appeared in the same breath are the device churning,
    /// not calls starting and stopping, and the caller must not read them
    /// as transitions.
    /// Is that process still running? A closed Teams is an answer that
    /// needs no confirmation window at all.
    private static bool ProcessAlive(uint pid)
    {
        try
        {
            using var p = System.Diagnostics.Process.GetProcessById((int)pid);
            return !p.HasExited;
        }
        catch (ArgumentException) { return false; }   // no such process
        catch (InvalidOperationException) { return false; }
        catch (Exception) { return true; }            // unknown: do not stop on a guess
    }

    /// Tell the core the call is over, once.
    private void FireSessionEnded()
    {
        try
        {
            int rc = DimmyNative.dimmy_call_signal_session_ended();
            App.Log($"origin gone (id=…{TailOf(_meetingOriginSessionId)}) → signal_session_ended rc={rc}", "CallDetect");
        }
        catch (Exception ex)
        {
            App.Log($"signal_session_ended failed: {ex.Message}", "CallDetect");
        }
        _meetingOriginSessionId = null;
        _originMissingSince = null;
    }

    private static List<(string sessionId, uint pid, string endpointId, int state)> SampleActiveCaptureSessions(
        out bool endpointsChanged)
    {
        var endpointIds = new HashSet<string>(StringComparer.Ordinal);
        var result = SampleActiveCaptureSessionsInner(endpointIds);
        // The first sample has no baseline to differ from. Calling that a
        // change would throw away the poll that discovers what is already
        // running, which is the one the pre-existing rule depends on.
        endpointsChanged = _lastEndpointIds.Count > 0 && !endpointIds.SetEquals(_lastEndpointIds);
        _lastEndpointIds = endpointIds;
        return result;
    }

    private static List<(string sessionId, uint pid, string endpointId, int state)> SampleActiveCaptureSessionsInner(
        HashSet<string> endpointIds)
    {
        var result = new List<(string, uint, string, int)>();
        IMMDeviceEnumerator? enumerator = null;
        IMMDeviceCollection? devices = null;
        try
        {
            var enumType = Type.GetTypeFromCLSID(CLSID_MMDeviceEnumerator);
            if (enumType == null) return result;
            enumerator = (IMMDeviceEnumerator?)Activator.CreateInstance(enumType);
            if (enumerator == null) return result;

            if (enumerator.EnumAudioEndpoints(EDataFlow.eCapture, DEVICE_STATE_ACTIVE, out devices) != 0
                || devices == null) return result;
            if (devices.GetCount(out uint deviceCount) != 0) return result;

            for (uint d = 0; d < deviceCount; d++)
            {
                IMMDevice? device = null;
                IAudioSessionManager2? manager = null;
                IAudioSessionEnumerator? sessionEnum = null;
                try
                {
                    if (devices.Item(d, out device) != 0 || device == null) continue;
                    string currentEndpointId = "";
                    if (device.GetId(out string? endpointId) == 0 && !string.IsNullOrEmpty(endpointId))
                    {
                        currentEndpointId = endpointId!;
                        endpointIds.Add(currentEndpointId);
                    }
                    var iid = IID_IAudioSessionManager2;
                    if (device.Activate(ref iid, CLSCTX_ALL, IntPtr.Zero, out object pManager) != 0
                        || pManager == null) continue;
                    manager = (IAudioSessionManager2)pManager;
                    if (manager.GetSessionEnumerator(out sessionEnum) != 0 || sessionEnum == null) continue;
                    if (sessionEnum.GetCount(out int count) != 0) continue;

                    for (int i = 0; i < count; i++)
                    {
                        IAudioSessionControl? control = null;
                        IAudioSessionControl2? control2 = null;
                        try
                        {
                            if (sessionEnum.GetSession(i, out control) != 0 || control == null) continue;
                            if (control.GetState(out int stateRaw) != 0) continue;
                            // Non-active sessions are KEPT. A process that
                            // has released the microphone still owns its
                            // session object, in Inactive state — that is the
                            // difference between "the call ended" and "the
                            // session moved to another endpoint", and
                            // discarding it left only the stopwatch.
                            try { control2 = (IAudioSessionControl2)control; } catch { continue; }
                            if (control2.GetSessionInstanceIdentifier(out string? sessionId) != 0
                                || string.IsNullOrEmpty(sessionId)) continue;
                            if (control2.GetProcessId(out int pid) != 0 || pid <= 0) continue;
                            result.Add((sessionId!, (uint)pid, currentEndpointId, stateRaw));
                        }
                        finally
                        {
                            if (control2 != null) Marshal.ReleaseComObject(control2);
                            if (control != null) Marshal.ReleaseComObject(control);
                        }
                    }
                }
                finally
                {
                    if (sessionEnum != null) Marshal.ReleaseComObject(sessionEnum);
                    if (manager != null) Marshal.ReleaseComObject(manager);
                    if (device != null) Marshal.ReleaseComObject(device);
                }
            }
        }
        catch
        {
            // Transient COM failure under contention — silently skip
            // this tick; the next one will retry.
        }
        finally
        {
            if (devices != null) Marshal.ReleaseComObject(devices);
            if (enumerator != null) Marshal.ReleaseComObject(enumerator);
        }
        return result;
    }

    private static string TailOf(string s) => s.Length <= 8 ? s : s.Substring(s.Length - 8);

    /// Resolve a PID to a lowercase exe name (no `.exe` suffix).
    /// Returns empty string on process-exited / access-denied.
    private static string ResolveProcessExeName(uint pid)
    {
        if (pid == 0) return string.Empty;
        try
        {
            using var p = Process.GetProcessById((int)pid);
            return p.ProcessName.ToLowerInvariant();
        }
        catch
        {
            return string.Empty;
        }
    }

    // ── COM interop declarations ────────────────────────────────────────

    private static readonly Guid CLSID_MMDeviceEnumerator =
        new("BCDE0395-E52F-467C-8E3D-C4579291692E");
    private static readonly Guid IID_IAudioSessionManager2 =
        new("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F");
    private const uint CLSCTX_ALL = 0x17;
    private const int DEVICE_STATE_ACTIVE = 0x00000001;

    private enum EDataFlow { eRender = 0, eCapture = 1, eAll = 2 }
    private enum AudioSessionState { Inactive = 0, Active = 1, Expired = 2 }

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceEnumerator
    {
        [PreserveSig] int EnumAudioEndpoints(EDataFlow dataFlow, int dwStateMask, out IMMDeviceCollection? ppDevices);
        [PreserveSig] int GetDefaultAudioEndpoint(EDataFlow dataFlow, int role, out IMMDevice? ppEndpoint);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string pwstrId, out IMMDevice? ppDevice);
        [PreserveSig] int RegisterEndpointNotificationCallback(IntPtr pClient);
        [PreserveSig] int UnregisterEndpointNotificationCallback(IntPtr pClient);
    }

    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceCollection
    {
        [PreserveSig] int GetCount(out uint pcDevices);
        [PreserveSig] int Item(uint nDevice, out IMMDevice? ppDevice);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDevice
    {
        [PreserveSig] int Activate(ref Guid iid, uint dwClsCtx, IntPtr pActivationParams,
            [MarshalAs(UnmanagedType.IUnknown)] out object ppInterface);
        [PreserveSig] int OpenPropertyStore(uint stgmAccess, out IntPtr ppProperties);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string? ppstrId);
        [PreserveSig] int GetState(out int pdwState);
    }

    [ComImport, Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionManager2
    {
        [PreserveSig] int GetAudioSessionControl(IntPtr audioSessionGuid, int streamFlags, out IntPtr sessionControl);
        [PreserveSig] int GetSimpleAudioVolume(IntPtr audioSessionGuid, int streamFlags, out IntPtr audioVolume);
        [PreserveSig] int GetSessionEnumerator(out IAudioSessionEnumerator? sessionEnum);
        [PreserveSig] int RegisterSessionNotification(IntPtr sessionNotification);
        [PreserveSig] int UnregisterSessionNotification(IntPtr sessionNotification);
    }

    [ComImport, Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionEnumerator
    {
        [PreserveSig] int GetCount(out int sessionCount);
        [PreserveSig] int GetSession(int sessionIndex, out IAudioSessionControl? session);
    }

    [ComImport, Guid("F4B1A599-7266-4319-A8CA-E70ACB11E8CD"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionControl
    {
        [PreserveSig] int GetState(out int pRetVal);
        [PreserveSig] int GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetDisplayName([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetIconPath([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetIconPath([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetGroupingParam(out Guid pRetVal);
        [PreserveSig] int SetGroupingParam(ref Guid grouping, IntPtr eventContext);
        [PreserveSig] int RegisterAudioSessionNotification(IntPtr newNotifications);
        [PreserveSig] int UnregisterAudioSessionNotification(IntPtr newNotifications);
    }

    [ComImport, Guid("BFB7FF88-7239-4FC9-8FA2-07C950BE9C6D"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionControl2
    {
        [PreserveSig] int GetState(out int pRetVal);
        [PreserveSig] int GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetDisplayName([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetIconPath([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetIconPath([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetGroupingParam(out Guid pRetVal);
        [PreserveSig] int SetGroupingParam(ref Guid grouping, IntPtr eventContext);
        [PreserveSig] int RegisterAudioSessionNotification(IntPtr newNotifications);
        [PreserveSig] int UnregisterAudioSessionNotification(IntPtr newNotifications);
        [PreserveSig] int GetSessionIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int GetSessionInstanceIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int GetProcessId(out int pRetVal);
        [PreserveSig] int IsSystemSoundsSession();
        [PreserveSig] int SetDuckingPreference([MarshalAs(UnmanagedType.Bool)] bool optOut);
    }
}
