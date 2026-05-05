//! Parakeet TDT v3 FP32 smoke test — runs `dimmy_lib::parakeet::transcribe`
//! against a local WAV file and prints the result + timings.
//!
//! Build:
//!     cd core
//!     cargo build --release --bin parakeet_smoke --features local-stt-parakeet
//!
//! Run:
//!     ORT_DYLIB_PATH=C:\path\to\onnxruntime.dll \
//!     target/release/parakeet_smoke <path-to-wav>
//!
//! The bundle must be present at `<config-dir>/parakeet-fp32/`. Get
//! one with `python3 -m onnx_asr` from the
//! `istupakov/parakeet-tdt-0.6b-v3-onnx` HuggingFace repo or via
//! `dimmy_lib::parakeet::download_bundle`.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = match args.get(1) {
        Some(p) => p.clone(),
        None => {
            eprintln!("usage: parakeet_smoke <path-to-wav>");
            std::process::exit(2);
        }
    };

    eprintln!("bundle dir: {:?}", dimmy_lib::parakeet::bundle_dir());
    eprintln!("bundle present: {}", dimmy_lib::parakeet::bundle_present());

    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!(
            "[FAIL] bundle not at {:?} — copy it from your WSL .scratch dir or run download_bundle",
            dimmy_lib::parakeet::bundle_dir()
        );
        std::process::exit(1);
    }

    eprintln!("loading WAV from {}", path);
    let mut r = match hound::WavReader::open(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[FAIL] cannot open WAV {}: {}", path, e);
            std::process::exit(1);
        }
    };
    let spec = r.spec();
    eprintln!(
        "spec: sr={} ch={} bits={} fmt={:?}",
        spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
    );
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
    let pcm_16k: Vec<f32> = if spec.sample_rate == 16_000 {
        mono
    } else {
        dimmy_lib::preprocess::downsample_to_16k(&mono, spec.sample_rate)
    };
    eprintln!(
        "pcm: {} samples ({:.2}s @ 16kHz)",
        pcm_16k.len(),
        pcm_16k.len() as f32 / 16_000.0
    );

    eprintln!("transcribing (cold path: model load + inference)...");
    let t0 = Instant::now();
    let res = dimmy_lib::parakeet::transcribe(&pcm_16k);
    let elapsed = t0.elapsed();

    match res {
        Ok(text) => {
            println!("--- TEXT ---");
            println!("{}", text);
            println!("--- /TEXT ---");
            eprintln!(
                "[OK] {}ms total, audio {:.2}s, rt={:.1}x",
                elapsed.as_millis(),
                pcm_16k.len() as f32 / 16_000.0,
                (pcm_16k.len() as f32 / 16_000.0) / elapsed.as_secs_f32()
            );
        }
        Err(e) => {
            eprintln!("[FAIL] {}", e);
            std::process::exit(1);
        }
    }
}
