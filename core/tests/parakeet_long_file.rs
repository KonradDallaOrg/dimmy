//! Diagnostic test that reproduces the dimmy_transcribe_file flow on a
//! long (>10 min) WAV and reports per-chunk statistics. Used to isolate
//! the "Parakeet returns empty after ~5 chunks on a 95-min file" bug
//! reported 2026-05-08.
//!
//! Skips cleanly when the user's local WAV isn't available — set the env
//! var `DIMMY_LONG_WAV` to override the path. By default it looks at
//! `C:\Users\konradd\Downloads\Audio Recording 2026-05-08 at 6.23.36 PM.wav\
//! Audio Recording 2026-05-08 at 6.23.36 PM.wav`.
//!
//! Run with:
//!     cargo test --release --test parakeet_long_file --features local-stt-parakeet -- --nocapture --test-threads=1

#![cfg(feature = "local-stt-parakeet")]

use std::path::PathBuf;
use std::time::Instant;

fn long_wav_path() -> PathBuf {
    std::env::var("DIMMY_LONG_WAV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(
                r"C:\Users\konradd\Downloads\Audio Recording 2026-05-08 at 6.23.36 PM.wav\Audio Recording 2026-05-08 at 6.23.36 PM.wav",
            )
        })
}

fn load_wav_16k_mono(path: &std::path::Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample as i32;
            let scale = (1i64 << (bits - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.expect("read i32") as f32 / scale)
                .collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.expect("read f32")).collect(),
    };
    let mono: Vec<f32> = if spec.channels == 1 {
        raw
    } else {
        let ch = spec.channels as usize;
        raw.chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    if spec.sample_rate == 16_000 {
        mono
    } else {
        dimmy_lib::preprocess::downsample_to_16k(&mono, spec.sample_rate)
    }
}

fn pcm_stats(pcm: &[f32]) -> (f32, f32, usize, usize) {
    let mut peak = 0f32;
    let mut sum_sq = 0f64;
    let mut nans = 0;
    let mut zeros = 0;
    for &s in pcm {
        if !s.is_finite() {
            nans += 1;
        } else {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
            if a < 1e-7 {
                zeros += 1;
            }
            sum_sq += (s as f64) * (s as f64);
        }
    }
    let rms = (sum_sq / pcm.len().max(1) as f64).sqrt() as f32;
    (peak, rms, nans, zeros)
}

#[test]
fn diagnose_long_file_chunks() {
    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!("[skip] parakeet bundle not present");
        return;
    }
    let path = long_wav_path();
    if !path.exists() {
        eprintln!("[skip] long wav not at {:?}", path);
        return;
    }

    eprintln!("Loading WAV at {:?}", path);
    let t0 = Instant::now();
    let pcm = load_wav_16k_mono(&path);
    eprintln!(
        "Loaded {} samples ({:.1}s) in {} ms",
        pcm.len(),
        pcm.len() as f32 / 16_000.0,
        t0.elapsed().as_millis()
    );

    // Same preprocess dimmy_transcribe_file uses post-fix: highpass only,
    // no VAD, no AGC (AGC on a long file produces NaN — see CLAUDE.md
    // AUDIO-001 / known-bugs.md). Pass DIMMY_LONG_WAV_LEGACY_AGC=1 to
    // exercise the broken pre-fix path for regression comparison.
    let use_legacy_agc = std::env::var("DIMMY_LONG_WAV_LEGACY_AGC").is_ok();
    eprintln!(
        "Preprocessing ({})…",
        if use_legacy_agc {
            "LEGACY: highpass + AGC (pre-fix, demonstrates the NaN bug)"
        } else {
            "highpass only, file-load mode"
        }
    );
    let t1 = Instant::now();
    let processed_samples = if use_legacy_agc {
        dimmy_lib::preprocess::process_buffer(&pcm, 16_000)
    } else {
        dimmy_lib::preprocess::process_buffer_for_file_load(&pcm, 16_000)
    };
    eprintln!(
        "Preprocessed {} → {} samples in {} ms",
        pcm.len(),
        processed_samples.len(),
        t1.elapsed().as_millis()
    );
    let (gpeak, grms, gnans, gzeros) = pcm_stats(&processed_samples);
    eprintln!(
        "Global preprocessed stats: peak={:.4} rms={:.5} nans={} near_zero_samples={}",
        gpeak, grms, gnans, gzeros
    );

    // Split into 30 s chunks via the same split_at_silence used by
    // dimmy_transcribe_file so we exercise the exact chunk shapes.
    const CHUNK_SECS: usize = 30;
    let chunk_samples = CHUNK_SECS * 16_000;
    let processed = dimmy_lib::audio::ProcessedAudio {
        samples: processed_samples,
        sample_rate: 16_000,
    };
    let chunks = processed.split_at_silence(chunk_samples);
    eprintln!("Split into {} chunks at silence boundaries", chunks.len());

    let mut total_text_chars = 0usize;
    let mut succeeded = 0;
    let mut empty = 0;
    let mut errored = 0;

    for (idx0, chunk_audio) in chunks.iter().enumerate() {
        let idx = idx0 + 1;
        let start = idx0 * chunk_samples; // approximate (variable len)
        let chunk = chunk_audio.samples.as_slice();

        let (peak, rms, nans, zeros) = pcm_stats(chunk);
        let t = Instant::now();
        let result = dimmy_lib::parakeet::transcribe(chunk);
        let ms = t.elapsed().as_millis();

        match result {
            Ok(text) => {
                if text.trim().is_empty() {
                    empty += 1;
                    eprintln!(
                        "[chunk {:3} @{:5}s len={} peak={:.3} rms={:.5} nan={} zero={}] EMPTY {}ms",
                        idx,
                        start / 16_000,
                        chunk.len(),
                        peak,
                        rms,
                        nans,
                        zeros,
                        ms,
                    );
                } else {
                    succeeded += 1;
                    total_text_chars += text.len();
                    eprintln!(
                        "[chunk {:3} @{:5}s len={} peak={:.3} rms={:.5}] {}ms → {} chars: {:?}",
                        idx,
                        start / 16_000,
                        chunk.len(),
                        peak,
                        rms,
                        ms,
                        text.len(),
                        text.chars().take(80).collect::<String>(),
                    );
                }
            }
            Err(e) => {
                errored += 1;
                eprintln!(
                    "[chunk {:3} @{:5}s peak={:.3} rms={:.5}] ERROR: {} ({}ms)",
                    idx,
                    start / 16_000,
                    peak,
                    rms,
                    e,
                    ms,
                );
            }
        }

        // Stop early if requested via env var (e.g., for fast iteration).
        if let Ok(limit) = std::env::var("DIMMY_LONG_WAV_MAX_CHUNKS") {
            if let Ok(n) = limit.parse::<usize>() {
                if idx >= n {
                    eprintln!(
                        "Stopping early at chunk {} (DIMMY_LONG_WAV_MAX_CHUNKS)",
                        idx
                    );
                    break;
                }
            }
        }
    }

    eprintln!(
        "\nSummary: {} chunks total, {} succeeded, {} empty, {} errored, total text {} chars",
        chunks.len(),
        succeeded,
        empty,
        errored,
        total_text_chars
    );
}
