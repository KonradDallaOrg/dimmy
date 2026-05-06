//! Realtime chunked transcription engine for Parakeet.
//!
//! Spawns a worker thread that, while the audio capture thread is
//! filling the shared PCM buffer, periodically slices off the most
//! recent N seconds (+ overlap with the previous chunk), runs them
//! through `parakeet::transcribe`, dedups the result against the
//! running cumulative text, and emits a callback so the FFI layer
//! can fan it out as an event to the native UI.
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
    pub fn start(
        audio_buffer: Arc<Mutex<Vec<f32>>>,
        device_sample_rate: u32,
        chunk_secs: f32,
        overlap_ms: u32,
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
                    cancel_w,
                    final_w,
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

fn worker_loop(
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    device_sample_rate: u32,
    chunk_secs: f32,
    overlap_ms: u32,
    cancel: Arc<AtomicBool>,
    final_text: Arc<Mutex<String>>,
    on_chunk: Arc<ChunkCallback>,
) {
    let chunk_samples = (chunk_secs * device_sample_rate as f32) as usize;
    let overlap_samples = ((overlap_ms as f32 / 1000.0) * device_sample_rate as f32) as usize;
    assert!(chunk_samples > 0, "chunk_samples must be positive");

    let mut cumulative = String::new();
    let mut last_processed: usize = 0;

    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }

        thread::sleep(POLL_INTERVAL);

        let buf_len = match audio_buffer.lock() {
            Ok(b) => b.len(),
            Err(_) => continue,
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
        let pcm_16k = downsample_if_needed(&snapshot, device_sample_rate);
        let transcribed = match crate::parakeet::transcribe(&pcm_16k) {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("{}", e);
                crate::log(&format!("[chunked] parakeet failed on chunk: {msg}"));
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

    if !trailing.is_empty() {
        let pcm_16k = downsample_if_needed(&trailing, device_sample_rate);
        match crate::parakeet::transcribe(&pcm_16k) {
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
                crate::log(&format!("[chunked] parakeet failed on tail: {e}"));
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

fn downsample_if_needed(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 {
        samples.to_vec()
    } else {
        crate::preprocess::downsample_to_16k(samples, source_rate)
    }
}

/// If the last 3 lower-cased tokens of `prev_cumulative` appear as a
/// 3-token window inside the first 8 tokens of `new_chunk`, return
/// `new_chunk` with everything up to and including that window
/// stripped. Otherwise return `new_chunk` unchanged. Punctuation is
/// stripped on the comparison side only; the returned string preserves
/// the original whitespace + punctuation of the surviving suffix so
/// downstream callers can append it directly to the cumulative.
///
/// Edge cases:
/// - Empty `prev_cumulative` → returns `new_chunk` as-is.
/// - Empty `new_chunk` → returns "".
/// - `prev_cumulative` has fewer than 3 tokens → returns `new_chunk`
///   as-is (no anchor to match against).
/// - `new_chunk` has fewer than 3 tokens → still scans up to its
///   length and matches if the prev tail equals the whole new_chunk.
pub fn dedup_last_3_words(prev_cumulative: &str, new_chunk: &str) -> String {
    if new_chunk.trim().is_empty() {
        return String::new();
    }
    if prev_cumulative.trim().is_empty() {
        return new_chunk.to_string();
    }

    let prev_norm = normalize_tokens(prev_cumulative);
    if prev_norm.len() < 3 {
        return new_chunk.to_string();
    }
    let anchor: &[String] = &prev_norm[prev_norm.len() - 3..];

    // Tokenize new_chunk along with the byte offset where each token
    // ends, so when we find a match we know where in the original
    // string to slice.
    let mut tokens: Vec<(String, usize)> = Vec::new();
    let mut byte_idx = 0usize;
    for raw_tok in new_chunk.split_whitespace() {
        // Locate this token in the original string starting at byte_idx
        // — split_whitespace collapses runs but we need the absolute
        // position to slice later.
        if let Some(rel) = new_chunk[byte_idx..].find(raw_tok) {
            let token_end_in_original = byte_idx + rel + raw_tok.len();
            tokens.push((normalize_one(raw_tok), token_end_in_original));
            byte_idx = token_end_in_original;
        }
        if tokens.len() >= 8 {
            break;
        }
    }

    // Look for `anchor` as a contiguous 3-token window inside the
    // first 8 tokens of new_chunk. Scan windows i, i+1, i+2.
    if tokens.len() >= 3 {
        let scan_upto = tokens.len().saturating_sub(2);
        for i in 0..scan_upto {
            if tokens[i].0 == anchor[0]
                && tokens[i + 1].0 == anchor[1]
                && tokens[i + 2].0 == anchor[2]
            {
                let cut_at = tokens[i + 2].1;
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
    fn dedup_scans_first_8_tokens_only() {
        // anchor at position 6-8 is in window
        let out = dedup_last_3_words(
            "uno due tre",
            "alfa beta gamma delta epsilon uno due tre fine",
        );
        assert_eq!(out, "fine");
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
