//! Auto-detect a meeting via mic-in-use signal, ask the user if they
//! want to record it.
//!
//! Audio is the primary signal — "the microphone is being captured" is
//! the trigger. Process/app inference is best-effort reinforcement
//! that enriches the popup card and powers the per-app cooldown +
//! exclusion list. We never *require* an app match: a browser call
//! (Meet, Whereby) fires the popup just as well as a Teams call —
//! the card just says "Microphone in use — record a meeting?" instead
//! of "Detected meeting in Microsoft Teams".
//!
//! The state machine is pure: no threads, no IO. The host
//! (C# / Swift) polls audio every ~1 s and pushes the observation via
//! `dimmy_call_signal(mic_active, app_id_opt)`. The FFI bridge holds
//! the singleton state, applies debounce / cooldown / exclusion /
//! meeting-active suppression, and emits `call_detected` /
//! `call_ended` via the existing event channel exactly once per
//! transition (no polling on the host side — CLAUDE.md event rule).

use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Per-app cooldown key used when no app could be inferred.
pub const GLOBAL_COOLDOWN_KEY: &str = "";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeResponse {
    /// Start-nudge: user accepted → meeting recording begins.
    RecordNow,
    /// Start-nudge: postpone for the per-app cooldown.
    NotNow,
    /// Start-nudge: never propose for this app.
    Never,
    /// Start-nudge: auto-dismissed (short cooldown).
    Timeout,
    /// Stop-nudge: user accepted → stop the recording + run recap.
    StopAndRecap,
    /// Stop-nudge: user wants to keep recording (e.g. call is paused,
    /// not finished). Apply a short cooldown before re-asking.
    KeepRecording,
    /// Stop-nudge: auto-dismissed without action.
    StopTimeout,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SuppressionReason {
    Disabled,
    Excluded(String),
    Cooldown(String),
    MeetingActive,
    Debouncing,
    /// User already accepted "record now" for this call and the host is
    /// starting the meeting (which can take seconds while a consent modal
    /// is up). Suppress re-detection in that gap. Cleared by meeting_stopped().
    RecordingAccepted,
    /// This call was ALREADY running the first time we looked.
    ///
    /// Dimmy can be launched, or restarted, or started at login, in the
    /// middle of a call. The detector wakes with no memory, sees a live
    /// session and has no way to tell it apart from one that just began
    /// — so on 2026-09-24 11:33 a restart mid-call recorded a meeting the
    /// user was already sitting in. A start we did not witness is not
    /// evidence of a start, so we adopt the session and stay quiet.
    /// Cleared when the session finally goes away.
    AlreadyRunning(String),
    /// The call we were recording released the microphone, we stopped,
    /// and the same app took it straight back.
    ///
    /// Apps do not let go cleanly: seen on macOS on 2026-09-24, where
    /// every auto-stopped meeting left a four-second one behind it in the
    /// list. From the microphone signal alone that reacquisition is
    /// indistinguishable from the next call arriving, so the two are told
    /// apart the only way they can be — by whether the microphone was
    /// ever genuinely free in between. A human placing another call takes
    /// tens of seconds; a tail takes one.
    TailOfLastCall(String),
    /// The user pressed Stop by hand while this call was still running.
    /// Nothing is emitted — no recording, and no nudge either: having been
    /// told "not this call", asking again every time the host re-signals the
    /// same live session is the same loop wearing a popup. Lifted when the
    /// call actually ends.
    StoppedByUser(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallSignalOutcome {
    NoChange,
    Detected {
        app: Option<String>,
        since_seconds: i64,
    },
    Ended {
        app: Option<String>,
    },
    /// Mic has been silent for `mic_inactive_for_stop_secs` while a
    /// meeting started by us is still recording. UI should ask the
    /// user if they want to stop and run the recap, or keep recording.
    /// Emitted exactly once per meeting (re-armed only by an explicit
    /// `meeting_stopped()` call).
    StopSuggested {
        app: Option<String>,
        inactive_for_secs: i64,
    },
    Suppressed(SuppressionReason),
}

pub struct CallDetectorState {
    enabled: bool,
    min_active_secs: u32,
    cooldown_secs: u32,
    timeout_cooldown_secs: u32,
    /// How long the mic must stay inactive (after we started recording
    /// from a detection) before we propose stopping. Avoids false
    /// positives on short mutes (user took a sip of water, brief
    /// silence between speakers). Default 5 s — paired AND with sys
    /// silence so the false-positive surface is "both sides silent
    /// for 5 s", not "user silent for 5 s".
    mic_inactive_for_stop_secs: u32,
    /// How long the system-audio loopback must stay inactive before
    /// the stop-suggestion fires. AND-combined with mic — only fires
    /// when BOTH have been silent for their respective thresholds.
    /// Default 5 s.
    sys_inactive_for_stop_secs: u32,
    /// Cooldown after KeepRecording: don't re-ask for this long.
    /// Default 300 s (5 min).
    stop_keep_cooldown_secs: u32,
    excluded: HashSet<String>,

    last_mic_active: bool,
    mic_active_since: Option<i64>,
    detection_emitted: bool,
    current_app: Option<String>,
    cooldown_until: HashMap<String, i64>,

    /// The app whose call we are recording, captured when recording starts
    /// so `meeting_stopped` knows whose call it was. `None` when no app
    /// could be inferred — which is why the hold falls back to a global key.
    recorded_app: Option<String>,
    /// The key held back from auto-starting again, because the user pressed
    /// Stop by hand while that call was still running. Keyed like
    /// `cooldown_until` (GLOBAL_COOLDOWN_KEY when no app was inferred), so
    /// an app-less call is held too — that was the hole that let the loop
    /// survive wherever a call infers no app, which on macOS is often.
    barred: Option<String>,
    /// The key held back because we stopped it ourselves a moment ago and
    /// the app may not have finished with the microphone. Same release as
    ///  — confirmed idle — but a separate field so the two reasons
    /// stay distinguishable in a log.
    tail_of: Option<String>,
    /// Sessions that were already live the first time we saw them.
    ///
    /// Held back from auto-record for as long as they last, because we
    /// never witnessed them start. A key leaves this set when the session
    /// actually ends, at which point the next one IS a start we watched.
    preexisting: HashSet<String>,
    /// Have we ever seen the microphone idle? Until we have, every active
    /// signal describes something that was already going when we arrived.
    /// One flag rather than per-key bookkeeping: the moment we observe
    /// idle once, everything after it is a transition we witnessed.
    observed_idle: bool,
    /// When the mic was last seen free, outside a meeting. Stamped on the
    /// inactive edge and judged on the NEXT active edge: the hosts signal
    /// "free" once, as an edge, not continuously, so waiting for further
    /// inactive ticks to accumulate would wait for ever.
    mic_free_since: Option<i64>,
    /// How long the mic must have been free for the previous call to count
    /// as over. Measured 2026-09-23 on a real Teams call: its capture
    /// session vanished and came back under the same id four seconds later,
    /// mid-call. Ten seconds gives that room.
    release_confirm_secs: u32,
    /// Whether a `mic_active = false` signal actually means "nobody holds
    /// the microphone". The hosts also send that zero while Dimmy itself is
    /// capturing, so the pill cannot self-trigger — and reading a ten-second
    /// dictation as "the call ended" would lift a hold on a call that was
    /// still running.
    mic_free_is_evidence: bool,
    /// Set by the StopAndRecap response, consumed by `meeting_stopped`: it
    /// marks the stop that follows as ours, not the user's. Armed only while
    /// we are actually recording, so a nudge answered after the meeting has
    /// already ended cannot disarm the NEXT hand-pressed Stop.
    stop_is_automatic: bool,

    /// True between RecordNow accept and meeting_stopped() call. Marks
    /// "this meeting was started by the detector" — only meetings we
    /// started get the auto-stop suggestion.
    recording_active_from_us: bool,
    /// Wall-clock secs since the mic went inactive while
    /// recording_active_from_us is true. None outside that window.
    mic_inactive_since: Option<i64>,
    /// Wall-clock secs since system-audio loopback went inactive
    /// while recording_active_from_us is true. None outside that
    /// window. Hosts that don't poll the render side (Mac for now)
    /// can leave `sys_signaling_enabled` false and the stop path
    /// degrades cleanly to mic-only.
    sys_inactive_since: Option<i64>,
    /// Set to true the first time `signal_sys` is called. Until then
    /// the AND-with-sys path is silently bypassed (mic-only fallback)
    /// so Mac hosts without sys-audio polling still get stop-
    /// suggestions when the user's own mic goes quiet.
    sys_signaling_enabled: bool,
    /// Idempotency guard: a single meeting gets at most one
    /// stop-suggestion emission. Reset by meeting_stopped() AND by
    /// KeepRecording (which also pushes out the cooldown).
    stop_suggestion_emitted: bool,
    /// Suppression deadline for stop suggestions after KeepRecording.
    /// Set as `now + stop_keep_cooldown_secs` on KeepRecording.
    stop_suggestion_until: Option<i64>,
    /// True while the host is deterministically watching a meeting-origin
    /// process (a detected call app). When set, the SILENCE heuristic
    /// (`check_stop_suggestion`, fed by mic/sys amplitude) is suppressed:
    /// the authoritative stop signal is `signal_call_session_ended`, fired
    /// when that process actually goes away. Without this, a real call with
    /// quiet stretches (everyone listening / muted) trips the both-sides-
    /// silent threshold every cooldown and re-nags "the call ended" — the
    /// 15-popups-in-one-meeting bug. Hosts with no trackable origin (a
    /// meeting without a detected call app) leave this false and the silence
    /// backstop stays active as the only stop signal.
    has_tracked_origin: bool,
}

impl CallDetectorState {
    pub fn new() -> Self {
        Self {
            enabled: true,
            // Was 5 s when the C# side polled at 1 Hz and a single
            // tick of "mic_active = true" was ambiguous between a
            // real call and a momentary system-sound chirp. The
            // C# side now uses `IAudioSessionNotification` —
            // event-driven, the OS callback IS the proof of a real
            // session, so we don't need a timeout-based debounce.
            // Keep 1 s as a cheap sanity guard.
            min_active_secs: 1,
            cooldown_secs: 1800,
            timeout_cooldown_secs: 300,
            mic_inactive_for_stop_secs: 5,
            sys_inactive_for_stop_secs: 5,
            stop_keep_cooldown_secs: 300,
            excluded: HashSet::new(),
            last_mic_active: false,
            mic_active_since: None,
            detection_emitted: false,
            current_app: None,
            cooldown_until: HashMap::new(),
            recorded_app: None,
            barred: None,
            tail_of: None,
            preexisting: HashSet::new(),
            observed_idle: false,
            mic_free_since: None,
            release_confirm_secs: 10,
            mic_free_is_evidence: true,
            stop_is_automatic: false,
            recording_active_from_us: false,
            mic_inactive_since: None,
            sys_inactive_since: None,
            sys_signaling_enabled: false,
            stop_suggestion_emitted: false,
            stop_suggestion_until: None,
            has_tracked_origin: false,
        }
    }

    /// Apply user-configurable knobs in one go. Called from the FFI
    /// bridge each time the config round-trips.
    pub fn configure(
        &mut self,
        enabled: bool,
        min_active_secs: u32,
        cooldown_secs: u32,
        timeout_cooldown_secs: u32,
        excluded: HashSet<String>,
    ) {
        assert!(min_active_secs > 0, "min_active_secs must be > 0");
        assert!(cooldown_secs > 0, "cooldown_secs must be > 0");
        assert!(
            timeout_cooldown_secs > 0,
            "timeout_cooldown_secs must be > 0"
        );
        self.enabled = enabled;
        self.min_active_secs = min_active_secs;
        self.cooldown_secs = cooldown_secs;
        self.timeout_cooldown_secs = timeout_cooldown_secs;
        self.excluded = excluded;
    }

    /// Push one observation. `mic_active` = true iff some process is
    /// currently capturing the default microphone (best-effort:
    /// IAudioSessionManager2 on Win, kAudioDevicePropertyDeviceIs
    /// RunningSomewhere on Mac). `app` = optional lowercase canonical
    /// id ("teams" / "zoom" / "slack" / "discord" / "webex") iff a
    /// known VoIP process is running. `is_meeting_active` = result of
    /// `MEETING.lock()` check by the FFI bridge so the state machine
    /// stays IO-free.
    pub fn signal(
        &mut self,
        mic_active: bool,
        app: Option<String>,
        is_meeting_active: bool,
        now: i64,
    ) -> CallSignalOutcome {
        if !self.enabled {
            self.last_mic_active = mic_active;
            return CallSignalOutcome::Suppressed(SuppressionReason::Disabled);
        }

        if !mic_active {
            return self.handle_inactive(is_meeting_active, now);
        }

        // Mic active again — clear any pending stop-suggestion timer so
        // a brief mute followed by speech doesn't trip the threshold.
        self.mic_inactive_since = None;
        self.handle_active(app, is_meeting_active, now)
    }

    /// Push one system-audio observation. `sys_active` = true iff the
    /// whitelisted app's render-side audio session is currently
    /// emitting sound (Win: WASAPI IAudioMeterInformation on the
    /// session's render endpoint). Only the stop-suggestion path uses
    /// this — `Detected` / `Ended` are mic-only signals.
    ///
    /// Calling this once flips `sys_signaling_enabled = true` for the
    /// life of the state, so a host that calls it intermittently still
    /// gets the AND-with-sys path (we don't want a single missed tick
    /// to fall back to mic-only mid-meeting). Hosts that never call it
    /// (Mac) keep the flag false and the stop-suggestion path uses
    /// mic-only — matches today's behaviour.
    pub fn signal_sys(
        &mut self,
        sys_active: bool,
        is_meeting_active: bool,
        now: i64,
    ) -> CallSignalOutcome {
        self.sys_signaling_enabled = true;
        if sys_active {
            self.sys_inactive_since = None;
            return CallSignalOutcome::NoChange;
        }
        if self.sys_inactive_since.is_none() {
            self.sys_inactive_since = Some(now);
        }
        // Re-check the stop-suggestion gate without touching the
        // mic-side state — sys is an auxiliary input, not a mic
        // transition. The AND-with-mic logic still lives in
        // `check_stop_suggestion` so both code paths share it.
        self.check_stop_suggestion(is_meeting_active, now)
    }

    /// Authoritative stop signal — the host has positive evidence
    /// that the call ended (WASAPI capture session belonging to the
    /// originating exe disappeared). Bypasses the amplitude-based
    /// silence thresholds entirely and fires `StopSuggested`
    /// immediately, gated only by the same one-shot + cooldown
    /// guards `check_stop_suggestion` enforces. Use this for the
    /// session-id-driven stop path; for the silence-heuristic path
    /// keep going through `signal()` / `signal_sys()`.
    pub fn signal_call_session_ended(
        &mut self,
        is_meeting_active: bool,
        now: i64,
    ) -> CallSignalOutcome {
        if !self.recording_active_from_us || !is_meeting_active || self.stop_suggestion_emitted {
            return CallSignalOutcome::NoChange;
        }
        if let Some(until) = self.stop_suggestion_until {
            if now < until {
                return CallSignalOutcome::NoChange;
            }
        }
        self.stop_suggestion_emitted = true;
        self.detection_emitted = false;
        CallSignalOutcome::StopSuggested {
            app: self.current_app.clone(),
            inactive_for_secs: 0,
        }
    }

    /// Stop-suggestion gate, reused by both `signal()` (mic side) and
    /// `signal_sys()` (sys side). Returns `StopSuggested` exactly once
    /// per meeting iff: we started this meeting, the meeting is still
    /// active, no KeepRecording cooldown, BOTH mic and sys (if sys
    /// signaling is enabled) have been silent past their thresholds.
    fn check_stop_suggestion(&mut self, is_meeting_active: bool, now: i64) -> CallSignalOutcome {
        if !self.recording_active_from_us || !is_meeting_active || self.stop_suggestion_emitted {
            return CallSignalOutcome::NoChange;
        }
        // When a meeting-origin process is being watched deterministically,
        // the silence heuristic is the wrong authority: a quiet stretch in a
        // live call isn't the call ending. Defer entirely to
        // `signal_call_session_ended` (fires when the process goes away).
        if self.has_tracked_origin {
            return CallSignalOutcome::NoChange;
        }
        if let Some(until) = self.stop_suggestion_until {
            if now < until {
                return CallSignalOutcome::NoChange;
            }
        }
        let mic_inactive_for = match self.mic_inactive_since {
            Some(t) => now - t,
            None => return CallSignalOutcome::NoChange,
        };
        if mic_inactive_for < self.mic_inactive_for_stop_secs as i64 {
            return CallSignalOutcome::NoChange;
        }
        let sys_inactive_for = match self.sys_inactive_since {
            Some(t) => now - t,
            None => {
                // No sys observation yet. If the host has been
                // signalling sys at all, we wait for the next tick; if
                // it hasn't (Mac), the mic-only fallback in
                // handle_inactive already covers it and we never come
                // here from signal_sys.
                if self.sys_signaling_enabled {
                    return CallSignalOutcome::NoChange;
                }
                0
            }
        };
        if self.sys_signaling_enabled && sys_inactive_for < self.sys_inactive_for_stop_secs as i64 {
            return CallSignalOutcome::NoChange;
        }
        self.stop_suggestion_emitted = true;
        self.detection_emitted = false;
        let inactive_for_secs = if self.sys_signaling_enabled {
            mic_inactive_for.min(sys_inactive_for)
        } else {
            mic_inactive_for
        };
        CallSignalOutcome::StopSuggested {
            app: self.current_app.clone(),
            inactive_for_secs,
        }
    }

    fn handle_inactive(&mut self, is_meeting_active: bool, now: i64) -> CallSignalOutcome {
        let was_active = self.last_mic_active;
        let ended_app = self.current_app.clone();
        self.last_mic_active = false;
        self.mic_active_since = None;

        // Stamp when the mic became free, to be judged on the next active
        // edge. Only outside a meeting, and only when the zero means what it
        // says: the hosts also send it while Dimmy itself is capturing, and
        // a dictation is not evidence that somebody else's call ended.
        // The first honest idle we see ends the bootstrap: from here on, a
        // session going active is a transition we watched, not something
        // that was already under way before we existed.
        // Seeing the machine quiet once is enough to know we are no longer
        // in the dark about what was already running. It is NOT enough to
        // declare a call over — that needs a confirmed gap, judged on the
        // next active edge.
        if self.mic_free_is_evidence {
            self.observed_idle = true;
        }
        if !is_meeting_active && self.mic_free_is_evidence && self.mic_free_since.is_none() {
            self.mic_free_since = Some(now);
        }

        // Stop-suggestion path: only if WE started this meeting and the
        // user hasn't already deferred via KeepRecording. Mic-silence
        // ≥ threshold while the meeting is still recording → propose
        // to stop and run the recap. Emit exactly once per meeting;
        // re-armed only by meeting_stopped() (new meeting).
        let stop_suppressed = match self.stop_suggestion_until {
            Some(until) => now < until,
            None => false,
        };
        if self.recording_active_from_us && is_meeting_active && !self.stop_suggestion_emitted {
            // Stamp the silence timestamp the moment mic goes quiet —
            // independent of the KeepRecording cooldown. Without this
            // a user who chose "Keep recording" early in a long silent
            // stretch would never get re-prompted: mic_inactive_since
            // stayed None until the cooldown expired, by which point
            // the timestamp would restart at zero and the threshold
            // could never elapse if the mic stayed quiet.
            if self.mic_inactive_since.is_none() {
                self.mic_inactive_since = Some(now);
            }
            if !stop_suppressed {
                if let outcome @ CallSignalOutcome::StopSuggested { .. } =
                    self.check_stop_suggestion(is_meeting_active, now)
                {
                    return match outcome {
                        CallSignalOutcome::StopSuggested {
                            inactive_for_secs, ..
                        } => CallSignalOutcome::StopSuggested {
                            app: ended_app,
                            inactive_for_secs,
                        },
                        other => other,
                    };
                }
            }
            // Still under threshold (or inside the KeepRecording
            // cooldown). Silent debounce.
            self.detection_emitted = false;
            return CallSignalOutcome::NoChange;
        }

        let was_active_and_emitted = was_active && self.detection_emitted;
        self.detection_emitted = false;
        self.current_app = None;
        if was_active_and_emitted {
            CallSignalOutcome::Ended { app: ended_app }
        } else {
            CallSignalOutcome::NoChange
        }
    }

    fn handle_active(
        &mut self,
        app: Option<String>,
        is_meeting_active: bool,
        now: i64,
    ) -> CallSignalOutcome {
        self.last_mic_active = true;

        // Somebody is holding the mic again. If it had been free long enough
        // for the previous call to be over, the hold from a hand-pressed Stop
        // is done: this is a new call, not the one that was stopped.
        //
        // Judged HERE, on the active edge, and not while inactive: the hosts
        // signal "mic free" once, as an edge, and then go quiet. Accumulating
        // the interval inside the inactive path would have waited for ticks
        // that never come, and the hold would never have lifted on Windows.
        if let Some(free_since) = self.mic_free_since.take() {
            if now.saturating_sub(free_since) >= self.release_confirm_secs as i64 {
                self.barred = None;
                self.tail_of = None;
                // Same standard for the call we walked in on. Teams drops
                // its capture session and reopens it under the same id
                // mid-call — half a second on 2026-09-24 11:57, four
                // seconds the day before — and clearing on the first quiet
                // tick read that flicker as a hangup, so the call the user
                // was still sitting in started recording itself.
                self.preexisting.clear();
            }
        }
        // There is deliberately NO deadline here. A hold used to expire
        // blind after half an hour, which meant a long call the user had
        // stopped by hand started recording itself again partway through
        // — the one outcome the hold exists to prevent. Between "misses an
        // auto-record the user can start by hand" and "records a call they
        // explicitly stopped", the first is the cheaper mistake, so the
        // hold now waits for evidence however long that takes.

        // App may be inferred late in the session (whitelist process
        // launched after mic activation). Take the first non-None we
        // see; don't overwrite once set so cooldown keys stay stable.
        if self.current_app.is_none() {
            self.current_app = app;
        }
        let app_for_lookup = self
            .current_app
            .clone()
            .unwrap_or_else(|| GLOBAL_COOLDOWN_KEY.to_string());

        // Already going when we arrived. Recorded BEFORE the meeting-active
        // check so that a restart during a meeting we are recording still
        // marks the session, and stopping that meeting by hand does not
        // then hand the same live call straight back to auto-record.
        if !self.observed_idle {
            self.preexisting.insert(app_for_lookup.clone());
        }

        if is_meeting_active {
            return CallSignalOutcome::Suppressed(SuppressionReason::MeetingActive);
        }
        if self.preexisting.contains(&app_for_lookup) {
            return CallSignalOutcome::Suppressed(SuppressionReason::AlreadyRunning(
                app_for_lookup,
            ));
        }
        // Already accepted "record now": the host is bringing up the meeting
        // (consent modal, window open) — that can take several seconds during
        // which the meeting isn't active yet. Without this guard the detector
        // re-crosses the activity threshold and emits a SECOND call_detected
        // nudge before recording begins. recording_active_from_us is cleared
        // by meeting_stopped(), re-arming detection for the next call.
        if self.recording_active_from_us {
            return CallSignalOutcome::Suppressed(SuppressionReason::RecordingAccepted);
        }
        // Stopped by hand, call still going. The host keeps signalling this
        // same live session every 250 ms — that is how one call restarted
        // itself six times in twenty-five seconds — so the refusal has to
        // hold against a signal that never stops arriving.
        if self.tail_of.as_deref() == Some(app_for_lookup.as_str()) {
            return CallSignalOutcome::Suppressed(SuppressionReason::TailOfLastCall(
                app_for_lookup,
            ));
        }
        if self.barred.as_deref() == Some(app_for_lookup.as_str()) {
            return CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser(app_for_lookup));
        }
        if let Some(real) = &self.current_app {
            if self.excluded.contains(real) {
                return CallSignalOutcome::Suppressed(SuppressionReason::Excluded(real.clone()));
            }
        }
        if let Some(until) = self.cooldown_until.get(&app_for_lookup) {
            if now < *until {
                return CallSignalOutcome::Suppressed(SuppressionReason::Cooldown(app_for_lookup));
            }
        }

        if self.mic_active_since.is_none() {
            self.mic_active_since = Some(now);
        }
        if self.detection_emitted {
            return CallSignalOutcome::NoChange;
        }
        let since = self
            .mic_active_since
            .expect("mic_active_since set above when None");
        let elapsed = now - since;
        if elapsed >= self.min_active_secs as i64 {
            self.detection_emitted = true;
            CallSignalOutcome::Detected {
                app: self.current_app.clone(),
                since_seconds: elapsed,
            }
        } else {
            CallSignalOutcome::Suppressed(SuppressionReason::Debouncing)
        }
    }

    /// Record the user's response to a nudge. App id must match the
    /// nudge that was emitted (None ↔ GLOBAL_COOLDOWN_KEY).
    pub fn record_response(&mut self, app: Option<String>, response: NudgeResponse, now: i64) {
        let key = app
            .clone()
            .unwrap_or_else(|| GLOBAL_COOLDOWN_KEY.to_string());
        match response {
            NudgeResponse::RecordNow => {
                self.detection_emitted = false;
                self.mic_active_since = None;
                // Remember whose call this is BEFORE current_app is dropped:
                // `meeting_stopped` needs it to hold the right app back.
                self.recorded_app = self.current_app.take().or(app.clone());
                // From this moment on, the meeting is "ours" → stop-
                // suggestion path is armed. Cleared by meeting_stopped().
                self.recording_active_from_us = true;
                self.mic_inactive_since = None;
                self.sys_inactive_since = None;
                self.stop_suggestion_emitted = false;
                self.stop_suggestion_until = None;
            }
            NudgeResponse::NotNow => {
                self.cooldown_until
                    .insert(key, now + self.cooldown_secs as i64);
                self.detection_emitted = false;
                self.mic_active_since = None;
            }
            NudgeResponse::Never => {
                if let Some(a) = app {
                    self.excluded.insert(a);
                }
                self.detection_emitted = false;
                self.mic_active_since = None;
            }
            NudgeResponse::Timeout => {
                self.cooldown_until
                    .insert(key, now + self.timeout_cooldown_secs as i64);
                self.detection_emitted = false;
                self.mic_active_since = None;
            }
            NudgeResponse::StopAndRecap => {
                // Caller will invoke meeting_stopped() right after this
                // (when dimmy_meeting_stop returns); we keep flags as-is
                // here and let that hook do the reset. The one thing we
                // record is whose decision the stop was — and only while a
                // recording of ours is actually running, so a nudge answered
                // after the meeting already ended cannot disarm the NEXT
                // hand-pressed Stop.
                self.stop_is_automatic = self.recording_active_from_us;
            }
            NudgeResponse::KeepRecording => {
                // Push out re-asking by `stop_keep_cooldown_secs` and
                // re-arm the emitter — if both sides go inactive again
                // AFTER the cooldown, we'll suggest stop again.
                self.stop_suggestion_emitted = false;
                self.mic_inactive_since = None;
                self.sys_inactive_since = None;
                self.stop_suggestion_until = Some(now + self.stop_keep_cooldown_secs as i64);
            }
            NudgeResponse::StopTimeout => {
                // Auto-dismiss without action — re-arm after a short
                // cooldown so we can still propose stop later in the
                // same meeting.
                self.stop_suggestion_emitted = false;
                self.mic_inactive_since = None;
                self.sys_inactive_since = None;
                self.stop_suggestion_until = Some(now + self.timeout_cooldown_secs as i64);
            }
        }
    }

    /// Hook called by the FFI bridge whenever a meeting ends (user
    /// stopped from the pill / meeting window, or our stop-suggestion
    /// path completed). Resets the recording-active flags so the next
    /// detection starts clean.
    pub fn meeting_stopped(&mut self) {
        self.meeting_stopped_at_inner(false);
    }

    /// `meeting_stopped` for callers that know the stop really happened
    /// now. Only this form can place a hold.
    ///
    /// The clock argument is gone: the hold has no deadline any more, so
    /// there is nothing left to measure against. It used to expire blind
    /// after half an hour, which meant a long call stopped by hand handed
    /// itself back to auto-record partway through.
    ///
    /// The zero-argument form above genuinely bars nothing, as its comment
    /// always claimed. It used to pass `now = 0`, which DID place a hold
    /// and then relied on the backstop to undo it — so removing the
    /// backstop would have left those callers holding for ever.
    pub fn meeting_stopped_at(&mut self, _now: i64) {
        self.meeting_stopped_at_inner(true);
    }

    fn meeting_stopped_at_inner(&mut self, may_hold: bool) {
        let recorded = self.recorded_app.take();
        let was_ours = self.recording_active_from_us;
        if self.stop_is_automatic && may_hold {
            // We stopped because the call ended — so the next call, the one
            // the user hung up for, must record by itself. It still does:
            // this hold is lifted by the microphone being genuinely free,
            // which by definition it is once a call has really ended.
            //
            // What it stops is the same app taking the microphone straight
            // back, which is not the next call but the tail of the one we
            // just recorded.
            self.stop_is_automatic = false;
            self.tail_of = Some(recorded.unwrap_or_else(|| GLOBAL_COOLDOWN_KEY.to_string()));
            self.mic_free_since = None;
        } else if self.stop_is_automatic {
            self.stop_is_automatic = false;
        } else if was_ours && may_hold {
            // Pressed by hand, while that call is in all likelihood still
            // running. Hold it until the call is really over. An app-less
            // call is held under the global key rather than not at all.
            self.barred = Some(recorded.unwrap_or_else(|| GLOBAL_COOLDOWN_KEY.to_string()));
            // The release clock starts at the stop. A silent stretch DURING
            // the meeting is not evidence that the call ended.
            self.mic_free_since = None;
        }
        self.recording_active_from_us = false;
        self.mic_inactive_since = None;
        self.sys_inactive_since = None;
        self.stop_suggestion_emitted = false;
        self.stop_suggestion_until = None;
        self.has_tracked_origin = false;
    }

    /// Host tells the detector whether it is deterministically watching a
    /// meeting-origin process (a detected call app). While true, the silence
    /// heuristic is suppressed and only `signal_call_session_ended` (the
    /// process-gone signal) can stop-suggest — see `has_tracked_origin`. Set
    /// true when the host binds/adopts an origin pid, false when it clears it
    /// or the meeting ends. Idempotent.
    pub fn set_tracked_origin(&mut self, tracked: bool) {
        self.has_tracked_origin = tracked;
    }

    /// Tell the state machine whether the next `mic_active = false` means
    /// "nobody is holding the microphone". The hosts send that zero while
    /// Dimmy itself is capturing — deliberately, so the pill cannot
    /// self-trigger — and a dictation is not evidence that somebody else's
    /// call has ended. Defaults to true; the FFI sets it per signal.
    pub fn set_mic_free_is_evidence(&mut self, evidence: bool) {
        self.mic_free_is_evidence = evidence;
    }

    /// A meeting was started OUTSIDE the "Record now" nudge — i.e. manually
    /// from the meeting window / pill — while a call was detected. Arm the
    /// stop-suggestion path exactly as `record_response(RecordNow)` does so
    /// that the call ending still suggests stop. Without this, a manually-
    /// started meeting left `recording_active_from_us=false`, so
    /// `signal_call_session_ended` returned NoChange and no popup appeared.
    /// Idempotent. Cleared by `meeting_stopped()`.
    pub fn meeting_started_external(&mut self) {
        self.recording_active_from_us = true;
        // Started by hand, but ON a detected call: stopping it by hand has
        // to close the same door, or the loop walks back in through the
        // manual entrance.
        if self.recorded_app.is_none() {
            self.recorded_app = self.current_app.clone();
        }
        self.mic_inactive_since = None;
        self.sys_inactive_since = None;
        self.stop_suggestion_emitted = false;
        self.stop_suggestion_until = None;
    }

    /// JSON snapshot for the Settings UI (exclusion list view +
    /// debug / observability).
    pub fn state_snapshot(&self, now: i64) -> serde_json::Value {
        let active_cooldowns: Vec<serde_json::Value> = self
            .cooldown_until
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(app, until)| json!({"app": app, "seconds_remaining": until - now}))
            .collect();
        let excluded: Vec<&String> = self.excluded.iter().collect();
        json!({
            "enabled": self.enabled,
            "min_active_secs": self.min_active_secs,
            "cooldown_secs": self.cooldown_secs,
            "excluded": excluded,
            "active_cooldowns": active_cooldowns,
            "mic_active": self.last_mic_active,
            "detection_emitted": self.detection_emitted,
            "current_app": self.current_app,
        })
    }
}

impl Default for CallDetectorState {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a detected call should start recording by itself.
///
/// Auto-record is a sub-option of detection, not a peer: with detection
/// off nothing observes the mic, so an auto-record left on would be a
/// dead switch that springs back to life the day detection is turned on
/// again — recording a call the user never agreed to record. The hosts
/// gate the nudge on this, and the config setter writes it back through
/// the same rule so the saved pair is always coherent.
pub fn auto_record_effective(detect_enabled: bool, auto_record: bool) -> bool {
    detect_enabled && auto_record
}

#[cfg(test)]
mod tests {

    // ── auto-record gating ───────────────────────────────────────

    #[test]
    fn auto_record_needs_detection() {
        assert!(auto_record_effective(true, true));
        assert!(!auto_record_effective(false, true), "detection off wins");
        assert!(!auto_record_effective(true, false));
        assert!(!auto_record_effective(false, false));
    }
    use super::*;

    /// A detector in the state it is in almost all the time: Dimmy has
    /// been running with nothing happening, so it has seen the microphone
    /// idle and the next call going active is a transition it watched.
    fn fresh() -> CallDetectorState {
        let mut s = cold();
        // One idle tick, which is what the hosts send while nothing holds
        // the mic. Without it every test would be describing the rarer
        // case below.
        s.signal(false, None, false, 1);
        s
    }

    /// A detector that has just been constructed and has never seen the
    /// microphone idle — Dimmy launched, restarted, or started at login
    /// in the middle of a call.
    fn cold() -> CallDetectorState {
        let mut s = CallDetectorState::new();
        s.configure(true, 5, 1800, 300, HashSet::new());
        s
    }

    #[test]
    fn signal_mic_inactive_does_nothing_on_clean_state() {
        let mut s = fresh();
        let out = s.signal(false, None, false, 1000);
        assert_eq!(out, CallSignalOutcome::NoChange);
    }

    #[test]
    fn signal_mic_active_under_debounce_returns_debouncing() {
        let mut s = fresh();
        let out = s.signal(true, Some("teams".into()), false, 1000);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Debouncing)
        );
        // 3 s later still under 5 s debounce
        let out = s.signal(true, Some("teams".into()), false, 1003);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Debouncing)
        );
    }

    #[test]
    fn signal_mic_active_over_debounce_emits_detected_once() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let out = s.signal(true, Some("teams".into()), false, 1005);
        assert!(matches!(out, CallSignalOutcome::Detected { .. }));
        // Subsequent ticks while session is still active → NoChange
        let out2 = s.signal(true, Some("teams".into()), false, 1010);
        assert_eq!(out2, CallSignalOutcome::NoChange);
    }

    #[test]
    fn signal_mic_inactive_after_detected_emits_ended() {
        let mut s = fresh();
        s.signal(true, Some("zoom".into()), false, 1000);
        s.signal(true, Some("zoom".into()), false, 1005);
        let out = s.signal(false, None, false, 1010);
        assert_eq!(
            out,
            CallSignalOutcome::Ended {
                app: Some("zoom".into())
            }
        );
    }

    #[test]
    fn signal_during_meeting_active_returns_suppressed() {
        let mut s = fresh();
        let out = s.signal(true, Some("teams".into()), true, 1000);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::MeetingActive)
        );
    }

    #[test]
    fn signal_for_excluded_app_returns_suppressed() {
        let mut s = fresh();
        let mut excluded = HashSet::new();
        excluded.insert("discord".to_string());
        s.configure(true, 5, 1800, 300, excluded);
        let out = s.signal(true, Some("discord".into()), false, 1000);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Excluded("discord".into()))
        );
    }

    #[test]
    fn signal_within_per_app_cooldown_returns_suppressed() {
        let mut s = fresh();
        s.record_response(Some("teams".into()), NudgeResponse::NotNow, 1000);
        let out = s.signal(true, Some("teams".into()), false, 1500);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Cooldown("teams".into()))
        );
    }

    #[test]
    fn cooldown_expires_re_emits_on_new_session() {
        let mut s = fresh();
        s.record_response(Some("teams".into()), NudgeResponse::NotNow, 1000);
        // 1800s + 1 → past cooldown
        s.signal(true, Some("teams".into()), false, 2801);
        let out = s.signal(true, Some("teams".into()), false, 2806);
        assert!(matches!(out, CallSignalOutcome::Detected { .. }));
    }

    #[test]
    fn record_response_never_adds_to_exclusion() {
        let mut s = fresh();
        s.record_response(Some("zoom".into()), NudgeResponse::Never, 1000);
        let out = s.signal(true, Some("zoom".into()), false, 2000);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Excluded("zoom".into()))
        );
    }

    #[test]
    fn record_now_suppresses_redetection_until_meeting_stopped() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        s.signal(true, Some("teams".into()), false, 1005); // Detected
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        // The host is bringing up the meeting (consent modal up): the meeting
        // is not active yet, but we already accepted. No matter how long the
        // modal stays up, the detector must NOT emit a second call_detected —
        // that was the "double notification" the consent gate exposed.
        let out = s.signal(true, Some("teams".into()), false, 1100);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::RecordingAccepted)
        );
        // After the meeting ends, detection re-arms for a fresh call
        // (min_active_secs = 5, so a full debounce window must elapse again).
        s.meeting_stopped();
        s.signal(true, Some("teams".into()), false, 2000);
        let out2 = s.signal(true, Some("teams".into()), false, 2010);
        assert!(
            matches!(out2, CallSignalOutcome::Detected { .. }),
            "detection must re-arm after the meeting stops, got {out2:?}"
        );
    }

    #[test]
    fn tracked_origin_suppresses_silence_stop_suggestion() {
        // The 15-popups bug: a live call with a quiet stretch trips the
        // both-sides-silent threshold. While the host is watching the call's
        // origin pid deterministically, the silence heuristic must stay quiet
        // — only the process-gone signal (`signal_call_session_ended`) decides.
        let mut s = fresh();
        s.meeting_started_external();
        s.set_tracked_origin(true);
        s.signal(false, None, true, 1000); // mic goes quiet
        let out = s.signal(false, None, true, 1010); // 10 s > 5 s threshold
        assert_eq!(
            out,
            CallSignalOutcome::NoChange,
            "silence must not stop-suggest while a meeting-origin pid is tracked"
        );
        // The deterministic path is still free to fire.
        let ended = s.signal_call_session_ended(true, 1011);
        assert!(
            matches!(ended, CallSignalOutcome::StopSuggested { .. }),
            "process-gone signal must still stop-suggest, got {ended:?}"
        );
    }

    #[test]
    fn without_tracked_origin_silence_still_stop_suggests() {
        // No detectable call app → the silence backstop is the only stop
        // signal and must keep working exactly as before.
        let mut s = fresh();
        s.meeting_started_external();
        s.signal(false, None, true, 1000);
        let out = s.signal(false, None, true, 1010);
        assert!(
            matches!(out, CallSignalOutcome::StopSuggested { .. }),
            "silence backstop must fire when no origin pid is tracked, got {out:?}"
        );
    }

    #[test]
    fn disabled_state_returns_suppressed() {
        let mut s = fresh();
        s.configure(false, 5, 1800, 300, HashSet::new());
        let out = s.signal(true, Some("teams".into()), false, 1000);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Disabled)
        );
    }

    #[test]
    fn app_inferred_late_in_session_propagates_to_outcome() {
        let mut s = fresh();
        // First few seconds: mic active but no app inferred yet.
        s.signal(true, None, false, 1000);
        s.signal(true, None, false, 1002);
        // App inferred just before debounce expires.
        s.signal(true, Some("teams".into()), false, 1004);
        let out = s.signal(true, Some("teams".into()), false, 1005);
        match out {
            CallSignalOutcome::Detected { app, .. } => {
                assert_eq!(app, Some("teams".into()));
            }
            _ => panic!("expected Detected, got {:?}", out),
        }
    }

    #[test]
    fn no_app_inferred_uses_global_cooldown_key() {
        let mut s = fresh();
        s.signal(true, None, false, 1000);
        s.signal(true, None, false, 1005); // Detected, app=None
        s.record_response(None, NudgeResponse::NotNow, 1006);
        s.signal(false, None, false, 1010);
        // A brand-new generic-mic session inside cooldown should be
        // suppressed via the global key.
        let out = s.signal(true, None, false, 1500);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Cooldown(GLOBAL_COOLDOWN_KEY.into()))
        );
    }

    #[test]
    fn record_response_timeout_uses_short_cooldown() {
        let mut s = fresh();
        s.record_response(Some("teams".into()), NudgeResponse::Timeout, 1000);
        // 299 s later still in short cooldown
        let out = s.signal(true, Some("teams".into()), false, 1299);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Cooldown("teams".into()))
        );
        // 5 min + 1 s past → cooldown expired
        s.signal(false, None, false, 1305);
        s.signal(true, Some("teams".into()), false, 1306);
        let out = s.signal(true, Some("teams".into()), false, 1311);
        assert!(matches!(out, CallSignalOutcome::Detected { .. }));
    }

    /// Helper: drive the state machine into "we accepted a detection,
    /// meeting is now active, mic just went silent at `start`". Mirrors
    /// what happens in production when the user clicks Record now.
    fn arm_recording(s: &mut CallDetectorState, start: i64) {
        // 6 s of mic-active → Detected.
        s.signal(true, Some("teams".into()), false, start - 6);
        s.signal(true, Some("teams".into()), false, start - 1);
        // Accept the detection: from now on this is "our" meeting.
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, start);
        // Mic transitions to inactive at `start`.
        let _ = s.signal(false, None, true, start);
    }

    #[test]
    fn stop_suggested_mic_only_after_5s_when_sys_signaling_disabled() {
        let mut s = fresh();
        arm_recording(&mut s, 1000);
        // 4 s of silence — under the 5 s threshold.
        let out = s.signal(false, None, true, 1004);
        assert_eq!(out, CallSignalOutcome::NoChange);
        // 5 s of silence — threshold met, sys-signaling disabled so
        // fall back to mic-only.
        let out = s.signal(false, None, true, 1005);
        assert!(
            matches!(out, CallSignalOutcome::StopSuggested { inactive_for_secs, .. } if inactive_for_secs >= 5),
            "expected StopSuggested, got {:?}",
            out
        );
    }

    #[test]
    fn stop_suggested_requires_sys_silent_when_sys_signaling_enabled() {
        let mut s = fresh();
        arm_recording(&mut s, 1000);
        // Host starts signalling sys-audio activity at t=1000, ACTIVE.
        let _ = s.signal_sys(true, true, 1000);
        // Mic silent for 10 s; sys still active → no stop.
        let out = s.signal(false, None, true, 1010);
        assert_eq!(out, CallSignalOutcome::NoChange);
        // Sys goes silent at t=1010; AND threshold needs 5 more s.
        let _ = s.signal_sys(false, true, 1010);
        let out = s.signal(false, None, true, 1014);
        assert_eq!(out, CallSignalOutcome::NoChange);
        // Both sides silent ≥ 5 s now (mic since 1000, sys since 1010).
        let out = s.signal_sys(false, true, 1015);
        assert!(
            matches!(out, CallSignalOutcome::StopSuggested { .. }),
            "expected StopSuggested, got {:?}",
            out
        );
    }

    #[test]
    fn stop_suggested_emitted_exactly_once_per_meeting() {
        let mut s = fresh();
        arm_recording(&mut s, 1000);
        let _ = s.signal_sys(false, true, 1000);
        let first = s.signal(false, None, true, 1006);
        assert!(matches!(first, CallSignalOutcome::StopSuggested { .. }));
        // Same conditions next tick — must NOT re-emit.
        let second = s.signal(false, None, true, 1007);
        assert_eq!(second, CallSignalOutcome::NoChange);
        let third = s.signal_sys(false, true, 1008);
        assert_eq!(third, CallSignalOutcome::NoChange);
    }

    #[test]
    fn keep_recording_response_re_arms_after_cooldown() {
        let mut s = fresh();
        arm_recording(&mut s, 1000);
        let _ = s.signal_sys(false, true, 1000);
        let out = s.signal(false, None, true, 1006);
        assert!(matches!(out, CallSignalOutcome::StopSuggested { .. }));
        // User says "keep recording" — 300 s cooldown.
        s.record_response(Some("teams".into()), NudgeResponse::KeepRecording, 1006);
        // 200 s later, both silent again → still suppressed.
        let _ = s.signal_sys(false, true, 1200);
        let out = s.signal(false, None, true, 1206);
        assert_eq!(out, CallSignalOutcome::NoChange);
        // 301 s past the keep response → re-arms. signal_sys with the
        // mic timestamp from t=1206 (silence has been continuous since
        // then) and sys timestamp from t=1200 already meets both ≥5 s
        // thresholds, so signal_sys is the call that re-emits.
        let out = s.signal_sys(false, true, 1310);
        assert!(
            matches!(out, CallSignalOutcome::StopSuggested { .. }),
            "expected StopSuggested after cooldown, got {:?}",
            out
        );
    }

    #[test]
    fn meeting_stopped_resets_state_so_next_meeting_can_emit() {
        let mut s = fresh();
        arm_recording(&mut s, 1000);
        let _ = s.signal_sys(false, true, 1000);
        let first = s.signal(false, None, true, 1006);
        assert!(matches!(first, CallSignalOutcome::StopSuggested { .. }));
        // Meeting ends — state machine should re-arm clean.
        s.meeting_stopped();
        // New detection cycle.
        arm_recording(&mut s, 2000);
        let _ = s.signal_sys(false, true, 2000);
        let out = s.signal(false, None, true, 2006);
        assert!(
            matches!(out, CallSignalOutcome::StopSuggested { .. }),
            "second meeting must be able to emit, got {:?}",
            out
        );
    }

    #[test]
    fn signal_sys_alone_does_not_emit_without_active_recording() {
        let mut s = fresh();
        // No RecordNow accepted → recording_active_from_us = false.
        let out = s.signal_sys(false, true, 1000);
        assert_eq!(out, CallSignalOutcome::NoChange);
        let out = s.signal_sys(false, true, 1010);
        assert_eq!(out, CallSignalOutcome::NoChange);
    }

    #[test]
    fn sys_active_during_mic_silence_blocks_stop_until_sys_also_quiet() {
        let mut s = fresh();
        arm_recording(&mut s, 1000);
        // Sys never goes silent — mic silence alone must not fire stop.
        let _ = s.signal_sys(true, true, 1000);
        let _ = s.signal_sys(true, true, 1010);
        let _ = s.signal_sys(true, true, 1100);
        let out = s.signal(false, None, true, 1100);
        assert_eq!(out, CallSignalOutcome::NoChange);
    }

    #[test]
    fn state_snapshot_omits_expired_cooldowns() {
        let mut s = fresh();
        s.record_response(Some("teams".into()), NudgeResponse::NotNow, 1000);
        s.record_response(Some("zoom".into()), NudgeResponse::NotNow, 1500);
        // teams cooldown ends at 2800, zoom at 3300.
        let snap = s.state_snapshot(2900);
        let active = snap["active_cooldowns"].as_array().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0]["app"], "zoom");
    }

    // ── meeting_started_external: arm stop-suggestion for a MANUAL start ──

    #[test]
    fn meeting_started_external_arms_session_ended_stop_path() {
        let mut s = fresh();
        // Manual start (no Record-now nudge accepted) leaves
        // recording_active_from_us=false, so a detected call ending
        // suggests nothing — this is the bug the FFI fixes.
        assert_eq!(
            s.signal_call_session_ended(true, 1000),
            CallSignalOutcome::NoChange,
            "un-armed manual meeting must not suggest stop"
        );
        // Arm via the manual / mid-meeting path (what
        // dimmy_call_meeting_started_external calls).
        s.meeting_started_external();
        assert!(
            matches!(
                s.signal_call_session_ended(true, 1001),
                CallSignalOutcome::StopSuggested { .. }
            ),
            "armed manual meeting must suggest stop when the call ends"
        );
    }

    #[test]
    fn meeting_started_external_emits_once_and_is_idempotent() {
        let mut s = fresh();
        // Host may arm on the start edge AND again on a mid-meeting tick.
        s.meeting_started_external();
        s.meeting_started_external();
        assert!(matches!(
            s.signal_call_session_ended(true, 1000),
            CallSignalOutcome::StopSuggested { .. }
        ));
        // Single-shot: the next session-ended tick is NoChange.
        assert_eq!(
            s.signal_call_session_ended(true, 1001),
            CallSignalOutcome::NoChange
        );
    }

    #[test]
    fn meeting_stopped_clears_external_arm() {
        // After the meeting ends the external arm must be cleared so a
        // stray call-ended signal can't suggest stop for a meeting that is
        // no longer recording (recording_active_from_us is the gate).
        let mut s = fresh();
        s.meeting_started_external();
        s.meeting_stopped();
        assert_eq!(
            s.signal_call_session_ended(true, 1000),
            CallSignalOutcome::NoChange
        );
    }

    // ── The hand-pressed Stop hold ───────────────────────────────
    //
    // One test per defect an adversarial review found in the first
    // attempt at this rule, plus the incident that started it.

    /// The incident, 2026-09-23: auto-record started a Teams call, the user
    /// pressed Stop, and the host kept signalling the same live session
    /// every 250 ms — six meetings and five recap calls in twenty-five
    /// seconds, with no session transition between them. Refusing has to
    /// hold against a signal that never stops arriving.
    #[test]
    fn a_hand_pressed_stop_holds_the_same_live_call() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let first = s.signal(true, Some("teams".into()), false, 1005);
        assert!(matches!(first, CallSignalOutcome::Detected { .. }));
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        for t in 1101..1131 {
            let out = s.signal(true, Some("teams".into()), false, t);
            assert_eq!(
                out,
                CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser("teams".into())),
                "tick {t} must not start anything"
            );
        }
    }

    /// One call ends and the user places another. That one records by
    /// itself — the requirement this must never break.
    ///
    /// It survives the tail hold because the hold is lifted by evidence,
    /// not by a clock: once a call has really ended the microphone IS
    /// free, and placing another takes a human tens of seconds.
    #[test]
    fn an_automatic_stop_holds_nothing_back() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.record_response(Some("teams".into()), NudgeResponse::StopAndRecap, 1100);
        s.meeting_stopped_at(1101);

        // The microphone really is free, because the call really ended.
        s.signal(false, None, false, 1102);
        // The next call, placed by a human.
        s.signal(true, Some("teams".into()), false, 1140);
        let out = s.signal(true, Some("teams".into()), false, 1145);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "the next call must record by itself, got {out:?}"
        );
    }

    /// Reported from macOS on 2026-09-24: every auto-stopped meeting left
    /// a second one of a few seconds behind it in the list. The app took
    /// its microphone straight back and that read as the next call.
    #[test]
    fn the_tail_of_a_call_we_just_stopped_is_not_the_next_call() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.record_response(Some("teams".into()), NudgeResponse::StopAndRecap, 1200);
        s.meeting_stopped_at(1201);

        // Two seconds later the same app is holding the microphone again.
        for t in [1203, 1204, 1208] {
            let out = s.signal(true, Some("teams".into()), false, t);
            assert_eq!(
                out,
                CallSignalOutcome::Suppressed(SuppressionReason::TailOfLastCall("teams".into())),
                "tick {t}: no four-second meeting behind the real one"
            );
        }
    }

    /// A different app is a different call, whatever we just stopped.
    #[test]
    fn the_tail_hold_is_keyed_to_the_app_it_came_from() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.record_response(Some("teams".into()), NudgeResponse::StopAndRecap, 1200);
        s.meeting_stopped_at(1201);

        s.signal(true, Some("zoom".into()), false, 1203);
        let out = s.signal(true, Some("zoom".into()), false, 1208);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "another app is another call, got {out:?}"
        );
    }

    /// DEFECT 1 — the hold never lifted on Windows, because the host signals
    /// "mic free" once as an edge and then goes quiet. The interval must be
    /// judged when the mic comes BACK, not by waiting for inactive ticks.
    #[test]
    fn the_hold_lifts_from_a_single_free_edge() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        // ONE inactive signal — the whole cadence Windows guarantees.
        let _ = s.signal(false, None, false, 1200);

        // Next call, 30 s later.
        s.signal(true, Some("teams".into()), false, 1230);
        let out = s.signal(true, Some("teams".into()), false, 1235);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "a single free edge is all the host sends — it must be enough, got {out:?}"
        );
    }

    /// DEFECT 2 — while Dimmy dictates, both hosts send mic_active = 0 every
    /// tick so the pill cannot self-trigger. That zero is not evidence that
    /// somebody else's call ended.
    #[test]
    fn a_dictation_does_not_lift_the_hold() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        // Twenty seconds of dictation: the mic is ours, not free.
        s.set_mic_free_is_evidence(false);
        for t in 1101..1121 {
            let _ = s.signal(false, None, false, t);
        }
        s.set_mic_free_is_evidence(true);

        let out = s.signal(true, Some("teams".into()), false, 1122);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser("teams".into())),
            "our own microphone is not proof that their call ended"
        );
    }

    /// DEFECT 3 — silence DURING the meeting must not pre-charge the release
    /// clock, or the hold is over the instant it is applied.
    #[test]
    fn silence_during_the_meeting_does_not_pre_charge_the_release() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);

        // A long quiet stretch while the meeting runs (everyone listening).
        for t in 1010..1080 {
            let _ = s.signal(false, None, true, t);
        }
        s.meeting_stopped_at(1081);

        let out = s.signal(true, Some("teams".into()), false, 1082);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser("teams".into())),
            "the release clock starts at the stop, not at a mid-meeting silence"
        );
    }

    /// DEFECT 4 — StopAndRecap answered when no recording of ours is running
    /// must not disarm the NEXT hand-pressed Stop.
    #[test]
    fn a_stale_stop_response_cannot_disarm_the_next_hand_stop() {
        let mut s = fresh();
        // A stray response with nothing running.
        s.record_response(Some("teams".into()), NudgeResponse::StopAndRecap, 900);

        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        let out = s.signal(true, Some("teams".into()), false, 1102);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser("teams".into())),
            "the hand-pressed Stop still holds"
        );
    }

    /// DEFECT 5 — a call with no app inferred (common on macOS) was held by
    /// nothing at all, so the loop survived there.
    #[test]
    fn a_call_with_no_app_is_held_under_the_global_key() {
        let mut s = fresh();
        s.signal(true, None, false, 1000);
        let first = s.signal(true, None, false, 1005);
        assert!(matches!(first, CallSignalOutcome::Detected { .. }));
        s.record_response(None, NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        let out = s.signal(true, None, false, 1102);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser(
                GLOBAL_COOLDOWN_KEY.to_string()
            ))
        );
    }

    /// A plain meeting with no call behind it holds nothing: there is no
    /// call to protect, and barring the global key would have switched
    /// auto-record off machine-wide.
    #[test]
    fn a_meeting_with_no_detected_call_holds_nothing() {
        let mut s = fresh();
        s.meeting_stopped_at(1000);
        s.signal(true, Some("teams".into()), false, 1001);
        let out = s.signal(true, Some("teams".into()), false, 1006);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "got {out:?}"
        );
    }

    /// The hold is one app's, not the machine's.
    #[test]
    fn holding_one_app_leaves_the_others_alone() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        s.signal(true, Some("zoom".into()), false, 1102);
        let out = s.signal(true, Some("zoom".into()), false, 1110);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "another app must be detected at once, got {out:?}"
        );
    }

    /// The measured flicker — session gone at 00:01:29, back under the same
    /// id at 00:01:33 — is four seconds. It must not pass for a hangup.
    #[test]
    fn a_four_second_gap_does_not_lift_the_hold() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        let _ = s.signal(false, None, false, 1200);
        let out = s.signal(true, Some("teams".into()), false, 1204);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser("teams".into())),
            "four seconds is a flicker, not a hangup"
        );
    }

    /// Evidence is the ONLY way out. A deadline used to lift the hold
    /// blind after half an hour, which is how a long call the user had
    /// stopped by hand started recording itself again partway through.
    #[test]
    fn a_hold_with_no_evidence_never_lifts_on_its_own() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped_at(1100);

        // Two hours of the same live session, not one free tick. The old
        // backstop was 1800 s, so every one of these would have recorded.
        for t in [2901, 2906, 4000, 8500] {
            let out = s.signal(true, Some("teams".into()), false, t);
            assert_eq!(
                out,
                CallSignalOutcome::Suppressed(SuppressionReason::StoppedByUser("teams".into())),
                "tick {t}: a call stopped by hand must not restart itself"
            );
        }

        // The call really ends. THAT is what lifts it.
        s.signal(false, None, false, 8600);
        s.signal(true, Some("teams".into()), false, 8700);
        let out = s.signal(true, Some("teams".into()), false, 8705);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "a genuinely new call must record, got {out:?}"
        );
    }

    /// The 2026-09-24 11:33 incident: Dimmy was restarted mid-call and
    /// started recording the meeting the user was already sitting in.
    #[test]
    fn a_call_already_running_at_startup_is_not_a_call_starting() {
        let mut s = cold();
        // Teams has been live for an hour; we have only just woken up.
        s.signal(true, Some("teams".into()), false, 1000);
        for t in [1005, 1010, 1200, 3000] {
            let out = s.signal(true, Some("teams".into()), false, t);
            assert_eq!(
                out,
                CallSignalOutcome::Suppressed(SuppressionReason::AlreadyRunning("teams".into())),
                "tick {t}: we never saw this call start"
            );
        }
    }

    /// The flicker that got past the first version of rule A: Teams drops
    /// its capture session and reopens it under the same id, mid-call.
    /// Measured at half a second on 2026-09-24 11:57, four seconds the day
    /// before. Clearing on the first quiet tick read that as a hangup and
    /// recorded the call the user was still in.
    #[test]
    fn a_flicker_does_not_turn_the_call_we_walked_in_on_into_a_new_one() {
        let mut s = cold();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1010);

        // The session vanishes and comes straight back, still the same call.
        s.signal(false, None, false, 1240);
        s.signal(true, Some("teams".into()), false, 1241);
        let out = s.signal(true, Some("teams".into()), false, 1246);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::AlreadyRunning("teams".into())),
            "one second of silence is a flicker, not a hangup, got {out:?}"
        );
    }

    /// ...but only that call. Once it really ends, we have watched a full
    /// transition and the next one is ours to offer.
    #[test]
    fn the_call_after_the_one_we_walked_in_on_records_normally() {
        let mut s = cold();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1010);
        // It ends, and stays ended past the confirmation window.
        s.signal(false, None, false, 2000);
        // A new one begins, and this time we watched it begin.
        s.signal(true, Some("teams".into()), false, 3000);
        let out = s.signal(true, Some("teams".into()), false, 3010);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "the next call is a start we witnessed, got {out:?}"
        );
    }

    /// A clockless stop must not create a hold that can never be lifted.
    /// It used to pass now = 0 and lean on the backstop to undo it.
    #[test]
    fn the_clockless_stop_holds_nothing() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        let _ = s.signal(true, Some("teams".into()), false, 1005);
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        s.meeting_stopped();
        s.signal(true, Some("teams".into()), false, 2000);
        let out = s.signal(true, Some("teams".into()), false, 2010);
        assert!(
            matches!(out, CallSignalOutcome::Detected { .. }),
            "no clock means no hold, got {out:?}"
        );
    }
}
