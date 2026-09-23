//! Calendar context for a meeting: which appointment was this, and who was in it.
//!
//! A recap says what was decided. It cannot say who committed to what,
//! because Dimmy hears two audio channels (you, and everybody else) and
//! no names. Speaker diarisation was measured and rejected — see
//! `core/src/bin/diarize_probe.rs`, where a local embedding pipeline gave
//! statistically identical distance distributions for a one-voice and a
//! two-voice file. The roster has to come from somewhere else, and the
//! only place that actually knows it is the calendar invite.
//!
//! ## Why this borrows an OAuth instead of building one
//!
//! Reading a calendar means OAuth: an app registration, PKCE, a loopback
//! listener, refresh-token lifecycle, and for Google a sensitive-scope
//! review with a video demo. [`crate::confluence`] (line 18) records the
//! house position on exactly that cost, and it says no.
//!
//! So this module does not hold a credential at all. It asks the user's
//! OWN `claude` CLI, which already carries the claude.ai connectors, to
//! talk to the calendar and hand back JSON. Measured on 2026-09-23
//! against a real Outlook tenant: `outlook_calendar_search` lists the
//! day's events, `read_resource` on an event fills `attendees[].name`,
//! and the round trip costs 19-29 s.
//!
//! The consequences are real and are the reason every entry point here
//! degrades to "no context" rather than to an error:
//!
//! - It works only for users on the Claude subscription backend.
//! - It works only if they authorised the connector IN THE CLI's own
//!   store. Authorising on claude.ai is not enough (verified: the CLI
//!   reported zero connector tools until `/mcp` was run). Dimmy can
//!   neither start nor repair that authorisation, only detect it.
//! - The grant may be read-only. The tested tenant returned
//!   `Calendars.Read` + `Mail.Read` and refused `outlook_create_draft`,
//!   which is why nothing here writes anything anywhere.
//!
//! ## What is trusted to the model, and what is not
//!
//! The model is used as a TRANSPORT, not as a judge. It fetches the
//! day's events and returns them verbatim as JSON. Every decision made
//! on top — which event overlaps the recording, by how much, which one
//! wins — is [`rank_candidates`], which is pure arithmetic in Rust with
//! its own tests. A roster ends up in a document the user forwards to
//! other people; "the model picked the right meeting" is not a standard
//! that survives contact with that.
//!
//! Nothing here is ever fed to telemetry. Attendee names and addresses
//! are personal data; see the privacy rules in CLAUDE.md.

use crate::error::TranscribeError;
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long the CLI round trip may take before we give up and report no
/// context. Measured at 19-29 s over three runs against a live tenant;
/// 120 s leaves room for a cold connector handshake without ever
/// becoming a hang the user has to watch.
pub const FETCH_TIMEOUT_SECS: u64 = 120;

/// One invited person. `name` is best-effort: `outlook_calendar_search`
/// returns addresses only, and the display name needs a second call per
/// event, so a fetch that could not afford it leaves this empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
}

impl Attendee {
    /// What goes in front of the recap model: the name when we have one,
    /// the local part of the address when we do not. The full address is
    /// never put in a prompt — it adds nothing to a summary and it is the
    /// most sensitive field we hold.
    pub fn display(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        let email = self.email.trim();
        match email.split('@').next() {
            Some(local) if !local.is_empty() => local.to_string(),
            _ => String::new(),
        }
    }
}

/// A calendar entry as the connector reported it. Times are unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub start_unix: i64,
    pub end_unix: i64,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    #[serde(default)]
    pub organizer: String,
}

/// An event ranked against a recording window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub event: CalendarEvent,
    /// Minutes the event and the recording were both running.
    pub overlap_mins: i64,
    /// Overlap as a fraction of the SHORTER of the two spans, 0..=100.
    /// A five-minute stand-up fully inside a two-hour recording scores
    /// 100 here and four minutes on `overlap_mins`; ranking on minutes
    /// alone would bury it under any long event that merely grazes.
    pub coverage_pct: i64,
    /// Why this event is being offered, so the host can word the row
    /// honestly instead of pretending one number means three things:
    /// `overlap` (the recording has ended and they share a span),
    /// `current` (still recording, and this invite contains right now),
    /// `nearby` (still recording, and this invite is about to start).
    pub match_kind: String,
}

/// Whether the borrowed connector is usable right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorStatus {
    pub available: bool,
    /// Stable machine-readable cause, for the host to map to its own copy:
    /// `ok`, `cli_missing`, `not_logged_in`, `connector_not_authorised`,
    /// `probe_failed`.
    pub reason: String,
}

impl ConnectorStatus {
    fn unavailable(reason: &str) -> Self {
        Self {
            available: false,
            reason: reason.to_string(),
        }
    }
}

/// Minutes during which both spans were running. Zero when they do not
/// touch. Both spans are half-open: an event ending exactly when the
/// recording starts overlaps by nothing, which is the answer a user
/// expects for back-to-back calls.
pub fn overlap_minutes(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> i64 {
    assert!(a_end >= a_start, "span A ends before it starts");
    assert!(b_end >= b_start, "span B ends before it starts");
    let start = a_start.max(b_start);
    let end = a_end.min(b_end);
    if end <= start {
        return 0;
    }
    let mins = (end - start) / 60;
    assert!(mins >= 0, "overlap must not be negative");
    mins
}

/// Rank the day's events against a recording window, best first.
///
/// Events that do not overlap at all are dropped: offering the user a
/// meeting that had already finished is noise, and the picker always
/// carries an explicit "none of these" anyway.
///
/// Ties break on the shorter event, because a specific 30-minute invite
/// describes a recording better than the all-day block it sits inside.
pub fn rank_candidates(rec_start: i64, rec_end: i64, events: &[CalendarEvent]) -> Vec<Candidate> {
    assert!(rec_end >= rec_start, "recording ends before it starts");

    let rec_mins = (rec_end - rec_start) / 60;
    let mut out: Vec<Candidate> = events
        .iter()
        .filter(|e| e.end_unix >= e.start_unix)
        .filter_map(|e| {
            let overlap = overlap_minutes(rec_start, rec_end, e.start_unix, e.end_unix);
            if overlap <= 0 {
                return None;
            }
            let ev_mins = (e.end_unix - e.start_unix) / 60;
            let shorter = rec_mins.min(ev_mins).max(1);
            let coverage = ((overlap * 100) / shorter).min(100);
            Some(Candidate {
                event: e.clone(),
                overlap_mins: overlap,
                coverage_pct: coverage,
                match_kind: "overlap".to_string(),
            })
        })
        .collect();

    out.sort_by(|a, b| {
        b.coverage_pct
            .cmp(&a.coverage_pct)
            .then(b.overlap_mins.cmp(&a.overlap_mins))
            .then(
                (a.event.end_unix - a.event.start_unix)
                    .cmp(&(b.event.end_unix - b.event.start_unix)),
            )
            .then(a.event.start_unix.cmp(&b.event.start_unix))
    });

    for c in &out {
        assert!(c.overlap_mins > 0, "a ranked candidate must overlap");
        assert!(
            (0..=100).contains(&c.coverage_pct),
            "coverage must be a percentage"
        );
    }
    out
}

/// The local calendar day a unix timestamp falls on, plus the host's
/// offset from UTC in minutes at that moment.
///
/// Both halves matter and neither can be skipped. The day is what we ask
/// the connector for, and it has to be the day the user's calendar shows,
/// not the UTC one — a 01:30 call in Rome is the previous day in UTC, and
/// asking for the wrong day returns an empty list rather than an error,
/// which is the worst possible failure because it looks like "you had
/// nothing on". The offset goes in the prompt so the model resolves
/// boundaries the same way.
pub fn local_day_and_offset(unix_secs: i64) -> (String, i32) {
    use chrono::{Local, Offset, TimeZone};
    match Local.timestamp_opt(unix_secs, 0) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            let day = dt.format("%Y-%m-%d").to_string();
            let offset_mins = dt.offset().fix().local_minus_utc() / 60;
            (day, offset_mins)
        }
        // A timestamp inside a DST spring-forward gap has no local
        // reading. Falling back to UTC keeps the fetch going with a day
        // that is wrong by at most one hour's worth of edge case.
        chrono::LocalResult::None => {
            let dt = chrono::DateTime::from_timestamp(unix_secs, 0).unwrap_or_default();
            (dt.format("%Y-%m-%d").to_string(), 0)
        }
    }
}

/// How far ahead an invite may start and still be offered to a
/// recording already in progress. People join a call before the invite
/// begins and start recording then; fifteen minutes is the widest gap
/// where "you are probably about to be in this" is still true rather
/// than a guess about the rest of the afternoon.
pub const NEARBY_WINDOW_SECS: i64 = 15 * 60;

/// Rank events for a recording that has NOT finished yet.
///
/// Overlap is undefined here: the recording has no end, so the shared
/// span is one instant and every ranking built on it collapses. The
/// question a user is actually asking mid-call is different — "which
/// meeting am I in right now?" — so this answers that one: invites that
/// contain this moment, shortest first, then invites about to start.
pub fn rank_in_progress(now: i64, events: &[CalendarEvent]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = events
        .iter()
        .filter(|e| e.end_unix >= e.start_unix)
        .filter_map(|e| {
            let duration_mins = (e.end_unix - e.start_unix) / 60;
            if e.start_unix <= now && now < e.end_unix {
                Some(Candidate {
                    event: e.clone(),
                    overlap_mins: duration_mins,
                    coverage_pct: 100,
                    match_kind: "current".to_string(),
                })
            } else if e.start_unix > now && e.start_unix - now <= NEARBY_WINDOW_SECS {
                Some(Candidate {
                    event: e.clone(),
                    overlap_mins: duration_mins,
                    coverage_pct: 0,
                    match_kind: "nearby".to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    out.sort_by(|a, b| {
        // Happening now beats about to happen, always.
        b.coverage_pct.cmp(&a.coverage_pct).then_with(|| {
            match (a.match_kind.as_str(), b.match_kind.as_str()) {
                ("current", "current") => a.overlap_mins.cmp(&b.overlap_mins),
                _ => a.event.start_unix.cmp(&b.event.start_unix),
            }
        })
    });
    out
}

/// Rank whatever we can, given what we know about the recording.
///
/// `ended_at` is `None` while the meeting is still being recorded. The
/// two cases answer different questions and are deliberately not merged
/// into one formula with a fudge factor.
pub fn rank_for_meeting(
    started_at: i64,
    ended_at: Option<i64>,
    now: i64,
    events: &[CalendarEvent],
) -> Vec<Candidate> {
    match ended_at {
        Some(end) if end > started_at => rank_candidates(started_at, end, events),
        _ => rank_in_progress(now, events),
    }
}

/// The roster line handed to the recap prompt, or an empty string when
/// there is nobody to name.
///
/// Deliberately names people and does NOT claim who spoke. The calendar
/// knows who was invited; half of them do not join and somebody unlisted
/// often does. Saying "these people were invited" is true and is enough
/// for the model to attribute an action item when a name is spoken out
/// loud, which is how the recap already works.
pub fn roster_for_prompt(event: &CalendarEvent) -> String {
    let names: Vec<String> = event
        .attendees
        .iter()
        .map(|a| a.display())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return String::new();
    }
    format!(
        "Invited to this meeting (from the calendar invite, not from the audio \
         — do not assume all of them spoke, and do not invent attributions): {}.",
        names.join(", ")
    )
}

/// The prompt that turns the CLI into a calendar transport.
///
/// Strict JSON and nothing else, because the answer is parsed and any
/// prose around it is a parse failure rather than a partial success.
fn fetch_prompt(day_iso: &str, tz_note: &str) -> String {
    format!(
        "Use the Microsoft 365 (Outlook) connector, or the Google Calendar connector \
if Microsoft 365 is unavailable, to read my calendar for {day_iso}{tz_note}.\n\n\
For every event that day, also resolve the attendees' display names (the search \
tool returns addresses only; reading the event itself fills attendees[].name).\n\n\
Answer with ONE JSON object and nothing else, no prose, no code fence:\n\
{{\"ok\":true,\"events\":[{{\"id\":\"<opaque id>\",\"title\":\"<subject>\",\
\"start_unix\":<unix seconds>,\"end_unix\":<unix seconds>,\
\"attendees\":[{{\"name\":\"<display name or empty>\",\"email\":\"<address>\"}}],\
\"organizer\":\"<display name or empty>\"}}]}}\n\n\
If no connector is authorised, or it returns nothing, answer exactly:\n\
{{\"ok\":false,\"error\":\"<short reason>\"}}"
    )
}

#[derive(Deserialize)]
struct FetchEnvelope {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    events: Vec<CalendarEvent>,
    #[serde(default)]
    error: String,
}

/// Pull the JSON object out of whatever the CLI printed.
///
/// The prompt asks for bare JSON, and in testing that is what comes
/// back, but a model that decides to wrap it in a code fence or add a
/// sentence must not cost the user the feature. Anything with no
/// balanced object in it is a failure, not a guess.
pub fn extract_json(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a CLI answer into events, rejecting anything shaped wrong.
pub fn parse_events(raw: &str) -> Result<Vec<CalendarEvent>, TranscribeError> {
    let json = extract_json(raw)
        .ok_or_else(|| TranscribeError::Network("calendar: no JSON in CLI answer".into()))?;
    let env: FetchEnvelope = serde_json::from_str(json)
        .map_err(|e| TranscribeError::Network(format!("calendar: bad JSON ({e})")))?;
    if !env.ok {
        let why = if env.error.is_empty() {
            "connector reported a failure".to_string()
        } else {
            // Truncated for the same reason every provider error is:
            // an upstream message is untrusted and may carry a token.
            env.error.chars().take(200).collect::<String>()
        };
        return Err(TranscribeError::Network(format!("calendar: {why}")));
    }
    // A zero-length day is a legitimate answer, an event with no span is not.
    let events: Vec<CalendarEvent> = env
        .events
        .into_iter()
        .filter(|e| e.end_unix >= e.start_unix && e.start_unix > 0)
        .collect();
    Ok(events)
}

#[cfg(target_os = "windows")]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: Dimmy is a GUI app with no parent console, so
    // without this every fetch flashes a black cmd window.
    cmd.creation_flags(0x08000000);
}

#[cfg(not(target_os = "windows"))]
fn hide_console(_cmd: &mut Command) {}

/// Run one `claude --print` turn with the connector tools unlocked.
///
/// `--permission-mode bypassPermissions` is required and is not a
/// shortcut: in print mode every MCP tool is refused by default, and a
/// non-interactive session has nobody to answer the prompt. Verified on
/// 2026-09-23 — without it the CLI answers "the call was blocked because
/// permission is missing" and returns no data.
fn run_cli(prompt: &str, timeout: Duration) -> Result<String, TranscribeError> {
    let binary = crate::claude_code::detect_binary()
        .ok_or_else(|| TranscribeError::Network("calendar: claude CLI not installed".into()))?;
    if !crate::claude_code::has_credentials() {
        return Err(TranscribeError::Network(
            "calendar: claude CLI not logged in".into(),
        ));
    }

    let mut cmd = Command::new(&binary);
    cmd.arg("--print");
    cmd.arg("--output-format");
    cmd.arg("text");
    cmd.arg("--permission-mode");
    cmd.arg("bypassPermissions");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    hide_console(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| TranscribeError::Network(format!("calendar: spawn failed ({e})")))?;

    // Drop the handle after writing so the CLI sees EOF and starts
    // generating — same contract as `claude_code::run_blocking`.
    match child.stdin.take() {
        Some(mut stdin) => {
            use std::io::Write;
            if let Err(e) = stdin.write_all(prompt.as_bytes()) {
                let _ = child.kill();
                return Err(TranscribeError::Network(format!(
                    "calendar: stdin write ({e})"
                )));
            }
            drop(stdin);
        }
        None => {
            let _ = child.kill();
            return Err(TranscribeError::Network("calendar: no stdin handle".into()));
        }
    }

    // std::process::Child has no timed wait, so poll at the same 100 ms
    // granularity claude_code.rs settled on. A wedged CLI must never
    // become a thread Dimmy waits on for ever.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(TranscribeError::Network("calendar: CLI timed out".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(TranscribeError::Network(format!("calendar: wait ({e})")));
            }
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| TranscribeError::Network(format!("calendar: output ({e})")))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Is the borrowed connector usable? One cheap turn that asks only for
/// tool NAMES, so it costs no calendar read and leaks nothing.
pub fn probe_connector() -> ConnectorStatus {
    if crate::claude_code::detect_binary().is_none() {
        return ConnectorStatus::unavailable("cli_missing");
    }
    if !crate::claude_code::has_credentials() {
        return ConnectorStatus::unavailable("not_logged_in");
    }
    let prompt = "List the exact names of the MCP tools you have available whose name \
contains \"calendar\" (case-insensitive). Answer with one name per line and nothing \
else. If you have none, answer exactly: NONE";
    let raw = match run_cli(prompt, Duration::from_secs(60)) {
        Ok(r) => r,
        Err(_) => return ConnectorStatus::unavailable("probe_failed"),
    };
    let lower = raw.to_ascii_lowercase();
    // A connector that is configured but unauthorised reports no tools at
    // all — that is the state the CLI was in before `/mcp` was run, and
    // it is indistinguishable from "not installed" except by this answer.
    if lower.contains("calendar_search") || lower.contains("calendar") && !lower.contains("none") {
        ConnectorStatus {
            available: true,
            reason: "ok".to_string(),
        }
    } else {
        ConnectorStatus::unavailable("connector_not_authorised")
    }
}

/// Fetch the events of `day_iso` (YYYY-MM-DD) as the connector sees them.
///
/// `tz_offset_mins` is the host's current offset from UTC, passed through
/// so the model resolves "that day" the way the user's calendar shows it
/// rather than the way the model's own clock would.
pub fn fetch_day(
    day_iso: &str,
    tz_offset_mins: i32,
) -> Result<Vec<CalendarEvent>, TranscribeError> {
    assert!(!day_iso.is_empty(), "day must not be empty");
    let tz_note = if tz_offset_mins == 0 {
        String::new()
    } else {
        let sign = if tz_offset_mins > 0 { '+' } else { '-' };
        let abs = tz_offset_mins.abs();
        format!(
            " (my local time is UTC{sign}{:02}:{:02})",
            abs / 60,
            abs % 60
        )
    };
    let raw = run_cli(
        &fetch_prompt(day_iso, &tz_note),
        Duration::from_secs(FETCH_TIMEOUT_SECS),
    )?;
    parse_events(&raw)
}

/// What the user decided for one meeting. Lives next to the audio as
/// `calendar.json`, like every other per-meeting artifact, so it travels
/// with the recording and survives the window being closed.
///
/// `dismissed` is not the same as "no event": it records that the user
/// was asked and said none of these. Without it the picker would come
/// back on every reopen, which is the same nagging loop the auto-record
/// nudge was fixed for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    #[serde(default)]
    pub event: Option<CalendarEvent>,
    #[serde(default)]
    pub dismissed: bool,
}

impl Assignment {
    /// Has the user answered for this meeting, either way?
    pub fn answered(&self) -> bool {
        self.dismissed || self.event.is_some()
    }
}

fn assignment_path(meeting_dir: &std::path::Path) -> std::path::PathBuf {
    meeting_dir.join("calendar.json")
}

/// Record the user's choice. `None` means "none of these".
pub fn save_assignment(
    meeting_dir: &std::path::Path,
    event: Option<&CalendarEvent>,
) -> Result<(), TranscribeError> {
    assert!(
        !meeting_dir.as_os_str().is_empty(),
        "meeting dir must not be empty"
    );
    let a = Assignment {
        event: event.cloned(),
        dismissed: event.is_none(),
    };
    let json = serde_json::to_string_pretty(&a)
        .map_err(|e| TranscribeError::Network(format!("calendar: serialise ({e})")))?;
    std::fs::write(assignment_path(meeting_dir), json)
        .map_err(|e| TranscribeError::Network(format!("calendar: write ({e})")))?;
    Ok(())
}

/// Read back the choice. `None` when the user has not been asked yet.
///
/// A corrupt file reads as "not asked" rather than as an error: the cost
/// of asking again is one row in a window, and the cost of failing the
/// meeting over a side artifact is the whole recap.
pub fn load_assignment(meeting_dir: &std::path::Path) -> Option<Assignment> {
    let raw = std::fs::read_to_string(assignment_path(meeting_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Open an interactive `claude` session so the user can run `/mcp` and
/// authorise the connector.
///
/// Dimmy cannot do this itself: the OAuth handshake needs a terminal the
/// user can answer, and the CLI keeps its own credential store that a
/// claude.ai authorisation does not reach (verified 2026-09-23 — the CLI
/// reported zero connector tools until `/mcp` had been run locally).
pub fn spawn_connector_setup() -> Result<(), TranscribeError> {
    let binary = crate::claude_code::detect_binary()
        .ok_or_else(|| TranscribeError::Network("calendar: claude CLI not installed".into()))?;

    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/c");
        cmd.arg("start");
        cmd.arg(""); // empty window title
        cmd.arg(&binary);
        cmd.arg("/mcp");
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| TranscribeError::Network(format!("calendar: spawn ({e})")))?;
    }
    #[cfg(target_os = "macos")]
    {
        let command = format!("{} /mcp", binary.display());
        crate::run_in_new_terminal_window(&command, "claude-mcp")
            .map_err(|e| TranscribeError::Network(format!("calendar: spawn ({e})")))?;
    }
    #[cfg(target_os = "linux")]
    {
        // Same reasoning as claude_code::spawn_login: too many terminal
        // emulators to dispatch to one, so the user re-runs it themselves.
        let mut cmd = Command::new(&binary);
        cmd.arg("/mcp");
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| TranscribeError::Network(format!("calendar: spawn ({e})")))?;
    }

    crate::log("[Calendar] connector setup session spawned");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, start: i64, end: i64) -> CalendarEvent {
        CalendarEvent {
            id: id.to_string(),
            title: format!("event {id}"),
            start_unix: start,
            end_unix: end,
            attendees: vec![],
            organizer: String::new(),
        }
    }

    const H: i64 = 3600;

    #[test]
    fn back_to_back_calls_do_not_overlap() {
        // An event that ends exactly when the recording begins is the
        // previous meeting, not this one.
        assert_eq!(overlap_minutes(10 * H, 11 * H, 9 * H, 10 * H), 0);
        assert_eq!(overlap_minutes(10 * H, 11 * H, 11 * H, 12 * H), 0);
    }

    #[test]
    fn overlap_is_the_shared_span() {
        // Recording 14:02-14:34 against an invite for 14:00-14:30: the
        // measured case from the 2026-09-23 tenant test, which the CLI
        // answered with 28 minutes.
        let rec_start = 14 * H + 2 * 60;
        let rec_end = 14 * H + 34 * 60;
        assert_eq!(
            overlap_minutes(rec_start, rec_end, 14 * H, 14 * H + 30 * 60),
            28
        );
        assert_eq!(
            overlap_minutes(rec_start, rec_end, 14 * H + 30 * 60, 15 * H),
            4
        );
    }

    #[test]
    fn the_real_meeting_outranks_the_one_it_grazes() {
        let rec_start = 14 * H + 2 * 60;
        let rec_end = 14 * H + 34 * 60;
        let events = vec![
            ev("next", 14 * H + 30 * 60, 15 * H),
            ev("this", 14 * H, 14 * H + 30 * 60),
        ];
        let ranked = rank_candidates(rec_start, rec_end, &events);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].event.id, "this");
        assert_eq!(ranked[0].overlap_mins, 28);
    }

    #[test]
    fn a_standup_inside_a_long_recording_beats_the_all_day_block() {
        // Ranking on raw minutes would hand a 15-minute stand-up to the
        // all-day block it sits inside, which is never the answer.
        let rec_start = 9 * H;
        let rec_end = 9 * H + 15 * 60;
        let events = vec![
            ev("allday", 0, 24 * H),
            ev("standup", 9 * H, 9 * H + 15 * 60),
        ];
        let ranked = rank_candidates(rec_start, rec_end, &events);
        assert_eq!(ranked[0].event.id, "standup");
        assert_eq!(ranked[0].coverage_pct, 100);
    }

    #[test]
    fn events_that_do_not_touch_the_recording_are_dropped() {
        let events = vec![ev("morning", 9 * H, 10 * H), ev("evening", 18 * H, 19 * H)];
        let ranked = rank_candidates(14 * H, 15 * H, &events);
        assert!(ranked.is_empty());
    }

    #[test]
    fn a_recording_with_no_events_ranks_nothing() {
        assert!(rank_candidates(14 * H, 15 * H, &[]).is_empty());
    }

    #[test]
    fn attendee_display_prefers_the_name_then_the_local_part() {
        let named = Attendee {
            name: "Anna Rossi".into(),
            email: "a.rossi@example.com".into(),
        };
        assert_eq!(named.display(), "Anna Rossi");
        let bare = Attendee {
            name: String::new(),
            email: "m.bianchi@example.com".into(),
        };
        // The full address never reaches a prompt.
        assert_eq!(bare.display(), "m.bianchi");
        let empty = Attendee {
            name: String::new(),
            email: String::new(),
        };
        assert_eq!(empty.display(), "");
    }

    #[test]
    fn the_roster_never_claims_who_spoke() {
        let mut e = ev("x", 0, H);
        e.attendees = vec![
            Attendee {
                name: "Anna Rossi".into(),
                email: "a@example.com".into(),
            },
            Attendee {
                name: String::new(),
                email: "m.bianchi@example.com".into(),
            },
        ];
        let line = roster_for_prompt(&e);
        assert!(line.contains("Anna Rossi"));
        assert!(line.contains("m.bianchi"));
        // The invite is evidence of invitation, never of attendance.
        assert!(line.contains("do not assume all of them spoke"));
        assert!(!line.contains("a@example.com"));
    }

    #[test]
    fn an_event_with_nobody_in_it_produces_no_roster_line() {
        assert_eq!(roster_for_prompt(&ev("x", 0, H)), "");
    }

    #[test]
    fn json_is_recovered_from_a_fenced_or_chatty_answer() {
        let bare = r#"{"ok":true,"events":[]}"#;
        assert_eq!(extract_json(bare), Some(bare));
        let fenced = "Ecco il risultato:\n```json\n{\"ok\":true,\"events\":[]}\n```\n";
        assert_eq!(extract_json(fenced), Some(r#"{"ok":true,"events":[]}"#));
        assert_eq!(extract_json("nessun oggetto qui"), None);
    }

    #[test]
    fn a_brace_inside_a_title_does_not_end_the_object() {
        let raw = r#"{"ok":true,"events":[{"id":"a","title":"sprint }{ review","start_unix":100,"end_unix":200,"attendees":[],"organizer":""}]}"#;
        let events = parse_events(raw).expect("parses");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "sprint }{ review");
    }

    #[test]
    fn a_refusal_is_an_error_not_an_empty_day() {
        // The difference matters: an empty day means "you had nothing on",
        // a refusal means "we could not look", and the host says something
        // different for each.
        let raw = r#"{"ok":false,"error":"connector not authorised"}"#;
        let err = parse_events(raw).unwrap_err().to_string();
        assert!(err.contains("not authorised"), "got {err}");
    }

    #[test]
    fn an_empty_day_is_a_success() {
        let events = parse_events(r#"{"ok":true,"events":[]}"#).expect("parses");
        assert!(events.is_empty());
    }

    #[test]
    fn events_with_an_impossible_span_are_discarded() {
        let raw = r#"{"ok":true,"events":[
            {"id":"good","title":"t","start_unix":100,"end_unix":200,"attendees":[],"organizer":""},
            {"id":"backwards","title":"t","start_unix":300,"end_unix":100,"attendees":[],"organizer":""},
            {"id":"nostart","title":"t","start_unix":0,"end_unix":100,"attendees":[],"organizer":""}
        ]}"#;
        let events = parse_events(raw).expect("parses");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "good");
    }

    #[test]
    fn the_day_asked_for_is_the_users_day_not_the_utc_one() {
        // 2026-09-23 01:30 in Rome (UTC+2) is 2026-09-22 23:30 UTC. Asking
        // the connector for the UTC day would come back empty, which reads
        // as "you had nothing on" rather than as a mistake.
        let (day, offset) = local_day_and_offset(1790120_000);
        assert_eq!(day.len(), 10, "YYYY-MM-DD");
        assert!(day.starts_with("20"), "got {day}");
        // The offset must be a whole number of minutes within a real range.
        assert!(
            (-14 * 60..=14 * 60).contains(&offset),
            "impossible offset {offset}"
        );
    }

    #[test]
    fn a_recording_in_progress_offers_the_invite_it_is_inside() {
        // The bug this exists for: mid-call there is no end time, so the
        // shared span is one instant and the overlap ranking returns
        // nothing at all. The user would see an empty picker during the
        // exact moment the picker is for.
        let now = 14 * H + 2 * 60;
        let events = vec![
            ev("earlier", 9 * H, 10 * H),
            ev("now", 14 * H, 14 * H + 30 * 60),
            ev("soon", 14 * H + 10 * 60, 14 * H + 40 * 60),
            ev("tonight", 20 * H, 21 * H),
        ];
        let ranked = rank_in_progress(now, &events);
        assert_eq!(ranked.len(), 2, "only current + nearby");
        assert_eq!(ranked[0].event.id, "now");
        assert_eq!(ranked[0].match_kind, "current");
        assert_eq!(ranked[1].event.id, "soon");
        assert_eq!(ranked[1].match_kind, "nearby");
    }

    #[test]
    fn the_shortest_invite_wins_when_two_contain_this_moment() {
        // The all-day block contains every moment; the half-hour invite
        // is the meeting you are actually in.
        let now = 14 * H + 2 * 60;
        let events = vec![
            ev("allday", 0, 24 * H),
            ev("real", 14 * H, 14 * H + 30 * 60),
        ];
        let ranked = rank_in_progress(now, &events);
        assert_eq!(ranked[0].event.id, "real");
    }

    #[test]
    fn the_dispatcher_picks_the_question_that_can_be_answered() {
        let started = 14 * H + 2 * 60;
        let events = vec![ev("this", 14 * H, 14 * H + 30 * 60)];
        // Finished: judged on the span they shared.
        let done = rank_for_meeting(started, Some(14 * H + 34 * 60), 0, &events);
        assert_eq!(done[0].match_kind, "overlap");
        assert_eq!(done[0].overlap_mins, 28);
        // Still recording: judged on where the clock is now.
        let live = rank_for_meeting(started, None, started, &events);
        assert_eq!(live[0].match_kind, "current");
        // A malformed end (before the start) must not be trusted as a span.
        let bad = rank_for_meeting(started, Some(started - 600), started, &events);
        assert_eq!(bad[0].match_kind, "current");
    }

    #[test]
    fn none_of_these_is_remembered_and_is_not_the_same_as_unanswered() {
        let dir = std::env::temp_dir().join(format!("dimmy-cal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        // Never asked.
        let _ = std::fs::remove_file(dir.join("calendar.json"));
        assert!(load_assignment(&dir).is_none());

        // Asked, answered "none of these": answered, but no event. Without
        // this distinction the picker reappears on every reopen.
        save_assignment(&dir, None).expect("saves");
        let a = load_assignment(&dir).expect("loads");
        assert!(a.answered());
        assert!(a.dismissed);
        assert!(a.event.is_none());

        // Asked, answered with an event.
        let mut e = ev("evt", 0, H);
        e.attendees = vec![Attendee {
            name: "Anna Rossi".into(),
            email: "a@example.com".into(),
        }];
        save_assignment(&dir, Some(&e)).expect("saves");
        let a = load_assignment(&dir).expect("loads");
        assert!(a.answered());
        assert!(!a.dismissed);
        assert_eq!(a.event.expect("event").id, "evt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_assignment_reads_as_unanswered_not_as_a_failure() {
        let dir = std::env::temp_dir().join(format!("dimmy-cal-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("calendar.json"), "{ not json").expect("writes");
        assert!(load_assignment(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prose_instead_of_json_is_a_failure_not_a_silent_empty() {
        let err = parse_events("Non riesco a leggere il calendario.")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no JSON"), "got {err}");
    }
}
