//! Nemotron 3.5 cache-aware streaming ASR — measurement harness.
//!
//! Answers the only question that matters before we consider adopting it:
//! does streaming Italian come out CLEAN, and at what cost per chunk?
//!
//! Our current realtime path cuts a fixed 3 s window, re-transcribes 500 ms of
//! overlap, and stitches with a last-N-words dedup. That dedup was validated on
//! 30 s windows; on 3 s windows whisper renders the overlap differently each
//! time, so words come out doubled or eaten. Nemotron instead carries encoder
//! state ACROSS chunks — there is no overlap to stitch, so the failure mode
//! cannot occur by construction. This binary is how we find out whether that
//! holds on real recordings rather than on the model card.
//!
//! Usage:
//!   cargo run --release --bin nemotron_smoke --features nemotron-streaming -- \
//!       <audio> [lang] [model-dir]
//!
//! `audio` is anything Symphonia decodes (our meeting `audio_mic.ogg` included).
//! `lang` defaults to it-IT. Pass `auto` to let the model choose.
//!
//! When the file sits in a meeting directory the harness also prints the
//! whisper transcript we already stored next to it, so the two can be read
//! side by side. That is a REFERENCE, not ground truth: whisper has its own
//! errors, and on these very recordings it is what produced the doubled words.

use std::path::{Path, PathBuf};
use std::time::Instant;

/// 560 ms at 16 kHz — the chunk the multilingual model is exported for.
const CHUNK_SAMPLES: usize = 8960;
const RATE: u32 = 16_000;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: nemotron_smoke <audio> [lang=it-IT] [model-dir=E:/nemotron-onnx]");
        std::process::exit(2);
    }
    let audio_path = &args[1];
    let lang = args.get(2).map(String::as_str).unwrap_or("it-IT");
    let model_dir = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "E:/nemotron-onnx".to_string());

    let audio = load_16k_mono(audio_path);
    let secs = audio.len() as f32 / RATE as f32;
    println!("audio    : {audio_path}");
    println!(
        "durata   : {secs:.1} s  ({} campioni @ {RATE} Hz)",
        audio.len()
    );
    println!("lingua   : {lang}");
    println!("modello  : {model_dir}\n");

    let t_load = Instant::now();
    let mut model = parakeet_rs::Nemotron::from_pretrained(&model_dir, None)
        .unwrap_or_else(|e| panic!("caricamento modello fallito: {e:?}"));
    println!("caricato in {:.1} s", t_load.elapsed().as_secs_f32());

    if model.mode() == parakeet_rs::NemotronMode::Multilingual {
        model
            .set_target_lang(lang)
            .unwrap_or_else(|e| panic!("lingua '{lang}' rifiutata: {e:?}"));
    } else {
        eprintln!("ATTENZIONE: modello English-only, '{lang}' ignorato");
    }

    // Stream it exactly as the app would: one chunk at a time, no lookahead.
    let mut per_chunk_ms: Vec<f32> = Vec::new();
    let t0 = Instant::now();
    for chunk in audio.chunks(CHUNK_SAMPLES) {
        let padded = if chunk.len() < CHUNK_SAMPLES {
            let mut p = chunk.to_vec();
            p.resize(CHUNK_SAMPLES, 0.0);
            p
        } else {
            chunk.to_vec()
        };
        let t = Instant::now();
        model
            .transcribe_chunk(&padded)
            .unwrap_or_else(|e| panic!("transcribe_chunk fallito: {e:?}"));
        per_chunk_ms.push(t.elapsed().as_secs_f32() * 1000.0);
    }
    // Trailing silence flushes whatever the decoder is still holding.
    for _ in 0..3 {
        let _ = model.transcribe_chunk(&vec![0.0; CHUNK_SAMPLES]);
    }
    let wall = t0.elapsed().as_secs_f32();

    let text = model.get_transcript();

    per_chunk_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |q: f32| -> f32 {
        if per_chunk_ms.is_empty() {
            return 0.0;
        }
        let i = ((per_chunk_ms.len() as f32 - 1.0) * q).round() as usize;
        per_chunk_ms[i]
    };

    println!("\n--- prestazioni ---");
    println!("chunk       : {} da 560 ms", per_chunk_ms.len());
    println!(
        "per chunk   : p50 {:.0} ms   p95 {:.0} ms   max {:.0} ms",
        pick(0.50),
        pick(0.95),
        pick(1.0)
    );
    // Below 560 ms per chunk means it keeps up with the microphone in realtime.
    println!(
        "budget      : {} (il tempo reale richiede < 560 ms per chunk)",
        if pick(0.95) < 560.0 {
            "RISPETTATO"
        } else {
            "SFORATO"
        }
    );
    println!(
        "totale      : {wall:.1} s per {secs:.1} s di audio (RTF {:.1}x)",
        secs / wall
    );

    println!("\n--- trascrizione Nemotron ---\n{}", text.trim());

    if let Some(reference) = whisper_reference(Path::new(audio_path)) {
        println!("\n--- riferimento whisper già su disco (NON verità assoluta) ---\n{reference}");
    }

    println!("\n--- da guardare a occhio ---");
    println!("1. parole doppie o tagliate ai confini (il difetto che vogliamo eliminare)");
    println!("2. punteggiatura e maiuscole (Nemotron le emette da solo, whisper pure)");
    println!("3. p95 sopra 560 ms significa che in tempo reale accumulerebbe ritardo");
}

/// Decode anything Symphonia handles, fold to mono 16 kHz, peak-normalise.
/// Peak normalisation mirrors what the crate's own example does; without it the
/// model sees a much quieter signal than it was trained on.
fn load_16k_mono(path: &str) -> Vec<f32> {
    let (samples, rate) = dimmy_lib::ffi::decode_via_symphonia(path)
        .unwrap_or_else(|e| panic!("decodifica di {path} fallita: {e}"));
    assert!(!samples.is_empty(), "{path} non contiene campioni");

    let mut mono = dimmy_lib::preprocess::downsample_to_16k(&samples, rate);
    assert!(
        !mono.is_empty(),
        "ricampionamento a 16 kHz ha prodotto zero campioni"
    );

    let peak = mono.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    if peak > 1e-6 {
        for s in &mut mono {
            *s /= peak + 1e-5;
        }
    }
    assert!(
        mono.iter().all(|s| s.is_finite()),
        "campioni non finiti dopo la normalizzazione"
    );
    mono
}

/// A meeting directory keeps the whisper transcript next to the audio.
fn whisper_reference(audio: &Path) -> Option<String> {
    let dir: PathBuf = audio.parent()?.to_path_buf();
    let txt = dir.join("transcripts.txt");
    let body = std::fs::read_to_string(txt).ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    // These run long; the head is enough to eyeball style and boundaries.
    Some(trimmed.chars().take(1200).collect())
}
