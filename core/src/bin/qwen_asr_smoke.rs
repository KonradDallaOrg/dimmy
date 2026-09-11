//! qwen_asr_smoke - transcribe wav files with Qwen3-ASR through OUR build.
//!
//! The point is not that Qwen3-ASR works: that was already shown with upstream
//! llama.cpp binaries. The point is that it works through the llama.cpp WE
//! compile and ship, with `local-stt-qwen` on and nothing else changed -- no
//! version bump, no second engine, no Python.
//!
//! It goes through the PRODUCT path, not a private load: catalog lookup, the
//! bundle-presence check, the resident cache and the VRAM handover are the same
//! ones dictation and the meeting use. The model is loaded once, so every file
//! after the first reports the warm cost -- the only one that matters when the
//! model stays resident for a session.
//!
//! Usage:
//!   qwen_asr_smoke <model-file-from-catalog> <audio.wav> [more.wav ...]
//!   qwen_asr_smoke --list

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() == 2 && args[1] == "--list" {
        for m in dimmy_lib::qwen_asr::AVAILABLE_MODELS {
            println!(
                "{:<28} {:>5} MB  {:<40} [{}]",
                m.model_file,
                m.size_mb,
                m.description,
                if dimmy_lib::qwen_asr::bundle_present(m.model_file) {
                    "on disk"
                } else {
                    "not downloaded"
                }
            );
        }
        return;
    }

    if args.len() < 3 {
        eprintln!("usage: qwen_asr_smoke <model-file-from-catalog> <audio.wav> [...]");
        eprintln!("       qwen_asr_smoke --list");
        std::process::exit(2);
    }

    let model = args[1].clone();
    if dimmy_lib::qwen_asr::find(&model).is_none() {
        eprintln!("'{model}' is not in the catalog - run --list");
        std::process::exit(2);
    }
    if !dimmy_lib::qwen_asr::bundle_present(&model) {
        eprintln!("'{model}' is not downloaded (both halves are needed)");
        std::process::exit(1);
    }

    let mut failures = 0;
    for path in &args[2..] {
        let Some((pcm, secs)) = read_16k_mono(path) else {
            println!("{:<22} UNREADABLE", short(path));
            failures += 1;
            continue;
        };
        let t = std::time::Instant::now();
        match dimmy_lib::qwen_asr::transcribe(&pcm, &model, "") {
            Ok(tr) => {
                let dt = t.elapsed().as_secs_f32();
                println!(
                    "{:<22} {:>5.1}s audio -> {:>5.2}s  ({:.1}x realtime)  lang={}",
                    short(path),
                    secs,
                    dt,
                    secs / dt,
                    tr.language.as_deref().unwrap_or("?")
                );
                println!("    {}\n", tr.text);
            }
            Err(e) => {
                println!("{:<22} FAILED {:?}", short(path), e);
                failures += 1;
            }
        }
    }
    if failures > 0 {
        eprintln!("{} file(s) failed", failures);
        std::process::exit(1);
    }
}

fn short(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Decode to the 16 kHz mono the model wants, the same way the product does.
fn read_16k_mono(path: &str) -> Option<(Vec<f32>, f32)> {
    let (mono, rate) = if path.to_ascii_lowercase().ends_with(".wav") {
        let mut reader = hound::WavReader::open(path).ok()?;
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => reader
                .samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / (1i64 << (spec.bits_per_sample - 1)) as f32)
                .collect(),
            hound::SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect(),
        };
        let mono = if spec.channels > 1 {
            samples
                .chunks(spec.channels as usize)
                .map(|c| c.iter().sum::<f32>() / c.len() as f32)
                .collect()
        } else {
            samples
        };
        (mono, spec.sample_rate)
    } else {
        dimmy_lib::ffi::decode_via_symphonia(path).ok()?
    };

    let pcm = if rate == 16_000 {
        mono
    } else {
        let ratio = f64::from(rate) / 16_000.0;
        let out_len = (mono.len() as f64 / ratio) as usize;
        (0..out_len)
            .map(|i| {
                let src = i as f64 * ratio;
                let a = src as usize;
                let b = (a + 1).min(mono.len() - 1);
                let f = (src - a as f64) as f32;
                mono[a] * (1.0 - f) + mono[b] * f
            })
            .collect()
    };
    if pcm.is_empty() {
        return None;
    }
    let secs = pcm.len() as f32 / 16_000.0;
    Some((pcm, secs))
}
