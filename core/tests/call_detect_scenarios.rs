//! Whole-lifecycle scenarios for call auto-detect + auto-record.
//!
//! The unit tests in `call_detector.rs` pin single rules. These drive the
//! state machine the way a host with auto-record ON does — a detection
//! starts a meeting, a stop suggestion stops it — across timelines taken
//! from real incidents, and count the recordings that come out. The one
//! number that matters to a user is that count: one call, one recording.
//!
//! What the host decides on its own (is the origin process still in the
//! call?) is modelled as the host's verdict — `call_ended()` is sent when
//! the call really ends. Whether the host reaches that verdict correctly
//! through a headset switch is tested on the host side (Mac:
//! `CallOriginJudgeTests`).

use dimmy_lib::call_detector::{CallDetectorState, CallSignalOutcome, NudgeResponse};
use std::collections::HashSet;

/// A host with auto-record on, plus the world it is watching.
struct Sim {
    d: CallDetectorState,
    now: i64,
    /// Start time of the recording in progress.
    meeting: Option<i64>,
    recordings: Vec<(i64, i64)>,
    /// Is Dimmy itself capturing a dictation right now?
    dictating: bool,
}

impl Sim {
    /// Dimmy has been running a while with nothing going on.
    fn warm() -> Self {
        let mut s = Self::cold();
        s.idle(30);
        s
    }

    /// Dimmy just launched — possibly in the middle of a call.
    fn cold() -> Self {
        let mut d = CallDetectorState::new();
        d.configure(true, 1, 1800, 300, HashSet::new());
        Sim {
            d,
            now: 1_000,
            meeting: None,
            recordings: Vec::new(),
            dictating: false,
        }
    }

    fn observe(&mut self, mic_active: bool, app: Option<&str>) {
        // During a meeting the hosts feed amplitude rather than "who holds
        // the mic"; with a tracked origin that path must decide nothing.
        let meeting_active = self.meeting.is_some();
        self.d.set_mic_free_is_evidence(!self.dictating);
        let out = self.d.signal(
            mic_active,
            app.map(str::to_string),
            meeting_active,
            self.now,
        );
        self.react(out);
    }

    fn react(&mut self, out: CallSignalOutcome) {
        match out {
            CallSignalOutcome::Detected { app, .. } => {
                assert!(self.meeting.is_none(), "detected while already recording");
                self.d
                    .record_response(app, NudgeResponse::RecordNow, self.now);
                self.d.set_tracked_origin(true);
                self.meeting = Some(self.now);
            }
            CallSignalOutcome::StopSuggested { app, .. } => {
                let start = self.meeting.expect("stop suggested with no meeting");
                self.d
                    .record_response(app, NudgeResponse::StopAndRecap, self.now);
                self.d.meeting_stopped_at(self.now);
                self.recordings.push((start, self.now));
                self.meeting = None;
            }
            _ => {}
        }
    }

    /// `app` holds the microphone for `secs`, observed every second (the
    /// hosts re-signal a live session far more often than that).
    fn in_call(&mut self, app: &str, secs: i64) {
        for i in 0..secs {
            // Inside a meeting the amplitude comes and goes: people pause.
            let speaking = self.meeting.is_none() || i % 7 < 4;
            self.observe(speaking, Some(app));
            self.now += 1;
        }
    }

    /// Nobody holds the microphone for `secs`.
    fn idle(&mut self, secs: i64) {
        for _ in 0..secs {
            self.observe(false, None);
            self.now += 1;
        }
    }

    /// The audio device set or default device changed (headset connected,
    /// dropped, switched A2DP↔HFP).
    fn device_change(&mut self) {
        self.d.device_changed(self.now);
    }

    /// The host has concluded the call it was recording is over.
    fn call_ended(&mut self) {
        let active = self.meeting.is_some();
        let out = self.d.signal_call_session_ended(active, self.now);
        self.react(out);
    }

    /// The user pressed Stop.
    fn user_stop(&mut self) {
        let start = self.meeting.take().expect("user stop with no meeting");
        self.d.meeting_stopped_at(self.now);
        self.recordings.push((start, self.now));
    }

    fn recording_now(&self) -> bool {
        self.meeting.is_some()
    }

    /// Recordings made so far, the one in progress included.
    fn total(&self) -> usize {
        self.recordings.len() + usize::from(self.meeting.is_some())
    }
}

/// The ordinary day: a call starts, records by itself, ends, stops by
/// itself. Exactly one recording, spanning the call.
#[test]
fn one_call_is_one_recording() {
    let mut s = Sim::warm();
    let start = s.now;
    s.in_call("teams", 1_800);
    assert!(s.recording_now(), "the call must be recording by now");
    s.call_ended();
    s.idle(120);

    assert_eq!(s.recordings.len(), 1, "{:?}", s.recordings);
    let (a, b) = s.recordings[0];
    assert!(a - start <= 2, "recording started {}s late", a - start);
    assert_eq!(b, start + 1_800);
}

/// The macOS report of 2026-09-24: after every auto-stopped call the app
/// took the microphone back for a moment, and that became a second
/// meeting of a few seconds. Tails of every length seen in the logs.
#[test]
fn the_app_taking_the_mic_back_after_hangup_is_not_a_second_meeting() {
    for (delay, tail) in [(0, 1), (1, 3), (2, 4), (4, 8), (9, 2)] {
        let mut s = Sim::warm();
        s.in_call("teams", 600);
        s.call_ended();
        s.idle(delay);
        s.in_call("teams", tail);
        s.idle(120);
        assert_eq!(
            s.total(),
            1,
            "tail {tail}s after {delay}s made a ghost: {:?}",
            s.recordings
        );
        assert!(!s.recording_now());
    }
}

/// The requirement the tail hold must never break: hang up, place the
/// next call, and it records by itself.
#[test]
fn the_next_call_records_by_itself() {
    let mut s = Sim::warm();
    s.in_call("teams", 600);
    s.call_ended();
    s.idle(40);
    s.in_call("teams", 600);
    s.call_ended();
    s.idle(60);
    assert_eq!(s.recordings.len(), 2, "{:?}", s.recordings);
}

/// A call in a different app straight after is a different call.
#[test]
fn a_call_in_another_app_records_straight_away() {
    let mut s = Sim::warm();
    s.in_call("teams", 600);
    s.call_ended();
    s.idle(3);
    s.in_call("zoom", 300);
    assert!(s.recording_now(), "zoom right after teams must record");
}

/// Quiet stretches inside a call — everyone listening, the user muted —
/// are not the call ending while the host is watching the call's process.
#[test]
fn silence_inside_a_call_never_stops_the_recording() {
    let mut s = Sim::warm();
    s.in_call("teams", 30);
    assert!(s.recording_now());
    // Five silent minutes in the middle of the call.
    for _ in 0..300 {
        s.observe(false, None);
        s.now += 1;
    }
    assert!(s.recording_now(), "silence stopped a live call");
    s.in_call("teams", 60);
    s.call_ended();
    assert_eq!(s.recordings.len(), 1);
}

/// Stop pressed by hand mid-call: that call must not come back, whatever
/// the app does with the microphone for the rest of it.
#[test]
fn a_call_stopped_by_hand_stays_stopped_through_flickers() {
    let mut s = Sim::warm();
    s.in_call("teams", 120);
    s.user_stop();
    // The rest of the call, with the capture session flickering as Teams
    // does: half a second on 2026-09-24, four seconds the day before.
    for gap in [1, 4, 1, 2, 4, 1] {
        s.in_call("teams", 90);
        s.idle(gap);
    }
    s.in_call("teams", 300);
    assert_eq!(s.total(), 1, "{:?}", s.recordings);
}

/// Bluetooth headsets drop out of the device list while they renegotiate
/// A2DP↔HFP, and every capture on them vanishes with them — 28 cycles in
/// one Windows log, up to 13 s apart; device moves up to 31 s. Longer than
/// the ten seconds that otherwise count as "the mic was really free", so
/// the gap has to be discounted because the devices were changing.
#[test]
fn a_headset_switch_does_not_resurrect_a_call_stopped_by_hand() {
    for gap in [6, 13, 20, 31] {
        let mut s = Sim::warm();
        s.in_call("teams", 120);
        s.user_stop();
        s.in_call("teams", 60);
        s.device_change();
        s.idle(gap);
        s.device_change();
        s.in_call("teams", 600);
        assert_eq!(
            s.total(),
            1,
            "{gap}s headset switch restarted a stopped call: {:?}",
            s.recordings
        );
        assert!(!s.recording_now());
    }
}

/// Same thing after an automatic stop that was wrong only in timing: the
/// app came back after a headset switch longer than the tail window.
#[test]
fn a_headset_switch_after_hangup_is_not_a_new_call() {
    let mut s = Sim::warm();
    s.in_call("teams", 600);
    s.call_ended();
    s.device_change();
    s.idle(15);
    s.device_change();
    s.in_call("teams", 5);
    s.idle(120);
    assert_eq!(s.total(), 1, "{:?}", s.recordings);
}

/// Dimmy started in the middle of a call (login, update, crash restart):
/// a start we did not witness is not a start, and a headset switch
/// during that call does not make it one.
#[test]
fn launched_mid_call_records_nothing_even_across_a_headset_switch() {
    let mut s = Sim::cold();
    s.in_call("teams", 120);
    s.device_change();
    s.idle(13);
    s.device_change();
    s.in_call("teams", 600);
    assert_eq!(s.total(), 0, "{:?} live={:?}", s.recordings, s.meeting);
}

/// Taking the headset off after the call is a device change too. It must
/// not cost the next call once the devices have settled.
#[test]
fn unplugging_the_headset_after_a_call_does_not_block_the_next_one() {
    let mut s = Sim::warm();
    s.in_call("teams", 600);
    s.call_ended();
    s.idle(2);
    s.device_change();
    s.idle(88);
    s.in_call("teams", 300);
    assert!(s.recording_now(), "the next call must still record");
    assert_eq!(s.recordings.len(), 1);
}

/// A device that changed long before the call is irrelevant to it.
#[test]
fn an_old_device_change_does_not_block_the_next_call() {
    let mut s = Sim::warm();
    s.device_change();
    s.idle(300);
    s.in_call("teams", 600);
    s.call_ended();
    s.idle(40);
    s.in_call("teams", 60);
    assert_eq!(s.recordings.len(), 1);
    assert!(s.recording_now(), "second call must record");
}

/// A dictation during a call the user stopped: Dimmy's own capture makes
/// the hosts report "mic free", which says nothing about the call.
#[test]
fn a_dictation_does_not_release_a_stopped_call() {
    let mut s = Sim::warm();
    s.in_call("teams", 120);
    s.user_stop();
    s.dictating = true;
    s.idle(20);
    s.dictating = false;
    s.in_call("teams", 300);
    assert_eq!(s.total(), 1, "{:?}", s.recordings);
}

/// Hang up by hand, then really hang up, then the next call: the hold is
/// lifted by the call ending, not by a timer, and the next call records.
#[test]
fn after_a_hand_stop_the_next_real_call_still_records() {
    let mut s = Sim::warm();
    s.in_call("teams", 120);
    s.user_stop();
    s.in_call("teams", 300);
    s.idle(60);
    s.in_call("teams", 120);
    assert!(s.recording_now(), "the next call must record");
    assert_eq!(s.recordings.len(), 1);
}

/// Tiny deterministic PRNG so the fuzz needs no extra dependency and every
/// failure names the seed that reproduces it.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
}

/// Hundreds of messy calls: random length, random flickers, random
/// headset switches, a tail after hangup, sometimes a hand stop. Whatever
/// happens inside one call, it produces at most one recording, and the
/// next call placed by a human still records.
#[test]
fn fuzz_one_call_never_becomes_two_recordings() {
    for seed in 1..=500u64 {
        let mut r = Lcg(seed);
        let mut s = Sim::warm();
        let mut hand_stop_at = if r.range(0, 3) == 0 {
            Some(r.range(30, 300))
        } else {
            None
        };
        let mut elapsed = 0;
        let length = r.range(120, 3_600);
        while elapsed < length {
            let chunk = r.range(20, 400).min(length - elapsed);
            s.in_call("teams", chunk);
            elapsed += chunk;
            if let Some(at) = hand_stop_at {
                if elapsed >= at && s.recording_now() {
                    s.user_stop();
                    hand_stop_at = Some(i64::MAX);
                }
            }
            match r.range(0, 5) {
                // Capture session flicker, no device involved.
                0 | 1 => s.idle(r.range(0, 4)),
                // Headset switch.
                2 => {
                    s.device_change();
                    s.idle(r.range(1, 35));
                    s.device_change();
                }
                _ => {}
            }
        }
        if s.recording_now() {
            s.call_ended();
        }
        // The app lets go untidily.
        s.idle(r.range(0, 3));
        s.in_call("teams", r.range(1, 6));
        // Long enough for a headset switch at the very end to have settled.
        s.idle(r.range(60, 120));

        assert!(
            s.total() <= 1,
            "seed {seed}: one call became {} recordings: {:?}",
            s.total(),
            s.recordings
        );
        if hand_stop_at.is_none() {
            assert_eq!(
                s.recordings.len(),
                1,
                "seed {seed}: the call never recorded"
            );
        }
        assert!(
            !s.recording_now(),
            "seed {seed}: still recording after the call"
        );

        // And the next call a human places still records by itself.
        s.in_call("teams", 60);
        assert!(
            s.recording_now(),
            "seed {seed}: the next call did not record"
        );
    }
}

/// The price of the rule above, pinned so it stays a decision and not an
/// accident: a headset switch at the moment a call ends, then the next
/// call in the same app within a minute, is not auto-recorded. The user can
/// start it by hand. The alternative is recording a call they stopped.
#[test]
fn the_next_call_within_a_minute_of_a_headset_switch_waits_for_a_hand() {
    let mut s = Sim::warm();
    s.in_call("teams", 600);
    s.idle(9);
    s.device_change();
    s.call_ended();
    s.idle(45);
    s.in_call("teams", 60);
    assert_eq!(s.total(), 1, "{:?}", s.recordings);
    // A different app is never held back by it.
    s.idle(20);
    s.in_call("zoom", 30);
    assert!(s.recording_now(), "zoom must record");
}
