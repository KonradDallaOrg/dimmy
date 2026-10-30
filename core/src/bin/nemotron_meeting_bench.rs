//! nemotron_meeting_bench -- replay a recorded meeting through Nemotron the
//! way a live meeting runs it, and measure whether this machine keeps up.
//!
//! Reads `<meeting dir>/audio_mic.ogg` and `audio_system.ogg`, cuts both into
//! the live window and feeds them to two `MeetingTrack`s, exactly as
//! `meeting.rs` does, simulating the clock: each window "arrives" when its
//! audio would have been spoken and waits for the previous one to finish.
//! The lag is how far the text trails the voice at each window.
//!
//! Usage:
//!   nemotron_meeting_bench <meeting dir> [--no-speakers] [--mic-speakers]
//!       [--parallel] [--adaptive] [--window 0.5] [--lang it] [--seconds 120]
//!
//! `--adaptive` drops the live speakers under sustained overload, as
//! `meeting.rs` does.
//!
//! DIMMY_FORCE_CPU=1 forces the CPU engine. The F32 model runs when it is on
//! disk; DIMMY_NEMOTRON_Q8=1 keeps Q8_0. Needs Nemotron downloaded in the
//! app's model directory.

use dimmy_lib::nemotron::{self, Fed, Line, MeetingTrack};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut dir, mut speakers, mut mic_speakers, mut parallel) = (None, true, false, false);
    let (mut window, mut lang, mut limit) = (0.5f64, String::new(), f64::MAX);
    let (mut adaptive, mut load) = (false, 0.0f64);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-speakers" => speakers = false,
            "--mic-speakers" => mic_speakers = true,
            "--parallel" => parallel = true,
            "--adaptive" => adaptive = true,
            "--window" => {
                window = args
                    .next()
                    .and_then(|w| w.parse().ok())
                    .expect("--window secs")
            }
            "--seconds" => {
                limit = args
                    .next()
                    .and_then(|w| w.parse().ok())
                    .expect("--seconds n")
            }
            "--lang" => lang = args.next().unwrap_or_default(),
            _ => dir = Some(PathBuf::from(a)),
        }
    }
    let dir = dir.expect("usage: nemotron_meeting_bench <meeting dir> [options]");
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
    let end = mic
        .len()
        .max(system.len())
        .min((limit * rate as f64) as usize);
    let secs = end as f64 / rate as f64;

    let t = Instant::now();
    let mut mic_t = MeetingTrack::open(rate, &lang, &[], mic_speakers).expect("mic track");
    let mut sys_t = MeetingTrack::open(rate, &lang, &[], speakers).expect("system track");
    eprintln!(
        "opened in {:.1} s, {secs:.0} s of audio",
        t.elapsed().as_secs_f64()
    );

    let win = (window * rate as f64) as usize;
    let (mut clock, mut busy, mut start) = (0.0f64, 0.0f64, 0usize);
    let mut lags: Vec<f64> = Vec::new();
    let mut line_lags: Vec<f64> = Vec::new();
    let (mut partials, mut all): (usize, Vec<(&str, Line)>) = (0, Vec::new());
    while start < end {
        let stop = (start + win).min(end);
        let m = &mic[start.min(mic.len())..stop.min(mic.len())];
        let s = &system[start.min(system.len())..stop.min(system.len())];
        let t = Instant::now();
        let (mf, sf): (Fed, Fed) = if parallel {
            std::thread::scope(|scope| {
                let job = scope.spawn(|| mic_t.feed(m, start).expect("mic"));
                let sf = sys_t.feed(s, start).expect("system");
                (job.join().expect("mic thread"), sf)
            })
        } else {
            (
                mic_t.feed(m, start).expect("mic"),
                sys_t.feed(s, start).expect("system"),
            )
        };
        let took = t.elapsed().as_secs_f64();
        busy += took;
        load = 0.95 * load + 0.05 * took / window;
        if adaptive && load > 0.9 && sys_t.has_speakers() {
            let d = Instant::now();
            all.extend(
                sys_t
                    .drop_speakers()
                    .expect("drop")
                    .into_iter()
                    .map(|l| ("system", l)),
            );
            clock += d.elapsed().as_secs_f64();
            eprintln!(
                "speakers dropped at {:.1} s, took {:.1} s",
                stop as f64 / rate as f64,
                d.elapsed().as_secs_f64()
            );
        }
        let arrival = stop as f64 / rate as f64;
        clock = clock.max(arrival) + took;
        lags.push(clock - arrival);
        partials += usize::from(mf.partial.is_some()) + usize::from(sf.partial.is_some());
        for l in &mf.lines {
            line_lags.push(clock - l.start);
        }
        for l in &sf.lines {
            line_lags.push(clock - l.start);
        }
        all.extend(mf.lines.into_iter().map(|l| ("mic", l)));
        all.extend(sf.lines.into_iter().map(|l| ("system", l)));
        start = stop;
    }
    let t = Instant::now();
    let (mic_tail, mic_spoken) = mic_t.finish().expect("mic");
    let (sys_tail, sys_spoken) = sys_t.finish().expect("system");
    let flush = t.elapsed().as_secs_f64();
    all.extend(mic_tail.into_iter().map(|l| ("mic", l)));
    all.extend(sys_tail.into_iter().map(|l| ("system", l)));
    all.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));
    for (track, l) in &all {
        let who = l
            .speaker
            .map_or_else(|| track.to_string(), |n| format!("Speaker {n}"));
        let t = l.start as u64;
        println!(
            "[{:02}:{:02}:{:02}] [{who}] {}",
            t / 3600,
            t / 60 % 60,
            t % 60,
            l.text
        );
    }
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        v.get(((v.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let longest = all
        .iter()
        .map(|(_, l)| l.text.split_whitespace().count())
        .max()
        .unwrap_or(0);
    println!(
        "\n=== load {:.2} | text behind voice: median {:.1} s, p95 {:.1} s, max {:.1} s | line from first word: median {:.1} s, p95 {:.1} s | {} lines (longest {longest} words), {partials} partial updates, {} speakers | flush at stop {flush:.1} s",
        busy / secs,
        pct(&mut lags, 0.5),
        pct(&mut lags, 0.95),
        pct(&mut lags, 1.0),
        pct(&mut line_lags, 0.5),
        pct(&mut line_lags, 0.95),
        all.len(),
        nemotron::speaker_list("x", &sys_spoken.segments).len()
            + nemotron::speaker_list("x", &mic_spoken.segments).len(),
    );
}
