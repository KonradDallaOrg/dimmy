//! Offline noise-suppression compare tool.
//!
//! Usage:
//!   cargo run --release --bin denoise_offline -- <input.wav> [<output.wav>]
//!
//! Reads a mono WAV (or downmixes stereo via average — same recipe
//! the runtime secondary callback uses), resamples to 48 kHz if
//! needed (linear interpolator from audio.rs's own LinearResampler
//! design — kept inline here to avoid the heavyweight `dimmy_lib`
//! features), runs each 480-sample frame through `dfn::DfnProcessor`
//! and writes the result back as a 48 kHz mono int16 WAV.
//!
//! Output filename defaults to `<input>_dfn.wav` next to the input.
//!
//! Intended for A/B-ing a meeting recording. Pair with the original
//! `audio_mic.wav` / `audio_system.wav` to compare side-by-side in
//! whatever audio editor you trust (Audacity, Reaper, …).
//!
//! NOTE on DFN3: this MVP wires nnnoiseless only. DeepFilterNet3
//! integration is tracked separately; the binary currently ships
//! one backend so the user can hear the nnnoiseless result first
//! and decide if it's enough.

use dimmy_lib::dfn::DfnProcessor;
use std::path::{Path, PathBuf};

const TARGET_RATE: u32 = 48_000;
const FRAME: usize = DfnProcessor::FRAME_SIZE;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "Usage: {} <input.wav> [<output.wav>]\n\nReads input, denoises via nnnoiseless,\nwrites a 48 kHz mono int16 WAV.",
            args.first().map(|s| s.as_str()).unwrap_or("denoise_offline")
        );
        std::process::exit(2);
    }
    let input_path = PathBuf::from(&args[1]);
    let output_path = if args.len() >= 3 {
        PathBuf::from(&args[2])
    } else {
        default_output_path(&input_path)
    };
    println!(
        "denoise_offline: backend=nnnoiseless\n  in : {}\n  out: {}",
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

    // --- Force the runtime toggle on so try_init returns Some -----------
    dimmy_lib::dfn::DENOISE_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut proc = DfnProcessor::try_init().ok_or("DfnProcessor::try_init returned None")?;

    // --- Process in 480-sample frames -----------------------------------
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
        // Pad the last partial frame with zeros so we don't lose it.
        // The denoised tail is then truncated back to `tail` samples
        // so the output duration matches the input.
        let mut padded = [0.0f32; FRAME];
        padded[..tail].copy_from_slice(&work[n_frames * FRAME..]);
        proc.process_frame(&padded, &mut frame_out);
        out.extend_from_slice(&frame_out[..tail]);
    }
    println!(
        "  processed: {} frames ({} samples → {:.2} s)",
        n_frames,
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

fn default_output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "denoised".to_string());
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{}_dfn.wav", stem))
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
