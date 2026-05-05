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
//! Fixtures are sourced two ways:
//!
//! - A small public-domain English clip (`tests/fixtures/jfk_16k_mono.wav`,
//!   ~344 KB) is committed alongside the test so the full path works on
//!   any developer's machine the moment the Parakeet bundle is on disk.
//!   This is the canonical smoke fixture across platforms.
//! - Italian dev captures under `<config-dir>/audio_debug/<timestamp>/processed.wav`
//!   exercise longer / harder samples but only land if the developer has
//!   recorded with the audio-debug toggle on. They skip cleanly otherwise.
//!
//! CI without the bundle skips at the gate.

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

/// Italian audio-debug captures (only on the developer's machine that
/// recorded them). Path is platform-agnostic via `Path::join` — the
/// joined components survive both Windows backslashes and POSIX slashes
/// at runtime.
const ITALIAN_FIXTURES: &[Fixture] = &[
    Fixture {
        label: "short 1.8s",
        path: "audio_debug/2026-05-02_23-34-12/processed.wav",
        expected_substring: "altra volta",
    },
    Fixture {
        label: "medium 5.2s",
        path: "audio_debug/2026-05-02_23-32-26/processed.wav",
        expected_substring: "un'altra volta",
    },
    Fixture {
        label: "long 50s",
        path: "audio_debug/2026-05-02_23-43-16/processed.wav",
        expected_substring: "Il secondo baco",
    },
];

/// Committed JFK clip — public-domain, 11 s mono 16 kHz. Path is relative
/// to the crate root (CARGO_MANIFEST_DIR) so it resolves under any tier.
const JFK_FIXTURE: Fixture = Fixture {
    label: "JFK 11s (committed)",
    path: "tests/fixtures/jfk_16k_mono.wav",
    expected_substring: "fellow Americans",
};

fn italian_fixture_root() -> Option<PathBuf> {
    dimmy_lib::config_dir_path()
}

fn jfk_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

/// Cross-platform smoke: the committed JFK clip is byte-identical on every
/// developer's machine, so this case has the same shape on Win/Mac/Linux
/// and lights up the moment the bundle is downloaded.
#[test]
fn transcribe_committed_jfk_fixture() {
    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!(
            "[skip] parakeet bundle not at {:?} — skipping committed-fixture test",
            dimmy_lib::parakeet::bundle_dir()
        );
        return;
    }

    let full = jfk_fixture_root().join(JFK_FIXTURE.path);
    assert!(
        full.exists(),
        "committed fixture missing at {:?} — should be in the repo",
        full
    );

    let pcm = load_wav_16k_mono(&full);
    assert!(!pcm.is_empty(), "JFK fixture pcm empty");

    let t = Instant::now();
    let text = dimmy_lib::parakeet::transcribe(&pcm)
        .unwrap_or_else(|e| panic!("JFK transcribe failed: {}", e));
    let ms = t.elapsed().as_millis();

    eprintln!("[{}] {}ms → {:?}", JFK_FIXTURE.label, ms, text);
    assert!(
        text.contains(JFK_FIXTURE.expected_substring),
        "expected substring {:?} not found in {:?}",
        JFK_FIXTURE.expected_substring,
        text,
    );
}

#[test]
fn transcribe_italian_fixtures() {
    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!(
            "[skip] parakeet bundle not at {:?} — skipping italian fixtures",
            dimmy_lib::parakeet::bundle_dir()
        );
        return;
    }

    let Some(root) = italian_fixture_root() else {
        eprintln!("[skip] config_dir_path() returned None");
        return;
    };

    let mut ran_any = false;
    for fx in ITALIAN_FIXTURES {
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
        eprintln!("[skip] no italian fixtures present — only the JFK case ran");
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
    let path = jfk_fixture_root().join(JFK_FIXTURE.path);
    if !path.exists() {
        eprintln!("[skip] committed JFK fixture missing: {:?}", path);
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
