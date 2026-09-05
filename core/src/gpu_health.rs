//! GPU crash-recovery markers (Firefox/Chrome-style sentinel + sticky known-bad).
//!
//! Two markers cooperate:
//!
//! 1. **Sentinel** (`.gpu_init_in_progress`) — short-lived. Written immediately
//!    before a GPU-backed model load, deleted immediately after the call returns
//!    (Ok or Err — only a hard `abort()` leaves it on disk). Lets the NEXT
//!    process detect that the previous one died inside ggml-vulkan.
//!
//! 2. **Known-bad** (`.gpu_known_bad`) — sticky across sessions. Written when
//!    the sentinel fires AND the recovery succeeds. Stores a driver
//!    *fingerprint* so the next launch can compare: same fingerprint → keep
//!    CPU mode, different fingerprint → driver/ICD changed, retry GPU once.
//!
//! Without #2 the user paid one crash + relaunch on every cold start, since
//! the sentinel is by-design session-scoped. The sticky marker breaks that
//! loop while remaining self-healing when the user updates drivers or
//! Windows.
//!
//! Manual override: `clear_known_bad` removes the sticky marker (UI button).

use std::path::PathBuf;

fn marker_path() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join(".gpu_init_in_progress"))
}

fn known_bad_path() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join(".gpu_known_bad"))
}

fn first_strike_path() -> Option<PathBuf> {
    marker_path().map(|p| p.with_file_name(".gpu_first_strike"))
}

/// Record that the GPU aborted once, and say whether it has now done so
/// TWICE in a row on the same driver.
///
/// One crash is not evidence the GPU is bad. The sentinel cannot tell an
/// abort inside the driver from the process being killed for any other
/// reason, and on 2026-09-05 it armed the sticky marker twice in one night
/// for reasons that had nothing to do with the GPU: a test binary that
/// exited badly, and Windows killing the process under memory pressure.
/// Each time, every later run silently fell back to the CPU — whisper from
/// 2 s to 8 s, a recap from 40 s to 230 s, with nothing said anywhere.
///
/// A real driver fault repeats; a kill does not. So the first strike is
/// remembered and the GPU gets another go, and only a second consecutive
/// failure on the SAME driver makes it sticky. The cost is one extra crash
/// on a genuinely broken host, which is cheap against silently halving
/// performance on a working one.
pub fn record_strike(fingerprint: &str) -> bool {
    assert!(
        !fingerprint.is_empty(),
        "gpu_health::record_strike: fingerprint must be non-empty"
    );
    let Some(path) = first_strike_path() else {
        // Without somewhere to remember the first strike, fall back to the
        // old behaviour rather than never marking anything.
        return true;
    };
    let previous = std::fs::read_to_string(&path).ok();
    match previous.as_deref().map(str::trim) {
        Some(prev) if prev == fingerprint => {
            let _ = std::fs::remove_file(&path);
            true
        }
        _ => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, fingerprint);
            false
        }
    }
}

/// Forget the first strike. Called when GPU init SUCCEEDS, so that two
/// crashes months apart are not treated as consecutive.
pub fn clear_strike() {
    if let Some(path) = first_strike_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Snapshot of a recovered GPU-init crash. Written by `mark_known_bad`,
/// read by `read_known_bad`. Compared against current driver fingerprint
/// at startup to decide whether to retry GPU.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KnownBadRecord {
    /// Local-time stamp at the moment of recovery (informational only).
    pub timestamp: String,
    /// Free-form context: which call aborted (e.g. "whisper_load: …path…").
    pub context: String,
    /// Opaque driver fingerprint at crash time. Equality check only — the
    /// caller does NOT parse this. See `gpu_diag::compute_driver_fingerprint`.
    pub fingerprint: String,
}

/// Called before attempting GPU-backed model init. Writes a sentinel file.
/// Failures are swallowed — the sentinel is best-effort, not load-bearing.
pub fn mark_begin(context: &str) {
    assert!(
        !context.is_empty(),
        "gpu_health::mark_begin: context must be non-empty"
    );
    let Some(path) = marker_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let _ = std::fs::write(&path, format!("{}: {}\n", ts, context));
}

/// Called after the GPU init call returns (success or Rust-level error).
/// If the process aborts between `mark_begin` and `mark_end`, the sentinel
/// survives and `previous_crash_detected()` returns true on next boot.
pub fn mark_end() {
    if let Some(path) = marker_path() {
        let _ = std::fs::remove_file(path);
    }
    // Reaching here means the process SURVIVED GPU init, so any earlier
    // strike is not part of a run of consecutive failures. Without this, two
    // unrelated crashes months apart would look consecutive and make the
    // marker sticky on a machine whose GPU works fine.
    clear_strike();
}

/// Returns true if a previous process aborted during GPU init.
/// Callers should force the CPU backend for the current session.
pub fn previous_crash_detected() -> bool {
    marker_path().map(|p| p.exists()).unwrap_or(false)
}

/// Read the context string from the sentinel (for logging). Best-effort.
pub fn crash_context() -> Option<String> {
    marker_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
}

/// Explicitly clear the sentinel — called once at startup after we've read it,
/// so that this run's GPU-init attempts start with a clean slate.
pub fn clear() {
    mark_end();
}

/// Persist a sticky "GPU known bad" record so future cold starts skip the
/// crashing GPU path until the driver fingerprint changes (or the user
/// clicks "Retry GPU"). Idempotent — overwrites any prior record.
pub fn mark_known_bad(context: &str, fingerprint: &str) {
    assert!(
        !context.is_empty(),
        "gpu_health::mark_known_bad: context must be non-empty"
    );
    assert!(
        !fingerprint.is_empty(),
        "gpu_health::mark_known_bad: fingerprint must be non-empty"
    );
    let Some(path) = known_bad_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let record = KnownBadRecord {
        timestamp: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        context: context.to_string(),
        fingerprint: fingerprint.to_string(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&record) {
        let _ = std::fs::write(&path, json);
    }
}

/// Read the sticky known-bad record if present. Returns `None` when the file
/// does not exist, is unreadable, fails to parse, or has empty fields.
pub fn read_known_bad() -> Option<KnownBadRecord> {
    let path = known_bad_path()?;
    let json = std::fs::read_to_string(&path).ok()?;
    let record: KnownBadRecord = serde_json::from_str(&json).ok()?;
    if record.fingerprint.is_empty() || record.timestamp.is_empty() {
        return None;
    }
    Some(record)
}

/// Remove the sticky marker. Called from the FFI when the user clicks
/// "Retry GPU on next launch".
pub fn clear_known_bad() {
    if let Some(path) = known_bad_path() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that touch the real config dir so parallel runs don't
    // race on the same sentinel/known-bad file. CI runs with default threading.
    static FS_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn previous_crash_detected_returns_bool_without_panic() {
        let _guard = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = previous_crash_detected();
    }

    #[test]
    fn mark_begin_and_end_clear_sentinel() {
        let _guard = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        mark_end();
        mark_begin("test_context");
        mark_begin("test_context");
        mark_end();
        assert!(
            !previous_crash_detected(),
            "mark_end must clear the sentinel"
        );
    }

    #[test]
    fn one_abort_is_not_enough_to_condemn_the_gpu() {
        // The sentinel cannot tell a driver abort from the process being
        // killed. Both happened on 2026-09-05 and both silently moved every
        // later run onto the CPU. A real fault repeats; a kill does not.
        let _g = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_strike();

        assert!(
            !record_strike("fp-same"),
            "the first abort must NOT be sticky"
        );
        assert!(
            record_strike("fp-same"),
            "a second consecutive abort on the same driver must be sticky"
        );
    }

    #[test]
    fn a_working_gpu_forgets_the_earlier_abort() {
        let _g = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_strike();

        assert!(!record_strike("fp-same"), "first abort");
        // A run that gets through GPU init calls mark_end, which clears it.
        clear_strike();
        assert!(
            !record_strike("fp-same"),
            "an abort after a HEALTHY run starts the count again"
        );
    }

    #[test]
    fn a_different_driver_starts_the_count_again() {
        // A driver update is a new situation, not the second half of an old
        // one — the same reason the sticky marker is compared by fingerprint.
        let _g = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_strike();

        assert!(!record_strike("fp-old"), "first abort on the old driver");
        assert!(
            !record_strike("fp-new"),
            "an abort on a DIFFERENT driver is a first strike, not a second"
        );
        clear_strike();
    }

    #[test]
    fn known_bad_round_trip_preserves_fields() {
        let _guard = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_known_bad();
        mark_known_bad("test_context: load model X", "fp-abc123");
        let record = read_known_bad().expect("known-bad must be readable after mark");
        assert_eq!(record.context, "test_context: load model X");
        assert_eq!(record.fingerprint, "fp-abc123");
        assert!(
            !record.timestamp.is_empty(),
            "timestamp must be populated by mark_known_bad"
        );
        clear_known_bad();
        assert!(
            read_known_bad().is_none(),
            "clear_known_bad must remove the sticky marker"
        );
    }

    #[test]
    fn mark_known_bad_overwrites_prior_record() {
        let _guard = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_known_bad();
        mark_known_bad("first", "fp-old");
        mark_known_bad("second", "fp-new");
        let record = read_known_bad().expect("must be readable");
        assert_eq!(record.context, "second", "newer mark must overwrite older");
        assert_eq!(record.fingerprint, "fp-new");
        clear_known_bad();
    }

    #[test]
    fn read_known_bad_returns_none_when_missing() {
        let _guard = FS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_known_bad();
        assert!(read_known_bad().is_none());
    }

    #[test]
    #[should_panic(expected = "context must be non-empty")]
    fn mark_known_bad_rejects_empty_context() {
        mark_known_bad("", "fp-x");
    }

    #[test]
    #[should_panic(expected = "fingerprint must be non-empty")]
    fn mark_known_bad_rejects_empty_fingerprint() {
        mark_known_bad("ctx", "");
    }

    #[test]
    #[should_panic(expected = "context must be non-empty")]
    fn mark_begin_rejects_empty_context() {
        mark_begin("");
    }
}
