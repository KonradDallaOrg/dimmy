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
use std::sync::Once;

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
unsafe extern "C" fn ggml_log_trampoline(level: i32, text: *const c_char, _user_data: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        if text.is_null() {
            return;
        }
        let msg = unsafe { CStr::from_ptr(text) }.to_string_lossy();
        let trimmed = msg.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            return;
        }
        let tag = match level {
            1 => "DEBUG",
            2 => "INFO",
            3 => "WARN",
            4 => "ERROR",
            _ => "LOG",
        };
        crate::log(&format!("[ggml {}] {}", tag, trimmed));
    });
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

#[cfg(target_os = "windows")]
fn log_registered_icds() {
    // HKLM\Software\Khronos\Vulkan\Drivers contains REG_DWORD values where the
    // value NAME is the ICD JSON path. Enumerate and log.
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
    unsafe {
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey_w.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            crate::log(
                "[gpu_diag] No Vulkan ICDs registered in HKLM\\...\\Khronos\\Vulkan\\Drivers",
            );
            return;
        }
        let mut index = 0u32;
        let mut found = 0u32;
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
            crate::log(&format!(
                "[gpu_diag] Vulkan ICD: {} (disabled={})",
                icd_path,
                data != 0
            ));
            found += 1;
            index += 1;
        }
        RegCloseKey(hkey);
        if found == 0 {
            crate::log("[gpu_diag] HKLM Khronos\\Vulkan\\Drivers exists but empty");
        }
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
}
