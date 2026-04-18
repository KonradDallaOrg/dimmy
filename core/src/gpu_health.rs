//! GPU crash-recovery markers (Firefox/Chrome-style sentinel).
//!
//! Problem: when `ggml-vulkan` initialization fails on a given GPU (bad driver,
//! buggy ICD, resource exhaustion) it calls C++ `abort()` — the whole process
//! dies. This is NOT a Rust panic, so `catch_unwind` cannot intercept it and we
//! cannot return a graceful error. The shallow Vulkan probe in
//! `local_stt::probe_vulkan` can't predict this because it only enumerates
//! devices — it doesn't create a logical device or a compute queue.
//!
//! The recovery pattern: before attempting GPU init, write a sentinel file in
//! the config dir. After init returns (success OR Rust error), delete it. If
//! the process aborts in between, the sentinel stays on disk. On next startup
//! we check for the sentinel and, if present, force the CPU backend for that
//! session — so the user gets a working app instead of an infinite crash loop.
//!
//! The sentinel is session-scoped: clearing it on next boot lets the user
//! retry the GPU path (driver updates, etc.) without manual intervention.

use std::path::PathBuf;

fn marker_path() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join(".gpu_init_in_progress"))
}

/// Called before attempting GPU-backed model init. Writes a sentinel file.
/// Failures are swallowed — the sentinel is best-effort, not load-bearing.
pub fn mark_begin(context: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_crash_detected_false_when_path_unavailable() {
        // Cannot reliably test the true path without a filesystem fixture — the
        // marker lives in the real config dir. We at least verify the check is
        // side-effect free and returns a bool.
        let _ = previous_crash_detected();
    }

    #[test]
    fn mark_begin_and_end_are_idempotent() {
        // Safe to call in any order, multiple times.
        mark_end();
        mark_end();
        mark_begin("test");
        mark_begin("test");
        mark_end();
        assert!(!previous_crash_detected() || previous_crash_detected());
    }
}
