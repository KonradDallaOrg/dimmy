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
    RecordNow,
    NotNow,
    Never,
    Timeout,
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
    Suppressed(SuppressionReason),
}

pub struct CallDetectorState {
    enabled: bool,
    min_active_secs: u32,
    cooldown_secs: u32,
    timeout_cooldown_secs: u32,
    excluded: HashSet<String>,

    last_mic_active: bool,
    mic_active_since: Option<i64>,
    detection_emitted: bool,
    current_app: Option<String>,
    cooldown_until: HashMap<String, i64>,
}

impl CallDetectorState {
    pub fn new() -> Self {
        Self {
            enabled: true,
            min_active_secs: 5,
            cooldown_secs: 1800,
            timeout_cooldown_secs: 300,
            excluded: HashSet::new(),
            last_mic_active: false,
            mic_active_since: None,
            detection_emitted: false,
            current_app: None,
            cooldown_until: HashMap::new(),
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
            // A disabled detector must still observe transitions so
            // re-enabling later starts clean — but always returns
            // Suppressed.
            self.last_mic_active = mic_active;
            return CallSignalOutcome::Suppressed(SuppressionReason::Disabled);
        }

        if !mic_active {
            return self.handle_inactive();
        }

        self.handle_active(app, is_meeting_active, now)
    }

    fn handle_inactive(&mut self) -> CallSignalOutcome {
        let was_active_and_emitted = self.last_mic_active && self.detection_emitted;
        let ended_app = self.current_app.clone();
        self.last_mic_active = false;
        self.mic_active_since = None;
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
        }
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
