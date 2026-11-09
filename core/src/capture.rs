//! Who owns the shared capture buffers: one explicit state for dictation
//! and meeting.
//!
//! Before this the owner was spread over the `recording` mutex,
//! `STOP_DRAINING`, the `MEETING` slot, `MEETING_GENERATION` and the
//! meeting's own claim counter, and two holes were reachable:
//!
//! - A dictation started while a meeting was wrapping up was wiped by the
//!   meeting's final buffer clear (`still_ours()` compared meeting
//!   generations only).
//! - A meeting could start while a toggle dictation was recording; ending
//!   the dictation then sent `AudioCommand::Stop` and the meeting kept
//!   "recording" with no audio.
//!
//! The meeting worker never takes the lock (THE AUDIO RULE): it reads the
//! claim through [`meeting_owns`], one atomic load.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Idle,
    /// A dictation is recording, or stopping and has not taken its audio yet.
    Dictation {
        id: u64,
    },
    /// A meeting is recording.
    Meeting {
        id: u64,
    },
    /// The meeting was stopped; its worker is still writing the tail of the
    /// audio out of the buffers. Ends when the worker lets go of them,
    /// before the (possibly long) wait on the transcriber.
    MeetingDraining {
        id: u64,
    },
}

/// Why a claim was refused: what holds the capture now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Dictation,
    Meeting,
    MeetingDraining,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    ClaimDictation,
    ReleaseDictation,
    ClaimMeeting,
    MeetingStopping {
        id: u64,
    },
    /// The meeting worker is done with the buffers, or its start failed.
    MeetingReleased {
        id: u64,
    },
}

impl Owner {
    /// The one transition function. `next_id` is the id a successful claim
    /// takes.
    pub fn apply(self, input: Input, next_id: u64) -> Result<Owner, Busy> {
        assert!(next_id > 0, "capture ids start at 1");
        use Owner::*;
        let next = match (self, input) {
            (Idle, Input::ClaimDictation) => Dictation { id: next_id },
            (Dictation { .. }, Input::ClaimDictation) => return Err(Busy::Dictation),
            (Meeting { .. }, Input::ClaimDictation) => return Err(Busy::Meeting),
            (MeetingDraining { .. }, Input::ClaimDictation) => return Err(Busy::MeetingDraining),

            (Dictation { .. }, Input::ReleaseDictation) => Idle,
            (other, Input::ReleaseDictation) => other,

            (Idle, Input::ClaimMeeting) => Meeting { id: next_id },
            // A new meeting may start while the last one drains: the old
            // worker sees the claim move and stops touching the buffers.
            (MeetingDraining { .. }, Input::ClaimMeeting) => Meeting { id: next_id },
            (Meeting { .. }, Input::ClaimMeeting) => return Err(Busy::Meeting),
            (Dictation { .. }, Input::ClaimMeeting) => return Err(Busy::Dictation),

            (Meeting { id }, Input::MeetingStopping { id: s }) if id == s => MeetingDraining { id },
            (other, Input::MeetingStopping { .. }) => other,

            (Meeting { id } | MeetingDraining { id }, Input::MeetingReleased { id: r })
                if id == r =>
            {
                Idle
            }
            (other, Input::MeetingReleased { .. }) => other,
        };
        Ok(next)
    }

    /// The meeting claim allowed to touch the buffers, 0 for none.
    fn meeting_claim(self) -> u64 {
        match self {
            Owner::Meeting { id } | Owner::MeetingDraining { id } => id,
            _ => 0,
        }
    }
}

static OWNER: Mutex<Owner> = Mutex::new(Owner::Idle);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Mirror of `Owner::meeting_claim`, for the capture worker's lock-free check.
static MEETING_CLAIM: AtomicU64 = AtomicU64::new(0);

fn transition(input: Input) -> Result<(Owner, Owner), Busy> {
    let mut owner = OWNER.lock().unwrap_or_else(|p| p.into_inner());
    let before = *owner;
    let next_id = NEXT_ID.load(Ordering::SeqCst);
    let after = before.apply(input, next_id)?;
    if after != before && matches!(input, Input::ClaimDictation | Input::ClaimMeeting) {
        NEXT_ID.fetch_add(1, Ordering::SeqCst);
    }
    *owner = after;
    MEETING_CLAIM.store(after.meeting_claim(), Ordering::SeqCst);
    if after != before {
        crate::log(&format!("[Capture] {before:?} -> {after:?}"));
    }
    Ok((before, after))
}

pub fn owner() -> Owner {
    *OWNER.lock().unwrap_or_else(|p| p.into_inner())
}

/// Take the capture for a dictation. Returns its id.
pub fn claim_dictation() -> Result<u64, Busy> {
    match transition(Input::ClaimDictation)?.1 {
        Owner::Dictation { id } => Ok(id),
        other => unreachable!("claim_dictation left {other:?}"),
    }
}

/// The dictation has taken its audio out of the buffer. Idempotent, and a
/// no-op when no dictation holds the capture.
pub fn release_dictation() {
    let _ = transition(Input::ReleaseDictation);
}

/// Take the capture for a meeting. Returns its claim. Call BEFORE clearing
/// the buffers: from here on an earlier, draining worker leaves them alone.
pub fn claim_meeting() -> Result<u64, Busy> {
    match transition(Input::ClaimMeeting)?.1 {
        Owner::Meeting { id } => Ok(id),
        other => unreachable!("claim_meeting left {other:?}"),
    }
}

pub fn meeting_stopping(id: u64) {
    let _ = transition(Input::MeetingStopping { id });
}

/// The meeting with this claim no longer needs the buffers: hand them back
/// and free their memory (0.6-1.3 GB after a long meeting), unless a newer
/// meeting already took them. The clear happens under the owner lock, so a
/// dictation can never claim a buffer that is about to be wiped.
/// Returns whether the buffers were released.
pub fn meeting_released(id: u64, buffers: &[&Arc<Mutex<Vec<f32>>>]) -> bool {
    let mut owner = OWNER.lock().unwrap_or_else(|p| p.into_inner());
    let before = *owner;
    let after = before
        .apply(
            Input::MeetingReleased { id },
            NEXT_ID.load(Ordering::SeqCst),
        )
        .expect("a release is never refused");
    if after == before {
        return false;
    }
    for buf in buffers {
        if let Ok(mut b) = buf.lock() {
            b.clear();
            b.shrink_to_fit();
        }
    }
    *owner = after;
    MEETING_CLAIM.store(0, Ordering::SeqCst);
    crate::log(&format!(
        "[Capture] {before:?} -> {after:?} (buffers freed)"
    ));
    assert_eq!(
        after,
        Owner::Idle,
        "a released meeting leaves the capture idle"
    );
    true
}

/// Lock-free: whether the meeting with this claim may still touch the
/// buffers. The only call the capture worker makes.
pub fn meeting_owns(id: u64) -> bool {
    MEETING_CLAIM.load(Ordering::SeqCst) == id
}

/// Whether a meeting (recording or draining) holds the capture.
pub fn meeting_holds() -> bool {
    MEETING_CLAIM.load(Ordering::SeqCst) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use Owner::*;

    #[test]
    fn the_transition_table() {
        let d = Dictation { id: 3 };
        let m = Meeting { id: 4 };
        let md = MeetingDraining { id: 4 };
        let cases: &[(Owner, Input, Result<Owner, Busy>)] = &[
            (Idle, Input::ClaimDictation, Ok(Dictation { id: 9 })),
            (d, Input::ClaimDictation, Err(Busy::Dictation)),
            (m, Input::ClaimDictation, Err(Busy::Meeting)),
            (md, Input::ClaimDictation, Err(Busy::MeetingDraining)),
            (d, Input::ReleaseDictation, Ok(Idle)),
            (Idle, Input::ReleaseDictation, Ok(Idle)),
            (m, Input::ReleaseDictation, Ok(m)),
            (Idle, Input::ClaimMeeting, Ok(Meeting { id: 9 })),
            (md, Input::ClaimMeeting, Ok(Meeting { id: 9 })),
            (m, Input::ClaimMeeting, Err(Busy::Meeting)),
            // A meeting no longer starts under a running dictation.
            (d, Input::ClaimMeeting, Err(Busy::Dictation)),
            (m, Input::MeetingStopping { id: 4 }, Ok(md)),
            (m, Input::MeetingStopping { id: 5 }, Ok(m)),
            (md, Input::MeetingReleased { id: 4 }, Ok(Idle)),
            (m, Input::MeetingReleased { id: 4 }, Ok(Idle)),
            // A stale worker cannot release a newer meeting's claim.
            (
                Meeting { id: 7 },
                Input::MeetingReleased { id: 4 },
                Ok(Meeting { id: 7 }),
            ),
            (d, Input::MeetingReleased { id: 4 }, Ok(d)),
        ];
        for (from, input, want) in cases {
            assert_eq!(from.apply(*input, 9), *want, "{from:?} + {input:?}");
        }
    }

    #[test]
    fn only_meetings_carry_a_buffer_claim() {
        assert_eq!(Idle.meeting_claim(), 0);
        assert_eq!(Dictation { id: 2 }.meeting_claim(), 0);
        assert_eq!(Meeting { id: 2 }.meeting_claim(), 2);
        assert_eq!(MeetingDraining { id: 2 }.meeting_claim(), 2);
    }
}
