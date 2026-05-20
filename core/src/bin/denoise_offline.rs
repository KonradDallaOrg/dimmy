//! Offline noise-suppression compare tool.
//!
//! Usage:
//!   denoise_offline [--backend nnnoise|dfn3] <input.wav> [<output.wav>]
//!
//! Reads a mono WAV (or downmixes stereo via average — same recipe
//! the runtime secondary callback uses), resamples to 48 kHz if
//! needed (linear interpolator from audio.rs's own LinearResampler
//! design — kept inline here), runs the chosen backend, and writes
//! the result back as a 48 kHz mono int16 WAV.
//!
//! Backends:
//!   - `nnnoise` (default if no flag): RNNoise port via the
//!     `nnnoiseless` crate, model embedded ~85 KB, ~1 ms/frame.
//!     Frame size 480 samples @ 48 kHz.
//!   - `dfn3`: DeepFilterNet3 via the upstream `deep_filter`
//!     crate (path dep — see Cargo.toml comment). Requires the
//!     `local-dfn` cargo feature at compile time. SOTA quality,
//!     ~5-10× RTF on a typical laptop CPU.
//!
//! Output filename defaults to `<input>_<backend>.wav` (e.g.
//! `audio_mic_nnnoise.wav` or `audio_mic_dfn3.wav`) so A/B side-by-
//! side files don't overwrite each other.

use dimmy_lib::dfn::DfnProcessor;
use std::path::{Path, PathBuf};

#[derive(Copy, Clone, Debug)]
enum Backend {
    Nnnoise,
    #[cfg(feature = "local-dfn")]
    Dfn3,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Backend::Nnnoise => "nnnoise",
            #[cfg(feature = "local-dfn")]
            Backend::Dfn3 => "dfn3",
        }
    }
}

const TARGET_RATE: u32 = 48_000;
const FRAME: usize = DfnProcessor::FRAME_SIZE;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw_args: Vec<String> = std::env::args().skip(1).collect();
    let mut backend = Backend::Nnnoise;
    // Hand-rolled flag parser — we only need `--backend X` here.
    if let Some(pos) = raw_args.iter().position(|a| a == "--backend") {
        if pos + 1 >= raw_args.len() {
            eprintln!("error: --backend requires a value (nnnoise|dfn3)");
            std::process::exit(2);
        }
        backend = match raw_args[pos + 1].as_str() {
            "nnnoise" => Backend::Nnnoise,
            #[cfg(feature = "local-dfn")]
            "dfn3" => Backend::Dfn3,
            #[cfg(not(feature = "local-dfn"))]
            "dfn3" => {
                eprintln!("error: dfn3 backend requires --features local-dfn at build time");
                std::process::exit(2);
            }
            other => {
                eprintln!("error: unknown backend '{}' (expected nnnoise|dfn3)", other);
                std::process::exit(2);
            }
        };
        raw_args.drain(pos..=pos + 1);
    }
    if raw_args.is_empty() {
        eprintln!("Usage: denoise_offline [--backend nnnoise|dfn3] <input.wav> [<output.wav>]");
        std::process::exit(2);
    }
    let input_path = PathBuf::from(&raw_args[0]);
    let output_path = if raw_args.len() >= 2 {
        PathBuf::from(&raw_args[1])
    } else {
        default_output_path(&input_path, backend)
    };
    println!(
        "denoise_offline: backend={}\n  in : {}\n  out: {}",
        backend.name(),
        input_path.display(),
        output_path.display()
    );

    // --- Load input WAV --------------------------------------------------
    let mut reader = hound::WavReader::open(&input_path)?;
    let spec = reader.spec();
    println!(
        "  input spec: sr={} channels={} bits={} format={:?}",
        spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
    );

    // Read all samples as f32 in [-1.0, 1.0]. Handle both int and float
    // input + downmix stereo to mono via AVERAGE (matches audio.rs).
    let samples_f32: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let denom = (1i64 << (spec.bits_per_sample - 1)) as f32;
            let raw: Vec<i32> = reader.samples::<i32>().filter_map(|s| s.ok()).collect();
            if spec.channels as usize > 1 {
                let chf = spec.channels as f32;
                raw.chunks(spec.channels as usize)
                    .map(|c| c.iter().map(|&s| s as f32 / denom).sum::<f32>() / chf)
                    .collect()
            } else {
                raw.into_iter().map(|s| s as f32 / denom).collect()
            }
        }
        hound::SampleFormat::Float => {
            let raw: Vec<f32> = reader.samples::<f32>().filter_map(|s| s.ok()).collect();
            if spec.channels as usize > 1 {
                let chf = spec.channels as f32;
                raw.chunks(spec.channels as usize)
                    .map(|c| c.iter().sum::<f32>() / chf)
                    .collect()
            } else {
                raw
            }
        }
    };
    println!(
        "  loaded: {} mono samples ({:.2} s @ {} Hz)",
        samples_f32.len(),
        samples_f32.len() as f32 / spec.sample_rate as f32,
        spec.sample_rate
    );

    // --- Resample to 48 kHz if needed -----------------------------------
    let work = if spec.sample_rate == TARGET_RATE {
        samples_f32
    } else {
        println!(
            "  resampling {} -> {} Hz (linear interpolation)",
            spec.sample_rate, TARGET_RATE
        );
        resample_linear(&samples_f32, spec.sample_rate, TARGET_RATE)
    };
    println!(
        "  resampled: {} samples ({:.2} s @ 48 kHz)",
        work.len(),
        work.len() as f32 / TARGET_RATE as f32
    );

    // --- Dispatch to the selected backend -------------------------------
    let out: Vec<f32> = match backend {
        Backend::Nnnoise => process_nnnoise(&work)?,
        #[cfg(feature = "local-dfn")]
        Backend::Dfn3 => process_dfn3(&work)?,
    };
    println!(
        "  processed: {} samples → {:.2} s",
        out.len(),
        out.len() as f32 / TARGET_RATE as f32
    );

    // --- Write output WAV -----------------------------------------------
    let out_spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&output_path, out_spec)?;
    for &s in &out {
        let clamped = s.clamp(-1.0, 1.0);
        writer.write_sample((clamped * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    println!("  wrote: {}", output_path.display());
    Ok(())
}

fn default_output_path(input: &Path, backend: Backend) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "denoised".to_string());
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{}_{}.wav", stem, backend.name()))
}

fn process_nnnoise(work: &[f32]) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    dimmy_lib::dfn::DENOISE_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut proc = DfnProcessor::try_init().ok_or("DfnProcessor::try_init returned None")?;
    let mut out = Vec::with_capacity(work.len());
    let mut frame_in = [0.0f32; FRAME];
    let mut frame_out = [0.0f32; FRAME];
    let n_frames = work.len() / FRAME;
    let tail = work.len() % FRAME;
    for i in 0..n_frames {
        let start = i * FRAME;
        frame_in.copy_from_slice(&work[start..start + FRAME]);
        proc.process_frame(&frame_in, &mut frame_out);
        out.extend_from_slice(&frame_out);
    }
    if tail > 0 {
        let mut padded = [0.0f32; FRAME];
        padded[..tail].copy_from_slice(&work[n_frames * FRAME..]);
        proc.process_frame(&padded, &mut frame_out);
        out.extend_from_slice(&frame_out[..tail]);
    }
    Ok(out)
}

#[cfg(feature = "local-dfn")]
fn process_dfn3(work: &[f32]) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    use dimmy_lib::dfn3::Dfn3Processor;
    let mut proc = Dfn3Processor::new()?;
    let model_sr = proc.sample_rate();
    if model_sr != TARGET_RATE {
        return Err(format!(
            "DFN3 model sample rate mismatch: model={} expected={}",
            model_sr, TARGET_RATE
        )
        .into());
    }
    let hop = proc.hop_size();
    let trimmed_len = (work.len() / hop) * hop;
    let out = proc.process_mono(&work[..trimmed_len])?;
    Ok(out)
}

/// Streaming linear resampler — single-shot variant of audio.rs's
/// `LinearResampler` that operates on an in-memory buffer in one go.
/// Adequate for offline file conversion; no anti-aliasing filter, so
/// only safe for upsampling or for downsampling within speech band.
fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if input.is_empty() || src_rate == dst_rate {
        return input.to_vec();
    }
    let step = src_rate as f64 / dst_rate as f64;
    let n_out = ((input.len() as f64) / step) as usize;
    let mut out = Vec::with_capacity(n_out);
    let mut pos = 0.0f64;
    while (pos as usize) + 1 < input.len() {
        let i = pos as usize;
        let frac = (pos - i as f64) as f32;
        let a = input[i];
        let b = input[i + 1];
        out.push(a + (b - a) * frac);
        pos += step;
    }
    out
}
