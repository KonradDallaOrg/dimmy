//! Windows-only helpers for locating just-installed CLI binaries
//! (`claude`, `codex`, `node`) that a running Dimmy can't see through
//! its own environment.
//!
//! Why this exists: Windows installers (winget portable packages, the
//! native `install.ps1` scripts, npm) place the binary in a per-user
//! dir and register it on PATH via the registry (`HKCU\Environment`)
//! plus a `WM_SETTINGCHANGE` broadcast that only windowed shells like
//! Explorer act on. A running process keeps its launch-time PATH
//! snapshot forever, so a PATH walk inside Dimmy stays blind to the
//! new install until a full app restart. Mac/Linux solve this with the
//! login-shell fallback (`zsh -l -c 'command -v claude'`); Windows has
//! no login-shell equivalent, so we read the LIVE PATH straight from
//! the registry — exactly what a freshly opened terminal would see.
//!
//! Burned 2026-07-03: the Claude CLI setup wizard installed via
//! `winget install Anthropic.ClaudeCode`, then sat frozen on the
//! "Looking for the Claude Code CLI..." step until the user restarted
//! Dimmy twice. The binary was at
//! `%LOCALAPPDATA%\Microsoft\WinGet\Packages\Anthropic.ClaudeCode_...\claude.exe`,
//! a dir neither enumerated explicitly nor present in the stale PATH.

use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};

/// Read a string value via `RegGetValueW`. `RRF_RT_REG_SZ` makes the
/// API expand `REG_EXPAND_SZ` values (e.g. `%USERPROFILE%\...`) before
/// returning, so callers always get concrete paths.
fn read_registry_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let value_w: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();

    let mut size: u32 = 0;
    // First call: query byte size (data pointer = None).
    let rc = unsafe {
        RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if rc.is_err() || size == 0 {
        return None;
    }

    // Second call: fetch the data. Size is in BYTES; buffer is u16.
    let mut buf: Vec<u16> = vec![0; (size as usize).div_ceil(2)];
    let mut size2 = size;
    let rc = unsafe {
        RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size2),
        )
    };
    if rc.is_err() {
        return None;
    }

    // Trim at the first NUL — RegGetValueW guarantees termination.
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = std::ffi::OsString::from_wide(&buf[..len]);
    let s = s.to_string_lossy().into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Directories on the LIVE registry PATH — user (`HKCU\Environment`)
/// first, then machine (`HKLM\...\Session Manager\Environment`). This
/// is the union a brand-new terminal would inherit, independent of
/// this process's stale launch-time snapshot.
pub(crate) fn registry_path_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let user = read_registry_string(HKEY_CURRENT_USER, "Environment", "Path");
    let machine = read_registry_string(
        HKEY_LOCAL_MACHINE,
        "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
        "Path",
    );
    for path in [user, machine].into_iter().flatten() {
        for dir in std::env::split_paths(&path) {
            if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// WinGet portable-package binary dirs. Two layouts exist:
///   - `%LOCALAPPDATA%\Microsoft\WinGet\Links\<binary>.exe` — the
///     symlink farm winget puts on PATH when it can create symlinks.
///   - `%LOCALAPPDATA%\Microsoft\WinGet\Packages\<Id>_<Source>\<binary>.exe`
///     — when symlink creation is unavailable (non-admin, developer
///     mode off) winget adds the package dir itself to PATH instead.
///     Observed live with `Anthropic.ClaudeCode` on 2026-07-03.
///
/// Both are enumerated directly so detection works even before the
/// registry PATH is re-read (and regardless of which layout winget
/// picked on this machine).
pub(crate) fn winget_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Ok(local_app_data) = std::env::var("LOCALAPPDATA") else {
        return dirs;
    };
    let winget = PathBuf::from(&local_app_data)
        .join("Microsoft")
        .join("WinGet");
    dirs.push(winget.join("Links"));
    if let Ok(entries) = std::fs::read_dir(winget.join("Packages")) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                dirs.push(p.join("bin"));
                dirs.push(p);
            }
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The machine PATH always contains System32 on any real Windows
    /// box (CI runners included). If this reads empty, the registry
    /// plumbing is broken and every just-installed CLI is invisible
    /// until app restart again. System32 arriving expanded (the raw
    /// value is `%SystemRoot%\system32`, REG_EXPAND_SZ) also pins that
    /// RegGetValueW expansion works. No blanket "no % anywhere" pin:
    /// entries referencing DELETED variables (a stale `%JAVA_HOME%\bin`)
    /// legitimately stay literal — they just never pass is_file().
    #[test]
    fn registry_path_dirs_sees_the_machine_path() {
        let dirs = registry_path_dirs();
        assert!(
            !dirs.is_empty(),
            "registry PATH read must not be empty on Windows"
        );
        let joined = dirs
            .iter()
            .map(|d| d.to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("system32"),
            "machine PATH must include System32; got:\n{}",
            joined
        );
    }

    #[test]
    fn winget_bin_dirs_include_the_links_farm() {
        if std::env::var("LOCALAPPDATA").is_err() {
            return; // bare container without a profile — nothing to assert
        }
        let joined = winget_bin_dirs()
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("WinGet\\Links"),
            "missing WinGet Links dir: {}",
            joined
        );
    }
}
