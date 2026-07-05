use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

/// Canonical sample rate the meeting writer + STT pipeline run on.
/// All capture streams (primary mic + secondary loopback) resample
/// to this rate inside their cpal callbacks via LinearResampler.
///
/// Why fixed 48 kHz (decided 2026-05-19 after the BT-disconnect / output
/// device swap incident):
/// - Choosing the primary's native rate as canonical breaks when the
///   primary is BT-HFP (16 kHz) and the secondary is BT-A2DP (48 kHz):
///   downsampling 48 → 16 with a plain LinearResampler aliases badly
///   ("audio sistema è na merda") because there's no anti-alias filter.
/// - A floating canonical (re-derived on each device swap) breaks the
///   WAV writer in meeting.rs, which is bound to ONE rate per session.
/// - 48 kHz is what AEC3 already expects (see aec.rs comment), what
///   WASAPI shared-mode resamples to internally, and matches every
///   modern output device — so resampling is always UPWARD or identity,
///   which linear interpolation handles cleanly (no aliasing).
pub const MEETING_CANONICAL_RATE: u32 = 48_000;

/// Long-dictation guardrail thresholds (seconds of buffered audio).
/// The pill dictation buffer has NO draining worker, so a marathon /
/// forgotten dictation grows unbounded → memory pressure (root of the
/// macOS whole-system freeze, 2026-06-30). At the soft threshold we nudge
/// the user toward Meeting mode; at the hard threshold the host auto-stops
/// (transcribes + pastes what was said) and offers to open it as a meeting.
/// Meeting mode is unaffected: its `audio_buffer` also grows, so the guard
/// is gated on `meeting_is_active() == false` at the call site.
pub const DICTATION_SOFT_WARN_SECS: f64 = 300.0; // 5 min
pub const DICTATION_HARD_STOP_SECS: f64 = 600.0; // 10 min

/// Test-only: incremented on every worker heartbeat tick (the 1 s
/// recv_timeout branch). The guardrail integration test waits for
/// tick DELTAS instead of sleeping fixed wall-clock windows — the
/// sleeps flaked on loaded CI runners. Compiled out of production.
#[cfg(any(test, feature = "test-ffi"))]
pub static HEARTBEAT_TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// True while a meeting is PAUSED. Every append site into
/// `audio_buffer` / `audio_buffer_secondary` (mic callback, loopback
/// callback, PushLoopback worker path, AEC worker output) checks this
/// and SKIPS the append while set — the meeting buffer is append-only
/// for the whole session (cursors, never drained), so a long pause
/// would otherwise accumulate unbounded RAM (~345 MB/h at 48 kHz) for
/// samples the resume cursor-jump discards anyway (gap-skip semantics).
/// Owned by `MeetingSession::{start,pause,resume,stop}` — false during
/// dictation, so the pill path is untouched. Audit 2026-07-02.
pub static MEETING_CAPTURE_GATED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Cheap relaxed read for the capture hot paths.
#[inline]
pub fn meeting_capture_gated() -> bool {
    MEETING_CAPTURE_GATED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pure threshold decision for the long-dictation guardrail. Returns
/// `(emit_soft_warning, emit_hard_stop)` given the current buffer length,
/// the canonical sample rate, and which events already fired this
/// recording. Each event fires at most once (the caller latches the
/// flags). Split out so it can be unit-tested without the audio pipeline.
fn dictation_duration_events(
    buf_len: usize,
    rate: u32,
    soft_warned: bool,
    hard_emitted: bool,
) -> (bool, bool) {
    let secs = buf_len as f64 / (rate.max(1) as f64);
    let warn = !soft_warned && secs >= DICTATION_SOFT_WARN_SECS;
    let stop = !hard_emitted && secs >= DICTATION_HARD_STOP_SECS;
    (warn, stop)
}

/// Stateful linear-interpolation resampler for streaming audio.
/// Used by `build_secondary_stream` so the loopback callback can
/// emit samples at the meeting's canonical rate even when the
/// underlying device's native rate differs (BT-A2DP 48 kHz → laptop
/// speakers 44.1 kHz mid-meeting, or any other endpoint swap).
///
/// Linear is sufficient for speech bandwidth (telephony / Teams /
/// Zoom) — anti-aliasing artifacts above 4 kHz are inaudible for
/// the recap use case. Phase + last-sample state persists across
/// callbacks so chunk boundaries don't introduce clicks.
struct LinearResampler {
    src_rate: u32,
    dst_rate: u32,
    /// Fractional position in source-sample units. Negative values
    /// mean "interpolate against the previous chunk's tail" (held
    /// in `last`). Re-based to subtract `input.len()` after each
    /// chunk so the next call sees a small fractional offset.
    pos: f64,
    /// Last sample of the previous input chunk, used as the "left"
    /// neighbour when `pos` lands before index 0 of the new chunk.
    last: f32,
}

impl LinearResampler {
    fn new(src_rate: u32, dst_rate: u32) -> Self {
        assert!(src_rate > 0 && dst_rate > 0);
        Self {
            src_rate,
            dst_rate,
            pos: 0.0,
            last: 0.0,
        }
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let step = self.src_rate as f64 / self.dst_rate as f64;
        let len_i64 = input.len() as i64;
        loop {
            let i_floor_f = self.pos.floor();
            let i_floor = i_floor_f as i64;
            // Need both input[i_floor] (a) and input[i_floor+1] (b)
            // for linear interpolation. When i_floor+1 falls beyond
            // the current chunk we defer to the next callback.
            if (i_floor + 1) >= len_i64 {
                break;
            }
            let frac = (self.pos - i_floor_f) as f32;
            let a = if i_floor < 0 {
                self.last
            } else {
                input[i_floor as usize]
            };
            // i_floor+1 is guaranteed in [0, len) by the check above.
            let b = if (i_floor + 1) < 0 {
                self.last
            } else {
                input[(i_floor + 1) as usize]
            };
            output.push(a + (b - a) * frac);
            self.pos += step;
        }
        // Re-base position so the next chunk's index 0 is "at" pos-len.
        self.pos -= input.len() as f64;
        self.last = *input.last().unwrap();
    }
}

/// Set true the first time a cpal stream error_callback fires. The
/// flag is reset whenever a fresh stream successfully opens. Used by
/// `dimmy_audio_stream_dead()` so the UI / meeting worker can tell
/// "the mic stream silently died mid-recording" from "no recording
/// in flight" — the smoking-gun pattern for the audio-loss incident
/// 2026-05-19 (Logitech meeting-room device-swap, 36/41 min lost).
///
/// First iteration emits a `audio.stream_error` event and sets this
/// flag; the auto-recovery path (close + reopen on new default
/// endpoint) will land in a follow-up commit. For now the goal is to
/// MAKE THE FAILURE VISIBLE so we can correlate UI reports with the
/// timestamped error and confirm the swap really is what kills the
/// stream.
pub(crate) static AUDIO_STREAM_DEAD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Helper invoked from each cpal error_callback in this module.
/// Logs (with role + sample-format context) and emits an event to the
/// host UI so the failure shows up in the C# / Swift log AND in any
/// future "device lost" diagnostic without polling the flag.
fn notify_audio_stream_error(stream_role: &str, format: &str, err: &cpal::StreamError) {
    let kind = match err {
        cpal::StreamError::DeviceNotAvailable => "device_not_available",
        cpal::StreamError::BackendSpecific { .. } => "backend_specific",
    };
    AUDIO_STREAM_DEAD.store(true, Ordering::Relaxed);
    crate::log(&format!(
        "[Audio] {} stream error ({}): kind={} err={}",
        stream_role, format, kind, err
    ));
    let payload = format!(
        r#"{{"role":"{}","format":"{}","kind":"{}"}}"#,
        stream_role, format, kind
    );
    crate::ffi::emit_event("audio.stream_error", &payload);
}

/// Sample rate (Hz) of the currently active cpal mic stream. Set to
/// the device's native rate when the stream opens, reset to 0 when
/// it closes. Read via `active_mic_sample_rate()` from inside the
/// crate and via `dimmy_get_active_mic_sample_rate` from FFI.
///
/// Lets the macOS `SystemAudioCaptureService` (ScreenCaptureKit
/// loopback) configure SCStream at the SAME rate the mic is running
/// at — without this, mic at 16 kHz (Bluetooth A2DP) + SCStream
/// forced at 48 kHz forces the macOS audio HAL to renegotiate the
/// global mixer, which the user perceives as the OUTPUT device's
/// audio quality dropping during recording. Suspected root cause of
/// the "audio in cuffia non pulito durante un meeting" report on
/// 2026-05-14 (Jabra Evolve 3, BT, meeting-only).
static ACTIVE_MIC_SAMPLE_RATE: AtomicU32 = AtomicU32::new(0);

/// Read the sample rate the cpal mic is currently running at, or 0
/// when no recording is in flight. Crate-internal.
pub fn active_mic_sample_rate() -> u32 {
    ACTIVE_MIC_SAMPLE_RATE.load(Ordering::Relaxed)
}

/// Device-native sample rate of the live cpal mic stream (Hz), or 0
/// when no recording is in flight. Distinct from `ACTIVE_MIC_SAMPLE_RATE`
/// which always returns the canonical post-resample rate (48 kHz). On
/// BT-HFP routes the device rate is 16 kHz; on built-in mic / USB-class
/// audio it's typically 44.1 / 48 kHz. macOS SystemAudioCaptureService
/// needs the device rate to configure SCStream at the actual wire rate
/// — otherwise SCKit either resamples internally (delivering upsampled
/// garbage tagged as 48 kHz) or forces the audio HAL to renegotiate.
static ACTIVE_MIC_DEVICE_RATE: AtomicU32 = AtomicU32::new(0);

/// Read the device-native mic rate, or 0 when no recording is live.
pub fn active_mic_device_rate() -> u32 {
    ACTIVE_MIC_DEVICE_RATE.load(Ordering::Relaxed)
}

/// Externally-supplied sample rate for the macOS / Linux loopback
/// push path (`dimmy_push_loopback_audio`). On Windows the secondary
/// stream is driven by cpal so its rate is read directly from the
/// device config and this override stays 0. On Mac, SCStream's
/// `SCStreamConfiguration.sampleRate` is matched to the live cpal
/// mic rate (typically 16 kHz on BT-HFP, 48 kHz on built-in) — the
/// Swift side stashes that rate here so `secondary_sample_rate()`
/// (read by `dimmy_meeting_start` to size the WAV header for
/// `audio_system.wav`) returns the rate samples will actually arrive
/// at, instead of the historical hardcoded 48 kHz. Without this the
/// WAV header advertises 48 kHz while samples are at 16 kHz, so
/// playback runs at 3× speed and the loopback STT call downsamples
/// against the wrong rate and returns empty. Set via
/// `dimmy_set_loopback_sample_rate` (one-shot, before
/// `dimmy_meeting_start`) and refreshed on every
/// `dimmy_push_loopback_audio` whose `sample_rate` arg is non-zero.
/// Cleared when the audio worker stops, so a subsequent recording
/// re-probes the rate from a clean slate.
static LOOPBACK_SAMPLE_RATE_OVERRIDE: AtomicU32 = AtomicU32::new(0);

/// Stash the loopback sample rate so `secondary_sample_rate()` returns
/// it on the next read. `rate == 0` clears the override (used by the
/// audio worker on Stop).
pub fn set_loopback_sample_rate_override(rate: u32) {
    if rate != 0 {
        assert!(
            (8_000..=192_000).contains(&rate),
            "set_loopback_sample_rate_override: rate {} out of audio range",
            rate
        );
    }
    LOOPBACK_SAMPLE_RATE_OVERRIDE.store(rate, Ordering::Relaxed);
}

/// What the audio thread should capture. `Mic` is the historical default
/// (default input device — built-in mic, headset, etc). `System` opens
/// the default OUTPUT device in WASAPI loopback mode on Windows so we
/// can transcribe what's playing through the speakers (Zoom, Teams,
/// music). `Mix` runs both streams concurrently and sums their samples
/// per channel — useful for meetings where you want both your voice
/// AND the other participants in one transcript.
///
/// Mac + Linux currently fall back to Mic when System / Mix are
/// requested — see docs/dev/system-audio-capture.md for the per-OS
/// implementation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    Mic,
    System,
    Mix,
}

impl AudioSource {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "system" => Self::System,
            "mix" => Self::Mix,
            _ => Self::Mic,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::System => "system",
            Self::Mix => "mix",
        }
    }
}

/// Commands sent to the audio capture thread
pub enum AudioCommand {
    /// Start capture. `device_name`: optional input device override
    /// (mic only — ignored when source is System or Mix-on-Win because
    /// the loopback always targets the default output device).
    Start {
        device_name: Option<String>,
        source: AudioSource,
    },
    Stop,
    /// Push externally-captured loopback samples (macOS/Linux ScreenCaptureKit
    /// or Core Audio tap path). The worker resamples to MEETING_CANONICAL_RATE
    /// when `source_rate != MEETING_CANONICAL_RATE` and appends to
    /// audio_buffer_secondary AND aec_ref_ring at the canonical rate — same
    /// invariant the Windows cpal loopback callback enforces via its own
    /// LinearResampler.
    ///
    /// `source_rate == 0` means the caller doesn't know; the worker falls
    /// back to the override published via `dimmy_set_loopback_sample_rate` /
    /// `LOOPBACK_SAMPLE_RATE_OVERRIDE`, then to MEETING_CANONICAL_RATE
    /// (passthrough). Pre-fix, this variant carried no rate hint and the
    /// handler pushed samples raw — when the macOS tap delivered at any rate
    /// other than 48 kHz the secondary buffer grew at the wrong cadence,
    /// audio_system.ogg/wav got a 48 kHz header on non-48 kHz samples, the
    /// AEC ref ring drained 480-sample frames at the wrong wall-clock rate,
    /// and the mix track's per-sample align broke.
    PushLoopback(Vec<f32>, u32),
}

/// Cached input-device names, refreshed by every LIVE enumeration
/// (`list_input_devices`) and seeded once at init. Exists so that
/// `dimmy_get_config_json` — called on hot paths like the post-meeting
/// recap kickoff — never touches the audio HAL: on macOS 26.x a CoreAudio
/// enumeration issued while the meeting's streams + system-audio tap are
/// tearing down can block FOREVER on the HAL's process-global lock, which
/// froze the recap of every >10-min meeting (burned 2026-07-03).
static INPUT_DEVICES_CACHE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Non-blocking snapshot of the last enumerated input-device list.
/// Config-JSON consumers read this; the pickers that must be current
/// call `list_input_devices()` (user-driven, safe timing) which
/// refreshes it.
pub fn cached_input_devices() -> Vec<String> {
    INPUT_DEVICES_CACHE
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn set_cached_input_devices_for_test(devices: Vec<String>) {
    if let Ok(mut c) = INPUT_DEVICES_CACHE.lock() {
        *c = devices;
    }
}

/// List available input device names (LIVE HAL enumeration; refreshes
/// the cache above).
///
/// Returns an empty list when `DIMMY_FORCE_CPU=1` is set: that flag marks
/// a headless/CI environment where Windows WASAPI's `EnumAudioEndpoints`
/// raises a STATUS_ACCESS_VIOLATION (0xc0000005) instead of returning
/// `Err` — Rust's panic machinery cannot catch Windows SEH faults, so we
/// must avoid the call entirely. Real users never set this flag, so the
/// production code path is unchanged.
pub fn list_input_devices() -> Vec<String> {
    if std::env::var_os("DIMMY_FORCE_CPU").is_some() {
        return Vec::new();
    }
    let host = cpal::default_host();
    let devices: Vec<String> = host
        .input_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    if let Ok(mut c) = INPUT_DEVICES_CACHE.lock() {
        *c = devices.clone();
    }
    devices
}

/// Spawn a dedicated audio capture thread.
/// Returns a sender to control the thread.
/// Captured samples are written to the shared buffer.
/// `input_gain` is shared so it can be updated at runtime (0.0-2.0, default 1.0).
pub fn spawn_audio_thread(
    buffer: Arc<Mutex<Vec<f32>>>,
    buffer_secondary: Arc<Mutex<Vec<f32>>>,
    input_gain: Arc<std::sync::atomic::AtomicU32>,
    loopback_gain: Arc<std::sync::atomic::AtomicU32>,
) -> mpsc::Sender<AudioCommand> {
    let (tx, rx) = mpsc::channel::<AudioCommand>();

    // ── AEC plumbing ─────────────────────────────────────────────
    // Mix mode routes mic and loopback through these per-stream rings.
    // The dimmy-aec worker thread drains 480-sample frames from each in
    // lockstep, runs WebRTC AEC3, and pushes cleaned mic samples to the
    // primary `buffer`. In Mic-only / System-only modes the rings stay
    // empty (callbacks write directly to `buffer`) so the worker idles
    // at zero CPU.
    let aec_mic_ring: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let aec_ref_ring: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let aec_shutdown: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _aec_handle = crate::aec::spawn_aec_thread(
        aec_mic_ring.clone(),
        aec_ref_ring.clone(),
        buffer.clone(),
        aec_shutdown.clone(),
    );

    thread::spawn(
        #[allow(unused_assignments, unused_variables)]
        move || {
            let host = cpal::default_host();
            // streams are held alive to keep recording; dropping/replacing
            // stops/starts. `Mix` mode keeps two streams alive in parallel.
            let mut streams: Vec<cpal::Stream> = Vec::new();

            // Last successful Start params — used by the device-change
            // auto-recovery path. When the cpal stream error_callback
            // sets AUDIO_STREAM_DEAD (e.g. user unplugged BT headset
            // mid-meeting), the next idle tick of this loop synthesises
            // a fresh Start using these params so the recording picks
            // up on the new default endpoint without losing the WAV
            // writer in meeting.rs. `None` until the first Start, and
            // reset to `None` on explicit Stop so a stray dead-flag
            // can't reopen a stream the user no longer wants.
            let mut last_start_params: Option<(Option<String>, AudioSource)> = None;
            // Name of the output device the WASAPI loopback is currently bound
            // to (the default output at the last Start). The 1 s heartbeat
            // compares this to the live default output; if they diverge (user
            // replugged headphones → default flipped) we rebind the loopback so
            // system audio follows the real output instead of capturing silence
            // on the stale device. `None` when no loopback is active (Mic-only,
            // or before the first Start) and on every non-Windows platform.
            let mut bound_loopback_name: Option<String> = None;
            // Name of the input device the mic stream is currently bound to
            // (the default input at the last Start, when Start was called
            // with device_name=None). The 1 s heartbeat compares this to
            // the live default input; if they diverge (user replugged
            // headphones / Jabra connect / AirPods switch) AND the user
            // hadn't pinned a device, we rebuild the mic stream on the
            // new endpoint so the meeting follows the hardware swap
            // without the user noticing. `None` when the user pinned a
            // device by name, or before the first Start.
            let mut bound_input_name: Option<String> = None;
            // True when the next AudioCommand::Start came from the
            // auto-recovery branch (not from a fresh user request).
            // The Start body must NOT clear the buffers in that case:
            // meeting.rs is tracking `samples_written` against the
            // continuously growing buffer, and clearing it mid-stream
            // would make `samples_written` larger than `buffer.len()`,
            // permanently freezing all WAV writes for the rest of the
            // meeting. Resampler state in the new callback is created
            // fresh; new samples are appended in coherent time order.
            let mut is_recovery_start = false;

            // Loopback resampler state. The external push path delivers
            // samples at whatever rate the macOS tap / ScreenCaptureKit /
            // Linux pulse handed us; we resample to MEETING_CANONICAL_RATE
            // here so audio_buffer_secondary and aec_ref_ring always grow
            // at the same cadence as the (already-canonicalised) primary
            // mic buffer. Without this, the meeting worker's
            // align_secondary invariant + per-sample mix walk against
            // mis-rated samples and produce wrong-speed playback or
            // silence-padded gaps. Identity (passthrough) when
            // source_rate == MEETING_CANONICAL_RATE; rebuilt fresh on each
            // source-rate change (device swap, BT renegotiation).
            let mut loopback_resampler: Option<LinearResampler> = None;
            let mut loopback_last_src_rate: u32 = 0;

            // Diagnostic counters for the periodic [Audio/loopback] tick.
            // Cumulative input + output sample counts let "is the tap even
            // delivering?" and "did the resampler do its job?" be answered
            // from dimmy.log alone, without a debugger or core dump.
            let mut loopback_diag_last_log = std::time::Instant::now();
            let mut loopback_diag_in_samples: u64 = 0;
            let mut loopback_diag_out_samples: u64 = 0;
            let mut loopback_diag_pushes: u64 = 0;
            let mut loopback_diag_peak_abs: f32 = 0.0;

            // Long-dictation guardrail latches (reset on each fresh Start).
            let mut dict_soft_warned = false;
            let mut dict_hard_emitted = false;

            // Event loop: wait for commands, with a 1 s timeout that
            // doubles as the heartbeat for device-change auto-recovery.
            loop {
                let cmd = match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                    Ok(c) => c,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Test-only heartbeat counter: lets the guardrail
                        // integration test key its assertions to OBSERVED
                        // ticks instead of wall-clock sleeps (which flaked
                        // on loaded runners). Compiled out of production.
                        #[cfg(any(test, feature = "test-ffi"))]
                        HEARTBEAT_TICKS.fetch_add(1, Ordering::Relaxed);
                        // Long-dictation guardrail. The pill dictation buffer
                        // has no draining worker, so a marathon dictation grows
                        // unbounded → memory pressure (macOS whole-system freeze,
                        // 2026-06-30). Gated on an actual LIVE DICTATION, not
                        // just meeting-inactive: `dimmy_meeting_stop` takes the
                        // MEETING option before the buffers are cleared, so a
                        // meeting-inactive check alone saw a stale >10-min
                        // meeting buffer during every long-meeting stop and
                        // fired a spurious dictation.max_duration (burned
                        // 2026-07-03). The recording flag is set only by
                        // dictation starts, so it is false for the entire
                        // meeting lifecycle including the stop window. The
                        // meeting check stays as negative-space belt.
                        if crate::ffi::dictation_recording_active()
                            && !crate::ffi::meeting_is_active()
                        {
                            let buf_len = buffer.lock().map(|b| b.len()).unwrap_or(0);
                            let (warn, stop) = dictation_duration_events(
                                buf_len,
                                MEETING_CANONICAL_RATE,
                                dict_soft_warned,
                                dict_hard_emitted,
                            );
                            if warn {
                                dict_soft_warned = true;
                                crate::ffi::emit_event("dictation.long_warning", "{}");
                            }
                            if stop {
                                dict_hard_emitted = true;
                                crate::log("[Audio] dictation exceeded hard limit (10 min) — emitting dictation.max_duration for host to stop");
                                crate::ffi::emit_event("dictation.max_duration", "{}");
                            }
                        }
                        let dead = AUDIO_STREAM_DEAD.load(Ordering::Relaxed);
                        // Default-output-follow (Windows): cpal raises no error
                        // when the default output merely CHANGES (the old device
                        // is still alive, just no longer default), so the
                        // dead-flag path can't catch a replug. Compare the live
                        // default output to the device the loopback is bound to.
                        let output_changed = if !dead {
                            match last_start_params {
                                Some((_, src)) if bound_loopback_name.is_some() => {
                                    loopback_should_follow_default(
                                        src,
                                        bound_loopback_name.as_deref(),
                                        current_default_output_name(&host).as_deref(),
                                    )
                                }
                                _ => false,
                            }
                        } else {
                            false
                        };
                        let input_changed = if !dead {
                            match last_start_params {
                                Some((ref dn, _)) => input_should_follow_default(
                                    dn.as_deref(),
                                    bound_input_name.as_deref(),
                                    current_default_input_name(&host).as_deref(),
                                ),
                                None => false,
                            }
                        } else {
                            false
                        };
                        let default_changed = output_changed || input_changed;
                        if dead || default_changed {
                            if let Some((ref dn, src)) = last_start_params {
                                let trigger = if dead {
                                    "stream_dead"
                                } else if input_changed {
                                    "default_input_changed"
                                } else {
                                    "default_output_changed"
                                };
                                crate::log(&format!(
                                    "[Audio] device-change auto-recovery: restarting streams (device={:?} source={:?} trigger={}) — preserving buffers",
                                    dn, src, trigger
                                ));
                                crate::ffi::emit_event(
                                    "audio.device_change_recovery",
                                    &format!(r#"{{"trigger":"{}"}}"#, trigger),
                                );
                                streams.clear();
                                AUDIO_STREAM_DEAD.store(false, Ordering::Relaxed);
                                is_recovery_start = true;
                                AudioCommand::Start {
                                    device_name: dn.clone(),
                                    source: src,
                                }
                            } else {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                };
                match cmd {
                    AudioCommand::Start {
                        device_name,
                        source,
                    } => {
                        crate::log(&format!(
                            "[Audio] Start command received, device_name={:?}, source={:?}",
                            device_name, source
                        ));
                        // Remember so device-change auto-recovery can
                        // resynthesise a Start with the same intent
                        // after a mid-recording disconnect.
                        last_start_params = Some((device_name.clone(), source));
                        // Drop any prior streams before re-opening.
                        streams.clear();

                        // Resolve which devices to open based on source. For Mix we open
                        // both mic + system. For System on Mac/Linux we currently fall
                        // back to Mic with a warning — proper loopback support tracked
                        // in docs/dev/system-audio-capture.md.
                        let mic_device = resolve_input_device(&host, device_name.as_deref());
                        let system_device =
                            if matches!(source, AudioSource::System | AudioSource::Mix) {
                                resolve_loopback_device(&host)
                            } else {
                                None
                            };
                        // Remember which output device backs the loopback so the
                        // heartbeat can notice a mid-meeting default-output change
                        // (replug) and rebind. None when no loopback is in play.
                        bound_loopback_name = system_device.as_ref().and_then(|d| d.name().ok());
                        // Same for the mic side: remember the resolved input
                        // device so the heartbeat can notice a default-input
                        // change (Jabra connect/disconnect, AirPods switch)
                        // and rebuild the mic stream. Only meaningful when
                        // device_name was None (user wants whatever the
                        // system default is at any given moment); if the
                        // user pinned a device by name, cpal keeps that one.
                        bound_input_name = if device_name.is_none() {
                            mic_device.as_ref().and_then(|d| d.name().ok())
                        } else {
                            None
                        };

                        // For Mix, both must succeed; if system fails we degrade to
                        // mic-only with a log line so the user still gets SOMETHING.
                        let want_system = matches!(source, AudioSource::System | AudioSource::Mix);
                        let want_mic = matches!(source, AudioSource::Mic | AudioSource::Mix);
                        if want_system && system_device.is_none() {
                            crate::log("[Audio] WARNING: system-audio loopback unavailable on this platform — using mic only");
                        }
                        // System-only mode without a loopback device → fall back to mic
                        // so recording still works (degraded UX, but transcript happens).
                        let fallback_to_mic =
                            matches!(source, AudioSource::System) && system_device.is_none();

                        // Decide PRIMARY: mic when available + wanted; otherwise the
                        // loopback (system-only mode, or System fallback to mic
                        // failed up-stream). Track is_primary_loopback so we read
                        // the right cpal config endpoint below.
                        let primary_is_loopback;
                        let primary = if (want_mic || fallback_to_mic) && mic_device.is_some() {
                            primary_is_loopback = false;
                            mic_device.clone()
                        } else if let Some(ref sys) = system_device {
                            primary_is_loopback = true;
                            Some(sys.clone())
                        } else {
                            primary_is_loopback = false;
                            None
                        };

                        let device = match primary {
                            Some(d) => {
                                crate::log(&format!(
                                    "[Audio] Primary device: {:?} loopback={}",
                                    d.name().unwrap_or_default(),
                                    primary_is_loopback
                                ));
                                d
                            }
                            None => {
                                crate::log("[Audio] ERROR: No usable audio device for the requested source");
                                continue;
                            }
                        };

                        // cpal Windows host rejects default_input_config() on
                        // output devices ("stream type not supported"). For
                        // loopback we must read default_output_config() and pass
                        // its config to build_input_stream — that activates
                        // WASAPI loopback mode silently.
                        let config_result = if primary_is_loopback {
                            device.default_output_config()
                        } else {
                            device.default_input_config()
                        };
                        let config = match config_result {
                            Ok(c) => {
                                crate::log(&format!(
                                    "[Audio] Primary config: sr={}, ch={}, fmt={:?}",
                                    c.sample_rate().0,
                                    c.channels(),
                                    c.sample_format()
                                ));
                                c
                            }
                            Err(e) => {
                                crate::log(&format!(
                                    "[Audio] ERROR: Failed to get primary config: {}",
                                    e
                                ));
                                continue;
                            }
                        };

                        let channels = config.channels() as usize;

                        // Use the primary device's native config — some
                        // devices (notably BT HFP/SCO mic) reject any rate
                        // other than their native, even via cpal's shared-mode
                        // path. Native config is guaranteed to build. The
                        // SECONDARY stream then gets forced to MATCH this
                        // primary rate so meeting.rs's WAV writer + per-sample
                        // mix stay consistent. See build_secondary_stream
                        // for the matching-rate fallback chain.
                        let primary_actual_sr = config.sample_rate().0;
                        let canonical_config: cpal::StreamConfig = config.clone().into();
                        // The buffer always receives samples at the
                        // meeting's canonical rate (48 kHz); the cpal
                        // callback resamples from the device-native
                        // rate before pushing. This is what makes
                        // mid-meeting device swaps (BT-HFP 16 k →
                        // Realtek 48 k → USB headset 44.1 k → …) safe:
                        // the WAV writer + STT both see a stable rate.
                        ACTIVE_MIC_SAMPLE_RATE.store(MEETING_CANONICAL_RATE, Ordering::Relaxed);
                        ACTIVE_MIC_DEVICE_RATE.store(primary_actual_sr, Ordering::Relaxed);
                        crate::log(&format!(
                            "[Audio] Primary cpal config: device_sr={} ch={} → canonical_sr={} (resample={})",
                            primary_actual_sr,
                            config.channels(),
                            MEETING_CANONICAL_RATE,
                            if primary_actual_sr != MEETING_CANONICAL_RATE { "yes" } else { "no" }
                        ));
                        let primary_resampler: Arc<Mutex<Option<LinearResampler>>> =
                            Arc::new(Mutex::new(if primary_actual_sr != MEETING_CANONICAL_RATE {
                                Some(LinearResampler::new(
                                    primary_actual_sr,
                                    MEETING_CANONICAL_RATE,
                                ))
                            } else {
                                None
                            }));

                        // Fresh stream means whatever killed the previous one
                        // is moot — reset the sticky stream-dead flag so the
                        // UI / meeting worker doesn't keep treating us as
                        // broken when the device-change recovery succeeds.
                        AUDIO_STREAM_DEAD.store(false, Ordering::Relaxed);

                        // Clear buffers + AEC rings ONLY on a fresh
                        // user-initiated Start. Recovery Start must
                        // preserve the buffers because meeting.rs is
                        // tracking `samples_written` against the
                        // continuously growing primary buffer; new
                        // samples produced by the freshly-built stream
                        // are appended in coherent time order. The AEC
                        // rings are drained 10 ms at a time by the AEC
                        // worker, so any stale tail from the old device
                        // flushes through in a single frame.
                        if !is_recovery_start {
                            if let Ok(mut b) = buffer.lock() {
                                b.clear();
                            }
                            if let Ok(mut b) = buffer_secondary.lock() {
                                b.clear();
                            }
                            if let Ok(mut r) = aec_mic_ring.lock() {
                                r.clear();
                            }
                            if let Ok(mut r) = aec_ref_ring.lock() {
                                r.clear();
                            }
                            // Fresh recording → re-arm the long-dictation
                            // guardrail latches (a recovery start continues the
                            // same recording, so it must NOT reset them).
                            dict_soft_warned = false;
                            dict_hard_emitted = false;
                        }
                        is_recovery_start = false;

                        // In Mix mode the mic primary feeds the AEC mic ring
                        // (NOT audio_buffer directly) — the AEC worker writes
                        // the cleaned output to audio_buffer. In Mic-only and
                        // System-only modes the primary writes to audio_buffer
                        // as before; AEC stays idle.
                        let mix_mode = matches!(source, AudioSource::Mix);
                        let buf = if mix_mode {
                            aec_mic_ring.clone()
                        } else {
                            buffer.clone()
                        };
                        let gain_ref = input_gain.clone();
                        let sample_count =
                            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                        let sc1 = sample_count.clone();
                        let s = match config.sample_format() {
                            cpal::SampleFormat::F32 => {
                                let pr = primary_resampler.clone();
                                device.build_input_stream(
                                    &canonical_config,
                                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                                        let prev = sc1.fetch_add(
                                            data.len(),
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        if prev == 0 {
                                            crate::log(&format!(
                                                "[Audio] First F32 callback: {} samples",
                                                data.len()
                                            ));
                                        }
                                        let gain = f32::from_bits(
                                            gain_ref.load(std::sync::atomic::Ordering::Relaxed),
                                        );
                                        assert!(
                                            gain.is_finite(),
                                            "audio callback F32: gain must be finite, got {}",
                                            gain
                                        );
                                        assert!(
                                            (0.0..=2.0).contains(&gain),
                                            "audio callback F32: gain must be in [0.0, 2.0], got {}",
                                            gain
                                        );
                                        // Mono-mix + gain BEFORE resampling so the
                                        // resampler operates on a single channel.
                                        let mono_samples: Vec<f32> = if channels > 1 {
                                            data.chunks(channels)
                                                .map(|chunk| chunk.iter().sum::<f32>() / channels as f32 * gain)
                                                .collect()
                                        } else if (gain - 1.0).abs() < 0.001 {
                                            data.to_vec()
                                        } else {
                                            data.iter().map(|&s| s * gain).collect()
                                        };
                                        let to_push: Vec<f32> = {
                                            let mut guard = pr.lock().expect("primary resampler poisoned");
                                            if let Some(r) = guard.as_mut() {
                                                let mut out = Vec::with_capacity(mono_samples.len() * 4);
                                                r.process(&mono_samples, &mut out);
                                                out
                                            } else {
                                                mono_samples
                                            }
                                        };
                                        // Paused meeting: skip the append (gap-skip
                                        // discards these at resume; keeping them only
                                        // grows RAM). See MEETING_CAPTURE_GATED.
                                        if !meeting_capture_gated() {
                                            if let Ok(mut b) = buf.lock() {
                                                b.extend_from_slice(&to_push);
                                            }
                                        }
                                    },
                                    |err| notify_audio_stream_error("primary", "F32", &err),
                                    None,
                                )
                            }
                            cpal::SampleFormat::I16 => {
                                let buf2 = buffer.clone();
                                let gain_ref2 = input_gain.clone();
                                let sc2 = sample_count.clone();
                                let pr = primary_resampler.clone();
                                device.build_input_stream(
                                    &canonical_config,
                                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                                        let prev = sc2.fetch_add(data.len(), std::sync::atomic::Ordering::Relaxed);
                                        if prev == 0 {
                                            crate::log(&format!("[Audio] First I16 callback: {} samples", data.len()));
                                        }
                                        let gain = f32::from_bits(gain_ref2.load(std::sync::atomic::Ordering::Relaxed));
                                        assert!(gain.is_finite(), "audio callback I16: gain must be finite, got {}", gain);
                                        assert!((0.0..=2.0).contains(&gain), "audio callback I16: gain must be in [0.0, 2.0], got {}", gain);
                                        let mono_samples: Vec<f32> = data
                                            .chunks(channels)
                                            .map(|chunk| {
                                                chunk.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / channels as f32 * gain
                                            })
                                            .collect();
                                        let to_push: Vec<f32> = {
                                            let mut guard = pr.lock().expect("primary resampler poisoned");
                                            if let Some(r) = guard.as_mut() {
                                                let mut out = Vec::with_capacity(mono_samples.len() * 4);
                                                r.process(&mono_samples, &mut out);
                                                out
                                            } else {
                                                mono_samples
                                            }
                                        };
                                        // Paused meeting: skip append (see F32 twin).
                                        if !meeting_capture_gated() {
                                            if let Ok(mut b) = buf2.lock() {
                                                b.extend_from_slice(&to_push);
                                            }
                                        }
                                    },
                                    |err| notify_audio_stream_error("primary", "I16", &err),
                                    None,
                                )
                            }
                            _ => {
                                crate::log(&format!(
                                    "[Audio] ERROR: Unsupported sample format: {:?}",
                                    config.sample_format()
                                ));
                                // Tell the USER too — without a stream this
                                // recording captures pure silence and before
                                // 2026-07-02 the only trace was dimmy.log.
                                crate::ffi::emit_event(
                                    "error",
                                    &format!(
                                        r#"{{"message":"Microphone format {:?} not supported — recording will be silent. Pick another input device in Settings."}}"#,
                                        config.sample_format()
                                    ),
                                );
                                continue;
                            }
                        };

                        match s {
                            Ok(s) => {
                                let _ = s.play();
                                streams.push(s);
                                crate::log("[Audio] Primary stream built and playing");
                            }
                            Err(e) => {
                                crate::log(&format!(
                                    "[Audio] ERROR: Failed to build primary stream: {}",
                                    e
                                ));
                                continue;
                            }
                        }

                        // ── Mix mode: open the loopback stream as a SECOND
                        // input that writes the SECONDARY buffer (separate
                        // from the mic's primary buffer). meeting.rs reads
                        // both buffers, takes min length, and mixes per-sample
                        // → correct playback speed and a real per-sample mix
                        // of the two sources instead of race-interleaved chaos.
                        // System-only (no Mix) was already handled above
                        // by treating the loopback as the PRIMARY device.
                        if matches!(source, AudioSource::Mix) {
                            if let Some(sysdev) = system_device {
                                if let Some(s) = build_secondary_stream(
                                    &sysdev,
                                    &buffer_secondary,
                                    Some(&aec_ref_ring),
                                    &input_gain,
                                    &loopback_gain,
                                    /* is_loopback */ true,
                                    /* prefer_sr = MEETING_CANONICAL_RATE.
                                     * Both primary and secondary always
                                     * resample to the meeting canonical
                                     * rate (48 kHz) inside their cpal
                                     * callbacks. Any device swap (BT
                                     * ↔ speakers ↔ USB headset) keeps
                                     * the WAV writer + mix coherent. */
                                    Some(MEETING_CANONICAL_RATE),
                                ) {
                                    let _ = s.play();
                                    streams.push(s);
                                    crate::log(
                                        "[Audio] Mix mode: secondary loopback stream playing into dedicated buffer + AEC ref ring",
                                    );
                                }
                            }
                        }
                    }
                    AudioCommand::Stop => {
                        // Dropping the streams stops recording
                        streams.clear();
                        // Explicit stop clears the recovery hint — we
                        // don't want a stale stream-dead flag to spin
                        // the recording back up after the user (or the
                        // meeting worker) explicitly asked us to halt.
                        last_start_params = None;
                        bound_loopback_name = None;
                        bound_input_name = None;
                        AUDIO_STREAM_DEAD.store(false, Ordering::Relaxed);
                        // Clear the published mic rate so callers
                        // (e.g. SystemAudioCaptureService on Mac)
                        // know there is no active mic to match.
                        ACTIVE_MIC_SAMPLE_RATE.store(0, Ordering::Relaxed);
                        ACTIVE_MIC_DEVICE_RATE.store(0, Ordering::Relaxed);
                        // And clear the externally-supplied loopback
                        // rate so the next recording re-probes from a
                        // clean slate instead of inheriting a stale
                        // value from a prior meeting.
                        LOOPBACK_SAMPLE_RATE_OVERRIDE.store(0, Ordering::Relaxed);
                    }
                    AudioCommand::PushLoopback(samples, source_rate_hint) => {
                        // Decide the effective source rate. Hint from the FFI
                        // wins (carried per-push from the macOS tap's
                        // negotiated format). When 0, fall back to whatever
                        // dimmy_set_loopback_sample_rate stashed in the
                        // override. When that's also 0 (no info at all),
                        // treat as canonical (passthrough) — same as the
                        // pre-resample behaviour so we never WORSEN a
                        // missing-rate caller.
                        let mut effective_src_rate = if source_rate_hint > 0 {
                            source_rate_hint
                        } else {
                            LOOPBACK_SAMPLE_RATE_OVERRIDE.load(Ordering::Relaxed)
                        };
                        if !(8_000..=192_000).contains(&effective_src_rate) {
                            effective_src_rate = MEETING_CANONICAL_RATE;
                        }

                        // Rebuild the resampler iff the source rate changed
                        // (first push of this meeting, or a mid-meeting BT /
                        // output-device renegotiation). Identity case
                        // (src == canonical) leaves it None — zero allocation,
                        // zero filter state to clear.
                        if effective_src_rate != loopback_last_src_rate {
                            loopback_last_src_rate = effective_src_rate;
                            loopback_resampler = if effective_src_rate != MEETING_CANONICAL_RATE {
                                crate::log(&format!(
                                    "[Audio/loopback] resampler rebuilt: {} Hz → {} Hz (canonical)",
                                    effective_src_rate, MEETING_CANONICAL_RATE
                                ));
                                Some(LinearResampler::new(
                                    effective_src_rate,
                                    MEETING_CANONICAL_RATE,
                                ))
                            } else {
                                crate::log(
                                    "[Audio/loopback] source rate matches canonical 48 kHz — passthrough",
                                );
                                None
                            };
                        }

                        let in_len = samples.len();
                        // Resample (or passthrough) into a single owned Vec
                        // before grabbing either lock so the critical
                        // sections stay small (callbacks competing for the
                        // same mutex are a known hotspot).
                        let canonical: Vec<f32> = if let Some(ref mut r) = loopback_resampler {
                            let mut out = Vec::with_capacity(
                                in_len.saturating_mul(MEETING_CANONICAL_RATE as usize)
                                    / effective_src_rate.max(1) as usize
                                    + 4,
                            );
                            r.process(&samples, &mut out);
                            out
                        } else {
                            samples
                        };
                        let out_len = canonical.len();

                        // Paused meeting: skip appends (gap-skip discards
                        // these at resume; keeping them only grows RAM).
                        if !meeting_capture_gated() {
                            if let Ok(mut b) = buffer_secondary.lock() {
                                for &s in &canonical {
                                    b.push(s.clamp(-1.0, 1.0));
                                }
                            }
                            if let Ok(mut r) = aec_ref_ring.lock() {
                                for &s in &canonical {
                                    r.push(s.clamp(-1.0, 1.0));
                                }
                            }
                        }

                        // Periodic 5 s diagnostic so the colleague's
                        // "doesn't record system audio" report can be
                        // diagnosed from dimmy.log:
                        //   - in_samples 0 → tap is denied / never fires
                        //     (macOS audio-capture TCC grant missing)
                        //   - in_samples > 0 but out_samples ≈ in/3 → tap
                        //     at 16 kHz, resampler upsampled to 48 kHz
                        //     (expected on BT-HFP outputs)
                        //   - secondary buffer length lags primary by more
                        //     than a chunk window → align_secondary is
                        //     zero-padding because pushes stopped (tap died
                        //     mid-meeting)
                        loopback_diag_in_samples += in_len as u64;
                        loopback_diag_out_samples += out_len as u64;
                        loopback_diag_pushes += 1;
                        if !canonical.is_empty() {
                            let push_peak =
                                canonical.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
                            if push_peak > loopback_diag_peak_abs {
                                loopback_diag_peak_abs = push_peak;
                            }
                        }
                        if loopback_diag_last_log.elapsed() >= std::time::Duration::from_secs(5) {
                            let sec_len = buffer_secondary.lock().map(|b| b.len()).unwrap_or(0);
                            let ref_len = aec_ref_ring.lock().map(|r| r.len()).unwrap_or(0);
                            crate::log(&format!(
                                "[Audio/loopback] 5s tick: pushes={} in_samples={} out_samples={} src_rate={} dst_rate={} peak_abs={:.4} secondary_buf_len={} aec_ref_len={}",
                                loopback_diag_pushes,
                                loopback_diag_in_samples,
                                loopback_diag_out_samples,
                                effective_src_rate,
                                MEETING_CANONICAL_RATE,
                                loopback_diag_peak_abs,
                                sec_len,
                                ref_len,
                            ));
                            loopback_diag_last_log = std::time::Instant::now();
                            loopback_diag_in_samples = 0;
                            loopback_diag_out_samples = 0;
                            loopback_diag_pushes = 0;
                            loopback_diag_peak_abs = 0.0;
                        }
                    }
                }
            }
        },
    );

    tx
}

// ── Device-resolution helpers used by spawn_audio_thread ─────────────

/// Resolve the input device for Mic capture. If a specific name was
/// requested, search the input list; otherwise return the system
/// default. Logs the resolution path so "device disappeared" / typos
/// in the saved config are debuggable from dimmy.log alone.
fn resolve_input_device(host: &cpal::Host, name: Option<&str>) -> Option<cpal::Device> {
    if let Some(name) = name {
        let found = host
            .input_devices()
            .ok()
            .and_then(|mut devs| devs.find(|d| d.name().ok().as_deref() == Some(name)));
        if found.is_some() {
            crate::log(&format!("[Audio] Found input device by name: {}", name));
            return found;
        }
        crate::log(&format!(
            "[Audio] Input device '{}' not found, falling back to default",
            name
        ));
    }
    host.default_input_device()
}

/// Resolve the loopback (system audio) device. On Windows, cpal's
/// default OUTPUT device exposes WASAPI loopback when treated as an
/// INPUT — `device.build_input_stream()` on it captures whatever's
/// playing through the speakers. On macOS / Linux this returns None
/// for now; proper implementation tracked separately.
#[cfg(target_os = "windows")]
fn resolve_loopback_device(host: &cpal::Host) -> Option<cpal::Device> {
    let dev = host.default_output_device();
    if let Some(ref d) = dev {
        crate::log(&format!(
            "[Audio] Loopback device (WASAPI default output): {:?}",
            d.name().unwrap_or_default()
        ));
    } else {
        crate::log("[Audio] No default output device — loopback unavailable");
    }
    dev
}

#[cfg(not(target_os = "windows"))]
fn resolve_loopback_device(_host: &cpal::Host) -> Option<cpal::Device> {
    crate::log("[Audio] Loopback not yet implemented on this platform");
    None
}

/// Name of the current system default OUTPUT device, used to detect a
/// mid-meeting default change (e.g. the user replugs headphones and Windows
/// flips the default back to them). Windows-only: the loopback follows the
/// default output. On other platforms loopback comes from an external push
/// path (no cpal default-output device), so this is always `None` and the
/// follow logic is a no-op.
#[cfg(target_os = "windows")]
fn current_default_output_name(host: &cpal::Host) -> Option<String> {
    host.default_output_device().and_then(|d| d.name().ok())
}

#[cfg(not(target_os = "windows"))]
fn current_default_output_name(_host: &cpal::Host) -> Option<String> {
    None
}

/// Live name of the system default INPUT device. Cross-platform — every
/// host needs this so the mic stream follows headset / Jabra / AirPods
/// hot-plug. Returns `None` if no default input is currently available
/// (transient state, e.g. all devices unplugged between events).
fn current_default_input_name(host: &cpal::Host) -> Option<String> {
    host.default_input_device().and_then(|d| d.name().ok())
}

/// Decide whether the audio thread should restart streams to FOLLOW a system
/// default-output change (the Windows loopback-follow path). Pure so it can be
/// unit-tested without cpal.
///
/// The WASAPI loopback is bound to whatever was the default output when the
/// meeting started. If the user unplugs/replugs an output device mid-meeting,
/// Windows can flip the default to a DIFFERENT endpoint while the old loopback
/// stream stays alive (no cpal error) capturing silence. We detect that here:
/// when system audio is wanted and the live default output no longer matches
/// the device the loopback is bound to, the caller rebinds via the normal
/// device-change recovery. Only fires when BOTH names are known and differ, so
/// a transient "no default output" tick can't thrash the streams.
fn loopback_should_follow_default(
    source: AudioSource,
    bound_loopback_name: Option<&str>,
    current_default_output: Option<&str>,
) -> bool {
    if !matches!(source, AudioSource::System | AudioSource::Mix) {
        return false;
    }
    match (bound_loopback_name, current_default_output) {
        (Some(bound), Some(current)) => bound != current,
        _ => false,
    }
}

/// True when the mic stream must be rebuilt because the system default
/// input device changed (e.g. Jabra plugged/unplugged, AirPods connect,
/// user picked a different input in Sound preferences). Mirrors
/// `loopback_should_follow_default` for the mic side: only fires when
/// the user explicitly asked for the default (`requested_device_name`
/// is None) — when they pinned a specific device by name, cpal keeps
/// using THAT device and we don't second-guess the choice.
///
/// `requested_device_name`: what the caller passed to `Start` (None
/// means "default"). `bound_input_name`: which device cpal actually
/// resolved to and bound the stream to. `current_default_input`:
/// the live system default input as seen from cpal right now.
fn input_should_follow_default(
    requested_device_name: Option<&str>,
    bound_input_name: Option<&str>,
    current_default_input: Option<&str>,
) -> bool {
    if requested_device_name.is_some() {
        return false;
    }
    match (bound_input_name, current_default_input) {
        (Some(bound), Some(current)) => bound != current,
        _ => false,
    }
}

/// Build a SECOND input stream that writes into the SAME shared
/// buffer as the primary mic stream. Used by Mix mode. Samples from
/// both streams interleave in time order; downstream VAD + AGC see
/// them as a single mono track. Returns None on config / build
/// failure (Mix mode degrades to mic-only in that case).
///
/// `is_loopback`: when true, the device is the default OUTPUT device
/// being treated as a loopback INPUT — on Windows we must read its
/// `default_output_config()` because cpal's Windows host rejects
/// `default_input_config()` on output devices ("stream type not
/// supported"), but `build_input_stream` with the OUTPUT config
/// activates WASAPI loopback mode silently.
fn build_secondary_stream(
    device: &cpal::Device,
    buffer: &Arc<Mutex<Vec<f32>>>,
    aec_ref_ring: Option<&Arc<Mutex<Vec<f32>>>>,
    input_gain: &Arc<std::sync::atomic::AtomicU32>,
    loopback_gain: &Arc<std::sync::atomic::AtomicU32>,
    is_loopback: bool,
    prefer_sr: Option<u32>,
) -> Option<cpal::Stream> {
    let config = if is_loopback {
        match device.default_output_config() {
            Ok(c) => {
                crate::log(&format!(
                    "[Audio] Secondary loopback config: sr={}, ch={}, fmt={:?}",
                    c.sample_rate().0,
                    c.channels(),
                    c.sample_format()
                ));
                c
            }
            Err(e) => {
                crate::log(&format!(
                    "[Audio] Secondary loopback default_output_config failed: {}",
                    e
                ));
                return None;
            }
        }
    } else {
        match device.default_input_config() {
            Ok(c) => {
                crate::log(&format!(
                    "[Audio] Secondary input config: sr={}, ch={}, fmt={:?}",
                    c.sample_rate().0,
                    c.channels(),
                    c.sample_format()
                ));
                c
            }
            Err(e) => {
                crate::log(&format!("[Audio] Secondary config failed: {}", e));
                return None;
            }
        }
    };
    let channels = config.channels() as usize;
    let buf = buffer.clone();
    // Open the cpal device at its NATIVE rate, then run a software
    // resampler in the callback to deliver samples at the meeting's
    // canonical rate. Two reasons we don't try to force-bind cpal to
    // `prefer_sr`:
    //   1. WASAPI shared-mode silently accepts a "wrong" rate on some
    //      drivers and produces samples-at-native while pretending it
    //      honoured the request — leads to "rallentato" playback in
    //      audio_system.wav exactly like the BT-disconnect symptom
    //      the user reported 2026-05-19.
    //   2. BT-HFP explicitly rejects anything but native, breaking the
    //      stream entirely.
    // Native + LinearResampler is the path that handles every device
    // transition (BT-A2DP ↔ laptop speakers ↔ USB headset) cleanly.
    let canonical_config: cpal::StreamConfig = config.clone().into();
    let device_sr = config.sample_rate().0;
    let target_sr = prefer_sr.unwrap_or(device_sr);
    crate::log(&format!(
        "[Audio] Secondary native config: sr={} ch={} → meeting canonical sr={} (resample={})",
        device_sr,
        config.channels(),
        target_sr,
        if device_sr != target_sr { "yes" } else { "no" }
    ));
    let resampler: Arc<Mutex<Option<LinearResampler>>> =
        Arc::new(Mutex::new(if device_sr != target_sr {
            Some(LinearResampler::new(device_sr, target_sr))
        } else {
            None
        }));
    // Optional second destination: in Mix mode the AEC worker needs the
    // SAME loopback samples as a `render` reference so it can subtract
    // the speaker echo from the mic capture. We push to both buffers
    // (raw record + AEC ref ring) inside the cpal callback.
    let aec_ref = aec_ref_ring.cloned();
    // NOTE: secondary stream path = loopback (system audio). We deliberately
    // DO NOT apply `input_gain` here (that knob is calibrated for mic peak
    // levels) and DO NOT divide by `channels` on stereo→mono downmix. The
    // sum-clamp form preserves perceived loudness for center-panned voice
    // (typical YouTube/Teams content) instead of giving up 6dB to match the
    // mic stream — the bug the user described as "system practically
    // inaudible" with mic-quality vs system-quality wildly mismatched.
    let _ = input_gain; // intentionally unused on the loopback path
                        // Loopback makeup gain — soft-clipped via tanh so high peaks
                        // don't produce clamp clicks. Read fresh each callback so a
                        // runtime config change takes effect immediately. Bounds check
                        // enforces sane range; outside it we treat as 1.0 (no change).
    let loopback_gain_ref = loopback_gain.clone();
    // Periodic loopback diagnostics. Every ~5 s of wall time the
    // callback emits ONE log line with the peak amplitude observed
    // in that window. Lets the user see in dimmy.log whether the
    // loopback is actually receiving audio (peak > 0.001) or pure
    // silence (peak == 0) — useful when debugging Bluetooth profile
    // switches (HSP vs A2DP) and per-app device routing.
    let diag_sr = config.sample_rate().0 as u64;
    let diag_window_samples = (diag_sr * 5) as usize;
    let diag_peak = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let diag_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let diag_window = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let dp = diag_peak.clone();
            let dc = diag_count.clone();
            let dw = diag_window.clone();
            let lg = loopback_gain_ref.clone();
            let rs = resampler.clone();
            device.build_input_stream(
                &canonical_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let g = f32::from_bits(lg.load(std::sync::atomic::Ordering::Relaxed));
                    let g = if g.is_finite() && (0.5..=4.0).contains(&g) {
                        g
                    } else {
                        1.0
                    };
                    let mono_samples: Vec<f32> = if channels > 1 {
                        let chf = channels as f32;
                        data.chunks(channels)
                            .map(|chunk| {
                                // Stereo→mono via AVERAGE (not sum):
                                // sum doubles amplitude for centre-
                                // panned voice (0.7 L + 0.7 R = 1.4 →
                                // clamp = 1.0 → harmonic clipping
                                // distortion EVEN with gain=1.0). Avg
                                // keeps amplitude in the natural range.
                                // If the user wants the loopback louder
                                // than the mic, the loopback_gain knob
                                // (config) makes that explicit instead
                                // of hard-clipping the sum.
                                let avg = chunk.iter().sum::<f32>() / chf;
                                (avg * g).clamp(-1.0, 1.0)
                            })
                            .collect()
                    } else {
                        data.iter().map(|&s| (s * g).clamp(-1.0, 1.0)).collect()
                    };
                    // Update diagnostic peak (atomic max via CAS loop) and
                    // sample count. Cheap; runs on every callback.
                    let chunk_peak = mono_samples
                        .iter()
                        .fold(0.0f32, |m, &s| m.max(s.abs()));
                    let mut prev = dp.load(std::sync::atomic::Ordering::Relaxed);
                    while chunk_peak.to_bits() > prev {
                        match dp.compare_exchange_weak(
                            prev,
                            chunk_peak.to_bits(),
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(p) => prev = p,
                        }
                    }
                    dc.fetch_add(mono_samples.len(), std::sync::atomic::Ordering::Relaxed);
                    let win =
                        dw.fetch_add(mono_samples.len(), std::sync::atomic::Ordering::Relaxed)
                            + mono_samples.len();
                    if win >= diag_window_samples {
                        let total = dc.load(std::sync::atomic::Ordering::Relaxed);
                        let peak_bits = dp.swap(0, std::sync::atomic::Ordering::Relaxed);
                        let peak = f32::from_bits(peak_bits);
                        dw.store(0, std::sync::atomic::Ordering::Relaxed);
                        crate::log(&format!(
                            "[Audio] Loopback diag: total_samples={}, last_5s_peak={:.4} (>0.001 means signal, ~0 means silence)",
                            total, peak
                        ));
                    }
                    let to_push: Vec<f32> = {
                        let mut guard = rs.lock().expect("resampler mutex poisoned");
                        if let Some(r) = guard.as_mut() {
                            let mut out = Vec::with_capacity(mono_samples.len() * 2);
                            r.process(&mono_samples, &mut out);
                            out
                        } else {
                            mono_samples
                        }
                    };
                    // Paused meeting: skip appends (gap-skip discards these
                    // at resume; keeping them only grows RAM).
                    if !meeting_capture_gated() {
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&to_push);
                        }
                        if let Some(ref aec) = aec_ref {
                            crate::aec::push_to_ring(aec, &to_push);
                        }
                    }
                },
                |err| notify_audio_stream_error("secondary", "F32", &err),
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let dp = diag_peak.clone();
            let dc = diag_count.clone();
            let dw = diag_window.clone();
            let lg = loopback_gain_ref.clone();
            let rs = resampler.clone();
            device.build_input_stream(
                &canonical_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let g = f32::from_bits(lg.load(std::sync::atomic::Ordering::Relaxed));
                    let g = if g.is_finite() && (0.5..=4.0).contains(&g) {
                        g
                    } else {
                        1.0
                    };
                    let mono_samples: Vec<f32> = {
                        let chf = channels as f32;
                        data.chunks(channels)
                            .map(|chunk| {
                                // Average channels (see F32 branch
                                // comment for why avg, not sum).
                                let avg = chunk
                                    .iter()
                                    .map(|&s| s as f32 / 32768.0)
                                    .sum::<f32>()
                                    / chf;
                                (avg * g).clamp(-1.0, 1.0)
                            })
                            .collect()
                    };
                    let chunk_peak = mono_samples
                        .iter()
                        .fold(0.0f32, |m, &s| m.max(s.abs()));
                    let mut prev = dp.load(std::sync::atomic::Ordering::Relaxed);
                    while chunk_peak.to_bits() > prev {
                        match dp.compare_exchange_weak(
                            prev,
                            chunk_peak.to_bits(),
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(p) => prev = p,
                        }
                    }
                    dc.fetch_add(mono_samples.len(), std::sync::atomic::Ordering::Relaxed);
                    let win =
                        dw.fetch_add(mono_samples.len(), std::sync::atomic::Ordering::Relaxed)
                            + mono_samples.len();
                    if win >= diag_window_samples {
                        let total = dc.load(std::sync::atomic::Ordering::Relaxed);
                        let peak_bits = dp.swap(0, std::sync::atomic::Ordering::Relaxed);
                        let peak = f32::from_bits(peak_bits);
                        dw.store(0, std::sync::atomic::Ordering::Relaxed);
                        crate::log(&format!(
                            "[Audio] Loopback diag: total_samples={}, last_5s_peak={:.4} (>0.001 means signal, ~0 means silence)",
                            total, peak
                        ));
                    }
                    let to_push: Vec<f32> = {
                        let mut guard = rs.lock().expect("resampler mutex poisoned");
                        if let Some(r) = guard.as_mut() {
                            let mut out = Vec::with_capacity(mono_samples.len() * 2);
                            r.process(&mono_samples, &mut out);
                            out
                        } else {
                            mono_samples
                        }
                    };
                    // Paused meeting: skip appends (see F32 twin).
                    if !meeting_capture_gated() {
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&to_push);
                        }
                        if let Some(ref aec) = aec_ref {
                            crate::aec::push_to_ring(aec, &to_push);
                        }
                    }
                },
                |err| notify_audio_stream_error("secondary", "I16", &err),
                None,
            )
        }
        _ => {
            crate::log("[Audio] Secondary unsupported sample format");
            return None;
        }
    };
    match stream {
        Ok(s) => Some(s),
        Err(e) => {
            crate::log(&format!("[Audio] Secondary build failed: {}", e));
            None
        }
    }
}

/// Get the default input device name
pub fn default_input_device_name() -> String {
    let host = cpal::default_host();
    host.default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_else(|| "No device".to_string())
}

/// Get sample rate for a specific device (by name), falling back to default
pub fn device_sample_rate(device_name: &Option<String>) -> u32 {
    primary_sample_rate(device_name, &AudioSource::Mic)
}

/// Sample rate of the PRIMARY capture stream — i.e. the rate at which
/// the shared audio buffer fills in wall time. The meeting writer needs
/// THIS rate (not the mic's input rate) to downsample correctly,
/// otherwise a System-mode recording plays back at the wrong speed
/// (loopback runs at 48 kHz on most systems, mic is often 16 kHz).
///
/// - Mic / Mix: native input rate of the selected mic (primary stream
///   is the mic; in Mix the loopback is a secondary that interleaves
///   into the same buffer, which has its own time-compression caveat
///   tracked separately).
/// - System: native OUTPUT rate of the default output device, since
///   that's the device cpal opens in WASAPI loopback mode.
pub fn primary_sample_rate(device_name: &Option<String>, source: &AudioSource) -> u32 {
    let host = cpal::default_host();
    let rate = match source {
        AudioSource::System => {
            #[cfg(target_os = "windows")]
            {
                host.default_output_device()
                    .and_then(|d| d.default_output_config().ok())
                    .map(|c| c.sample_rate().0)
                    .unwrap_or_else(|| primary_sample_rate(device_name, &AudioSource::Mic))
            }
            #[cfg(not(target_os = "windows"))]
            {
                primary_sample_rate(device_name, &AudioSource::Mic)
            }
        }
        AudioSource::Mic | AudioSource::Mix => {
            let device = if let Some(ref name) = device_name {
                host.input_devices()
                    .ok()
                    .and_then(|mut devs| {
                        devs.find(|d| d.name().ok().as_deref() == Some(name.as_str()))
                    })
                    .or_else(|| host.default_input_device())
            } else {
                host.default_input_device()
            };
            device
                .and_then(|d| d.default_input_config().ok())
                .map(|c| c.sample_rate().0)
                .unwrap_or(44100)
        }
    };
    assert!(
        (8000..=192_000).contains(&rate),
        "primary_sample_rate: rate {} out of range",
        rate
    );
    rate
}

/// Native sample rate of the loopback (system audio) source. Used by
/// meeting.rs in Mix mode to write `audio_system.wav` with the correct
/// header rate when the mic and loopback devices have different native
/// rates (typical: BT mic in HFP at 16 kHz + speakers loopback at 48 kHz).
///
/// Resolution order:
///   1. Explicit override set via `set_loopback_sample_rate_override`
///      (Swift / Linux push paths publish the SCStream / Pulse rate
///      here BEFORE the meeting worker creates the WAV file).
///   2. On Windows — cpal default output device's config rate
///      (WASAPI loopback runs at this rate).
///   3. On Mac / Linux — `primary_fallback`, the cpal mic rate. The
///      external push path (`SystemAudioCaptureService` on macOS)
///      matches SCStream to that rate so the two streams collapse to
///      a single rate and `audio.wav` (the mix) plays back without
///      rate distortion. Pre-fix this fell through to a hardcoded
///      48 kHz that broke playback whenever the mic was on BT-HFP.
pub fn secondary_sample_rate(primary_fallback: u32) -> u32 {
    let override_rate = LOOPBACK_SAMPLE_RATE_OVERRIDE.load(Ordering::Relaxed);
    if override_rate != 0 {
        return override_rate;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = primary_fallback;
        let host = cpal::default_host();
        host.default_output_device()
            .and_then(|d| d.default_output_config().ok())
            .map(|c| c.sample_rate().0)
            .unwrap_or(48_000)
    }
    #[cfg(not(target_os = "windows"))]
    {
        if primary_fallback >= 8_000 {
            primary_fallback
        } else {
            48_000
        }
    }
}

// ── Typed audio pipeline ─────────────────────────────────────────────
// These types enforce the correct processing order at compile time:
//   RawAudio → ProcessedAudio → WavPayload
// Impossible to encode non-downsampled audio or skip preprocessing.

/// Audio raw from capture (device sample rate, mono)
pub struct RawAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Audio after optional preprocessing (VAD, AGC) at device sample rate
pub struct ProcessedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// WAV-encoded audio ready for STT API (16kHz, 16-bit PCM)
pub struct WavPayload {
    pub data: Vec<u8>,
    pub duration_secs: f32,
}

impl RawAudio {
    /// Apply preprocessing (highpass + VAD + AGC) if enabled.
    /// If disabled, passes audio through unchanged.
    pub fn preprocess(self, enabled: bool) -> ProcessedAudio {
        // Raw audio must have a valid sample rate
        assert!(
            self.sample_rate > 0,
            "RawAudio sample_rate must be positive"
        );

        let samples = if enabled {
            crate::preprocess::process_buffer(&self.samples, self.sample_rate)
        } else {
            self.samples
        };
        ProcessedAudio {
            samples,
            sample_rate: self.sample_rate,
        }
    }

    /// Encode raw audio to WAV at device sample rate (for debug output).
    pub fn to_wav(&self) -> Result<Vec<u8>, crate::error::AudioError> {
        encode_wav(&self.samples, self.sample_rate)
    }
}

impl ProcessedAudio {
    /// Downsample to 16kHz and encode as WAV for STT.
    pub fn to_wav_payload(self) -> Result<WavPayload, crate::error::AudioError> {
        // Processed audio must have a valid sample rate before resampling
        assert!(
            self.sample_rate > 0,
            "ProcessedAudio sample_rate must be positive"
        );

        let resampled = crate::preprocess::downsample_to_16k(&self.samples, self.sample_rate);
        let duration_secs = resampled.len() as f32 / 16000.0;
        let data = encode_wav(&resampled, 16000)?;

        // WAV output must start with RIFF header
        assert!(data.starts_with(b"RIFF"), "WAV data missing RIFF header");
        // Duration cannot be negative
        assert!(duration_secs >= 0.0, "WAV duration must be non-negative");

        Ok(WavPayload {
            data,
            duration_secs,
        })
    }

    /// Encode at original sample rate (for debug output, not for STT).
    pub fn to_wav(&self) -> Result<Vec<u8>, crate::error::AudioError> {
        encode_wav(&self.samples, self.sample_rate)
    }

    /// Whether the processed audio is empty (no speech detected by VAD).
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Estimate WAV file size after downsampling to 16kHz, without encoding.
    /// Formula: ceil(samples / downsample_ratio) * 2 bytes + 44 byte WAV header.
    pub fn estimate_wav_size(&self) -> usize {
        assert!(
            self.sample_rate > 0,
            "estimate_wav_size: sample_rate must be positive"
        );
        let ratio = self.sample_rate as f64 / 16000.0;
        let output_samples = (self.samples.len() as f64 / ratio).ceil() as usize;
        let size = output_samples * 2 + 44; // 16-bit mono PCM + WAV header
                                            // Size must be at least the header
        assert!(size >= 44, "estimate_wav_size: size below header minimum");
        size
    }

    /// Split audio into chunks, each at most `max_chunk_samples` long.
    /// Searches backwards from the max boundary in the last 25% for a silence
    /// window (RMS < 0.01 for 300ms). If no silence found, force-splits at max.
    /// All chunks inherit the same sample_rate. Total samples in = total samples out.
    pub fn split_at_silence(self, max_chunk_samples: usize) -> Vec<ProcessedAudio> {
        assert!(
            max_chunk_samples > 0,
            "split_at_silence: max_chunk_samples must be positive"
        );
        let sr = self.sample_rate;
        let total = self.samples.len();

        if total <= max_chunk_samples {
            return vec![ProcessedAudio {
                samples: self.samples,
                sample_rate: sr,
            }];
        }

        let silence_threshold: f32 = 0.01;
        let silence_window = ((sr as f64 * 0.3) as usize).max(1); // 300ms, min 1
        let mut chunks = Vec::new();
        let mut offset = 0;

        while offset < total {
            let remaining = total - offset;
            if remaining <= max_chunk_samples {
                chunks.push(ProcessedAudio {
                    samples: self.samples[offset..total].to_vec(),
                    sample_rate: sr,
                });
                break;
            }

            // Search for silence in the last 25% of the chunk window
            let chunk_end = (offset + max_chunk_samples).min(total);
            let search_start = offset + (max_chunk_samples - max_chunk_samples / 4);
            let mut split_at = None;

            if chunk_end > silence_window + offset {
                let scan_from = search_start.max(offset + silence_window);
                for pos in (scan_from..chunk_end).rev() {
                    let win_start = pos.saturating_sub(silence_window);
                    if win_start < offset {
                        break;
                    }
                    let window = &self.samples[win_start..pos];
                    let rms =
                        (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();
                    if rms < silence_threshold {
                        split_at = Some(pos);
                        break;
                    }
                }
            }

            let end = split_at.unwrap_or(chunk_end);
            // Defensive: never produce a zero-length chunk (would infinite-loop)
            let end = end.max(offset + 1);
            chunks.push(ProcessedAudio {
                samples: self.samples[offset..end].to_vec(),
                sample_rate: sr,
            });
            offset = end;
        }

        // Post-condition: total samples preserved
        let out_total: usize = chunks.iter().map(|c| c.samples.len()).sum();
        assert_eq!(
            out_total, total,
            "split_at_silence lost samples: {} in, {} out",
            total, out_total
        );

        chunks
    }
}

/// Encode f32 samples to WAV bytes (16-bit PCM, mono)
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, crate::error::AudioError> {
    // Sample rate must be positive and within sane hardware limits
    assert!(sample_rate > 0, "encode_wav: sample_rate must be positive");
    assert!(
        sample_rate <= 192000,
        "encode_wav: sample_rate exceeds 192kHz sanity limit"
    );
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for &sample in samples {
            // Sanitize: NaN/Inf → 0 (silence), then clamp to 16-bit range.
            // NaN/Inf should never reach here if the pipeline is correct, but
            // crashing the app in production over a corrupt sample is worse.
            let safe = if sample.is_finite() { sample } else { 0.0 };
            let s16 = (safe * 32767.0).clamp(-32768.0, 32767.0) as i16;
            writer.write_sample(s16)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_duration_events_fire_once_at_thresholds() {
        let r = MEETING_CANONICAL_RATE;
        let at = |secs: f64| (secs * r as f64) as usize;
        // Below the soft threshold: nothing fires.
        assert_eq!(
            dictation_duration_events(at(100.0), r, false, false),
            (false, false)
        );
        // At the soft threshold, not yet warned: warn only.
        assert_eq!(
            dictation_duration_events(at(300.0), r, false, false),
            (true, false)
        );
        // Past soft but already warned: no repeat, hard not yet reached.
        assert_eq!(
            dictation_duration_events(at(350.0), r, true, false),
            (false, false)
        );
        // At the hard threshold, warned already: hard-stop fires once.
        assert_eq!(
            dictation_duration_events(at(600.0), r, true, false),
            (false, true)
        );
        // Past hard, both already emitted: nothing repeats.
        assert_eq!(
            dictation_duration_events(at(700.0), r, true, true),
            (false, false)
        );
        // Guards against a divide-by-zero on a bogus rate.
        assert_eq!(
            dictation_duration_events(1_000_000, 0, false, false),
            (true, true)
        );
    }

    #[test]
    fn loopback_follows_default_when_output_changes_mid_meeting() {
        // The exact failure from the field: meeting on "Cuffie (HD 350BT)",
        // user unplugs/replugs, Windows flips the default to the Realtek
        // speakers, the loopback stays on the headphones (or vice-versa) and
        // records silence. With Mix/System we must rebind.
        assert!(loopback_should_follow_default(
            AudioSource::Mix,
            Some("Cuffie (HD 350BT)"),
            Some("Altoparlanti (2- Realtek(R) Audio)"),
        ));
        assert!(loopback_should_follow_default(
            AudioSource::System,
            Some("Altoparlanti (2- Realtek(R) Audio)"),
            Some("Cuffie (HD 350BT)"),
        ));
    }

    #[test]
    fn loopback_does_not_follow_when_default_unchanged() {
        assert!(!loopback_should_follow_default(
            AudioSource::Mix,
            Some("Cuffie (HD 350BT)"),
            Some("Cuffie (HD 350BT)"),
        ));
    }

    #[test]
    fn loopback_follow_is_noop_for_mic_only_or_unknown_names() {
        // Mic-only never has a loopback to follow.
        assert!(!loopback_should_follow_default(
            AudioSource::Mic,
            Some("Cuffie (HD 350BT)"),
            Some("Altoparlanti (2- Realtek(R) Audio)"),
        ));
        // A transient "no default output" tick (None) must not thrash streams.
        assert!(!loopback_should_follow_default(
            AudioSource::Mix,
            Some("Cuffie (HD 350BT)"),
            None,
        ));
        // No loopback bound yet → nothing to follow.
        assert!(!loopback_should_follow_default(
            AudioSource::Mix,
            None,
            Some("Cuffie (HD 350BT)"),
        ));
    }

    #[test]
    fn encode_wav_produces_valid_header() {
        let samples = vec![0.0f32; 100];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed");

        // RIFF header check
        assert_eq!(&wav[0..4], b"RIFF", "Missing RIFF header");
        assert_eq!(&wav[8..12], b"WAVE", "Missing WAVE identifier");

        // Read with hound to validate structure
        let reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 44100);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    }

    #[test]
    fn encode_wav_correct_sample_count() {
        let samples = vec![0.5, -0.5, 0.0, 1.0, -1.0];
        let wav = encode_wav(&samples, 16000).expect("encode_wav failed");

        let reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        assert_eq!(reader.len() as usize, samples.len());
        assert_eq!(reader.spec().sample_rate, 16000);
    }

    #[test]
    fn encode_wav_clamps_out_of_range() {
        // Values beyond [-1.0, 1.0] should be clamped, not wrap
        let samples = vec![2.0, -2.0, 5.0];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed");

        let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(decoded[0], 32767, "Positive overflow not clamped to max");
        assert_eq!(decoded[1], -32768, "Negative overflow not clamped to min");
        assert_eq!(decoded[2], 32767, "Large positive not clamped");
    }

    #[test]
    fn encode_wav_silence_is_zeros() {
        let samples = vec![0.0f32; 50];
        let wav = encode_wav(&samples, 48000).expect("encode_wav failed");

        let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert!(
            decoded.iter().all(|&s| s == 0),
            "Silent input must produce zero samples"
        );
    }

    #[test]
    fn encode_wav_empty_input() {
        let samples: Vec<f32> = vec![];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed on empty input");

        let reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        assert_eq!(reader.len(), 0);
    }

    #[test]
    fn encode_wav_preserves_polarity() {
        let samples = vec![0.5f32, -0.5f32];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed");

        let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert!(decoded[0] > 0, "Positive sample must produce positive i16");
        assert!(decoded[1] < 0, "Negative sample must produce negative i16");
    }

    #[test]
    fn device_sample_rate_returns_valid_rate() {
        // With None (default device), should return a reasonable rate
        let rate = device_sample_rate(&None);
        // Valid rates: 8000, 16000, 22050, 44100, 48000, 96000, etc.
        assert!(rate >= 8000, "Sample rate too low: {}", rate);
        assert!(rate <= 192000, "Sample rate too high: {}", rate);
    }

    #[test]
    fn device_sample_rate_unknown_device_falls_back() {
        // Non-existent device name should fall back to default, not panic
        let rate = device_sample_rate(&Some("NonExistentDevice12345".to_string()));
        assert!(rate >= 8000, "Fallback rate too low: {}", rate);
    }

    #[test]
    fn list_input_devices_does_not_panic() {
        // Should return a list (possibly empty on CI), never panic
        let devices = list_input_devices();
        // Just verify it returns without panic — count may be 0 in headless env
        assert!(devices.len() < 1000, "Unreasonable device count");
    }

    #[test]
    fn typed_pipeline_produces_valid_wav() {
        let samples = vec![0.5f32; 48000]; // 1 second at 48kHz
        let raw = RawAudio {
            samples,
            sample_rate: 48000,
        };
        // Skip preprocessing (no nnnoiseless in test env without audio device)
        let processed = raw.preprocess(false);
        assert!(!processed.is_empty());
        assert_eq!(processed.sample_rate, 48000);

        let payload = processed.to_wav_payload().expect("to_wav_payload failed");
        assert!(payload.data.len() > 0);
        // 48kHz → 16kHz = 1/3 samples, so ~1 second of audio at 16kHz
        assert!((payload.duration_secs - 1.0).abs() < 0.1);

        // Verify it's valid WAV
        let reader =
            hound::WavReader::new(Cursor::new(&payload.data)).expect("hound can't read WAV");
        assert_eq!(reader.spec().sample_rate, 16000);
    }

    #[test]
    fn raw_audio_to_wav_preserves_rate() {
        let raw = RawAudio {
            samples: vec![0.0f32; 100],
            sample_rate: 44100,
        };
        let wav = raw.to_wav().expect("to_wav failed");
        let reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        assert_eq!(reader.spec().sample_rate, 44100);
    }

    #[test]
    fn spawn_audio_thread_responds_to_stop() {
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits()));
        let secondary = Arc::new(Mutex::new(Vec::<f32>::new()));
        let lgain = Arc::new(std::sync::atomic::AtomicU32::new(2.0f32.to_bits()));
        let tx = spawn_audio_thread(buffer.clone(), secondary, gain, lgain);
        // Sending Stop should not panic even with no prior Start
        let _ = tx.send(AudioCommand::Stop);
        // Drop sender — thread should exit cleanly
        drop(tx);
    }

    #[test]
    fn spawn_audio_thread_with_half_gain_stop() {
        // gain=0.5 should not panic on Stop
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(0.5f32.to_bits()));
        let secondary = Arc::new(Mutex::new(Vec::<f32>::new()));
        let lgain = Arc::new(std::sync::atomic::AtomicU32::new(2.0f32.to_bits()));
        let tx = spawn_audio_thread(buffer.clone(), secondary, gain, lgain);
        let _ = tx.send(AudioCommand::Stop);
        drop(tx);
    }

    #[test]
    fn spawn_audio_thread_with_zero_gain_stop() {
        // gain=0.0 should not panic on Stop
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(0.0f32.to_bits()));
        let secondary = Arc::new(Mutex::new(Vec::<f32>::new()));
        let lgain = Arc::new(std::sync::atomic::AtomicU32::new(2.0f32.to_bits()));
        let tx = spawn_audio_thread(buffer.clone(), secondary, gain, lgain);
        let _ = tx.send(AudioCommand::Stop);
        drop(tx);
    }

    // ── WAV size estimation ──

    #[test]
    fn estimate_wav_size_1sec_48khz() {
        // 1 sec at 48kHz → 16000 samples at 16kHz → 32000 bytes + 44 header = 32044
        let audio = ProcessedAudio {
            samples: vec![0.0; 48000],
            sample_rate: 48000,
        };
        let est = audio.estimate_wav_size();
        assert!(
            (est as i64 - 32044).abs() < 100,
            "Expected ~32044, got {}",
            est
        );
    }

    #[test]
    fn estimate_wav_size_30min() {
        let audio = ProcessedAudio {
            samples: vec![0.0; 48000 * 30 * 60],
            sample_rate: 48000,
        };
        let est = audio.estimate_wav_size();
        // 30min at 16kHz 16-bit mono = ~57.6 MB
        assert!(
            est > 50_000_000 && est < 60_000_000,
            "30min should be ~57MB, got {}",
            est
        );
    }

    #[test]
    fn estimate_wav_size_empty() {
        let audio = ProcessedAudio {
            samples: vec![],
            sample_rate: 48000,
        };
        // Empty audio → just the 44-byte header
        assert_eq!(audio.estimate_wav_size(), 44);
    }

    #[test]
    fn estimate_vs_actual_wav_size() {
        // Estimate should be within 1% of actual encoded size
        let audio = ProcessedAudio {
            samples: vec![0.5; 48000 * 10], // 10 sec
            sample_rate: 48000,
        };
        let estimated = audio.estimate_wav_size();
        // Actually encode to compare
        let resampled = crate::preprocess::downsample_to_16k(&audio.samples, audio.sample_rate);
        let actual = encode_wav(&resampled, 16000).unwrap().len();
        let diff = (estimated as f64 - actual as f64).abs() / actual as f64;
        assert!(
            diff < 0.01,
            "Estimate {} vs actual {} differs by {:.1}%",
            estimated,
            actual,
            diff * 100.0
        );
    }

    // ── Audio splitting ──

    #[test]
    fn split_short_audio_no_split() {
        let audio = ProcessedAudio {
            samples: vec![0.5; 48000 * 5],
            sample_rate: 48000,
        };
        let chunks = audio.split_at_silence(48000 * 600); // 600s max
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].samples.len(), 48000 * 5);
        assert_eq!(chunks[0].sample_rate, 48000);
    }

    #[test]
    fn split_at_silence_boundary() {
        // 25s: 10s speech + 1s silence + 14s speech. Max chunk = 12s.
        // First chunk should cut at silence (~11s), second chunk = 12s, third = remainder
        let sr: usize = 48000;
        let mut samples = vec![0.5f32; sr * 10];
        samples.extend(vec![0.0f32; sr]);
        samples.extend(vec![0.5f32; sr * 14]);
        let audio = ProcessedAudio {
            samples,
            sample_rate: sr as u32,
        };
        let chunks = audio.split_at_silence(sr * 12);
        assert!(
            chunks.len() >= 2,
            "Should split, got {} chunks",
            chunks.len()
        );
        // First chunk should find the silence and cut there (~11s, not 12s)
        assert!(
            chunks[0].samples.len() <= sr * 11 + sr / 2,
            "First chunk should split at silence near 11s, got {}s",
            chunks[0].samples.len() / sr
        );
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(chunk.samples.len() <= sr * 12, "Chunk {} exceeds max", i);
        }
    }

    #[test]
    fn split_force_at_max_no_silence() {
        // Continuous speech → must force-split at max
        let sr: usize = 48000;
        let audio = ProcessedAudio {
            samples: vec![0.5f32; sr * 25],
            sample_rate: sr as u32,
        };
        let chunks = audio.split_at_silence(sr * 10);
        assert!(
            chunks.len() >= 3,
            "25s / 10s = at least 3 chunks, got {}",
            chunks.len()
        );
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(
                chunk.samples.len() <= sr * 10,
                "Chunk {} exceeds max: {}s",
                i,
                chunk.samples.len() / sr
            );
        }
    }

    #[test]
    fn split_preserves_all_samples() {
        let sr: usize = 48000;
        let total = sr * 20;
        let audio = ProcessedAudio {
            samples: (0..total).map(|i| (i as f32 / sr as f32).sin()).collect(),
            sample_rate: sr as u32,
        };
        let chunks = audio.split_at_silence(sr * 8);
        let reconstructed: usize = chunks.iter().map(|c| c.samples.len()).sum();
        assert_eq!(reconstructed, total, "Split must not lose samples");
    }

    #[test]
    fn split_preserves_sample_rate() {
        let audio = ProcessedAudio {
            samples: vec![0.5; 48000 * 20],
            sample_rate: 44100,
        };
        let chunks = audio.split_at_silence(48000 * 8);
        for chunk in &chunks {
            assert_eq!(chunk.sample_rate, 44100, "Chunk must inherit sample rate");
        }
    }

    #[test]
    fn split_single_sample_over_max() {
        // Edge case: max_chunk_samples = 0 should not infinite loop
        // We'll use 1 as max to avoid division issues
        let audio = ProcessedAudio {
            samples: vec![0.5; 100],
            sample_rate: 48000,
        };
        let chunks = audio.split_at_silence(1);
        assert_eq!(chunks.iter().map(|c| c.samples.len()).sum::<usize>(), 100);
    }
}

#[cfg(test)]
mod device_rate_tests {
    use super::*;

    /// Single test rather than two — the two ran in parallel by default
    /// and raced on the shared atomic.
    #[test]
    fn device_rate_store_load_round_trip() {
        ACTIVE_MIC_DEVICE_RATE.store(0, Ordering::Relaxed);
        assert_eq!(active_mic_device_rate(), 0);
        ACTIVE_MIC_DEVICE_RATE.store(16_000, Ordering::Relaxed);
        assert_eq!(active_mic_device_rate(), 16_000);
        ACTIVE_MIC_DEVICE_RATE.store(0, Ordering::Relaxed);
        assert_eq!(active_mic_device_rate(), 0);
    }
}

#[cfg(test)]
mod diag_tests {
    /// Mirrors the inline peak-tracking expression inside the
    /// PushLoopback arm. Guards the f32::abs / fold pattern against
    /// regression.
    fn peak_for(samples: &[f32]) -> f32 {
        samples.iter().map(|s| s.abs()).fold(0.0_f32, f32::max)
    }

    #[test]
    fn peak_zero_for_all_zero_buffer() {
        let zeros = vec![0.0_f32; 4800];
        assert_eq!(peak_for(&zeros), 0.0);
    }

    #[test]
    fn peak_tracks_max_abs() {
        let samples = [0.0_f32, -0.3, 0.1, -0.95, 0.2];
        assert!((peak_for(&samples) - 0.95).abs() < f32::EPSILON);
    }
}
