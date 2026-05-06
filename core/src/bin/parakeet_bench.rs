//! Warm-path benchmark for Parakeet TDT v3 FP32.
//!
//! Runs 3 fixture lengths (short / medium / long) through
//! `dimmy_lib::parakeet::transcribe` and prints cold + warm timings.
//! Same model session is reused across calls — second invocation
//! measures pure inference (no model load).
//!
//! Build:
//!     cargo build --release --bin parakeet_bench --features local-stt-parakeet
//!
//! Run:
//!     ORT_DYLIB_PATH=...\onnxruntime.dll target\release\parakeet_bench.exe

use std::time::Instant;

const FIXTURES: &[(&str, &str)] = &[
    (
        "short  1.0s",
        r"C:\Users\konradd\AppData\Roaming\dimmy\audio_debug\2026-04-26_18-09-01\processed.wav",
    ),
    (
        "medium 13.6s",
        r"C:\Users\konradd\AppData\Roaming\dimmy\audio_debug\2026-05-02_23-23-03\processed.wav",
    ),
    (
        "long   50.1s",
        r"C:\Users\konradd\AppData\Roaming\dimmy\audio_debug\2026-05-02_23-43-16\processed.wav",
    ),
];

fn read_wav_16k_mono(path: &str) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample as i32;
            let scale = (1i64 << (bits - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
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

fn main() {
    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!(
            "[FAIL] bundle missing at {:?}",
            dimmy_lib::parakeet::bundle_dir()
        );
        std::process::exit(1);
    }

    println!("# Parakeet TDT v3 FP32 — Rust native warm bench");
    println!(
        "{:<14} {:>8} {:>8} {:>8} {:>8}  text",
        "label", "audio_ms", "cold_ms", "warm_ms", "rt_warm",
    );
    println!("{}", "-".repeat(80));

    for (label, path) in FIXTURES {
        let pcm = read_wav_16k_mono(path);
        let dur_ms = (pcm.len() as f32 / 16.0) as u32;

        // Cold (or "first call" — second fixture and beyond reuse the
        // already-loaded session, so cold for them is really another
        // warm call after a different-length input).
        let t0 = Instant::now();
        let cold_text = dimmy_lib::parakeet::transcribe(&pcm).expect("cold");
        let cold_ms = t0.elapsed().as_millis() as u32;

        // Warm: pure inference, model already loaded + warmed.
        let t1 = Instant::now();
        let warm_text = dimmy_lib::parakeet::transcribe(&pcm).expect("warm");
        let warm_ms = t1.elapsed().as_millis() as u32;
        let rt_warm = pcm.len() as f32 / 16_000.0 / (warm_ms as f32 / 1000.0);

        println!(
            "{:<14} {:>7}ms {:>7}ms {:>7}ms {:>6.1}x  {}",
            label,
            dur_ms,
            cold_ms,
            warm_ms,
            rt_warm,
            warm_text.chars().take(80).collect::<String>(),
        );
        assert_eq!(cold_text, warm_text, "{}: cold/warm output diverged", label);
    }
}
