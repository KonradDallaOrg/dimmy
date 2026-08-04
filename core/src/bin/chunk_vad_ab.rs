//! Does the chunk-path VAD help or hurt REAL speech? Measure it.
//!
//! Replays real `audio_debug/<session>/raw.wav` captures through the chunked
//! dictation worker's window size, and for each window reports what
//! `process_chunk_vad_only` keeps and what whisper makes of it — trimmed
//! versus whole.
//!
//! Exists because the July anti-hallucination work was validated on an IDLE
//! mic (where trimming is obviously right) and never on speech. The batch
//! path was cleared by `preprocess_transcribe_ab`; this is the same
//! measurement aimed at the path that actually runs.
//!
//! Usage:
//!   chunk_vad_ab <dir-of-sessions> <model.bin> <lang> [max_sessions]

use std::path::Path;

/// Window the dictation worker uses (`ffi.rs`: 3 s chunks, 500 ms overlap).
const CHUNK_SECS: f32 = 3.0;

fn read_wav(path: &Path) -> Option<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .filter_map(|s| s.ok())
            .map(|s| s as f32 / (1i64 << (spec.bits_per_sample - 1)) as f32)
            .collect(),
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
    };
    let mono = if spec.channels > 1 {
        samples
            .chunks(spec.channels as usize)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect()
    } else {
        samples
    };
    Some((mono, spec.sample_rate))
}

fn transcribe(model: &Path, pcm: &[f32], rate: u32, lang: &str) -> String {
    if pcm.is_empty() {
        return "<skipped: no audio>".to_string();
    }
    let pcm16k = dimmy_lib::preprocess::downsample_to_16k(pcm, rate);
    if pcm16k.len() < 1600 {
        return format!("<sliver: {} samples @16k>", pcm16k.len());
    }
    match dimmy_lib::local_stt::transcribe_local(model, &pcm16k, lang, "") {
        Ok(t) => t.trim().to_string(),
        Err(e) => format!("<error: {e}>"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: chunk_vad_ab <dir> <model.bin> <lang> [max_sessions]");
        std::process::exit(2);
    }
    let dir = &args[1];
    let model = dimmy_lib::local_stt::model_path(&args[2]);
    let lang = &args[3];
    let max: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(2);

    if !model.exists() {
        eprintln!("model not found: {}", model.display());
        std::process::exit(1);
    }

    let mut sessions: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("cannot read {dir}: {e}");
            std::process::exit(1);
        }
    };
    sessions.sort();
    sessions.reverse();

    let mut retentions: Vec<f32> = Vec::new();
    let mut dropped = 0usize;
    let mut windows = 0usize;
    let mut done = 0usize;

    for session in sessions {
        if done >= max {
            break;
        }
        let raw_path = session.join("raw.wav");
        if !raw_path.exists() {
            continue;
        }
        let (raw, rate) = match read_wav(&raw_path) {
            Some(v) => v,
            None => continue,
        };
        if raw.len() < rate as usize * 6 {
            continue;
        }

        println!("\n═══ {} ═══", session.display());
        let win = (rate as f32 * CHUNK_SECS) as usize;
        for (i, chunk) in raw.chunks(win).enumerate() {
            if chunk.len() < win / 2 {
                continue;
            }
            let trimmed = dimmy_lib::preprocess::process_chunk_vad_only(chunk, rate);
            let keep = 100.0 * trimmed.len() as f32 / chunk.len() as f32;
            windows += 1;
            retentions.push(keep);
            if trimmed.is_empty() {
                dropped += 1;
            }
            println!(
                "  [win {i:02}] kept {keep:5.1}%  ({:.2}s of {:.2}s)",
                trimmed.len() as f32 / rate as f32,
                chunk.len() as f32 / rate as f32
            );
            println!(
                "      TRIMMED : {}",
                transcribe(&model, &trimmed, rate, lang)
            );
            println!("      WHOLE   : {}", transcribe(&model, chunk, rate, lang));
        }
        done += 1;
    }

    retentions.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = if retentions.is_empty() {
        0.0
    } else {
        retentions[retentions.len() / 2]
    };
    println!("\n── summary ──");
    println!("windows           : {windows}");
    println!("dropped entirely  : {dropped}");
    println!("median retention  : {median:.1} %");
}
