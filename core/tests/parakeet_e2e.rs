//! End-to-end integration test for the Parakeet TDT v3 FP32 path.
//!
//! Skips cleanly (early return with `eprintln`) when:
//! - the `local-stt-parakeet` cargo feature is off, OR
//! - the model bundle is not at `<config-dir>/parakeet-fp32/`, OR
//! - the audio fixture is not on disk.
//!
//! Run with:
//!     cargo test --test parakeet_e2e --features local-stt-parakeet
//!
//! `onnxruntime.dll` / `.dylib` must be discoverable: either next to the
//! test binary (target/debug/deps/), in `PATH`, or via `ORT_DYLIB_PATH`.
//!
//! Fixtures are debug captures the developer recorded with Dimmy under
//! `~/AppData/Roaming/dimmy/audio_debug/<timestamp>/processed.wav` on
//! Windows or the platform equivalent. Picked because they exercise
//! short / medium / long lengths in Italian — the language Parakeet v3 is
//! used for in production. CI without the bundle skips at the gate.

#![cfg(feature = "local-stt-parakeet")]

use std::path::{Path, PathBuf};
use std::time::Instant;

/// Fixtures of varying length the developer captured locally. The
/// `expected_substring` is a fragment of the byte-identical Python
/// `onnx_asr` output — substring check survives minor regressions in
/// punctuation while still catching real algorithm drift.
struct Fixture {
    label: &'static str,
    path: &'static str,
    expected_substring: &'static str,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "short 1.8s",
        path: r"audio_debug\2026-05-02_23-34-12\processed.wav",
        expected_substring: "altra volta",
    },
    Fixture {
        label: "medium 5.2s",
        path: r"audio_debug\2026-05-02_23-32-26\processed.wav",
        expected_substring: "un'altra volta",
    },
    Fixture {
        label: "long 50s",
        path: r"audio_debug\2026-05-02_23-43-16\processed.wav",
        expected_substring: "Il secondo baco",
    },
];

fn fixture_root() -> Option<PathBuf> {
    dimmy_lib::config_dir_path()
}

fn load_wav_16k_mono(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample as i32;
            let scale = (1i64 << (bits - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.expect("read i32 sample") as f32 / scale)
                .collect()
        }
        hound::SampleFormat::Float => r
            .samples::<f32>()
            .map(|s| s.expect("read f32 sample"))
            .collect(),
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

#[test]
fn transcribe_italian_fixtures() {
    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!(
            "[skip] parakeet bundle not at {:?} — skipping integration test",
            dimmy_lib::parakeet::bundle_dir()
        );
        return;
    }

    let Some(root) = fixture_root() else {
        eprintln!("[skip] config_dir_path() returned None");
        return;
    };

    let mut ran_any = false;
    for fx in FIXTURES {
        let full = root.join(fx.path);
        if !full.exists() {
            eprintln!("[skip] {} fixture missing at {:?}", fx.label, full);
            continue;
        }
        ran_any = true;

        let pcm = load_wav_16k_mono(&full);
        assert!(!pcm.is_empty(), "{}: empty pcm", fx.label);

        let t = Instant::now();
        let text = dimmy_lib::parakeet::transcribe(&pcm)
            .unwrap_or_else(|e| panic!("{}: transcribe failed: {}", fx.label, e));
        let ms = t.elapsed().as_millis();

        eprintln!("[{}] {}ms → {:?}", fx.label, ms, text);
        assert!(
            text.contains(fx.expected_substring),
            "{}: expected substring {:?} not found in {:?}",
            fx.label,
            fx.expected_substring,
            text,
        );
    }

    if !ran_any {
        eprintln!("[skip] no fixtures present — full test path was not exercised");
    }
}

/// Regression guard: the same input must produce byte-identical output
/// across two warm calls. Catches non-determinism creeping in (e.g.,
/// stale LSTM state being carried over between calls).
#[test]
fn warm_call_is_deterministic() {
    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!("[skip] bundle missing");
        return;
    }
    let Some(root) = fixture_root() else {
        return;
    };
    let path = root.join(FIXTURES[0].path);
    if !path.exists() {
        eprintln!("[skip] fixture missing: {:?}", path);
        return;
    }
    let pcm = load_wav_16k_mono(&path);

    let a = dimmy_lib::parakeet::transcribe(&pcm).expect("first call");
    let b = dimmy_lib::parakeet::transcribe(&pcm).expect("second call");
    assert_eq!(a, b, "warm call drift: {:?} vs {:?}", a, b);
}

/// Empty PCM must return Ok("") — we rely on this to short-circuit
/// VAD-trimmed silent buffers in the upstream pipeline without paying
/// the model-load cost.
#[test]
fn empty_pcm_returns_empty_string() {
    // No bundle check needed — empty PCM short-circuits before model
    // load. This test runs even without a bundle present.
    let res = dimmy_lib::parakeet::transcribe(&[]);
    assert!(matches!(res, Ok(ref s) if s.is_empty()), "got {:?}", res);
}
