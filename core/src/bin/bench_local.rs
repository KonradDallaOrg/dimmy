//! bench_local — Local STT benchmark for all downloaded whisper models.
//!
//! Usage:
//!   cargo run --release --features local-stt-vulkan --bin bench_local -- [WAV_DIR]
//!
//! WAV_DIR defaults to `../tests/audio` (same samples as cloud benchmark).
//! Only processes WAV files < 120s to keep runs fast.
//!
//! Output: markdown table to stdout + saves to `../tests/results/`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core/ must have a parent")
        .to_path_buf();
    let default_audio_dir = project_dir.join("tests").join("audio");
    let audio_dir = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        default_audio_dir
    };
    let results_dir = project_dir.join("tests").join("results");

    assert!(
        audio_dir.is_dir(),
        "Audio directory not found: {}. Run ./tests/test_benchmark.sh download first.",
        audio_dir.display()
    );
    fs::create_dir_all(&results_dir).expect("failed to create results dir");

    // ── Discover downloaded models ──────────────────────────────────
    let models: Vec<&dimmy_lib::local_stt::LocalModel> = dimmy_lib::local_stt::AVAILABLE_MODELS
        .iter()
        .filter(|m| dimmy_lib::local_stt::model_exists(m.filename))
        .collect();

    if models.is_empty() {
        eprintln!("No models downloaded. Download at least one model first:");
        for m in dimmy_lib::local_stt::AVAILABLE_MODELS {
            eprintln!("  {} — {} ({}MB)", m.filename, m.description, m.size_mb);
        }
        std::process::exit(1);
    }

    eprintln!("Found {} downloaded model(s):", models.len());
    for m in &models {
        eprintln!("  {} ({}MB)", m.name, m.size_mb);
    }

    // ── Discover WAV files (quick tier only: < 120s) ────────────────
    let mut wav_files: Vec<PathBuf> = fs::read_dir(&audio_dir)
        .expect("cannot read audio dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "wav"))
        .collect();
    wav_files.sort();

    if wav_files.is_empty() {
        eprintln!(
            "No WAV files found in {}. Run ./tests/test_benchmark.sh download first.",
            audio_dir.display()
        );
        std::process::exit(1);
    }

    // Reference transcripts (same as cloud benchmark)
    let refs: HashMap<&str, &str> = HashMap::from([
        ("jfk", "And so my fellow Americans, ask not what your country can do for you, ask what you can do for your country."),
        ("micromachines", "This is the Micro Machine Man presenting the most midget miniature motorcade of Micro Machines."),
        ("gettysburg", "Four score and seven years ago our fathers brought forth on this continent, a new nation, conceived in Liberty"),
        ("harvard_f", "The birch canoe slid on the smooth planks."),
        ("harvard_m", "The boy was there when the sun rose."),
    ]);

    // Filter to short WAVs only (< 120s ≈ ~3.84 MB at 16kHz mono 16-bit)
    let max_samples: usize = 120 * 16_000; // 120 seconds at 16kHz
    let mut short_wavs: Vec<(PathBuf, String, Vec<f32>)> = Vec::new();

    for wav_path in &wav_files {
        match load_wav_16k_mono(wav_path) {
            Ok(samples) => {
                if samples.len() <= max_samples {
                    let id = wav_path.file_stem().unwrap().to_string_lossy().to_string();
                    let dur_s = samples.len() as f32 / 16000.0;
                    eprintln!("  {} — {:.1}s, {} samples", id, dur_s, samples.len());
                    short_wavs.push((wav_path.clone(), id, samples));
                } else {
                    let dur_s = samples.len() as f32 / 16000.0;
                    eprintln!(
                        "  {} — {:.0}s SKIPPED (>120s)",
                        wav_path.file_stem().unwrap().to_string_lossy(),
                        dur_s
                    );
                }
            }
            Err(e) => {
                eprintln!("  {} — LOAD ERROR: {}", wav_path.display(), e);
            }
        }
    }

    if short_wavs.is_empty() {
        eprintln!("No short WAV files (<120s) found.");
        std::process::exit(1);
    }

    // ── Run benchmark ───────────────────────────────────────────────
    eprintln!("\nRunning benchmark...\n");

    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H%M%S");
    let result_path = results_dir.join(format!("benchmark_local_{}.md", timestamp));

    let mut md = String::new();
    md.push_str("# Local STT Benchmark\n\n");
    md.push_str(&format!(
        "Date: {}\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));

    // Models table
    md.push_str("## Models\n\n");
    md.push_str("| Model | File | Size |\n");
    md.push_str("|---|---|---|\n");
    for m in &models {
        md.push_str(&format!(
            "| {} | {} | {}MB |\n",
            m.name, m.filename, m.size_mb
        ));
    }

    // Audio table
    md.push_str("\n## Audio Samples\n\n");
    md.push_str("| ID | Duration | Size |\n");
    md.push_str("|---|---|---|\n");
    for (path, id, samples) in &short_wavs {
        let dur_s = samples.len() as f32 / 16000.0;
        let file_kb = fs::metadata(path).map(|m| m.len() / 1024).unwrap_or(0);
        md.push_str(&format!("| {} | {:.1}s | {}KB |\n", id, dur_s, file_kb));
    }

    // Results table
    md.push_str("\n## Results\n\n");
    md.push_str("| Sample | Model | Inference | Words | Match% | First 80 chars |\n");
    md.push_str("|---|---|---|---|---|---|\n");

    // Also print to stdout
    println!();
    let header = format!(
        "  {:15} {:28} {:>10} {:>6} {:>6}  Preview",
        "Sample", "Model", "Time", "Words", "Match"
    );
    println!("{header}");
    let sep = format!(
        "  {:15} {:28} {:>10} {:>6} {:>6}  ────────────────────────────────",
        "───────────────", "────────────────────────────", "──────────", "──────", "──────"
    );
    println!("{sep}");

    for (_, id, samples) in &short_wavs {
        for m in &models {
            let model_path = dimmy_lib::local_stt::model_path(m.filename);
            assert!(
                model_path.is_file(),
                "model disappeared: {}",
                model_path.display()
            );

            // Detect language from sample id
            let lang = if id.contains("divina") || id.contains("pinocchio_it") {
                "it"
            } else {
                "en"
            };

            let start = Instant::now();
            let result = dimmy_lib::local_stt::transcribe_local(&model_path, samples, lang);
            let elapsed = start.elapsed();
            let ms = elapsed.as_millis();

            match result {
                Ok(text) => {
                    let word_count = text.split_whitespace().count();
                    let overlap = refs
                        .get(id.as_str())
                        .map(|r| word_overlap(r, &text))
                        .unwrap_or("-".to_string());
                    let preview = truncate(&text, 80);

                    println!(
                        "  {:15} {:28} {:>8}ms {:>6} {:>5}%  {}",
                        id, m.name, ms, word_count, overlap, preview
                    );
                    md.push_str(&format!(
                        "| {} | {} | {}ms | {} | {}% | {} |\n",
                        id, m.name, ms, word_count, overlap, preview
                    ));
                }
                Err(e) => {
                    let err_msg = format!("ERROR: {}", e);
                    println!(
                        "  {:15} {:28} {:>8}ms {:>6} {:>6}  {}",
                        id,
                        m.name,
                        ms,
                        "-",
                        "-",
                        &err_msg[..err_msg.len().min(60)]
                    );
                    md.push_str(&format!(
                        "| {} | {} | {}ms | - | - | {} |\n",
                        id,
                        m.name,
                        ms,
                        truncate(&err_msg, 80)
                    ));
                }
            }
        }
        println!();
    }

    md.push_str("\n---\n*Generated by Dimmy bench_local*\n");

    fs::write(&result_path, &md).expect("failed to write results");
    eprintln!("Results saved to: {}", result_path.display());
}

/// Load a WAV file and resample to 16kHz mono f32.
fn load_wav_16k_mono(path: &Path) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| format!("cannot open {}: {}", path.display(), e))?;

    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let channels = spec.channels as usize;

    // Read all samples as f32
    let raw_samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            let max_val = (1u32 << (bits - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / max_val)
                .collect()
        }
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(|s| s.ok())
            .collect(),
    };

    // Mix to mono
    let mono: Vec<f32> = if channels > 1 {
        raw_samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    } else {
        raw_samples
    };

    // Resample to 16kHz if needed (simple linear interpolation)
    let resampled = if sample_rate != 16000 {
        let ratio = 16000.0 / sample_rate as f64;
        let new_len = (mono.len() as f64 * ratio) as usize;
        let mut out = Vec::with_capacity(new_len);
        for i in 0..new_len {
            let src_idx = i as f64 / ratio;
            let idx0 = src_idx.floor() as usize;
            let idx1 = (idx0 + 1).min(mono.len() - 1);
            let frac = (src_idx - idx0 as f64) as f32;
            out.push(mono[idx0] * (1.0 - frac) + mono[idx1] * frac);
        }
        out
    } else {
        mono
    };

    // Clamp to [-1.0, 1.0]
    let clamped: Vec<f32> = resampled.iter().map(|s| s.clamp(-1.0, 1.0)).collect();

    Ok(clamped)
}

/// Simple word overlap score (same as cloud benchmark).
fn word_overlap(reference: &str, hypothesis: &str) -> String {
    let ref_words: Vec<String> = reference
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let hyp_words: Vec<String> = hypothesis
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    if ref_words.is_empty() {
        return "-".to_string();
    }

    let found = ref_words.iter().filter(|w| hyp_words.contains(w)).count();

    format!("{}", found * 100 / ref_words.len())
}

fn truncate(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if clean.len() > max {
        format!("{}...", &clean[..max - 3])
    } else {
        clean
    }
}
