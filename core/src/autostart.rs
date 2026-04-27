//! Cross-platform "launch at login" toggle.
//!
//! Thin wrapper around the [`auto-launch`] crate so the native UI
//! (C# WinUI / Swift / GTK4) can flip a single switch without
//! caring which OS-specific mechanism is involved:
//!
//! - **Windows**: writes/removes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Dimmy`.
//!   User-scope (no admin needed). Visible in Task Manager → Startup tab.
//! - **macOS**: writes/removes `~/Library/LaunchAgents/dimmy.plist`.
//! - **Linux**: writes/removes `~/.config/autostart/dimmy.desktop`
//!   (XDG autostart spec — honored by GNOME, KDE, XFCE, …).
//!
//! All three are user-scope, reversible, and survive across reboots.
//!
//! # Re-entrancy
//!
//! `set_enabled(true)` followed by `set_enabled(true)` is a no-op
//! (the underlying crate handles existing entries gracefully).
//! `is_enabled()` is cheap (one filesystem stat or registry read).
//!
//! # Failure mode
//!
//! Every public function returns `Result` so the FFI caller can
//! report a meaningful error to the user (e.g. "cannot write to
//! `~/Library/LaunchAgents/`, check permissions"). The autostart
//! state is never load-bearing for the app's runtime — losing it
//! just means "next reboot, the user has to launch Dimmy manually".

use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use std::sync::OnceLock;

/// Display name used by the OS to label the autostart entry.
/// Stable — changing this string would orphan existing autostart
/// entries on user machines.
const APP_NAME: &str = "Dimmy";

/// Single-process cache. The underlying crate is cheap to construct
/// but resolves the current exe path each time, which involves a
/// syscall. Once-init means the FFI's "is_enabled" hot path stays
/// allocation-free after the first call.
fn auto_launch() -> Result<&'static AutoLaunch, String> {
    static INST: OnceLock<Option<AutoLaunch>> = OnceLock::new();
    INST.get_or_init(build_auto_launch)
        .as_ref()
        .ok_or_else(|| "autostart: failed to resolve current exe path".to_string())
}

/// Build the `AutoLaunch` instance using the builder API. The plain
/// `AutoLaunch::new` constructor has a *different* arity per OS
/// (macOS adds a `use_launch_agent: bool` parameter that Windows /
/// Linux don't have), which would break the build on at least one
/// platform. The builder normalises this — `set_use_launch_agent`
/// is a no-op outside macOS and we always set it `true` to prefer
/// the per-user LaunchAgent over a system-wide LaunchDaemon.
fn build_auto_launch() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let exe_str = exe.to_string_lossy().to_string();
    AutoLaunchBuilder::new()
        .set_app_name(APP_NAME)
        .set_app_path(&exe_str)
        .set_use_launch_agent(true)
        .build()
        .ok()
}

/// Enable or disable autostart. Returns `Err` if the underlying OS
/// operation fails (registry write denied, plist directory missing,
/// etc.). Idempotent: calling `set_enabled(true)` while already
/// enabled is a no-op success.
pub fn set_enabled(want: bool) -> Result<(), String> {
    let al = auto_launch()?;
    let result = if want { al.enable() } else { al.disable() };
    result.map_err(|e| format!("autostart: {}", e))
}

/// Whether the autostart entry is currently present. Returns `false`
/// on any error (we treat "can't tell" as "not enabled" so the UI
/// toggle defaults to off rather than misrepresenting state).
pub fn is_enabled() -> bool {
    auto_launch()
        .and_then(|al| al.is_enabled().map_err(|e| e.to_string()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke: the OnceLock-cached AutoLaunch must resolve. If the
    /// test runner can't determine the current exe path, the whole
    /// suite is busted anyway, so this catches regressions in our
    /// init plumbing rather than environmental issues.
    #[test]
    fn auto_launch_resolves_current_exe() {
        let r = auto_launch();
        assert!(r.is_ok(), "expected exe path to resolve, got {:?}", r);
    }

    /// `is_enabled` must never panic regardless of OS state. This
    /// runs against the real underlying mechanism (registry / plist
    /// / xdg) but the read is harmless.
    #[test]
    fn is_enabled_does_not_panic() {
        let _ = is_enabled();
    }
}
