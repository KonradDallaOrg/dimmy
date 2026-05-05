//! Offline chunked-STT smoke for Parakeet.
//!
//! Drives `dimmy_lib::chunked_stt::ChunkedTranscriber` against a long
//! WAV by simulating a live audio capture: a producer thread pushes
//! 100 ms slices of the WAV into the shared PCM buffer, the chunked
//! worker fires per-chunk events, and we collect every callback for
//! a printed summary. After the producer is done we call `stop()` to
//! drain the trailing audio. Use this on macOS / Linux dev machines
//! where there's no microphone but we still want to validate the
//! chunked path end-to-end.
//!
//! Build:
//!     cd core
//!     cargo build --release --bin chunked_smoke --features local-stt-parakeet
//!
//! Run:
//!     ORT_DYLIB_PATH=.../libonnxruntime.dylib \
//!     target/release/chunked_smoke <path-to-wav> [tile_count]
//!
//! `tile_count` (default 4) repeats the input WAV that many times so a
//! short fixture like `jfk_16k_mono.wav` (11 s) becomes 44 s and reliably
//! crosses several chunk boundaries (chunk_secs = 5).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = match args.get(1) {
        Some(p) => p.clone(),
        None => {
            eprintln!("usage: chunked_smoke <path-to-wav> [tile_count]");
            std::process::exit(2);
        }
    };
    let tile_count: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n >= 1)
        .unwrap_or(4);

    eprintln!("bundle present: {}", dimmy_lib::parakeet::bundle_present());
    if !dimmy_lib::parakeet::bundle_present() {
        eprintln!(
            "[FAIL] bundle missing at {:?}",
            dimmy_lib::parakeet::bundle_dir()
        );
        std::process::exit(1);
    }

    // ── Load + tile the WAV to 16 kHz mono f32. ─────────────────────
    let mut r = hound::WavReader::open(&path).expect("open wav");
    let spec = r.spec();
    eprintln!(
        "input: sr={} ch={} bits={} fmt={:?}",
        spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
    );
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample as i32;
            let scale = (1i64 << (bits - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    let mono: Vec<f32> = if spec.channels == 1 {
        raw
    } else {
        let ch = spec.channels as usize;
        raw.chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    let pcm_16k: Vec<f32> = if spec.sample_rate == 16_000 {
        mono
    } else {
        dimmy_lib::preprocess::downsample_to_16k(&mono, spec.sample_rate)
    };
    let mut tiled: Vec<f32> = Vec::with_capacity(pcm_16k.len() * tile_count);
    for _ in 0..tile_count {
        tiled.extend_from_slice(&pcm_16k);
    }
    let total_secs = tiled.len() as f32 / 16_000.0;
    eprintln!(
        "tiled: {} samples = {:.2}s @ 16 kHz (×{})",
        tiled.len(),
        total_secs,
        tile_count
    );

    // ── Wire the chunked transcriber. ───────────────────────────────
    let audio_buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(tiled.len())));

    // We collect (delta, cumulative, is_final, ts_ms) for the summary.
    type Event = (String, String, bool, u128);
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let events_w = events.clone();
    let t0 = Instant::now();
    let on_chunk: Arc<dimmy_lib::chunked_stt::ChunkCallback> =
        Arc::new(move |delta: &str, cumulative: &str, is_final: bool| {
            let ts = t0.elapsed().as_millis();
            eprintln!(
                "[chunk @ {:>5} ms] is_final={} delta={:?}",
                ts, is_final, delta
            );
            if let Ok(mut v) = events_w.lock() {
                v.push((delta.to_string(), cumulative.to_string(), is_final, ts));
            }
        });

    let transcriber = dimmy_lib::chunked_stt::ChunkedTranscriber::start(
        audio_buffer.clone(),
        16_000,
        5.0,
        500,
        on_chunk,
    );

    // ── Producer: feed the buffer in 100 ms slices, real-time. ─────
    // Mimics cpal's callback cadence well enough for the chunked
    // worker to fire multiple windows during the producer's run.
    let produce_chunk_samples = 1_600usize; // 100 ms @ 16 kHz
    let produce_sleep = Duration::from_millis(100);
    let mut idx = 0usize;
    while idx < tiled.len() {
        let end = (idx + produce_chunk_samples).min(tiled.len());
        if let Ok(mut b) = audio_buffer.lock() {
            b.extend_from_slice(&tiled[idx..end]);
        }
        idx = end;
        std::thread::sleep(produce_sleep);
    }
    eprintln!(
        "[producer done @ {:>5} ms] all {} samples buffered",
        t0.elapsed().as_millis(),
        tiled.len()
    );

    // ── Drain + summary. ────────────────────────────────────────────
    let final_text = transcriber.stop();
    eprintln!(
        "[stop @ {:>5} ms] final cumulative len={}",
        t0.elapsed().as_millis(),
        final_text.len()
    );

    let evs = events.lock().expect("event lock").clone();
    let chunk_count = evs.iter().filter(|(_, _, f, _)| !*f).count();
    let final_count = evs.iter().filter(|(_, _, f, _)| *f).count();

    println!("--- TEXT ---");
    println!("{}", final_text);
    println!("--- /TEXT ---");
    eprintln!(
        "[summary] chunks={} finals={} total_audio={:.2}s wall_to_final={} ms",
        chunk_count,
        final_count,
        total_secs,
        t0.elapsed().as_millis()
    );
}
