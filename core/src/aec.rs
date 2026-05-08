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

    // DeepFilterNet — optional ML noise suppressor stacked upstream of
    // AEC. try_init returns None if the feature is off OR the model
    // bundle isn't present, in which case the mic frame goes straight
    // into AEC capture unchanged.
    let mut dfn = crate::dfn::DfnProcessor::try_init();
    if dfn.is_some() {
        crate::log("[AEC] DeepFilterNet active on mic capture (DFN -> AEC3)");
    }

    let mut render_frame = vec![0.0f32; FRAME_SAMPLES];
    let mut capture_frame = vec![0.0f32; FRAME_SAMPLES];
    let mut dfn_frame = vec![0.0f32; FRAME_SAMPLES];
    let mut output_frame = vec![0.0f32; FRAME_SAMPLES];

    loop {
        if shutdown.load(Ordering::Relaxed) {
            crate::log("[AEC] shutdown signalled — worker exiting");
            return;
        }

        // Lockstep drain: only proceed when BOTH rings have a full
        // frame. Otherwise sleep and retry.
        let (mic_have, ref_have) = current_lengths(&mic_ring, &ref_ring);
        if mic_have < FRAME_SAMPLES || ref_have < FRAME_SAMPLES {
            thread::sleep(POLL_SLEEP);
            continue;
        }

        // Drain ref first because aec3 expects render-then-capture
        // for a given time window. (The internal delay estimator
        // tolerates moderate misalignment but the convention helps.)
        if !drain_frame(&ref_ring, &mut render_frame) {
            thread::sleep(POLL_SLEEP);
            continue;
        }
        if !drain_frame(&mic_ring, &mut capture_frame) {
            thread::sleep(POLL_SLEEP);
            continue;
        }

        // Stage 1: DFN noise suppression on the mic capture (if loaded).
        // Output overwrites capture_frame so the AEC sees the cleaned
        // signal as its near-end.
        if let Some(ref mut p) = dfn {
            p.process_frame(&capture_frame, &mut dfn_frame);
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
        if let Ok(mut buf) = output_buffer.lock() {
            buf.extend_from_slice(&output_frame);
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
        if let Ok(mut buf) = output_buffer.lock() {
            buf.extend_from_slice(&frame);
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
