//! Long-form meeting-mode recording.
//!
//! Distinct from the live-dictation path in two ways:
//! 1. **Streaming-to-disk**: the WAV is written incrementally as the
//!    user speaks (~115 MB / hour at 16 kHz mono int16). Memory stays
//!    bounded regardless of meeting length, and a `.recording` marker
//!    file lets a future startup detect crashed sessions and offer
//!    recovery.
//! 2. **Post-process pipeline**: at stop, the full transcript is sent
//!    to the LLM once to produce a structured artifact (recap +
//!    action items + optional translation). Live captioning happens
//!    via the same chunked engine the dictation path uses, with a
//!    longer chunk window (15 s) tuned for context not interactivity.
//!
//! On-disk layout per meeting (`<config>/meetings/<id>/`):
//! - `audio.wav`        — 16 kHz mono int16 stream, finalized at stop
//! - `transcripts.txt`  — one line per chunk, prefixed with `[ts_ms]`
//! - `meta.json`        — start_ts, sample_rate, last_chunk_ts (live)
//! - `recap.md`         — post-stop LLM output (only after stop)
//! - `actions.json`     — post-stop LLM output (only after stop)
//! - `.recording`       — marker file deleted on clean stop;
//!                        presence at startup → "recover meeting?"
//!
//! Identifiers are RFC4122 v4 UUIDs so two meetings started on the
//! same wall-clock second don't collide.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default chunk window for live transcript when the user hasn't
/// configured `meeting_chunk_secs`. Cloud STT (Groq / OpenAI / Deepgram)
/// happily takes 60 s, so longer chunks give the LLM more context and
/// reduce per-call overhead at the cost of later transcript visibility.
/// Override at runtime via the AppState.meeting_chunk_secs config knob.
const DEFAULT_CHUNK_SECS: f32 = 15.0;
const DEFAULT_OVERLAP_MS: u32 = 500;
const FSYNC_INTERVAL: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Snapshot of the user's STT preferences taken at meeting-start time.
/// Worker uses this to route each chunk to the SAME backend the
/// dictation pipeline would use (cloud or local), so meeting and
/// dictation never disagree on which engine transcribes.
#[derive(Clone, Debug)]
pub struct SttSnapshot {
    pub mode: String, // "cloud" | "local"
    pub api_url: String,
    pub api_model: String,
    pub api_key: Option<String>, // None for local
    pub prompt: String,
    pub local_model: String, // whisper filename, looked up in models/ dir
    /// Local STT backend: "whisper" | "parakeet". Mirrors the
    /// `local_stt_backend` config knob the dictation chunked path
    /// already honours. Defaults to "whisper" when missing.
    pub local_backend: String,
    pub language: String,
    /// User-configurable chunk window — falls back to DEFAULT_CHUNK_SECS
    /// when None (e.g. older callers).
    pub chunk_secs: Option<f32>,
}

/// Encode 16 kHz mono f32 PCM into an in-memory WAV byte buffer suitable
/// for cloud STT upload. No I/O — pure computation.
fn pcm16k_to_wav_bytes(pcm_16k: &[f32]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
    if let Ok(mut w) = hound::WavWriter::new(&mut cursor, spec) {
        for s in pcm_16k {
            let clamped = s.clamp(-1.0, 1.0);
            let i = (clamped * i16::MAX as f32) as i16;
            let _ = w.write_sample(i);
        }
        let _ = w.finalize();
    }
    cursor.into_inner()
}

/// A single meeting audio track sink. Records to **Ogg/Vorbis** (compact,
/// ~10× smaller than int16 WAV) when libvorbis initialises; falls back to
/// int16 **WAV** per-track if the encoder can't start, so recording never
/// fails for lack of the codec. The on-disk extension reflects the chosen
/// format (`.ogg` vs `.wav`) — the host handles both: old meetings are
/// `.wav`, and re-transcription + waveform peaks decode either format via
/// the shared Symphonia/hound path (`dimmy_transcribe_file`,
/// `dimmy_compute_audio_peaks`).
enum TrackSink {
    // Sink is the raw `File` (no userspace BufWriter): `encode_audio_block`
    // writes complete Ogg pages straight to the OS as they're produced, so
    // an app crash mid-meeting loses nothing already encoded (the file is a
    // valid, decodable Ogg stream up to the last page — just missing the
    // end-of-stream marker, which Symphonia tolerates).
    // Boxed: the Vorbis encoder is far larger than the WAV writer, so an
    // unboxed variant bloats every TrackSink to the encoder's size
    // (clippy `large_enum_variant`).
    Ogg(Box<vorbis_rs::VorbisEncoder<File>>),
    Wav(hound::WavWriter<std::io::BufWriter<File>>),
}

impl TrackSink {
    /// Create a mono sink for `<dir>/<base>`: tries `<base>.ogg`, falls
    /// back to `<base>.wav` if the Vorbis encoder can't initialise.
    fn create(dir: &std::path::Path, base: &str, sample_rate: u32) -> Result<TrackSink, String> {
        // Ogg/Vorbis recording is enabled on Windows + macOS — both
        // host UIs now resolve `.ogg` with `.wav` fallback (playback +
        // waveform peaks + regenerate-transcript) via their respective
        // helpers (`MeetingViewModel.resolveMeetingAudio` on Mac,
        // `MeetingRecapHelpers.ResolveAudioTrack` on Win). Linux UI
        // hasn't caught up yet → stays on WAV until it does. `cfg!`
        // (not `#[cfg]`) keeps BOTH arms compiling on every target so
        // CI lints / type-checks the Ogg path even on Linux builds.
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            let rate = std::num::NonZeroU32::new(sample_rate.max(1))
                .unwrap_or_else(|| std::num::NonZeroU32::new(48_000).unwrap());
            let mono = std::num::NonZeroU8::new(1).unwrap();
            let ogg_path = dir.join(format!("{base}.ogg"));
            match File::create(&ogg_path) {
                Ok(f) => match vorbis_rs::VorbisEncoderBuilder::new(rate, mono, f) {
                    Ok(mut builder) => match builder.build() {
                        Ok(enc) => return Ok(TrackSink::Ogg(Box::new(enc))),
                        Err(e) => {
                            crate::log(&format!(
                                "[Meeting] vorbis build {base}.ogg failed: {e}; using WAV"
                            ));
                            let _ = std::fs::remove_file(&ogg_path);
                        }
                    },
                    Err(e) => {
                        crate::log(&format!(
                            "[Meeting] vorbis init {base}.ogg failed: {e}; using WAV"
                        ));
                        let _ = std::fs::remove_file(&ogg_path);
                    }
                },
                Err(e) => crate::log(&format!(
                    "[Meeting] create {base}.ogg failed: {e}; using WAV"
                )),
            }
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sample_rate.max(1),
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let w = hound::WavWriter::create(dir.join(format!("{base}.wav")), spec)
            .map_err(|e| format!("wav create {base}.wav: {e}"))?;
        Ok(TrackSink::Wav(w))
    }

    /// Append a window of mono f32 samples. Ogg encodes directly; WAV
    /// clamps to [-1, 1] and converts to int16.
    fn write(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        match self {
            TrackSink::Ogg(enc) => {
                if let Err(e) = enc.encode_audio_block([samples]) {
                    crate::log(&format!("[Meeting] vorbis encode failed: {e}"));
                }
            }
            TrackSink::Wav(w) => {
                for s in samples {
                    let clamped = s.clamp(-1.0, 1.0);
                    let _ = w.write_sample((clamped * i16::MAX as f32) as i16);
                }
            }
        }
    }

    /// Flush buffered data. Ogg writes pages incrementally inside
    /// `write`, so only the WAV path needs an explicit flush.
    fn flush(&mut self) {
        if let TrackSink::Wav(w) = self {
            let _ = w.flush();
        }
    }

    /// Finalise the stream (Ogg trailer / WAV header) and close.
    fn finalize(self) {
        match self {
            TrackSink::Ogg(enc) => {
                if let Err(e) = enc.finish() {
                    crate::log(&format!("[Meeting] vorbis finish failed: {e}"));
                }
            }
            TrackSink::Wav(w) => {
                if let Err(e) = w.finalize() {
                    crate::log(&format!("[Meeting] wav finalize failed: {e}"));
                }
            }
        }
    }
}

pub struct MeetingSession {
    id: String,
    dir: PathBuf,
    cancel: Arc<AtomicBool>,
    /// User-driven pause toggle. While true, the worker leaves
    /// audio_buffer / audio_buffer_secondary growing (cpal callbacks
    /// keep firing — we don't bounce the streams) but stops draining
    /// them, stops writing to the WAV files, and stops emitting STT
    /// chunks. On resume the worker advances last_processed +
    /// samples_written past the paused window so the gap doesn't end
    /// up in the audio file or in the transcript timeline.
    paused: Arc<AtomicBool>,
    handle: Option<JoinHandle<MeetingResult>>,
}

#[derive(Clone, Debug)]
pub struct MeetingResult {
    pub id: String,
    pub dir: PathBuf,
    pub transcript: String,
    pub duration_secs: f64,
    pub chunk_count: u32,
    pub error: Option<String>,
}

impl MeetingSession {
    /// Create the meeting directory + marker, open audio.wav writer,
    /// spawn the worker thread. The worker keeps reading from
    /// `audio_buffer` (which the cpal callback fills concurrently)
    /// and writes / transcribes incrementally until `stop()` is called.
    pub fn start(
        audio_buffer: Arc<Mutex<Vec<f32>>>,
        audio_buffer_secondary: Arc<Mutex<Vec<f32>>>,
        device_sample_rate: u32,
        system_sample_rate: u32,
        source: crate::audio::AudioSource,
        stt: SttSnapshot,
    ) -> Result<Self, String> {
        assert!(
            device_sample_rate > 0,
            "device_sample_rate must be positive"
        );
        let id = uuid_v4_simple();
        let dir = crate::meetings_dir()
            .ok_or_else(|| "config dir unavailable".to_string())?
            .join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {:?}: {}", dir, e))?;

        // Marker file — deleted on clean stop, leftover after a crash.
        let marker = dir.join(".recording");
        std::fs::write(&marker, "1").map_err(|e| format!("marker write: {}", e))?;

        let meta_initial = serde_json::json!({
            "id": id,
            "started_at": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            "device_sample_rate": device_sample_rate,
            "chunk_secs": DEFAULT_CHUNK_SECS,
            "overlap_ms": DEFAULT_OVERLAP_MS,
        });
        std::fs::write(
            dir.join("meta.json"),
            serde_json::to_string_pretty(&meta_initial).unwrap_or_default(),
        )
        .map_err(|e| format!("meta write: {}", e))?;

        // Audio capture is now stored at the device's NATIVE sample
        // rate (typically 48 kHz on modern systems) instead of being
        // downsampled to 16 kHz on disk. Music / YouTube no longer
        // sounds telephone-ish on playback. STT chunks are downsampled
        // to 16 kHz only at inference time; what's on disk is the full
        // bandwidth signal.
        //
        // In Mix mode we also write two separate per-track files so
        // the user (and a future diarization pass) can reprocess the
        // streams independently:
        //   - audio.wav         = mix (mic + system, what you hear)
        //   - audio_mic.wav     = AEC-cleaned mic only
        //   - audio_system.wav  = raw loopback only (Mix mode only)
        // Per-track WAV files use their RESPECTIVE device's native rate.
        // audio_mic.wav  -> primary device (the mic) sr
        // audio_system.wav -> loopback device sr (typically 48 kHz on
        //                     speakers, but lower with some BT setups)
        // audio.wav (mix) -> primary's sr; per-sample mix synchronises to
        //                    primary's clock so mic content is correct.
        //                    System content in the mix may be slightly
        //                    rate-distorted when system_sample_rate differs;
        //                    the per-track audio_system.wav stays the
        //                    source of truth for system audio and plays
        //                    back at correct speed.
        crate::log(&format!(
            "[Meeting] audio tracks: mic={} Hz, system={} Hz (Ogg/Vorbis, WAV fallback)",
            device_sample_rate, system_sample_rate
        ));
        // Track sinks are created INSIDE the worker thread (the Vorbis
        // encoder holds raw libvorbis pointers and is !Send, so it can't
        // be moved across the spawn boundary). The worker owns them end
        // to end.

        let cancel = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let cancel_w = cancel.clone();
        let paused_w = paused.clone();
        let dir_w = dir.clone();
        let id_w = id.clone();

        let handle = thread::Builder::new()
            .name(format!("meeting-{}", &id[..8]))
            .spawn(move || {
                worker_loop(
                    audio_buffer,
                    audio_buffer_secondary,
                    device_sample_rate,
                    system_sample_rate,
                    source,
                    stt,
                    dir_w,
                    id_w,
                    cancel_w,
                    paused_w,
                )
            })
            .map_err(|e| format!("spawn meeting worker: {}", e))?;

        crate::log(&format!("[Meeting] started id={} dir={:?}", id, dir));
        crate::telemetry::track(crate::telemetry::Event::MeetingStarted);
        Ok(Self {
            id,
            dir,
            cancel,
            paused,
            handle: Some(handle),
        })
    }

    /// Pause the meeting. Idempotent: a second pause while already
    /// paused is a no-op (no double markers in transcripts.txt).
    /// Returns true if the state actually flipped, false if it was
    /// already paused.
    pub fn pause(&self) -> bool {
        let was_paused = self.paused.swap(true, Ordering::SeqCst);
        if !was_paused {
            crate::log(&format!("[Meeting] pause id={}", self.id));
            crate::telemetry::track(crate::telemetry::Event::MeetingPaused);
            true
        } else {
            false
        }
    }

    /// Resume after pause. Idempotent. Returns true if the state
    /// actually flipped (i.e. we WERE paused).
    pub fn resume(&self) -> bool {
        let was_paused = self.paused.swap(false, Ordering::SeqCst);
        if was_paused {
            crate::log(&format!("[Meeting] resume id={}", self.id));
            crate::telemetry::track(crate::telemetry::Event::MeetingResumed);
            true
        } else {
            false
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Signal the worker, join, finalize. Returns the result bundle —
    /// caller is then expected to invoke the post-processing pipeline
    /// (LLM recap + actions) and persist the artifacts.
    pub fn stop(mut self) -> MeetingResult {
        self.cancel.store(true, Ordering::SeqCst);
        let handle = self.handle.take();
        let result = handle
            .map(|h| h.join().ok())
            .unwrap_or(None)
            .unwrap_or(MeetingResult {
                id: self.id.clone(),
                dir: self.dir.clone(),
                transcript: String::new(),
                duration_secs: 0.0,
                chunk_count: 0,
                error: Some("worker panicked".into()),
            });
        // Marker is removed only on clean exit so a crash leaves it.
        let _ = std::fs::remove_file(self.dir.join(".recording"));
        crate::log(&format!(
            "[Meeting] stopped id={} duration={:.1}s chunks={} err={:?}",
            result.id, result.duration_secs, result.chunk_count, result.error
        ));
        // Bucketed stop metrics — never the raw count.
        let word_count = result.transcript.split_whitespace().count() as u32;
        crate::telemetry::track(crate::telemetry::Event::MeetingStopped {
            duration_bucket: crate::telemetry::sanitize::bucket_audio_secs(result.duration_secs),
            words_bucket: crate::telemetry::sanitize::bucket_word_count(word_count),
            // The recap fires later in the C#/Swift host after stop()
            // returns — emitted separately via the typed dispatcher
            // when it actually completes. We only know here that the
            // user pressed Stop, not whether the recap pipeline ran.
            had_recap: false,
        });
        result
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

/// Read `buf[start..end]`, zero-filling any portion past `buf.len()`.
/// Always returns exactly `end - start` samples so the meeting's three WAV
/// writers stay in lockstep even when one source has no samples for this
/// window (e.g. the mic track on a microphone-less machine).
fn slice_or_zeros(buf: &[f32], start: usize, end: usize) -> Vec<f32> {
    assert!(
        start <= end,
        "slice_or_zeros: start {} > end {}",
        start,
        end
    );
    let win = end - start;
    let mut out = Vec::with_capacity(win);
    let avail_end = end.min(buf.len());
    if start < avail_end {
        out.extend_from_slice(&buf[start..avail_end]);
    }
    out.resize(win, 0.0);
    assert_eq!(
        out.len(),
        win,
        "slice_or_zeros postcondition: window length"
    );
    out
}

/// One-time decision: should the meeting clock off the SECONDARY (system)
/// track instead of the primary (mic)? True only when the mic has produced
/// NOTHING yet AND the system track has already accumulated `mic_grace`
/// samples — i.e. there is definitively no microphone (a present mic always
/// fills the primary buffer within a fraction of a second of capture start).
/// The caller latches the result so a mic that starts a beat late still
/// clocks on the mic (no samples lost) and the clock never flip-flops.
fn no_mic_detected(primary_len: usize, secondary_len: usize, mic_grace: usize) -> bool {
    primary_len == 0 && secondary_len >= mic_grace
}

/// Mix two equal-length per-track windows into the meeting's `audio.wav`
/// stream: `mic[i] + system[i]` clamped to [-1, 1]. Both inputs come from
/// `slice_or_zeros`, so they're guaranteed equal length.
fn mix_windows(mic: &[f32], system: &[f32]) -> Vec<f32> {
    assert_eq!(
        mic.len(),
        system.len(),
        "mix_windows: mic/system window length mismatch"
    );
    mic.iter()
        .zip(system.iter())
        .map(|(&m, &s)| (m + s).clamp(-1.0, 1.0))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    audio_buffer_secondary: Arc<Mutex<Vec<f32>>>,
    device_sample_rate: u32,
    system_sample_rate: u32,
    source: crate::audio::AudioSource,
    stt: SttSnapshot,
    dir: PathBuf,
    id: String,
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
) -> MeetingResult {
    let chunk_secs = stt
        .chunk_secs
        .map(|s| s.clamp(5.0, 60.0))
        .unwrap_or(DEFAULT_CHUNK_SECS);
    let chunk_samples = (chunk_secs * device_sample_rate as f32) as usize;
    let overlap_samples =
        ((DEFAULT_OVERLAP_MS as f32 / 1000.0) * device_sample_rate as f32) as usize;
    let mix_active = matches!(source, crate::audio::AudioSource::Mix);
    let local_model_filename = stt.local_model.clone();
    let language = stt.language.clone();
    crate::log(&format!(
        "[Meeting] worker source={:?} mix_active={} stt_mode={} local_backend={} model={} lang={}",
        source, mix_active, stt.mode, stt.local_backend, local_model_filename, language
    ));

    // Resolve a usable local-whisper model up-front. If the user has a
    // configured `local_model` that doesn't exist on disk, fall back to
    // any other .bin in the models dir — better than silently producing
    // empty transcripts. Cloud mode ignores this.
    let resolved_local_model: Option<std::path::PathBuf> = if stt.mode == "cloud" {
        None
    } else {
        let primary = crate::local_stt::model_path(&local_model_filename);
        if primary.is_file() {
            Some(primary)
        } else {
            let dir = primary.parent().map(|p| p.to_path_buf());
            let alt = dir.and_then(|d| {
                std::fs::read_dir(&d).ok().and_then(|rd| {
                    rd.filter_map(|e| e.ok().map(|e| e.path())).find(|p| {
                        p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("bin")
                    })
                })
            });
            if let Some(ref p) = alt {
                crate::log(&format!(
                    "[Meeting] configured local_model='{}' missing — falling back to '{}'",
                    local_model_filename,
                    p.file_name().and_then(|s| s.to_str()).unwrap_or("?")
                ));
            } else {
                crate::log(&format!(
                    "[Meeting] configured local_model='{}' missing and no fallback found in models/",
                    local_model_filename
                ));
            }
            alt
        }
    };

    // Tokio runtime for cloud STT calls (built only if needed).
    let cloud_rt: Option<tokio::runtime::Runtime> = if stt.mode == "cloud" {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => Some(rt),
            Err(e) => {
                crate::log(&format!("[Meeting] tokio runtime build failed: {}", e));
                None
            }
        }
    } else {
        None
    };

    // `samples_written` tracks how many SOURCE-RATE samples have been
    // streamed into the WAV (per-track WAVs all advance in lockstep
    // with the synth stream, so a single cursor covers all three).
    let mut samples_written: usize = 0;
    let mut last_processed: usize = 0;
    let mut last_fsync = Instant::now();
    // Per-speaker accumulators. dedup_last_3_words is stateful per
    // speaker so they get independent contexts. The merged ordered
    // transcript lives in transcripts.txt (one labeled line per chunk
    // emitted in time order).
    let mut mic_accum = String::new();
    let mut system_accum = String::new();
    let mut chunk_count: u32 = 0;
    let started = Instant::now();
    // Speaker labels routed by AudioSource. Mic-only and Mix put the
    // user-mic stream as "mic"; System-only puts the loopback as
    // "system" since there's no mic in that mode.
    let mic_label = match source {
        crate::audio::AudioSource::System => "system",
        _ => "mic",
    };

    let transcripts_path = dir.join("transcripts.txt");
    let mut transcripts_file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcripts_path)
    {
        Ok(f) => f,
        Err(e) => {
            return MeetingResult {
                id,
                dir,
                transcript: String::new(),
                duration_secs: 0.0,
                chunk_count: 0,
                error: Some(format!("open transcripts.txt: {}", e)),
            };
        }
    };

    // Track sinks — created HERE (on the worker thread) because the
    // Vorbis encoder is !Send and can't cross the spawn boundary. Each
    // tries Ogg/Vorbis, falls back to WAV. audio(.ogg)=mix @ primary
    // rate, audio_mic=cleaned mic @ primary rate, audio_system=raw
    // loopback @ system rate (Mix mode only).
    let make_sink = |base: &str, rate: u32| -> Result<TrackSink, MeetingResult> {
        TrackSink::create(&dir, base, rate).map_err(|e| MeetingResult {
            id: id.clone(),
            dir: dir.clone(),
            transcript: String::new(),
            duration_secs: 0.0,
            chunk_count: 0,
            error: Some(e),
        })
    };
    let mut writer = match make_sink("audio", device_sample_rate) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut writer_mic = match make_sink("audio_mic", device_sample_rate) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut writer_system = if mix_active {
        match make_sink("audio_system", system_sample_rate) {
            Ok(s) => Some(s),
            Err(r) => return r,
        }
    } else {
        None
    };

    // Helper: read a slice of the SYNTHESIZED mix stream at the
    // primary's sample rate. Indices `start..end` are in mic-rate
    // samples. When the system loopback runs at a different rate
    // (typical: BT-HFP mic 16k + speakers A2DP 48k), the secondary
    // buffer fills `system_sample_rate / device_sample_rate` times
    // faster — we map each mic-rate sample to its time-aligned
    // secondary index via the rate ratio so the mix is
    // time-coherent. Without this, the mix WAV plays system content
    // at the wrong speed (3x slow mumbling at low pitch when
    // mic_sr=16k & system_sr=48k were mixed into a 16k container).
    // Both buffers are at the same canonical rate (48 kHz, enforced
    // by the resampler in audio.rs callbacks), so this is always 1.0.
    // Kept for backward compatibility — read-only diagnostic at info
    // level on entry.
    let _rate_ratio = system_sample_rate as f64 / device_sample_rate as f64;

    // ── Continuous secondary alignment ─────────────────────────
    //
    // The WASAPI loopback driver does NOT emit callbacks while the
    // default output device is silent (no apps producing audio).
    // The user-reported scenario "I talk to the mic for 10 s, THEN
    // I start the Teams call" makes the loopback wake up only at
    // t=10s. Without intervention, secondary[0] would land at
    // primary index 0, mixing the LATE loopback sample with the
    // EARLY mic sample → 10 s wall-clock skew.
    //
    // Robust fix: enforce the invariant `secondary.len() ==
    // primary.len()` at the top of every worker tick. We pad the
    // secondary buffer with zeros up to the primary's length under
    // a lock. When the loopback finally fires its first callback,
    // its samples are appended at the END of the padded zeros — at
    // a buffer index that corresponds to "wall-clock NOW" both for
    // primary and secondary. From that point on, every new chunk
    // appended by the cpal callback maps 1:1 to the simultaneously
    // arriving mic samples.
    //
    // Effect across the whole meeting:
    //   - audio_mic.wav    : full meeting duration
    //   - audio_system.wav : full meeting duration (zeros while the
    //                        loopback was dormant; real audio after)
    //   - audio.wav (mix)  : primary[i] + secondary[i], always
    //                        time-aligned regardless of when (or
    //                        how many times) the loopback woke up
    //
    // No timeout, no polling magic, no offset bookkeeping — just a
    // single resize() per tick under lock.
    let align_secondary =
        |audio_buffer: &Arc<Mutex<Vec<f32>>>, audio_buffer_secondary: &Arc<Mutex<Vec<f32>>>| {
            if !mix_active {
                return;
            }
            let p_len = audio_buffer.lock().map(|b| b.len()).unwrap_or(0);
            if let Ok(mut s) = audio_buffer_secondary.lock() {
                if s.len() < p_len {
                    s.resize(p_len, 0.0);
                }
            }
        };

    // Mic-presence latch. The meeting clock is normally driven by the
    // PRIMARY (mic) buffer length — every WAV + STT cursor advances against
    // it. But a machine with NO microphone (no input device: the cpal
    // worker opens no stream, so `audio_buffer` never grows) would then
    // record NOTHING, silently discarding the system audio the tap IS
    // capturing into `audio_buffer_secondary`. So if the mic has produced
    // nothing after ~2 s of system-only audio, latch the clock onto the
    // secondary track (mic track becomes pure silence). Decided exactly
    // once: a mic that starts a beat late still clocks on the mic (the
    // grace window is generous), and the clock never flip-flops. On
    // Windows the loopback IS the primary device when there's no mic, so
    // `audio_buffer` always grows and this never triggers — no regression.
    let mic_grace_samples = (device_sample_rate as usize) * 2;
    let mut clock_decided = false;
    let mut clock_on_secondary = false;
    // Effective synth-stream length up to which we can drain this tick:
    // the secondary (system) buffer once the no-mic latch flipped, the
    // primary (mic) buffer otherwise.
    let effective_len = |clock_on_secondary: bool,
                         audio_buffer: &Arc<Mutex<Vec<f32>>>,
                         audio_buffer_secondary: &Arc<Mutex<Vec<f32>>>|
     -> Option<usize> {
        if clock_on_secondary {
            audio_buffer_secondary.lock().ok().map(|b| b.len())
        } else {
            audio_buffer.lock().ok().map(|b| b.len())
        }
    };

    // Track pause transitions so we can both (a) skip the paused
    // window when writing/transcribing and (b) emit a [paused N s]
    // marker to transcripts.txt on resume.
    let mut was_paused = false;
    let mut pause_started_at: Option<Instant> = None;

    loop {
        let cancelled = cancel.load(Ordering::SeqCst);
        thread::sleep(POLL_INTERVAL);

        // Enforce the secondary-tracks-primary invariant. Zero-pads
        // the loopback buffer up to the mic buffer's length, so
        // every WAV cursor we maintain sees a time-coherent pair.
        align_secondary(&audio_buffer, &audio_buffer_secondary);

        // Decide the clock source once (see mic-presence latch above).
        if mix_active && !clock_decided {
            let p_len = audio_buffer.lock().map(|b| b.len()).unwrap_or(0);
            if p_len > 0 {
                clock_decided = true; // mic present → primary clock (default path)
            } else {
                let s_len = audio_buffer_secondary.lock().map(|b| b.len()).unwrap_or(0);
                if no_mic_detected(p_len, s_len, mic_grace_samples) {
                    clock_on_secondary = true;
                    clock_decided = true;
                    crate::log(
                        "[Meeting] no microphone detected — clocking off system audio (mic track will be silent)",
                    );
                }
            }
        }

        // Pause gate. While paused: cpal is still filling the audio
        // buffers (we don't bounce the streams — that would race with
        // device acquisition on resume) but the worker simply doesn't
        // drain or transcribe. On resume, advance samples_written +
        // last_processed to the current buffer length so the paused
        // window doesn't end up in the WAV files or in the chunked
        // transcript timeline. A `[paused Ns]` marker is written
        // into transcripts.txt at the seam so the recap LLM sees the
        // discontinuity.
        let is_paused_now = paused.load(Ordering::SeqCst);
        if is_paused_now && !cancelled {
            if !was_paused {
                was_paused = true;
                pause_started_at = Some(Instant::now());
            }
            continue;
        }
        if was_paused && (!is_paused_now || cancelled) {
            // Resume edge OR stop-while-paused. In both cases drop the
            // paused window: advance the cursors past whatever cpal has
            // accumulated, so the audio captured during the user's
            // bathroom break / call interruption never lands in the
            // WAV files or the chunked transcript.
            was_paused = false;
            let dur_ms = pause_started_at
                .map(|t| t.elapsed().as_millis())
                .unwrap_or(0);
            pause_started_at = None;
            let snap = effective_len(clock_on_secondary, &audio_buffer, &audio_buffer_secondary)
                .unwrap_or_default();
            crate::log(&format!(
                "[Meeting] {}{} ms — skipping {} mic-rate samples",
                if cancelled {
                    "stop while paused after "
                } else {
                    "resumed after "
                },
                dur_ms,
                snap.saturating_sub(samples_written)
            ));
            samples_written = snap;
            last_processed = snap;
            // Note the gap in the transcripts file so the recap LLM
            // sees the timeline jump.
            let elapsed_ms = started.elapsed().as_millis();
            let line = format!(
                "[{:>6} ms] [paused] (resumed after {} ms)\n",
                elapsed_ms, dur_ms
            );
            let _ = transcripts_file.write_all(line.as_bytes());
            let _ = transcripts_file.flush();
        }

        // Take a snapshot of the SYNCED stream length up to which we'll
        // process this iteration (mic clock by default, system clock once
        // the no-mic latch flipped).
        let buf_len_now =
            match effective_len(clock_on_secondary, &audio_buffer, &audio_buffer_secondary) {
                Some(n) => n,
                None => continue,
            };

        // Stream new samples into the WAV files at NATIVE sample rate
        // (no downsample). Three writers fan out:
        //   audio.wav         = mix (synth = primary + secondary clamped)
        //   audio_mic.wav     = primary buffer (cleaned mic post-AEC)
        //   audio_system.wav  = secondary buffer (raw loopback) — Mix only
        if buf_len_now > samples_written {
            // Per-track windows for this tick. `slice_or_zeros` copies ONLY
            // the [samples_written, buf_len_now] window (not the whole
            // buffer) and zero-fills any short tail, so all three vecs are
            // EXACTLY that window long and the WAV writers stay in lockstep.
            // With a mic present this is byte-identical to the old read
            // (primary[w], secondary[w], their clamped sum); with no mic the
            // mic window is silence and the mix IS the system audio. Each
            // buffer is locked once, briefly.
            let win = buf_len_now - samples_written;
            let new_mic: Vec<f32> = match audio_buffer.lock() {
                Ok(b) => slice_or_zeros(&b, samples_written, buf_len_now),
                Err(_) => vec![0.0; win],
            };
            let new_system: Vec<f32> = if mix_active {
                match audio_buffer_secondary.lock() {
                    Ok(b) => slice_or_zeros(&b, samples_written, buf_len_now),
                    Err(_) => vec![0.0; win],
                }
            } else {
                Vec::new()
            };
            let new_synth: Vec<f32> = if mix_active {
                mix_windows(&new_mic, &new_system)
            } else {
                new_mic.clone()
            };

            // Fan out to the three track sinks (Ogg/Vorbis or WAV fallback;
            // each handles its own f32→encoded conversion + error logging).
            writer.write(&new_synth);
            writer_mic.write(&new_mic);
            if let Some(ref mut w) = writer_system {
                w.write(&new_system);
            }
            samples_written = buf_len_now;

            if last_fsync.elapsed() >= FSYNC_INTERVAL {
                writer.flush();
                writer_mic.flush();
                if let Some(ref mut w) = writer_system {
                    w.flush();
                }
                last_fsync = Instant::now();
            }
        }

        // Fire a chunk transcribe if enough new audio has accumulated.
        // Whisper.cpp via local_stt::transcribe_local — the same path
        // dictation uses. Falls in line with whatever local model the
        // user has configured (no hardcoded engine).
        let want_end = last_processed + chunk_samples;
        if buf_len_now >= want_end || (cancelled && buf_len_now > last_processed) {
            let start = last_processed.saturating_sub(overlap_samples);
            let end = if cancelled {
                buf_len_now
            } else {
                want_end.min(buf_len_now)
            };

            // Per-track chunk reads. mic_chunk = primary buffer slice
            // (cleaned mic in Mix mode, raw input in Mic-only or the
            // loopback in System-only). system_chunk = secondary buffer
            // slice — only populated in Mix mode.
            let mic_chunk: Vec<f32> = match audio_buffer.lock() {
                Ok(b) if end <= b.len() => b[start..end].to_vec(),
                _ => Vec::new(),
            };
            // System chunk: aligned 1:1 with mic chunk thanks to the
            // align_secondary invariant; same window indices.
            let system_chunk: Vec<f32> = if mix_active {
                match audio_buffer_secondary.lock() {
                    Ok(b) if end <= b.len() => b[start..end].to_vec(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };

            // Helper: downsample a slice (if needed) and run STT through
            // whichever backend the user has configured. The caller MUST
            // pass the source rate of THE SPECIFIC slice — mic chunks
            // come at device_sample_rate, system chunks at system_sample_rate
            // (different in BT-HFP-mic + speakers-A2DP-loopback setups).
            // Mixing them up = sending 48k-sampled data with a 16k WAV
            // header to the cloud STT, which Gemini returns as empty
            // because the speech is 3x compressed and unintelligible.
            let transcribe = |slice: Vec<f32>, source_sr: u32| -> String {
                if slice.is_empty() {
                    return String::new();
                }
                let pcm_16k = if source_sr == 16_000 {
                    slice
                } else {
                    crate::preprocess::downsample_to_16k(&slice, source_sr)
                };
                if pcm_16k.is_empty() {
                    return String::new();
                }
                if stt.mode == "cloud" {
                    let wav = pcm16k_to_wav_bytes(&pcm_16k);
                    match (&cloud_rt, &stt.api_key) {
                        (Some(rt), Some(key)) => {
                            // First call only: dump url/model/key-present
                            // so 404s and auth bugs are debuggable.
                            static LOGGED: std::sync::Once = std::sync::Once::new();
                            LOGGED.call_once(|| {
                                let key_tail = if key.len() > 4 {
                                    &key[key.len() - 4..]
                                } else {
                                    "?"
                                };
                                crate::log(&format!(
                                    "[Meeting] cloud STT: POST {} model={} lang={} key_suffix=...{} wav_bytes={}",
                                    stt.api_url, stt.api_model, language, key_tail, wav.len()
                                ));
                            });
                            let result = rt.block_on(async {
                                crate::transcribe::transcribe_audio(
                                    &stt.api_url,
                                    &stt.api_model,
                                    key,
                                    &wav,
                                    &language,
                                    &stt.prompt,
                                )
                                .await
                            });
                            match result {
                                Ok(t) => t,
                                Err(e) => {
                                    crate::log(&format!("[Meeting] cloud STT error: {}", e));
                                    String::new()
                                }
                            }
                        }
                        _ => {
                            crate::log("[Meeting] cloud STT unavailable: missing key or runtime");
                            String::new()
                        }
                    }
                } else if stt.local_backend == "parakeet" {
                    // Parakeet TDT v3 — same path the dictation chunked
                    // worker uses. The 3 ONNX sessions live in a global
                    // OnceLock cache so each call only pays mel/encoder/
                    // decoder inference, not model load. No .bin file
                    // needed — the bundle ships separately.
                    match crate::parakeet::transcribe(&pcm_16k) {
                        Ok(t) => t,
                        Err(e) => {
                            crate::log(&format!("[Meeting] parakeet error: {}", e));
                            String::new()
                        }
                    }
                } else {
                    // Default = whisper. Routes through the cached
                    // WhisperContext via local_stt::transcribe_local.
                    match &resolved_local_model {
                        Some(model_path) => {
                            match crate::local_stt::transcribe_local(
                                model_path,
                                &pcm_16k,
                                &language,
                                &stt.prompt,
                            ) {
                                Ok(t) => t,
                                Err(e) => {
                                    crate::log(&format!("[Meeting] whisper error: {}", e));
                                    String::new()
                                }
                            }
                        }
                        None => {
                            crate::log("[Meeting] local STT skipped: no usable .bin model");
                            String::new()
                        }
                    }
                }
            };

            let mic_text = transcribe(mic_chunk, device_sample_rate);
            let system_text = if mix_active {
                transcribe(system_chunk, system_sample_rate)
            } else {
                String::new()
            };
            let elapsed_ms = started.elapsed().as_millis();

            // Append helper: dedup vs the per-speaker accumulator,
            // append the delta to the speaker's accumulator, emit a
            // labeled line into transcripts.txt, AND fire a
            // `meeting_chunk` event so host UIs can refresh their
            // live transcript view without polling the on-disk file.
            // Replaces the 1-2 s DispatcherTimer polling that Win
            // MeetingWindow used to run on transcripts.txt — event-
            // driven so Mac (which had no equivalent poll) also lights
            // up, and idle CPU drops to zero between chunks.
            let dir_str = dir.to_string_lossy().to_string();
            let mut emit = |speaker: &str, text: &str, accum: &mut String| {
                if text.trim().is_empty() {
                    return;
                }
                let delta = crate::chunked_stt::dedup_last_3_words(accum, text);
                if delta.is_empty() {
                    return;
                }
                if !accum.is_empty() && !accum.ends_with(' ') {
                    accum.push(' ');
                }
                accum.push_str(&delta);
                chunk_count += 1;
                let line = format!("[{:>6} ms] [{}] {}\n", elapsed_ms, speaker, delta);
                let _ = transcripts_file.write_all(line.as_bytes());
                let _ = transcripts_file.flush();

                // Push event to host UIs. Payload mirrors the line
                // we just wrote so consumers can either re-render
                // from transcripts.txt at open + tail this event, or
                // build their own rolling buffer from `line` deltas.
                let payload = serde_json::json!({
                    "dir": dir_str,
                    "speaker": speaker,
                    "elapsed_ms": elapsed_ms as u64,
                    "chunk_count": chunk_count,
                    "line": line,
                })
                .to_string();
                crate::ffi::emit_event("meeting_chunk", &payload);
            };

            emit(mic_label, &mic_text, &mut mic_accum);
            if mix_active {
                emit("system", &system_text, &mut system_accum);
            }

            // Stats: this chunk contributed transcribed words +
            // captured audio time. Mirror the bookkeeping that
            // `dimmy_stop_recording` does for pill dictation so the
            // Settings → Stats card ("Total words", "Time saved")
            // reflects meeting usage too. Time is counted ONCE per
            // chunk window (mic + system cover the same wall-time);
            // words sum across both speakers.
            //
            // We approximate words from the FULL chunk text (not the
            // post-dedup `delta`) — the overlap dedup removes 1-3
            // words per chunk-pair which is ~1-3% over-count, well
            // inside "Settings shows an approximate counter" budget.
            // The line that lands on disk in transcripts.txt is the
            // delta — but the user's STT minutes are spent on the
            // full chunk, so counting full text is also more honest
            // about throughput.
            let chunk_words = mic_text.split_whitespace().count() as std::os::raw::c_int
                + if mix_active {
                    system_text.split_whitespace().count() as std::os::raw::c_int
                } else {
                    0
                };
            let chunk_secs = if device_sample_rate > 0 {
                (end - start) as f64 / device_sample_rate as f64
            } else {
                0.0
            };
            if chunk_words > 0 || chunk_secs > 0.0 {
                let _ = crate::ffi::dimmy_update_stats(chunk_words, chunk_secs);
            }

            last_processed = end;
        }

        if cancelled {
            break;
        }
    }

    // Finalize all three track sinks (Ogg trailer / WAV header) + meta.
    writer.finalize();
    writer_mic.finalize();
    if let Some(w) = writer_system {
        w.finalize();
    }
    let duration_secs = started.elapsed().as_secs_f64();
    finalize_meeting_meta(&dir, &id, duration_secs, chunk_count);

    // Build the final transcript: time-ordered labeled stream read
    // back from transcripts.txt (one line per chunk, format
    // `[ts ms] [speaker] text`). The `[ts ms]` prefix is preserved
    // so the LLM recap can use timestamps for diarization context.
    // Falls back to a per-speaker concat if the file read fails.
    let merged_transcript = std::fs::read_to_string(&transcripts_path)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            let mut s = String::new();
            if !mic_accum.trim().is_empty() {
                s.push_str(&format!("[{}] {}\n", mic_label, mic_accum));
            }
            if !system_accum.trim().is_empty() {
                s.push_str(&format!("[system] {}\n", system_accum));
            }
            s
        });

    MeetingResult {
        id,
        dir,
        transcript: merged_transcript,
        duration_secs,
        chunk_count,
        error: None,
    }
}

/// Tiny RFC4122-v4 UUID generator. Avoids pulling in a uuid crate
/// for one usage. Variant + version bits set per spec; randomness
/// from the OS via the rand crate (already a transitive dep).
fn uuid_v4_simple() -> String {
    let mut bytes = [0u8; 16];
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Mix counter + nanos into the 16 bytes — not cryptographically
    // strong but sufficient for unique per-meeting directory names.
    let mut seed = now_ns ^ (counter.wrapping_mul(0x9E3779B97F4A7C15));
    for b in bytes.iter_mut() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (seed >> 56) as u8;
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // RFC4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Persist the post-process LLM output (recap + actions) into the
/// meeting directory. The C#/Swift host calls this after running
/// `dimmy_process_with_llm` against the full transcript. Splitting
/// LLM execution from persistence keeps Rust free of HTTP-runtime
/// concerns specific to the meeting flow — the existing LLM client
/// is already wired through dimmy_process_with_llm.
///
/// Also extracts the meeting title from the first Markdown H1 of the
/// recap and writes it into `meta.json`. All recap templates across
/// providers (cloud, local, CLI subscription, Claude Desktop MCP)
/// follow the same convention — first line is `# Short title`. The
/// extracted title is what the UI shows in the meeting list; if no
/// title is parseable the UI falls back to the meeting id.
pub fn save_post_process(
    meeting_dir: &std::path::Path,
    recap_md: &str,
    actions_json: &str,
    translated: Option<&str>,
) -> Result<(), String> {
    if !recap_md.trim().is_empty() {
        std::fs::write(meeting_dir.join("recap.md"), recap_md)
            .map_err(|e| format!("write recap.md: {}", e))?;
        // Title sync: best-effort, never fail the save.
        if let Some(title) = parse_recap_title(recap_md) {
            update_meeting_meta_title(meeting_dir, &title);
        }
    }
    if !actions_json.trim().is_empty() {
        std::fs::write(meeting_dir.join("actions.json"), actions_json)
            .map_err(|e| format!("write actions.json: {}", e))?;
    }
    if let Some(t) = translated {
        if !t.trim().is_empty() {
            std::fs::write(meeting_dir.join("translated.txt"), t)
                .map_err(|e| format!("write translated.txt: {}", e))?;
        }
    }
    Ok(())
}

/// Extract a meeting title from a recap Markdown blob. Rule: the
/// first non-empty line must be a Markdown H1 (`# Title`). Returns
/// None if the first non-empty line is not a heading, or the title
/// is empty / unreasonably long (>200 chars guards against the LLM
/// emitting the entire recap on a single H1 line).
///
/// This is the single source of truth used everywhere a recap lands
/// — the FFI `save_post_process` path and the standalone mcp-server
/// `save_recap` tool both call `parse_recap_title` to keep the title
/// in meta.json in sync regardless of which provider produced the
/// recap.
pub fn parse_recap_title(recap_md: &str) -> Option<String> {
    for line in recap_md.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim().trim_matches(|c: char| {
                // Strip trailing punctuation noise the LLM sometimes
                // leaks (`# Title.` or `# Title :`).
                c == '.' || c == ':' || c == ',' || c.is_whitespace()
            });
            if title.is_empty() || title.len() > 200 {
                return None;
            }
            return Some(title.to_string());
        }
        // First non-empty line wasn't a heading — abort. Don't scan
        // further lines; the convention is "title is first or
        // nothing".
        return None;
    }
    None
}

/// Update `<meeting_dir>/meta.json` with the given `title` field,
/// preserving every other field already in the file. Best-effort —
/// silent on errors because the title is metadata polish, not load-
/// bearing data.
pub fn update_meeting_meta_title(meeting_dir: &std::path::Path, title: &str) {
    let path = meeting_dir.join("meta.json");
    let mut obj: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    obj.insert("title".into(), serde_json::Value::String(title.to_string()));
    if let Ok(serialized) = serde_json::to_string_pretty(&obj) {
        let _ = std::fs::write(&path, serialized);
    }
}

/// Merge the end-of-meeting fields into the existing `meta.json` instead
/// of replacing the file. The start-time write (`started_at`, `id`,
/// `device_sample_rate`, …) MUST survive: the UI orders the meeting list
/// by `started_at` (falling back to file mtime), so a wholesale rewrite
/// here — which dropped `started_at` — meant a later title edit (which
/// also rewrites meta.json and bumps the dir mtime) made the meeting jump
/// to the top of the list. Preserving `started_at` keeps the date stable.
fn finalize_meeting_meta(
    meeting_dir: &std::path::Path,
    id: &str,
    duration_secs: f64,
    chunk_count: u32,
) {
    let path = meeting_dir.join("meta.json");
    let mut obj: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("duration_secs".into(), serde_json::json!(duration_secs));
    obj.insert("chunk_count".into(), serde_json::json!(chunk_count));
    obj.insert(
        "ended_at".into(),
        serde_json::json!(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)),
    );
    if let Ok(serialized) = serde_json::to_string_pretty(&obj) {
        let _ = std::fs::write(&path, serialized);
    }
}

/// Lazy backfill: if `meta.json` has no `title` but `recap.md` exists
/// and contains a parseable first-line H1, write it. Called by the
/// UI on meeting open so old meetings recorded before this schema
/// land bring their titles forward without a batch migration.
/// Returns the title if one was written or already present.
pub fn backfill_meeting_title(meeting_dir: &std::path::Path) -> Option<String> {
    let meta_path = meeting_dir.join("meta.json");
    let existing: Option<String> = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("title")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        });
    if let Some(t) = existing {
        if !t.is_empty() {
            return Some(t);
        }
    }
    let recap = std::fs::read_to_string(meeting_dir.join("recap.md")).ok()?;
    let title = parse_recap_title(&recap)?;
    update_meeting_meta_title(meeting_dir, &title);
    Some(title)
}

/// Scan the meetings dir for sessions with a leftover `.recording`
/// marker. These are the crash-recoverable orphans the UI surfaces
/// at startup. Returns a JSON array of metadata so the C#/Swift host
/// can populate a "Recover meeting" prompt without needing its own
/// directory parser.
pub fn list_orphans() -> Vec<serde_json::Value> {
    let Some(base) = crate::meetings_dir() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let marker = dir.join(".recording");
        if !marker.exists() {
            continue;
        }
        // Best-effort: read meta.json + transcripts.txt size.
        let meta_str = std::fs::read_to_string(dir.join("meta.json")).unwrap_or_default();
        let meta_json: serde_json::Value =
            serde_json::from_str(&meta_str).unwrap_or(serde_json::Value::Null);
        let transcript_size = std::fs::metadata(dir.join("transcripts.txt"))
            .map(|m| m.len())
            .unwrap_or(0);
        out.push(serde_json::json!({
            "id": dir.file_name().and_then(|n| n.to_str()).unwrap_or(""),
            "dir": dir.to_string_lossy(),
            "meta": meta_json,
            "transcript_size_bytes": transcript_size,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── No-mic system-audio recording (mic-clock-driven worker fix) ──

    #[test]
    fn slice_or_zeros_exact_window_when_in_bounds() {
        let buf = [1.0_f32, 2.0, 3.0, 4.0];
        assert_eq!(slice_or_zeros(&buf, 1, 3), vec![2.0, 3.0]);
        assert_eq!(slice_or_zeros(&buf, 0, 4), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn slice_or_zeros_zero_fills_short_tail() {
        let buf = [1.0_f32, 2.0, 3.0];
        // Window extends past the buffer → real samples then zeros.
        assert_eq!(slice_or_zeros(&buf, 1, 5), vec![2.0, 3.0, 0.0, 0.0]);
    }

    #[test]
    fn slice_or_zeros_all_zeros_when_buffer_empty() {
        // The mic-less case: primary buffer is empty, the window must still
        // come back full-length (all silence) so the WAV writers stay in
        // lockstep with the system track.
        let empty: [f32; 0] = [];
        assert_eq!(slice_or_zeros(&empty, 0, 3), vec![0.0, 0.0, 0.0]);
        // Start beyond a non-empty buffer → all zeros too.
        let buf = [1.0_f32, 2.0];
        assert_eq!(slice_or_zeros(&buf, 5, 8), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn slice_or_zeros_empty_window() {
        let buf = [1.0_f32, 2.0, 3.0];
        assert_eq!(slice_or_zeros(&buf, 2, 2), Vec::<f32>::new());
    }

    #[test]
    fn no_mic_detected_only_when_primary_empty_and_grace_met() {
        // Mic present (primary grew) → never switch to the secondary clock.
        assert!(!no_mic_detected(1, 1_000_000, 96_000));
        // Mic empty but not enough system audio yet → wait (mic may be late).
        assert!(!no_mic_detected(0, 95_999, 96_000));
        // Mic empty AND system has crossed the grace window → no mic.
        assert!(no_mic_detected(0, 96_000, 96_000));
        assert!(no_mic_detected(0, 200_000, 96_000));
    }

    #[test]
    fn mix_windows_sums_and_clamps() {
        let mic = [0.5_f32, -0.5, 0.8, -0.8];
        let sys = [0.25_f32, -0.25, 0.8, -0.8];
        // 0.8+0.8=1.6 → clamps to 1.0; -1.6 → -1.0.
        assert_eq!(mix_windows(&mic, &sys), vec![0.75, -0.75, 1.0, -1.0]);
    }

    #[test]
    fn mix_windows_no_mic_equals_system() {
        // The fix's core promise: with the mic track all-silence, the mix
        // IS the system audio (so a mic-less meeting records the call).
        let mic = [0.0_f32, 0.0, 0.0];
        let sys = [0.3_f32, -0.4, 0.5];
        assert_eq!(mix_windows(&mic, &sys), vec![0.3, -0.4, 0.5]);
    }

    #[test]
    fn no_mic_windows_stay_in_lockstep() {
        // End-to-end of the per-tick windowing for a mic-less machine:
        // empty primary, growing secondary. All three WAV windows must be
        // the SAME length (else the .wav files desync), mic = silence,
        // mix = system.
        let primary: [f32; 0] = [];
        let secondary = [0.1_f32, 0.2, 0.3, 0.4, 0.5];
        let (start, end) = (1usize, 4usize);
        let new_mic = slice_or_zeros(&primary, start, end);
        let new_system = slice_or_zeros(&secondary, start, end);
        let new_synth = mix_windows(&new_mic, &new_system);
        assert_eq!(new_mic.len(), end - start);
        assert_eq!(new_system.len(), end - start);
        assert_eq!(new_synth.len(), end - start);
        assert_eq!(new_mic, vec![0.0, 0.0, 0.0]);
        assert_eq!(new_system, vec![0.2, 0.3, 0.4]);
        assert_eq!(new_synth, vec![0.2, 0.3, 0.4]);
    }

    #[test]
    fn mic_present_windows_match_legacy_read() {
        // Regression guard: when a mic IS present (primary == secondary
        // length after align), the windows equal the pre-fix behaviour —
        // exact primary slice, exact secondary slice, clamped sum.
        let primary = [0.1_f32, 0.2, 0.3, 0.4];
        let secondary = [0.05_f32, 0.05, 0.05, 0.05];
        let (start, end) = (1usize, 4usize);
        let new_mic = slice_or_zeros(&primary, start, end);
        let new_system = slice_or_zeros(&secondary, start, end);
        let new_synth = mix_windows(&new_mic, &new_system);
        assert_eq!(new_mic, vec![0.2, 0.3, 0.4]);
        assert_eq!(new_system, vec![0.05, 0.05, 0.05]);
        let expected = [0.25_f32, 0.35, 0.45];
        assert_eq!(new_synth.len(), expected.len());
        for (got, want) in new_synth.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-6, "got {got} want {want}");
        }
    }

    #[test]
    fn uuid_v4_format() {
        let id = uuid_v4_simple();
        assert_eq!(id.len(), 36);
        let chars: Vec<char> = id.chars().collect();
        assert_eq!(chars[8], '-');
        assert_eq!(chars[13], '-');
        assert_eq!(chars[18], '-');
        assert_eq!(chars[23], '-');
        // version-4 nibble at position 14
        assert_eq!(chars[14], '4');
    }

    #[test]
    fn uuid_v4_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            seen.insert(uuid_v4_simple());
        }
        assert_eq!(seen.len(), 1000);
    }

    #[test]
    fn finalize_meta_preserves_started_at() {
        // Reproduces the date-jump bug: the stop-time meta write used to
        // replace the file, dropping `started_at`; the UI then ordered by
        // file mtime, so editing a title reordered the meeting.
        let tmp = std::env::temp_dir().join(format!("dimmy_meta_{}", uuid_v4_simple()));
        std::fs::create_dir_all(&tmp).unwrap();
        let initial = serde_json::json!({
            "id": "abc",
            "started_at": 1234.5_f64,
            "device_sample_rate": 48000,
        });
        std::fs::write(
            tmp.join("meta.json"),
            serde_json::to_string(&initial).unwrap(),
        )
        .unwrap();

        finalize_meeting_meta(&tmp, "abc", 42.0, 7);

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("meta.json")).unwrap()).unwrap();
        // Start-time fields survive the merge…
        assert_eq!(v["started_at"].as_f64(), Some(1234.5));
        assert_eq!(v["device_sample_rate"].as_i64(), Some(48000));
        // …and the end-of-meeting fields are written.
        assert_eq!(v["duration_secs"].as_f64(), Some(42.0));
        assert_eq!(v["chunk_count"].as_u64(), Some(7));
        assert!(v["ended_at"].as_f64().unwrap() > 0.0);

        // A subsequent title edit must not clobber started_at either.
        update_meeting_meta_title(&tmp, "My Meeting");
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("meta.json")).unwrap()).unwrap();
        assert_eq!(v2["started_at"].as_f64(), Some(1234.5));
        assert_eq!(v2["title"].as_str(), Some("My Meeting"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── TrackSink: meeting audio persistence (Ogg on Windows, WAV elsewhere) ──

    #[test]
    fn track_sink_preserves_every_sample() {
        // Regression guard for the recording path: a TrackSink must produce
        // a finalized, decodable file with every written sample preserved.
        // On non-Windows (incl. macOS, which records meetings as WAV) this
        // exercises the exact WAV branch the Mac meeting recorder relies on.
        let dir = std::env::temp_dir().join(format!("dimmy_tracksink_{}", uuid_v4_simple()));
        std::fs::create_dir_all(&dir).unwrap();

        let samples: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut sink = TrackSink::create(&dir, "audio_mic", 48_000).expect("sink create");
        sink.write(&samples);
        sink.flush();
        sink.finalize();

        if cfg!(any(target_os = "windows", target_os = "macos")) {
            let ogg = dir.join("audio_mic.ogg");
            assert!(ogg.exists(), "Windows/macOS must record .ogg");
            let bytes = std::fs::read(&ogg).unwrap();
            assert_eq!(&bytes[0..4], b"OggS", "valid Ogg stream magic");
        } else {
            let wav = dir.join("audio_mic.wav");
            assert!(
                wav.exists(),
                "Linux must record .wav (gate not widened yet)"
            );
            let reader = hound::WavReader::open(&wav).unwrap();
            let spec = reader.spec();
            assert_eq!(spec.sample_rate, 48_000);
            assert_eq!(spec.channels, 1);
            assert_eq!(spec.bits_per_sample, 16);
            let decoded = reader.into_samples::<i16>().count();
            assert_eq!(decoded, samples.len(), "every sample preserved");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn track_sink_clamps_out_of_range_and_ignores_empty() {
        // The WAV branch must clamp to [-1, 1] before int16 conversion
        // (CLAUDE.md audio invariant — un-clamped values would wrap to the
        // wrong sign as int16). An empty window must be a no-op, never a
        // panic. On Windows this just proves the encoder accepts the input
        // without panicking and still produces a valid Ogg.
        let dir = std::env::temp_dir().join(format!("dimmy_tracksink_clamp_{}", uuid_v4_simple()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut sink = TrackSink::create(&dir, "audio", 16_000).expect("sink create");
        sink.write(&[]); // no-op, must not panic or write
        sink.write(&[2.0, -3.0, 0.5, -0.5]); // 2.0/-3.0 out of range
        sink.finalize();

        if cfg!(any(target_os = "windows", target_os = "macos")) {
            assert!(dir.join("audio.ogg").exists());
        } else {
            let wav = dir.join("audio.wav");
            let reader = hound::WavReader::open(&wav).unwrap();
            let decoded: Vec<i16> = reader.into_samples::<i16>().map(|s| s.unwrap()).collect();
            assert_eq!(decoded.len(), 4, "empty write skipped, 4 real samples kept");
            // 2.0 → clamp 1.0 → i16::MAX; -3.0 → clamp -1.0 → -i16::MAX.
            assert_eq!(decoded[0], i16::MAX);
            assert_eq!(decoded[1], -i16::MAX);
            // Clamp to [-1, 1] then *i16::MAX never reaches i16::MIN (-32768).
            for s in &decoded {
                assert!(*s >= -i16::MAX, "stays in clamped range");
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
