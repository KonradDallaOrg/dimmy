//! Who is on a call, and on which devices (Windows). A call app does not
//! tell anyone when the user moves its microphone or speaker from its own
//! device menu: its audio sessions simply show up active on other
//! endpoints. That is how a meeting finds the microphone the call moved to
//! when ours stops delivering (`audio.rs::mic_decision`), and the output
//! the call now plays to (`audio.rs::loopback_target`).
//!
//! "The call" is any process, other than Dimmy, holding an ACTIVE capture
//! session — no list of apps: Teams, Zoom, a browser tab all qualify. Its
//! audio may be played by a relative rather than by itself (the process
//! capturing the mic and the one rendering the call are not always the
//! same), so playback counts when it comes from that process, an ancestor
//! or descendant of it, or a sibling under the same app parent.
//!
//! Names are the endpoints' friendly names, the same strings cpal reports,
//! so they can be handed straight to device resolution.

use std::collections::{HashMap, HashSet};

use windows::core::{Interface, Result};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    eCapture, eRender, AudioSessionStateActive, EDataFlow, IAudioSessionControl2,
    IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// What the call is doing with audio right now.
#[derive(Debug, Default, PartialEq)]
pub struct CallDevices {
    /// Inputs some other process captures from.
    pub inputs: Vec<String>,
    /// Outputs the capturing processes (or their relatives) play to.
    pub outputs: Vec<String>,
}

/// One scan of every active endpoint. Empty when nothing else captures, or
/// when the scan fails: a transient COM error is not a reason to move.
pub fn call_devices() -> CallDevices {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let found = unsafe { scan(std::process::id()) }.unwrap_or_default();
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
    found
}

unsafe fn scan(me: u32) -> Result<CallDevices> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let capture = active_sessions(&enumerator, eCapture)?;
    let callers: HashSet<u32> = capture
        .iter()
        .flat_map(|(_, pids)| pids.iter().copied())
        .filter(|&pid| pid != me)
        .collect();
    let mut found = CallDevices::default();
    if callers.is_empty() {
        return Ok(found);
    }
    for (name, pids) in &capture {
        if pids.iter().any(|p| callers.contains(p)) {
            push_unique(&mut found.inputs, name);
        }
    }
    let tree = ProcessTree::snapshot();
    for (name, pids) in &active_sessions(&enumerator, eRender)? {
        let plays_the_call = pids
            .iter()
            .any(|&p| p != me && callers.iter().any(|&c| tree.related(p, c)));
        if plays_the_call {
            push_unique(&mut found.outputs, name);
        }
    }
    Ok(found)
}

fn push_unique(list: &mut Vec<String>, name: &str) {
    if !list.iter().any(|n| n == name) {
        list.push(name.to_string());
    }
}

/// (endpoint friendly name, PIDs with an active session on it).
unsafe fn active_sessions(
    enumerator: &IMMDeviceEnumerator,
    flow: EDataFlow,
) -> Result<Vec<(String, Vec<u32>)>> {
    let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
    let mut out = Vec::new();
    for i in 0..devices.GetCount()? {
        let Ok(device) = devices.Item(i) else {
            continue;
        };
        let pids = active_pids(&device).unwrap_or_default();
        if pids.is_empty() {
            continue;
        }
        if let Ok(name) = friendly_name(&device) {
            out.push((name, pids));
        }
    }
    Ok(out)
}

unsafe fn active_pids(device: &IMMDevice) -> Result<Vec<u32>> {
    let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
    let sessions = manager.GetSessionEnumerator()?;
    let mut pids = Vec::new();
    for i in 0..sessions.GetCount()? {
        let Ok(control) = sessions.GetSession(i) else {
            continue;
        };
        if control.GetState().ok() != Some(AudioSessionStateActive) {
            continue;
        }
        let Ok(control) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        let pid = control.GetProcessId().unwrap_or(0);
        if pid != 0 {
            pids.push(pid);
        }
    }
    Ok(pids)
}

unsafe fn friendly_name(device: &IMMDevice) -> Result<String> {
    let store = device.OpenPropertyStore(STGM_READ)?;
    // windows-rs frees the PROPVARIANT (PropVariantClear) when it drops.
    let value = store.GetValue(&PKEY_Device_FriendlyName)?;
    let name = value.Anonymous.Anonymous.Anonymous.pwszVal.to_string();
    name.map_err(|_| windows::Win32::Foundation::E_FAIL.into())
}

/// Parent links of every process, from one ToolHelp snapshot.
struct ProcessTree {
    parent: HashMap<u32, u32>,
    shells: HashSet<u32>,
}

/// Processes that parent unrelated apps: being their child says nothing
/// about belonging to the call.
const SHELL_EXES: [&str; 5] = [
    "explorer.exe",
    "svchost.exe",
    "services.exe",
    "sihost.exe",
    "runtimebroker.exe",
];

impl ProcessTree {
    fn snapshot() -> Self {
        let mut tree = ProcessTree {
            parent: HashMap::new(),
            shells: HashSet::new(),
        };
        let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return tree;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
        while more {
            tree.parent
                .insert(entry.th32ProcessID, entry.th32ParentProcessID);
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            if SHELL_EXES.contains(&exe.as_str()) {
                tree.shells.insert(entry.th32ProcessID);
            }
            more = unsafe { Process32NextW(snap, &mut entry) }.is_ok();
        }
        unsafe {
            let _ = CloseHandle(snap);
        }
        tree
    }

    fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut cur = pid;
        for _ in 0..6 {
            match self.parent.get(&cur) {
                Some(&p) if p != 0 && p != cur && !self.shells.contains(&p) => {
                    chain.push(p);
                    cur = p;
                }
                _ => break,
            }
        }
        chain
    }

    /// Same process, ancestor/descendant, or children of the same app
    /// parent (never of a shell process).
    fn related(&self, a: u32, b: u32) -> bool {
        if a == b {
            return true;
        }
        let (up_a, up_b) = (self.ancestors(a), self.ancestors(b));
        up_a.contains(&b) || up_b.contains(&a) || up_a.iter().any(|p| up_b.contains(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(links: &[(u32, u32)], shells: &[u32]) -> ProcessTree {
        ProcessTree {
            parent: links.iter().copied().collect(),
            shells: shells.iter().copied().collect(),
        }
    }

    #[test]
    fn a_helper_playing_for_the_app_that_holds_the_mic_is_the_call() {
        // explorer(1) → ms-teams(10) → webview(11), webview(12)
        let t = tree(&[(10, 1), (11, 10), (12, 10)], &[1]);
        assert!(t.related(11, 10), "child renders for the capturing parent");
        assert!(t.related(10, 11), "parent renders for the capturing child");
        assert!(t.related(11, 12), "siblings under the app");
    }

    #[test]
    fn apps_that_only_share_the_shell_are_not_the_call() {
        // explorer(1) → teams(10), spotify(20)
        let t = tree(&[(10, 1), (20, 1)], &[1]);
        assert!(!t.related(20, 10));
    }
}
