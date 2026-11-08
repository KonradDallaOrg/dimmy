//! Native crash dumps. A crash inside whisper.cpp, ggml, onnxruntime or a GPU
//! driver kills the process with no Rust panic, so Sentry's panic hook never
//! runs and the only trace was a Windows Error Reporting entry on the user's
//! machine. On Windows an unhandled-exception filter now writes a small
//! minidump into `<config>/crashes/`; the next launch sends it (with consent,
//! or once on request, see `crash_prompt`) and deletes it.
//!
//! The dump holds thread stacks and the module list, nothing else:
//! `MiniDumpFilterMemory` keeps only the pointer values a stack walk needs and
//! `MiniDumpFilterModulePaths` drops the folders (the account name) from
//! module paths. No heap, no handles, no environment.
//!
//! macOS and Linux write no dumps; the scan and the upload below find nothing
//! there. A SIGABRT on every platform is still reported by `abort_report`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const SUBDIR: &str = "crashes";
const EXT: &str = "dmp";
/// A dump the user was asked about and did not answer before the app closed.
/// No answer means no: the next launch deletes it.
pub(crate) const ASKED_EXT: &str = "asked";
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// More than this in one launch is a crash loop; the newest say it all.
const MAX_PER_LAUNCH: usize = 3;
/// Above this the dump is not the small kind we write; never upload it.
pub(crate) const MAX_BYTES: u64 = 20 * 1024 * 1024;

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// One dump left by an earlier launch, with the build that wrote it: the
/// running build may already be a newer one after an update.
#[derive(Debug, Clone, PartialEq)]
pub struct Dump {
    pub path: PathBuf,
    pub environment: String,
    pub release: String,
}

/// `<unix ms>_<pid>_<environment>_<release>.dmp`. The release goes last
/// because it is the only part that may contain `_`.
pub(crate) fn dump_name(unix_ms: u128, pid: u32, environment: &str, release: &str) -> String {
    assert!(!environment.is_empty() && !environment.contains('_'));
    assert!(!release.is_empty());
    format!("{unix_ms}_{pid}_{environment}_{release}.{EXT}")
}

pub(crate) fn parse_name(file_name: &str) -> Option<(String, String)> {
    let stem = file_name.strip_suffix(&format!(".{EXT}"))?;
    let mut parts = stem.splitn(4, '_');
    let ts = parts.next()?;
    let pid = parts.next()?;
    let environment = parts.next()?;
    let release = parts.next()?;
    if ts.parse::<u128>().is_err() || pid.parse::<u32>().is_err() || release.is_empty() {
        return None;
    }
    Some((environment.to_string(), release.to_string()))
}

/// Deletes what may not stay (older than a week, unanswered prompts, files
/// that are not ours, oversized dumps) and returns the dumps to report,
/// newest first, at most `MAX_PER_LAUNCH`; the older surplus is deleted too.
pub(crate) fn collect_in(dir: &Path, now: SystemTime) -> Vec<Dump> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, Dump)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let modified = meta.as_ref().and_then(|m| m.modified().ok()).unwrap_or(now);
        let too_old = now.duration_since(modified).unwrap_or_default() > MAX_AGE;
        let too_big = meta.as_ref().map(|m| m.len() > MAX_BYTES).unwrap_or(false);
        match parse_name(&name) {
            Some((environment, release)) if !too_old && !too_big => found.push((
                modified,
                Dump {
                    path,
                    environment,
                    release,
                },
            )),
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, surplus) in found.iter().skip(MAX_PER_LAUNCH) {
        let _ = std::fs::remove_file(&surplus.path);
    }
    let dumps: Vec<Dump> = found
        .into_iter()
        .take(MAX_PER_LAUNCH)
        .map(|(_, d)| d)
        .collect();
    assert!(dumps.len() <= MAX_PER_LAUNCH);
    dumps
}

/// The dumps earlier launches left, after the clean-up of `collect_in`.
pub fn collect() -> Vec<Dump> {
    match DIR.get() {
        Some(dir) => collect_in(dir, SystemTime::now()),
        None => Vec::new(),
    }
}

/// Marks a dump as asked about, so a launch that ends before the answer does
/// not ask about the same crash again.
pub(crate) fn mark_asked(dump: &Dump) -> Dump {
    let mut asked = dump.path.clone().into_os_string();
    asked.push(format!(".{ASKED_EXT}"));
    let asked = PathBuf::from(asked);
    match std::fs::rename(&dump.path, &asked) {
        Ok(()) => Dump {
            path: asked,
            ..dump.clone()
        },
        Err(_) => dump.clone(),
    }
}

/// Arms the dump writer for this process. Call once, from `telemetry::init`.
pub fn install(config_dir: &Path) {
    assert!(!config_dir.as_os_str().is_empty(), "dumps need a folder");
    let dir = config_dir.join(SUBDIR);
    let _ = std::fs::create_dir_all(&dir);
    if DIR.set(dir.clone()).is_err() {
        return;
    }
    let unix_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = dump_name(
        unix_ms,
        std::process::id(),
        crate::telemetry::sentry_pipeline::current_environment(),
        &crate::telemetry::sentry_pipeline::current_release(),
    );
    #[cfg(windows)]
    if win::arm(&dir.join(name)) {
        crate::log("[telemetry] native crash dumps armed");
    }
    #[cfg(not(windows))]
    let _ = name;
}

/// Called by the SIGABRT handler: an abort leaves a dump on Windows too.
pub(crate) fn write_for_abort() {
    #[cfg(windows)]
    win::write_for_abort();
}

/// `DIMMY_TEST_NATIVE_CRASH=1` crashes a debug build 3 s after init, on a
/// thread of its own like the inference threads, to prove the path end to
/// end. Release builds do not contain it.
#[cfg(debug_assertions)]
pub fn crash_on_request() {
    if std::env::var("DIMMY_TEST_NATIVE_CRASH").as_deref() != Ok("1") {
        return;
    }
    crate::log("[telemetry] DIMMY_TEST_NATIVE_CRASH=1: crashing in 3 s");
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        native_fault();
    });
}

#[cfg(any(test, debug_assertions))]
#[inline(never)]
fn native_fault() {
    // SAFETY: none, on purpose. Address 8 is never mapped on any OS Dimmy
    // supports, so the store faults like a bad pointer in C code would.
    unsafe { std::ptr::write_volatile(8usize as *mut u8, 1) };
}

#[cfg(windows)]
mod win {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
    use std::sync::OnceLock;
    use std::time::Duration;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE, NTSTATUS};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, DeleteFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE,
    };
    use windows::Win32::System::Diagnostics::Debug::{
        MiniDumpFilterMemory, MiniDumpFilterModulePaths, MiniDumpWithThreadInfo,
        MiniDumpWithUnloadedModules, MiniDumpWriteDump, RtlCaptureContext,
        SetUnhandledExceptionFilter, CONTEXT, EXCEPTION_POINTERS, EXCEPTION_RECORD,
        LPTOP_LEVEL_EXCEPTION_FILTER, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
    };

    /// A .NET exception nobody caught. The Windows host reports those itself,
    /// with managed frames a native dump cannot show.
    const CLR_EXCEPTION: u32 = 0xE043_4352;
    /// What crashpad uses for an abort, which raises no exception of its own.
    const STATUS_FATAL_APP_EXIT: i32 = 0x4000_0015;
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
    /// A dump of a few MB takes well under a second. If the writer is stuck
    /// (the crash holds a lock it needs), the app dies as it did before.
    const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

    /// The path as UTF-16, built at install: the crash path allocates nothing.
    static PATH: OnceLock<Vec<u16>> = OnceLock::new();
    /// Started at install and parked: a crashed thread may have no stack left
    /// (stack overflow), and creating a thread during a crash can deadlock on
    /// the loader lock.
    static WRITER: OnceLock<std::thread::Thread> = OnceLock::new();
    static PREVIOUS: AtomicUsize = AtomicUsize::new(0);
    static REQUEST: AtomicPtr<EXCEPTION_POINTERS> = AtomicPtr::new(std::ptr::null_mut());
    static REQUEST_THREAD: AtomicU32 = AtomicU32::new(0);
    static DONE: AtomicBool = AtomicBool::new(false);
    /// One dump per process: a second fault while writing must not recurse.
    static TAKEN: AtomicBool = AtomicBool::new(false);

    pub(super) fn arm(path: &Path) -> bool {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        assert!(wide.len() > 1 && wide.last() == Some(&0));
        if PATH.set(wide).is_err() {
            return false;
        }
        let writer = std::thread::Builder::new()
            .name("dimmy-crash-dump".into())
            .stack_size(256 * 1024)
            .spawn(writer_loop);
        let Ok(writer) = writer else {
            return false;
        };
        let _ = WRITER.set(writer.thread().clone());
        // SAFETY: the filter is a plain function that lives for the process;
        // the previous one is kept and called after ours.
        let previous = unsafe { SetUnhandledExceptionFilter(Some(on_unhandled)) };
        PREVIOUS.store(previous.map(|f| f as usize).unwrap_or(0), Ordering::SeqCst);
        true
    }

    fn previous() -> LPTOP_LEVEL_EXCEPTION_FILTER {
        let raw = PREVIOUS.load(Ordering::SeqCst);
        if raw == 0 {
            return None;
        }
        // SAFETY: `raw` came from SetUnhandledExceptionFilter as this type.
        Some(unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32>(
                raw,
            )
        })
    }

    unsafe extern "system" fn on_unhandled(info: *const EXCEPTION_POINTERS) -> i32 {
        let code = if info.is_null() || (*info).ExceptionRecord.is_null() {
            0
        } else {
            (*(*info).ExceptionRecord).ExceptionCode.0 as u32
        };
        let wrote = code != CLR_EXCEPTION && !info.is_null() && request(info as *mut _);
        let verdict = match previous() {
            Some(prev) => prev(info),
            None => EXCEPTION_CONTINUE_SEARCH,
        };
        if wrote && verdict == EXCEPTION_CONTINUE_EXECUTION {
            // The previous filter fixed it up: not a crash, not a report.
            if let Some(path) = PATH.get() {
                let _ = DeleteFileW(PCWSTR(path.as_ptr()));
            }
            DONE.store(false, Ordering::SeqCst);
            TAKEN.store(false, Ordering::SeqCst);
        }
        verdict
    }

    pub(super) fn write_for_abort() {
        // x64 RtlCaptureContext stores XMM registers with aligned moves.
        #[repr(C, align(16))]
        struct Aligned(CONTEXT);
        let mut context = Aligned(CONTEXT::default());
        // SAFETY: fills a 16-byte aligned CONTEXT on this stack frame.
        unsafe { RtlCaptureContext(&mut context.0) };
        let mut record = EXCEPTION_RECORD {
            ExceptionCode: NTSTATUS(STATUS_FATAL_APP_EXIT),
            ..Default::default()
        };
        let mut pointers = EXCEPTION_POINTERS {
            ExceptionRecord: &mut record,
            ContextRecord: &mut context.0,
        };
        request(&mut pointers);
    }

    /// Hands the crash to the writer thread and waits for it, bounded.
    fn request(info: *mut EXCEPTION_POINTERS) -> bool {
        let Some(writer) = WRITER.get() else {
            return false;
        };
        if TAKEN.swap(true, Ordering::SeqCst) {
            return false;
        }
        // SAFETY: plain Win32 getter.
        REQUEST_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        REQUEST.store(info, Ordering::SeqCst);
        writer.unpark();
        let step = Duration::from_millis(10);
        let mut waited = Duration::ZERO;
        while waited < WRITE_TIMEOUT {
            if DONE.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(step);
            waited += step;
        }
        false
    }

    fn writer_loop() {
        loop {
            std::thread::park();
            let info = REQUEST.swap(std::ptr::null_mut(), Ordering::SeqCst);
            if info.is_null() {
                continue;
            }
            write(info, REQUEST_THREAD.load(Ordering::SeqCst));
            DONE.store(true, Ordering::SeqCst);
        }
    }

    fn write(info: *mut EXCEPTION_POINTERS, thread_id: u32) {
        let Some(path) = PATH.get() else {
            return;
        };
        // SAFETY: `path` is NUL-terminated UTF-16; the handle is closed below.
        let file = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        let Ok(file) = file else {
            return;
        };
        let exception = MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: thread_id,
            ExceptionPointers: info,
            ClientPointers: false.into(),
        };
        let kind = MINIDUMP_TYPE(
            MiniDumpFilterMemory.0
                | MiniDumpFilterModulePaths.0
                | MiniDumpWithUnloadedModules.0
                | MiniDumpWithThreadInfo.0,
        );
        // SAFETY: the crashed thread is parked in `request` until DONE, so its
        // exception pointers stay valid for the whole call.
        unsafe {
            let _ = MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                file,
                kind,
                Some(&exception),
                None,
                None,
            );
            let _ = CloseHandle(file);
        }
    }

    #[cfg(test)]
    pub(super) fn previous_is_set() -> bool {
        PREVIOUS.load(Ordering::SeqCst) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dump_name_keeps_the_build_that_wrote_it() {
        let name = dump_name(1_700_000_000_000, 42, "production", "dimmy@0.7.20-rc.1+a_b");
        assert_eq!(
            parse_name(&name),
            Some(("production".into(), "dimmy@0.7.20-rc.1+a_b".into()))
        );
        assert_eq!(parse_name("notes.txt"), None);
        assert_eq!(parse_name("x_1_production_r.dmp"), None);
        assert_eq!(parse_name(&format!("{name}.{ASKED_EXT}")), None);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dimmy-dumps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("folder");
        dir
    }

    #[test]
    fn collect_keeps_fresh_dumps_and_deletes_the_rest() {
        let dir = temp_dir("collect");
        let fresh = dir.join(dump_name(2, 1, "development", "dimmy@1.0.0"));
        let asked = dir.join(format!(
            "{}.{ASKED_EXT}",
            dump_name(1, 1, "development", "dimmy@1.0.0")
        ));
        let stray = dir.join("other.txt");
        for p in [&fresh, &asked, &stray] {
            std::fs::write(p, b"MDMP").unwrap();
        }
        let dumps = collect_in(&dir, SystemTime::now());
        assert_eq!(dumps.len(), 1);
        assert_eq!(dumps[0].path, fresh);
        assert_eq!(dumps[0].release, "dimmy@1.0.0");
        assert!(!asked.exists(), "an unanswered prompt means no");
        assert!(!stray.exists());

        let week_later = SystemTime::now() + MAX_AGE + Duration::from_secs(60);
        assert!(collect_in(&dir, week_later).is_empty());
        assert!(!fresh.exists(), "a week-old dump is deleted unsent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_loop_reports_only_the_newest_dumps() {
        let dir = temp_dir("loop");
        for i in 0..5u128 {
            let p = dir.join(dump_name(i, 1, "development", "dimmy@1.0.0"));
            std::fs::write(&p, b"MDMP").unwrap();
            let t = SystemTime::now() - Duration::from_secs(100 - i as u64);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        let dumps = collect_in(&dir, SystemTime::now());
        assert_eq!(dumps.len(), MAX_PER_LAUNCH);
        assert_eq!(
            dumps[0].path,
            dir.join(dump_name(4, 1, "development", "dimmy@1.0.0"))
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), MAX_PER_LAUNCH);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_asked_renames_so_the_next_launch_deletes_it() {
        let dir = temp_dir("asked");
        let p = dir.join(dump_name(1, 1, "development", "dimmy@1.0.0"));
        std::fs::write(&p, b"MDMP").unwrap();
        let dumps = collect_in(&dir, SystemTime::now());
        let asked = mark_asked(&dumps[0]);
        assert!(!p.exists() && asked.path.exists());
        assert!(collect_in(&dir, SystemTime::now()).is_empty());
        assert!(!asked.path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    const CHILD_DIR: &str = "DIMMY_TEST_DUMP_DIR";
    #[cfg(windows)]
    const CHILD_MODE: &str = "DIMMY_TEST_DUMP_MODE";

    #[cfg(windows)]
    static PREV_MARKER: OnceLock<PathBuf> = OnceLock::new();

    #[cfg(windows)]
    unsafe extern "system" fn earlier_filter(
        _info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
    ) -> i32 {
        if let Some(p) = PREV_MARKER.get() {
            let _ = std::fs::write(p, b"called");
        }
        0
    }

    /// Runs only as a child of the tests below.
    #[cfg(windows)]
    #[test]
    fn dump_child() {
        let Some(dir) = std::env::var_os(CHILD_DIR) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let _ = PREV_MARKER.set(dir.join("previous_filter_ran"));
        // SAFETY: test-only filter, installed before ours to prove the chain.
        unsafe {
            windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
                earlier_filter,
            ));
        }
        install(&dir);
        assert!(win::previous_is_set());
        match std::env::var(CHILD_MODE).as_deref() {
            Ok("abort") => {
                crate::telemetry::abort_report::install(&dir);
                extern "C" {
                    fn abort() -> !;
                }
                unsafe { abort() }
            }
            _ => {
                let t = std::thread::spawn(native_fault);
                let _ = t.join();
            }
        }
    }

    #[cfg(windows)]
    fn run_child(mode: &str) -> PathBuf {
        let dir = temp_dir(mode);
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "telemetry::crash_dump::tests::dump_child",
                "--test-threads=1",
            ])
            .env(CHILD_DIR, &dir)
            .env(CHILD_MODE, mode)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("child");
        assert!(!status.success(), "the child crashed");
        dir
    }

    #[cfg(windows)]
    fn the_dump_in(dir: &Path) -> Vec<u8> {
        let dumps = collect_in(&dir.join(SUBDIR), SystemTime::now());
        assert_eq!(dumps.len(), 1, "one dump");
        let bytes = std::fs::read(&dumps[0].path).expect("dump");
        assert_eq!(&bytes[..4], b"MDMP");
        assert!((bytes.len() as u64) < MAX_BYTES);
        bytes
    }

    #[cfg(windows)]
    fn mentions(bytes: &[u8], needle: &str) -> bool {
        let utf16: Vec<u8> = needle
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
            || bytes.windows(utf16.len()).any(|w| w == utf16.as_slice())
    }

    #[cfg(windows)]
    #[test]
    fn a_native_fault_leaves_a_dump_and_still_runs_the_previous_filter() {
        let dir = run_child("fault");
        let bytes = the_dump_in(&dir);
        assert!(
            dir.join("previous_filter_ran").exists(),
            "the chain is kept"
        );
        // The account name shows up in module paths unless they are filtered.
        if let Ok(user) = std::env::var("USERNAME") {
            if user.len() >= 3 {
                assert!(!mentions(&bytes, &format!("\\{user}\\")), "no account name");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn an_abort_leaves_a_dump_too() {
        let dir = run_child("abort");
        let _ = the_dump_in(&dir);
        assert!(dir.join("last_abort.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
