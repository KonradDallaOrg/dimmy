//! stt_race -- how fast does one meeting window actually transcribe, and on
//! which chip.
//!
//! Built for a machine with NO MICROPHONE: everything comes from a WAV file,
//! so it runs on a rented Mac, in CI, or over SSH. A shorter fixture is tiled
//! up to one full meeting window -- whisper pads any input to a 30 s encoder
//! window regardless, so measuring a 10 s clip would flatter it by exactly the
//! padding. The window length is read from the config (`meeting_chunk_secs`,
//! 30 s by default) so the bench measures the window this machine will
//! actually use.
//!
//! What the numbers mean:
//!
//! - **cold** includes loading the model. Paid once per process, and it is
//!   NOT what a meeting feels like.
//! - **warm** is the number that matters: with a 30 s window, transcription
//!   keeps up only while warm stays under 30 s, and the margin is the headroom
//!   for everything else on the machine.
//! - **RTF** is audio-seconds per wall-second. 1x is break-even; the ANE paths
//!   are reported upstream at 50x and above.
//!
//! Run (macOS, both engines):
//!     cargo run --release --bin stt_race \
//!       --features local-stt-metal,local-stt-coreml,local-stt-parakeet-fluid \
//!       -- tests/fixtures/jfk_16k_mono.wav
//!
//! With no argument it uses that fixture. Add `--iterations N` for more warm
//! passes; the median of those is what gets printed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Fallback window, used only if the config holds nothing sane.
const FALLBACK_WINDOW_SECS: f32 = 30.0;
const SAMPLE_RATE: usize = 16_000;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut wav: Option<PathBuf> = None;
    let mut iterations = 3usize;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--iterations" => {
                iterations = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--iterations needs a number");
            }
            other => wav = Some(PathBuf::from(other)),
        }
    }
    assert!(iterations >= 1, "need at least one warm pass");

    let wav = wav.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("jfk_16k_mono.wav")
    });
    assert!(wav.is_file(), "no such WAV: {}", wav.display());

    let window_secs = {
        let c = dimmy_lib::load_config_file().meeting_chunk_secs;
        if c.is_finite() && c > 0.0 {
            c
        } else {
            FALLBACK_WINDOW_SECS
        }
    };
    let clip = read_wav_16k_mono(&wav);
    assert!(
        !clip.is_empty(),
        "{} decoded to zero samples",
        wav.display()
    );
    let window = tile_to_window(&clip, window_secs);
    let audio_secs = window.len() as f32 / SAMPLE_RATE as f32;

    println!("fixture   {}", wav.display());
    println!(
        "window    {:.1}s ({} samples, tiled from {:.1}s)",
        audio_secs,
        window.len(),
        clip.len() as f32 / SAMPLE_RATE as f32
    );
    println!("warm runs {}", iterations);
    println!(
        "threads   {}",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );
    println!();

    let mut any = false;
    for engine in engines() {
        any |= run(&engine, &window, audio_secs, iterations);
    }
    if !any {
        eprintln!(
            "No engine was usable. Build with at least one of local-stt-metal, \
             local-stt-parakeet-fluid, local-stt-parakeet, and download its model first."
        );
        std::process::exit(2);
    }
}

/// One window in, text out. Boxed so whisper (which captures a model path)
/// and parakeet (which captures nothing) fit the same slot.
type Transcriber = Box<dyn Fn(&[f32]) -> Result<String, String>>;

struct Engine {
    label: String,
    /// Err when the build or the disk cannot provide it; the string says why.
    run: Result<Transcriber, String>,
}

fn engines() -> Vec<Engine> {
    let mut out = Vec::new();

    // whisper. The model is whatever the user configured; falling back to a
    // hardcoded name would measure a model they do not run.
    out.push(Engine {
        label: whisper_label(),
        run: whisper_runner(),
    });

    out.push(Engine {
        label: "parakeet".to_string(),
        run: if dimmy_lib::parakeet::active_bundle_present() {
            Ok(Box::new(|pcm: &[f32]| {
                dimmy_lib::parakeet::transcribe(pcm).map_err(|e| e.to_string())
            }))
        } else {
            Err("bundle not downloaded".to_string())
        },
    });

    out
}

fn whisper_model() -> Option<String> {
    let cfg = dimmy_lib::load_config_file();
    let name = cfg.local_model;
    if !name.is_empty() && dimmy_lib::local_stt::model_exists(&name) {
        return Some(name);
    }
    // Config points at nothing usable: take any model that IS on disk, so the
    // bench still runs on a fresh machine.
    dimmy_lib::local_stt::AVAILABLE_MODELS
        .iter()
        .find(|m| dimmy_lib::local_stt::model_exists(m.filename))
        .map(|m| m.filename.to_string())
}

fn whisper_label() -> String {
    match whisper_model() {
        Some(m) => {
            // Which chip runs the encoder is the whole question, so it goes in
            // the label rather than being left for the reader to assume.
            let encoder = if !cfg!(feature = "local-stt-coreml") {
                "gpu encoder"
            } else if dimmy_lib::coreml_encoder::bundle_present(&m) {
                "ANE encoder"
            } else {
                "gpu encoder, coreml bundle missing"
            };
            format!("whisper {m} ({encoder})")
        }
        None => "whisper".to_string(),
    }
}

fn whisper_runner() -> Result<Transcriber, String> {
    let model = whisper_model().ok_or("no whisper model on disk")?;
    let path = dimmy_lib::local_stt::model_path(&model);
    Ok(Box::new(move |pcm: &[f32]| {
        dimmy_lib::local_stt::transcribe_local(&path, pcm, "", "").map_err(|e| e.to_string())
    }))
}

/// Returns true when the engine actually produced a measurement.
fn run(engine: &Engine, window: &[f32], audio_secs: f32, iterations: usize) -> bool {
    let f = match &engine.run {
        Ok(f) => f,
        Err(why) => {
            println!("{:<48} skipped: {}", engine.label, why);
            return false;
        }
    };

    let t0 = Instant::now();
    let first = match f(window) {
        Ok(t) => t,
        Err(e) => {
            println!("{:<48} FAILED: {}", engine.label, e);
            return false;
        }
    };
    let cold = t0.elapsed();

    let mut warm: Vec<Duration> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t = Instant::now();
        // A failure here after a successful cold run is worth seeing, not
        // averaging away.
        if let Err(e) = f(window) {
            println!("{:<48} FAILED on a warm pass: {}", engine.label, e);
            return false;
        }
        warm.push(t.elapsed());
    }
    warm.sort();
    let median = warm[warm.len() / 2];
    let rtf = audio_secs / median.as_secs_f32();
    let verdict = if median.as_secs_f32() < audio_secs {
        format!("keeps up, {:.0}% of the window", 100.0 / rtf)
    } else {
        "FALLS BEHIND a live meeting".to_string()
    };

    println!("{}", engine.label);
    println!(
        "    cold {:>8.2}s   warm {:>8.2}s   RTF {:>6.1}x   {}",
        cold.as_secs_f32(),
        median.as_secs_f32(),
        rtf,
        verdict
    );
    // The text is the check that it transcribed rather than returned quickly:
    // an engine that emits nothing is infinitely fast.
    let preview: String = first.chars().take(90).collect();
    println!("    \"{}\"", preview.trim());
    println!();
    true
}

/// Repeat the clip until it fills one meeting window.
///
/// Tiling rather than zero-padding on purpose: silence is cheap for both
/// engines and whisper is known to hallucinate on it, so padding would measure
/// neither speed nor behaviour honestly.
fn tile_to_window(clip: &[f32], window_secs: f32) -> Vec<f32> {
    let target = (window_secs * SAMPLE_RATE as f32) as usize;
    let mut out = Vec::with_capacity(target);
    while out.len() < target {
        let take = (target - out.len()).min(clip.len());
        out.extend_from_slice(&clip[..take]);
    }
    assert_eq!(out.len(), target, "tiled window must be exactly one window");
    out
}

fn read_wav_16k_mono(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    let mono: Vec<f32> = if spec.channels > 1 {
        let ch = spec.channels as usize;
        raw.chunks(ch)
            .map(|c| c.iter().sum::<f32>() / ch as f32)
            .collect()
    } else {
        raw
    };
    assert!(
        spec.sample_rate == SAMPLE_RATE as u32,
        "fixture must already be {} Hz, got {} -- resampling here would measure the resampler",
        SAMPLE_RATE,
        spec.sample_rate
    );
    assert!(
        mono.iter().all(|s| s.is_finite()),
        "fixture contains NaN/Inf"
    );
    mono
}
