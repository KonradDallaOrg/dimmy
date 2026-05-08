use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Cursor;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

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
}

/// List available input device names.
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
    host.input_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Spawn a dedicated audio capture thread.
/// Returns a sender to control the thread.
/// Captured samples are written to the shared buffer.
/// `input_gain` is shared so it can be updated at runtime (0.0-2.0, default 1.0).
pub fn spawn_audio_thread(
    buffer: Arc<Mutex<Vec<f32>>>,
    buffer_secondary: Arc<Mutex<Vec<f32>>>,
    input_gain: Arc<std::sync::atomic::AtomicU32>,
) -> mpsc::Sender<AudioCommand> {
    let (tx, rx) = mpsc::channel::<AudioCommand>();

    thread::spawn(
        #[allow(unused_assignments, unused_variables)]
        move || {
            let host = cpal::default_host();
            // streams are held alive to keep recording; dropping/replacing
            // stops/starts. `Mix` mode keeps two streams alive in parallel.
            let mut streams: Vec<cpal::Stream> = Vec::new();

            // Event loop: wait for commands
            for cmd in rx {
                match cmd {
                    AudioCommand::Start {
                        device_name,
                        source,
                    } => {
                        crate::log(&format!(
                            "[Audio] Start command received, device_name={:?}, source={:?}",
                            device_name, source
                        ));
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

                        // Clear both buffers — Mix uses a separate secondary
                        // buffer for the loopback so meeting.rs can mix
                        // primary+secondary per-sample (instead of having
                        // them race-interleave into one buffer, which made
                        // playback half-speed and acoustically garbled).
                        if let Ok(mut b) = buffer.lock() {
                            b.clear();
                        }
                        if let Ok(mut b) = buffer_secondary.lock() {
                            b.clear();
                        }

                        let buf = buffer.clone();
                        let gain_ref = input_gain.clone();
                        let sample_count =
                            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                        let sc1 = sample_count.clone();
                        let s = match config.sample_format() {
                            cpal::SampleFormat::F32 => device.build_input_stream(
                                &config.clone().into(),
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
                                    if let Ok(mut b) = buf.lock() {
                                        if channels > 1 {
                                            for chunk in data.chunks(channels) {
                                                let mono = chunk.iter().sum::<f32>()
                                                    / channels as f32
                                                    * gain;
                                                b.push(mono);
                                            }
                                        } else if (gain - 1.0).abs() < 0.001 {
                                            b.extend_from_slice(data);
                                        } else {
                                            b.extend(data.iter().map(|&s| s * gain));
                                        }
                                    }
                                },
                                |err| crate::log(&format!("[Audio] Stream error (F32): {}", err)),
                                None,
                            ),
                            cpal::SampleFormat::I16 => {
                                let buf2 = buffer.clone();
                                let gain_ref2 = input_gain.clone();
                                let sc2 = sample_count.clone();
                                device.build_input_stream(
                                    &config.clone().into(),
                                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                                        let prev = sc2.fetch_add(data.len(), std::sync::atomic::Ordering::Relaxed);
                                        if prev == 0 {
                                            crate::log(&format!("[Audio] First I16 callback: {} samples", data.len()));
                                        }
                                        let gain = f32::from_bits(gain_ref2.load(std::sync::atomic::Ordering::Relaxed));
                                        assert!(gain.is_finite(), "audio callback I16: gain must be finite, got {}", gain);
                                        assert!((0.0..=2.0).contains(&gain), "audio callback I16: gain must be in [0.0, 2.0], got {}", gain);
                                        if let Ok(mut b) = buf2.lock() {
                                            for chunk in data.chunks(channels) {
                                                let mono: f32 = chunk
                                                    .iter()
                                                    .map(|&s| s as f32 / 32768.0)
                                                    .sum::<f32>()
                                                    / channels as f32 * gain;
                                                b.push(mono);
                                            }
                                        }
                                    },
                                    |err| crate::log(&format!("[Audio] Stream error (I16): {}", err)),
                                    None,
                                )
                            }
                            _ => {
                                crate::log(&format!(
                                    "[Audio] ERROR: Unsupported sample format: {:?}",
                                    config.sample_format()
                                ));
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
                                    &input_gain,
                                    /* is_loopback */ true,
                                ) {
                                    let _ = s.play();
                                    streams.push(s);
                                    crate::log(
                                        "[Audio] Mix mode: secondary loopback stream playing into dedicated buffer",
                                    );
                                }
                            }
                        }
                    }
                    AudioCommand::Stop => {
                        // Dropping the streams stops recording
                        streams.clear();
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
    input_gain: &Arc<std::sync::atomic::AtomicU32>,
    is_loopback: bool,
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
    // NOTE: secondary stream path = loopback (system audio). We deliberately
    // DO NOT apply `input_gain` here (that knob is calibrated for mic peak
    // levels) and DO NOT divide by `channels` on stereo→mono downmix. The
    // sum-clamp form preserves perceived loudness for center-panned voice
    // (typical YouTube/Teams content) instead of giving up 6dB to match the
    // mic stream — the bug the user described as "system practically
    // inaudible" with mic-quality vs system-quality wildly mismatched.
    let _ = input_gain; // intentionally unused on the loopback path
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.clone().into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if let Ok(mut b) = buf.lock() {
                    if channels > 1 {
                        for chunk in data.chunks(channels) {
                            let mono = chunk.iter().sum::<f32>().clamp(-1.0, 1.0);
                            b.push(mono);
                        }
                    } else {
                        b.extend_from_slice(data);
                    }
                }
            },
            |err| crate::log(&format!("[Audio] Secondary stream error (F32): {}", err)),
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.clone().into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                if let Ok(mut b) = buf.lock() {
                    for chunk in data.chunks(channels) {
                        let mono: f32 = chunk
                            .iter()
                            .map(|&s| s as f32 / 32768.0)
                            .sum::<f32>()
                            .clamp(-1.0, 1.0);
                        b.push(mono);
                    }
                }
            },
            |err| crate::log(&format!("[Audio] Secondary stream error (I16): {}", err)),
            None,
        ),
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
            let r = host
                .default_output_device()
                .and_then(|d| d.default_output_config().ok())
                .map(|c| c.sample_rate().0);
            #[cfg(not(target_os = "windows"))]
            let r: Option<u32> = None;
            r.unwrap_or_else(|| primary_sample_rate(device_name, &AudioSource::Mic))
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

    // Returned rate must be within valid audio hardware range
    assert!(
        rate >= 8000,
        "primary_sample_rate: rate {} below 8kHz minimum",
        rate
    );
    assert!(
        rate <= 192000,
        "primary_sample_rate: rate {} above 192kHz maximum",
        rate
    );

    rate
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
        let tx = spawn_audio_thread(buffer.clone(), secondary, gain);
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
        let tx = spawn_audio_thread(buffer.clone(), secondary, gain);
        let _ = tx.send(AudioCommand::Stop);
        drop(tx);
    }

    #[test]
    fn spawn_audio_thread_with_zero_gain_stop() {
        // gain=0.0 should not panic on Stop
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(0.0f32.to_bits()));
        let secondary = Arc::new(Mutex::new(Vec::<f32>::new()));
        let tx = spawn_audio_thread(buffer.clone(), secondary, gain);
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
