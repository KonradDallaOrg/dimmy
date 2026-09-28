//! Speaker-diarization probe: model download, the diarizer alone, ASR word
//! timestamps chunked like the meeting pipeline, and the whole FFI path.
//!
//!   diarize_asr_probe download
//!   diarize_asr_probe <in_16k.wav> <out.f32> diar          (probs, 8 f32 per 10 ms)
//!   diarize_asr_probe <in_16k.wav> <out.json> <parakeet|whisper> [chunk_secs] [whisper_model]
//!   diarize_asr_probe retranscribe <meeting_dir> '<config json>'
//!       (build with test-ffi and set DIMMY_TEST_CONFIG_DIR — never the real config)
//!
//! Numbers from these runs: docs/dev/diarization.md.

use std::time::Instant;

fn read_wav(path: &str) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    assert_eq!(spec.sample_rate, 16_000, "expects 16 kHz");
    assert_eq!(spec.channels, 1, "expects mono");
    match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => r
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
    }
}

#[derive(serde::Serialize)]
struct Word {
    w: String,
    s: f64,
    e: f64,
}

fn parakeet_chunk(pcm: &[f32], off: f64, out: &mut Vec<Word>) {
    let (_, js) = dimmy_lib::parakeet::transcribe_with_word_timestamps(pcm).expect("parakeet");
    let v: Vec<serde_json::Value> = serde_json::from_str(&js).expect("ts json");
    for w in v {
        out.push(Word {
            w: w["word"].as_str().unwrap_or("").to_string(),
            s: off + w["start"].as_f64().unwrap_or(0.0),
            e: off + w["end"].as_f64().unwrap_or(0.0),
        });
    }
}

#[cfg(feature = "local-stt")]
fn whisper_chunk(
    state: &mut whisper_rs::WhisperState,
    eot: i32,
    pcm: &[f32],
    off: f64,
    out: &mut Vec<Word>,
) {
    use whisper_rs::{FullParams, SamplingStrategy};
    let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    p.set_no_context(true);
    p.set_language(Some("it"));
    p.set_n_threads(4);
    p.set_token_timestamps(true);
    p.set_suppress_nst(true);
    p.set_print_progress(false);
    p.set_print_realtime(false);
    p.set_print_timestamps(false);
    state.full(p, pcm).expect("whisper_full");
    for i in 0..state.full_n_segments() {
        let seg = state.get_segment(i).unwrap();
        for t in 0..seg.n_tokens() {
            let tok = seg.get_token(t).unwrap();
            let d = tok.token_data();
            if d.id >= eot {
                continue;
            }
            let txt = tok.to_str().unwrap_or("").to_string();
            let (s, e) = (off + d.t0 as f64 / 100.0, off + d.t1 as f64 / 100.0);
            match out.last_mut() {
                Some(last) if !txt.starts_with(' ') && !last.w.is_empty() => {
                    last.w.push_str(&txt);
                    last.e = e;
                }
                _ => out.push(Word {
                    w: txt.trim().to_string(),
                    s,
                    e,
                }),
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a[1] == "retranscribe" {
        // End-to-end through the FFI, against DIMMY_TEST_CONFIG_DIR (needs the
        // test-ffi feature) so the real config is never touched.
        dimmy_lib::ffi::dimmy_init();
        let cfg = std::ffi::CString::new(a[3].as_str()).unwrap();
        let rc = unsafe { dimmy_lib::ffi::dimmy_set_config_json(cfg.as_ptr()) };
        eprintln!("set_config rc={rc}");
        let dir = std::ffi::CString::new(a[2].as_str()).unwrap();
        let mut buf = vec![0u8; 1 << 22];
        let t = Instant::now();
        let n = unsafe {
            dimmy_lib::ffi::dimmy_meeting_retranscribe(
                dir.as_ptr(),
                buf.as_mut_ptr() as *mut _,
                buf.len() as i32,
            )
        };
        eprintln!("retranscribe rc={n} in {:.1}s", t.elapsed().as_secs_f64());
        return;
    }
    if a[1] == "download" {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(dimmy_lib::diarize::download(|d, t| eprint!(" {d}/{t}")))
            .expect("download");
        eprintln!("model present: {}", dimmy_lib::diarize::model_present());
        return;
    }
    let pcm = read_wav(&a[1]);
    let engine = a[3].as_str();
    if engine == "diar" {
        let t = Instant::now();
        let d = dimmy_lib::diarize::diarize(&pcm).expect("diarize");
        let secs = t.elapsed().as_secs_f64();
        eprintln!(
            "diar: {:.0}s audio in {secs:.1}s (RTFx {:.1}), speakers {:?}",
            pcm.len() as f64 / 16_000.0,
            pcm.len() as f64 / 16_000.0 / secs,
            d.speakers(1.0)
        );
        let flat: Vec<u8> = d
            .probs
            .iter()
            .flatten()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        std::fs::write(&a[2], flat).unwrap();
        return;
    }
    let chunk_s: usize = a.get(4).map(|s| s.parse().unwrap()).unwrap_or(15);
    let audio_s = pcm.len() as f64 / 16_000.0;
    let mut words = Vec::new();
    let mut chunks_ms = Vec::new();

    let t_load = Instant::now();
    #[cfg(feature = "local-stt")]
    let mut wh = if engine == "whisper" {
        use whisper_rs::{WhisperContext, WhisperContextParameters};
        let mut cp = WhisperContextParameters::default();
        cp.use_gpu(std::env::var("WHISPER_CPU").is_err());
        cp.gpu_device(
            std::env::var("WHISPER_DEV")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        );
        let ctx = WhisperContext::new_with_params(&a[5], cp).expect("load whisper");
        let eot = ctx.token_eot();
        let state = ctx.create_state().expect("state");
        Some((ctx, state, eot))
    } else {
        None
    };
    if engine == "parakeet" {
        parakeet_chunk(&vec![0.0; 16_000], 0.0, &mut Vec::new());
    }
    let load_s = t_load.elapsed().as_secs_f64();

    let t0 = Instant::now();
    for (i, c) in pcm.chunks(chunk_s * 16_000).enumerate() {
        if c.len() < 8_000 {
            continue;
        }
        let off = (i * chunk_s) as f64;
        let tc = Instant::now();
        match engine {
            "parakeet" => parakeet_chunk(c, off, &mut words),
            #[cfg(feature = "local-stt")]
            "whisper" => {
                let (_, st, eot) = wh.as_mut().unwrap();
                whisper_chunk(st, *eot, c, off, &mut words)
            }
            _ => panic!("engine"),
        }
        chunks_ms.push(tc.elapsed().as_millis() as u64);
    }
    let wall_s = t0.elapsed().as_secs_f64();
    words.retain(|w| !w.w.is_empty());
    eprintln!(
        "{engine}: {audio_s:.0}s audio, load {load_s:.1}s, transcribe {wall_s:.1}s, RTFx {:.1}, {} words",
        audio_s / wall_s,
        words.len()
    );
    let j = serde_json::json!({"engine": engine, "audio_s": audio_s, "load_s": load_s, "wall_s": wall_s, "chunks_ms": chunks_ms, "words": words});
    std::fs::write(&a[2], serde_json::to_string(&j).unwrap()).unwrap();
}
