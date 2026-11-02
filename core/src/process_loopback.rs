//! System audio on Windows through PROCESS loopback
//! (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, Windows 10 2004+).
//!
//! Endpoint loopback copies what leaves ONE output device: the default one
//! at the moment the meeting started. Teams lets the user move its speaker
//! to another device mid-call without touching the Windows default, and
//! from then on the copy is of a device nobody plays to: the meeting keeps
//! recording, the participants' side is silence (2026-11-02, a switch in
//! Teams' own device menu).
//!
//! Process loopback asks Windows for what PROCESSES play instead, before it
//! is routed to any device. We capture every process except Dimmy's own
//! tree (`PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` on our PID):
//! the same "all system sound" the endpoint loopback gave, minus Dimmy's
//! own chimes, on any output — measured here: a tone played on a speaker
//! that was NOT the default output came through at its exact amplitude.
//!
//! The format is ours to choose and Windows converts to it
//! (`AUTOCONVERTPCM`): float, stereo, `MEETING_CANONICAL_RATE`. The output
//! device's rate (16 kHz hands-free, 44.1 kHz Jabra, 48 kHz speakers) never
//! reaches the meeting. Unlike endpoint loopback the stream is also
//! continuous: silence arrives as silence, not as missing packets.
//!
//! Activation fails on Windows builds without the API; the caller then
//! falls back to endpoint loopback, so an old Windows records exactly as
//! before.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref, HRESULT};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IAgileObject, IAgileObject_Impl, BLOB, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use crate::audio::MEETING_CANONICAL_RATE;

const CHANNELS: u16 = 2;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// 200 ms of buffer, in 100 ns units: room for the capture thread to be
/// late without Windows dropping audio.
const BUFFER_DURATION_HNS: i64 = 2_000_000;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
const WAIT_SLICE_MS: u32 = 200;
const DIAG_EVERY: Duration = Duration::from_secs(5);

/// Receives each captured packet as mono samples at `MEETING_CANONICAL_RATE`.
pub type MonoSink = Box<dyn FnMut(&mut Vec<f32>) + Send>;

/// A running capture. Dropping it stops the capture and joins its thread.
pub struct ProcessLoopback {
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ProcessLoopback {
    /// The capture stopped on an error (it also raised AUDIO_STREAM_DEAD).
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

impl Drop for ProcessLoopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Open the capture and start delivering to `sink`. `Err` when Windows
/// cannot give us a process-loopback client; nothing is left running then.
pub fn start(sink: MonoSink) -> Result<ProcessLoopback, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let failed = Arc::new(AtomicBool::new(false));
    let (stop_worker, failed_worker) = (stop.clone(), failed.clone());
    let worker = std::thread::Builder::new()
        .name("dimmy-process-loopback".into())
        .spawn(move || run(stop_worker, failed_worker, ready_tx, sink))
        .map_err(|e| format!("spawn: {e}"))?;
    match ready_rx.recv_timeout(ACTIVATION_TIMEOUT + Duration::from_secs(1)) {
        Ok(Ok(())) => Ok(ProcessLoopback {
            stop,
            failed,
            worker: Some(worker),
        }),
        Ok(Err(e)) => {
            let _ = worker.join();
            Err(e)
        }
        Err(_) => {
            // The thread is stuck inside a COM call; it sees the flag the
            // moment it returns, and owns nothing we need back.
            stop.store(true, Ordering::Relaxed);
            Err("activation did not complete".into())
        }
    }
}

fn run(
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
    mut sink: MonoSink,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    match unsafe { open() } {
        Ok(capture) => {
            let _ = ready.send(Ok(()));
            if let Err(e) = unsafe { pump(&capture, &stop, &mut sink) } {
                failed.store(true, Ordering::Relaxed);
                crate::audio::notify_capture_failed("process_loopback", &e);
            }
            unsafe {
                let _ = capture.client.Stop();
                let _ = CloseHandle(capture.event);
            }
        }
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    }
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
}

struct Capture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activation(Mutex<Option<mpsc::Sender<windows::core::Result<IAudioClient>>>>);

impl IAgileObject_Impl for Activation_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activation_Impl {
    fn ActivateCompleted(
        &self,
        operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let mut hr = HRESULT(0);
        let mut unknown = None;
        let result = unsafe { operation.ok()?.GetActivateResult(&mut hr, &mut unknown) }
            .and_then(|()| hr.ok())
            .and_then(|()| unknown.ok_or_else(|| windows::core::Error::from(E_FAIL)))
            .and_then(|u| u.cast::<IAudioClient>());
        if let Some(tx) = self.0.lock().ok().and_then(|mut slot| slot.take()) {
            let _ = tx.send(result);
        }
        Ok(())
    }
}

unsafe fn open() -> Result<Capture, String> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: std::process::id(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // ManuallyDrop: windows-rs drops a PROPVARIANT with PropVariantClear,
    // which would CoTaskMemFree the blob, i.e. `params` on this stack:
    // heap corruption, measured (0xC0000374).
    let activation = std::mem::ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &mut params as *mut _ as *mut u8,
                    },
                },
            }),
        },
    });
    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Activation(Mutex::new(Some(tx))).into();
    // `params` and `activation` must outlive the completion: both stay on
    // this frame until `recv_timeout` returns.
    let _operation = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&*activation),
        &handler,
    )
    .map_err(|e| format!("ActivateAudioInterfaceAsync: {e}"))?;
    let client = rx
        .recv_timeout(ACTIVATION_TIMEOUT)
        .map_err(|_| "activation timed out".to_string())?
        .map_err(|e| format!("activation: {e}"))?;

    let block_align = CHANNELS * 4;
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: CHANNELS,
        nSamplesPerSec: MEETING_CANONICAL_RATE,
        nAvgBytesPerSec: MEETING_CANONICAL_RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            BUFFER_DURATION_HNS,
            0,
            &format,
            None,
        )
        .map_err(|e| format!("Initialize: {e}"))?;
    let event = CreateEventW(None, false, false, None).map_err(|e| format!("CreateEvent: {e}"))?;
    let opened = (|| {
        client.SetEventHandle(event)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok::<_, windows::core::Error>(capture)
    })();
    match opened {
        Ok(capture) => Ok(Capture {
            client,
            capture,
            event,
        }),
        Err(e) => {
            let _ = CloseHandle(event);
            Err(format!("start: {e}"))
        }
    }
}

unsafe fn pump(capture: &Capture, stop: &AtomicBool, sink: &mut MonoSink) -> Result<(), String> {
    let started = Instant::now();
    let mut total_frames: u64 = 0;
    let mut rate_checked = false;
    let mut diag_at = Instant::now();
    let mut diag_peak = 0.0f32;
    let mut diag_gaps = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if WaitForSingleObject(capture.event, WAIT_SLICE_MS) != WAIT_OBJECT_0 {
            continue;
        }
        loop {
            let next = capture
                .capture
                .GetNextPacketSize()
                .map_err(|e| format!("GetNextPacketSize: {e}"))?;
            if next == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            capture
                .capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                .map_err(|e| format!("GetBuffer: {e}"))?;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                diag_gaps += 1;
            }
            let mut mono = if silent || data.is_null() {
                vec![0.0f32; frames as usize]
            } else {
                let interleaved = std::slice::from_raw_parts(
                    data as *const f32,
                    frames as usize * CHANNELS as usize,
                );
                downmix(interleaved, CHANNELS as usize)
            };
            capture
                .capture
                .ReleaseBuffer(frames)
                .map_err(|e| format!("ReleaseBuffer: {e}"))?;
            assert_eq!(mono.len(), frames as usize, "one mono sample per frame");
            diag_peak = mono.iter().fold(diag_peak, |m, s| m.max(s.abs()));
            total_frames += frames as u64;
            sink(&mut mono);
        }
        if !rate_checked {
            let elapsed = started.elapsed().as_secs_f64();
            if let Some(measured) =
                crate::audio::reconcile_loopback_rate(MEETING_CANONICAL_RATE, total_frames, elapsed)
            {
                crate::audio::report_loopback_rate_mismatch(
                    MEETING_CANONICAL_RATE,
                    measured,
                    total_frames,
                    elapsed,
                );
                rate_checked = true;
            } else if elapsed >= 30.0 {
                rate_checked = true;
            }
        }
        if diag_at.elapsed() >= DIAG_EVERY {
            crate::log(&format!(
                "[Audio] Loopback diag (process): total_samples={}, last_5s_peak={:.4}, discontinuities={} (>0.001 means signal, ~0 means silence)",
                total_frames, diag_peak, diag_gaps
            ));
            diag_at = Instant::now();
            diag_peak = 0.0;
            diag_gaps = 0;
        }
    }
    Ok(())
}

/// Interleaved frames → mono by averaging (the endpoint-loopback path does
/// the same, see `build_secondary_stream`). A non-finite sample from the
/// engine becomes silence instead of reaching the mix.
fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    assert!(channels > 0, "downmix: channels must be > 0");
    assert_eq!(interleaved.len() % channels, 0, "downmix: partial frame");
    interleaved
        .chunks_exact(channels)
        .map(|frame| {
            let avg = frame.iter().sum::<f32>() / channels as f32;
            if avg.is_finite() {
                avg.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_each_frame() {
        assert_eq!(downmix(&[0.5, 0.1, -0.2, -0.4], 2), vec![0.3, -0.3]);
    }

    #[test]
    fn downmix_turns_non_finite_into_silence_and_clamps() {
        assert_eq!(
            downmix(&[f32::NAN, 0.0, f32::INFINITY, 1.0, 3.0, 3.0], 2),
            vec![0.0, 0.0, 1.0]
        );
    }
}
