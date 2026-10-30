//! nemotron_meeting_replay -- a recorded meeting through the real pipeline, in
//! real time: `MeetingSession` with its capture and transcription threads, the
//! host events and the stop, with the audio arriving where the cpal callbacks
//! would put it. What `nemotron_meeting_bench` measures with a simulated clock,
//! this measures on a wall clock.
//!
//! Usage: nemotron_meeting_replay [--download] <meeting dir> [--no-speakers] [--seconds 120]
//!
//! `--download` first fetches the bundle and, on a GPU machine, the F32 model,
//! through the functions the download center calls.
//!
//! DIMMY_FORCE_CPU=1 forces the CPU engine. Writes a meeting into the app's
//! meetings folder and removes it at the end.

use dimmy_lib::meeting::{MeetingSession, SttSnapshot};
use std::ffi::{c_char, CStr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Seen {
    partials: usize,
    lines: usize,
    behind: usize,
    deferred: usize,
    line_lag: Vec<f64>,
}

static SEEN: Mutex<Option<(Instant, Seen)>> = Mutex::new(None);

extern "C" fn on_event(json: *const c_char) {
    // SAFETY: the core hands over a NUL-terminated string for this call.
    let text = unsafe { CStr::from_ptr(json) }.to_string_lossy();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let mut guard = SEEN.lock().unwrap();
    let Some((start, seen)) = guard.as_mut() else {
        return;
    };
    let payload = &v["payload"];
    match v["event"].as_str().or(v["type"].as_str()).unwrap_or("") {
        "meeting_partial" => seen.partials += 1,
        "meeting_transcription_behind" => seen.behind += 1,
        "meeting_speakers_deferred" => seen.deferred += 1,
        "meeting_chunk" => {
            seen.lines += 1;
            let at = payload["elapsed_ms"].as_f64().unwrap_or(0.0) / 1000.0;
            seen.line_lag.push(start.elapsed().as_secs_f64() - at);
        }
        _ => {}
    }
}

fn download_models() {
    use dimmy_lib::nemotron;
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(nemotron::download_bundle(|_, _| {}))
        .expect("bundle download");
    if nemotron::full_precision_available() {
        rt.block_on(nemotron::download_full_precision(|_, _| {}))
            .expect("F32 download");
    }
    eprintln!(
        "downloaded: F32 present = {}",
        nemotron::full_precision_present()
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut dir, mut speakers, mut limit) = (None, true, f64::MAX);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-speakers" => speakers = false,
            "--download" => download_models(),
            "--seconds" => {
                limit = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .expect("--seconds n")
            }
            _ => dir = Some(PathBuf::from(a)),
        }
    }
    let dir = dir.expect("usage: nemotron_meeting_replay <meeting dir> [options]");
    let read = |name: &str| {
        let p = dir.join(name);
        dimmy_lib::ffi::decode_via_symphonia(p.to_str().expect("utf-8 path"))
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    };
    let (mic, rate) = read("audio_mic.ogg");
    let (system, sys_rate) = read("audio_system.ogg");
    assert_eq!(rate, sys_rate, "the meeting writes both tracks at one rate");
    let end = mic
        .len()
        .max(system.len())
        .min((limit * rate as f64) as usize);
    assert!(end > 0, "nothing to replay");

    dimmy_lib::ffi::init_headless();
    dimmy_lib::ffi::dimmy_set_event_callback(on_event);
    let mic_buf = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sys_buf = Arc::new(Mutex::new(Vec::<f32>::new()));
    *SEEN.lock().unwrap() = Some((Instant::now(), Seen::default()));
    let session = MeetingSession::start(
        Arc::clone(&mic_buf),
        Arc::clone(&sys_buf),
        rate,
        rate,
        dimmy_lib::audio::AudioSource::Mix,
        SttSnapshot {
            mode: "local".into(),
            api_url: String::new(),
            api_model: String::new(),
            api_key: None,
            prompt: String::new(),
            local_model: "ggml-small-q5_1.bin".into(),
            local_backend: "nemotron".into(),
            language: String::new(),
            chunk_secs: None,
            preprocessing_enabled: false,
            speaker_labels: speakers,
        },
    )
    .expect("meeting start");

    let start = Instant::now();
    let mut pushed = 0usize;
    while pushed < end {
        std::thread::sleep(Duration::from_millis(20));
        let due = (start.elapsed().as_secs_f64() * rate as f64) as usize;
        let to = due.min(end);
        let slice = |v: &[f32]| v[pushed.min(v.len())..to.min(v.len())].to_vec();
        mic_buf.lock().unwrap().extend(slice(&mic));
        sys_buf.lock().unwrap().extend(slice(&system));
        pushed = to;
    }
    let stopped = Instant::now();
    let result = session.stop();
    let stop_secs = stopped.elapsed().as_secs_f64();

    let (_, mut seen) = SEEN.lock().unwrap().take().expect("events");
    seen.line_lag.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        let n = seen.line_lag.len();
        if n == 0 {
            return 0.0;
        }
        seen.line_lag[((n - 1) as f64 * p) as usize]
    };
    println!("{}", result.transcript);
    println!(
        "=== {:.0} s replayed, {:.1} s recorded | stop took {stop_secs:.1} s, speakers_labeled={} | {} lines, {} partials | line after its first word: median {:.1} s, p95 {:.1} s, max {:.1} s | windows dropped warnings {}, speakers deferred {} | error {:?}",
        end as f64 / rate as f64,
        result.duration_secs,
        result.speakers_labeled,
        seen.lines,
        seen.partials,
        pct(0.5),
        pct(0.95),
        pct(1.0),
        seen.behind,
        seen.deferred,
        result.error,
    );
    let files = [
        "audio_mic.ogg",
        "audio_system.ogg",
        "transcripts.txt",
        "speakers.json",
    ];
    for f in files {
        let size = std::fs::metadata(result.dir.join(f)).map_or(0, |m| m.len());
        println!("{f}: {size} bytes");
    }
    assert!(
        result.dir.parent().is_some_and(|p| p.ends_with("meetings")),
        "only ever remove a meeting this run created"
    );
    std::fs::remove_dir_all(&result.dir).expect("remove the replay's meeting");
}
