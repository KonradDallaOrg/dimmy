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
            recording_active_from_us: false,
            mic_inactive_since: None,
            sys_inactive_since: None,
            sys_signaling_enabled: false,
            stop_suggestion_emitted: false,
            stop_suggestion_until: None,
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

        if is_meeting_active {
            return CallSignalOutcome::Suppressed(SuppressionReason::MeetingActive);
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
                self.current_app = None;
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
                // here and let that hook do the reset.
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
        self.recording_active_from_us = false;
        self.mic_inactive_since = None;
        self.sys_inactive_since = None;
        self.stop_suggestion_emitted = false;
        self.stop_suggestion_until = None;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> CallDetectorState {
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
    fn record_response_record_now_resets_state() {
        let mut s = fresh();
        s.signal(true, Some("teams".into()), false, 1000);
        s.signal(true, Some("teams".into()), false, 1005); // Detected
        s.record_response(Some("teams".into()), NudgeResponse::RecordNow, 1006);
        // After Record now, mic still active but state is reset so a
        // *new* session needs a *new* debounce window.
        let out = s.signal(true, Some("teams".into()), false, 1007);
        assert_eq!(
            out,
            CallSignalOutcome::Suppressed(SuppressionReason::Debouncing)
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
}
