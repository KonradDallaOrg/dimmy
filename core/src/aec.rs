//! Acoustic echo cancellation worker for Mix-mode meeting capture.
//!
//! When the user records mic + system loopback together (Mix mode), the
//! same audio that's playing through their speakers gets captured
//! TWICE: once directly via WASAPI loopback, and once acoustically by
//! the mic (with ~5-30ms propagation delay + room reverb). Summing the
//! two streams produces a textbook echo + transient clamp clicks. This
//! module wires the WebRTC AEC3 algorithm (via the pure-Rust `aec3`
//! crate) so the loopback is used as a `render` reference signal that
//! gets subtracted from the mic `capture` signal — what reaches the
//! mix is mic - speaker_echo.
//!
//! Pipeline operates on 10 ms frames at 48 kHz mono (480 samples). cpal
//! callbacks deliver arbitrary-size chunks so we use small ring buffers
//! per stream and the worker drains 480-sample frames in lockstep.
//!
//! When Mix mode is NOT active the rings stay empty and the worker
//! sleeps in 5 ms ticks — zero CPU when idle.

use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const FRAME_SAMPLES: usize = 480; // 10 ms @ 48 kHz mono
const POLL_SLEEP: Duration = Duration::from_millis(5);

/// Per-stream ring buffers. The cpal callbacks PUSH samples; the AEC
/// worker DRAINS 480-sample frames. Capped at MAX_RING_SAMPLES to stop
/// pathological growth if one callback stalls.
const MAX_RING_SAMPLES: usize = 48_000; // 1 s headroom

/// Push samples to a ring buffer with overflow guard. If the ring
/// exceeds MAX_RING_SAMPLES the OLDEST samples are dropped — better
/// than unbounded memory growth, and AEC will resync via its delay
/// estimator when the new tail comes in.
pub fn push_to_ring(ring: &Arc<Mutex<Vec<f32>>>, samples: &[f32]) {
    if let Ok(mut r) = ring.lock() {
        r.extend_from_slice(samples);
        if r.len() > MAX_RING_SAMPLES {
            let drop = r.len() - MAX_RING_SAMPLES;
            r.drain(..drop);
        }
    }
}

/// Spawn the AEC worker thread. Runs forever (until process exit).
/// While idle (no Mix recording) the rings stay empty and the loop
/// sleeps in 5 ms ticks. When samples arrive from both rings, drains
/// 480-sample frames in lockstep, runs them through the AEC pipeline,
/// and pushes the cleaned mic output to `output_buffer` — the same
/// `audio_buffer` the rest of the codebase reads from.
///
/// `shutdown` is checked at every poll and lets the host (test harness
/// / shutdown sequence) terminate the loop deterministically.
pub fn spawn_aec_thread(
    mic_ring: Arc<Mutex<Vec<f32>>>,
    ref_ring: Arc<Mutex<Vec<f32>>>,
    output_buffer: Arc<Mutex<Vec<f32>>>,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("dimmy-aec".to_string())
        .spawn(move || {
            run(mic_ring, ref_ring, output_buffer, shutdown);
        })
        .expect("AEC thread spawn must succeed")
}

fn run(
    mic_ring: Arc<Mutex<Vec<f32>>>,
    ref_ring: Arc<Mutex<Vec<f32>>>,
    output_buffer: Arc<Mutex<Vec<f32>>>,
    shutdown: Arc<AtomicBool>,
) {
    // Build the AEC pipeline. `linear::builder` produces a graph
    // wired as: render+capture -> HPF -> AEC3 -> NS -> AGC2.
    let format = AudioFormat::ten_ms(48_000, 1);
    let mut pipeline = match linear::builder(format, format)
        .initial_delay_ms(120) // typical speaker→mic acoustic latency
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            crate::log(&format!(
                "[AEC] pipeline build failed: {} — AEC disabled",
                e
            ));
            // Without AEC we still need to keep the buffer fed in Mix
            // mode. Fall back to passthrough: drain mic frames and
            // forward them unchanged.
            run_passthrough(mic_ring, ref_ring, output_buffer, shutdown);
            return;
        }
    };
    crate::log("[AEC] pipeline ready (10ms @ 48kHz mono, HPF+AEC3+NS+AGC2)");

    // Noise suppressor stacked upstream of AEC. The backend is
    // chosen at compile time via the `local-dfn` cargo feature:
    //   - ON (release default): DeepFilterNet3 via the upstream
    //     `deep_filter` crate. SOTA on PESQ/STOI, ~3-5 ms / frame,
    //     ~7 MB model embedded.
    //   - OFF (fallback / dev / no-DFN3 builds): nnnoiseless
    //     (RNNoise port). ~1 ms / frame, ~85 KB model embedded,
    //     less aggressive on non-stationary noise but always
    //     available without the patched libDF dep.
    // Either way, `try_init_*` returns None when the runtime toggle
    // is off, in which case the mic frame goes straight into AEC
    // capture unchanged.
    #[cfg(feature = "local-dfn")]
    let mut dfn = crate::dfn3::Dfn3Processor::try_init_runtime();
    #[cfg(not(feature = "local-dfn"))]
    let mut dfn = crate::dfn::DfnProcessor::try_init();
    if dfn.is_some() {
        crate::log("[AEC] noise suppressor active on mic capture (NN -> AEC3)");
    }

    let mut render_frame = vec![0.0f32; FRAME_SAMPLES];
    let mut capture_frame = vec![0.0f32; FRAME_SAMPLES];
    let mut dfn_frame = vec![0.0f32; FRAME_SAMPLES];
    let mut output_frame = vec![0.0f32; FRAME_SAMPLES];

    // Track whether we've ever seen a ref frame. Required for the
    // "always-on capture" architecture: when the loopback stream fails
    // to open or the system isn't producing any audio, ref_ring stays
    // empty forever. Blocking the AEC loop on that = mic_ring fills
    // unbounded and audio_buffer never gets populated, so the pill /
    // dictation captures NOTHING. Instead: process each mic frame
    // immediately, padding the ref with zeros if no ref samples have
    // arrived in time. AEC3 with a zero ref ≈ mic passthrough with the
    // pre-DFN/HPF/AGC2 chain still in effect.
    let mut ref_seen_ever = false;
    let mut last_ref_log: Option<std::time::Instant> = None;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            crate::log("[AEC] shutdown signalled — worker exiting");
            return;
        }

        // Drain mic at its own rate. We never block on ref availability.
        let (mic_have, ref_have) = current_lengths(&mic_ring, &ref_ring);
        if mic_have < FRAME_SAMPLES {
            thread::sleep(POLL_SLEEP);
            continue;
        }

        // Take ref frame if we have one; else use zeros. This lets the
        // pipeline keep producing cleaned mic samples even when the
        // loopback delivers no data (e.g. nothing playing on speakers,
        // no default-output device, or BT routed away from loopback).
        let mut ref_frame_zeroed = false;
        if ref_have >= FRAME_SAMPLES {
            if !drain_frame(&ref_ring, &mut render_frame) {
                thread::sleep(POLL_SLEEP);
                continue;
            }
            if !ref_seen_ever {
                ref_seen_ever = true;
                crate::log("[AEC] first ref frame received — full echo cancellation active");
            }
        } else {
            // Zero-fill render frame.
            for s in render_frame.iter_mut() {
                *s = 0.0;
            }
            ref_frame_zeroed = true;
            // Periodic info log so prolonged ref starvation is visible
            // without spamming. Only emit once every 30 s. NOTE: this must
            // fire even after the first ref frame arrived — the original
            // `should_log && !ref_seen_ever` condition silenced it for the
            // rest of the session, so a loopback device dying MID-meeting
            // (BT speaker disconnect) starved the ref ring with zero trace
            // in dimmy.log (audit 2026-07-02).
            let now = std::time::Instant::now();
            let should_log = match last_ref_log {
                None => true,
                Some(t) => now.duration_since(t).as_secs() >= 30,
            };
            if should_log {
                // Neutral wording for the mid-session case: an idle ref ring
                // can be a dead loopback device OR simply nothing playing
                // (WASAPI loopback goes quiet with no render streams).
                crate::log(if ref_seen_ever {
                    "[AEC] ref ring empty (no loopback frames recently) — echo cancellation idle, mic passthrough"
                } else {
                    "[AEC] ref ring empty — running mic-only (no echo cancellation reference)"
                });
                last_ref_log = Some(now);
            }
        }
        let _ = ref_frame_zeroed; // info-only flag, may be useful later
        if !drain_frame(&mic_ring, &mut capture_frame) {
            thread::sleep(POLL_SLEEP);
            continue;
        }

        // Stage 1: NN noise suppression on the mic capture (if loaded).
        // Output overwrites capture_frame so the AEC sees the cleaned
        // signal as its near-end. The two backends have slightly
        // different return signatures (DFN3 → Result; nnnoiseless →
        // ()), so we dispatch on the cargo feature.
        if let Some(ref mut p) = dfn {
            #[cfg(feature = "local-dfn")]
            {
                if let Err(e) = p.process_frame(&capture_frame, &mut dfn_frame) {
                    crate::log(&format!(
                        "[AEC] DFN3 process_frame failed: {} — bypassing this frame",
                        e
                    ));
                    dfn_frame.copy_from_slice(&capture_frame);
                }
            }
            #[cfg(not(feature = "local-dfn"))]
            {
                p.process_frame(&capture_frame, &mut dfn_frame);
            }
            capture_frame.copy_from_slice(&dfn_frame);
        }

        // Stage 2: AEC. handle_render_frame stores reference; process_capture_frame
        // does the actual echo cancellation against the recent render history.
        if let Err(e) = pipeline.handle_render_frame(&render_frame) {
            crate::log(&format!("[AEC] handle_render_frame: {} (skipping)", e));
            continue;
        }
        if let Err(e) = pipeline.process_capture_frame(&capture_frame, &mut output_frame) {
            crate::log(&format!("[AEC] process_capture_frame: {} (skipping)", e));
            continue;
        }

        // Push cleaned mic to the audio buffer — same Vec<f32> the
        // meeting worker / amplitude probe / live waveform read from.
        // Paused meeting: skip the append (gap-skip discards these at
        // resume; keeping them only grows RAM).
        if !crate::audio::meeting_capture_gated() {
            if let Ok(mut buf) = output_buffer.lock() {
                buf.extend_from_slice(&output_frame);
            }
        }
    }
}

/// Fallback if the AEC pipeline failed to construct. Just forwards
/// mic frames untouched and drops ref frames so meeting capture
/// continues to work, just without echo cancellation.
fn run_passthrough(
    mic_ring: Arc<Mutex<Vec<f32>>>,
    ref_ring: Arc<Mutex<Vec<f32>>>,
    output_buffer: Arc<Mutex<Vec<f32>>>,
    shutdown: Arc<AtomicBool>,
) {
    let mut frame = vec![0.0f32; FRAME_SAMPLES];
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        let mic_have = mic_ring.lock().map(|r| r.len()).unwrap_or(0);
        if mic_have < FRAME_SAMPLES {
            thread::sleep(POLL_SLEEP);
            continue;
        }
        if !drain_frame(&mic_ring, &mut frame) {
            continue;
        }
        // Drop a matching ref frame to keep the rings from ballooning.
        if let Ok(mut r) = ref_ring.lock() {
            let drop = FRAME_SAMPLES.min(r.len());
            r.drain(..drop);
        }
        // Paused meeting: skip append (see run_aec twin).
        if !crate::audio::meeting_capture_gated() {
            if let Ok(mut buf) = output_buffer.lock() {
                buf.extend_from_slice(&frame);
            }
        }
    }
}

fn current_lengths(mic: &Arc<Mutex<Vec<f32>>>, rref: &Arc<Mutex<Vec<f32>>>) -> (usize, usize) {
    let m = mic.lock().map(|r| r.len()).unwrap_or(0);
    let r = rref.lock().map(|r| r.len()).unwrap_or(0);
    (m, r)
}

fn drain_frame(ring: &Arc<Mutex<Vec<f32>>>, dest: &mut [f32]) -> bool {
    if let Ok(mut r) = ring.lock() {
        if r.len() < dest.len() {
            return false;
        }
        dest.copy_from_slice(&r[..dest.len()]);
        r.drain(..dest.len());
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a sine-ish synthetic mic signal at 48 kHz so the
    /// AEC worker has finite, in-range samples to chew on.
    fn synth_mic(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.3)
            .collect()
    }

    #[test]
    fn push_to_ring_caps_at_max() {
        let ring: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        // Push 2 × MAX_RING_SAMPLES; ring should clamp to the cap and
        // keep the most-recent tail (drops the oldest excess).
        let chunk = vec![0.5f32; MAX_RING_SAMPLES];
        push_to_ring(&ring, &chunk);
        push_to_ring(&ring, &chunk);
        let len = ring.lock().unwrap().len();
        assert_eq!(len, MAX_RING_SAMPLES, "ring must not grow beyond the cap");
    }

    #[test]
    fn drain_frame_returns_false_when_short() {
        let ring: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(vec![0.5f32; 100]));
        let mut dst = vec![0.0f32; FRAME_SAMPLES];
        assert!(!drain_frame(&ring, &mut dst));
        // Ring is untouched.
        assert_eq!(ring.lock().unwrap().len(), 100);
    }

    #[test]
    fn drain_frame_consumes_exact_size() {
        let ring: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(vec![0.5f32; FRAME_SAMPLES * 3]));
        let mut dst = vec![0.0f32; FRAME_SAMPLES];
        assert!(drain_frame(&ring, &mut dst));
        assert_eq!(ring.lock().unwrap().len(), FRAME_SAMPLES * 2);
        assert!(dst.iter().all(|&s| s == 0.5));
    }

    /// Regression: AEC worker must NOT block when ref ring is empty.
    /// Before commit 3eddac3 the worker required BOTH rings to have a
    /// frame, so a configuration where loopback never delivers (no
    /// default output, BT routed away, silent system) would deadlock
    /// the mic stream — output_buffer stayed empty forever, dictation
    /// captured nothing. After the fix, the worker zero-pads the ref
    /// frame and processes mic immediately, AEC3 with zero ref ≈
    /// passthrough.
    #[test]
    fn worker_processes_mic_when_ref_ring_empty() {
        let mic: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let rref: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let out: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let h = spawn_aec_thread(mic.clone(), rref.clone(), out.clone(), shutdown.clone());

        // Push 5 frames worth of mic samples; ref stays empty.
        push_to_ring(&mic, &synth_mic(FRAME_SAMPLES * 5));

        // Give the worker time to drain + process. 200 ms is generous
        // for 5 frames at 5 ms poll + AEC3 inference per frame.
        thread::sleep(Duration::from_millis(300));

        let produced = out.lock().unwrap().len();
        assert!(
            produced >= FRAME_SAMPLES,
            "worker should have produced at least one frame even with empty ref ring (produced={})",
            produced
        );
        assert!(
            out.lock().unwrap().iter().all(|s| s.is_finite()),
            "all output samples must be finite"
        );

        shutdown.store(true, Ordering::SeqCst);
        let _ = h.join();
    }

    /// Symmetric case: ref present, mic present. Worker drains lockstep
    /// and produces cleaned mic output.
    #[test]
    fn worker_processes_mic_with_ref_present() {
        let mic: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let rref: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let out: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let h = spawn_aec_thread(mic.clone(), rref.clone(), out.clone(), shutdown.clone());

        // Push 5 frames each. Ref is a slightly-delayed copy of mic so
        // AEC3 has a sensible echo correlation to learn.
        let mic_samples = synth_mic(FRAME_SAMPLES * 5);
        push_to_ring(&mic, &mic_samples);
        push_to_ring(&rref, &mic_samples);

        thread::sleep(Duration::from_millis(300));

        let produced = out.lock().unwrap().len();
        assert!(
            produced >= FRAME_SAMPLES,
            "worker should produce frames when both rings are filled (produced={})",
            produced
        );

        shutdown.store(true, Ordering::SeqCst);
        let _ = h.join();
    }

    /// Sanity: worker exits promptly on shutdown signal even if rings
    /// are completely empty. Catches a regression where a future
    /// refactor might block on a long sleep without checking shutdown.
    #[test]
    fn worker_honours_shutdown_signal() {
        let mic: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let rref: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let out: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let h = spawn_aec_thread(mic.clone(), rref.clone(), out.clone(), shutdown.clone());
        // Brief settle so the thread enters its main loop.
        thread::sleep(Duration::from_millis(50));
        shutdown.store(true, Ordering::SeqCst);

        // Should join within a few poll intervals — POLL_SLEEP is 5 ms,
        // 200 ms is many orders of magnitude above the worst case.
        let join_deadline = std::time::Instant::now() + Duration::from_millis(500);
        loop {
            if h.is_finished() {
                break;
            }
            if std::time::Instant::now() > join_deadline {
                panic!("AEC worker did not exit within 500 ms of shutdown");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = h.join();
    }
}
