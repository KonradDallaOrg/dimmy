//! GPU diagnostics: capture ggml's internal log output and dump a snapshot of
//! the Vulkan environment when the GPU backend is about to be used.
//!
//! Rationale: when `ggml-vulkan` aborts the process during whisper/llama init
//! (see `gpu_health.rs`), the C++ layer usually prints an error message first
//! — but it goes to stderr, which is invisible on Windows GUI apps. Registering
//! a log callback on both whisper and llama routes those messages into our
//! `dimmy.log`, so the last words before a crash become visible post-mortem.
//!
//! The environment snapshot is a one-shot log of what's loaded: vulkan-1.dll
//! path and size, per-device VRAM, driver version (best-effort, registry). Any
//! of these being surprising can explain why GPU init fails on one machine but
//! not another with ostensibly identical hardware.

use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

/// Whether to forward `[ggml DEBUG]` lines to dimmy.log. Toggled from the UI
/// via FFI (`dimmy_set_config_json` → `set_ggml_debug_enabled`). Default OFF
/// because the per-tensor / per-layer dumps from whisper + llama load fill
/// hundreds of lines per cold start. INFO/WARN/ERROR always pass through —
/// those carry the actual diagnostic signal we keep for crash post-mortems.
static GGML_DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

/// Set at startup from `AppConfig::ggml_debug_logging` and on every
/// `dimmy_set_config_json` call. Lock-free read in the C-ABI trampoline.
pub fn set_ggml_debug_enabled(enabled: bool) {
    GGML_DEBUG_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Read the current toggle state. Used only by the trampoline.
fn is_ggml_debug_enabled() -> bool {
    GGML_DEBUG_ENABLED.load(Ordering::Relaxed)
}

// `ggml_log_level` is bindgen-generated from a C enum and its underlying type
// differs per target: MSVC on Windows emits `c_int` (signed) because MSVC
// defaults C enums to signed int; gcc on Linux and Apple clang on macOS both
// emit `c_uint` (unsigned) because non-negative enum values round up to the
// unsigned representation. The fn-pointer signature must match exactly for
// `whisper_log_set` / `llama_log_set`, so type-alias it conditionally.
#[cfg(target_os = "windows")]
type GgmlLogLevel = std::os::raw::c_int;
#[cfg(not(target_os = "windows"))]
type GgmlLogLevel = std::os::raw::c_uint;

/// Install log callbacks for whisper and llama. Safe to call multiple times —
/// only the first call installs. Callbacks route all ggml output through
/// `crate::log` with a `[ggml]` prefix and level tag.
pub fn install_ggml_log_callbacks() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        #[cfg(feature = "local-stt")]
        unsafe {
            whisper_rs::set_log_callback(Some(ggml_log_trampoline), std::ptr::null_mut());
        }
        #[cfg(feature = "local-llm")]
        unsafe {
            llama_cpp_4::log_set(Some(ggml_log_trampoline), std::ptr::null_mut());
        }
        crate::log("[gpu_diag] ggml log callbacks installed");
    });
}

/// C ABI trampoline for ggml log output. Must NOT unwind across the FFI
/// boundary — wrap everything in `catch_unwind` and swallow panics.
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
unsafe extern "C" fn ggml_log_trampoline(
    level: GgmlLogLevel,
    text: *const c_char,
    _user_data: *mut c_void,
) {
    let _ = std::panic::catch_unwind(|| {
        if text.is_null() {
            return;
        }
        let msg = unsafe { CStr::from_ptr(text) }.to_string_lossy();
        let trimmed = msg.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            return;
        }
        const LVL_DEBUG: GgmlLogLevel = 1;
        const LVL_INFO: GgmlLogLevel = 2;
        const LVL_WARN: GgmlLogLevel = 3;
        const LVL_ERROR: GgmlLogLevel = 4;
        // DEBUG is the loud one (per-tensor / per-layer dumps). Drop unless
        // the user explicitly asked for it via the Settings toggle.
        if level == LVL_DEBUG && !is_ggml_debug_enabled() {
            return;
        }
        let tag = match level {
            LVL_DEBUG => "DEBUG",
            LVL_INFO => "INFO",
            LVL_WARN => "WARN",
            LVL_ERROR => "ERROR",
            _ => "LOG",
        };
        crate::log(&format!("[ggml {}] {}", tag, trimmed));
    });
}

/// Deterministic, opaque fingerprint of the GPU loader environment. Used by
/// the sticky known-bad marker (see `gpu_health::KnownBadRecord`) to decide
/// whether a previously-crashing GPU stack might now be fixed: if the
/// fingerprint changes between sessions (driver update, ICD added/removed,
/// vulkan-1.dll replaced), we retry GPU once. If it matches, we stay on CPU
/// and skip the crash.
///
/// Format is intentionally human-readable rather than hashed — the file is
/// inspected by hand during diagnosis. Stability across runs is required;
/// we sort the ICD list to drop registry enumeration order.
///
/// macOS returns a constant since Metal has no equivalent loader to probe.
pub fn compute_driver_fingerprint() -> String {
    let result;
    #[cfg(target_os = "windows")]
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some((_path, size)) = find_dll("vulkan-1.dll") {
            parts.push(format!("vk={}", size));
        } else {
            parts.push("vk=none".to_string());
        }
        let mut icds: Vec<String> = enumerate_registered_icds()
            .into_iter()
            .map(|(name, disabled)| format!("{}|{}", name, disabled as u8))
            .collect();
        icds.sort();
        parts.push(format!("icds=[{}]", icds.join(",")));
        result = parts.join(";");
    }
    #[cfg(target_os = "linux")]
    {
        let mut entries: Vec<String> = Vec::new();
        for dir in ["/etc/vulkan/icd.d", "/usr/share/vulkan/icd.d"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let path = e.path();
                    let size = path.metadata().map(|m| m.len()).unwrap_or(0);
                    entries.push(format!("{}|{}", path.display(), size));
                }
            }
        }
        entries.sort();
        result = format!("icds=[{}]", entries.join(","));
    }
    #[cfg(target_os = "macos")]
    {
        result = "macos-metal".to_string();
    }
    assert!(
        !result.is_empty(),
        "compute_driver_fingerprint: result must be non-empty"
    );
    result
}

/// Log a one-shot snapshot of the Vulkan environment. Call once before first
/// GPU init. Safe to call multiple times — only the first call does work.
pub fn log_environment_snapshot() {
    static DONE: Once = Once::new();
    DONE.call_once(|| {
        #[cfg(target_os = "windows")]
        log_windows_env();
        #[cfg(target_os = "linux")]
        log_linux_env();
        #[cfg(target_os = "macos")]
        crate::log("[gpu_diag] macOS — Metal backend, no Vulkan probe needed");
    });
}

/// Point the Vulkan loader at a non-existent ICD manifest so that
/// `vkEnumeratePhysicalDevices` returns zero devices. ggml-vulkan's
/// `ggml_vk_instance_init` then takes the "No devices found" early-return
/// path at ggml-vulkan.cpp:5500 and never touches driver code that may
/// abort the process.
///
/// Critical on hosts where `ggml_vk_instance_init` aborts even when the
/// caller passes `use_gpu=false`: whisper.cpp's `ggml_backend_init_by_type(CPU)`
/// unconditionally triggers the backend-registry singleton, which calls
/// `ggml_backend_vk_reg` → `ggml_vk_instance_init`. The only reliable way to
/// bypass that C++ path from Rust is at the Vulkan loader layer.
///
/// Must be called BEFORE the first `LlamaBackend::init()` or
/// `WhisperContext::new_with_params` call in the process. Idempotent.
///
/// No-op on macOS (Metal backend, no Vulkan loader).
pub fn disable_vulkan_loader(reason: &str) {
    #[cfg(not(target_os = "macos"))]
    {
        static DONE: Once = Once::new();
        DONE.call_once(|| {
            // Cross-platform: both env var names are honored — VK_DRIVER_FILES
            // is the current name (Vulkan SDK 1.3.207+), VK_ICD_FILENAMES is
            // the legacy name still accepted by older loaders.
            #[cfg(target_os = "windows")]
            let bogus = r"C:\nonexistent\dimmy-vulkan-disabled.json";
            #[cfg(target_os = "linux")]
            let bogus = "/nonexistent/dimmy-vulkan-disabled.json";
            #[cfg(not(any(target_os = "windows", target_os = "linux")))]
            let bogus = "/nonexistent/dimmy-vulkan-disabled.json";

            // SAFETY: first caller is `compute_gpu_backend_status` on the
            // thread that will next call into ggml; ggml-vulkan reads these
            // env vars inside `vk::createInstance` on the same thread shortly
            // after. No other thread reads these Vulkan-loader env vars.
            std::env::set_var("VK_DRIVER_FILES", bogus);
            std::env::set_var("VK_ICD_FILENAMES", bogus);
            crate::log(&format!(
                "[gpu_diag] Vulkan loader disabled (VK_DRIVER_FILES + VK_ICD_FILENAMES → {}). \
                 Reason: {}. ggml-vulkan will see zero ICDs and take the safe early-return.",
                bogus, reason
            ));
        });
    }
    #[cfg(target_os = "macos")]
    {
        let _ = reason;
    }
}

#[cfg(target_os = "windows")]
fn log_windows_env() {
    // vulkan-1.dll location + size (if present in PATH or System32)
    if let Some((path, size)) = find_dll("vulkan-1.dll") {
        crate::log(&format!(
            "[gpu_diag] vulkan-1.dll: {} ({} bytes)",
            path, size
        ));
    } else {
        crate::log("[gpu_diag] vulkan-1.dll: NOT FOUND in loader path");
    }

    // TdrDelay — how long Windows waits before killing a hung GPU operation.
    // Default is 2 seconds. Model load on slow GPUs can exceed this.
    if let Some(v) = read_registry_dword(
        r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers",
        "TdrDelay",
    ) {
        crate::log(&format!("[gpu_diag] TdrDelay = {} seconds", v));
    }
    if let Some(v) = read_registry_dword(
        r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers",
        "TdrDdiDelay",
    ) {
        crate::log(&format!("[gpu_diag] TdrDdiDelay = {} seconds", v));
    }

    // Registered Vulkan ICDs — one per GPU vendor present on the machine.
    // If this list is empty or has stale entries, init can fail weirdly.
    log_registered_icds();
}

#[cfg(target_os = "linux")]
fn log_linux_env() {
    // Linux ICD registration is via /etc/vulkan/icd.d and
    // /usr/share/vulkan/icd.d. Log which files exist.
    for dir in ["/etc/vulkan/icd.d", "/usr/share/vulkan/icd.d"] {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                crate::log(&format!("[gpu_diag] ICD: {}", e.path().display()));
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn find_dll(name: &str) -> Option<(String, u64)> {
    extern "system" {
        fn LoadLibraryA(name: *const u8) -> *mut std::ffi::c_void;
        fn GetModuleFileNameA(module: *mut std::ffi::c_void, buf: *mut u8, size: u32) -> u32;
        fn FreeLibrary(module: *mut std::ffi::c_void) -> i32;
    }
    let mut c_name = name.as_bytes().to_vec();
    c_name.push(0);
    unsafe {
        let module = LoadLibraryA(c_name.as_ptr());
        if module.is_null() {
            return None;
        }
        let mut buf = vec![0u8; 1024];
        let len = GetModuleFileNameA(module, buf.as_mut_ptr(), buf.len() as u32);
        FreeLibrary(module);
        if len == 0 {
            return None;
        }
        buf.truncate(len as usize);
        let path = String::from_utf8_lossy(&buf).into_owned();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Some((path, size))
    }
}

#[cfg(target_os = "windows")]
fn read_registry_dword(subkey: &str, value: &str) -> Option<u32> {
    // Minimal raw Win32 to read HKEY_LOCAL_MACHINE\subkey\value as REG_DWORD.
    // Avoids pulling in a registry crate for 20 lines.
    const HKEY_LOCAL_MACHINE: isize = 0x80000002_u32 as i32 as isize;
    const KEY_READ: u32 = 0x20019;
    const ERROR_SUCCESS: i32 = 0;

    extern "system" {
        fn RegOpenKeyExW(
            key: isize,
            subkey: *const u16,
            options: u32,
            sam_desired: u32,
            out_key: *mut isize,
        ) -> i32;
        fn RegQueryValueExW(
            key: isize,
            value: *const u16,
            reserved: *mut u32,
            typ: *mut u32,
            data: *mut u8,
            data_len: *mut u32,
        ) -> i32;
        fn RegCloseKey(key: isize) -> i32;
    }

    let subkey_w: Vec<u16> = subkey.encode_utf16().chain([0]).collect();
    let value_w: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let mut hkey: isize = 0;
    unsafe {
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey_w.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut data: u32 = 0;
        let mut len: u32 = 4;
        let rc = RegQueryValueExW(
            hkey,
            value_w.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut u8,
            &mut len,
        );
        RegCloseKey(hkey);
        if rc == ERROR_SUCCESS {
            Some(data)
        } else {
            None
        }
    }
}

/// Enumerate registered Vulkan ICDs from
/// `HKLM\SOFTWARE\Khronos\Vulkan\Drivers`. Each entry is `(value_name,
/// disabled)` where `value_name` is the ICD JSON path and `disabled` is true
/// when the REG_DWORD value is non-zero (Vulkan loader convention).
///
/// Returns an empty Vec if the registry key is missing, unreadable, or empty.
/// Used both by `log_registered_icds` (one-line-per-ICD diagnostic) and
/// `compute_driver_fingerprint` (sorted, joined into the fingerprint).
#[cfg(target_os = "windows")]
fn enumerate_registered_icds() -> Vec<(String, bool)> {
    const HKEY_LOCAL_MACHINE: isize = 0x80000002_u32 as i32 as isize;
    const KEY_READ: u32 = 0x20019;
    const ERROR_SUCCESS: i32 = 0;

    extern "system" {
        fn RegOpenKeyExW(
            key: isize,
            subkey: *const u16,
            options: u32,
            sam_desired: u32,
            out_key: *mut isize,
        ) -> i32;
        fn RegEnumValueW(
            key: isize,
            index: u32,
            value_name: *mut u16,
            value_name_len: *mut u32,
            reserved: *mut u32,
            typ: *mut u32,
            data: *mut u8,
            data_len: *mut u32,
        ) -> i32;
        fn RegCloseKey(key: isize) -> i32;
    }

    let subkey_w: Vec<u16> = r"SOFTWARE\Khronos\Vulkan\Drivers"
        .encode_utf16()
        .chain([0])
        .collect();
    let mut hkey: isize = 0;
    let mut entries = Vec::new();
    unsafe {
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey_w.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return entries;
        }
        let mut index = 0u32;
        loop {
            let mut name = [0u16; 1024];
            let mut name_len = name.len() as u32;
            let mut data = 0u32;
            let mut data_len = 4u32;
            let rc = RegEnumValueW(
                hkey,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut data as *mut u32 as *mut u8,
                &mut data_len,
            );
            if rc != ERROR_SUCCESS {
                break;
            }
            let icd_path = String::from_utf16_lossy(&name[..name_len as usize]);
            entries.push((icd_path, data != 0));
            index += 1;
        }
        RegCloseKey(hkey);
    }
    entries
}

#[cfg(target_os = "windows")]
fn log_registered_icds() {
    let entries = enumerate_registered_icds();
    if entries.is_empty() {
        crate::log("[gpu_diag] No Vulkan ICDs registered in HKLM\\...\\Khronos\\Vulkan\\Drivers");
        return;
    }
    for (path, disabled) in &entries {
        crate::log(&format!(
            "[gpu_diag] Vulkan ICD: {} (disabled={})",
            path, disabled
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_callbacks_is_idempotent() {
        install_ggml_log_callbacks();
        install_ggml_log_callbacks();
    }

    #[test]
    fn log_environment_snapshot_is_idempotent() {
        log_environment_snapshot();
        log_environment_snapshot();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn disable_vulkan_loader_sets_env_vars() {
        // First call in this test process — other tests may not touch VK vars.
        disable_vulkan_loader("unit test");
        let drv = std::env::var("VK_DRIVER_FILES").unwrap_or_default();
        let icd = std::env::var("VK_ICD_FILENAMES").unwrap_or_default();
        assert!(
            drv.contains("dimmy-vulkan-disabled.json"),
            "VK_DRIVER_FILES must point to bogus path, got: {:?}",
            drv
        );
        assert!(
            icd.contains("dimmy-vulkan-disabled.json"),
            "VK_ICD_FILENAMES must point to bogus path, got: {:?}",
            icd
        );
        // Subsequent calls must be idempotent (Once) — same values after.
        disable_vulkan_loader("second call should be a no-op");
        assert_eq!(std::env::var("VK_DRIVER_FILES").unwrap(), drv);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn disable_vulkan_loader_is_noop_on_macos() {
        // macOS uses Metal — no Vulkan loader to disable. Must not touch env.
        std::env::remove_var("VK_DRIVER_FILES");
        disable_vulkan_loader("macOS no-op check");
        assert!(std::env::var("VK_DRIVER_FILES").is_err());
    }

    #[test]
    fn driver_fingerprint_is_non_empty() {
        let fp = compute_driver_fingerprint();
        assert!(
            !fp.is_empty(),
            "fingerprint must always be a non-empty string"
        );
    }

    #[test]
    fn driver_fingerprint_is_stable_across_calls() {
        // Same env → same fingerprint. Required for the known-bad equality
        // check to be meaningful: spurious mismatches would force GPU retry
        // (and another crash) on every cold start.
        let a = compute_driver_fingerprint();
        let b = compute_driver_fingerprint();
        assert_eq!(a, b, "fingerprint must be deterministic for a given env");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn driver_fingerprint_is_constant_on_macos() {
        // Metal has no equivalent loader — fingerprint must be a fixed marker
        // so the known-bad path becomes a no-op on macOS.
        assert_eq!(compute_driver_fingerprint(), "macos-metal");
    }

    #[test]
    fn ggml_debug_toggle_round_trips() {
        // The toggle is a process-wide AtomicBool; restore it after the test
        // so we don't leak state to other tests in this run.
        let prior = is_ggml_debug_enabled();
        set_ggml_debug_enabled(true);
        assert!(is_ggml_debug_enabled());
        set_ggml_debug_enabled(false);
        assert!(!is_ggml_debug_enabled());
        set_ggml_debug_enabled(prior);
    }
}
