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

    /// Signal the worker to drain the trailing audio + exit. Joins
    /// the worker thread and returns the final cumulative transcript.
    /// Bounded by the time of one last `parakeet::transcribe` call
    /// on the residual audio (typically <1 s for a few-second tail).
    pub fn stop(mut self) -> String {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
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
        "parakeet" => 5.0,
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
        for b in ["parakeet", "qwen", "whisper", "", "something-new"] {
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
        for b in ["parakeet", "whisper", "qwen", "anything"] {
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
}
