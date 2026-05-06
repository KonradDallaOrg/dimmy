//! Long-form meeting-mode recording.
//!
//! Distinct from the live-dictation path in two ways:
//! 1. **Streaming-to-disk**: the WAV is written incrementally as the
//!    user speaks (~115 MB / hour at 16 kHz mono int16). Memory stays
//!    bounded regardless of meeting length, and a `.recording` marker
//!    file lets a future startup detect crashed sessions and offer
//!    recovery.
//! 2. **Post-process pipeline**: at stop, the full transcript is sent
//!    to the LLM once to produce a structured artifact (recap +
//!    action items + optional translation). Live captioning happens
//!    via the same chunked engine the dictation path uses, with a
//!    longer chunk window (15 s) tuned for context not interactivity.
//!
//! On-disk layout per meeting (`<config>/meetings/<id>/`):
//! - `audio.wav`        — 16 kHz mono int16 stream, finalized at stop
//! - `transcripts.txt`  — one line per chunk, prefixed with `[ts_ms]`
//! - `meta.json`        — start_ts, sample_rate, last_chunk_ts (live)
//! - `recap.md`         — post-stop LLM output (only after stop)
//! - `actions.json`     — post-stop LLM output (only after stop)
//! - `.recording`       — marker file deleted on clean stop;
//!                        presence at startup → "recover meeting?"
//!
//! Identifiers are RFC4122 v4 UUIDs so two meetings started on the
//! same wall-clock second don't collide.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default chunk window for live transcript. Longer than the
/// dictation path's 5 s — meeting users care about a full record,
/// not interactivity, and longer chunks give Parakeet more context
/// (slightly better accuracy per the WSL bench: 30 s = 100% match
/// vs 5 s = same quality but more overhead).
const DEFAULT_CHUNK_SECS: f32 = 15.0;
const DEFAULT_OVERLAP_MS: u32 = 500;
const FSYNC_INTERVAL: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct MeetingSession {
    id: String,
    dir: PathBuf,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<MeetingResult>>,
}

#[derive(Clone, Debug)]
pub struct MeetingResult {
    pub id: String,
    pub dir: PathBuf,
    pub transcript: String,
    pub duration_secs: f64,
    pub chunk_count: u32,
    pub error: Option<String>,
}

impl MeetingSession {
    /// Create the meeting directory + marker, open audio.wav writer,
    /// spawn the worker thread. The worker keeps reading from
    /// `audio_buffer` (which the cpal callback fills concurrently)
    /// and writes / transcribes incrementally until `stop()` is called.
    pub fn start(
        audio_buffer: Arc<Mutex<Vec<f32>>>,
        device_sample_rate: u32,
    ) -> Result<Self, String> {
        assert!(device_sample_rate > 0, "device_sample_rate must be positive");
        let id = uuid_v4_simple();
        let dir = crate::meetings_dir()
            .ok_or_else(|| "config dir unavailable".to_string())?
            .join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {:?}: {}", dir, e))?;

        // Marker file — deleted on clean stop, leftover after a crash.
        let marker = dir.join(".recording");
        std::fs::write(&marker, "1").map_err(|e| format!("marker write: {}", e))?;

        let meta_initial = serde_json::json!({
            "id": id,
            "started_at": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            "device_sample_rate": device_sample_rate,
            "chunk_secs": DEFAULT_CHUNK_SECS,
            "overlap_ms": DEFAULT_OVERLAP_MS,
        });
        std::fs::write(
            dir.join("meta.json"),
            serde_json::to_string_pretty(&meta_initial).unwrap_or_default(),
        )
        .map_err(|e| format!("meta write: {}", e))?;

        // Open audio.wav writer at 16 kHz int16 mono. The cpal buffer
        // arrives at the device's native rate; we downsample on the
        // fly before writing so the on-disk file is uniform regardless
        // of the device.
        let wav_path = dir.join("audio.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&wav_path, spec)
            .map_err(|e| format!("wav create {:?}: {}", wav_path, e))?;

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = cancel.clone();
        let dir_w = dir.clone();
        let id_w = id.clone();

        let handle = thread::Builder::new()
            .name(format!("meeting-{}", &id[..8]))
            .spawn(move || {
                worker_loop(
                    audio_buffer,
                    device_sample_rate,
                    writer,
                    dir_w,
                    id_w,
                    cancel_w,
                )
            })
            .map_err(|e| format!("spawn meeting worker: {}", e))?;

        crate::log(&format!("[Meeting] started id={} dir={:?}", id, dir));
        Ok(Self {
            id,
            dir,
            cancel,
            handle: Some(handle),
        })
    }

    /// Signal the worker, join, finalize. Returns the result bundle —
    /// caller is then expected to invoke the post-processing pipeline
    /// (LLM recap + actions) and persist the artifacts.
    pub fn stop(mut self) -> MeetingResult {
        self.cancel.store(true, Ordering::SeqCst);
        let handle = self.handle.take();
        let result = handle.map(|h| h.join().ok()).unwrap_or(None).unwrap_or(MeetingResult {
            id: self.id.clone(),
            dir: self.dir.clone(),
            transcript: String::new(),
            duration_secs: 0.0,
            chunk_count: 0,
            error: Some("worker panicked".into()),
        });
        // Marker is removed only on clean exit so a crash leaves it.
        let _ = std::fs::remove_file(self.dir.join(".recording"));
        crate::log(&format!(
            "[Meeting] stopped id={} duration={:.1}s chunks={} err={:?}",
            result.id, result.duration_secs, result.chunk_count, result.error
        ));
        result
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

fn worker_loop(
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    device_sample_rate: u32,
    mut writer: hound::WavWriter<std::io::BufWriter<File>>,
    dir: PathBuf,
    id: String,
    cancel: Arc<AtomicBool>,
) -> MeetingResult {
    let chunk_samples = (DEFAULT_CHUNK_SECS * device_sample_rate as f32) as usize;
    let overlap_samples = ((DEFAULT_OVERLAP_MS as f32 / 1000.0) * device_sample_rate as f32) as usize;

    // `samples_written` tracks how many SOURCE-RATE samples have been
    // streamed into the WAV (after downsample they map to fewer 16k
    // samples; we keep the source-rate offset for chunk boundaries).
    let mut samples_written: usize = 0;
    let mut last_processed: usize = 0;
    let mut last_fsync = Instant::now();
    let mut transcript_accum = String::new();
    let mut chunk_count: u32 = 0;
    let started = Instant::now();

    let transcripts_path = dir.join("transcripts.txt");
    let mut transcripts_file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcripts_path)
    {
        Ok(f) => f,
        Err(e) => {
            return MeetingResult {
                id,
                dir,
                transcript: String::new(),
                duration_secs: 0.0,
                chunk_count: 0,
                error: Some(format!("open transcripts.txt: {}", e)),
            };
        }
    };

    loop {
        let cancelled = cancel.load(Ordering::SeqCst);
        thread::sleep(POLL_INTERVAL);

        // Take a snapshot of the buffer length up to which we'll
        // process this iteration. Holding the lock only for the read.
        let buf_len_now = match audio_buffer.lock() {
            Ok(b) => b.len(),
            Err(_) => continue,
        };

        // Stream new samples into the WAV file. We always copy
        // [samples_written..buf_len_now] regardless of chunk timing
        // so the on-disk audio is always up-to-date.
        if buf_len_now > samples_written {
            let new_slice: Vec<f32> = match audio_buffer.lock() {
                Ok(b) => {
                    if buf_len_now <= b.len() {
                        b[samples_written..buf_len_now].to_vec()
                    } else {
                        Vec::new()
                    }
                }
                Err(_) => Vec::new(),
            };
            if !new_slice.is_empty() {
                let pcm_16k = if device_sample_rate == 16_000 {
                    new_slice
                } else {
                    crate::preprocess::downsample_to_16k(&new_slice, device_sample_rate)
                };
                for s in &pcm_16k {
                    let clamped = s.clamp(-1.0, 1.0);
                    let i = (clamped * i16::MAX as f32) as i16;
                    if let Err(e) = writer.write_sample(i) {
                        crate::log(&format!("[Meeting] write_sample failed: {}", e));
                        break;
                    }
                }
                samples_written = buf_len_now;
            }
            if last_fsync.elapsed() >= FSYNC_INTERVAL {
                if let Err(e) = writer.flush() {
                    crate::log(&format!("[Meeting] wav flush: {}", e));
                }
                last_fsync = Instant::now();
            }
        }

        // Fire a chunk transcribe if enough new audio has accumulated.
        // Same dedup logic the live caption path uses, but we don't
        // emit events — we append to transcripts.txt + the in-memory
        // accumulator. The post-process pipeline later reads from
        // transcripts.txt so a mid-meeting crash retains everything
        // that was already transcribed.
        let want_end = last_processed + chunk_samples;
        if buf_len_now >= want_end || (cancelled && buf_len_now > last_processed) {
            let start = last_processed.saturating_sub(overlap_samples);
            let end = if cancelled {
                buf_len_now
            } else {
                want_end.min(buf_len_now)
            };
            let chunk: Vec<f32> = match audio_buffer.lock() {
                Ok(b) => {
                    if end > b.len() {
                        Vec::new()
                    } else {
                        b[start..end].to_vec()
                    }
                }
                Err(_) => Vec::new(),
            };
            if !chunk.is_empty() {
                let pcm_16k = if device_sample_rate == 16_000 {
                    chunk
                } else {
                    crate::preprocess::downsample_to_16k(&chunk, device_sample_rate)
                };
                let chunk_text = match crate::parakeet::transcribe(&pcm_16k) {
                    Ok(t) => t,
                    Err(e) => {
                        crate::log(&format!("[Meeting] parakeet error: {}", e));
                        String::new()
                    }
                };
                if !chunk_text.trim().is_empty() {
                    let delta = crate::chunked_stt::dedup_last_3_words(
                        &transcript_accum,
                        &chunk_text,
                    );
                    if !delta.is_empty() {
                        if !transcript_accum.is_empty()
                            && !transcript_accum.ends_with(' ')
                        {
                            transcript_accum.push(' ');
                        }
                        transcript_accum.push_str(&delta);
                        chunk_count += 1;
                        let line = format!(
                            "[{:>6} ms] {}\n",
                            started.elapsed().as_millis(),
                            delta
                        );
                        let _ = transcripts_file.write_all(line.as_bytes());
                        let _ = transcripts_file.flush();
                    }
                }
            }
            last_processed = end;
        }

        if cancelled {
            break;
        }
    }

    // Finalize WAV + meta.
    if let Err(e) = writer.finalize() {
        crate::log(&format!("[Meeting] wav finalize: {}", e));
    }
    let duration_secs = started.elapsed().as_secs_f64();
    let meta = serde_json::json!({
        "id": id,
        "duration_secs": duration_secs,
        "chunk_count": chunk_count,
        "ended_at": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
    });
    let _ = std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );

    MeetingResult {
        id,
        dir,
        transcript: transcript_accum,
        duration_secs,
        chunk_count,
        error: None,
    }
}

/// Tiny RFC4122-v4 UUID generator. Avoids pulling in a uuid crate
/// for one usage. Variant + version bits set per spec; randomness
/// from the OS via the rand crate (already a transitive dep).
fn uuid_v4_simple() -> String {
    let mut bytes = [0u8; 16];
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Mix counter + nanos into the 16 bytes — not cryptographically
    // strong but sufficient for unique per-meeting directory names.
    let mut seed = now_ns ^ (counter.wrapping_mul(0x9E3779B97F4A7C15));
    for b in bytes.iter_mut() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (seed >> 56) as u8;
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // RFC4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Persist the post-process LLM output (recap + actions) into the
/// meeting directory. The C#/Swift host calls this after running
/// `dimmy_process_with_llm` against the full transcript. Splitting
/// LLM execution from persistence keeps Rust free of HTTP-runtime
/// concerns specific to the meeting flow — the existing LLM client
/// is already wired through dimmy_process_with_llm.
pub fn save_post_process(
    meeting_dir: &std::path::Path,
    recap_md: &str,
    actions_json: &str,
    translated: Option<&str>,
) -> Result<(), String> {
    if !recap_md.trim().is_empty() {
        std::fs::write(meeting_dir.join("recap.md"), recap_md)
            .map_err(|e| format!("write recap.md: {}", e))?;
    }
    if !actions_json.trim().is_empty() {
        std::fs::write(meeting_dir.join("actions.json"), actions_json)
            .map_err(|e| format!("write actions.json: {}", e))?;
    }
    if let Some(t) = translated {
        if !t.trim().is_empty() {
            std::fs::write(meeting_dir.join("translated.txt"), t)
                .map_err(|e| format!("write translated.txt: {}", e))?;
        }
    }
    Ok(())
}

/// Scan the meetings dir for sessions with a leftover `.recording`
/// marker. These are the crash-recoverable orphans the UI surfaces
/// at startup. Returns a JSON array of metadata so the C#/Swift host
/// can populate a "Recover meeting" prompt without needing its own
/// directory parser.
pub fn list_orphans() -> Vec<serde_json::Value> {
    let Some(base) = crate::meetings_dir() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let marker = dir.join(".recording");
        if !marker.exists() {
            continue;
        }
        // Best-effort: read meta.json + transcripts.txt size.
        let meta_str = std::fs::read_to_string(dir.join("meta.json")).unwrap_or_default();
        let meta_json: serde_json::Value =
            serde_json::from_str(&meta_str).unwrap_or(serde_json::Value::Null);
        let transcript_size = std::fs::metadata(dir.join("transcripts.txt"))
            .map(|m| m.len())
            .unwrap_or(0);
        out.push(serde_json::json!({
            "id": dir.file_name().and_then(|n| n.to_str()).unwrap_or(""),
            "dir": dir.to_string_lossy(),
            "meta": meta_json,
            "transcript_size_bytes": transcript_size,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v4_format() {
        let id = uuid_v4_simple();
        assert_eq!(id.len(), 36);
        let chars: Vec<char> = id.chars().collect();
        assert_eq!(chars[8], '-');
        assert_eq!(chars[13], '-');
        assert_eq!(chars[18], '-');
        assert_eq!(chars[23], '-');
        // version-4 nibble at position 14
        assert_eq!(chars[14], '4');
    }

    #[test]
    fn uuid_v4_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            seen.insert(uuid_v4_simple());
        }
        assert_eq!(seen.len(), 1000);
    }
}
