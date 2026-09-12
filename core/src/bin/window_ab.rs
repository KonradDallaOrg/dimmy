//! window_ab — does a longer transcription window buy anything?
//!
//! The meeting path chops audio into fixed windows and transcribes each one
//! alone. The window size was chosen for whisper, which pads ANY input to a
//! full 30 s encoder frame, so a 15 s window costs exactly what a 30 s one
//! does — half the work for nothing. The other two engines have no such
//! padding, so the trade is real for them and unmeasured.
//!
//! Quality is the other half. A window is all the context the model gets:
//! measured 2026-09-09 on Qwen3-ASR, "tag NFC" came out right on a 30 s
//! window and as "tag N S C" on a 15 s one, from the same audio. This runs
//! the same file at several window sizes through the same engine and prints
//! both the cost and the text, so the trade can be read instead of guessed.
//!
//! Usage:
//!   window_ab parakeet          <audio.wav|ogg> [secs...]
//!   window_ab whisper:<model>   <audio.wav|ogg> [secs...]
//!   window_ab qwen:<model.gguf> <audio.wav|ogg> [secs...]
//!
//! Default window set: 3 15 30 0   (0 = the whole file in one call)

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: window_ab <engine> <audio> [window_secs...]");
        eprintln!("  engine: parakeet | whisper:<model.bin> | qwen:<model.gguf>");
        std::process::exit(2);
    }
    let engine = args[1].clone();
    let path = args[2].clone();

    // `--max-secs N` keeps only the first N seconds. A real meeting is the
    // honest input, but a 32-minute one costs ~500 MB just to decode and the
    // run gets killed on a loaded laptop before a single window reports --
    // which is worse than a shorter sample, because it reports nothing at all.
    let mut max_secs: Option<f32> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args[3..].iter();
    while let Some(a) = it.next() {
        if a == "--max-secs" {
            max_secs = it.next().and_then(|v| v.parse().ok());
        } else {
            rest.push(a.clone());
        }
    }
    let windows: Vec<f32> = if rest.is_empty() {
        vec![3.0, 15.0, 30.0, 0.0]
    } else {
        rest.iter().filter_map(|s| s.parse().ok()).collect()
    };

    let Some(mut pcm) = read_16k_mono(&path) else {
        eprintln!("cannot read {path}");
        std::process::exit(1);
    };
    if let Some(m) = max_secs {
        assert!(m > 0.0, "--max-secs must be positive");
        let keep = (m * 16_000.0) as usize;
        if keep < pcm.len() {
            pcm.truncate(keep);
            // The decoder's slack is what got us killed; hand it back before
            // the model is loaded rather than holding it for the whole run.
            pcm.shrink_to_fit();
        }
    }
    let pcm = pcm;
    let total_secs = pcm.len() as f32 / 16_000.0;
    println!("{}  {:.1}s  engine={}\n", short(&path), total_secs, engine);

    for w in windows {
        let chunk = if w <= 0.0 {
            pcm.len()
        } else {
            (w * 16_000.0) as usize
        };
        let label = if w <= 0.0 {
            "intero".to_string()
        } else {
            format!("{w:.0}s")
        };

        let t0 = std::time::Instant::now();
        let mut out = String::new();
        let mut failed = 0usize;
        let mut n = 0usize;
        for slice in pcm.chunks(chunk) {
            n += 1;
            match transcribe(&engine, slice) {
                Ok(t) if !t.trim().is_empty() => {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(t.trim());
                }
                Ok(_) => {}
                Err(e) => {
                    failed += 1;
                    if failed == 1 {
                        eprintln!("  [{label}] first failure: {e}");
                    }
                }
            }
        }
        let dt = t0.elapsed().as_secs_f32();
        println!(
            "finestra {:<7} {:>3} chiamate  {:>7.2}s  ({:.1}x realtime)  {} parole{}",
            label,
            n,
            dt,
            total_secs / dt.max(0.001),
            out.split_whitespace().count(),
            if failed > 0 {
                format!("  [{failed} fallite]")
            } else {
                String::new()
            }
        );
        println!("   {}\n", out);
    }
}

fn transcribe(engine: &str, pcm: &[f32]) -> Result<String, String> {
    if engine == "parakeet" {
        return dimmy_lib::parakeet::transcribe(pcm).map_err(|e| format!("{e:?}"));
    }
    if let Some(model) = engine.strip_prefix("qwen:") {
        return dimmy_lib::qwen_asr::transcribe(pcm, model, "")
            .map(|t| t.text)
            .map_err(|e| format!("{e:?}"));
    }
    if let Some(model) = engine.strip_prefix("whisper:") {
        // The language MUST be forced. Local whisper auto-detect returns ZERO
        // segments (measured 2026-07-29, large-v3 and turbo alike), so an empty
        // language here reads as "the engine produced nothing" and silently
        // scores whisper at zero words. Override with DIMMY_AB_LANG.
        let lang = std::env::var("DIMMY_AB_LANG").unwrap_or_else(|_| "it".to_string());
        return dimmy_lib::local_stt::transcribe_local(std::path::Path::new(model), pcm, &lang, "")
            .map_err(|e| format!("{e:?}"));
    }
    Err(format!("unknown engine '{engine}'"))
}

fn short(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn read_16k_mono(path: &str) -> Option<Vec<f32>> {
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

    if rate == 16_000 {
        return Some(mono);
    }
    let ratio = f64::from(rate) / 16_000.0;
    let out_len = (mono.len() as f64 / ratio) as usize;
    Some(
        (0..out_len)
            .map(|i| {
                let src = i as f64 * ratio;
                let a = src as usize;
                let b = (a + 1).min(mono.len() - 1);
                let f = (src - a as f64) as f32;
                mono[a] * (1.0 - f) + mono[b] * f
            })
            .collect(),
    )
}
