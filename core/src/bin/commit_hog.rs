//! commit_hog -- holds commit charge until only `--free-gb` of it is left, to
//! test what Dimmy does when memory runs out. Commits without touching pages,
//! so it costs no RAM. Windows only: the commit limit is a Windows concept.
//!
//! Usage: commit_hog --free-gb 5 [--hold-secs 300]
//!
//! Stops growing when the commit limit moves and the disk drops under 5 GB:
//! a system-managed page file grows into the disk, and stays grown until reboot.

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    extern "system" {
        fn VirtualAlloc(
            addr: *mut std::ffi::c_void,
            size: usize,
            kind: u32,
            protect: u32,
        ) -> *mut std::ffi::c_void;
    }
    const MEM_COMMIT_RESERVE: u32 = 0x1000 | 0x2000;
    const PAGE_READWRITE: u32 = 0x04;
    const CHUNK: u64 = 128 * 1024 * 1024;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const DISK_FLOOR: u64 = 5 * 1024 * 1024 * 1024;

    let (mut free_gb, mut hold_secs) = (None, 300u64);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let value = args.next().expect("every option takes a value");
        match a.as_str() {
            "--free-gb" => free_gb = value.parse::<f64>().ok(),
            "--hold-secs" => hold_secs = value.parse().expect("--hold-secs n"),
            _ => panic!("unknown option {a}"),
        }
    }
    let free_gb = free_gb.expect("usage: commit_hog --free-gb 5 [--hold-secs 300]");
    assert!(free_gb > 0.25, "leave the system room to run");
    let target = (free_gb * GB) as u64;

    let started = Instant::now();
    let mut held = 0u64;
    let mut announced = false;
    while started.elapsed() < Duration::from_secs(hold_secs) {
        let free = dimmy_lib::setup::available_memory_bytes().expect("Windows reports commit");
        if free > target + CHUNK {
            let disk = dimmy_lib::setup::free_disk_bytes(std::path::Path::new("C:\\"));
            if disk.is_some_and(|d| d < DISK_FLOOR) {
                println!("disk floor reached, holding {:.1} GB", held as f64 / GB);
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            // SAFETY: a fresh anonymous mapping, kept until the process exits.
            let p = unsafe {
                VirtualAlloc(
                    std::ptr::null_mut(),
                    CHUNK as usize,
                    MEM_COMMIT_RESERVE,
                    PAGE_READWRITE,
                )
            };
            assert!(!p.is_null(), "commit refused at {free} free");
            held += CHUNK;
            continue;
        }
        if !announced {
            println!(
                "at target: {:.1} GB free, holding {:.1} GB",
                free as f64 / GB,
                held as f64 / GB
            );
            announced = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("commit_hog runs on Windows only");
    std::process::exit(2);
}
