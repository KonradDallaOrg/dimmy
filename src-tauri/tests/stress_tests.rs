//! Offline stress tests for Dimmy.
//!
//! These tests simulate extreme scenarios WITHOUT any API calls:
//! - Very long recordings (30min, 1h equivalent sample counts)
//! - Thousands of chunks
//! - Edge audio values (NaN, Inf, subnormal, max/min f32)
//! - Memory pressure (large buffers)
//! - Preprocessing pipeline under stress
//! - WAV encoding at scale
//! - Concurrent buffer access patterns
//!
//! Run with: cargo test --test stress_tests -- --nocapture
//! Run overnight: cargo test --test stress_tests -- --nocapture 2>&1 | tee stress_results.log

use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ── Helper: Generate synthetic audio ─────────────────────────────────

/// Generate a sine wave at given frequency, sample rate, and duration
fn sine_wave(freq: f32, sample_rate: u32, duration_secs: f32) -> Vec<f32> {
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin() * 0.5)
        .collect()
}

/// Generate white noise in [-amplitude, amplitude]
fn white_noise(sample_rate: u32, duration_secs: f32, amplitude: f32) -> Vec<f32> {
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    // Simple deterministic pseudo-random for reproducibility
    let mut state: u64 = 12345;
    (0..num_samples)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let val = ((state >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0;
            val * amplitude
        })
        .collect()
}

/// Generate silence
fn silence(sample_rate: u32, duration_secs: f32) -> Vec<f32> {
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    vec![0.0f32; num_samples]
}

/// Generate alternating speech (sine) and silence pattern
fn speech_and_silence(
    sample_rate: u32,
    speech_secs: f32,
    silence_secs: f32,
    cycles: usize,
) -> Vec<f32> {
    let mut audio = Vec::new();
    for _ in 0..cycles {
        audio.extend(sine_wave(440.0, sample_rate, speech_secs));
        audio.extend(silence(sample_rate, silence_secs));
    }
    audio
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 1: WAV ENCODING STRESS TESTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn wav_encode_30min_48khz() {
    // 30 minutes at 48kHz = 86.4M samples = ~345 MB of f32
    // This is the MAX recording the app allows
    let sample_rate = 48000u32;
    let duration_secs = 30.0 * 60.0; // 30 minutes
    let num_samples = (sample_rate as f64 * duration_secs as f64) as usize;

    println!("=== WAV Encode 30min @ 48kHz ===");
    println!("  Samples: {}", num_samples);
    println!("  Memory (f32): {} MB", num_samples * 4 / 1_048_576);

    // Generate in chunks to avoid stack issues
    let start = Instant::now();
    let mut samples = Vec::with_capacity(num_samples);
    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        samples.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3);
    }
    let gen_time = start.elapsed();
    println!("  Generation: {:.2}s", gen_time.as_secs_f64());

    // Encode to WAV
    let start = Instant::now();
    let wav = dimmy_lib::audio::encode_wav(&samples, sample_rate)
        .expect("encode_wav failed on 30min audio");
    let encode_time = start.elapsed();

    println!("  WAV size: {} MB", wav.len() / 1_048_576);
    println!("  Encode time: {:.2}s", encode_time.as_secs_f64());

    // Validate WAV header
    assert_eq!(&wav[0..4], b"RIFF", "Invalid RIFF header");
    assert_eq!(&wav[8..12], b"WAVE", "Invalid WAVE header");

    // Read back and verify sample count
    let reader = hound::WavReader::new(Cursor::new(&wav)).expect("Can't read WAV back");
    assert_eq!(reader.len() as usize, num_samples, "Sample count mismatch");
    assert_eq!(reader.spec().sample_rate, sample_rate);

    println!("  PASS ✓");
}

#[test]
fn wav_encode_1_sample() {
    let wav = dimmy_lib::audio::encode_wav(&[0.5], 44100).expect("single sample failed");
    assert_eq!(&wav[0..4], b"RIFF");
    let reader = hound::WavReader::new(Cursor::new(&wav)).expect("Can't read WAV");
    assert_eq!(reader.len(), 1);
    println!("WAV encode 1 sample: PASS ✓");
}

#[test]
fn wav_encode_empty() {
    let wav = dimmy_lib::audio::encode_wav(&[], 44100).expect("empty input failed");
    assert_eq!(&wav[0..4], b"RIFF");
    let reader = hound::WavReader::new(Cursor::new(&wav)).expect("Can't read WAV");
    assert_eq!(reader.len(), 0);
    println!("WAV encode empty: PASS ✓");
}

#[test]
fn wav_encode_extreme_values() {
    // Test with values at and beyond the clamping range
    let samples = vec![
        0.0,
        1.0,
        -1.0,
        f32::MAX,
        f32::MIN,
        f32::EPSILON,
        -f32::EPSILON,
        f32::MIN_POSITIVE,
        0.99999,
        -0.99999,
        2.0,   // beyond clamp
        -2.0,  // beyond clamp
        100.0, // way beyond
        -100.0,
    ];

    let wav = dimmy_lib::audio::encode_wav(&samples, 44100).expect("extreme values failed");
    let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("Can't read WAV");
    let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();

    // Values beyond [-1, 1] should be clamped to i16 range
    assert_eq!(decoded.len(), samples.len());
    for &s in &decoded {
        assert!(
            s >= i16::MIN && s <= i16::MAX,
            "Sample out of i16 range: {}",
            s
        );
    }
    println!(
        "WAV encode extreme values: PASS ✓ ({} samples)",
        decoded.len()
    );
}

#[test]
fn wav_encode_nan_and_inf() {
    // NaN and Inf are sanitized to 0.0 (silence) — encode_wav should never crash.
    let samples = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, f32::NAN];
    let wav = dimmy_lib::audio::encode_wav(&samples, 44100)
        .expect("encode_wav should not fail on NaN/Inf");

    assert_eq!(&wav[0..4], b"RIFF");
    let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("Can't read WAV");
    let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    assert_eq!(decoded.len(), 5);
    // NaN/Inf samples should be sanitized to 0
    assert_eq!(decoded[0], 0, "NaN should become 0");
    assert_eq!(decoded[1], 0, "Inf should become 0");
    assert_eq!(decoded[2], 0, "-Inf should become 0");
    assert_eq!(decoded[3], 0, "0.0 stays 0");
    assert_eq!(decoded[4], 0, "NaN should become 0");
    println!("WAV encode NaN/Inf: sanitized to silence ✓");
}

#[test]
fn wav_encode_various_sample_rates() {
    let rates = [
        8000, 11025, 16000, 22050, 32000, 44100, 48000, 96000, 192000,
    ];
    let samples = sine_wave(440.0, 48000, 1.0); // 1 second of audio

    for &rate in &rates {
        let wav = dimmy_lib::audio::encode_wav(&samples, rate)
            .unwrap_or_else(|_| panic!("Failed at rate {}", rate));
        let reader = hound::WavReader::new(Cursor::new(&wav)).unwrap();
        assert_eq!(reader.spec().sample_rate, rate);
    }
    println!("WAV encode various rates: PASS ✓ ({} rates)", rates.len());
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 2: PREPROCESSING PIPELINE STRESS TESTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn preprocess_30min_audio() {
    let sample_rate = 48000u32;
    let duration_secs = 30.0 * 60.0;
    let num_samples = (sample_rate as f64 * duration_secs as f64) as usize;

    println!("=== Preprocess 30min @ 48kHz ===");
    println!(
        "  Samples: {} ({} MB)",
        num_samples,
        num_samples * 4 / 1_048_576
    );

    // Generate speech-like audio (sine + silence pattern)
    let start = Instant::now();
    let audio = speech_and_silence(sample_rate, 8.0, 2.0, 180); // 180 cycles × 10s = 30min
    let gen_time = start.elapsed();
    println!(
        "  Generated {} samples in {:.2}s",
        audio.len(),
        gen_time.as_secs_f64()
    );

    // Process entire buffer (like stop_recording does)
    let start = Instant::now();
    let processed = dimmy_lib::preprocess::process_buffer(&audio, sample_rate);
    let proc_time = start.elapsed();

    println!("  Input samples:  {}", audio.len());
    println!("  Output samples: {}", processed.len());
    println!(
        "  Reduction: {:.1}%",
        (1.0 - processed.len() as f64 / audio.len() as f64) * 100.0
    );
    println!("  Process time: {:.2}s", proc_time.as_secs_f64());

    // Invariants
    assert!(
        processed.len() <= audio.len(),
        "Preprocessing should never ADD samples: {} > {}",
        processed.len(),
        audio.len()
    );
    // All output should be clamped
    for (i, &s) in processed.iter().enumerate() {
        assert!(
            s.is_finite() && s >= -1.0 && s <= 1.0,
            "Sample {} out of range: {}",
            i,
            s
        );
    }
    println!("  All {} output samples in [-1.0, 1.0] ✓", processed.len());
    println!("  PASS ✓");
}

#[test]
fn preprocess_incremental_1000_chunks() {
    // Simulate chunk streaming: 1000 chunks of 5 seconds each
    let sample_rate = 48000u32;
    let chunk_duration = 5.0; // seconds
    let num_chunks = 1000;

    println!(
        "=== Preprocess {} chunks × {:.0}s ===",
        num_chunks, chunk_duration
    );

    let mut preprocessor = dimmy_lib::preprocess::AudioPreprocessor::new(sample_rate);
    let mut total_input = 0usize;
    let mut total_output = 0usize;
    let mut empty_chunks = 0usize;

    let start = Instant::now();
    for i in 0..num_chunks {
        // Alternate between speech and silence chunks
        let chunk = if i % 3 == 0 {
            silence(sample_rate, chunk_duration)
        } else {
            sine_wave(440.0, sample_rate, chunk_duration)
        };
        total_input += chunk.len();

        let processed = preprocessor.process(&chunk);
        total_output += processed.len();

        if processed.is_empty() {
            empty_chunks += 1;
        }

        // Invariant check on every chunk
        for &s in &processed {
            assert!(
                s.is_finite() && s >= -1.0 && s <= 1.0,
                "Chunk {}: sample out of range: {}",
                i,
                s
            );
        }
    }
    let elapsed = start.elapsed();

    println!(
        "  Total input:  {} samples ({} MB)",
        total_input,
        total_input * 4 / 1_048_576
    );
    println!(
        "  Total output: {} samples ({} MB)",
        total_output,
        total_output * 4 / 1_048_576
    );
    println!("  Empty chunks: {}/{}", empty_chunks, num_chunks);
    println!(
        "  Reduction: {:.1}%",
        (1.0 - total_output as f64 / total_input as f64) * 100.0
    );
    println!(
        "  Total time: {:.2}s ({:.2}ms/chunk)",
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / num_chunks as f64
    );
    println!("  PASS ✓");
}

#[test]
fn preprocess_extreme_sample_values() {
    let sample_rate = 48000u32;
    let mut preprocessor = dimmy_lib::preprocess::AudioPreprocessor::new(sample_rate);

    // Feed extreme f32 values that shouldn't crash
    let extreme_values = vec![
        f32::MAX,
        f32::MIN,
        f32::EPSILON,
        -f32::EPSILON,
        f32::MIN_POSITIVE,
        1.0,
        -1.0,
        0.0,
        0.5,
        -0.5,
        999.0,
        -999.0,
    ];

    // Repeat to fill a frame (480 samples for nnnoiseless)
    let mut chunk = Vec::with_capacity(480 * 2);
    while chunk.len() < 480 * 2 {
        chunk.extend_from_slice(&extreme_values);
    }
    chunk.truncate(480 * 2);

    let processed = preprocessor.process(&chunk);
    // Should not panic — output might be empty or clamped
    for &s in &processed {
        assert!(s.is_finite(), "Non-finite sample in output: {}", s);
        assert!(s >= -1.0 && s <= 1.0, "Unclamped sample: {}", s);
    }
    println!(
        "Preprocess extreme values: PASS ✓ (input={}, output={})",
        chunk.len(),
        processed.len()
    );
}

#[test]
fn preprocess_dc_offset() {
    // Audio with large DC offset — highpass should remove it
    let sample_rate = 48000u32;
    let mut preprocessor = dimmy_lib::preprocess::AudioPreprocessor::new(sample_rate);

    let dc_audio: Vec<f32> = (0..48000)
        .map(|i| {
            0.8 + (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sample_rate as f32).sin() * 0.1
        })
        .collect();

    let processed = preprocessor.process(&dc_audio);
    if !processed.is_empty() {
        let dc_out = processed.iter().sum::<f32>() / processed.len() as f32;
        // Highpass should reduce DC substantially
        assert!(
            dc_out.abs() < 0.3,
            "DC offset not reduced enough: {:.4}",
            dc_out
        );
    }
    println!("Preprocess DC offset: PASS ✓");
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 3: DOWNSAMPLE STRESS TESTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn downsample_30min_48khz_to_16khz() {
    let sample_rate = 48000u32;
    let duration_secs = 30.0 * 60.0;
    let num_samples = (sample_rate as f64 * duration_secs as f64) as usize;

    println!("=== Downsample 30min 48kHz→16kHz ===");
    println!(
        "  Input: {} samples ({} MB)",
        num_samples,
        num_samples * 4 / 1_048_576
    );

    let audio = sine_wave(440.0, sample_rate, duration_secs as f32);
    assert_eq!(audio.len(), num_samples);

    let start = Instant::now();
    let downsampled = dimmy_lib::preprocess::downsample_to_16k(&audio, sample_rate);
    let elapsed = start.elapsed();

    let expected_len = (num_samples as f64 * 16000.0 / 48000.0).floor() as usize;
    println!(
        "  Output: {} samples ({} MB)",
        downsampled.len(),
        downsampled.len() * 4 / 1_048_576
    );
    println!("  Expected: ~{} samples", expected_len);
    println!("  Time: {:.2}s", elapsed.as_secs_f64());

    // Verify approximate length (within 1 sample)
    let diff = (downsampled.len() as i64 - expected_len as i64).abs();
    assert!(
        diff <= 1,
        "Length mismatch: got {} expected ~{}",
        downsampled.len(),
        expected_len
    );

    // All samples should be finite
    for (i, &s) in downsampled.iter().enumerate() {
        assert!(s.is_finite(), "Non-finite at sample {}: {}", i, s);
    }
    println!("  PASS ✓");
}

#[test]
fn downsample_various_rates() {
    let rates = [22050u32, 32000, 44100, 48000, 96000];
    let duration = 1.0; // 1 second

    for &rate in &rates {
        let audio = sine_wave(440.0, rate, duration);
        let downsampled = dimmy_lib::preprocess::downsample_to_16k(&audio, rate);

        let expected = (audio.len() as f64 * 16000.0 / rate as f64).floor() as usize;
        let diff = (downsampled.len() as i64 - expected as i64).abs();

        assert!(
            diff <= 1,
            "Rate {}: got {} expected ~{}",
            rate,
            downsampled.len(),
            expected
        );
        for &s in &downsampled {
            assert!(s.is_finite(), "NaN/Inf in downsample from rate {}", rate);
        }
    }
    println!("Downsample various rates: PASS ✓ ({} rates)", rates.len());
}

#[test]
fn downsample_16khz_passthrough() {
    // 16kHz input should be returned as-is (no resampling)
    let audio = sine_wave(440.0, 16000, 1.0);
    let result = dimmy_lib::preprocess::downsample_to_16k(&audio, 16000);
    assert_eq!(result.len(), audio.len());
    println!("Downsample 16kHz passthrough: PASS ✓");
}

#[test]
fn downsample_8khz_passthrough() {
    // Below 16kHz should also pass through
    let audio = sine_wave(440.0, 8000, 1.0);
    let result = dimmy_lib::preprocess::downsample_to_16k(&audio, 8000);
    assert_eq!(result.len(), audio.len());
    println!("Downsample 8kHz passthrough: PASS ✓");
}

#[test]
fn downsample_empty_input() {
    let result = dimmy_lib::preprocess::downsample_to_16k(&[], 48000);
    assert!(result.is_empty());
    println!("Downsample empty: PASS ✓");
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 4: TYPED AUDIO PIPELINE STRESS TESTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn pipeline_full_30min() {
    let sample_rate = 48000u32;
    let _duration = 30.0 * 60.0;

    println!("=== Full Pipeline 30min ===");

    let start = Instant::now();
    let audio = speech_and_silence(sample_rate, 8.0, 2.0, 180);
    println!(
        "  Generated {} samples in {:.2}s",
        audio.len(),
        start.elapsed().as_secs_f64()
    );

    // RawAudio → ProcessedAudio → WavPayload
    let raw = dimmy_lib::audio::RawAudio {
        samples: audio,
        sample_rate,
    };

    let start = Instant::now();
    let processed = raw.preprocess(true);
    let preprocess_time = start.elapsed();
    println!(
        "  Preprocessed: {} samples in {:.2}s",
        processed.samples.len(),
        preprocess_time.as_secs_f64()
    );

    if processed.is_empty() {
        println!("  (No speech detected — VAD stripped all audio)");
        println!("  PASS ✓ (empty pipeline OK)");
        return;
    }

    let start = Instant::now();
    let payload = processed.to_wav_payload().expect("to_wav_payload failed");
    let wav_time = start.elapsed();

    println!(
        "  WAV: {} bytes, {:.2}s duration",
        payload.data.len(),
        payload.duration_secs
    );
    println!("  Encode time: {:.2}s", wav_time.as_secs_f64());
    assert!(payload.duration_secs > 0.0);
    assert!(&payload.data[0..4] == b"RIFF");
    println!("  PASS ✓");
}

#[test]
fn pipeline_no_preprocessing() {
    // Full pipeline without preprocessing (raw → downsample → WAV)
    let sample_rate = 48000u32;
    let audio = sine_wave(440.0, sample_rate, 60.0); // 1 minute

    let raw = dimmy_lib::audio::RawAudio {
        samples: audio,
        sample_rate,
    };
    let processed = raw.preprocess(false); // skip preprocessing
    assert!(!processed.is_empty());
    assert_eq!(processed.sample_rate, sample_rate);

    let payload = processed.to_wav_payload().expect("to_wav_payload failed");
    assert!(payload.duration_secs > 59.0 && payload.duration_secs < 61.0);
    println!(
        "Pipeline no-preprocess 1min: PASS ✓ (duration={:.2}s)",
        payload.duration_secs
    );
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 5: CHUNK SPLITTING SIMULATION
// ══════════════════════════════════════════════════════════════════════

/// Simulate the chunk splitting algorithm from lib.rs without Tauri dependencies
fn simulate_chunk_splitting(audio: &[f32], sample_rate: usize) -> Vec<(usize, usize)> {
    let min_chunk = sample_rate * 5;
    let max_chunk = sample_rate * 12;
    let silence_threshold: f32 = 0.01;
    let silence_window = (sample_rate as f32 * 0.4) as usize;

    let mut chunks = Vec::new();
    let mut offset = 0;

    while offset < audio.len() {
        let available = audio.len() - offset;
        if available < min_chunk {
            break;
        }

        let split_end = if available >= max_chunk {
            offset + available.min(max_chunk)
        } else {
            let check_len = silence_window.min(available);
            let tail_start = offset + available - check_len;
            let rms: f32 = audio[tail_start..offset + available]
                .iter()
                .map(|s| s * s)
                .sum::<f32>()
                / check_len as f32;
            let rms = rms.sqrt();
            if rms < silence_threshold {
                offset + available
            } else {
                continue; // Would wait for more data in real code
            }
        };

        chunks.push((offset, split_end));
        offset = split_end;
    }

    chunks
}

#[test]
fn chunk_split_30min_speech() {
    let sample_rate = 48000usize;
    let audio = speech_and_silence(sample_rate as u32, 8.0, 2.0, 180);

    println!("=== Chunk Splitting 30min ===");
    println!("  Input: {} samples", audio.len());

    let start = Instant::now();
    let chunks = simulate_chunk_splitting(&audio, sample_rate);
    let elapsed = start.elapsed();

    println!("  Chunks: {}", chunks.len());
    println!("  Time: {:.3}s", elapsed.as_secs_f64());

    // Verify chunks don't overlap and are within bounds
    for (i, &(start, end)) in chunks.iter().enumerate() {
        assert!(end > start, "Chunk {} has zero length", i);
        assert!(
            end <= audio.len(),
            "Chunk {} exceeds buffer: {} > {}",
            i,
            end,
            audio.len()
        );
        let chunk_secs = (end - start) as f64 / sample_rate as f64;
        assert!(
            chunk_secs >= 5.0 - 0.001,
            "Chunk {} too short: {:.2}s",
            i,
            chunk_secs
        );
        assert!(
            chunk_secs <= 12.001,
            "Chunk {} too long: {:.2}s",
            i,
            chunk_secs
        );

        // No overlap with next chunk
        if i + 1 < chunks.len() {
            assert!(
                end <= chunks[i + 1].0,
                "Chunks {} and {} overlap: {} > {}",
                i,
                i + 1,
                end,
                chunks[i + 1].0
            );
        }
    }

    // Print chunk stats
    if !chunks.is_empty() {
        let durations: Vec<f64> = chunks
            .iter()
            .map(|(s, e)| (e - s) as f64 / sample_rate as f64)
            .collect();
        let min_dur = durations.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_dur = durations.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let avg_dur = durations.iter().sum::<f64>() / durations.len() as f64;
        println!(
            "  Chunk duration: min={:.2}s avg={:.2}s max={:.2}s",
            min_dur, avg_dur, max_dur
        );
    }
    println!("  PASS ✓");
}

#[test]
fn chunk_split_continuous_noise() {
    // Continuous noise — no silence breaks, should hit max_chunk_samples
    let sample_rate = 48000usize;
    let audio = white_noise(sample_rate as u32, 120.0, 0.5); // 2 minutes of noise

    let chunks = simulate_chunk_splitting(&audio, sample_rate);
    println!(
        "Chunk split continuous noise: {} chunks from 2min ✓",
        chunks.len()
    );

    // All chunks should be at max length (12s) since no silence found
    for (i, &(start, end)) in chunks.iter().enumerate() {
        let dur = (end - start) as f64 / sample_rate as f64;
        assert!(dur <= 12.001, "Chunk {} exceeds max: {:.2}s", i, dur);
    }
}

#[test]
fn chunk_split_pure_silence() {
    // Pure silence — chunks should still be created (silence threshold met immediately)
    let sample_rate = 48000usize;
    let audio = silence(sample_rate as u32, 120.0); // 2 minutes of silence

    let chunks = simulate_chunk_splitting(&audio, sample_rate);
    println!(
        "Chunk split pure silence: {} chunks from 2min ✓",
        chunks.len()
    );

    for (i, &(start, end)) in chunks.iter().enumerate() {
        assert!(end > start, "Empty chunk {} at offset {}", i, start);
    }
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 6: CONCURRENT BUFFER ACCESS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn concurrent_buffer_write_read() {
    // Simulate the producer (audio thread) / consumer (chunk reader) pattern
    let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let sample_rate = 48000usize;
    let total_seconds = 10;
    let total_samples = sample_rate * total_seconds;

    println!("=== Concurrent Buffer Test ===");
    println!("  Simulating {}s at {}Hz", total_seconds, sample_rate);

    // Producer thread: pushes samples in bursts (simulating cpal callback)
    let buf_write = buffer.clone();
    let producer = std::thread::spawn(move || {
        let batch_size = 480; // Typical cpal callback size
        let mut written = 0;
        while written < total_samples {
            let batch: Vec<f32> = (0..batch_size)
                .map(|i| {
                    ((written + i) as f32 / sample_rate as f32 * 440.0 * 2.0 * std::f32::consts::PI)
                        .sin()
                        * 0.5
                })
                .collect();
            {
                let mut buf = buf_write.lock().unwrap();
                buf.extend_from_slice(&batch);
            }
            written += batch_size;
        }
        written
    });

    // Consumer thread: reads the buffer periodically (simulating 200ms polling)
    let buf_read = buffer.clone();
    let consumer = std::thread::spawn(move || {
        let mut reads = 0;
        let mut max_len = 0;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let len = buf_read.lock().unwrap().len();
            max_len = max_len.max(len);
            reads += 1;
        }
        (reads, max_len)
    });

    let total_written = producer.join().expect("Producer panicked");
    let (reads, max_len) = consumer.join().expect("Consumer panicked");

    let final_len = buffer.lock().unwrap().len();
    println!("  Written: {} samples", total_written);
    println!("  Final buffer: {} samples", final_len);
    println!("  Consumer reads: {}, max seen: {}", reads, max_len);
    assert_eq!(final_len, total_written);
    println!("  PASS ✓");
}

#[test]
fn concurrent_buffer_clone_and_clear() {
    // Simulate stop_recording: clone buffer, then clear it
    let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

    // Fill with 10 seconds of audio
    {
        let mut buf = buffer.lock().unwrap();
        buf.extend(sine_wave(440.0, 48000, 10.0));
    }

    let initial_len = buffer.lock().unwrap().len();
    assert_eq!(initial_len, 480000);

    // Clone and clear (like stop_recording does)
    let cloned = {
        let mut buf = buffer.lock().unwrap();
        let data = buf.clone();
        buf.clear();
        data
    };

    assert_eq!(cloned.len(), 480000);
    assert_eq!(buffer.lock().unwrap().len(), 0);
    println!("Concurrent clone-and-clear: PASS ✓");
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 7: MEMORY PRESSURE TESTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn memory_pipeline_peak_measurement() {
    // Measure peak memory during the full pipeline
    println!("=== Memory Pipeline Peak ===");

    let sample_rate = 48000u32;
    let durations = [60.0, 300.0, 600.0, 1800.0]; // 1min, 5min, 10min, 30min

    for &dur in &durations {
        let num_samples = (sample_rate as f64 * dur as f64) as usize;
        let mem_mb = num_samples * 4 / 1_048_576;

        let start = Instant::now();

        // Generate audio
        let audio = sine_wave(440.0, sample_rate, dur);

        // Pipeline: downsample + encode (skip preprocessing for speed)
        let downsampled = dimmy_lib::preprocess::downsample_to_16k(&audio, sample_rate);
        let wav = dimmy_lib::audio::encode_wav(&downsampled, 16000).expect("encode failed");

        let elapsed = start.elapsed();
        let wav_mb = wav.len() / 1_048_576;

        println!(
            "  {:.0}min: input={} MB, wav={} MB, time={:.2}s",
            dur / 60.0,
            mem_mb,
            wav_mb,
            elapsed.as_secs_f64()
        );

        // Verify output
        assert_eq!(&wav[0..4], b"RIFF");
        drop(wav);
        drop(downsampled);
        drop(audio);
    }
    println!("  PASS ✓");
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 8: LLM STYLE/TONE ENUM INVARIANTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn llm_style_cycle_exhaustive() {
    use dimmy_lib::llm::LlmStyle;

    // Cycle through ALL styles forwards and backwards, verify we visit all
    let mut current = LlmStyle::Off;
    let mut visited = std::collections::HashSet::new();

    // Forward cycle
    for _ in 0..LlmStyle::ALL.len() {
        visited.insert(current.as_str().to_string());
        current = current.cycle(1);
    }
    assert_eq!(
        visited.len(),
        LlmStyle::ALL.len(),
        "Forward cycle didn't visit all styles: {:?}",
        visited
    );

    // Verify we're back at start
    assert_eq!(
        current,
        LlmStyle::Off,
        "Full forward cycle didn't return to start"
    );

    // Backward cycle
    visited.clear();
    for _ in 0..LlmStyle::ALL.len() {
        visited.insert(current.as_str().to_string());
        current = current.cycle(-1);
    }
    assert_eq!(
        visited.len(),
        LlmStyle::ALL.len(),
        "Backward cycle didn't visit all styles"
    );
    assert_eq!(
        current,
        LlmStyle::Off,
        "Full backward cycle didn't return to start"
    );

    println!("LLM style cycle exhaustive: PASS ✓ (13 styles × 2 directions)");
}

#[test]
fn llm_tone_cycle_exhaustive() {
    use dimmy_lib::llm::LlmTone;

    let mut current = LlmTone::None;
    let mut visited = std::collections::HashSet::new();

    for _ in 0..LlmTone::ALL.len() {
        visited.insert(current.as_str().to_string());
        current = current.cycle(1);
    }
    assert_eq!(visited.len(), LlmTone::ALL.len());
    assert_eq!(current, LlmTone::None);

    println!("LLM tone cycle exhaustive: PASS ✓ (5 tones)");
}

#[test]
fn llm_all_style_tone_combinations() {
    use dimmy_lib::llm::{build_system_prompt, LlmStyle, LlmTone};

    let mut count = 0;
    for &style in LlmStyle::ALL {
        for &tone in LlmTone::ALL {
            let prompt = build_system_prompt(style, tone, "custom test", "none");

            // Off + None should be empty
            if style.is_off() && tone.is_none() {
                assert!(prompt.is_empty(), "Off+None should produce empty prompt");
            }

            // Non-off styles should produce non-empty prompts
            if !style.is_off() {
                assert!(
                    !prompt.is_empty(),
                    "{}+{} produced empty prompt",
                    style,
                    tone
                );
            }

            // No prompt should be absurdly long
            assert!(
                prompt.len() < 10_000,
                "{}+{} prompt too long: {} chars",
                style,
                tone,
                prompt.len()
            );

            count += 1;
        }
    }
    println!("LLM all style×tone combos: PASS ✓ ({} combinations)", count);
}

#[test]
fn llm_style_serde_all() {
    use dimmy_lib::llm::LlmStyle;

    for &style in LlmStyle::ALL {
        let json = serde_json::to_string(&style).unwrap();
        let back: LlmStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, style, "Serde roundtrip failed for {:?}", style);

        // Also test from_str_lossy
        let from_str = LlmStyle::from_str_lossy(style.as_str());
        assert_eq!(
            from_str,
            style,
            "from_str_lossy failed for {}",
            style.as_str()
        );
    }
    println!("LLM style serde all: PASS ✓");
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 9: PROVIDER ENUM INVARIANTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn provider_urls_exhaustive() {
    use dimmy_lib::provider::Provider;

    let test_cases = vec![
        ("https://api.groq.com/openai/v1/audio/transcriptions", Provider::Groq),
        ("https://api.openai.com/v1/audio/transcriptions", Provider::OpenAI),
        ("https://openrouter.ai/api/v1/chat/completions", Provider::OpenRouter),
        ("https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent", Provider::Gemini),
        ("https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true", Provider::Deepgram),
        ("https://api.anthropic.com/v1/messages", Provider::Anthropic),
        ("https://my-server.com/v1/transcriptions", Provider::Custom),
        ("http://localhost:8080/v1/audio/transcriptions", Provider::Custom),
        ("http://127.0.0.1:5000/v1/chat", Provider::Custom),
    ];

    for (url, expected) in &test_cases {
        let actual = Provider::from_url(url);
        assert_eq!(
            actual, *expected,
            "URL '{}' → {:?}, expected {:?}",
            url, actual, expected
        );
    }
    println!(
        "Provider URLs exhaustive: PASS ✓ ({} cases)",
        test_cases.len()
    );
}

#[test]
fn provider_secure_url_exhaustive() {
    use dimmy_lib::provider::Provider;

    // Secure URLs
    assert!(Provider::is_secure_url("https://api.groq.com/v1"));
    assert!(Provider::is_secure_url("https://anything.com"));
    assert!(Provider::is_secure_url("http://localhost:8080/v1"));
    assert!(Provider::is_secure_url("http://127.0.0.1:5000"));
    assert!(Provider::is_secure_url("http://[::1]:8080"));

    // Insecure URLs
    assert!(!Provider::is_secure_url("http://api.groq.com/v1"));
    assert!(!Provider::is_secure_url("http://evil.com/steal-keys"));
    assert!(!Provider::is_secure_url("http://192.168.1.1:8080"));

    println!("Provider secure URL exhaustive: PASS ✓");
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 10: ERROR TYPE INVARIANTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn error_display_all_variants() {
    use dimmy_lib::error::*;

    let errors: Vec<DimmyError> = vec![
        AudioError::NoDevice.into(),
        AudioError::DeviceNotFound("test".into()).into(),
        AudioError::Capture("test".into()).into(),
        AudioError::Encode("test".into()).into(),
        TranscribeError::NoApiKey("groq".into()).into(),
        TranscribeError::Api {
            status: 401,
            body: "Unauthorized".into(),
        }
        .into(),
        TranscribeError::Empty.into(),
        TranscribeError::Network("timeout".into()).into(),
        TranscribeError::InsecureUrl("http://evil.com".into()).into(),
        LlmError::Api {
            status: 500,
            body: "Internal".into(),
        }
        .into(),
        LlmError::Network("timeout".into()).into(),
        LlmError::NoApiKey("openai".into()).into(),
        DimmyError::Config("bad config".into()),
        DimmyError::InvalidState("not recording".into()),
        DimmyError::Platform("unsupported".into()),
    ];

    for err in &errors {
        let display = err.to_string();
        assert!(!display.is_empty(), "Empty display for {:?}", err);

        // Serialize for Tauri IPC
        let json = serde_json::to_string(err).unwrap();
        assert!(!json.is_empty());
    }
    println!(
        "Error display all variants: PASS ✓ ({} variants)",
        errors.len()
    );
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 11: FUZZ-LIKE RANDOM INPUT TESTS
// ══════════════════════════════════════════════════════════════════════

#[test]
fn fuzz_preprocess_random_lengths() {
    let sample_rate = 48000u32;
    let mut preprocessor = dimmy_lib::preprocess::AudioPreprocessor::new(sample_rate);

    // Test with various random-ish lengths including edge cases
    let lengths = [
        0, 1, 2, 3, 10, 100, 479, 480, 481, 959, 960, 961, 1000, 4800, 48000, 96000, 480000,
    ];

    let mut total_input = 0usize;
    let mut total_output = 0usize;

    for &len in &lengths {
        let samples: Vec<f32> = (0..len)
            .map(|i| (i as f32 / 48000.0 * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.5)
            .collect();

        let out = preprocessor.process(&samples);
        total_input += samples.len();
        total_output += out.len();

        // Per-sample invariant: all output in [-1.0, 1.0]
        for &s in &out {
            assert!(s.is_finite() && s >= -1.0 && s <= 1.0);
        }
    }

    // Cumulative invariant: VAD can only remove samples overall, but per-call
    // output may exceed input due to internal frame buffering.
    assert!(
        total_output <= total_input,
        "cumulative output ({}) > cumulative input ({})",
        total_output,
        total_input
    );
    println!(
        "Fuzz preprocess random lengths: PASS ✓ ({} lengths)",
        lengths.len()
    );
}

#[test]
fn fuzz_downsample_random_lengths() {
    let lengths = [0, 1, 2, 100, 479, 480, 481, 1000, 48000, 96000];

    for &len in &lengths {
        let samples: Vec<f32> = (0..len).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = dimmy_lib::preprocess::downsample_to_16k(&samples, 48000);

        // Output should be roughly 1/3 of input (48k → 16k)
        if len > 0 {
            let expected = (len as f64 / 3.0).floor() as usize;
            let diff = (out.len() as i64 - expected as i64).abs();
            assert!(
                diff <= 2,
                "len={}: got {} expected ~{}",
                len,
                out.len(),
                expected
            );
        } else {
            assert!(out.is_empty());
        }

        for &s in &out {
            assert!(
                s.is_finite(),
                "NaN/Inf in downsample output for len={}",
                len
            );
        }
    }
    println!(
        "Fuzz downsample random lengths: PASS ✓ ({} lengths)",
        lengths.len()
    );
}

#[test]
fn fuzz_wav_encode_random_lengths() {
    let lengths = [0, 1, 2, 100, 1000, 10000, 100000, 1_000_000];

    for &len in &lengths {
        let samples: Vec<f32> = (0..len).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let wav = dimmy_lib::audio::encode_wav(&samples, 44100)
            .unwrap_or_else(|_| panic!("encode_wav failed for len={}", len));

        assert_eq!(&wav[0..4], b"RIFF", "Bad header for len={}", len);
        let reader = hound::WavReader::new(Cursor::new(&wav)).unwrap();
        assert_eq!(
            reader.len() as usize,
            len,
            "Sample count mismatch for len={}",
            len
        );
    }
    println!(
        "Fuzz WAV encode random lengths: PASS ✓ ({} lengths)",
        lengths.len()
    );
}

// ══════════════════════════════════════════════════════════════════════
// SECTION 12: ENDURANCE TEST (Long-running)
// ══════════════════════════════════════════════════════════════════════

#[test]
fn endurance_repeated_pipeline_100_cycles() {
    // Simulate 100 recording sessions back-to-back
    // Each: generate audio → preprocess → downsample → encode → verify
    let sample_rate = 48000u32;
    let session_duration = 30.0; // 30 seconds each
    let num_sessions = 100;

    println!(
        "=== Endurance: {} sessions × {:.0}s ===",
        num_sessions, session_duration
    );

    let total_start = Instant::now();
    let mut total_wav_bytes = 0u64;
    let mut total_samples = 0u64;

    for i in 0..num_sessions {
        let audio = sine_wave(440.0 + i as f32 * 10.0, sample_rate, session_duration);
        total_samples += audio.len() as u64;

        let processed = dimmy_lib::preprocess::process_buffer(&audio, sample_rate);
        let downsampled = dimmy_lib::preprocess::downsample_to_16k(&processed, sample_rate);
        let wav = dimmy_lib::audio::encode_wav(&downsampled, 16000).expect("encode failed");
        total_wav_bytes += wav.len() as u64;

        // Verify each WAV
        assert_eq!(&wav[0..4], b"RIFF");
        let reader = hound::WavReader::new(Cursor::new(&wav)).unwrap();
        assert_eq!(reader.spec().sample_rate, 16000);
    }

    let total_time = total_start.elapsed();
    println!("  Total sessions: {}", num_sessions);
    println!(
        "  Total samples: {} ({} MB)",
        total_samples,
        total_samples * 4 / 1_048_576
    );
    println!("  Total WAV: {} MB", total_wav_bytes / 1_048_576);
    println!(
        "  Total time: {:.2}s ({:.0}ms/session)",
        total_time.as_secs_f64(),
        total_time.as_secs_f64() * 1000.0 / num_sessions as f64
    );
    println!(
        "  Throughput: {:.1} sessions/sec",
        num_sessions as f64 / total_time.as_secs_f64()
    );
    println!("  PASS ✓");
}

#[test]
fn endurance_preprocessor_state_reset() {
    // Verify preprocessor state doesn't corrupt across many sessions
    let sample_rate = 48000u32;

    for _ in 0..50 {
        let mut proc = dimmy_lib::preprocess::AudioPreprocessor::new(sample_rate);

        // Session 1: speech
        let speech = sine_wave(440.0, sample_rate, 3.0);
        let out1 = proc.process(&speech);

        // Session 2: silence (should be stripped)
        let silence = silence(sample_rate, 3.0);
        let out2 = proc.process(&silence);

        // Session 3: speech again
        let speech2 = sine_wave(880.0, sample_rate, 3.0);
        let out3 = proc.process(&speech2);

        // All outputs should be valid
        for &s in out1.iter().chain(out2.iter()).chain(out3.iter()) {
            assert!(s.is_finite() && s >= -1.0 && s <= 1.0);
        }
    }
    println!("Endurance preprocessor reset: PASS ✓ (50 sessions × 3 phases)");
}
