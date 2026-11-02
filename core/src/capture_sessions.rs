//! Which input devices OTHER processes are capturing from right now
//! (Windows). A call app does not tell anyone when the user moves its
//! microphone from its own device menu; its audio session simply shows up
//! active on another endpoint. That is how a meeting can find the
//! microphone the call moved to when ours stops delivering
//! (`audio.rs::mic_decision`).
//!
//! Names are the endpoints' friendly names, the same strings cpal reports,
//! so they can be handed straight to `resolve_input_device`.

use windows::core::{Interface, Result};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

/// Friendly names of the active capture endpoints on which some process
/// other than Dimmy has an ACTIVE session. Empty when nothing else
/// captures, or when the scan fails (a transient COM error is not a
/// reason to move the microphone).
pub fn devices_captured_by_others() -> Vec<String> {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let found = unsafe { scan(std::process::id()) }.unwrap_or_default();
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
    found
}

unsafe fn scan(me: u32) -> Result<Vec<String>> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
    let mut names = Vec::new();
    for i in 0..devices.GetCount()? {
        let Ok(device) = devices.Item(i) else {
            continue;
        };
        if !captured_by_other(&device, me).unwrap_or(false) {
            continue;
        }
        if let Ok(name) = friendly_name(&device) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    Ok(names)
}

unsafe fn captured_by_other(device: &IMMDevice, me: u32) -> Result<bool> {
    let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
    let sessions = manager.GetSessionEnumerator()?;
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
        if pid != 0 && pid != me {
            return Ok(true);
        }
    }
    Ok(false)
}

unsafe fn friendly_name(device: &IMMDevice) -> Result<String> {
    let store = device.OpenPropertyStore(STGM_READ)?;
    // windows-rs frees the PROPVARIANT (PropVariantClear) when it drops.
    let value = store.GetValue(&PKEY_Device_FriendlyName)?;
    let name = value.Anonymous.Anonymous.Anonymous.pwszVal.to_string();
    name.map_err(|_| windows::Win32::Foundation::E_FAIL.into())
}
