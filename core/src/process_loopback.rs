//! Per-process audio loopback capture using the Windows
//! `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` API.
//!
//! Why this exists: standard WASAPI loopback captures whatever is
//! routed to a specific OUTPUT device. When a meeting app (Teams,
//! Zoom, Discord) puts a Bluetooth device into HFP/SCO profile for
//! its bidirectional voice channel, the audio bypasses the normal
//! WASAPI render endpoint entirely — loopback on the A2DP "Cuffie"
//! device returns silence and loopback on the HFP "Cuffia auricolare"
//! device isn't supported by most BT chipsets. Net result: no system
//! audio capture during BT meetings.
//!
//! Process loopback solves this at the source: we ask Windows to
//! capture the audio THE PROCESS ITSELF is producing, before it goes
//! to any device. Routing is irrelevant — Bluetooth, speakers,
//! virtual cable, the audio is the same. This is what OBS Studio
//! "Application Audio Capture" uses, what Notion AI Meeting Notes
//! uses, and what Microsoft documents as the recommended approach
//! for cross-device meeting capture on Win11 22H2+.
//!
//! ## API surface (this module)
//!
//! - [`list_meeting_processes`]: enumerate running processes that
//!   look like meeting apps (Teams, Zoom, Slack, Discord, browser
//!   tabs in Meet/Webex/etc.). Returns Vec<(pid, name)> for the UI
//!   to populate a dropdown.
//! - [`auto_detect_meeting_pid`]: pick one PID heuristically — the
//!   most-recently-started meeting app, falling back to any audio-
//!   producing process. Used when the user wants "Auto" mode.
//! - [`spawn_process_capture`]: open a process loopback stream and
//!   spawn a worker that writes captured PCM into the shared audio
//!   buffer (and AEC ref ring if Mix mode is active). Returns a
//!   stop-handle the caller drops to terminate the capture.
//!
//! ## Implementation status
//!
//! Phase 5 lands the SCAFFOLDING and process enumeration. The actual
//! WASAPI ProcessLoopback activation (`ActivateAudioInterfaceAsync`
//! with `AUDIOCLIENT_ACTIVATION_PARAMS::ProcessLoopbackParams`) is a
//! ~200-line block of unsafe COM + an `IActivateAudioInterfaceCompletionHandler`
//! impl, landing in the next commit. For now `spawn_process_capture`
//! returns Err and the audio.rs path falls back to standard loopback.
//!
//! ## Platform gating
//!
//! Windows-only. Stub on Mac/Linux returns empty enumeration and an
//! error from `spawn_process_capture`; the audio.rs caller treats
//! that as "not available, use default loopback".

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    /// Hardcoded list of process executables we recognize as meeting
    /// apps. Lowercased for comparison. Order matters for the
    /// auto-detect heuristic — the FIRST match wins, so listing the
    /// most likely first (Teams) means it's preferred when multiple
    /// run simultaneously. Add new ones as users report them.
    const KNOWN_MEETING_EXES: &[&str] = &[
        "ms-teams.exe",
        "teams.exe",
        "zoom.exe",
        "cpthost.exe", // Zoom helper
        "discord.exe",
        "slack.exe",
        "webex.exe",
        "googlemeet.exe",
        "skype.exe",
        "whereby.exe",
    ];

    /// Public process descriptor for UI consumption.
    #[derive(Clone, Debug)]
    pub struct MeetingProcess {
        pub pid: u32,
        pub name: String,
    }

    /// Enumerate running processes whose executable name matches a
    /// known meeting-app pattern. Returns the list sorted by name for
    /// stable UI display. Errors during enumeration produce an empty
    /// vec rather than propagating — non-essential diagnostic.
    pub fn list_meeting_processes() -> Vec<MeetingProcess> {
        let mut out = Vec::new();
        unsafe {
            let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
                Ok(h) => h,
                Err(e) => {
                    crate::log(&format!("[ProcLoopback] snapshot failed: {}", e));
                    return out;
                }
            };
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut entry).is_ok() {
                loop {
                    let name_len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
                    let lc = name.to_ascii_lowercase();
                    if KNOWN_MEETING_EXES.iter().any(|e| *e == lc) {
                        out.push(MeetingProcess {
                            pid: entry.th32ProcessID,
                            name,
                        });
                    }
                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
        out.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        out
    }

    /// Pick one PID heuristically when the user has set the loopback
    /// target to "auto". Strategy: take the first match from
    /// list_meeting_processes (which is sorted alphabetically — gives
    /// stable behaviour). If no meeting app is running, returns None;
    /// the caller falls back to default-output loopback.
    pub fn auto_detect_meeting_pid() -> Option<u32> {
        let candidates = list_meeting_processes();
        if candidates.is_empty() {
            crate::log("[ProcLoopback] auto-detect: no known meeting apps running");
            return None;
        }
        let pick = &candidates[0];
        crate::log(&format!(
            "[ProcLoopback] auto-detect picked '{}' (pid={}) out of {} candidates",
            pick.name,
            pick.pid,
            candidates.len()
        ));
        Some(pick.pid)
    }

    /// Stop handle returned by spawn_process_capture. Dropping it
    /// signals the worker to release the WASAPI capture client and
    /// exit cleanly.
    pub struct ProcessCaptureHandle {
        pub stop: Arc<AtomicBool>,
    }

    impl Drop for ProcessCaptureHandle {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Spawn a worker thread that captures audio from `pid` via the
    /// WASAPI ProcessLoopback API and writes mono f32 samples to the
    /// shared `output_buffer`. If `aec_ref_ring` is provided (Mix
    /// mode), samples are also pushed to the AEC reference ring so
    /// the speaker→mic echo cancellation continues to work even when
    /// audio comes from process capture instead of device loopback.
    ///
    /// PHASE 5 STUB: returns Err for now. The real ActivateAudioInterfaceAsync
    /// + IActivateAudioInterfaceCompletionHandler implementation
    /// lands in the next commit. Caller must check the result and
    /// fall back to default loopback on error.
    pub fn spawn_process_capture(
        _pid: u32,
        _output_buffer: Arc<Mutex<Vec<f32>>>,
        _aec_ref_ring: Option<Arc<Mutex<Vec<f32>>>>,
    ) -> Result<ProcessCaptureHandle, String> {
        Err("process loopback capture not yet implemented (scaffolding only)".to_string())
    }
}

#[cfg(target_os = "windows")]
pub use windows_impl::*;

// ── Stub for non-Windows platforms ──────────────────────────────────

#[cfg(not(target_os = "windows"))]
#[derive(Clone, Debug)]
pub struct MeetingProcess {
    pub pid: u32,
    pub name: String,
}

#[cfg(not(target_os = "windows"))]
pub fn list_meeting_processes() -> Vec<MeetingProcess> {
    Vec::new()
}

#[cfg(not(target_os = "windows"))]
pub fn auto_detect_meeting_pid() -> Option<u32> {
    None
}

#[cfg(not(target_os = "windows"))]
pub struct ProcessCaptureHandle;

#[cfg(not(target_os = "windows"))]
pub fn spawn_process_capture(
    _pid: u32,
    _output_buffer: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
    _aec_ref_ring: Option<std::sync::Arc<std::sync::Mutex<Vec<f32>>>>,
) -> Result<ProcessCaptureHandle, String> {
    Err("process loopback only available on Windows".to_string())
}
