//! Does the bundled ONNX Runtime accept the session options `gtcrn.rs` sets?
//!
//! Worth its own test because the failure is INVISIBLE: `Session::builder()`
//! returns the rejection as an `Err`, `GtcrnDenoiser::load` propagates it, and
//! `maybe_denoise_16k` then passes the audio through by design. A rejected
//! option therefore ships a build with no noise suppression at all and no
//! symptom other than worse transcripts. The module comment already records
//! that this build rejects `with_optimization_level`, so the risk is real and
//! not hypothetical.
//!
//! Only `commit_from_file` actually validates the options, and that needs the
//! real model, so this is gated on the installed asset being present and skips
//! everywhere else (CI, fresh clones, non-Windows).
#![cfg(feature = "denoise-gtcrn")]

#[test]
fn session_options_are_accepted_by_this_ort_build() {
    let installed = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
        .join("Dimmy")
        .join("current")
        .join("gtcrn_simple.onnx");
    if !installed.is_file() {
        eprintln!("skipped: no installed model at {}", installed.display());
        return;
    }
    let d = dimmy_lib::gtcrn::GtcrnDenoiser::load(&installed);
    assert!(
        d.is_ok(),
        "session builder rejected our options, so the denoiser would silently \
         pass audio through: {:?}",
        d.err()
    );
    let mut d = d.expect("checked");
    let input: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
    let out = d.process(&input).expect("process must succeed");
    assert_eq!(out.len(), input.len(), "denoiser must preserve length");
    assert!(
        out.iter().all(|s| s.is_finite()),
        "denoiser emitted NaN/Inf"
    );
}

/// Does feeding the denoiser a slice at a time produce the same audio as
/// feeding it the whole recording?
///
/// This is the licence for running the denoiser DURING capture instead of
/// after it. If the answer were "almost", the streaming path would be a
/// different denoiser wearing the same name — the audio whisper sees would
/// depend on how the capture thread happened to slice the buffer, and none of
/// the tuning done against the batch path would carry over. So the bar is
/// bit-for-bit, not "close enough": the framing state lives in the struct
/// precisely so chunk size cannot be observable.
#[test]
fn streaming_matches_batch_exactly() {
    let installed = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
        .join("Dimmy")
        .join("current")
        .join("gtcrn_simple.onnx");
    if !installed.is_file() {
        eprintln!("skipped: no installed model at {}", installed.display());
        return;
    }

    let rate = dimmy_lib::gtcrn::REQUIRED_RATE as usize;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let input: Vec<f32> = (0..rate * 10)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let noise = (seed >> 40) as f32 / 12_000.0 - 0.1;
            let t = i as f32 / rate as f32;
            let voice =
                (t * 2.0 * std::f32::consts::PI * 190.0).sin() * 0.3 * (t * 3.0).sin().abs();
            (voice + noise * 0.15).clamp(-1.0, 1.0)
        })
        .collect();

    let mut d = dimmy_lib::gtcrn::GtcrnDenoiser::load(&installed).expect("load");
    let batch = d.process(&input).expect("batch pass");

    // Chunk sizes a capture thread might realistically hand over, plus two
    // adversarial ones: 1 sample, and a size coprime with the 256-sample hop.
    for chunk in [1usize, 333, 480, 1600, 4096, 16_000] {
        let mut d = dimmy_lib::gtcrn::GtcrnDenoiser::load(&installed).expect("load");
        d.reset();
        let mut streamed: Vec<f32> = Vec::with_capacity(input.len());
        for slice in input.chunks(chunk) {
            streamed.extend_from_slice(&d.push(slice).expect("push"));
        }
        streamed.extend_from_slice(&d.flush().expect("flush"));

        assert_eq!(
            streamed.len(),
            batch.len(),
            "chunk {chunk}: streaming produced a different number of samples"
        );
        let drift = streamed.iter().zip(&batch).position(|(a, b)| a != b);
        assert!(
            drift.is_none(),
            "chunk {chunk}: streaming diverged from the batch pass at sample {:?}",
            drift
        );
    }
}
