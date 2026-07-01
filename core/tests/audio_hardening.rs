//! Audio-pipeline hardening — regression tests for the dictation path.
//!
//! Blinds the two silent-regression classes fixed 2026-07-01 (see
//! `docs/dev/known-bugs.md`):
//!   - BUG A — capture loss (Mix AEC ring dropped ~60 % of dictation audio).
//!   - BUG B — preprocessing degrading a quiet input below raw ("Ah!").
//!
//! These drive the FULL production pipeline via `dimmy_inject_pcm_for_test`
//! → `dimmy_stop_recording`, so they exercise the exact route-aware
//! preprocess selection (`preprocess::preprocess_route`) + the local
//! make-it-worse guard (`preprocess::process_buffer_guarded`) that ship.
//!
//! NOTE on the capture-ratio guard (§3): it reads a recording-start `Instant`
//! set only by `dimmy_start_recording`. The injection harness writes the
//! buffer directly and never starts a real capture, so the guard is inert
//! here by design — its arithmetic is unit-tested in
//! `telemetry::events::tests` / `telemetry::sanitize`. The real capture-path
//! drop (a cpal ring overflow) is not reachable from `test-ffi`; the
//! `dictation.capture_ratio` telemetry is its production detector.
//!
//! Run: `cargo test --release --test audio_hardening --features local-stt,test-ffi -- --test-threads=1`

#![cfg(all(feature = "test-ffi", feature = "local-stt"))]

use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use serial_test::serial;

use dimmy_lib::ffi::{
    dimmy_init, dimmy_inject_pcm_for_test, dimmy_set_config_json, dimmy_stop_recording,
    dimmy_transcribe_file,
};

// ── Fixtures (mirrors ffi_e2e.rs; each test file is self-contained) ─────

const JFK_WAV_URL: &str = "https://github.com/ggml-org/whisper.cpp/raw/master/samples/jfk.wav";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en-q8_0.bin";
const MODEL_FILENAME: &str = "ggml-tiny.en-q8_0.bin";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-fixtures")
}

fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        return Ok(());
    }
    eprintln!("[fixture] downloading {} → {}", url, dest.display());
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    let resp = ureq::get(url)
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("ureq call: {}", e))?;
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn jfk_wav() -> PathBuf {
    let p = fixture_dir().join("jfk.wav");
    download_to(JFK_WAV_URL, &p).expect("download jfk.wav");
    p
}

fn ensure_tiny_model() {
    let src = fixture_dir().join(MODEL_FILENAME);
    download_to(MODEL_URL, &src).expect("download tiny model");
    let models_dir = dimmy_lib::local_stt::model_directory();
    std::fs::create_dir_all(&models_dir).ok();
    let dest = models_dir.join(MODEL_FILENAME);
    if !dest.exists() {
        std::fs::copy(&src, &dest).expect("copy model into models dir");
    }
}

fn load_wav_f32(path: &Path) -> (Vec<f32>, u32) {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("read f32 sample"))
            .collect(),
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            let max = (1i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.expect("read int sample") as f32 / max)
                .collect()
        }
    };
    let mono: Vec<f32> = if spec.channels > 1 {
        raw.chunks(spec.channels as usize).map(|c| c[0]).collect()
    } else {
        raw
    };
    (mono, spec.sample_rate)
}

/// Write f32 mono samples to a 16-bit PCM WAV so `dimmy_transcribe_file`
/// can decode it via its hound fast path.
fn write_wav_16bit_mono(samples: &[f32], sample_rate: u32, dest: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(dest, spec).expect("create wav");
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        w.write_sample(v).expect("write sample");
    }
    w.finalize().expect("finalize wav");
}

// ── Harness ─────────────────────────────────────────────────────────────

static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
        let rc = dimmy_init();
        assert!(
            rc == 0 || rc == 1,
            "dimmy_init must succeed (0) or report already-inited (1), got {}",
            rc
        );
    });
}

fn set_config(json: &str) {
    let c = CString::new(json).expect("no nul in json");
    let rc = unsafe { dimmy_set_config_json(c.as_ptr()) };
    assert_eq!(rc, 0, "set_config_json failed: {} (json={})", rc, json);
}

fn transcribe_pcm(samples: &[f32], sample_rate: u32) -> String {
    let rc =
        unsafe { dimmy_inject_pcm_for_test(samples.as_ptr(), samples.len() as c_int, sample_rate) };
    assert_eq!(rc, 0, "inject_pcm failed: {}", rc);
    let mut buf: Vec<u8> = vec![0; 8192];
    let n = dimmy_stop_recording(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int);
    if n < 0 {
        return String::new();
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
    cstr.to_string_lossy().into_owned()
}

// ── Tests ────────────────────────────────────────────────────────────────

/// LOCAL route with preprocessing ON drives the full VAD+AGC pipeline
/// through the make-it-worse guard. On real speech the guard must NOT trip,
/// and the transcript survives. Locks in that the guard doesn't break normal
/// local dictation (the behaviour validated live 2026-07-01).
#[test]
#[serial]
fn local_full_route_with_preprocessing_transcribes_jfk() {
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": true, // Full route + process_buffer_guarded
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    let (samples, sr) = load_wav_f32(&jfk_wav());
    let transcript = transcribe_pcm(&samples, sr);

    eprintln!("[test] local Full-route transcript: {:?}", transcript);
    assert!(
        transcript.to_lowercase().contains("ask not"),
        "Full-route local STT lost the speech, got: {:?}",
        transcript
    );
}

/// CLOUD route with preprocessing ON reaches the (mocked) provider and
/// delivers a substantial audio body — it does NOT short-circuit to an empty
/// upload. Guards the regression where the cloud preprocess path returns
/// nothing (the provider then transcribes silence). The precise
/// highpass-vs-full *selection* is pinned deterministically by the
/// `preprocess_route` unit test; a synthetic clip can't reliably reproduce
/// the VAD's RNN misclassification, so we don't assert on that here.
#[test]
#[serial]
fn cloud_route_delivers_nonempty_audio_body() {
    ensure_init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async { wiremock::MockServer::start().await });
    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/transcriptions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "text": "hello world from the cloud"
                })),
            )
            .mount(&server)
            .await;
    });

    set_config(
        &serde_json::json!({
            "stt_mode": "cloud",
            "api_url": format!("{}/audio/transcriptions", server.uri()),
            "api_model": "whisper-1",
            "api_key": "test-cloud-key",
            "language": "en",
            "preprocessing_enabled": true, // HighpassOnly route
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    // 1 s of a ~150 Hz tone — enough audio to produce a real multipart body.
    let samples: Vec<f32> = (0..48_000)
        .map(|i| (i as f32 * 0.02).sin() * 0.03)
        .collect();
    let transcript = transcribe_pcm(&samples, 48_000);

    assert!(
        transcript.to_lowercase().contains("hello world"),
        "cloud route should return the mocked transcript, got: {:?}",
        transcript
    );

    // The real invariant: the provider actually received the audio. A ~1 s
    // clip is tens of KB of multipart WAV (observed ~32 KB); the 8 KB floor
    // is a ~4x safety margin. A short-circuit-to-empty regression would send
    // a few hundred bytes at most.
    let max_body = rt
        .block_on(async { server.received_requests().await })
        .expect("wiremock recorded requests")
        .iter()
        .map(|r| r.body.len())
        .max()
        .unwrap_or(0);
    eprintln!("[test] cloud request max body = {} bytes", max_body);
    assert!(
        max_body > 8_000,
        "cloud provider received a collapsed body ({} bytes) — highpass route \
         should preserve the ~1 s clip; a Full-route trim would land here",
        max_body
    );
}

/// BUG B regression, local variant: attenuated real speech (simulating a
/// low `input_gain` mic) through the LOCAL Full route + guard still
/// transcribes. The guard's job is to guarantee the pipeline never ships
/// audio worse than raw; the outcome must be preserved.
#[test]
#[serial]
fn quiet_local_speech_still_transcribes() {
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": true,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    let (samples, sr) = load_wav_f32(&jfk_wav());
    // Attenuate to ~30% — a quiet mic / input_gain 0.3 world.
    let quiet: Vec<f32> = samples.iter().map(|s| s * 0.3).collect();
    let transcript = transcribe_pcm(&quiet, sr);

    eprintln!("[test] quiet local transcript: {:?}", transcript);
    assert!(
        transcript.to_lowercase().contains("ask not"),
        "quiet speech collapsed through the local pipeline, got: {:?}",
        transcript
    );
}

/// File-load / e2e: a synthesized MEDIUM file (jfk repeated 4×, ~44 s) driven
/// through `dimmy_transcribe_file` (highpass-only path) must still transcribe
/// — catches STT + file-load preprocess regressions on longer inputs without
/// committing a large fixture.
#[test]
#[serial]
fn transcribe_file_medium_synthesized_jfk_transcribes() {
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": true,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    let (samples, sr) = load_wav_f32(&jfk_wav());
    let mut medium = Vec::with_capacity(samples.len() * 4);
    for _ in 0..4 {
        medium.extend_from_slice(&samples);
    }
    let dest = fixture_dir().join("jfk_medium_x4.wav");
    write_wav_16bit_mono(&medium, sr, &dest);

    let path = CString::new(dest.to_string_lossy().to_string()).unwrap();
    let mut buf: Vec<u8> = vec![0; 16_384];
    let n = unsafe {
        dimmy_transcribe_file(
            path.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert!(n >= 0, "dimmy_transcribe_file returned error rc {}", n);
    let transcript = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) }
        .to_string_lossy()
        .into_owned();

    eprintln!("[test] medium file transcript: {:?}", transcript);
    assert!(
        transcript.to_lowercase().contains("ask not"),
        "medium synthesized file lost the speech, got: {:?}",
        transcript
    );
}
