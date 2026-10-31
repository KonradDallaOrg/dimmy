//! nemotron_release_probe -- does destroying a Nemotron recognizer give the
//! memory back, and survive being loaded again? Run it after an engine bump, on
//! every GPU backend: close Dimmy first, the model must fit twice otherwise.
//! DIMMY_NEMOTRON_F32=1 tests F32.
//!
//! Usage: nemotron_release_probe <wait ms after the stream closes> <cycles>

use dimmy_lib::nemotron;
use std::time::{Duration, Instant};

fn report(what: &str, base: u64) {
    let free = dimmy_lib::setup::available_memory_bytes().unwrap_or(0);
    let vram = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!(
        "{what}: commit used {:.2} GB, VRAM {vram} MB",
        (base as f64 - free as f64) / (1024.0 * 1024.0 * 1024.0)
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let wait: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1000);
    let cycles: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    dimmy_lib::ffi::init_headless();
    let base = dimmy_lib::setup::available_memory_bytes().unwrap_or(0);
    report("start", base);
    for cycle in 1..=cycles {
        let started = Instant::now();
        let r = nemotron::app_recognizer(true).expect("load");
        println!(
            "cycle {cycle}: loaded in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        {
            let mut s = std::sync::Arc::clone(&r)
                .stream("", &[], true)
                .expect("stream");
            s.push(&vec![0.0; 16_000 * 3]).expect("push");
            s.finish().expect("finish");
        }
        drop(r);
        report("  in use", base);
        std::thread::sleep(Duration::from_millis(wait));
        let freed = nemotron::release_idle();
        std::thread::sleep(Duration::from_millis(500));
        report(&format!("  released {freed}"), base);
    }
    println!("clean exit");
}
