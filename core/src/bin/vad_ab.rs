//! Silero vs our RNNoise+energy gate: which one decides better?
//!
//! The chunk gate answers ONE question — is there speech in this window? —
//! and gets it wrong in two ways that cost very different things:
//!
//!   FALSE DROP  window held speech, gate said no  -> the user loses words
//!   FALSE PASS  window held nothing, gate said yes -> whisper hallucinates
//!               a training-set sign-off ("Grazie", "Thank you everyone")
//!
//! Today's gate is nnnoiseless (RNNoise) voice probability AND a per-frame
//! energy floor. The energy term exists only because RNNoise is trained to be
//! indifferent to level, so a keyboard click at -60 dBFS opens a speech window
//! (see preprocess.rs). That crutch is the fragile part: its threshold has to
//! span sessions, and the gap between the noisiest room and the quietest
//! speech in the real corpus is only ~6 dB.
//!
//! Silero scores a level-robust probability instead, which is why
//! faster-whisper and whisper.cpp both threshold it at a FIXED 0.5 and carry
//! no energy term at all. whisper-rs already wraps it, so this is a pure
//! measurement: nothing in the product changes either way.
//!
//! GROUND TRUTH is whisper's own reading of the UNTRIMMED window, classified
//! by `classify`. That is a noisy label — whisper hallucinates, which is the
//! whole problem — so the sign-off phrases are treated as "no speech". Any
//! window whose verdict is UNCERTAIN is reported but excluded from the score,
//! because scoring against a label we do not trust would launder a guess into
//! a number.
//!
//! Usage:
//!   vad_ab <dir-of-sessions> <whisper-model.bin> <silero-model.bin> <lang> [max]

use std::path::{Path, PathBuf};

/// Window the dictation worker uses (`ffi.rs`: 3 s chunks).
const CHUNK_SECS: f32 = 3.0;
/// Silero's own default, and the value faster-whisper and whisper.cpp ship.
const SILERO_THRESHOLD: f32 = 0.5;

/// Read a capture, WAV or Ogg. The Ogg branch is what lets this run over the
/// MEETING mic tracks, which is where the silence actually lives: dictations
/// are almost all speech, so a gate scored only on them never meets the case
/// it exists for.
fn read_audio(path: &Path) -> Option<(Vec<f32>, u32)> {
    if path.extension().and_then(|e| e.to_str()) == Some("ogg") {
        return dimmy_lib::ffi::decode_via_symphonia(&path.to_string_lossy()).ok();
    }
    read_wav(path)
}

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

/// What whisper makes of the untrimmed window, used as the label.
#[derive(PartialEq, Clone, Copy, Debug)]
enum Truth {
    Speech,
    Silence,
    Uncertain,
}

/// Known whisper sign-offs. A window that produces ONLY one of these held no
/// speech: they are what the model falls back on when handed silence.
const SIGN_OFFS: &[&str] = &[
    "grazie",
    "grazie a tutti",
    "grazie per la visione",
    "grazie mille",
    "thank you",
    "thanks for watching",
    "thank you everyone",
    "thank you for watching",
    "sottotitoli",
    "sottotitoli e revisione a cura di qtss",
    "amara.org",
    "you",
    "bye",
];

fn classify(text: &str) -> Truth {
    let t = text
        .trim()
        .trim_matches(|c: char| c == '.' || c == '!' || c == '?' || c == ',')
        .to_lowercase();
    if t.is_empty() || t.starts_with('<') {
        return Truth::Silence;
    }
    if SIGN_OFFS.iter().any(|s| t == *s) {
        return Truth::Silence;
    }
    // Short fragments are exactly where the label is unreliable: a real "si"
    // and a hallucinated "you" look the same from here. Do not pretend.
    if t.chars().count() < 12 {
        return Truth::Uncertain;
    }
    Truth::Speech
}

struct Score {
    false_drop: usize,
    false_pass: usize,
    correct: usize,
}

impl Score {
    fn new() -> Self {
        Self {
            false_drop: 0,
            false_pass: 0,
            correct: 0,
        }
    }
    fn record(&mut self, truth: Truth, passed: bool) {
        match (truth, passed) {
            (Truth::Speech, false) => self.false_drop += 1,
            (Truth::Silence, true) => self.false_pass += 1,
            (Truth::Uncertain, _) => {}
            _ => self.correct += 1,
        }
    }
    fn report(&self, name: &str) {
        let total = self.correct + self.false_drop + self.false_pass;
        if total == 0 {
            println!("  {name:<22} no scored windows");
            return;
        }
        println!(
            "  {:<22} corrette {:>3}/{:<3}  PAROLE PERSE {:>3}  allucinazioni passate {:>3}",
            name, self.correct, total, self.false_drop, self.false_pass
        );
    }
}

fn transcribe(model: &Path, pcm: &[f32], rate: u32, lang: &str) -> String {
    if pcm.is_empty() {
        return String::new();
    }
    let pcm16k = dimmy_lib::preprocess::downsample_to_16k(pcm, rate);
    if pcm16k.len() < 1600 {
        return String::new();
    }
    match dimmy_lib::local_stt::transcribe_local(model, &pcm16k, lang, "") {
        Ok(t) => t.trim().to_string(),
        Err(_) => String::new(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: vad_ab <dir> <whisper-model.bin> <silero-model.bin> <lang> [max]");
        std::process::exit(2);
    }
    let dir = &args[1];
    let whisper_model = dimmy_lib::local_stt::model_path(&args[2]);
    let silero_model: PathBuf = dimmy_lib::local_stt::model_path(&args[3]);
    let lang = &args[4];
    let max: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(8);

    for (label, p) in [("whisper", &whisper_model), ("silero", &silero_model)] {
        if !p.exists() {
            eprintln!("{label} model not found: {}", p.display());
            std::process::exit(1);
        }
    }

    // Speed-only mode: no whisper in the loop, so the number is Silero's real
    // cost and not contention with a model on the other side of the process.
    // A gate that takes longer than the audio it judges is unusable whatever
    // its accuracy, so this is the question to settle FIRST.
    let speed_only = std::env::var("VAD_AB_SPEED_ONLY").is_ok();
    let mut ctx_params = whisper_rs::WhisperVadContextParams::new();
    if std::env::var("VAD_AB_SILERO_GPU").is_ok() {
        ctx_params.set_use_gpu(true);
    }
    let mut vad = match whisper_rs::WhisperVadContext::new(
        silero_model.to_string_lossy().as_ref(),
        ctx_params,
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cannot load Silero: {e:?}");
            std::process::exit(1);
        }
    };

    let mut sessions: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("cannot read {dir}: {e}");
            std::process::exit(1);
        }
    };
    sessions.sort();
    sessions.reverse();

    let mut ours = Score::new();
    let mut silero = Score::new();
    let mut uncertain = 0usize;
    let mut windows = 0usize;
    let mut disagreements: Vec<String> = Vec::new();
    let mut ours_ms: Vec<f64> = Vec::new();
    let mut silero_ms: Vec<f64> = Vec::new();
    let mut done = 0usize;

    for session in sessions {
        if done >= max {
            break;
        }
        // Dictation session (raw.wav) or meeting (audio_mic.ogg). The meeting
        // mic track is the silence-heavy material.
        let raw_path = [session.join("raw.wav"), session.join("audio_mic.ogg")]
            .into_iter()
            .find(|p| p.exists());
        let raw_path = match raw_path {
            Some(p) => p,
            None => continue,
        };
        let (raw, rate) = match read_audio(&raw_path) {
            Some(v) => v,
            None => continue,
        };
        if raw.len() < rate as usize * 6 {
            continue;
        }
        let name = session
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        println!("\n═══ {name} ═══");

        // Cap per session: a 20-minute meeting is 400 windows and whisper has
        // to read every one of them for the label. Breadth across recordings
        // beats depth inside one, since the mic conditions are what vary.
        let per_session: usize = std::env::var("VAD_AB_WINDOWS_PER_SESSION")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX);
        let win = (rate as f32 * CHUNK_SECS) as usize;
        let mut in_session = 0usize;
        for (i, chunk) in raw.chunks(win).enumerate() {
            if chunk.len() < win / 2 {
                continue;
            }
            if in_session >= per_session {
                break;
            }
            in_session += 1;
            windows += 1;

            // Ours: the shipping gate. Empty output == window dropped.
            let t_ours = std::time::Instant::now();
            let ours_pass = !dimmy_lib::preprocess::process_chunk_vad_only(chunk, rate).is_empty();
            ours_ms.push(t_ours.elapsed().as_secs_f64() * 1000.0);

            // Silero: whisper.cpp's VAD wants 16 kHz.
            let pcm16k = dimmy_lib::preprocess::downsample_to_16k(chunk, rate);
            let mut params = whisper_rs::WhisperVadParams::new();
            params.set_threshold(SILERO_THRESHOLD);
            let t_sil = std::time::Instant::now();
            let silero_pass = vad
                .segments_from_samples(params, &pcm16k)
                .map(|segs| segs.num_segments() > 0)
                .unwrap_or(false);
            silero_ms.push(t_sil.elapsed().as_secs_f64() * 1000.0);

            if speed_only {
                continue;
            }
            let text = transcribe(&whisper_model, chunk, rate, lang);
            let truth = classify(&text);
            if truth == Truth::Uncertain {
                uncertain += 1;
            }
            ours.record(truth, ours_pass);
            silero.record(truth, silero_pass);

            let mark = |p: bool| if p { "PASSA" } else { "scarta" };
            println!(
                "  [win {i:02}] verita={truth:?}  nostra={}  silero={}  | {}",
                mark(ours_pass),
                mark(silero_pass),
                if text.is_empty() { "<vuoto>" } else { &text }
            );
            if ours_pass != silero_pass {
                disagreements.push(format!(
                    "{name} win{i:02}  verita={truth:?}  nostra={}  silero={}  | {text}",
                    mark(ours_pass),
                    mark(silero_pass)
                ));
            }
        }
        done += 1;
    }

    let pct = |v: &mut Vec<f64>, p: f64| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[((v.len() - 1) as f64 * p) as usize]
    };
    println!("\n────────── COSTO PER FINESTRA DA 3 s ──────────");
    println!("  finestre cronometrate: {}", ours_ms.len());
    println!(
        "  RNNoise + energia   mediana {:>9.2} ms   p95 {:>9.2} ms",
        pct(&mut ours_ms, 0.5),
        pct(&mut ours_ms, 0.95)
    );
    println!(
        "  Silero              mediana {:>9.2} ms   p95 {:>9.2} ms",
        pct(&mut silero_ms, 0.5),
        pct(&mut silero_ms, 0.95)
    );
    println!("  (la finestra dura 3000 ms: oltre, il gate non tiene il passo)");
    if speed_only {
        return;
    }

    println!("\n────────── RISULTATO ──────────");
    println!("finestre totali        : {windows}");
    println!("escluse (verita incerta): {uncertain}");
    println!();
    ours.report("RNNoise + energia");
    silero.report("Silero");
    println!("\ndisaccordi ({}):", disagreements.len());
    for d in disagreements.iter().take(40) {
        println!("  {d}");
    }
}
