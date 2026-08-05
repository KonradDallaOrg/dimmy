//! Does preprocessing help or hurt the LOCAL transcription? Measure it.
//!
//! Takes real `audio_debug/<session>/raw.wav` captures and transcribes each
//! one THREE times with the same local whisper model:
//!
//!   raw       — no preprocessing at all (what the user gets with the toggle off)
//!   highpass  — 80 Hz highpass only (the cloud/file-load route)
//!   full      — highpass + VAD + AGC (the local route, toggle on)
//!
//! The clipping bug fixed on 2026-08-04 was found by comparing levels. This
//! answers the question levels cannot: whether the surviving stages make the
//! TEXT better or worse. Guessing at that from waveform statistics is how the
//! VAD ended up suspected without evidence.
//!
//! Usage:
//!   preprocess_transcribe_ab <dir-of-sessions> <model.bin> <lang> [max_sessions]

use std::path::Path;

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

fn transcribe(model: &Path, pcm48: &[f32], rate: u32, lang: &str) -> String {
    if pcm48.is_empty() {
        return "<empty audio>".to_string();
    }
    let pcm16k = dimmy_lib::preprocess::downsample_to_16k(pcm48, rate);
    if pcm16k.is_empty() {
        return "<empty after downsample>".to_string();
    }
    match dimmy_lib::local_stt::transcribe_local(model, &pcm16k, lang, "") {
        Ok(t) => t.trim().to_string(),
        Err(e) => format!("<error: {e}>"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: preprocess_transcribe_ab <dir> <model.bin> <lang> [max_sessions]");
        std::process::exit(2);
    }
    let dir = &args[1];
    let model = dimmy_lib::local_stt::model_path(&args[2]);
    let lang = &args[3];
    let max: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5);

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
    sessions.reverse(); // newest first — most representative of the current mic

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
        // Skip anything too short to carry a sentence.
        if raw.len() < rate as usize * 3 {
            continue;
        }

        let highpass = dimmy_lib::preprocess::process_buffer_for_file_load(&raw, rate);
        let full = dimmy_lib::preprocess::process_buffer_guarded(&raw, rate);

        println!(
            "\n═══ {} ({:.1}s) ═══",
            session.display(),
            raw.len() as f32 / rate as f32
        );
        println!(
            "  kept: highpass {:.0}%  full {:.0}%",
            100.0 * highpass.len() as f32 / raw.len() as f32,
            100.0 * full.len() as f32 / raw.len() as f32
        );
        println!("  RAW      : {}", transcribe(&model, &raw, rate, lang));
        println!("  HIGHPASS : {}", transcribe(&model, &highpass, rate, lang));
        println!("  FULL     : {}", transcribe(&model, &full, rate, lang));
        done += 1;
    }
    println!("\n{done} session(s) compared.");
}
