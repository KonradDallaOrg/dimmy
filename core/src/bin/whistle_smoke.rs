//! whistle_smoke - transcribe wav files with Whistle through the product path.
//!
//! Downloads the model and the engine into the real models directory when
//! they are missing (so the app sees them afterwards), then transcribes each
//! file the way dictation does. The engine is loaded once, so every file
//! after the first reports the warm cost.
//!
//! Usage:
//!   whistle_smoke [--lang it] [--keywords "Dimmy,Cactus Compute"] <audio.wav> [more.wav ...]

fn main() {
    let mut language = String::new();
    let mut keywords: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lang" => language = args.next().unwrap_or_default(),
            "--keywords" => {
                keywords = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|k| k.trim().to_string())
                    .collect()
            }
            _ => files.push(a),
        }
    }
    if files.is_empty() {
        eprintln!("usage: whistle_smoke [--lang it] [--keywords \"a,b\"] <audio.wav> [...]");
        std::process::exit(2);
    }
    if !dimmy_lib::whistle::engine_available() {
        eprintln!("Whistle has no engine for this platform");
        std::process::exit(1);
    }

    if !dimmy_lib::whistle::bundle_present() {
        println!(
            "downloading Whistle ({} MB) ...",
            dimmy_lib::whistle::size_mb()
        );
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = rt.block_on(dimmy_lib::whistle::download_bundle(|_, _| {})) {
            eprintln!("download failed: {e}");
            std::process::exit(1);
        }
    }

    let mut failures = 0;
    for path in &files {
        let Some(audio) = read_mono(path) else {
            println!("{path}: UNREADABLE");
            failures += 1;
            continue;
        };
        let secs = audio.samples.len() as f64 / f64::from(audio.sample_rate);
        let started = std::time::Instant::now();
        match dimmy_lib::transcribe::transcribe_audio_local_whistle(&audio, &language, &keywords) {
            Ok(text) => {
                let took = started.elapsed().as_secs_f64();
                println!(
                    "{path}: {secs:.1} s audio in {took:.2} s ({:.1}x realtime)\n  {text}",
                    secs / took
                );
            }
            Err(e) => {
                println!("{path}: FAILED {e}");
                failures += 1;
            }
        }
    }
    std::process::exit(if failures == 0 { 0 } else { 1 });
}

fn read_mono(path: &str) -> Option<dimmy_lib::audio::ProcessedAudio> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>().ok()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()
                .ok()?
        }
    };
    let channels = usize::from(spec.channels);
    let samples: Vec<f32> = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    (!samples.is_empty()).then_some(dimmy_lib::audio::ProcessedAudio {
        samples,
        sample_rate: spec.sample_rate,
    })
}
