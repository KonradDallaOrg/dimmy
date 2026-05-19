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
        let mix_active = matches!(source, crate::audio::AudioSource::Mix);
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
        let mic_spec = hound::WavSpec {
            channels: 1,
            sample_rate: device_sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let system_spec = hound::WavSpec {
            channels: 1,
            sample_rate: system_sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        crate::log(&format!(
            "[Meeting] WAV rates: mic={} Hz, system={} Hz, mix(audio.wav)={} Hz",
            device_sample_rate, system_sample_rate, device_sample_rate
        ));
        let writer = hound::WavWriter::create(dir.join("audio.wav"), mic_spec)
            .map_err(|e| format!("wav create audio.wav: {}", e))?;
        let writer_mic = hound::WavWriter::create(dir.join("audio_mic.wav"), mic_spec)
            .map_err(|e| format!("wav create audio_mic.wav: {}", e))?;
        let writer_system = if mix_active {
            Some(
                hound::WavWriter::create(dir.join("audio_system.wav"), system_spec)
                    .map_err(|e| format!("wav create audio_system.wav: {}", e))?,
            )
        } else {
            None
        };

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
                    writer,
                    writer_mic,
                    writer_system,
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

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    audio_buffer_secondary: Arc<Mutex<Vec<f32>>>,
    device_sample_rate: u32,
    system_sample_rate: u32,
    source: crate::audio::AudioSource,
    stt: SttSnapshot,
    mut writer: hound::WavWriter<std::io::BufWriter<File>>,
    mut writer_mic: hound::WavWriter<std::io::BufWriter<File>>,
    mut writer_system: Option<hound::WavWriter<std::io::BufWriter<File>>>,
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
    // by the resampler in audio.rs callbacks). `rate_ratio` is
    // therefore always 1.0 — kept for backward compatibility with
    // existing call sites in case a future single-source mode needs
    // a different secondary rate.
    let rate_ratio = system_sample_rate as f64 / device_sample_rate as f64;

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

    let read_synth = |start: usize,
                      end: usize,
                      audio_buffer: &Arc<Mutex<Vec<f32>>>,
                      audio_buffer_secondary: &Arc<Mutex<Vec<f32>>>|
     -> Vec<f32> {
        let primary = match audio_buffer.lock() {
            Ok(b) if end <= b.len() => b[start..end].to_vec(),
            _ => Vec::new(),
        };
        if !mix_active {
            return primary;
        }
        // Both buffers are aligned: secondary[i] = same wall-time
        // instant as primary[i] (zero-padded where loopback was
        // dormant). Read the same window from secondary; if for
        // some reason it's shorter (race with align_secondary that
        // hasn't run yet), missing tail = zeros.
        let secondary = match audio_buffer_secondary.lock() {
            Ok(b) => {
                let take_end = end.min(b.len());
                if start < take_end {
                    b[start..take_end].to_vec()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        let n = primary.len();
        let mut out = Vec::with_capacity(n);
        for (i, &p) in primary.iter().enumerate() {
            let s = secondary.get(i).copied().unwrap_or(0.0);
            out.push((p + s).clamp(-1.0, 1.0));
        }
        out
    };

    // Snapshot the primary buffer length we can safely write up to.
    // No coupling with secondary because the alignment invariant
    // (secondary.len() >= primary.len() after align_secondary) is
    // guaranteed by every worker tick.
    let synth_len = |audio_buffer: &Arc<Mutex<Vec<f32>>>,
                     _audio_buffer_secondary: &Arc<Mutex<Vec<f32>>>|
     -> Option<usize> { audio_buffer.lock().ok().map(|b| b.len()) };

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
            let snap = synth_len(&audio_buffer, &audio_buffer_secondary).unwrap_or_default();
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
        // process this iteration.
        let buf_len_now = match synth_len(&audio_buffer, &audio_buffer_secondary) {
            Some(n) => n,
            None => continue,
        };

        // Stream new samples into the WAV files at NATIVE sample rate
        // (no downsample). Three writers fan out:
        //   audio.wav         = mix (synth = primary + secondary clamped)
        //   audio_mic.wav     = primary buffer (cleaned mic post-AEC)
        //   audio_system.wav  = secondary buffer (raw loopback) — Mix only
        if buf_len_now > samples_written {
            let new_synth = read_synth(
                samples_written,
                buf_len_now,
                &audio_buffer,
                &audio_buffer_secondary,
            );
            // Per-track slices read straight from each buffer (no mixing).
            // mic indices are at primary's rate (= device_sample_rate).
            // system indices are at secondary's rate (= system_sample_rate)
            // and may differ by `rate_ratio` from mic indices when devices
            // run at different native rates.
            let new_mic: Vec<f32> = match audio_buffer.lock() {
                Ok(b) if buf_len_now <= b.len() => b[samples_written..buf_len_now].to_vec(),
                _ => Vec::new(),
            };
            let new_system: Vec<f32> = if mix_active {
                // Both buffers are aligned 1:1. Read the same window
                // from the secondary as we did from the primary.
                match audio_buffer_secondary.lock() {
                    Ok(b) if buf_len_now <= b.len() => b[samples_written..buf_len_now].to_vec(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };

            // Helper: write an f32 buffer to a hound int16 WAV writer.
            let write_buf = |w: &mut hound::WavWriter<std::io::BufWriter<File>>,
                             samples: &[f32]|
             -> Result<(), hound::Error> {
                for s in samples {
                    let clamped = s.clamp(-1.0, 1.0);
                    let i = (clamped * i16::MAX as f32) as i16;
                    w.write_sample(i)?;
                }
                Ok(())
            };

            if let Err(e) = write_buf(&mut writer, &new_synth) {
                crate::log(&format!("[Meeting] audio.wav write failed: {}", e));
            }
            if let Err(e) = write_buf(&mut writer_mic, &new_mic) {
                crate::log(&format!("[Meeting] audio_mic.wav write failed: {}", e));
            }
            if let Some(ref mut w) = writer_system {
                if let Err(e) = write_buf(w, &new_system) {
                    crate::log(&format!("[Meeting] audio_system.wav write failed: {}", e));
                }
            }
            samples_written = buf_len_now;

            if last_fsync.elapsed() >= FSYNC_INTERVAL {
                if let Err(e) = writer.flush() {
                    crate::log(&format!("[Meeting] audio.wav flush: {}", e));
                }
                if let Err(e) = writer_mic.flush() {
                    crate::log(&format!("[Meeting] audio_mic.wav flush: {}", e));
                }
                if let Some(ref mut w) = writer_system {
                    if let Err(e) = w.flush() {
                        crate::log(&format!("[Meeting] audio_system.wav flush: {}", e));
                    }
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

    // Finalize all three WAVs + meta.
    if let Err(e) = writer.finalize() {
        crate::log(&format!("[Meeting] audio.wav finalize: {}", e));
    }
    if let Err(e) = writer_mic.finalize() {
        crate::log(&format!("[Meeting] audio_mic.wav finalize: {}", e));
    }
    if let Some(w) = writer_system {
        if let Err(e) = w.finalize() {
            crate::log(&format!("[Meeting] audio_system.wav finalize: {}", e));
        }
    }
    let duration_secs = started.elapsed().as_secs_f64();
    let meta = serde_json::json!({
        "id": id,
        "duration_secs": duration_secs,
        "chunk_count": chunk_count,
        "ended_at": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
    });
    let _ = std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );

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
pub fn save_post_process(
    meeting_dir: &std::path::Path,
    recap_md: &str,
    actions_json: &str,
    translated: Option<&str>,
) -> Result<(), String> {
    if !recap_md.trim().is_empty() {
        std::fs::write(meeting_dir.join("recap.md"), recap_md)
            .map_err(|e| format!("write recap.md: {}", e))?;
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
}
