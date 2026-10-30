//! nemotron_smoke -- drive the Nemotron engine in-process from a WAV, as the
//! app would from a microphone: 100 ms pushes, results polled after each.
//!
//! Usage:
//!   nemotron_smoke --lib <nemo_speech_asr_c.dll> --model <asr.gguf> [--gpu N]
//!                  [--diar <diarization.gguf>] [--lang it] [--keywords "WSL,Sentry"]
//!                  [--oneshot] <audio.wav>
//!
//! Prints every delta as it would be typed, the final text, speaker turns
//! when --diar is given, and how far ahead of realtime the engine stayed.

use dimmy_lib::nemotron::{self, EngineSpec, Transcript};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let mut a = std::env::args().skip(1);
    let (mut lib, mut model, mut diar, mut wav) = (None, None, None, None);
    let (mut gpu, mut lang, mut keywords, mut oneshot) = (None, String::new(), Vec::new(), false);
    let mut no_speakers = false;
    while let Some(x) = a.next() {
        match x.as_str() {
            "--lib" => lib = a.next().map(PathBuf::from),
            "--model" => model = a.next().map(PathBuf::from),
            "--diar" => diar = a.next().map(PathBuf::from),
            "--gpu" => gpu = a.next().and_then(|g| g.parse().ok()),
            "--lang" => lang = a.next().unwrap_or_default(),
            "--keywords" => {
                keywords = a
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|k| k.trim().to_string())
                    .collect()
            }
            "--oneshot" => oneshot = true,
            "--no-speakers" => no_speakers = true,
            _ => wav = Some(PathBuf::from(x)),
        }
    }
    let (Some(library), Some(model), Some(wav)) = (lib, model, wav) else {
        eprintln!("usage: nemotron_smoke --lib <dll> --model <gguf> [--gpu N] [--diar <gguf>] [--lang it] [--keywords a,b] [--oneshot] <wav>");
        std::process::exit(2);
    };
    // --no-speakers loads the diarizer but asks no stream for speakers:
    // what a dictation pays in the app, where the diarizer is always loaded.
    let speakers = diar.is_some() && !no_speakers;
    let spec = EngineSpec {
        library,
        model,
        gpu,
        diarizer: diar,
    };

    let t = Instant::now();
    let rec = nemotron::recognizer(&spec).unwrap_or_else(|e| fail(&e.to_string()));
    eprintln!("loaded in {:.1}s", t.elapsed().as_secs_f32());
    let pcm = read_16k(&wav);
    let secs = pcm.len() as f64 / nemotron::SAMPLE_RATE as f64;

    if oneshot {
        let t = Instant::now();
        let (text, words) = rec
            .transcribe(&pcm, &lang, &keywords)
            .unwrap_or_else(|e| fail(&e.to_string()));
        let wall = t.elapsed().as_secs_f64();
        println!(
            "{text}\n--- one-shot: {secs:.1}s audio in {wall:.2}s ({:.1}x), {} words",
            secs / wall,
            words.len()
        );
        return;
    }

    let mut stream = Arc::clone(&rec)
        .stream(&lang, &keywords, speakers)
        .unwrap_or_else(|e| fail(&e.to_string()));
    let mut transcript = Transcript::default();
    let mut words = Vec::new();
    let (mut busy, mut worst_lag) = (0.0f64, 0.0f64);
    let step = nemotron::SAMPLE_RATE / 10;
    for (i, chunk) in pcm.chunks(step).enumerate() {
        let t = Instant::now();
        stream.push(chunk).unwrap_or_else(|e| fail(&e.to_string()));
        let ups = stream.poll().unwrap_or_else(|e| fail(&e.to_string()));
        let took = t.elapsed().as_secs_f64();
        busy += took;
        worst_lag = worst_lag.max(took - 0.1);
        for u in ups {
            let typed = transcript.apply(&u);
            let delta = &typed.delta;
            if typed.backspace > 0 {
                println!(
                    "[{:6.1}s] <- {} chars",
                    (i + 1) as f64 / 10.0,
                    typed.backspace
                );
            }
            if !delta.is_empty() {
                println!(
                    "[{:6.1}s] +{delta:?}{}",
                    (i + 1) as f64 / 10.0,
                    if u.is_final { "  (final)" } else { "" }
                );
            }
            if u.is_final {
                words.extend(u.words);
            }
        }
    }
    for u in stream.finish().unwrap_or_else(|e| fail(&e.to_string())) {
        transcript.apply(&u);
        if u.is_final {
            words.extend(u.words);
        }
    }
    println!("\n=== FINAL ===\n{}", transcript.text());
    if speakers {
        println!("\n=== SPEAKERS ===");
        let mut cur: Option<(u8, f64, Vec<String>)> = None;
        for w in &words {
            match cur.as_mut() {
                Some(c) if c.0 == w.speaker => c.2.push(w.text.clone()),
                _ => {
                    if let Some((s, st, ws)) = cur.take() {
                        println!("[{st:6.1}s] Speaker {s}: {}", ws.join(" "));
                    }
                    cur = Some((w.speaker, w.start, vec![w.text.clone()]));
                }
            }
        }
        if let Some((s, st, ws)) = cur {
            println!("[{st:6.1}s] Speaker {s}: {}", ws.join(" "));
        }
    }
    println!(
        "\n--- stream: {secs:.1}s audio, {busy:.2}s compute ({:.1}x realtime), worst push+poll {:.0} ms over budget, {} words",
        secs / busy.max(1e-6),
        worst_lag.max(0.0) * 1000.0,
        words.len()
    );
}

fn fail(msg: &str) -> ! {
    eprintln!("nemotron_smoke: {msg}");
    std::process::exit(1)
}

fn read_16k(path: &PathBuf) -> Vec<f32> {
    let mut r =
        hound::WavReader::open(path).unwrap_or_else(|e| fail(&format!("{}: {e}", path.display())));
    let spec = r.spec();
    let ch = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
    };
    let mono: Vec<f32> = raw
        .chunks_exact(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    if spec.sample_rate as usize == nemotron::SAMPLE_RATE {
        mono
    } else {
        dimmy_lib::preprocess::downsample_to_16k(&mono, spec.sample_rate)
    }
}
