//! A meeting's two audio streams, from files instead of hardware.
//!
//! Local test builds only (`sim-audio`, never in a release feature line). It
//! exists to reproduce, on a Mac with no microphone and without the Core Audio
//! tap's permission prompt, what a real meeting puts through the core: a mic
//! and a system track, each arriving in 10 ms blocks at the pace the hardware
//! would deliver them.
//!
//! Both enter where the real ones do, so everything after is production code:
//! the mic block lands in the buffer the cpal callback writes (the AEC mic ring
//! in Mix mode), the system block goes through `AudioCommand::PushLoopback`,
//! the same message `dimmy_push_loopback_audio` sends for the Swift tap.
//!
//!   DIMMY_SIM_MIC_WAV=/path/mic.wav        mono, 48 kHz, f32 or i16
//!   DIMMY_SIM_SYSTEM_WAV=/path/system.wav  same format
//!
//! Each file loops at its end, so a test can run longer than the recording.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RATE: u32 = crate::audio::MEETING_CANONICAL_RATE;
/// 10 ms, what a Core Audio IO proc hands over at 48 kHz.
const BLOCK: usize = (RATE / 100) as usize;

/// While true, pushes from the real system-audio tap are ignored: two
/// producers into one loopback ring would double its sample rate.
static SYSTEM_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn system_active() -> bool {
    SYSTEM_ACTIVE.load(Ordering::SeqCst)
}

pub fn configured() -> bool {
    std::env::var_os("DIMMY_SIM_MIC_WAV").is_some()
}

/// Stops both feeders when dropped, the way dropping a cpal stream stops it.
pub struct SimFeed {
    stop: Arc<AtomicBool>,
}

impl Drop for SimFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        SYSTEM_ACTIVE.store(false, Ordering::SeqCst);
    }
}

pub fn start(
    mic_target: Arc<Mutex<Vec<f32>>>,
    audio_tx: std::sync::mpsc::Sender<crate::audio::AudioCommand>,
    want_system: bool,
) -> Option<SimFeed> {
    let mic_path = std::env::var_os("DIMMY_SIM_MIC_WAV")?;
    let mic = read_mono_48k(Path::new(&mic_path));
    let system = if want_system {
        std::env::var_os("DIMMY_SIM_SYSTEM_WAV").map(|p| read_mono_48k(Path::new(&p)))
    } else {
        None
    };
    crate::log(&format!(
        "[SimAudio] mic {:.0}s from {:?}, system {}",
        mic.len() as f32 / RATE as f32,
        mic_path,
        system
            .as_ref()
            .map(|s| format!("{:.0}s", s.len() as f32 / RATE as f32))
            .unwrap_or_else(|| "none".into())
    ));

    let stop = Arc::new(AtomicBool::new(false));

    let stop_mic = stop.clone();
    std::thread::Builder::new()
        .name("dimmy-sim-mic".into())
        .spawn(move || {
            paced(&mic, &stop_mic, |block| {
                if !crate::audio::meeting_capture_gated() {
                    if let Ok(mut b) = mic_target.lock() {
                        b.extend_from_slice(block);
                    }
                }
            })
        })
        .expect("spawn sim mic");

    if let Some(system) = system {
        SYSTEM_ACTIVE.store(true, Ordering::SeqCst);
        crate::audio::set_loopback_sample_rate_override(RATE);
        let stop_sys = stop.clone();
        std::thread::Builder::new()
            .name("dimmy-sim-system".into())
            .spawn(move || {
                paced(&system, &stop_sys, |block| {
                    let _ = audio_tx.send(crate::audio::AudioCommand::PushLoopback(
                        block.to_vec(),
                        RATE,
                    ));
                })
            })
            .expect("spawn sim system");
    }

    Some(SimFeed { stop })
}

/// Hand `samples` to `push` one block per 10 ms of wall clock, looping.
/// Deadlines are absolute, so a late wake-up is caught up rather than
/// accumulated: over an hour the feed stays at 48 kHz, as hardware does.
fn paced(samples: &[f32], stop: &AtomicBool, mut push: impl FnMut(&[f32])) {
    assert!(samples.len() >= BLOCK, "sim audio shorter than one block");
    let t0 = Instant::now();
    let mut sent: u64 = 0;
    let mut pos = 0usize;
    while !stop.load(Ordering::SeqCst) {
        let due = t0 + Duration::from_micros(sent * 1_000_000 / RATE as u64);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
            continue;
        }
        if pos + BLOCK > samples.len() {
            pos = 0;
        }
        push(&samples[pos..pos + BLOCK]);
        pos += BLOCK;
        sent += BLOCK as u64;
    }
}

fn read_mono_48k(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path)
        .unwrap_or_else(|e| panic!("sim audio {}: {e}", path.display()));
    let spec = r.spec();
    assert_eq!(
        spec.channels,
        1,
        "sim audio must be mono: {}",
        path.display()
    );
    assert_eq!(
        spec.sample_rate,
        RATE,
        "sim audio must be 48 kHz: {}",
        path.display()
    );
    let out: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
    };
    out.into_iter().map(|s| s.clamp(-1.0, 1.0)).collect()
}
