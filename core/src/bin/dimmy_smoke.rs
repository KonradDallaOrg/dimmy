//! dimmy_smoke — installer FFI smoke test.
//!
//! Runtime-loads `dimmy_lib.{dll,dylib,so}` from the current directory
//! (or the system search path) and calls every shipping `dimmy_*` export
//! with safe, side-effect-light arguments. Exits 0 if every call returns
//! a sensible code without panicking; exits 1 with a diagnostic line if
//! any symbol is missing, mangled, or crashes the host.
//!
//! This guards against the v0.6.10 class of failures: the cdylib was
//! built against a runtime ABI the installed environment couldn't honour,
//! so the very first FFI call from the UI took the process down. The
//! release `test-install.yml` only checks 15 s of UI startup — it never
//! crossed the FFI boundary, so the bug shipped. This smoke test calls
//! the actual symbols, so any ABI / linker / packaging regression that
//! manifests at first use surfaces here at PR time.
//!
//! Usage (typically from CI after installing the .exe):
//!   1. Build:  cargo build --release --bin dimmy_smoke --features smoke-test
//!   2. Copy `dimmy_smoke.exe` next to the installed `dimmy_lib.dll`.
//!   3. Run with that directory as CWD; assert exit code 0.
//!
//! Build:  cargo build --release --bin dimmy_smoke --features smoke-test

use std::ffi::CString;
use std::os::raw::{c_char, c_float, c_int};
use std::process::ExitCode;

use libloading::Library;

/// Per-platform library file name. We only load by name (not path) so the
/// OS loader picks it up from the current directory (Windows search order)
/// or LD_LIBRARY_PATH / DYLD_LIBRARY_PATH (Unix). The CI script is
/// responsible for placing the smoke binary next to the installed library.
fn lib_filename() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "dimmy_lib.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libdimmy_lib.dylib"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "libdimmy_lib.so"
    }
}

/// Aggregate failure tracking — we run every check, then report. A single
/// missing symbol shouldn't hide problems with the rest of the surface.
struct Report {
    failures: Vec<String>,
    checks: usize,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            checks: 0,
        }
    }

    fn ok(&mut self, name: &str, detail: impl std::fmt::Display) {
        self.checks += 1;
        println!("  ok    {}  {}", name, detail);
    }

    fn fail(&mut self, name: impl std::fmt::Display, reason: impl std::fmt::Display) {
        self.checks += 1;
        let msg = format!("{}: {}", name, reason);
        println!("  FAIL  {}", msg);
        self.failures.push(msg);
    }
}

/// Resolve a symbol from the loaded library; on failure, record the miss
/// and return None so we can keep going with the rest of the surface.
fn sym<T: Copy>(lib: &Library, name: &[u8], report: &mut Report) -> Option<T> {
    unsafe {
        match lib.get::<T>(name) {
            Ok(s) => {
                let value: T = *s;
                Some(value)
            }
            Err(e) => {
                report.fail(
                    String::from_utf8_lossy(name).trim_end_matches('\0'),
                    format!("symbol resolution failed: {}", e),
                );
                None
            }
        }
    }
}

/// Helper for the many `(buf, len) -> i32` getters.
fn call_buf_getter(
    name: &str,
    f: unsafe extern "C" fn(*mut c_char, c_int) -> c_int,
    report: &mut Report,
) {
    let mut buf = vec![0u8; 8192];
    let n = unsafe { f(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
    if n < 0 {
        report.fail(name, format!("returned negative rc={}", n));
    } else {
        report.ok(name, format!("rc={} bytes", n));
    }
}

fn main() -> ExitCode {
    println!("dimmy_smoke: loading {}", lib_filename());

    let lib = match unsafe { Library::new(lib_filename()) } {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: failed to load {}: {}", lib_filename(), e);
            eprintln!("Place dimmy_smoke next to the installed dimmy library and retry.");
            return ExitCode::from(2);
        }
    };

    let mut r = Report::new();

    // ── Lifecycle ───────────────────────────────────────────────────
    if let Some(dimmy_init) = sym::<unsafe extern "C" fn() -> c_int>(&lib, b"dimmy_init", &mut r) {
        let rc = unsafe { dimmy_init() };
        if rc == 0 || rc == 1 {
            r.ok("dimmy_init", format!("rc={}", rc));
        } else {
            r.fail("dimmy_init", format!("unexpected rc={}", rc));
        }
    }

    // ── Read-only getters ───────────────────────────────────────────
    if let Some(f) =
        sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(&lib, b"dimmy_get_version", &mut r)
    {
        call_buf_getter("dimmy_get_version", f, &mut r);
    }
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_get_config_json",
        &mut r,
    ) {
        call_buf_getter("dimmy_get_config_json", f, &mut r);
    }
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_list_devices_json",
        &mut r,
    ) {
        call_buf_getter("dimmy_list_devices_json", f, &mut r);
    }
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_check_audio_health",
        &mut r,
    ) {
        call_buf_getter("dimmy_check_audio_health", f, &mut r);
    }
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_list_local_models",
        &mut r,
    ) {
        call_buf_getter("dimmy_list_local_models", f, &mut r);
    }
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_list_llm_models",
        &mut r,
    ) {
        call_buf_getter("dimmy_list_llm_models", f, &mut r);
    }
    if let Some(f) = sym::<unsafe extern "C" fn(c_int, *mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_history_recent",
        &mut r,
    ) {
        let mut buf = vec![0u8; 8192];
        let n = unsafe { f(5, buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
        if n < 0 {
            r.fail("dimmy_history_recent", format!("rc={}", n));
        } else {
            r.ok("dimmy_history_recent", format!("rc={}", n));
        }
    }
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_history_stats",
        &mut r,
    ) {
        call_buf_getter("dimmy_history_stats", f, &mut r);
    }
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_gpu_get_status",
        &mut r,
    ) {
        call_buf_getter("dimmy_gpu_get_status", f, &mut r);
    }

    // ── Scalar getters ──────────────────────────────────────────────
    if let Some(f) = sym::<unsafe extern "C" fn() -> c_int>(&lib, b"dimmy_has_api_key", &mut r) {
        let rc = unsafe { f() };
        r.ok("dimmy_has_api_key", format!("rc={}", rc));
    }
    if let Some(f) = sym::<unsafe extern "C" fn() -> c_int>(&lib, b"dimmy_is_recording", &mut r) {
        let rc = unsafe { f() };
        if rc == 0 {
            r.ok("dimmy_is_recording", "rc=0 (idle as expected)");
        } else {
            r.fail("dimmy_is_recording", format!("expected 0, got {}", rc));
        }
    }
    if let Some(f) = sym::<unsafe extern "C" fn() -> c_float>(&lib, b"dimmy_get_amplitude", &mut r)
    {
        let amp = unsafe { f() };
        if amp.is_finite() {
            r.ok("dimmy_get_amplitude", format!("{:.4}", amp));
        } else {
            r.fail("dimmy_get_amplitude", format!("non-finite: {}", amp));
        }
    }

    // ── A safe set_config round-trip ─────────────────────────────────
    if let Some(f) =
        sym::<unsafe extern "C" fn(*const c_char) -> c_int>(&lib, b"dimmy_set_config_json", &mut r)
    {
        let json = CString::new(r#"{"language":"en"}"#).unwrap();
        let rc = unsafe { f(json.as_ptr()) };
        if rc == 0 {
            r.ok("dimmy_set_config_json", "rc=0");
        } else {
            r.fail("dimmy_set_config_json", format!("rc={}", rc));
        }
    }

    // ── stop_recording with no active session: contract is "no crash". ──
    // Returns either 0 (writes empty string) or a negative error; both
    // are acceptable. The point is the call must not blow up the process.
    if let Some(f) = sym::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
        &lib,
        b"dimmy_stop_recording",
        &mut r,
    ) {
        let mut buf = vec![0u8; 8192];
        let n = unsafe { f(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
        r.ok("dimmy_stop_recording", format!("rc={} (no-op call)", n));
    }

    // ── Shutdown ────────────────────────────────────────────────────
    if let Some(f) = sym::<unsafe extern "C" fn()>(&lib, b"dimmy_shutdown", &mut r) {
        unsafe { f() };
        r.ok("dimmy_shutdown", "returned");
    }

    println!();
    println!(
        "dimmy_smoke: {} checks, {} failures",
        r.checks,
        r.failures.len()
    );
    if r.failures.is_empty() {
        println!("PASS");
        ExitCode::SUCCESS
    } else {
        println!("FAIL — first failure: {}", r.failures[0]);
        ExitCode::FAILURE
    }
}
