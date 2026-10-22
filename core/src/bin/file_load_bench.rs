//! file_load_bench — memory and time of loading a long audio file, the way
//! file load did it before 2026-10-22 against the streamed path.
//!
//!   file_load_bench old <file>                 whole decode + copies + old split
//!   file_load_bench new <file>                 streamed decode/filter/chunks
//!   file_load_bench transcribe <file> <model|parakeet> [lang]
//!                                              streamed + local STT per chunk
//!
//! Prints `PROGRESS` lines (percent, audio position, elapsed, ETA, peak RSS)
//! and a final `RESULT` line. Peak RSS is the process's own ru_maxrss.

use dimmy_lib::file_stream::{for_each_chunk, FileAudioStream};
use std::time::Instant;

// libc is a unix-only dependency of the core; on Windows the bench reports 0
// and the peak is read from Task Manager or Get-Process.
#[cfg(not(unix))]
fn peak_rss_mb() -> f64 {
    0.0
}

#[cfg(unix)]
fn peak_rss_mb() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    // Bytes on macOS, kilobytes on Linux.
    if cfg!(target_os = "macos") {
        ru.ru_maxrss as f64 / 1_048_576.0
    } else {
        ru.ru_maxrss as f64 / 1024.0
    }
}

/// The split as it was: recompute the window RMS at every position, and
/// copy every chunk out before the first is used.
fn old_split(samples: &[f32], sr: u32, max: usize) -> Vec<Vec<f32>> {
    let total = samples.len();
    if total <= max {
        return vec![samples.to_vec()];
    }
    let w = ((sr as f64 * 0.3) as usize).max(1);
    let mut chunks = Vec::new();
    let mut offset = 0;
    while offset < total {
        if total - offset <= max {
            chunks.push(samples[offset..].to_vec());
            break;
        }
        let chunk_end = offset + max;
        let search_start = offset + (max - max / 4);
        let mut split_at = None;
        if chunk_end > w + offset {
            let scan_from = search_start.max(offset + w);
            for pos in (scan_from..chunk_end).rev() {
                let win = &samples[pos - w..pos];
                let rms = (win.iter().map(|s| s * s).sum::<f32>() / win.len() as f32).sqrt();
                if rms < 0.01 {
                    split_at = Some(pos);
                    break;
                }
            }
        }
        let end = split_at.unwrap_or(chunk_end).max(offset + 1);
        chunks.push(samples[offset..end].to_vec());
        offset = end;
    }
    chunks
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: file_load_bench old|new|transcribe <file> [model] [lang]";
    let mode = args.get(1).expect(usage).as_str();
    let path = args.get(2).expect(usage);
    let t0 = Instant::now();
    match mode {
        "old" => {
            let mono_t = Instant::now();
            let (mono, rate) = dimmy_lib::ffi::decode_via_symphonia(path).expect("decode");
            println!(
                "PHASE decode {:.1}s, {} samples @ {} Hz, peak {:.0} MB",
                mono_t.elapsed().as_secs_f64(),
                mono.len(),
                rate,
                peak_rss_mb()
            );
            let pp_t = Instant::now();
            let processed = dimmy_lib::preprocess::process_buffer_for_file_load(&mono, rate);
            drop(mono);
            println!(
                "PHASE highpass {:.1}s, peak {:.0} MB",
                pp_t.elapsed().as_secs_f64(),
                peak_rss_mb()
            );
            let sp_t = Instant::now();
            let chunks = old_split(&processed, rate, 30 * rate as usize);
            println!(
                "PHASE split {:.1}s, {} chunks, peak {:.0} MB",
                sp_t.elapsed().as_secs_f64(),
                chunks.len(),
                peak_rss_mb()
            );
            println!(
                "RESULT old chunks={} total={:.1}s peak_rss={:.0}MB",
                chunks.len(),
                t0.elapsed().as_secs_f64(),
                peak_rss_mb()
            );
        }
        "new" | "transcribe" => {
            let model = args.get(3).cloned().unwrap_or_default();
            let lang = args.get(4).cloned().unwrap_or_else(|| "en".to_string());
            let transcribe = mode == "transcribe";
            assert!(!transcribe || !model.is_empty(), "{usage}");
            if model == "parakeet" && !dimmy_lib::parakeet::bundle_present() {
                println!("DOWNLOADING parakeet bundle");
                tokio::runtime::Runtime::new()
                    .expect("runtime")
                    .block_on(dimmy_lib::parakeet::download_active_bundle(|_, _| {}))
                    .expect("parakeet download");
            }
            let mut stream = FileAudioStream::open(path).expect("open");
            let rate = stream.sample_rate;
            let declared = stream
                .total_frames
                .map(|f| f as f64 / rate as f64)
                .unwrap_or(0.0);
            println!(
                "START {} Hz, declared {:.0}s ({:.2} h)",
                rate,
                declared,
                declared / 3600.0
            );
            let mut n = 0usize;
            let mut words = 0usize;
            let mut text_out = String::new();
            let mut last_pct = -1i64;
            let decoded = for_each_chunk(&mut stream, 30 * rate as usize, |c, s| {
                n += 1;
                if transcribe {
                    let r = if model == "parakeet" {
                        dimmy_lib::transcribe::transcribe_audio_local_parakeet_with_word_ts(
                            &c.audio,
                        )
                        .map(|(t, _)| t)
                    } else {
                        dimmy_lib::transcribe::transcribe_audio_local(&c.audio, &lang, &model, "")
                    };
                    match r {
                        Ok(t) => {
                            words += t.split_whitespace().count();
                            text_out.push_str(t.trim());
                            text_out.push('\n');
                        }
                        Err(e) => println!("CHUNK {} failed: {}", n, e),
                    }
                }
                let pct = s.progress() * 100.0;
                let step = if transcribe { 1 } else { 5 };
                if (pct as i64) / step > last_pct / step || pct >= 100.0 {
                    last_pct = pct as i64;
                    let el = t0.elapsed().as_secs_f64();
                    let pos = (c.start_sample + c.audio.samples.len()) as f64 / rate as f64;
                    let eta = if pct > 0.0 {
                        el * (100.0 - pct) / pct
                    } else {
                        0.0
                    };
                    println!(
                        "PROGRESS {:5.1}% audio {:6.0}s chunk {:4} elapsed {:6.1}s eta {:6.1}s words {:6} peak_rss {:5.0}MB",
                        pct, pos, n, el, eta, words, peak_rss_mb()
                    );
                }
                true
            });
            if transcribe {
                let _ = std::fs::write(format!("{path}.transcript.txt"), &text_out);
            }
            println!(
                "RESULT {} chunks={} audio={:.0}s total={:.1}s words={} peak_rss={:.0}MB",
                mode,
                n,
                decoded as f64 / rate as f64,
                t0.elapsed().as_secs_f64(),
                words,
                peak_rss_mb()
            );
        }
        _ => panic!("{usage}"),
    }
}
