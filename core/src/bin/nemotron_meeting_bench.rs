//! nemotron_meeting_bench -- replay a recorded meeting through Nemotron the
//! way a live meeting runs it, and measure whether this machine keeps up.
//!
//! Reads `<meeting dir>/audio_mic.ogg` and `audio_system.ogg`, cuts both into
//! the live window (2 s) and feeds them to two `MeetingTrack`s, the system one
//! with live speakers, exactly as `meeting.rs` does. Prints the load (compute
//! seconds per audio second, smoothed like the meeting does; above 0.9 the
//! meeting drops live speakers) and the transcript lines.
//!
//! Usage:
//!   nemotron_meeting_bench <meeting dir> [--no-speakers] [--lang it]
//!
//! Needs Nemotron downloaded in the app's model directory (the app's own
//! engine choice applies: GPU build when present, else CPU).

use dimmy_lib::nemotron::{Line, MeetingTrack};
use std::path::PathBuf;
use std::time::Instant;

const WINDOW_SECS: f64 = 2.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut dir, mut speakers, mut lang) = (None, true, String::new());
    let mut serial = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-speakers" => speakers = false,
            "--serial" => serial = true,
            "--lang" => lang = args.next().unwrap_or_default(),
            _ => dir = Some(PathBuf::from(a)),
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: nemotron_meeting_bench <meeting dir> [--no-speakers] [--lang it]");
        std::process::exit(2);
    };
    let read = |name: &str| {
        let p = dir.join(name);
        dimmy_lib::ffi::decode_via_symphonia(p.to_str().expect("utf-8 path"))
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    };
    let (mic, mic_rate) = read("audio_mic.ogg");
    let (system, sys_rate) = read("audio_system.ogg");
    assert_eq!(
        mic_rate, sys_rate,
        "the meeting writes both tracks at one rate"
    );
    let rate = mic_rate;
    let secs = mic.len().max(system.len()) as f64 / rate as f64;
    eprintln!(
        "{}: {:.0} s, {} Hz, speakers={speakers}",
        dir.display(),
        secs,
        rate
    );

    let t = Instant::now();
    let mut mic_t = MeetingTrack::open(rate, &lang, &[], false).expect("mic track");
    let mut sys_t = MeetingTrack::open(rate, &lang, &[], speakers).expect("system track");
    eprintln!("tracks open in {:.1} s", t.elapsed().as_secs_f64());

    let win = (WINDOW_SECS * rate as f64) as usize;
    let (mut load, mut worst, mut busy, mut over) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    let mut all: Vec<(String, Line)> = Vec::new();
    let mut start = 0usize;
    while start < mic.len().max(system.len()) {
        let m = &mic[start.min(mic.len())..(start + win).min(mic.len())];
        let s = &system[start.min(system.len())..(start + win).min(system.len())];
        let t = Instant::now();
        // Side by side, as meeting.rs runs them; --serial one after the other.
        let (mic_lines, sys_lines) = if serial {
            (mic_t.feed(m, start).expect("mic feed"), sys_t.feed(s, start).expect("system feed"))
        } else {
            std::thread::scope(|scope| {
                let mic_job = scope.spawn(|| mic_t.feed(m, start).expect("mic feed"));
                let sys_lines = sys_t.feed(s, start).expect("system feed");
                (mic_job.join().expect("mic thread"), sys_lines)
            })
        };
        all.extend(mic_lines.into_iter().map(|l| ("mic".into(), l)));
        all.extend(sys_lines.into_iter().map(|l| ("system".into(), l)));
        let took = t.elapsed().as_secs_f64();
        busy += took;
        let ratio = took / WINDOW_SECS;
        load = if load == 0.0 {
            ratio
        } else {
            0.8 * load + 0.2 * ratio
        };
        worst = worst.max(load);
        over += usize::from(load > 0.9);
        start += win;
    }
    for (track, l) in mic_t
        .finish()
        .expect("mic")
        .0
        .into_iter()
        .map(|l| ("mic".to_string(), l))
    {
        all.push((track, l));
    }
    for (track, l) in sys_t
        .finish()
        .expect("system")
        .0
        .into_iter()
        .map(|l| ("system".to_string(), l))
    {
        all.push((track, l));
    }
    all.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));
    for (track, l) in &all {
        let who = l
            .speaker
            .map_or_else(|| track.clone(), |n| format!("Speaker {n}"));
        let t = l.start as u64;
        println!(
            "[{:02}:{:02}:{:02}] [{who}] {}",
            t / 3600,
            t / 60 % 60,
            t % 60,
            l.text
        );
    }
    println!(
        "\n--- {secs:.0} s of meeting, {busy:.1} s compute ({:.2} load), worst smoothed load {worst:.2}, {over} window(s) over 0.9",
        busy / secs
    );
}
