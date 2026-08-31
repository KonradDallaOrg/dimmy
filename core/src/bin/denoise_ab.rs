//! Does the GTCRN denoise help or hurt the local transcription? Measure it.
//!
//! The denoiser has been ON by default, with no toggle, since 2026-08-10, and
//! its effect on the TEXT has never been checked — only its effect on levels
//! and on latency. Meanwhile the 2025-2026 literature on speech enhancement in
//! front of modern ASR is uncomfortably consistent: enhancement artifacts
//! (spectral smearing, temporal discontinuities) are a distribution whisper was
//! never trained on, and denoising frequently makes recognition WORSE rather
//! than better. That is a claim about aggressive models on benchmark corpora,
//! not about a 48K-parameter model on this user's microphone, which is exactly
//! why it has to be measured here rather than argued.
//!
//! Takes real `audio_debug/<session>/raw.wav` captures and transcribes each one
//! TWICE with the same local whisper model, from the SAME 16 kHz buffer:
//!
//!   off — straight to whisper (the default since this harness answered)
//!   on  — through `gtcrn::maybe_denoise_16k` first (`DIMMY_GTCRN=1`)
//!
//! The downsample happens once and is shared, so the denoise is the only
//! variable. Whisper is deterministic on identical input, so any difference in
//! the output is attributable to it and nothing else.
//!
//! Writes nothing to the config dir beyond log lines: it never calls
//! `dimmy_init`, and the whisper model path is explicit.
//!
//! Usage:
//!   denoise_ab <dir-of-sessions> <model.bin> <lang> [max_sessions]

use std::path::{Path, PathBuf};

/// Short outputs that match one of these are whisper talking to itself over
/// silence, not transcribing. Collected from this project's own bug reports:
/// the "Grazie" family has shown up on every silent or near-silent capture
/// since the chunked paths went in.
const HALLUCINATION_MARKERS: &[&str] = &[
    "grazie",
    "grazie a tutti",
    "grazie per la visione",
    "grazie mille",
    "sottotitoli",
    "sottotitoli e revisione a cura di",
    "sottotitoli creati dalla comunità amara.org",
    "buongiorno a tutti",
    "thank you",
    "thanks for watching",
    "you",
];

/// Longest an output can be and still be judged a hallucination rather than a
/// short real sentence.
const HALLUCINATION_MAX_CHARS: usize = 60;

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

/// Lowercase, strip punctuation, collapse whitespace. Two transcripts that
/// differ only in commas are the same answer for this purpose.
fn normalize_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

/// Word-level Levenshtein distance. Reported against the longer of the two so
/// the number reads as "what fraction of the words changed".
fn word_distance(a: &[String], b: &[String]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, wa) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, wb) in b.iter().enumerate() {
            let cost = if wa == wb { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn looks_like_hallucination(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false; // empty is its own category, counted separately
    }
    if t.chars().count() > HALLUCINATION_MAX_CHARS {
        return false;
    }
    let words = normalize_words(t).join(" ");
    HALLUCINATION_MARKERS
        .iter()
        .any(|m| words == *m || words.starts_with(m))
}

fn transcribe(model: &Path, pcm16k: &[f32], lang: &str) -> String {
    if pcm16k.is_empty() {
        return String::new();
    }
    match dimmy_lib::local_stt::transcribe_local(model, pcm16k, lang, "") {
        Ok(t) => t.trim().to_string(),
        Err(e) => format!("<error: {e}>"),
    }
}

struct Row {
    session: String,
    secs: f32,
    off: String,
    on: String,
    changed_words: usize,
    total_words: usize,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "usage: {} <dir-of-sessions> <model.bin> <lang> [max_sessions]",
            args[0]
        );
        std::process::exit(2);
    }
    let sessions_dir = PathBuf::from(&args[1]);
    let model = PathBuf::from(&args[2]);
    let lang = args[3].clone();
    let max: usize = args
        .get(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    assert!(
        sessions_dir.is_dir(),
        "sessions dir not found: {}",
        sessions_dir.display()
    );
    assert!(model.is_file(), "model not found: {}", model.display());

    // The denoise is OFF by default since 2026-08-31 — this harness is what
    // decided that. It therefore has to opt back IN to have anything to compare
    // against; without this the "on" arm is byte-identical to the "off" arm and
    // the run reads as a confident "the denoise changes nothing".
    std::env::set_var("DIMMY_GTCRN", "1");

    // Same failure shape, different cause: `maybe_denoise_16k` also passes
    // audio straight through when the model is missing.
    let gtcrn_model = dimmy_lib::gtcrn::model_path();
    assert!(
        gtcrn_model.is_file(),
        "gtcrn model not found at {} — copy gtcrn_simple.onnx next to this binary, \
         otherwise both arms are the same audio and the comparison is meaningless",
        gtcrn_model.display()
    );
    eprintln!("[ab] gtcrn model: {}", gtcrn_model.display());
    eprintln!("[ab] whisper model: {}", model.display());

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&sessions_dir)
        .expect("read sessions dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("raw.wav").is_file())
        .collect();
    dirs.sort();
    dirs.truncate(max);
    eprintln!("[ab] {} sessions with raw.wav\n", dirs.len());

    let mut rows: Vec<Row> = Vec::new();
    for (i, dir) in dirs.iter().enumerate() {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let Some((pcm, rate)) = read_wav(&dir.join("raw.wav")) else {
            eprintln!("[{:>3}/{}] {name}  SKIP (unreadable)", i + 1, dirs.len());
            continue;
        };
        // One downsample, shared by both arms: the denoise is the only variable.
        let pcm16k = dimmy_lib::preprocess::downsample_to_16k(&pcm, rate);
        if pcm16k.is_empty() {
            eprintln!("[{:>3}/{}] {name}  SKIP (empty)", i + 1, dirs.len());
            continue;
        }
        let secs = pcm16k.len() as f32 / 16_000.0;

        let off = transcribe(&model, &pcm16k, &lang);
        let enhanced = dimmy_lib::gtcrn::maybe_denoise_16k(&pcm16k);
        // Borrowed means the denoiser declined — switched off, model missing,
        // or an inference error. Every one of those makes this session's two
        // arms the same audio, so refuse to fold it into the totals.
        assert!(
            matches!(enhanced, std::borrow::Cow::Owned(_)),
            "the denoise did not run on {name}: both arms would be identical audio \
             and the comparison would be a lie"
        );
        let denoised = enhanced.into_owned();
        assert_eq!(
            denoised.len(),
            pcm16k.len(),
            "denoise must preserve length, otherwise the arms are not comparable"
        );
        let on = transcribe(&model, &denoised, &lang);

        let wa = normalize_words(&off);
        let wb = normalize_words(&on);
        let changed = word_distance(&wa, &wb);
        let total = wa.len().max(wb.len());
        eprintln!(
            "[{:>3}/{}] {name}  {secs:>5.1}s  {} words changed of {}{}",
            i + 1,
            dirs.len(),
            changed,
            total,
            if changed == 0 { "  (identical)" } else { "" }
        );

        rows.push(Row {
            session: name,
            secs,
            off,
            on,
            changed_words: changed,
            total_words: total,
        });
    }

    report(&rows, &sessions_dir);
}

fn report(rows: &[Row], sessions_dir: &Path) {
    if rows.is_empty() {
        println!("\nno sessions transcribed.");
        return;
    }
    let identical = rows.iter().filter(|r| r.changed_words == 0).count();
    let empty_off = rows.iter().filter(|r| r.off.trim().is_empty()).count();
    let empty_on = rows.iter().filter(|r| r.on.trim().is_empty()).count();
    let hall_off = rows
        .iter()
        .filter(|r| looks_like_hallucination(&r.off))
        .count();
    let hall_on = rows
        .iter()
        .filter(|r| looks_like_hallucination(&r.on))
        .count();

    let mut divergence: Vec<f32> = rows
        .iter()
        .filter(|r| r.total_words > 0)
        .map(|r| r.changed_words as f32 / r.total_words as f32)
        .collect();
    divergence.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = if divergence.is_empty() {
        0.0
    } else {
        divergence[divergence.len() / 2]
    };
    let mean = if divergence.is_empty() {
        0.0
    } else {
        divergence.iter().sum::<f32>() / divergence.len() as f32
    };

    println!("\n════════ GTCRN denoise A/B ════════");
    println!("sessions                  {}", rows.len());
    println!(
        "identical output          {} / {}  ({:.0}%)",
        identical,
        rows.len(),
        100.0 * identical as f32 / rows.len() as f32
    );
    println!("word divergence, median   {:.1}%", 100.0 * median);
    println!("word divergence, mean     {:.1}%", 100.0 * mean);
    println!();
    println!("                          denoise OFF   denoise ON");
    println!("empty transcripts         {empty_off:>11}   {empty_on:>10}");
    println!("hallucination-shaped      {hall_off:>11}   {hall_on:>10}");
    println!();
    if empty_on + hall_on < empty_off + hall_off {
        println!("=> the denoise removes more failures than it creates.");
    } else if empty_on + hall_on > empty_off + hall_off {
        println!("=> the denoise CREATES more failures than it removes.");
    } else {
        println!("=> neither arm fails more often than the other.");
    }
    println!(
        "   Objective markers only. Where the two differ in wording, the pairs\n   \
         below need a human to say which is right."
    );

    // The pairs a person actually has to read, worst divergence first.
    let mut ranked: Vec<&Row> = rows.iter().filter(|r| r.changed_words > 0).collect();
    ranked.sort_by(|a, b| {
        let fa = a.changed_words as f32 / a.total_words.max(1) as f32;
        let fb = b.changed_words as f32 / b.total_words.max(1) as f32;
        fb.partial_cmp(&fa).unwrap()
    });

    let out = sessions_dir
        .parent()
        .unwrap_or(sessions_dir)
        .join("denoise_ab_report.txt");
    let mut text = String::new();
    text.push_str("GTCRN denoise A/B — pairs that differ, worst first\n");
    text.push_str("OFF = straight to whisper, ON = denoised first\n\n");
    for r in &ranked {
        text.push_str(&format!(
            "── {} ({:.1}s, {}/{} words changed)\n  OFF: {}\n  ON : {}\n\n",
            r.session,
            r.secs,
            r.changed_words,
            r.total_words,
            if r.off.is_empty() { "<empty>" } else { &r.off },
            if r.on.is_empty() { "<empty>" } else { &r.on },
        ));
    }
    match std::fs::write(&out, &text) {
        Ok(()) => println!("\nfull pairs written to {}", out.display()),
        Err(e) => println!("\ncould not write report: {e}"),
    }
}
