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
