//! Native aborts (ggml's `GGML_ASSERT`, an engine running out of memory) kill
//! the process before Sentry's panic hook or a host handler sees anything: three
//! of them on 2026-10-30 left a Windows crash dump and nothing else. SIGABRT
//! does run a handler first, so it leaves a marker and the next launch reports it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MARKER_FILE: &str = "last_abort.txt";
#[cfg(windows)]
const SIGABRT: i32 = 22;
#[cfg(not(windows))]
const SIGABRT: i32 = 6;

static MARKER: OnceLock<PathBuf> = OnceLock::new();

extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

extern "C" fn on_abort(_signum: i32) {
    if let Some(path) = MARKER.get() {
        let _ = std::fs::write(path, marker_text(crate::setup::available_memory_bytes()));
    }
}

fn marker_text(available: Option<u64>) -> String {
    match available {
        Some(bytes) => format!("native abort, {} MB free", bytes / (1024 * 1024)),
        None => "native abort".to_string(),
    }
}

/// Returns what the previous launch left, and arms the handler for this one.
pub fn install(dir: &Path) -> Option<String> {
    assert!(!dir.as_os_str().is_empty(), "the marker needs a folder");
    let path = dir.join(MARKER_FILE);
    let previous = std::fs::read_to_string(&path).ok();
    if previous.is_some() {
        let _ = std::fs::remove_file(&path);
    }
    let _ = MARKER.set(path);
    // SAFETY: registers a handler that lives for the process, and the C runtime
    // the engines abort through is the one this library links.
    unsafe { signal(SIGABRT, on_abort as extern "C" fn(i32) as usize) };
    previous
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD_DIR: &str = "DIMMY_TEST_ABORT_DIR";

    extern "C" {
        fn abort() -> !;
    }

    #[test]
    fn abort_child() {
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            install(Path::new(&dir));
            unsafe { abort() };
        }
    }

    #[test]
    fn an_abort_is_reported_once_by_the_next_launch() {
        let dir = std::env::temp_dir().join(format!("dimmy-abort-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("folder");
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "telemetry::abort_report::tests::abort_child"])
            .env(CHILD_DIR, &dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("child");
        assert!(!status.success(), "the child aborted");
        let path = dir.join(MARKER_FILE);
        let text = std::fs::read_to_string(&path).expect("the handler left the marker");
        assert!(text.starts_with("native abort"), "{text}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn the_marker_says_only_how_much_memory_was_free() {
        assert_eq!(
            marker_text(Some(3 * 1024 * 1024)),
            "native abort, 3 MB free"
        );
        assert_eq!(marker_text(None), "native abort");
    }
}
