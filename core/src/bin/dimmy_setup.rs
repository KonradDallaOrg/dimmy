//! dimmy-setup launcher (Windows / Linux).
//!
//! Loads `dimmy_lib` from the folder this exe lives in (never the current
//! directory, which an agent controls) and forwards argv to
//! `dimmy_setup_cli`, so the install carries one copy of the core. macOS runs
//! the same entry point as `Dimmy.app/Contents/MacOS/Dimmy --setup`.
//! Contract: `docs/dev/agent-setup.md`.
//!
//! Build: cargo build --release --bin dimmy_setup --features setup-launcher

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use libloading::Library;

type SetupCli = unsafe extern "C" fn(c_int, *const *const c_char) -> c_int;

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

fn fail(code: i32, error: &str, message: &str) -> ! {
    let v = serde_json::json!({ "ok": false, "error": {
        "code": error, "message": message,
        "fix": "Reinstall Dimmy from https://dimmy.app." } });
    println!("{v}");
    std::process::exit(code);
}

fn main() {
    let exe = std::env::current_exe().unwrap_or_else(|e| fail(1, "launcher", &e.to_string()));
    let dir = exe
        .parent()
        .unwrap_or_else(|| fail(1, "launcher", "the launcher has no folder"));
    let path = dir.join(lib_filename());
    assert!(
        path.is_absolute(),
        "the core is loaded by absolute path only"
    );

    let args: Vec<CString> = std::env::args_os()
        .skip(1)
        .map(|a| {
            let s = a
                .into_string()
                .unwrap_or_else(|_| fail(2, "usage", "arguments must be UTF-8"));
            CString::new(s).unwrap_or_else(|_| fail(2, "usage", "arguments must not contain NUL"))
        })
        .collect();
    let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();

    let lib = unsafe { Library::new(&path) }.unwrap_or_else(|e| {
        fail(
            1,
            "core_missing",
            &format!("could not load the Dimmy core: {e}"),
        )
    });
    let code = unsafe {
        let f: libloading::Symbol<SetupCli> = lib
            .get(b"dimmy_setup_cli\0")
            .unwrap_or_else(|_| fail(1, "core_too_old", "this Dimmy core has no dimmy_setup_cli"));
        f(argv.len() as c_int, argv.as_ptr())
    };
    assert!((0..=3).contains(&code), "exit code out of contract: {code}");
    // Hard exit, like dimmy_smoke: skip the core's teardown, which can fault
    // after the work is done (model caches, ggml backends, worker threads).
    std::process::exit(code);
}
