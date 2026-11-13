//! Realtime chunked transcription engine (backend-agnostic).
//!
//! Spawns a worker thread that, while the audio capture thread is
//! filling the shared PCM buffer, periodically slices off the most
//! recent N seconds (+ overlap with the previous chunk), runs them
//! through a caller-supplied `TranscribeFn` (Parakeet or whisper), dedups
//! the result against the running cumulative text, and emits a callback so
//! the FFI layer can fan it out as an event to the native UI. The caller
//! decides the backend + whether the host injects each delta at the cursor
//! (typing mode) or only shows it as a live caption.
//!
//! Pattern proven on WSL CPU against 272 min of LibriVox/whisper.cpp
//! audio: 30 s window + 500 ms overlap + last-3-words dedup gave 100 %
//! match on 7 of 9 ground-truth fixtures, 0 OOM, 8.7× realtime. See
//! `docs/dev/stt-benchmark-parakeet-local-2026-05-05.md`. We pick a
//! shorter chunk (5 s) for the realtime path so the user-perceived
//! "text appears" cadence is interactive.
//!
//! The worker is safe to start with whichever sample rate the cpal
//! callback writes (commonly 48 kHz on Windows, 44.1 kHz on macOS) —
//! it downsamples to 16 kHz per chunk before calling the model. No
//! preprocessing (highpass/VAD/AGC) is applied per chunk: those are
//! tuned for end-of-recording silence trim, not for streaming, and
//! Parakeet is robust to mic-level noise on its own. Final batch
//! transcribe (current code path) keeps preprocess for whisper.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Polling cadence of the worker. The worker wakes up every
/// `POLL_INTERVAL` and checks whether enough new audio has accumulated
/// for the next chunk. Picked to balance responsiveness (smaller =
/// chunk fires sooner once the budget is met) against CPU spent on
/// idle wakeups (larger = less work).
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Callback fired once per chunk. Args:
/// - `new_text`: the delta produced by this chunk after dedup.
/// - `cumulative`: the full transcript so far (sum of all dedup'd chunks).
/// - `is_final`: true on the last call, when the worker has drained
///   the trailing tail after `stop()` was requested.
pub type ChunkCallback = dyn Fn(&str, &str, bool) + Send + Sync + 'static;

/// Per-chunk transcription function. Input is 16 kHz mono PCM (the worker
/// downsamples before calling). The caller picks the backend: Parakeet
/// (`parakeet::transcribe`) or whisper (`local_stt::transcribe_local`),
/// so this engine is backend-agnostic — it no longer hard-codes Parakeet.
pub type TranscribeFn =
    dyn Fn(&[f32]) -> Result<String, crate::error::TranscribeError> + Send + Sync + 'static;

pub struct ChunkedTranscriber {
    cancel: Arc<AtomicBool>,
    final_text: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl ChunkedTranscriber {
    /// Spawn the worker. `audio_buffer` is the same shared PCM buffer
    /// the cpal callback writes into. `device_sample_rate` is the
    /// sample rate at which the buffer is being filled (cpal native
    /// rate, NOT 16 kHz).
    /// `vad_trim` runs each window through the RNNoise VAD before the model
    /// sees it (see `preprocess::process_chunk_vad_only`). Caller passes the
    /// batch path's own decision, i.e. `preprocess_route(..) == Full`.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        audio_buffer: Arc<Mutex<Vec<f32>>>,
        device_sample_rate: u32,
        chunk_secs: f32,
        overlap_ms: u32,
        vad_trim: bool,
        transcribe_fn: Arc<TranscribeFn>,
        on_chunk: Arc<ChunkCallback>,
    ) -> Self {
        assert!(chunk_secs > 0.0, "chunk_secs must be positive");
        assert!(chunk_secs <= 60.0, "chunk_secs > 60 is excessive");
        assert!(
            device_sample_rate > 0,
            "device_sample_rate must be positive"
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let final_text = Arc::new(Mutex::new(String::new()));

        let cancel_w = cancel.clone();
        let final_w = final_text.clone();
        let handle = thread::Builder::new()
            .name("chunked-stt".into())
            .spawn(move || {
                worker_loop(
                    audio_buffer,
                    device_sample_rate,
                    chunk_secs,
                    overlap_ms,
                    vad_trim,
                    cancel_w,
                    final_w,
                    transcribe_fn,
                    on_chunk,
                );
            })
            .expect("spawn chunked-stt thread");

        Self {
            cancel,
            final_text,
            handle: Some(handle),
        }
    }

    /// Like `start`, but the audio is cut where the speaker pauses and each
    /// utterance is transcribed whole, as soon as it ends. For a session of
    /// spoken commands: a fixed window (5 s Parakeet, 15 s Whisper) hands
    /// over a command long after it was said, so the user had to stop the
    /// recording after each one (2026-11-10). `on_speech` fires when an
    /// utterance starts.
    pub fn start_utterances(
        audio_buffer: Arc<Mutex<Vec<f32>>>,
        device_sample_rate: u32,
        vad_trim: bool,
        transcribe_fn: Arc<TranscribeFn>,
        on_speech: Arc<dyn Fn() + Send + Sync>,
        on_chunk: Arc<ChunkCallback>,
    ) -> Self {
        assert!(
            device_sample_rate >= 8_000,
            "device_sample_rate too low for 20 ms frames"
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let final_text = Arc::new(Mutex::new(String::new()));
        let cancel_w = cancel.clone();
        let final_w = final_text.clone();
        let handle = thread::Builder::new()
            .name("utterance-stt".into())
            .spawn(move || {
                utterance_loop(
                    audio_buffer,
                    device_sample_rate,
                    vad_trim,
                    cancel_w,
                    final_w,
                    transcribe_fn,
                    on_speech,
                    on_chunk,
                );
            })
            .expect("spawn utterance-stt thread");
        Self {
            cancel,
            final_text,
            handle: Some(handle),
        }
    }

    /// Signal the worker to drain the trailing audio + exit. Joins
    /// the worker thread and returns the final cumulative transcript.
    /// Takes the time of one last `parakeet::transcribe` call on the
    /// residual audio (typically <1 s for a few-second tail), and never
    /// more than [`crate::stream_bounds::LOCAL_STOP_JOIN_TIMEOUT`].
    pub fn stop(mut self) -> String {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            crate::stream_bounds::join_worker(
                h,
                crate::stream_bounds::LOCAL_STOP_JOIN_TIMEOUT,
                "chunked-stt",
            );
        }
        self.final_text
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    device_sample_rate: u32,
    chunk_secs: f32,
    overlap_ms: u32,
    vad_trim: bool,
    cancel: Arc<AtomicBool>,
    final_text: Arc<Mutex<String>>,
    transcribe_fn: Arc<TranscribeFn>,
    on_chunk: Arc<ChunkCallback>,
) {
    let chunk_samples = (chunk_secs * device_sample_rate as f32) as usize;
    let overlap_samples = ((overlap_ms as f32 / 1000.0) * device_sample_rate as f32) as usize;
    assert!(chunk_samples > 0, "chunk_samples must be positive");

    let mut cumulative = String::new();
    let mut last_processed: usize = 0;
    let mut poison_logged = false;

    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }

        thread::sleep(POLL_INTERVAL);

        let buf_len = match audio_buffer.lock() {
            Ok(b) => b.len(),
            Err(_) => {
                // A poisoned buffer lock means a capture-side thread
                // panicked mid-write. The worker can only idle, but that
                // must be VISIBLE (once, not per 100 ms tick) — before
                // this it silently spun while the transcript stalled
                // (audit 2026-07-02).
                if !poison_logged {
                    poison_logged = true;
                    crate::log("[ChunkedSTT] WARN audio_buffer lock poisoned — capture thread panicked? worker idling");
                }
                continue;
            }
        };

        if buf_len < last_processed.saturating_add(chunk_samples) {
            continue;
        }

        // Snapshot the chunk window. The lock is held only for the
        // copy — the cpal callback can resume writing immediately.
        let start = last_processed.saturating_sub(overlap_samples);
        let end = last_processed + chunk_samples;
        let snapshot: Vec<f32> = match audio_buffer.lock() {
            Ok(b) => {
                if end > b.len() {
                    continue;
                }
                b[start..end].to_vec()
            }
            Err(_) => continue,
        };

        let t0 = Instant::now();
        let window = maybe_vad_trim(snapshot, device_sample_rate, vad_trim);
        if window.is_empty() {
            // No speech in this window. Skipping the model call is the whole
            // point: it removes the silence hallucination at the source and
            // saves a full whisper encoder pass.
            crate::log("[chunked] silent window — skipped (VAD)");
            last_processed = end;
            continue;
        }
        let pcm_16k = downsample_if_needed(&window, device_sample_rate);
        let transcribed = match transcribe_fn(&pcm_16k) {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("{}", e);
                crate::log(&format!("[chunked] transcribe failed on chunk: {msg}"));
                last_processed = end;
                continue;
            }
        };
        let elapsed_ms = t0.elapsed().as_millis();

        let delta = dedup_last_3_words(&cumulative, &transcribed);
        if !delta.is_empty() {
            if !cumulative.is_empty() && !cumulative.ends_with(' ') {
                cumulative.push(' ');
            }
            cumulative.push_str(&delta);
        }
        crate::log(&format!(
            "[chunked] +{} chars in {} ms, cumulative {} chars",
            delta.len(),
            elapsed_ms,
            cumulative.len()
        ));
        on_chunk(&delta, &cumulative, false);

        last_processed = end;
    }

    // Drain the trailing audio — anything that arrived after the
    // last full chunk fired but before stop() was requested. Use
    // overlap with the last processed window so a word straddling
    // the boundary still gets caught.
    let trailing: Vec<f32> = match audio_buffer.lock() {
        Ok(b) => {
            if b.len() > last_processed {
                let start = last_processed.saturating_sub(overlap_samples);
                b[start..].to_vec()
            } else {
                Vec::new()
            }
        }
        Err(_) => Vec::new(),
    };

    let trailing = maybe_vad_trim(trailing, device_sample_rate, vad_trim);

    if !trailing.is_empty() {
        let pcm_16k = downsample_if_needed(&trailing, device_sample_rate);
        match transcribe_fn(&pcm_16k) {
            Ok(transcribed) => {
                let delta = dedup_last_3_words(&cumulative, &transcribed);
                if !delta.is_empty() {
                    if !cumulative.is_empty() && !cumulative.ends_with(' ') {
                        cumulative.push(' ');
                    }
                    cumulative.push_str(&delta);
                }
                on_chunk(&delta, &cumulative, true);
            }
            Err(e) => {
                crate::log(&format!("[chunked] transcribe failed on tail: {e}"));
                on_chunk("", &cumulative, true);
            }
        }
    } else {
        on_chunk("", &cumulative, true);
    }

    if let Ok(mut s) = final_text.lock() {
        *s = cumulative;
    }
}

// -- Utterances: cut where the speaker pauses --------------------------------

const FRAME_MS: usize = 20;
/// Silence that ends an utterance. Shorter cuts "apri ... il browser" in
/// two; longer is the wait before every command runs.
const END_SILENCE_FRAMES: usize = 550 / FRAME_MS;
/// Voiced frames in a row that open an utterance.
const START_FRAMES: usize = 3;
/// Fewer voiced frames than this is a click or a cough, not words.
const MIN_VOICED_FRAMES: usize = 8;
const PRE_ROLL_FRAMES: usize = 240 / FRAME_MS;
const POST_ROLL_FRAMES: usize = 160 / FRAME_MS;
const MAX_FRAMES: usize = 15_000 / FRAME_MS;
const FLOOR_MIN: f32 = 0.0002;
const FLOOR_MAX: f32 = 0.02;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    /// An utterance opened.
    Start,
    /// An utterance ended: frames `first..end`.
    End(usize, usize),
}

/// Finds where an utterance starts and ends from the loudness of 20 ms
/// frames, against a noise floor that follows the room: it drops at once
/// and climbs slowly, so a quiet moment sets it and speech barely moves it.
/// A wrong start costs little: the window still goes through the VAD gate
/// before the model sees it.
pub(crate) struct Endpointer {
    floor: f32,
    index: usize,
    run: usize,
    start: Option<usize>,
    voiced: usize,
    quiet: usize,
}

impl Endpointer {
    pub(crate) fn new() -> Self {
        Self {
            floor: 0.001,
            index: 0,
            run: 0,
            start: None,
            voiced: 0,
            quiet: 0,
        }
    }

    /// One frame's RMS level, in sample units.
    pub(crate) fn push(&mut self, level: f32) -> Option<Edge> {
        let level = if level.is_finite() {
            level.max(0.0)
        } else {
            0.0
        };
        self.floor = if level < self.floor {
            level.max(FLOOR_MIN)
        } else {
            (self.floor * 1.003).min(FLOOR_MAX)
        };
        let voiced = level > self.floor * 3.5 && level > 0.0015;
        let i = self.index;
        self.index += 1;
        if voiced {
            self.run += 1;
            self.quiet = 0;
        } else {
            self.run = 0;
            self.quiet += 1;
        }
        let Some(first) = self.start else {
            if self.run >= START_FRAMES {
                self.start = Some((i + 1).saturating_sub(START_FRAMES + PRE_ROLL_FRAMES));
                self.voiced = self.run;
                return Some(Edge::Start);
            }
            return None;
        };
        if voiced {
            self.voiced += 1;
        }
        let long = i + 1 - first >= MAX_FRAMES;
        if self.quiet < END_SILENCE_FRAMES && !long {
            return None;
        }
        let end = if long {
            i + 1
        } else {
            i + 1 - self.quiet + POST_ROLL_FRAMES
        };
        assert!(first < end && end <= i + 1, "an utterance has frames");
        let words = self.voiced >= MIN_VOICED_FRAMES;
        self.start = None;
        self.voiced = 0;
        self.run = 0;
        words.then_some(Edge::End(first, end))
    }
}

/// Transcribe one stretch of audio and add its words to `cumulative`.
/// Returns the words added: none for silence or a failed call.
fn transcribe_into(
    pcm: Vec<f32>,
    rate: u32,
    vad_trim: bool,
    transcribe_fn: &TranscribeFn,
    cumulative: &mut String,
) -> String {
    let secs = pcm.len() as f32 / rate as f32;
    let t0 = Instant::now();
    let window = maybe_vad_trim(pcm, rate, vad_trim);
    if window.is_empty() {
        crate::log(&format!("[utterance] {secs:.1} s without speech, skipped"));
        return String::new();
    }
    let text = match transcribe_fn(&downsample_if_needed(&window, rate)) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            crate::log(&format!("[utterance] transcribe failed: {e}"));
            return String::new();
        }
    };
    crate::log(&format!(
        "[utterance] {secs:.1} s -> {} chars in {} ms",
        text.len(),
        t0.elapsed().as_millis()
    ));
    if !text.is_empty() {
        if !cumulative.is_empty() {
            cumulative.push(' ');
        }
        cumulative.push_str(&text);
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn utterance_loop(
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    vad_trim: bool,
    cancel: Arc<AtomicBool>,
    final_text: Arc<Mutex<String>>,
    transcribe_fn: Arc<TranscribeFn>,
    on_speech: Arc<dyn Fn() + Send + Sync>,
    on_chunk: Arc<ChunkCallback>,
) {
    let frame = rate as usize * FRAME_MS / 1000;
    assert!(frame > 0, "a frame has samples");
    let mut ends = Endpointer::new();
    // Samples the endpointer has seen, and samples already transcribed or
    // dropped: an utterance never reaches back before `done`.
    let (mut scanned, mut done) = (0usize, 0usize);
    let mut cumulative = String::new();
    while !cancel.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(40));
        let fresh: Vec<f32> = match audio_buffer.lock() {
            Ok(b) => {
                let n = b.len().saturating_sub(scanned) / frame * frame;
                b[scanned.min(b.len())..scanned.min(b.len()) + n].to_vec()
            }
            Err(_) => continue,
        };
        for f in fresh.chunks_exact(frame) {
            let rms = (f.iter().map(|x| x * x).sum::<f32>() / frame as f32).sqrt();
            scanned += frame;
            match ends.push(rms) {
                Some(Edge::Start) => on_speech(),
                Some(Edge::End(first, end)) => {
                    let (a, b) = ((first * frame).max(done), (end * frame).min(scanned));
                    done = b;
                    let pcm: Vec<f32> = match audio_buffer.lock() {
                        Ok(buf) if a < b && b <= buf.len() => buf[a..b].to_vec(),
                        _ => continue,
                    };
                    let delta =
                        transcribe_into(pcm, rate, vad_trim, &*transcribe_fn, &mut cumulative);
                    if !delta.is_empty() {
                        on_chunk(&delta, &cumulative, false);
                    }
                }
                None => {}
            }
        }
    }
    // The stop came in the middle of an utterance, or right after one.
    let trailing: Vec<f32> = match audio_buffer.lock() {
        Ok(b) if b.len() > done => b[done..].to_vec(),
        _ => Vec::new(),
    };
    let delta = if trailing.is_empty() {
        String::new()
    } else {
        transcribe_into(trailing, rate, vad_trim, &*transcribe_fn, &mut cumulative)
    };
    on_chunk(&delta, &cumulative, true);
    if let Ok(mut s) = final_text.lock() {
        *s = cumulative;
    }
}

/// Trim silence out of one window before the model sees it, when the caller
/// asked for it. Takes ownership so the pass-through case costs nothing.
fn maybe_vad_trim(samples: Vec<f32>, source_rate: u32, enabled: bool) -> Vec<f32> {
    if !enabled {
        return samples;
    }
    crate::preprocess::process_chunk_vad_only(&samples, source_rate)
}

fn downsample_if_needed(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 {
        samples.to_vec()
    } else {
        crate::preprocess::downsample_to_16k(samples, source_rate)
    }
}

/// Find and remove the boundary-overlap duplication between
/// `prev_cumulative` (the running transcript so far) and `new_chunk`
/// (the latest Parakeet output, which was transcribed from audio that
/// overlaps the previous chunk by `overlap_ms`).
///
/// Algorithm — **longest suffix-prefix match with offset tolerance**:
///
/// 1\. Tokenize both sides (whitespace + punctuation strip + lowercase).
///
/// 2\. For k ∈ \[`MAX_K`..`MIN_K`\], scan whether `prev_norm[-k..]` matches
/// `new_chunk[offset..offset+k]` for some `offset ∈ \[0..MAX_OFFSET\]`.
///
/// 3\. The first (largest k, smallest offset) match wins; trim
/// `new_chunk` up to and including the matched k-th token. Larger k
/// preferred so we don't over-trim a coincidental short match when a
/// longer real overlap is present.
///
/// 4\. If no match found, return `new_chunk` unchanged.
///
/// Why we changed from "exact last-3-words anchor in first 12 tokens":
/// Parakeet hallucinates / drops the partial-word lead token when the
/// chunk's audio starts mid-word (which it does, at every boundary).
/// The strict 3-word anchor failed because the cumulative tail's first
/// word wasn't in the new chunk's start (e.g. cumulative ends with
/// "abbiamo anche mangiato"; new chunk starts with "anche mangiato un
/// sacco" — `abbiamo` is gone, anchor never matches, duplicate ships).
///
/// Tunables (compile-time constants below): `MIN_K=2` keeps over-trim
/// risk low (single-word matches like "il", "the" would trigger too
/// often); `MAX_K=6` covers the worst-case overlap; `MAX_OFFSET=3`
/// tolerates up to 3 garbage tokens at the chunk start.
///
/// **Known false-positive case**: if the user intentionally repeats a
/// 2+ word phrase across the boundary (e.g. "vado al lavoro, vado al
/// lavoro"), this algorithm trims one copy. Trade-off accepted: the
/// boundary-duplicate noise was reported by users as more frequent and
/// more annoying than legitimate repetitions.
/// How much audio to hand the model per chunk during dictation, by local
/// backend. It was a flat 3.0 s for every engine, which is the wrong shape:
/// what a chunk COSTS depends entirely on which model receives it.
///
/// - **whisper** pads ANY input to a full 30 s encoder window, so the cost
///   per call is nearly fixed: measured 3.5 s in = 11 s, 15.4 s in = 12 s,
///   same session. At 3 s chunks a one-minute dictation paid twenty of
///   those instead of four, for identical text. Bigger is strictly
///   cheaper here until 30 s; 15 keeps the captions from lagging half a
///   minute behind the speaker.
/// - **parakeet** costs what the audio costs, so short chunks are nearly
///   free and keep latency low. Nudged 3 -> 5 s only for context: window
///   length is what lets a model get a domain word right, and a 5 s
///   window still leaves under a second of residual work at 6x realtime.
/// - **qwen** is built around 7.5 s per encoder chunk and its context is
///   sized for roughly this; it also dies on a GGML_ASSERT if a chunk
///   overruns `N_BATCH`, so this stays where the model expects it.
///
/// `visible_output` is the second axis, and it is the one that decides
/// whether the chunk is a latency budget at all. With realtime typing, or
/// with live captions on screen, the chunk IS the update interval and the
/// user watches it tick. With "Accelerate transcription" alone, nothing is
/// shown until the end, so the only thing a short chunk buys is a smaller
/// tail at release — and it costs accuracy to buy it.
///
/// Measured on Parakeet, 10 minutes of real meeting, 2026-09-12:
///
/// | window | realtime | words |
/// |--------|----------|-------|
/// | 3 s    | 5.3x     | 785   |
/// | 5 s    | 5.9x     | 866   |
/// | 15 s   | **6.5x** | **882** |
/// | 30 s   | 5.4x     | 822   |
///
/// 3 s was the WORST on both counts: 11% less text than 15 s, and slower
/// too. The old flat 3.0 s was costing accuracy to buy responsiveness the
/// user could not even see when captions were off.
pub fn chunk_secs_for_backend(local_backend: &str, visible_output: bool) -> f32 {
    if !visible_output {
        // Nothing on screen until the end: take the measured optimum.
        return 15.0;
    }
    match local_backend {
        // Short enough to read as it appears; 5 s already recovers most of
        // what 3 s was throwing away (866 words vs 785).
        // Whistle has the same shape of cost: a second of CPU per window.
        "parakeet" | "whistle" => 5.0,
        // Both pay a fixed encoder window per call, so a short chunk buys
        // responsiveness at several times the compute. Neither is a
        // realistic choice for live captions in the first place.
        _ => 15.0,
    }
}

pub fn dedup_last_3_words(prev_cumulative: &str, new_chunk: &str) -> String {
    const MIN_K: usize = 2;
    const MAX_K: usize = 6;
    const MAX_OFFSET: usize = 3;
    const MAX_HEAD_TOKENS: usize = MAX_K + MAX_OFFSET;

    if new_chunk.trim().is_empty() {
        return String::new();
    }
    if prev_cumulative.trim().is_empty() {
        return new_chunk.to_string();
    }

    let prev_norm = normalize_tokens(prev_cumulative);
    if prev_norm.len() < MIN_K {
        return new_chunk.to_string();
    }

    // Tokenize new_chunk along with the byte offset where each token
    // ends, so when we find a match we know where in the original
    // string to slice.
    let mut tokens: Vec<(String, usize)> = Vec::new();
    let mut byte_idx = 0usize;
    for raw_tok in new_chunk.split_whitespace() {
        if let Some(rel) = new_chunk[byte_idx..].find(raw_tok) {
            let token_end_in_original = byte_idx + rel + raw_tok.len();
            tokens.push((normalize_one(raw_tok), token_end_in_original));
            byte_idx = token_end_in_original;
        }
        if tokens.len() >= MAX_HEAD_TOKENS {
            break;
        }
    }
    if tokens.len() < MIN_K {
        return new_chunk.to_string();
    }

    // Try the LARGEST k first — a longer match is more confident and
    // avoids over-trimming when a coincidental short prefix-suffix
    // would also pass.
    let max_k_avail = MAX_K.min(prev_norm.len()).min(tokens.len());
    for k in (MIN_K..=max_k_avail).rev() {
        if k > prev_norm.len() {
            continue;
        }
        let anchor = &prev_norm[prev_norm.len() - k..];
        let max_offset_avail = if tokens.len() >= k {
            (tokens.len() - k).min(MAX_OFFSET)
        } else {
            continue;
        };
        for offset in 0..=max_offset_avail {
            let mut all_match = true;
            for j in 0..k {
                if tokens[offset + j].0 != anchor[j] {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                let cut_at = tokens[offset + k - 1].1;
                return new_chunk[cut_at..].trim_start().to_string();
            }
        }
    }

    new_chunk.to_string()
}

fn normalize_tokens(s: &str) -> Vec<String> {
    s.split_whitespace().map(normalize_one).collect()
}

fn normalize_one(tok: &str) -> String {
    tok.trim_matches(|c: char| matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | '"' | '\''))
        .to_lowercase()
}

#[cfg(test)]
mod chunk_sizing {
    use super::chunk_secs_for_backend;

    /// The assertion in `ChunkedTranscriber::start` kills the process on a
    /// bad value, and this function feeds it. Every arm has to survive it,
    /// including the fallback an unknown backend string takes.
    #[test]
    fn every_backend_is_within_the_start_assertion() {
        for b in [
            "parakeet",
            "qwen",
            "whistle",
            "whisper",
            "",
            "something-new",
        ] {
            for visible in [true, false] {
                let s = chunk_secs_for_backend(b, visible);
                assert!(
                    s > 0.0 && s <= 60.0,
                    "{b}/{visible} -> {s}s trips the assert"
                );
            }
        }
    }

    /// The measured optimum, taken whenever nothing is on screen to wait
    /// for. 3 s produced 11% less text than 15 s on the same audio AND ran
    /// slower, so a short window here would be paying for nothing.
    #[test]
    fn invisible_output_takes_the_measured_optimum() {
        for b in ["parakeet", "whisper", "qwen", "whistle", "anything"] {
            assert_eq!(chunk_secs_for_backend(b, false), 15.0, "{b}");
        }
    }

    /// whisper pays a full 30 s encoder window whatever you give it, so a
    /// small chunk is pure waste. This is the whole reason the value stopped
    /// being one number for everyone.
    #[test]
    fn whisper_gets_a_window_worth_paying_for() {
        assert!(
            chunk_secs_for_backend("whisper", true) >= 10.0,
            "short whisper chunks cost a full 30s window each for nothing"
        );
    }

    /// Parakeet is the engine people run for live captions and realtime
    /// typing, and the chunk IS the update interval. Keep it short.
    /// Only when there is something to be responsive FOR.
    #[test]
    fn parakeet_stays_responsive_while_text_is_on_screen() {
        let s = chunk_secs_for_backend("parakeet", true);
        assert!(s <= 6.0, "{s}s between caption updates is too slow to read");
        assert!(s < chunk_secs_for_backend("parakeet", false));
    }

    /// An unknown backend must not get the shortest window by accident:
    /// the fallback is whisper, and whisper is the one that suffers most.
    #[test]
    fn an_unknown_backend_falls_back_to_the_whisper_value() {
        assert_eq!(
            chunk_secs_for_backend("brand-new-engine", true),
            chunk_secs_for_backend("whisper", true)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_empty_prev_returns_chunk_asis() {
        let out = dedup_last_3_words("", "Ciao come stai");
        assert_eq!(out, "Ciao come stai");
    }

    #[test]
    fn dedup_empty_chunk_returns_empty() {
        let out = dedup_last_3_words("Ciao come stai", "");
        assert_eq!(out, "");
    }

    #[test]
    fn dedup_strips_overlap_at_start() {
        // overlap = "ciao come stai" → first 3 tokens of new_chunk match
        let out = dedup_last_3_words("Ciao come stai", "Ciao come stai bene grazie e tu?");
        assert_eq!(out, "bene grazie e tu?");
    }

    #[test]
    fn dedup_handles_punctuation_difference() {
        // new_chunk has "stai." but prev ends with "stai" — should still match
        let out = dedup_last_3_words("Ciao come stai", "Ciao come stai. Bene grazie.");
        assert_eq!(out, "Bene grazie.");
    }

    #[test]
    fn dedup_case_insensitive() {
        let out = dedup_last_3_words("CIAO come Stai", "ciao COME stai bene");
        assert_eq!(out, "bene");
    }

    #[test]
    fn dedup_anchor_in_offset_window() {
        // Anchor (last 3 of cumulative) at offset 0 → trim works.
        let out = dedup_last_3_words("uno due tre", "uno due tre fine");
        assert_eq!(out, "fine");
    }

    #[test]
    fn dedup_anchor_after_garbage_lead_token() {
        // Parakeet-hallucinated single garbage token at chunk start.
        // Anchor [uno, due, tre] starts at offset 1. MAX_OFFSET=3 so
        // we still find it. Real-world failure mode: chunk starts
        // mid-word and Parakeet emits a phantom syllable as token 0.
        let out = dedup_last_3_words("uno due tre", "garbage uno due tre fine");
        assert_eq!(out, "fine");
    }

    #[test]
    fn dedup_anchor_past_max_offset_falls_through() {
        // Anchor at offset 4 (past MAX_OFFSET=3) → no trim. Ensures
        // the algorithm doesn't scan unboundedly deep — that would
        // open up false-positive trims when a coincidental phrase
        // appears later in genuinely new content.
        let out = dedup_last_3_words(
            "uno due tre",
            "alpha beta gamma delta epsilon uno due tre fine",
        );
        assert_eq!(out, "alpha beta gamma delta epsilon uno due tre fine");
    }

    #[test]
    fn dedup_two_word_match_when_three_word_anchor_first_word_dropped() {
        // The user-reported failure mode (2026-05-10):
        //   cumulative ends "alla festa di compleanno"
        //   chunk starts  "di compleanno ci siamo"   (Parakeet
        //                                             dropped "festa"
        //                                             because chunk
        //                                             audio cut into
        //                                             middle of word)
        // Old algo: anchor [festa, di, compleanno] not found in new
        // chunk → fall through → "compleanno di compleanno" duplicate.
        // New algo: tries k=4 [alla,festa,di,compleanno] (no match),
        // k=3 [festa,di,compleanno] (no match), k=2 [di,compleanno]
        // (MATCH at offset 0) → trim → "ci siamo" appended cleanly.
        let out = dedup_last_3_words("alla festa di compleanno", "di compleanno ci siamo");
        assert_eq!(out, "ci siamo");
    }

    #[test]
    fn dedup_single_word_repeat_at_boundary_falls_through() {
        // MIN_K=2, so a 1-word repeat at boundary is NOT trimmed.
        // Avoids over-trimming common articles (il, la, the, a) that
        // would coincidentally match on every chunk.
        // Cumulative: "tanto" (single trailing word)
        // Chunk:      "tanto e poi"
        // 1-word match would trim "tanto" → "e poi". We don't want
        // that, especially because "tanto" might be a legitimate
        // standalone sentence the user dictated.
        let out = dedup_last_3_words("ci siamo divertiti tanto", "tanto e poi");
        // 2-word anchor is [divertiti, tanto], chunk starts [tanto, e,
        // poi] — `divertiti` not at offset 0..3 → no match → no trim.
        assert_eq!(out, "tanto e poi");
    }

    #[test]
    fn dedup_no_match_returns_chunk_asis() {
        let out = dedup_last_3_words("alpha beta gamma", "delta epsilon zeta");
        assert_eq!(out, "delta epsilon zeta");
    }

    #[test]
    fn dedup_short_prev_returns_chunk_asis() {
        // prev has <3 tokens → no anchor possible
        let out = dedup_last_3_words("Ciao", "Ciao come stai");
        assert_eq!(out, "Ciao come stai");
    }

    #[test]
    fn dedup_three_word_repeat_full_chunk_eaten() {
        let out = dedup_last_3_words("uno due tre", "uno due tre");
        assert_eq!(out, "");
    }

    /// Frames of one level, pushed in order; returns every edge with the
    /// index of the frame that produced it.
    fn feed(e: &mut Endpointer, at: &mut usize, level: f32, frames: usize) -> Vec<(usize, Edge)> {
        let mut out = Vec::new();
        for _ in 0..frames {
            if let Some(edge) = e.push(level) {
                out.push((*at, edge));
            }
            *at += 1;
        }
        out
    }

    // Burned 2026-11-10: with Parakeet a command reached the router only at
    // the stop, so the shortcut had to be pressed after every command.
    #[test]
    fn an_utterance_ends_at_the_pause_after_it() {
        let (mut e, mut at) = (Endpointer::new(), 0);
        assert!(feed(&mut e, &mut at, 0.0005, 50).is_empty(), "room noise");
        let speech = feed(&mut e, &mut at, 0.05, 60);
        assert_eq!(
            speech,
            vec![(52, Edge::Start)],
            "three voiced frames open it"
        );
        let pause = feed(&mut e, &mut at, 0.0005, 40);
        // Opened 15 frames before the third voiced one (pre-roll), closed 8
        // frames after the last voiced one, once 27 silent frames passed.
        assert_eq!(pause, vec![(110 + 26, Edge::End(38, 118))]);
        // A second command needs no key press: it opens and closes again.
        let again = feed(&mut e, &mut at, 0.05, 30);
        assert_eq!(again.len(), 1);
        assert!(matches!(again[0].1, Edge::Start));
        let closed = feed(&mut e, &mut at, 0.0005, 30);
        assert!(matches!(closed.as_slice(), [(_, Edge::End(_, _))]));
    }

    #[test]
    fn a_click_is_not_an_utterance_and_a_monologue_is_cut() {
        let (mut e, mut at) = (Endpointer::new(), 0);
        feed(&mut e, &mut at, 0.0005, 50);
        let click = feed(&mut e, &mut at, 0.2, 4);
        assert_eq!(click.len(), 1, "it opens");
        let after = feed(&mut e, &mut at, 0.0005, 40);
        assert!(
            after.is_empty(),
            "too few voiced frames: dropped, {after:?}"
        );
        // 15 s of speech with no pause is handed over anyway.
        let long = feed(&mut e, &mut at, 0.05, MAX_FRAMES + 20);
        assert!(long.iter().any(|(_, edge)| matches!(edge, Edge::End(_, _))));
        // Bad samples are silence, not a panic.
        for bad in [f32::NAN, f32::INFINITY, -1.0] {
            let _ = e.push(bad);
        }
    }

    #[test]
    fn the_floor_follows_a_noisy_room() {
        let (mut e, mut at) = (Endpointer::new(), 0);
        // A fan at 0.004: above the starting threshold, so it opens once,
        // then the floor reaches it and the noise is silence again.
        let fan = feed(&mut e, &mut at, 0.004, 1500);
        let opened = fan.iter().filter(|(_, x)| *x == Edge::Start).count();
        assert!(opened <= 1, "{opened} utterances out of steady noise");
        let quiet_tail = feed(&mut e, &mut at, 0.004, 200);
        assert!(quiet_tail.is_empty(), "{quiet_tail:?}");
        // Speech over the fan is still found.
        let speech = feed(&mut e, &mut at, 0.06, 40);
        assert!(speech.iter().any(|(_, x)| *x == Edge::Start));
    }

    #[test]
    fn utterances_reach_the_callback_one_by_one_without_a_stop() {
        let rate = 16_000u32;
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let heard: Arc<Mutex<Vec<(String, String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (heard_w, starts_w, n_w) = (heard.clone(), starts.clone(), n.clone());
        let t = ChunkedTranscriber::start_utterances(
            buffer.clone(),
            rate,
            false,
            Arc::new(move |_pcm: &[f32]| {
                let k = n_w.fetch_add(1, Ordering::SeqCst);
                Ok::<String, crate::error::TranscribeError>(
                    ["apri chrome", "apri chrome", "chiudi"][k.min(2)].to_string(),
                )
            }),
            Arc::new(move || {
                starts_w.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(move |delta: &str, cumulative: &str, is_final: bool| {
                heard_w
                    .lock()
                    .unwrap()
                    .push((delta.into(), cumulative.into(), is_final));
            }),
        );
        let tone = |secs: f32, level: f32| {
            (0..(secs * rate as f32) as usize)
                .map(|i| if i % 2 == 0 { level } else { -level })
                .collect::<Vec<f32>>()
        };
        let say = |secs: f32, level: f32| buffer.lock().unwrap().extend(tone(secs, level));
        let wait = |want: usize| {
            for _ in 0..100 {
                if heard.lock().unwrap().len() >= want {
                    return;
                }
                thread::sleep(Duration::from_millis(30));
            }
        };
        say(0.5, 0.0003);
        say(0.8, 0.05);
        say(0.9, 0.0003);
        wait(1);
        // The same words twice are two commands: nothing is deduplicated.
        say(0.8, 0.05);
        say(0.9, 0.0003);
        wait(2);
        say(0.6, 0.05);
        thread::sleep(Duration::from_millis(200));
        let total = t.stop();
        let got = heard.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                ("apri chrome".into(), "apri chrome".into(), false),
                (
                    "apri chrome".into(),
                    "apri chrome apri chrome".into(),
                    false
                ),
                (
                    "chiudi".into(),
                    "apri chrome apri chrome chiudi".into(),
                    true
                ),
            ]
        );
        assert_eq!(total, "apri chrome apri chrome chiudi");
        assert!(starts.load(Ordering::SeqCst) >= 2);
    }
}
