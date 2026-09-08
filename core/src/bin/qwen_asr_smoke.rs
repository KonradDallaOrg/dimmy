//! qwen_asr_smoke - transcribe wav files with Qwen3-ASR through OUR build.
//!
//! The point is not that Qwen3-ASR works: that was already shown with upstream
//! llama.cpp binaries. The point is that it works through the llama.cpp WE
//! compile and ship, with `local-stt-qwen` on and nothing else changed -- no
//! version bump, no second engine, no Python.
//!
//! The model is loaded ONCE and every file goes through the same instance, so
//! the printed per-file time is the warm cost, which is the only one that
//! matters: in the product the model is resident for the whole session.
//!
//! Usage:
//!   qwen_asr_smoke <model.gguf> <mmproj.gguf> <audio.wav> [more.wav ...]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: qwen_asr_smoke <model.gguf> <mmproj.gguf> <audio.wav> [...]");
        std::process::exit(2);
    }

    let t0 = std::time::Instant::now();
    let asr = match dimmy_lib::qwen_asr::QwenAsr::load(
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("load failed: {:?}", e);
            std::process::exit(1);
        }
    };
    println!("model loaded in {:.1}s\n", t0.elapsed().as_secs_f32());

    let mut failures = 0;
    for path in &args[3..] {
        let Some((pcm, secs)) = read_16k_mono(path) else {
            println!("{:<22} UNREADABLE", short(path));
            failures += 1;
            continue;
        };
        let t = std::time::Instant::now();
        match asr.transcribe(&pcm) {
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
