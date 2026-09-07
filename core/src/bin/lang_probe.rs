//! lang_probe — ask whisper which language is being SPOKEN, from the audio.
//!
//! The recap follows "the transcript's language" as a natural-language
//! instruction, and a 4B local model ignores it: measured 2026-09-07, Qwen 3
//! answered in English on an Italian meeting. Naming the language in the
//! system prompt fixes it — so the recap needs to KNOW the language, and the
//! settings combo is the user's dictation language, not necessarily what was
//! spoken in the room.
//!
//! Whisper already knows. Its decoder emits a language token before it
//! transcribes anything, and `whisper_lang_auto_detect` exposes that as a
//! full probability vector without decoding a single word. This is the
//! model answering, not a word-frequency heuristic.
//!
//! The documented failure of whisper's language ID is that it looks at ONE
//! 30 s window — the first — so an English greeting or our own consent
//! announcement decides a whole Italian meeting. This probe therefore takes
//! SEVERAL windows spread across the recording and reports each one, so the
//! measurement shows whether a majority vote actually fixes that or whether
//! we are fooling ourselves again.
//!
//! whisper-rs 0.16 wraps `lang_str` but NOT `lang_auto_detect` or
//! `pcm_to_mel`, so both are called through the re-exported `-sys` bindings.
//!
//! Set DIMMY_LANG_FULL=1 to ALSO run whisper's own auto mode on the same
//! 30 s slice — `whisper_full` with `detect_language`, reading the verdict
//! back with `whisper_full_lang_id` — because that is what the 2026-07-29
//! experiment measured when it concluded local detection was unusable
//! ("clear English audio -> it at 99.8%, every window, every offset"). Both
//! numbers on the same audio, side by side, settle which of the two the
//! failure belongs to.
//!
//! Usage: lang_probe <model.gguf-or-bin> <windows> <audio.wav> [more.wav …]

#[cfg(feature = "local-stt")]
fn main() {
    use std::ffi::CStr;

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: lang_probe <whisper-model.bin> <windows> <audio.wav> [...]");
        std::process::exit(2);
    }
    let model = args[1].clone();
    let windows: usize = args[2].parse().expect("windows must be a number");
    assert!(windows > 0, "at least one window");

    let ctx = unsafe {
        let cpath = std::ffi::CString::new(model.as_str()).unwrap();
        let params = whisper_rs::whisper_rs_sys::whisper_context_default_params();
        whisper_rs::whisper_rs_sys::whisper_init_from_file_with_params(cpath.as_ptr(), params)
    };
    assert!(!ctx.is_null(), "whisper model failed to load: {model}");

    let n_langs = unsafe { whisper_rs::whisper_rs_sys::whisper_lang_max_id() } + 1;
    let n_threads = 4;
    let full_mode = std::env::var("DIMMY_LANG_FULL").is_ok();

    println!("model={model}  windows={windows}  languages={n_langs}");
    println!();

    for path in &args[3..] {
        // Meetings are stored as ogg/opus, not wav — the core already has the
        // decoder the file-load path uses, so the probe reads exactly what the
        // product reads rather than a format of its own.
        let decoded = if path.to_ascii_lowercase().ends_with(".wav") {
            read_wav_mono(path).ok_or_else(|| "hound failed".to_string())
        } else {
            dimmy_lib::ffi::decode_via_symphonia(path)
        };
        let (mono, rate) = match decoded {
            Ok(v) => v,
            Err(e) => {
                println!("{}: UNREADABLE ({e})", short(path));
                continue;
            }
        };
        // Whisper is a 16 kHz model. Linear decimation is enough here: we are
        // asking which language it is, not transcribing.
        let pcm = resample_to_16k(&mono, rate);
        let secs = pcm.len() as f32 / 16_000.0;
        if secs < 31.0 {
            println!("{}: TOO SHORT ({secs:.0}s)", short(path));
            continue;
        }

        // Whisper reads one 30 s window per call. Spread the windows over the
        // recording and skip the first 20 s outright: that is where our own
        // consent announcement and the "hi, how are you" live.
        let usable = secs - 30.0;
        let start = 20.0_f32.min(usable);
        let step = if windows > 1 {
            (usable - start) / (windows - 1) as f32
        } else {
            0.0
        };

        let mut votes: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut detail = Vec::new();
        let t0 = std::time::Instant::now();

        for w in 0..windows {
            let offset_s = start + step * w as f32;
            let offset_ms = (offset_s * 1000.0) as i32;
            let mut probs = vec![0.0f32; n_langs as usize];

            // Only the 30 s the detector will look at. Handing pcm_to_mel the
            // whole recording and then offsetting into it costs a full-length
            // mel per window — 155 s average, measured, which is the probe
            // being wasteful and says nothing about the method.
            let from = (offset_s * 16_000.0) as usize;
            let to = (from + 30 * 16_000).min(pcm.len());
            let slice = &pcm[from..to];
            let _ = offset_ms;

            let id = unsafe {
                let ok = whisper_rs::whisper_rs_sys::whisper_pcm_to_mel(
                    ctx,
                    slice.as_ptr(),
                    slice.len() as i32,
                    n_threads,
                );
                assert_eq!(ok, 0, "pcm_to_mel failed");
                whisper_rs::whisper_rs_sys::whisper_lang_auto_detect(
                    ctx,
                    0,
                    n_threads,
                    probs.as_mut_ptr(),
                )
            };
            if id < 0 {
                detail.push(format!("{offset_s:.0}s=ERR"));
                continue;
            }
            let lang = unsafe {
                CStr::from_ptr(whisper_rs::whisper_rs_sys::whisper_lang_str(id))
                    .to_string_lossy()
                    .into_owned()
            };
            let p = probs[id as usize];

            // Same slice, whisper's own auto mode, so the comparison has no
            // "but it was different audio" escape hatch.
            let full = if full_mode {
                let (flang, nseg) = unsafe {
                    let mut fp = whisper_rs::whisper_rs_sys::whisper_full_default_params(
                        whisper_rs::whisper_rs_sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY,
                    );
                    fp.detect_language = true;
                    fp.language = std::ptr::null();
                    fp.n_threads = n_threads;
                    fp.print_special = false;
                    fp.print_progress = false;
                    fp.print_realtime = false;
                    fp.print_timestamps = false;
                    let rc = whisper_rs::whisper_rs_sys::whisper_full(
                        ctx,
                        fp,
                        slice.as_ptr(),
                        slice.len() as i32,
                    );
                    if rc != 0 {
                        ("ERR".to_string(), -1)
                    } else {
                        let lid = whisper_rs::whisper_rs_sys::whisper_full_lang_id(ctx);
                        let n = whisper_rs::whisper_rs_sys::whisper_full_n_segments(ctx);
                        let name = if lid < 0 {
                            "?".to_string()
                        } else {
                            CStr::from_ptr(whisper_rs::whisper_rs_sys::whisper_lang_str(lid))
                                .to_string_lossy()
                                .into_owned()
                        };
                        (name, n)
                    }
                };
                format!("/full={flang}({nseg}seg)")
            } else {
                String::new()
            };

            detail.push(format!("{offset_s:.0}s={lang}({p:.2}){full}"));
            *votes.entry(lang).or_insert(0) += 1;
        }

        let mut tally: Vec<_> = votes.into_iter().collect();
        tally.sort_by(|a, b| b.1.cmp(&a.1));
        let winner = tally
            .first()
            .map(|(l, n)| format!("{l} {n}/{windows}"))
            .unwrap_or_else(|| "none".to_string());

        println!(
            "{:<26} {:>5.0}s  {:<12} [{}]  {:.1}s",
            short(path),
            secs,
            winner,
            detail.join(" "),
            t0.elapsed().as_secs_f32()
        );
    }

    unsafe { whisper_rs::whisper_rs_sys::whisper_free(ctx) };
}

#[cfg(feature = "local-stt")]
fn short(path: &str) -> String {
    let p = std::path::Path::new(path);
    p.parent()
        .and_then(|d| d.file_name())
        .map(|s| s.to_string_lossy().chars().take(24).collect())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(feature = "local-stt")]
fn read_wav_mono(path: &str) -> Option<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .filter_map(|s| s.ok())
            .map(|s| s as f32 / (1i64 << (spec.bits_per_sample - 1)) as f32)
            .collect(),
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
    };
    let mono = if spec.channels > 1 {
        samples
            .chunks(spec.channels as usize)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect()
    } else {
        samples
    };
    Some((mono, spec.sample_rate))
}

#[cfg(feature = "local-stt")]
fn resample_to_16k(input: &[f32], rate: u32) -> Vec<f32> {
    if rate == 16_000 {
        return input.to_vec();
    }
    let ratio = rate as f64 / 16_000.0;
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let src = i as f64 * ratio;
            let a = src as usize;
            let b = (a + 1).min(input.len() - 1);
            let f = (src - a as f64) as f32;
            input[a] * (1.0 - f) + input[b] * f
        })
        .collect()
}

#[cfg(not(feature = "local-stt"))]
fn main() {
    eprintln!("lang_probe needs --features local-stt");
    std::process::exit(2);
}
