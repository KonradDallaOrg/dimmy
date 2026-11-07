//! The dictation session: one state machine for every way a dictation starts,
//! stops or is thrown away, on every platform.
//!
//! The hosts used to keep this in loose flags (Windows had six, plus a forced
//! reset to idle; macOS had its own enum and rules), so the answer to "what
//! does a press do right now" depended on which flag a code path checked.
//! Commands arriving close together found the gaps: a press while the last
//! dictation was still transcribing vanished without a sign, and the release
//! that followed put the pill back to idle while the transcript was still on
//! its way.
//!
//! Hosts hand every input to [`input`] and do what the returned [`Action`]
//! says. The table, in short:
//!
//! | state     | press               | release (same kind)        | stop     | delivered |
//! |-----------|---------------------|----------------------------|----------|-----------|
//! | idle      | start               | -                          | -        | -         |
//! | recording | toggle: finish (*)  | hold: finish, or discard   | finish   | -         |
//! |           | hold: -             | if held < `MIN_HOLD_MS`    |          |           |
//! | finishing | busy (tell the user)| -                          | -        | idle      |
//!
//! (*) not within `TOGGLE_BOUNCE_MS` of the start: that is the same press
//! bouncing. The mode is fixed when the session starts, so changing it in
//! Settings mid-dictation can't turn a release into a stop.

use std::sync::Mutex;
use std::time::Instant;

/// A push-to-talk press shorter than this was not meant: the recording is
/// thrown away, no transcription, no empty result. Same value macOS used.
pub const MIN_HOLD_MS: u64 = 150;

/// A toggle press this soon after the start is the same press bouncing.
pub const TOGGLE_BOUNCE_MS: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dictation,
    Command,
    /// The spoken words drive the PC (open, place, find) instead of being typed.
    Execute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Toggle,
    Hold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A shortcut (or the menu) was pressed.
    Press(Kind, Mode),
    /// A shortcut was released.
    Release(Kind),
    /// Stop requested from the pill, the menu, or the length cap.
    Stop,
    /// The host's start call failed: nothing is recording.
    StartFailed,
    /// The host finished transcribing and delivering (or gave up).
    Delivered,
    /// Abandon whatever is going on (quit, meeting took over).
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do.
    None,
    /// Start recording for this kind.
    Start(Kind),
    /// Stop, transcribe, deliver. Reply with `Delivered` when done.
    Finish,
    /// Throw the recording away: no transcription.
    Discard,
    /// The last dictation is still being transcribed: tell the user, do nothing else.
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Recording {
        kind: Kind,
        mode: Mode,
        since_ms: u64,
    },
    Finishing,
}

#[derive(Debug)]
pub struct Session {
    phase: Phase,
}

impl Default for Session {
    fn default() -> Self {
        Session { phase: Phase::Idle }
    }
}

impl Session {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Feed one input at `now_ms` (any monotonic clock, in ms).
    pub fn input(&mut self, input: Input, now_ms: u64) -> Action {
        let (next, action) = match (self.phase, input) {
            (_, Input::Cancel) => (Phase::Idle, Action::None),

            (Phase::Idle, Input::Press(kind, mode)) => (
                Phase::Recording {
                    kind,
                    mode,
                    since_ms: now_ms,
                },
                Action::Start(kind),
            ),
            (Phase::Idle, _) => (Phase::Idle, Action::None),

            (Phase::Recording { .. }, Input::StartFailed) => (Phase::Idle, Action::None),
            (Phase::Recording { .. }, Input::Stop) => (Phase::Finishing, Action::Finish),
            (
                Phase::Recording {
                    kind,
                    mode: Mode::Hold,
                    since_ms,
                },
                Input::Release(released),
            ) if released == kind => {
                if now_ms.saturating_sub(since_ms) < MIN_HOLD_MS {
                    (Phase::Idle, Action::Discard)
                } else {
                    (Phase::Finishing, Action::Finish)
                }
            }
            (
                Phase::Recording {
                    mode: Mode::Toggle,
                    since_ms,
                    ..
                },
                Input::Press(..),
            ) if now_ms.saturating_sub(since_ms) >= TOGGLE_BOUNCE_MS => {
                (Phase::Finishing, Action::Finish)
            }
            (p @ Phase::Recording { .. }, _) => (p, Action::None),

            (Phase::Finishing, Input::Press(..)) => (Phase::Finishing, Action::Busy),
            (Phase::Finishing, Input::Delivered) => (Phase::Idle, Action::None),
            (Phase::Finishing, _) => (Phase::Finishing, Action::None),
        };
        self.phase = next;
        assert!(
            !matches!(action, Action::Start(_)) || matches!(next, Phase::Recording { .. }),
            "a start always leaves the session recording"
        );
        assert!(
            !matches!(action, Action::Finish) || next == Phase::Finishing,
            "a finish always leaves the session finishing"
        );
        action
    }
}

static SESSION: Mutex<Session> = Mutex::new(Session { phase: Phase::Idle });
static CLOCK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The process-wide session, fed by the FFI.
pub fn input(i: Input) -> Action {
    let mut s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let action = s.input(i, now_ms());
    crate::log(&format!(
        "[Session] {:?} -> {:?} (now {:?})",
        i,
        action,
        s.phase()
    ));
    action
}

pub fn phase() -> Phase {
    SESSION.lock().unwrap_or_else(|e| e.into_inner()).phase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Kind = Kind::Dictation;
    const C: Kind = Kind::Command;

    fn started(kind: Kind, mode: Mode) -> Session {
        let mut s = Session::default();
        assert_eq!(
            s.input(Input::Press(kind, mode), 1_000),
            Action::Start(kind)
        );
        s
    }

    // ── push-to-talk ────────────────────────────────────────────────

    #[test]
    fn hold_press_starts_and_release_finishes() {
        let mut s = started(D, Mode::Hold);
        assert_eq!(s.input(Input::Release(D), 1_000 + 2_000), Action::Finish);
        assert_eq!(s.phase(), Phase::Finishing);
        assert_eq!(s.input(Input::Delivered, 5_000), Action::None);
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn a_tap_too_short_to_speak_is_thrown_away() {
        let mut s = started(D, Mode::Hold);
        assert_eq!(s.input(Input::Release(D), 1_000 + 100), Action::Discard);
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn the_min_hold_boundary_finishes() {
        let mut s = started(D, Mode::Hold);
        assert_eq!(
            s.input(Input::Release(D), 1_000 + MIN_HOLD_MS),
            Action::Finish
        );
    }

    #[test]
    fn holding_ignores_repeated_presses_and_the_other_shortcut() {
        let mut s = started(D, Mode::Hold);
        assert_eq!(s.input(Input::Press(D, Mode::Hold), 1_500), Action::None);
        assert_eq!(s.input(Input::Press(C, Mode::Hold), 1_600), Action::None);
        assert_eq!(s.input(Input::Release(C), 1_700), Action::None);
        assert!(matches!(
            s.phase(),
            Phase::Recording {
                kind: Kind::Dictation,
                ..
            }
        ));
        assert_eq!(s.input(Input::Release(D), 3_000), Action::Finish);
    }

    #[test]
    fn a_command_hold_is_ended_by_its_own_release() {
        let mut s = started(C, Mode::Hold);
        assert_eq!(s.input(Input::Release(D), 3_000), Action::None);
        assert_eq!(s.input(Input::Release(C), 3_100), Action::Finish);
    }

    // ── toggle ──────────────────────────────────────────────────────

    #[test]
    fn toggle_press_starts_and_the_next_press_finishes() {
        let mut s = started(D, Mode::Toggle);
        assert_eq!(s.input(Input::Release(D), 1_100), Action::None);
        assert_eq!(
            s.input(Input::Press(D, Mode::Toggle), 4_000),
            Action::Finish
        );
    }

    #[test]
    fn a_toggle_press_bouncing_right_after_the_start_is_ignored() {
        let mut s = started(D, Mode::Toggle);
        assert_eq!(
            s.input(Input::Press(D, Mode::Toggle), 1_000 + 120),
            Action::None
        );
        assert!(matches!(s.phase(), Phase::Recording { .. }));
        assert_eq!(
            s.input(Input::Press(D, Mode::Toggle), 1_000 + TOGGLE_BOUNCE_MS),
            Action::Finish
        );
    }

    #[test]
    fn the_mode_is_fixed_when_the_session_starts() {
        // Settings switched to hold mid-recording: the toggle session still
        // ignores the release and ends on the next press.
        let mut s = started(D, Mode::Toggle);
        assert_eq!(s.input(Input::Release(D), 2_000), Action::None);
        assert_eq!(s.input(Input::Press(D, Mode::Hold), 3_000), Action::Finish);
    }

    // ── commands close together ─────────────────────────────────────

    #[test]
    fn a_press_while_the_last_dictation_is_transcribing_says_busy() {
        let mut s = started(D, Mode::Hold);
        s.input(Input::Release(D), 3_000);
        assert_eq!(s.input(Input::Press(D, Mode::Hold), 3_200), Action::Busy);
        assert_eq!(s.input(Input::Press(C, Mode::Toggle), 3_300), Action::Busy);
        // ...and its release changes nothing: no reset to idle mid-transcription.
        assert_eq!(s.input(Input::Release(D), 3_400), Action::None);
        assert_eq!(s.phase(), Phase::Finishing);
        s.input(Input::Delivered, 5_000);
        assert_eq!(
            s.input(Input::Press(D, Mode::Hold), 5_100),
            Action::Start(D)
        );
    }

    #[test]
    fn only_one_finish_per_session() {
        let mut s = started(D, Mode::Toggle);
        assert_eq!(s.input(Input::Stop, 2_000), Action::Finish);
        assert_eq!(s.input(Input::Stop, 2_010), Action::None);
        assert_eq!(s.input(Input::Press(D, Mode::Toggle), 2_500), Action::Busy);
    }

    #[test]
    fn the_pill_stop_finishes_a_hold_too() {
        let mut s = started(D, Mode::Hold);
        assert_eq!(s.input(Input::Stop, 2_000), Action::Finish);
    }

    // ── odd orders ──────────────────────────────────────────────────

    #[test]
    fn stray_inputs_while_idle_do_nothing() {
        let mut s = Session::default();
        for i in [
            Input::Release(D),
            Input::Stop,
            Input::Delivered,
            Input::StartFailed,
            Input::Cancel,
        ] {
            assert_eq!(s.input(i, 1_000), Action::None);
            assert_eq!(s.phase(), Phase::Idle);
        }
    }

    #[test]
    fn a_failed_start_goes_back_to_idle() {
        let mut s = started(D, Mode::Toggle);
        assert_eq!(s.input(Input::StartFailed, 1_010), Action::None);
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn cancel_ends_any_phase() {
        let mut s = started(D, Mode::Hold);
        s.input(Input::Cancel, 1_500);
        assert_eq!(s.phase(), Phase::Idle);
        let mut s = started(D, Mode::Hold);
        s.input(Input::Release(D), 3_000);
        s.input(Input::Cancel, 3_500);
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn delivered_before_finishing_is_ignored() {
        let mut s = started(D, Mode::Toggle);
        assert_eq!(s.input(Input::Delivered, 1_500), Action::None);
        assert!(matches!(s.phase(), Phase::Recording { .. }));
    }

    #[test]
    fn a_clock_that_steps_back_cannot_underflow() {
        let mut s = started(D, Mode::Hold);
        assert_eq!(s.input(Input::Release(D), 500), Action::Discard);
    }
}
