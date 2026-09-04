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

/// Outcome of a time-bounded thread join.
#[derive(Debug)]
pub(crate) enum BoundedJoin<T> {
    Done(T),
    Panicked,
    TimedOut,
}

/// Join `handle` but never block longer than `timeout`. If the joined
/// thread is wedged — e.g. pinned on the macOS 26 CoreAudio HAL
/// process-global lock during stream/tap teardown — this returns
/// `TimedOut` and leaves the thread running detached instead of hanging
/// the caller forever. This is the invariant that keeps `dimmy_meeting_stop`
/// (and therefore the whole app) from freezing on a stuck meeting worker.
pub(crate) fn join_bounded<T: Send + 'static>(
    handle: JoinHandle<T>,
    timeout: Duration,
) -> BoundedJoin<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(v)) => BoundedJoin::Done(v),
        Ok(Err(_)) => BoundedJoin::Panicked,
        Err(_) => BoundedJoin::TimedOut,
    }
}

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
    /// Mirrors the `preprocessing_enabled` config knob. Combined with `mode`
    /// it decides whether each chunk gets the RNNoise VAD trim before the
    /// model sees it — the same rule the batch dictation path applies
    /// (`preprocess_route(..) == Full`).
    pub preprocessing_enabled: bool,
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
                    Ok(mut builder) => {
                        // Default quality (0.5, ~80 kbit/s) rolls off highs and
                        // adds warble — inaudible on speech, but it makes the
                        // meeting loopback (music / system audio) sound muffled
                        // next to the source. Bump to 0.8 for faithful tracks.
                        builder.bitrate_management_strategy(
                            vorbis_rs::VorbisBitrateManagementStrategy::QualityVbr {
                                target_quality: 0.8,
                            },
                        );
                        match builder.build() {
                            Ok(enc) => return Ok(TrackSink::Ogg(Box::new(enc))),
                            Err(e) => {
                                crate::log(&format!(
                                    "[Meeting] vorbis build {base}.ogg failed: {e}; using WAV"
                                ));
                                let _ = std::fs::remove_file(&ogg_path);
                            }
                        }
                    }
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
    ///
    /// Returns the failure instead of swallowing it: a finalize error
    /// (classically disk-full while rewriting the RIFF size header)
    /// means the file on disk is INCOMPLETE — the caller must surface
    /// that in `MeetingResult.error` so the user learns the recording
    /// is damaged now, not when the recap mysteriously fails later.
    fn finalize(self) -> Result<(), String> {
        match self {
            TrackSink::Ogg(enc) => enc.finish().map(|_| ()).map_err(|e| {
                let msg = format!("vorbis finish failed: {e}");
                crate::log(&format!("[Meeting] {msg}"));
                msg
            }),
            TrackSink::Wav(w) => w.finalize().map_err(|e| {
                let msg = format!("wav finalize failed: {e}");
                crate::log(&format!("[Meeting] {msg}"));
                msg
            }),
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
        // Fresh session: the capture gate must be open regardless of how
        // the previous session ended (belt — stop() already clears it).
        crate::audio::MEETING_CAPTURE_GATED.store(false, Ordering::SeqCst);
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
            // Gate every capture append while paused: the resume cursor-jump
            // discards the paused window anyway, so buffering it is pure RAM
            // waste (~345 MB/h at 48 kHz). Audit 2026-07-02.
            crate::audio::MEETING_CAPTURE_GATED.store(true, Ordering::SeqCst);
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
            crate::audio::MEETING_CAPTURE_GATED.store(false, Ordering::SeqCst);
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
        // Never leave the capture gate latched — a stop-while-paused
        // would otherwise silence the NEXT recording's buffers.
        crate::audio::MEETING_CAPTURE_GATED.store(false, Ordering::SeqCst);
        self.cancel.store(true, Ordering::SeqCst);
        let handle = self.handle.take();
        // Bounded join. The worker normally exits within one chunk's STT
        // time after `cancel`, but on macOS 26 (Tahoe) a CoreAudio HAL
        // wedge during stream teardown can pin any thread that touches the
        // HAL indefinitely. A naked `h.join()` there would hang
        // `dimmy_meeting_stop` forever and freeze the caller. Joining on a
        // helper thread with a timeout guarantees stop ALWAYS returns: a
        // truly wedged worker leaks one thread and yields a partial recap
        // instead of freezing the app (Francesco, 2026-07-06).
        let result = match handle {
            Some(h) => match join_bounded(h, Duration::from_secs(120)) {
                BoundedJoin::Done(r) => r,
                BoundedJoin::Panicked => MeetingResult {
                    id: self.id.clone(),
                    dir: self.dir.clone(),
                    transcript: String::new(),
                    duration_secs: 0.0,
                    chunk_count: 0,
                    error: Some("worker panicked".into()),
                },
                BoundedJoin::TimedOut => {
                    crate::log("[Meeting] stop join TIMED OUT — worker wedged (likely a CoreAudio HAL lock); returning partial result so the app never freezes");
                    crate::telemetry::track(crate::telemetry::Event::MeetingStopTimeout);
                    MeetingResult {
                        id: self.id.clone(),
                        dir: self.dir.clone(),
                        transcript: String::new(),
                        duration_secs: 0.0,
                        chunk_count: 0,
                        error: Some(
                            "meeting stop timed out (audio subsystem wedged); recap may be incomplete"
                                .into(),
                        ),
                    }
                }
            },
            None => MeetingResult {
                id: self.id.clone(),
                dir: self.dir.clone(),
                transcript: String::new(),
                duration_secs: 0.0,
                chunk_count: 0,
                error: Some("worker never started".into()),
            },
        };
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

/// Soft-knee limiter: linear below ±KNEE, tanh-compressed above so a loud
/// mic+system overlap rounds off smoothly instead of hard-clipping to ±1.0
/// (which produces audible clicks / "distorted" peaks on music). Output is
/// always strictly inside (-1, 1).
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        (KNEE + (1.0 - KNEE) * over.tanh()).copysign(x)
    }
}

/// Mix two equal-length per-track windows into the meeting's `audio.wav`
/// stream: `mic[i] + system[i]` passed through `soft_clip`. Unity gain on
/// both tracks (real meetings need full mic voice); the soft limiter only
/// engages on peaks above the knee, replacing the old hard clamp. Both
/// inputs come from `slice_or_zeros`, so they're guaranteed equal length.
fn mix_windows(mic: &[f32], system: &[f32]) -> Vec<f32> {
    assert_eq!(
        mic.len(),
        system.len(),
        "mix_windows: mic/system window length mismatch"
    );
    mic.iter()
        .zip(system.iter())
        .map(|(&m, &s)| soft_clip(m + s))
        .collect()
}

/// Don't touch the buffer until at least this much is provably finished
/// with. Draining is a memmove of the retained tail under the capture lock,
/// so doing it every 100 ms tick would pay the cost constantly for nothing;
/// at 10 s the tail stays a few MiB and the memmove is well under a
/// millisecond, which the cpal callback can absorb.
const DRAIN_THRESHOLD_SAMPLES: usize = 48_000 * 10;

/// How many leading samples are provably finished with — written to disk AND
/// past the window the next chunk will read.
///
/// Two cursors index the same buffer and they do NOT move together: the
/// writer runs ahead, the chunk extractor trails it by up to a chunk, and
/// the next extraction starts `overlap` samples BEFORE `last_processed`.
/// The safe point is therefore the minimum of the two, minus the overlap —
/// get this wrong in either direction and you either leak memory or hand
/// whisper a window that starts mid-sentence.
///
/// Returns 0 when there is nothing worth reclaiming yet.
fn drainable_samples(
    samples_written: usize,
    last_processed: usize,
    overlap_samples: usize,
    threshold: usize,
) -> usize {
    let safe = samples_written.min(last_processed.saturating_sub(overlap_samples));
    if safe >= threshold {
        safe
    } else {
        0
    }
}

/// Coarse bucket for "how far into the meeting did this happen".
/// Categorical so telemetry never carries a raw timing.
fn bucket_elapsed_secs(secs: f64) -> &'static str {
    match secs {
        s if s < 30.0 => "lt_30",
        s if s < 120.0 => "30_120",
        s if s < 600.0 => "120_600",
        _ => "ge_600",
    }
}

/// Work handed from the capture worker to the transcription thread.
///
/// Owned data only: the capture worker copies the window out from under
/// the buffer lock and gives it away, so the transcription side never
/// touches a lock the audio callbacks are waiting on.
enum SttJob {
    Chunk {
        mic: Vec<f32>,
        system: Vec<f32>,
        elapsed_ms: u128,
    },
    /// A `[paused Ns]` marker, queued rather than written inline so it
    /// keeps its place in transcripts.txt relative to the chunks around it.
    Paused { elapsed_ms: u128, dur_ms: u128 },
}

/// How many chunk jobs may wait in the queue before the capture worker
/// starts dropping them.
///
/// This is the knob that decides what happens when the machine cannot
/// transcribe as fast as it records. It drops TRANSCRIPT, never audio:
/// the recording is already on disk and "Regenerate transcript" can redo
/// the whole thing afterwards from the file. Four jobs of a default 15 s
/// chunk at 48 kHz is roughly 24 MB of queued PCM — enough to absorb a
/// slow patch, small enough that a wedged transcriber cannot grow the
/// process without bound.
const STT_QUEUE_DEPTH: usize = 4;

/// Everything the transcription thread owns outright. Bundled so the
/// thread takes one argument instead of a dozen.
struct SttThreadCtx {
    stt: SttSnapshot,
    resolved_local_model: Option<std::path::PathBuf>,
    cloud_rt: Option<tokio::runtime::Runtime>,
    language: String,
    vad_trim: bool,
    mix_active: bool,
    mic_label: &'static str,
    device_sample_rate: u32,
    system_sample_rate: u32,
    dir_str: String,
    transcripts_file: std::fs::File,
    /// Shared so the capture worker can read the count even when the
    /// join times out on a wedged transcriber.
    chunk_count: Arc<std::sync::atomic::AtomicU32>,
}

/// Transcription thread: receives windows, runs STT, appends to
/// transcripts.txt, emits `meeting_chunk`.
///
/// Everything slow, everything that can wedge, and everything that calls
/// back into the host lives HERE — deliberately, because none of it may
/// ever stand between captured audio and the disk. See the module-level
/// note on `worker_loop`. Exits when the sender is dropped and the queue
/// has drained.
/// Returns the per-speaker accumulators joined as a fallback transcript,
/// used only when transcripts.txt cannot be read back at stop.
fn stt_thread_loop(rx: std::sync::mpsc::Receiver<SttJob>, mut ctx: SttThreadCtx) -> String {
    use std::io::Write as _;

    let mut mic_accum = String::new();
    let mut system_accum = String::new();

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
        // Silence trim before the model sees the window. An idle
        // chunk collapses to empty here and never reaches whisper,
        // which is what stops the phantom "grazie" on the mic track.
        let slice = if ctx.vad_trim {
            crate::preprocess::process_chunk_vad_only(&slice, source_sr)
        } else {
            slice
        };
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
        if ctx.stt.mode == "cloud" {
            let wav = pcm16k_to_wav_bytes(&pcm_16k);
            match (&ctx.cloud_rt, &ctx.stt.api_key) {
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
                            ctx.stt.api_url, ctx.stt.api_model, ctx.language, key_tail, wav.len()
                        ));
                    });
                    let result = rt.block_on(async {
                        crate::transcribe::transcribe_audio(
                            &ctx.stt.api_url,
                            &ctx.stt.api_model,
                            key,
                            &wav,
                            &ctx.language,
                            &ctx.stt.prompt,
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
        } else if ctx.stt.local_backend == "parakeet" {
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
            match &ctx.resolved_local_model {
                Some(model_path) => {
                    match crate::local_stt::transcribe_local(
                        model_path,
                        &pcm_16k,
                        &ctx.language,
                        &ctx.stt.prompt,
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

    while let Ok(job) = rx.recv() {
        let (mic_slice, system_slice, elapsed_ms) = match job {
            SttJob::Paused { elapsed_ms, dur_ms } => {
                let line = format!(
                    "[{:>6} ms] [paused] (resumed after {} ms)\n",
                    elapsed_ms, dur_ms
                );
                let _ = ctx.transcripts_file.write_all(line.as_bytes());
                let _ = ctx.transcripts_file.flush();
                continue;
            }
            SttJob::Chunk {
                mic,
                system,
                elapsed_ms,
            } => (mic, system, elapsed_ms),
        };

        let mic_text = transcribe(mic_slice, ctx.device_sample_rate);
        let system_text = if ctx.mix_active {
            transcribe(system_slice, ctx.system_sample_rate)
        } else {
            String::new()
        };

        // Append helper: dedup vs the per-speaker accumulator,
        // append the delta to the speaker's accumulator, emit a
        // labeled line into transcripts.txt, AND fire a
        // `meeting_chunk` event so host UIs can refresh their
        // live transcript view without polling the on-disk file.
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
            let count = ctx
                .chunk_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            let line = format!("[{:>6} ms] [{}] {}\n", elapsed_ms, speaker, delta);
            let _ = ctx.transcripts_file.write_all(line.as_bytes());
            let _ = ctx.transcripts_file.flush();

            // Push event to host UIs. Payload mirrors the line
            // we just wrote so consumers can either re-render
            // from transcripts.txt at open + tail this event, or
            // build their own rolling buffer from `line` deltas.
            let payload = serde_json::json!({
                "dir": ctx.dir_str,
                "speaker": speaker,
                "elapsed_ms": elapsed_ms as u64,
                "chunk_count": count,
                "line": line,
            })
            .to_string();
            crate::ffi::emit_event("meeting_chunk", &payload);
        };

        emit(ctx.mic_label, &mic_text, &mut mic_accum);
        if ctx.mix_active {
            emit("system", &system_text, &mut system_accum);
        }

        // Stats: this chunk contributed transcribed words. Mirrors the
        // bookkeeping `dimmy_stop_recording` does for pill dictation so
        // the Settings → Stats card reflects meeting usage too. Words
        // are approximated from the FULL chunk text (not the post-dedup
        // delta) — the overlap dedup removes 1-3 words per chunk-pair,
        // ~1-3% over-count, well inside "approximate counter" budget.
        let chunk_words = (mic_text.split_whitespace().count()
            + system_text.split_whitespace().count())
            as std::os::raw::c_int;
        if chunk_words > 0 {
            let _ = crate::ffi::dimmy_update_stats(chunk_words, 0.0);
        }
    }

    let mut fallback = String::new();
    if !mic_accum.trim().is_empty() {
        fallback.push_str(&format!(
            "[{}] {}
",
            ctx.mic_label, mic_accum
        ));
    }
    if !system_accum.trim().is_empty() {
        fallback.push_str(&format!(
            "[system] {}
",
            system_accum
        ));
    }
    fallback
}

/// Capture worker: drains the audio buffers to disk and hands windows to
/// the transcription thread.
///
/// # The one rule
///
/// **Nothing may stand between captured audio and the disk.** Transcription,
/// host callbacks, model loads and network calls all live on the STT thread
/// (`stt_thread_loop`); this loop only reads the shared buffer, writes the
/// three track sinks, and moves its cursor. If the transcriber wedges, falls
/// behind, or dies, the recording keeps landing on disk at full fidelity.
///
/// Before 2026-09-03 the two were the same thread. A real 34-minute meeting
/// produced an 11-minute file: the loop stalled for 22 minutes after a chunk
/// (the stall was NOT inside whisper — no `whisper_full` was logged in that
/// window — which is precisely why the fix cannot be "make STT faster"), the
/// audio piled up in RAM, stop timed out at its 120 s join, and the buffer
/// was gone by the time the loop came back. 23 minutes of a real
/// conversation, unrecoverable.
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
    // Trim silence out of each chunk only on the local route. Cloud STT runs
    // its own VAD server-side and our pipeline provably degrades quiet audio
    // for it (AUDIO-004), so the decision is delegated to the same pure
    // function the batch dictation path uses.
    let vad_trim = matches!(
        crate::preprocess::preprocess_route(stt.preprocessing_enabled, &stt.mode),
        crate::preprocess::PreprocessRoute::Full
    );
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

    // `samples_written` and `last_processed` are INDICES INTO THE BUFFER,
    // not totals: `drain_consumed_samples` removes audio that is already on
    // disk and shifts both cursors down by the same amount. Before that
    // (until 2026-09-04) the buffer only ever grew, holding every sample of
    // the meeting in RAM even though it was safely written — 0.366 MiB/s,
    // measured 459 MiB over 22 minutes, 2.6 GiB over two hours.
    let mut samples_written: usize = 0;
    let mut last_processed: usize = 0;
    // Monotonic count of everything written since the meeting began. The
    // capture-integrity ratio at stop needs a TOTAL, which the rebased
    // cursor above can no longer provide.
    let mut total_written: usize = 0;
    let mut last_fsync = Instant::now();
    // Per-speaker accumulators. dedup_last_3_words is stateful per
    // speaker so they get independent contexts. The merged ordered
    // transcript lives in transcripts.txt (one labeled line per chunk
    // emitted in time order).
    let started = Instant::now();
    // Speaker labels routed by AudioSource. Mic-only and Mix put the
    // user-mic stream as "mic"; System-only puts the loopback as
    // "system" since there's no mic in that mode.
    let mic_label = match source {
        crate::audio::AudioSource::System => "system",
        _ => "mic",
    };

    let transcripts_path = dir.join("transcripts.txt");
    let transcripts_file = match OpenOptions::new()
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

    // Transcription runs on its own thread from here on. The handoff is a
    // bounded queue of owned windows; this thread keeps every cursor and
    // never waits on the other side. See `worker_loop`'s doc comment for
    // why that separation is not negotiable.
    let chunk_count_shared = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let (stt_tx, stt_rx) = std::sync::mpsc::sync_channel::<SttJob>(STT_QUEUE_DEPTH);
    let stt_ctx = SttThreadCtx {
        stt: stt.clone(),
        resolved_local_model,
        cloud_rt,
        language: language.clone(),
        vad_trim,
        mix_active,
        mic_label,
        device_sample_rate,
        system_sample_rate,
        dir_str: dir.to_string_lossy().to_string(),
        transcripts_file,
        chunk_count: Arc::clone(&chunk_count_shared),
    };
    let stt_handle = thread::Builder::new()
        .name("dimmy-meeting-stt".into())
        .spawn(move || stt_thread_loop(stt_rx, stt_ctx))
        .ok();
    if stt_handle.is_none() {
        // Could not spawn: record anyway. A meeting with audio and no
        // transcript is recoverable; the reverse is not.
        crate::log("[Meeting] could not spawn the transcription thread — recording audio only");
    }
    let mut stt_dropped_chunks: u32 = 0;
    let mut stt_disconnected_logged = false;

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
    // Sum of paused-window durations, so the stop-time capture-ratio
    // guard compares audio-on-disk against REAL ACTIVE recording time
    // (elapsed minus pauses) instead of raw wall-clock.
    let mut total_paused_ms: u128 = 0;

    // Periodic diagnostic tick. Every 5 s log the state machine's view
    // of the worker:
    //   - elapsed (worker wall-clock)
    //   - primary_len / secondary_len (raw buffer sizes the worker sees)
    //   - clock_decided / clock_on_secondary (mic-presence latch)
    //   - samples_written / last_processed (how far the WAV writer and
    //     STT chunker have advanced)
    // Crucial when the user reports "meeting recorded nothing": a
    // primary_len stuck at 0 means the cpal mic callback never fired
    // (permission / device / sample-format) or the AEC worker never
    // drained a mic frame; a secondary_len stuck at 0 means the macOS
    // tap / SCKit fallback never pushed (audio-capture grant missing).
    let mut diag_last_log = Instant::now();

    loop {
        let cancelled = cancel.load(Ordering::SeqCst);
        thread::sleep(POLL_INTERVAL);

        // Enforce the secondary-tracks-primary invariant. Zero-pads
        // the loopback buffer up to the mic buffer's length, so
        // every WAV cursor we maintain sees a time-coherent pair.
        align_secondary(&audio_buffer, &audio_buffer_secondary);

        // Periodic 5 s diagnostic. Cheap (two locks, one log line), and
        // worth its weight when the user files "meeting recorded nothing"
        // — primary_len stuck at 0 finger-prints a dead mic stream;
        // secondary_len stuck at 0 finger-prints a denied tap; both
        // growing but samples_written stuck finger-prints a pause-state
        // bug.
        if diag_last_log.elapsed() >= Duration::from_secs(5) {
            let p_len = audio_buffer.lock().map(|b| b.len()).unwrap_or(0);
            let s_len = audio_buffer_secondary.lock().map(|b| b.len()).unwrap_or(0);
            let elapsed = started.elapsed().as_secs_f64();
            crate::log(&format!(
                "[Meeting/diag] elapsed={:.1}s primary_len={} secondary_len={} clock_decided={} clock_on_secondary={} total_written={} write_cursor={} last_processed={} chunk_count={} paused={} mix_active={}",
                elapsed,
                p_len,
                s_len,
                clock_decided,
                clock_on_secondary,
                // Monotonic total: "stuck" here means the writer stalled.
                total_written,
                // Index into the DRAINED buffer. `primary_len - write_cursor`
                // is how far the writer trails the capture, which is the
                // number THE AUDIO RULE is about — it was invisible for one
                // build after the drain landed, because this line reported
                // only the total.
                samples_written,
                last_processed,
                chunk_count_shared.load(std::sync::atomic::Ordering::Relaxed),
                paused.load(Ordering::SeqCst),
                mix_active,
            ));
            diag_last_log = Instant::now();
        }

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
            total_paused_ms += dur_ms;
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
            // Note the gap in the transcripts file so the recap LLM sees
            // the timeline jump. Queued rather than written here so it
            // keeps its position relative to the surrounding chunks — and
            // so this thread never touches the transcripts file.
            let _ = stt_tx.try_send(SttJob::Paused {
                elapsed_ms: started.elapsed().as_millis(),
                dur_ms,
            });
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
            total_written += buf_len_now - samples_written;
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

        // Hand a window to the transcription thread if enough new audio
        // has accumulated. EXTRACTION ONLY: the slices are copied out
        // from under the buffer lock and given away. This loop never runs
        // a model, never calls the host, and never blocks on either.
        let want_end = last_processed + chunk_samples;
        if buf_len_now >= want_end || (cancelled && buf_len_now > last_processed) {
            let start = last_processed.saturating_sub(overlap_samples);
            let end = if cancelled {
                // Final window. Normally this is one chunk's worth or less,
                // but if the transcriber fell far behind, `buf_len_now` can
                // be minutes away — one giant slice would allocate hundreds
                // of MB and hand whisper an input it cannot use anyway. Cap
                // it; the audio is on disk regardless and "Regenerate
                // transcript" can redo the lot from the file.
                let capped = (last_processed + chunk_samples * 4).min(buf_len_now);
                if capped < buf_len_now {
                    crate::log(&format!(
                        "[Meeting] final window capped: {} of {} samples transcribed at stop \
                         (transcriber was behind) — audio is complete on disk, \
                         use Regenerate transcript",
                        capped - last_processed,
                        buf_len_now - last_processed
                    ));
                }
                capped
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

            // try_send, never send: a full queue means the machine cannot
            // transcribe as fast as it records, and the correct answer is
            // to drop TRANSCRIPT and keep recording. Blocking here would
            // reintroduce exactly the coupling this split removes.
            let job = SttJob::Chunk {
                mic: mic_chunk,
                system: system_chunk,
                elapsed_ms: started.elapsed().as_millis(),
            };
            match stt_tx.try_send(job) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    stt_dropped_chunks += 1;
                    if stt_dropped_chunks == 1 || stt_dropped_chunks.is_multiple_of(10) {
                        crate::log(&format!(
                            "[Meeting] transcription is behind — dropped {} window(s) so far; \
                             audio recording is UNAFFECTED \
                             (regenerate the transcript afterwards)",
                            stt_dropped_chunks
                        ));
                    }
                    // Tell the user ONCE, while it is still happening and
                    // they can act on it. Finding out at stop time is too
                    // late to change engine for this meeting.
                    if stt_dropped_chunks == 1 {
                        let engine: &'static str = if stt.mode == "cloud" {
                            "cloud"
                        } else if stt.local_backend == "parakeet" {
                            "parakeet"
                        } else {
                            "whisper"
                        };
                        let secs = started.elapsed().as_secs_f64();
                        crate::telemetry::track(
                            crate::telemetry::Event::MeetingTranscriptionBehind {
                                engine,
                                elapsed_bucket: bucket_elapsed_secs(secs),
                            },
                        );
                        crate::ffi::emit_event(
                            "meeting_transcription_behind",
                            &serde_json::json!({
                                "engine": engine,
                                "elapsed_secs": secs as u64,
                            })
                            .to_string(),
                        );
                    }
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    // Transcriber died. Keep recording; say so once.
                    if !stt_disconnected_logged {
                        stt_disconnected_logged = true;
                        crate::log(
                            "[Meeting] transcription thread is gone — continuing to record audio",
                        );
                    }
                }
            }

            // Time captured for this window counts regardless of whether the
            // transcriber took it: the audio IS recorded. Deliberately the
            // same `end - start` span the single-threaded version used
            // (overlap included), so the Settings "Time saved" figure does
            // not shift. Words are added separately by the transcription
            // thread — the only side that knows them — and the two calls sum
            // to exactly what the single call did.
            let chunk_secs = if device_sample_rate > 0 {
                (end - start) as f64 / device_sample_rate as f64
            } else {
                0.0
            };
            if chunk_secs > 0.0 {
                let _ = crate::ffi::dimmy_update_stats(0, chunk_secs);
            }

            last_processed = end;
        }

        // Reclaim what is finished with. The audio is already in the Ogg
        // files; keeping it in RAM as well cost 0.366 MiB/s for the whole
        // meeting — 459 MiB at 22 minutes, 2.6 GiB over two hours, measured
        // 2026-09-04. Both buffers are drained by the SAME amount so the
        // mic/system alignment `align_secondary` maintains is preserved, and
        // both cursors shift down with them.
        //
        // Skipped while paused: the pause/resume edge re-derives both
        // cursors from the live buffer length, and moving the floor out from
        // under it would make the resume skip the wrong window.
        if !is_paused_now {
            let drop_n = drainable_samples(
                samples_written,
                last_processed,
                overlap_samples,
                DRAIN_THRESHOLD_SAMPLES,
            );
            if drop_n > 0 {
                let mut dropped = 0usize;
                if let Ok(mut b) = audio_buffer.lock() {
                    let n = drop_n.min(b.len());
                    b.drain(..n);
                    dropped = n;
                }
                if dropped > 0 {
                    if mix_active {
                        if let Ok(mut s) = audio_buffer_secondary.lock() {
                            let n = dropped.min(s.len());
                            s.drain(..n);
                        }
                    }
                    samples_written -= dropped;
                    last_processed -= dropped;
                }
            }
        }

        if cancelled {
            break;
        }
    }

    // ORDER MATTERS. Close the audio FIRST, then wait on the transcriber.
    //
    // The Ogg trailer / WAV header rewrite is what makes the recording a
    // valid, seekable file. Doing it before the join means a wedged
    // transcriber can cost the tail of the transcript and nothing else —
    // the audio is already complete and playable on disk by the time we
    // wait for anything.
    //
    // Collect the FIRST failure into MeetingResult.error — a finalize
    // error (disk-full mid-header-rewrite) leaves the audio file
    // incomplete on disk and the user must hear about it at stop time.
    let mut finalize_error: Option<String> = None;
    if let Err(e) = writer.finalize() {
        finalize_error.get_or_insert(format!("audio track incomplete: {e}"));
    }
    if let Err(e) = writer_mic.finalize() {
        finalize_error.get_or_insert(format!("mic track incomplete: {e}"));
    }
    if let Some(w) = writer_system {
        if let Err(e) = w.finalize() {
            finalize_error.get_or_insert(format!("system track incomplete: {e}"));
        }
    }

    // Audio is safe. NOW wait for the transcriber to drain what is queued.
    //
    // Dropping the sender ends its `recv()` once the queue empties. The
    // wait is bounded twice over: the queue holds at most STT_QUEUE_DEPTH
    // windows, and the join gives up after 90 s. A transcriber wedged
    // inside a model call is abandoned (it holds no lock the audio path
    // needs, and the process is about to be idle) — we keep the chunk
    // count it reached and move on.
    drop(stt_tx);
    // Say what the wait IS. Stopping a meeting whose transcriber is behind
    // leaves the user on "Wrapping up..." for up to 90 s with nothing
    // happening and no way to tell working from wedged — measured at 90 s
    // exactly on 2026-09-04, with 42 windows outstanding. The recap has not
    // even been dispatched yet at this point, so there is no stream to show
    // and only the core knows why.
    //
    // Emitted HERE and not from the capture loop: the audio sinks are
    // already finalized above, so a host callback that blocks can no longer
    // cost a single sample.
    if stt_handle.is_some() {
        crate::ffi::emit_event(
            "meeting_finishing_transcription",
            &serde_json::json!({ "dropped_windows": stt_dropped_chunks }).to_string(),
        );
    }
    let stt_fallback = match stt_handle {
        Some(h) => match join_bounded(h, Duration::from_secs(90)) {
            BoundedJoin::Done(s) => s,
            BoundedJoin::Panicked => {
                crate::log("[Meeting] transcription thread panicked — audio is unaffected");
                String::new()
            }
            BoundedJoin::TimedOut => {
                crate::log(
                    "[Meeting] transcription thread still busy at stop — abandoning it; \
                     the recording and everything transcribed so far are already on disk",
                );
                String::new()
            }
        },
        None => String::new(),
    };
    if stt_dropped_chunks > 0 {
        crate::log(&format!(
            "[Meeting] {} window(s) never reached the transcriber (machine slower than realtime); \
             audio.ogg is complete — Regenerate transcript re-runs the whole recording",
            stt_dropped_chunks
        ));
    }
    let chunk_count = chunk_count_shared.load(std::sync::atomic::Ordering::Relaxed);

    let duration_secs = started.elapsed().as_secs_f64();
    // Capture-integrity guard — the meeting sibling of the dictation
    // capture-ratio at StopRec (ffi.rs). Compares audio actually on disk
    // (`samples_written` at the canonical rate) against the REAL ACTIVE
    // recording time (elapsed minus paused windows). A healthy meeting is
    // ~1.0; a low ratio means capture ran at the wrong rate and audio.wav
    // is time-distorted — the "voce accelerata 3×" class (a BT headset
    // flipping A2DP↔HFP mid-meeting → ratio ~0.33). The rate-based rebuild
    // in audio.rs should keep this at ge_95; a low bucket in the field is
    // the alarm that a gap slipped through. WARN-not-assert: rate drift is
    // device-dependent, not a logic bug, and crashing at stop would lose
    // the whole recording. Gated on >5 s active so startup jitter over a
    // short meeting can't mis-bucket as unhealthy.
    {
        let active_secs = (duration_secs - total_paused_ms as f64 / 1000.0).max(0.0);
        let captured_secs = total_written as f64 / device_sample_rate.max(1) as f64;
        if active_secs > 5.0 {
            let ratio = captured_secs / active_secs;
            if ratio.is_finite() {
                if ratio < 0.85 {
                    crate::log(&format!(
                        "[Meeting] WARN capture ratio {:.0}% — {:.1}s audio of {:.1}s active recording; \
                         playback may be time-distorted (input rate drift?)",
                        ratio * 100.0,
                        captured_secs,
                        active_secs
                    ));
                }
                crate::telemetry::track(crate::telemetry::Event::MeetingCaptureRatio {
                    ratio_bucket: crate::telemetry::sanitize::bucket_capture_ratio(ratio),
                });
            }
        }
    }
    finalize_meeting_meta(&dir, &id, duration_secs, chunk_count);

    // Build the final transcript: time-ordered labeled stream read
    // back from transcripts.txt (one line per chunk, format
    // `[ts ms] [speaker] text`). The `[ts ms]` prefix is preserved
    // so the LLM recap can use timestamps for diarization context.
    // Falls back to a per-speaker concat if the file read fails.
    let merged_transcript = std::fs::read_to_string(&transcripts_path)
        .ok()
        .filter(|s| !s.trim().is_empty())
        // Fallback: the transcriber's own accumulators, returned when it
        // joined cleanly. Empty when it was abandoned — the audio file is
        // complete either way.
        .unwrap_or(stt_fallback);

    MeetingResult {
        id,
        dir,
        transcript: merged_transcript,
        duration_secs,
        chunk_count,
        error: finalize_error,
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
    model: Option<&str>,
) -> Result<(), String> {
    if !recap_md.trim().is_empty() {
        let marked = mark_ai_generated(recap_md, model);
        std::fs::write(meeting_dir.join("recap.md"), &marked)
            .map_err(|e| format!("write recap.md: {}", e))?;
        // Title sync: best-effort, never fail the save. Parse the ORIGINAL:
        // the marker is placed so it cannot disturb this, but not depending
        // on that is free.
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

/// Machine-readable marker stamped on every recap Dimmy writes.
///
/// EU AI Act art. 50(2) requires the provider of a system that generates
/// synthetic text to mark its outputs "in a machine-readable format and
/// detectable as artificially generated". A recap is genuinely new text —
/// a summary with decisions and action items — so it is in scope.
///
/// Dictation, filler removal and translation are NOT: 50(2) exempts a system
/// that "performs an assistive function for standard editing or does not
/// substantially alter the input data or its semantics". Stamping every
/// dictated sentence would also be actively hostile, since that text goes
/// straight into whatever app the user is typing in.
///
/// Not a lawyer; the classification of the recap is the point to put in front
/// of one if certainty is needed.
pub const AI_GENERATED_TAG: &str = "<!-- dimmy-ai-generated: true; by: Dimmy -->";

/// Prepare a recap to leave Dimmy for somewhere a human will read it and
/// Dimmy's HTML comments will not survive (Notion today).
///
/// Two jobs. It strips Dimmy's internal `<!-- dimmy-* -->` tags, which would
/// otherwise either vanish silently or land as literal text in the page. And
/// it prepends the VISIBLE notice, because that is the surface where a recap
/// is most likely to be shared onward: the machine-readable marker alone would
/// be lost in the conversion, taking the art. 50 marking with it.
///
/// Pure, so it is testable and so the next integration that needs it does not
/// grow its own copy.
pub fn recap_for_sharing(recap_md: &str, lang: &str) -> String {
    let body: String = recap_md
        .lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with("<!--") && t.contains("dimmy-") && t.ends_with("-->"))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let title = ai_notice_text("title", lang).unwrap_or_default();
    let hint = ai_notice_text("hint", lang).unwrap_or_default();
    let notice = format!("> **{}** {}", title.trim(), hint.trim());

    let trimmed = body.trim_start_matches('\n');
    if trimmed.trim().is_empty() {
        return notice;
    }
    format!("{notice}\n\n{trimmed}")
}

/// Localized notice shown ABOVE a recap in the UI.
///
/// Art. 50(2) is satisfied by `AI_GENERATED_TAG`, which is machine-readable
/// but invisible. Art. 50(5) additionally wants information that is "clear and
/// distinguishable, at the latest at the time of the first interaction" — that
/// one only counts if a human SEES it, so it is a separate, visible string.
///
/// Lives in the core for the same reason `consent::ui_text` does: hardcoding it
/// host-side once already left half a legal notice in English while the rest
/// was localized. `kind` is `"title"` or `"hint"`; unknown kinds return `None`
/// so the FFI can reject them. ASCII-apostrophe style matches `consent`.
pub fn ai_notice_text(kind: &str, lang: &str) -> Option<String> {
    let l = crate::consent::norm_lang(lang);
    let s = match kind {
        "title" => match l {
            "it" => "Riassunto generato con AI",
            "es" => "Resumen generado con IA",
            "fr" => "Resume genere par IA",
            "de" => "Mit KI erstellte Zusammenfassung",
            "pt" => "Resumo gerado por IA",
            _ => "AI-generated summary",
        },
        "hint" => match l {
            "it" => "Rileggilo prima di condividerlo.",
            "es" => "Revisalo antes de compartirlo.",
            "fr" => "Relisez-le avant de le partager.",
            "de" => "Vor dem Teilen bitte pruefen.",
            "pt" => "Revise antes de compartilhar.",
            _ => "Review it before sharing.",
        },
        _ => return None,
    };
    Some(s.to_string())
}

/// Build the machine-readable marker, optionally naming the model.
///
/// The model id is filtered to `[A-Za-z0-9 / . - _ : +]` — an id carrying a
/// `>` or a `--` would terminate or corrupt the HTML comment and could push
/// arbitrary text into the recap body. Real ids
/// (`accounts/fireworks/models/kimi-k3`, `claude-opus-5`) pass untouched; a
/// value that filters down to nothing falls back to the plain marker rather
/// than emitting an empty `model:` field.
fn build_ai_tag(model: Option<&str>) -> String {
    let cleaned = model
        // Hosts pass `recap_model_override` straight through, which carries
        // the picker's `cloud:` / `local:` prefix. Strip it here rather than
        // in each host so Windows and macOS cannot drift on what the marker
        // says (and so `local:gemma-3.gguf` reads as the model, not a mode).
        .map(|m| {
            m.strip_prefix("cloud:")
                .or_else(|| m.strip_prefix("local:"))
                .unwrap_or(m)
        })
        .map(|m| {
            m.chars()
                .filter(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | '+')
                })
                .collect::<String>()
        })
        .filter(|m| !m.is_empty() && !m.contains("--"));
    match cleaned {
        Some(m) => format!("<!-- dimmy-ai-generated: true; by: Dimmy; model: {m} -->"),
        None => AI_GENERATED_TAG.to_string(),
    }
}

/// Stamp `AI_GENERATED_TAG` on a recap, idempotently.
///
/// Placement follows the convention already in the file: a leading `# Title`
/// stays the first non-empty line and the tag goes on line 2, exactly like
/// `<!-- dimmy-type: KEY -->`. That is load-bearing — `parse_recap_title`
/// aborts unless the first non-empty line is an H1, so anything placed above
/// it (YAML front-matter included) silently breaks title sync. Measured on the
/// real corpus: 30 of 54 recaps carry an H1 and would have lost their title.
///
/// Idempotent because regenerating a recap re-saves it, and stacking markers
/// would be both ugly and wrong.
///
/// `model` names the model that wrote the recap, for provenance — with 40+
/// selectable models "generated by AI" no longer says much. It is deliberately
/// confined to this machine-readable marker: the user-visible art. 50 notice is
/// a localized disclosure, and an id like
/// `accounts/fireworks/models/kimi-k3` would clutter it without adding
/// anything the regulation asks for. Pass `None` when the writer is genuinely
/// unknown — a wrong attribution is worse than an absent one.
pub fn mark_ai_generated(recap_md: &str, model: Option<&str>) -> String {
    if recap_md.contains("dimmy-ai-generated:") {
        return recap_md.to_string();
    }
    if recap_md.trim().is_empty() {
        return recap_md.to_string();
    }

    // Insert after a leading H1 when there is one, else at the very top.
    // Anything else (the LLM often opens with `## Context`) already returns
    // None from parse_recap_title, so a tag on line 1 costs nothing there.
    //
    // Splice by byte offset instead of rebuilding the document from
    // `lines()`: that iterator strips the `\r` of a CRLF pair, so re-emitting
    // each line with a bare `\n` silently dropped one byte per line. On a
    // CRLF recap that loss outweighs the inserted tag and tripped the
    // no-content-lost postcondition below — taking the process down, since
    // this runs under an `extern "C"` frame that cannot unwind. Splicing
    // keeps every original byte (line endings included) and only ever adds.
    // Burned 2026-08-11: Kimi K3 via Fireworks answers in CRLF, where every
    // previously-tested provider used LF.
    let mut line_start = 0usize;
    let mut first_content: Option<(usize, usize)> = None;
    for line in recap_md.split_inclusive('\n') {
        if !line.trim().is_empty() {
            first_content = Some((line_start, line_start + line.len()));
            break;
        }
        line_start += line.len();
    }
    let Some((start, end)) = first_content else {
        return recap_md.to_string();
    };

    let tag = build_ai_tag(model);
    let mut out = String::with_capacity(recap_md.len() + tag.len() + 2);
    if recap_md[start..end].trim_start().starts_with("# ") {
        // Title line: tag goes on the line after it.
        out.push_str(&recap_md[..end]);
        if !out.ends_with('\n') {
            out.push('\n'); // no trailing newline in the source
        }
        out.push_str(&tag);
        out.push('\n');
        out.push_str(&recap_md[end..]);
    } else {
        // Not a title: tag goes above the first content line.
        out.push_str(&recap_md[..start]);
        out.push_str(&tag);
        out.push('\n');
        out.push_str(&recap_md[start..]);
    }

    // Postconditions: the marker is present exactly once, and no content was
    // lost. The second is the one that matters — this runs on the user's only
    // copy of a meeting summary.
    assert_eq!(
        out.matches("dimmy-ai-generated:").count(),
        1,
        "mark_ai_generated must stamp exactly one marker"
    );
    assert!(
        out.len() >= recap_md.len(),
        "mark_ai_generated must not drop recap content"
    );
    out
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
mod buffer_reclaim {
    //! The capture buffer must not hold audio that is already on disk.
    //!
    //! Measured 2026-09-04 over a real 22-minute meeting: the buffer grew
    //! 0.366 MiB/s from the first sample to the last, reaching 459 MiB, and
    //! nothing ever freed it — 1.3 GiB at an hour, 2.6 GiB at two. Every
    //! byte of it was already written to the Ogg files.
    //!
    //! Reclaiming it means moving a floor that TWO cursors index against,
    //! and the next chunk read starts BEFORE `last_processed` by the
    //! overlap. Drop too much and whisper gets a window that starts
    //! mid-sentence; drop too little and the leak stays. These pin the
    //! boundary from both sides.
    use super::*;

    const OVERLAP: usize = 24_000; // 500 ms at 48 kHz
    const THRESH: usize = DRAIN_THRESHOLD_SAMPLES;

    #[test]
    fn nothing_is_reclaimed_before_the_threshold() {
        // Draining is a memmove under the capture lock. Doing it on every
        // tick would pay that cost constantly to reclaim a few hundred KB.
        assert_eq!(drainable_samples(1_000, 1_000, OVERLAP, THRESH), 0);
        assert_eq!(drainable_samples(THRESH - 1, THRESH - 1, 0, THRESH), 0);
    }

    #[test]
    fn never_reclaims_past_what_the_writer_has_written() {
        // The transcriber can run AHEAD of the writer when a window is
        // extracted before the tick's write completes. Audio that is not yet
        // on disk must never be dropped — that is the whole invariant.
        let written = THRESH;
        let processed = THRESH * 5;
        assert_eq!(
            drainable_samples(written, processed, OVERLAP, THRESH),
            written,
            "the floor must be the WRITER, not the transcriber"
        );
    }

    #[test]
    fn never_reclaims_the_overlap_the_next_window_needs() {
        // The next chunk starts at `last_processed - overlap`. Reclaiming
        // into that range would shift the buffer under a read that has not
        // happened yet, and the chunk would begin mid-word.
        let processed = THRESH * 2;
        let got = drainable_samples(usize::MAX, processed, OVERLAP, THRESH);
        assert_eq!(got, processed - OVERLAP);
        assert!(got < processed, "the overlap must survive the drain");
    }

    #[test]
    fn a_transcriber_that_has_not_started_blocks_reclaim() {
        // Before the first chunk, `last_processed` is 0: nothing is safe to
        // drop however much the writer has written. Underflowing here would
        // wrap to a colossal count and drain the entire buffer.
        assert_eq!(drainable_samples(THRESH * 10, 0, OVERLAP, THRESH), 0);
        assert_eq!(
            drainable_samples(THRESH * 10, OVERLAP / 2, OVERLAP, THRESH),
            0
        );
    }

    #[test]
    fn the_buffer_stays_bounded_across_a_long_meeting() {
        // Simulate two hours at 48 kHz with a transcriber one chunk behind,
        // draining exactly as the worker does. Without reclaim this reaches
        // 345.6 M samples (2.6 GiB per track); the point is that it does not.
        let chunk = 48_000 * 15;
        let mut buffer_len = 0usize;
        let mut written = 0usize;
        let mut processed = 0usize;
        let mut peak = 0usize;

        for tick in 1..=(2 * 60 * 60 * 10) {
            buffer_len += 4_800; // 100 ms of capture per tick
            written = buffer_len;
            if buffer_len >= processed + chunk {
                processed += chunk;
            }
            let drop_n = drainable_samples(written, processed, OVERLAP, DRAIN_THRESHOLD_SAMPLES);
            if drop_n > 0 {
                buffer_len -= drop_n;
                written -= drop_n;
                processed -= drop_n;
            }
            peak = peak.max(buffer_len);
            assert!(
                written <= buffer_len && processed <= buffer_len,
                "cursors escaped the buffer at tick {tick}"
            );
        }

        let peak_mib = peak as f64 * 8.0 / (1024.0 * 1024.0);
        assert!(
            peak_mib < 40.0,
            "buffer peaked at {peak_mib:.0} MiB over two hours — reclaim is not working"
        );
    }

    #[test]
    fn cursors_stay_consistent_when_the_transcriber_falls_far_behind() {
        // The measured case: whisper at 1.8x realtime, windows dropped. The
        // transcriber lags badly, so little can be reclaimed and the buffer
        // grows — that is CORRECT, the audio is still needed. What must not
        // happen is a cursor going negative or past the end.
        let mut buffer_len = 0usize;
        let mut written = 0usize;
        let mut processed = 0usize;

        for _ in 0..(30 * 60 * 10) {
            buffer_len += 4_800;
            written = buffer_len;
            // Transcriber advances at one third of realtime.
            if buffer_len >= processed * 3 + 48_000 * 15 {
                processed += 48_000 * 5;
            }
            let drop_n = drainable_samples(written, processed, OVERLAP, DRAIN_THRESHOLD_SAMPLES);
            assert!(drop_n <= written, "would drop audio that is not on disk");
            assert!(
                drop_n <= processed.saturating_sub(OVERLAP),
                "would drop the next window's overlap"
            );
            if drop_n > 0 {
                buffer_len -= drop_n;
                written -= drop_n;
                processed -= drop_n;
            }
        }
    }
}

#[cfg(test)]
mod audio_never_blocked {
    //! The rule: nothing may stand between captured audio and the disk.
    //!
    //! These drive the real handoff primitive the capture worker uses —
    //! `sync_channel(STT_QUEUE_DEPTH)` + `try_send` — against a consumer
    //! that is slow, wedged, or dead. What is pinned is that the producer
    //! keeps going at full speed in every case. A 34-minute meeting once
    //! produced an 11-minute file because the two shared a thread
    //! (2026-09-02); the split is the fix and this is its proof.
    use super::*;
    use std::sync::mpsc::TrySendError;

    /// The queue must be bounded, or a slow transcriber becomes a memory
    /// leak instead of a dropped window.
    #[test]
    fn the_queue_is_bounded_and_small() {
        assert!(
            STT_QUEUE_DEPTH > 0 && STT_QUEUE_DEPTH <= 8,
            "queue depth {} is outside the range that bounds memory while \
             absorbing a slow patch",
            STT_QUEUE_DEPTH
        );
    }

    #[test]
    fn a_wedged_consumer_never_blocks_the_producer() {
        // Consumer takes one job then wedges forever, exactly like a
        // whisper call that does not return.
        let (tx, rx) = std::sync::mpsc::sync_channel::<u32>(STT_QUEUE_DEPTH);
        let wedged = thread::spawn(move || {
            let _first = rx.recv();
            thread::sleep(Duration::from_secs(30));
        });

        // The producer's whole meeting: 500 windows, no blocking allowed.
        let started = Instant::now();
        let mut sent = 0u32;
        let mut dropped = 0u32;
        for i in 0..500 {
            match tx.try_send(i) {
                Ok(()) => sent += 1,
                Err(TrySendError::Full(_)) => dropped += 1,
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "the producer stalled for {:?} behind a wedged consumer — this is \
             precisely the coupling that lost 23 minutes of a real meeting",
            elapsed
        );
        assert_eq!(sent + dropped, 500, "every window was accounted for");
        assert!(dropped > 0, "a wedged consumer must cause drops, not waits");
        drop(tx);
        let _ = wedged.join();
    }

    #[test]
    fn a_dead_consumer_never_blocks_the_producer() {
        // Transcriber thread panicked or exited: the producer must notice
        // and carry on recording, not die with it.
        let (tx, rx) = std::sync::mpsc::sync_channel::<u32>(STT_QUEUE_DEPTH);
        drop(rx);

        let started = Instant::now();
        let mut disconnected = 0;
        for i in 0..500 {
            if let Err(TrySendError::Disconnected(_)) = tx.try_send(i) {
                disconnected += 1;
            }
        }
        assert_eq!(disconnected, 500, "every send reported the dead consumer");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a dead consumer must not slow the producer down"
        );
    }

    #[test]
    fn a_consumer_that_keeps_up_loses_nothing() {
        // The happy path must be unchanged: when the machine can transcribe
        // as fast as it records, every window is delivered, in order.
        //
        // The consumer ACKS each item and the producer waits for it, so
        // "keeping up" is guaranteed rather than hoped for. The first
        // version slept 1 ms between sends and trusted the scheduler to
        // drain in time; on a loaded machine it did not, and the test failed
        // for a reason that had nothing to do with the code under test
        // (2026-09-04). A test whose pass depends on thread timing tells you
        // about the machine, not the program.
        let (tx, rx) = std::sync::mpsc::sync_channel::<u32>(STT_QUEUE_DEPTH);
        let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
        let consumer = thread::spawn(move || {
            let mut seen = Vec::new();
            while let Ok(v) = rx.recv() {
                seen.push(v);
                let _ = ack_tx.send(());
            }
            seen
        });

        let mut dropped = 0;
        for i in 0..200 {
            if tx.try_send(i).is_err() {
                dropped += 1;
                continue;
            }
            // Block until this item has been consumed: the queue is provably
            // empty again before the next send.
            ack_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("consumer acked");
        }
        drop(tx);
        let seen = consumer.join().expect("consumer joined");

        assert_eq!(dropped, 0, "a consumer that keeps up must lose nothing");
        assert_eq!(seen.len(), 200);
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "windows must arrive in capture order"
        );
    }

    #[test]
    fn dropping_the_sender_ends_the_consumer() {
        // How stop() unblocks the transcriber: drop the sender, the queue
        // drains, `recv()` returns Err, the thread exits on its own. If this
        // ever stops holding, stop() would rely entirely on its join timeout.
        let (tx, rx) = std::sync::mpsc::sync_channel::<u32>(STT_QUEUE_DEPTH);
        let consumer = thread::spawn(move || {
            let mut n = 0;
            while rx.recv().is_ok() {
                n += 1;
            }
            n
        });
        for i in 0..STT_QUEUE_DEPTH as u32 {
            let _ = tx.try_send(i);
        }
        drop(tx);
        match join_bounded(consumer, Duration::from_secs(5)) {
            BoundedJoin::Done(n) => assert_eq!(n, STT_QUEUE_DEPTH),
            other => panic!(
                "consumer did not exit after the sender dropped: {:?}",
                other
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Bounded join: stop() must NEVER hang on a wedged worker ──────
    // Regression guard for the macOS 26 CoreAudio HAL wedge that froze
    // the whole app on meeting stop (Francesco, 2026-07-06). We can't
    // reproduce the OS wedge in a unit test, but we CAN pin the invariant
    // that makes the app robust to it: the join returns promptly even
    // when the joined thread is stuck.

    #[test]
    fn join_bounded_times_out_promptly_on_a_wedged_thread() {
        let h = thread::spawn(|| {
            thread::sleep(Duration::from_secs(30));
            42
        });
        let start = Instant::now();
        let outcome = join_bounded(h, Duration::from_millis(100));
        assert!(matches!(outcome, BoundedJoin::TimedOut));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "join_bounded must return on timeout, not wait out the wedged thread"
        );
    }

    #[test]
    fn join_bounded_returns_the_value_on_clean_exit() {
        let h = thread::spawn(|| 42);
        match join_bounded(h, Duration::from_secs(5)) {
            BoundedJoin::Done(v) => assert_eq!(v, 42),
            other => panic!("expected Done(42), got {other:?}"),
        }
    }

    #[test]
    fn join_bounded_reports_a_panicking_thread() {
        let h = thread::spawn(|| -> i32 { panic!("boom") });
        assert!(matches!(
            join_bounded(h, Duration::from_secs(5)),
            BoundedJoin::Panicked
        ));
    }

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
    fn mix_windows_soft_limits_peaks_without_hard_clip() {
        let mic = [0.5_f32, -0.5, 0.8, -0.8];
        let sys = [0.25_f32, -0.25, 0.8, -0.8];
        let out = mix_windows(&mic, &sys);
        // Below the knee (0.75): summed and passed through unchanged.
        assert!((out[0] - 0.75).abs() < 1e-6, "got {}", out[0]);
        assert!((out[1] + 0.75).abs() < 1e-6, "got {}", out[1]);
        // Above the knee (raw sum 1.6): soft-limited close to but strictly
        // below 1.0, NOT hard-clamped to exactly 1.0 (no clipping clicks).
        assert!(out[2] > 0.95 && out[2] < 1.0, "got {}", out[2]);
        assert!(out[3] < -0.95 && out[3] > -1.0, "got {}", out[3]);
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
        sink.finalize().expect("finalize must succeed on tempdir");

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
        sink.finalize().expect("finalize must succeed on tempdir");

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

    // ── AI Act art. 50(2) marking ────────────────────────────────────

    #[test]
    fn ai_marker_goes_after_a_leading_h1_so_title_sync_survives() {
        // Load-bearing: parse_recap_title aborts unless the FIRST non-empty
        // line is an H1. 30 of the 54 recaps in the real corpus have one.
        let recap = "# Autenticazione tag NFC

## Context

Testo.
";
        let out = mark_ai_generated(recap, None);
        let mut lines = out.lines();
        assert_eq!(lines.next(), Some("# Autenticazione tag NFC"));
        assert_eq!(lines.next(), Some(AI_GENERATED_TAG));
        assert_eq!(
            parse_recap_title(&out).as_deref(),
            Some("Autenticazione tag NFC"),
            "the marker must not cost the meeting its title"
        );
    }

    #[test]
    fn ai_marker_records_the_model_that_wrote_the_recap() {
        let recap = "# Titolo\n\n## Context\n\nTesto.\n";
        let out = mark_ai_generated(recap, Some("accounts/fireworks/models/kimi-k3"));
        assert!(
            out.contains("<!-- dimmy-ai-generated: true; by: Dimmy; model: accounts/fireworks/models/kimi-k3 -->"),
            "model must be recorded in the marker, got: {out}"
        );
        // The tag still sits on line 2 so parse_recap_title keeps working —
        // the longer marker must not disturb title sync.
        assert_eq!(parse_recap_title(&out).as_deref(), Some("Titolo"));
        // And an already-marked recap is left alone, model or not.
        assert_eq!(mark_ai_generated(&out, Some("claude-opus-5")), out);
    }

    #[test]
    fn ai_marker_falls_back_to_the_plain_tag_without_a_model() {
        // Every recap saved before the model parameter existed looks like
        // this, and a caller that does not know the writer must produce it
        // too rather than inventing an attribution.
        let recap = "# Titolo\n\nTesto.\n";
        assert!(mark_ai_generated(recap, None).contains(AI_GENERATED_TAG));
        assert!(mark_ai_generated(recap, Some("")).contains(AI_GENERATED_TAG));
        assert!(mark_ai_generated(recap, Some("<<>>")).contains(AI_GENERATED_TAG));
    }

    #[test]
    fn ai_marker_strips_the_picker_prefix_from_the_model() {
        // Hosts pass recap_model_override verbatim, and the picker encodes
        // the mode into it. The marker must name the model, not the mode.
        let recap = "# Titolo\n\nTesto.\n";
        assert!(mark_ai_generated(recap, Some("cloud:claude-opus-5"))
            .contains("model: claude-opus-5 -->"));
        assert!(
            mark_ai_generated(recap, Some("local:gemma-3-4b-it-q4.gguf"))
                .contains("model: gemma-3-4b-it-q4.gguf -->")
        );
    }

    #[test]
    fn ai_marker_model_cannot_break_out_of_the_html_comment() {
        // A model id carrying `-->` would close the comment early and spill
        // the rest into the rendered recap. Ids are ours today, but this
        // string ends up in a file the user reads and shares.
        let recap = "# Titolo\n\nTesto.\n";
        let out = mark_ai_generated(recap, Some("evil --> <script>alert(1)</script>"));
        assert!(!out.contains("<script>"), "must not emit markup: {out}");
        assert_eq!(
            out.matches("-->").count(),
            1,
            "exactly one comment terminator: {out}"
        );
        assert_eq!(out.matches("<!--").count(), 1, "exactly one comment opener");
    }

    #[test]
    fn ai_marker_preserves_crlf_line_endings() {
        // Regression: the old implementation rebuilt the document from
        // `lines()`, which strips the `\r` of a CRLF pair, losing a byte per
        // line. On a real recap that loss exceeded the inserted tag and blew
        // the no-content-lost postcondition — a process abort, because this
        // runs under a non-unwinding FFI frame. Kimi K3 via Fireworks answers
        // in CRLF; every provider tested before it used LF.
        for recap in [
            "# Titolo\r\n\r\n## Context\r\n\r\nTesto lungo abbastanza.\r\n",
            "## Context\r\n\r\nSenza titolo, tante righe.\r\nAltra riga.\r\n",
            "\r\n\r\n## Dopo righe vuote\r\n\r\nTesto.\r\n",
        ] {
            let out = mark_ai_generated(recap, None);
            assert!(
                out.len() > recap.len(),
                "marking must only ever add bytes, never drop them"
            );
            assert_eq!(
                out.matches(AI_GENERATED_TAG).count(),
                1,
                "exactly one marker"
            );
            // Every original byte survives: strip the inserted tag line back
            // out and the document must be identical to what came in.
            let restored = out.replacen(&format!("{AI_GENERATED_TAG}\n"), "", 1);
            assert_eq!(restored, recap, "CRLF document must round-trip verbatim");
            assert_eq!(
                mark_ai_generated(&out, None),
                out,
                "marking must stay idempotent on CRLF too"
            );
        }
    }

    #[test]
    fn ai_marker_goes_on_top_when_there_is_no_title() {
        // The LLM often opens with "## Context" or "## TL;DR" (24 of 54 real
        // recaps). parse_recap_title already returns None there, so a tag on
        // line 1 costs nothing.
        let recap = "## Context

Testo.
";
        let out = mark_ai_generated(recap, None);
        assert!(out.starts_with(AI_GENERATED_TAG));
        assert!(out.contains("## Context"));
        assert_eq!(parse_recap_title(recap), None);
    }

    #[test]
    fn ai_marker_is_idempotent() {
        // Regenerating a recap re-saves it; markers must not stack.
        let recap = "# Titolo

Testo.
";
        let once = mark_ai_generated(recap, None);
        let twice = mark_ai_generated(&once, None);
        assert_eq!(once, twice);
        assert_eq!(twice.matches("dimmy-ai-generated:").count(), 1);
    }

    #[test]
    fn ai_marker_never_drops_recap_content() {
        // This runs on the user's only copy of a meeting summary.
        for recap in [
            "# T

## Context

A

## Decisions

- uno
- due
",
            "## TL;DR

Riassunto
",
            "nessun heading affatto
",
            "# T
<!-- dimmy-type: technical -->

## Context
",
        ] {
            let out = mark_ai_generated(recap, None);
            for line in recap.lines() {
                assert!(
                    out.contains(line),
                    "line {line:?} lost when marking {recap:?}"
                );
            }
        }
    }

    #[test]
    fn ai_marker_leaves_an_empty_recap_alone() {
        assert_eq!(mark_ai_generated("", None), "");
        assert_eq!(mark_ai_generated("   \n", None), "   \n");
    }

    #[test]
    fn ai_marker_coexists_with_the_type_tag() {
        // Both tags live in the preamble; neither may displace the title.
        let recap = "# Titolo
<!-- dimmy-type: technical -->

## Context
";
        let out = mark_ai_generated(recap, None);
        assert_eq!(parse_recap_title(&out).as_deref(), Some("Titolo"));
        assert!(out.contains("dimmy-type: technical"));
        assert!(out.contains(AI_GENERATED_TAG));
    }

    #[test]
    fn ai_notice_is_localized_and_rejects_unknown_kinds() {
        // Mirrors consent::ui_text: the core owns the wording so all three
        // hosts render the same notice instead of half of it in English.
        assert_eq!(
            ai_notice_text("title", "en").as_deref(),
            Some("AI-generated summary")
        );
        assert_eq!(
            ai_notice_text("title", "it-IT").as_deref(),
            Some("Riassunto generato con AI"),
            "BCP-47 tags must collapse to the base language"
        );
        assert_eq!(
            ai_notice_text("hint", "it").as_deref(),
            Some("Rileggilo prima di condividerlo.")
        );
        // Unsupported language falls back to English, never to empty.
        assert_eq!(
            ai_notice_text("title", "ja").as_deref(),
            Some("AI-generated summary")
        );
        assert_eq!(ai_notice_text("nonesiste", "en"), None);
    }

    #[test]
    fn ai_notice_copy_follows_the_house_rules() {
        // UI copy in this codebase carries no em-dashes and no tildes: both
        // have broken things before (PowerShell 5.1 among them).
        for kind in ["title", "hint"] {
            for lang in ["en", "it", "es", "fr", "de", "pt"] {
                let s = ai_notice_text(kind, lang).expect("kind/lang must resolve");
                assert!(!s.is_empty(), "{kind}/{lang} must not be empty");
                assert!(
                    !s.contains('—') && !s.contains('~'),
                    "{kind}/{lang} breaks the UI copy rules: {s:?}"
                );
            }
        }
    }

    #[test]
    fn recap_for_sharing_swaps_the_invisible_marker_for_a_visible_one() {
        // Notion's markdown conversion eats HTML comments, so a page sent
        // there would carry NO marking at all. The visible notice replaces it.
        let recap = "# Titolo\n<!-- dimmy-ai-generated: true; by: Dimmy -->\n<!-- dimmy-type: technical -->\n\n## Context\n\nTesto.\n";
        let out = recap_for_sharing(recap, "it");
        assert!(
            !out.contains("dimmy-ai-generated") && !out.contains("dimmy-type"),
            "internal tags must not land in the shared page: {out:?}"
        );
        assert!(out.starts_with("> **Riassunto generato con AI**"));
        assert!(out.contains("Rileggilo prima di condividerlo."));
        // The recap itself survives intact.
        assert!(out.contains("# Titolo"));
        assert!(out.contains("## Context"));
        assert!(out.contains("Testo."));
    }

    #[test]
    fn recap_for_sharing_is_localized_and_never_returns_an_unmarked_body() {
        for lang in ["en", "it", "de", "ja"] {
            let out = recap_for_sharing("## Context\n\nTesto.\n", lang);
            let expected = ai_notice_text("title", lang).unwrap();
            assert!(
                out.contains(&expected),
                "{lang}: notice missing from shared recap"
            );
            assert!(out.contains("Testo."));
        }
        // Degenerate input still comes back marked rather than bare.
        let only_tags = "<!-- dimmy-ai-generated: true; by: Dimmy -->\n";
        let out = recap_for_sharing(only_tags, "en");
        assert!(out.contains("AI-generated summary"));
        assert!(!out.contains("dimmy-ai-generated"));
    }
}
