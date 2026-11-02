//! Wakes the audio thread the moment the audio hardware picture changes,
//! so device follow reacts to the event instead of waiting for a timer.
//!
//! What counts as a change:
//! - a new default input or output, a device added, removed, enabled or
//!   disabled, a device's format (sample rate) changing — Windows
//!   `IMMNotificationClient`, macOS CoreAudio property listeners;
//! - Windows only: an app starting or stopping to use a microphone — the
//!   privacy registry key behind the tray's microphone icon, the same
//!   source `MicUsageWatcher.cs` uses (measured ~200 ms ahead of a 4 Hz
//!   poll on every edge, Teams and Chrome alike, 2026-09-25). It names no
//!   device, it only says WHEN to look at who captures from which one.
//!
//! The callback runs on an OS thread and must only signal: the audio thread
//! does all the reading and deciding. Watching lasts as long as the value
//! returned by `watch`, which the audio thread keeps for one recording.
//! The heartbeat stays as the net for anything an event misses (Linux has
//! no watcher at all and relies on it).

/// Called on an OS notification thread. Must not block.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

pub use imp::{watch, DeviceEvents};

#[cfg(target_os = "windows")]
mod imp {
    use super::Wake;
    use std::thread::JoinHandle;
    use windows::core::{implement, w, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, PROPERTYKEY, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::{
        EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
        MMDeviceEnumerator, PKEY_AudioEngine_DeviceFormat, DEVICE_STATE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY,
        REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, REG_NOTIFY_THREAD_AGNOSTIC,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE,
    };

    const MIC_PRIVACY_KEY: PCWSTR = w!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone"
    );

    #[implement(IMMNotificationClient)]
    struct Endpoints(Wake);

    impl IMMNotificationClient_Impl for Endpoints_Impl {
        fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
            (self.0)();
            Ok(())
        }
        fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
            (self.0)();
            Ok(())
        }
        fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
            (self.0)();
            Ok(())
        }
        fn OnDefaultDeviceChanged(
            &self,
            _: EDataFlow,
            _: ERole,
            _: &PCWSTR,
        ) -> windows::core::Result<()> {
            (self.0)();
            Ok(())
        }
        fn OnPropertyValueChanged(
            &self,
            _: &PCWSTR,
            key: &PROPERTYKEY,
        ) -> windows::core::Result<()> {
            // Endpoint properties change all the time (names, icons, jack
            // info); only the format is ours to react to.
            if *key == PKEY_AudioEngine_DeviceFormat {
                (self.0)();
            }
            Ok(())
        }
    }

    pub struct DeviceEvents {
        endpoints: Option<(IMMDeviceEnumerator, IMMNotificationClient)>,
        mic_usage: Option<(HANDLE, JoinHandle<()>)>,
    }

    impl Drop for DeviceEvents {
        fn drop(&mut self) {
            if let Some((enumerator, client)) = self.endpoints.take() {
                let _ = unsafe { enumerator.UnregisterEndpointNotificationCallback(&client) };
            }
            if let Some((stop, thread)) = self.mic_usage.take() {
                unsafe {
                    let _ = SetEvent(stop);
                }
                let _ = thread.join();
                unsafe {
                    let _ = CloseHandle(stop);
                }
            }
        }
    }

    /// Start watching. Each source that cannot be set up is logged and
    /// skipped; the heartbeat still covers what it would have reported.
    pub fn watch(wake: Wake) -> DeviceEvents {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let endpoints = unsafe {
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .and_then(|enumerator| {
                    let client: IMMNotificationClient = Endpoints(wake.clone()).into();
                    enumerator.RegisterEndpointNotificationCallback(&client)?;
                    Ok((enumerator, client))
                })
        };
        if let Err(ref e) = endpoints {
            crate::log(&format!("[Audio] endpoint notifications unavailable: {e}"));
        }
        let mic_usage = watch_mic_usage(wake);
        DeviceEvents {
            endpoints: endpoints.ok(),
            mic_usage,
        }
    }

    fn watch_mic_usage(wake: Wake) -> Option<(HANDLE, JoinHandle<()>)> {
        let mut key = HKEY::default();
        let opened = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                MIC_PRIVACY_KEY,
                None,
                KEY_NOTIFY,
                &mut key,
            )
        };
        if opened.is_err() {
            crate::log(&format!(
                "[Audio] microphone privacy key unavailable: {opened:?}"
            ));
            return None;
        }
        let events = unsafe {
            (
                CreateEventW(None, false, false, None),
                CreateEventW(None, true, false, None),
            )
        };
        let (signal, stop) = match events {
            (Ok(signal), Ok(stop)) => (signal, stop),
            (signal, stop) => {
                unsafe {
                    for h in [signal, stop].into_iter().flatten() {
                        let _ = CloseHandle(h);
                    }
                    let _ = RegCloseKey(key);
                }
                return None;
            }
        };
        // Handles are plain integers; the thread owns key and signal.
        let (key_raw, signal_raw, stop_raw) = (key.0 as isize, signal.0 as isize, stop.0 as isize);
        let thread = std::thread::Builder::new()
            .name("dimmy-mic-usage".into())
            .spawn(move || {
                let key = HKEY(key_raw as *mut _);
                let signal = HANDLE(signal_raw as *mut _);
                let stop = HANDLE(stop_raw as *mut _);
                loop {
                    // Re-armed on every wake; THREAD_AGNOSTIC keeps the
                    // registration alive independent of this thread.
                    let armed = unsafe {
                        RegNotifyChangeKeyValue(
                            key,
                            true,
                            REG_NOTIFY_CHANGE_NAME
                                | REG_NOTIFY_CHANGE_LAST_SET
                                | REG_NOTIFY_THREAD_AGNOSTIC,
                            Some(signal),
                            true,
                        )
                    };
                    if armed.is_err() {
                        crate::log(&format!(
                            "[Audio] microphone privacy watch failed: {armed:?}"
                        ));
                        break;
                    }
                    let woke = unsafe { WaitForMultipleObjects(&[signal, stop], false, INFINITE) };
                    if woke != WAIT_OBJECT_0 {
                        break;
                    }
                    wake();
                }
                unsafe {
                    let _ = RegCloseKey(key);
                    let _ = CloseHandle(signal);
                }
            });
        match thread {
            Ok(thread) => Some((stop, thread)),
            Err(e) => {
                crate::log(&format!("[Audio] microphone privacy watch thread: {e}"));
                unsafe {
                    let _ = RegCloseKey(key);
                    let _ = CloseHandle(signal);
                    let _ = CloseHandle(stop);
                }
                None
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Wake;
    use coreaudio_sys::{
        kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultInputDevice,
        kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, AudioObjectAddPropertyListener,
        AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
        AudioObjectRemovePropertyListener, OSStatus,
    };
    use std::os::raw::c_void;

    const ELEMENT_MAIN: u32 = 0;

    fn address(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: ELEMENT_MAIN,
        }
    }

    unsafe extern "C" fn on_change(
        _object: AudioObjectID,
        _count: u32,
        _addresses: *const AudioObjectPropertyAddress,
        data: *mut c_void,
    ) -> OSStatus {
        let wake = &*(data as *const Wake);
        wake();
        0
    }

    pub struct DeviceEvents {
        /// Boxed so its address, handed to CoreAudio, never moves.
        wake: Box<Wake>,
        listening: Vec<(AudioObjectID, AudioObjectPropertyAddress)>,
    }

    impl Drop for DeviceEvents {
        fn drop(&mut self) {
            let data = &*self.wake as *const Wake as *mut c_void;
            for (object, addr) in &self.listening {
                unsafe {
                    AudioObjectRemovePropertyListener(*object, addr, Some(on_change), data);
                }
            }
        }
    }

    /// Default input/output, the device list, and the sample rate of the
    /// device that is the default input right now (a hands-free flip).
    pub fn watch(wake: Wake) -> DeviceEvents {
        let mut events = DeviceEvents {
            wake: Box::new(wake),
            listening: Vec::new(),
        };
        let system = kAudioObjectSystemObject;
        let mut targets = vec![
            (system, kAudioHardwarePropertyDefaultInputDevice),
            (system, kAudioHardwarePropertyDefaultOutputDevice),
            (system, kAudioHardwarePropertyDevices),
        ];
        if let Some(input) = default_input() {
            targets.push((input, kAudioDevicePropertyNominalSampleRate));
        }
        let data = &*events.wake as *const Wake as *mut c_void;
        for (object, selector) in targets {
            let addr = address(selector);
            let status =
                unsafe { AudioObjectAddPropertyListener(object, &addr, Some(on_change), data) };
            if status == 0 {
                events.listening.push((object, addr));
            } else {
                crate::log(&format!(
                    "[Audio] CoreAudio listener {selector:#x} on {object} failed: {status}"
                ));
            }
        }
        events
    }

    fn default_input() -> Option<AudioObjectID> {
        let addr = address(kAudioHardwarePropertyDefaultInputDevice);
        let mut id: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut id as *mut _ as *mut c_void,
            )
        };
        (status == 0 && id != 0).then_some(id)
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    use super::Wake;

    pub struct DeviceEvents;

    pub fn watch(_wake: Wake) -> DeviceEvents {
        DeviceEvents
    }
}
