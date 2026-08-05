//! Offline A/B of the dictation preprocess pipeline over real captures.
//!
//! Point it at a directory of `audio_debug/<session>/raw.wav` captures and it
//! reruns the CURRENT `process_buffer` over each one, reporting what the
//! pipeline does to level, peak and clipping. Compares against the
//! `processed.wav` the shipping build wrote next to it, when present.
//!
//! This exists because the clipping bug fixed on 2026-08-04 was invisible in
//! unit tests but obvious in one column of real data: every preprocessing-ON
//! capture peaked at exactly 1.000. Keep a way to look at that column.
//!
//! Usage:
//!   preprocess_ab <dir-of-sessions>

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

fn rms(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let sum: f64 = s.iter().map(|&x| (x as f64) * (x as f64)).sum();
    (sum / s.len() as f64).sqrt() as f32
}

fn peak(s: &[f32]) -> f32 {
    s.iter().fold(0.0f32, |m, &x| m.max(x.abs()))
}

fn clip_pct(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    100.0 * s.iter().filter(|x| x.abs() >= 0.999).count() as f32 / s.len() as f32
}

fn db(x: f32) -> f32 {
    if x <= 0.0 {
        -999.0
    } else {
        20.0 * x.log10()
    }
}

fn main() {
    let dir = match std::env::args().nth(1) {
        Some(d) => d,
        None => {
            eprintln!("usage: preprocess_ab <dir-of-sessions>");
            std::process::exit(2);
        }
    };

    println!(
        "{:<22} {:>8} {:>8} {:>7} | {:>8} {:>7} {:>6} | {:>8} {:>7} {:>6} {:>6}",
        "session",
        "rawRMS",
        "rawPk",
        "crest",
        "oldRMS",
        "oldPk",
        "oldClp",
        "newRMS",
        "newPk",
        "newClp",
        "keep%"
    );
    println!("{}", "-".repeat(118));

    let mut entries: Vec<_> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("cannot read {dir}: {e}");
            std::process::exit(1);
        }
    };
    entries.sort();

    let (mut n, mut old_clipping, mut new_clipping) = (0usize, 0usize, 0usize);

    for session in entries {
        let raw_path = session.join("raw.wav");
        if !raw_path.exists() {
            continue;
        }
        let (raw, sr) = match read_wav(&raw_path) {
            Some(v) => v,
            None => continue,
        };
        if raw.is_empty() {
            continue;
        }

        let old = read_wav(&session.join("processed.wav")).map(|(s, _)| s);
        // Skip pass-through sessions (preprocessing was OFF): nothing to compare.
        if let Some(ref o) = old {
            if o.len() == raw.len() && o.iter().zip(raw.iter()).all(|(a, b)| a == b) {
                continue;
            }
        }

        let new = dimmy_lib::preprocess::process_buffer(&raw, sr);

        let name = session
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let (r_rms, r_pk) = (rms(&raw), peak(&raw));
        let crest = db(r_pk) - db(r_rms);
        let (o_rms, o_pk, o_clp) = match old {
            Some(ref o) => (rms(o), peak(o), clip_pct(o)),
            None => (f32::NAN, f32::NAN, f32::NAN),
        };
        let (n_rms, n_pk, n_clp) = (rms(&new), peak(&new), clip_pct(&new));
        let keep = 100.0 * new.len() as f32 / raw.len() as f32;

        if o_clp > 0.0 {
            old_clipping += 1;
        }
        if n_clp > 0.0 {
            new_clipping += 1;
        }
        n += 1;

        println!(
            "{name:<22} {r_rms:8.5} {r_pk:8.3} {crest:6.1}dB | \
             {o_rms:8.5} {o_pk:7.3} {o_clp:5.2}% | \
             {n_rms:8.5} {n_pk:7.3} {n_clp:5.2}% {keep:5.1}"
        );
    }

    println!();
    println!("sessions compared          : {n}");
    println!("clipping BEFORE (shipped)  : {old_clipping}/{n}");
    println!("clipping AFTER  (this code): {new_clipping}/{n}");
}
