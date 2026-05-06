//! Parakeet via FluidInference / Apple Neural Engine — smoke test.
//!
//! Reads a WAV, calls `dimmy_lib::parakeet_fluid::transcribe` (Apple
//! Silicon arm64 only), prints the text + total wall time. First run
//! pays the ~3 GB CoreML bundle download into `~/.cache/fluidaudio/`
//! plus the ANE compile, then subsequent calls reuse the cache.
//!
//! Build:
//!     cd core
//!     cargo build --release --bin parakeet_fluid_smoke \
//!         --features local-stt-parakeet-fluid
//!
//! Run:
//!     target/release/parakeet_fluid_smoke <path-to-wav>
//!
//! Expected to succeed on macOS arm64 only — the binary will fail to
//! build on Win/Linux/Intel-Mac because the underlying crate is target-
//! gated to `cfg(all(target_os = "macos", target_arch = "aarch64"))`.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = match args.get(1) {
        Some(p) => p.clone(),
        None => {
            eprintln!("usage: parakeet_fluid_smoke <path-to-wav>");
            std::process::exit(2);
        }
    };

    eprintln!(
        "fluid cache dir: {:?}",
        dimmy_lib::parakeet_fluid::cache_dir()
    );
    eprintln!(
        "fluid bundle present (heuristic): {}",
        dimmy_lib::parakeet_fluid::bundle_present()
    );

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

    eprintln!("transcribing (cold path: model load + ANE compile + inference)...");
    let t0 = Instant::now();
    let res = dimmy_lib::parakeet_fluid::transcribe(&pcm_16k);
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
