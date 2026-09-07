//! Which language is actually being SPOKEN in a recording.
//!
//! The recap prompt asks for "the transcript's language" in words, and a 4 B
//! local model ignores it: measured 2026-09-07, Qwen 3 answered in English on
//! an Italian meeting. Naming the language explicitly fixes it — so something
//! has to know the language, and the settings combo does not: it says "I speak
//! Italian", not "this meeting was in Italian". Nobody changes it before a
//! call with an English-speaking client.
//!
//! Whisper knows. Its decoder emits a language token before transcribing
//! anything, and `whisper_lang_auto_detect` reads that out as a probability
//! vector without decoding a single word. The model answers, not a word-count
//! heuristic.
//!
//! Two things were measured before writing this, because both were previously
//! believed to be false (see `docs/dev/recap-language-2026-09-07.md`):
//!
//! - **It works.** 10 real meetings, 4 English and 6 Italian: every window
//!   voted correctly, unanimously. `tiny` gives the same verdicts as
//!   `large-v3-turbo` — lower confidence (0.66 at worst) but never a wrong
//!   vote — for 1.4 s instead of 60 s.
//! - **The 2026-07-29 note was half wrong.** It concluded local detection was
//!   unusable because "clear English audio -> it at 99.8%". Re-run on the same
//!   class of audio, whisper's own auto mode and the dedicated call agree with
//!   each other and with reality on 24 windows out of 24. What auto mode
//!   really does is return ZERO SEGMENTS — the transcription comes back empty,
//!   which is a different bug, already documented at `local_stt.rs:979`. So
//!   detect-then-force is sound; what is broken is transcribing in auto mode.
//!
//! Detection never gates anything. Every failure — no model, no audio, audio
//! too short, no majority — returns `None`, and the caller keeps the wording
//! it uses today.

use std::path::Path;

/// Windows sampled across the recording. Five, because a single window is
/// what makes whisper's built-in detection unreliable: ours open with the
/// consent announcement and "hi, how are you", which is the least
/// representative half-minute in the file.
pub const WINDOWS: usize = 5;

/// Skipped at the head for the same reason.
pub const SKIP_HEAD_SECS: f32 = 20.0;

/// One whisper window.
pub const WINDOW_SECS: f32 = 30.0;

/// Below this there is no room for the head skip plus a window, and a verdict
/// from a single opening window is exactly the one we do not trust.
pub const MIN_AUDIO_SECS: f32 = 60.0;

/// A verdict needs this many agreeing windows. Three of five: one odd window
/// (a silent stretch, a sentence in the other language) cannot carry it.
pub const MIN_AGREEING: usize = 3;

/// Windows below this probability do not vote at all. `tiny` sits at
/// 0.66-1.00 on real meetings, so this only drops genuinely undecided
/// windows rather than trimming the model's normal spread.
pub const MIN_WINDOW_CONFIDENCE: f32 = 0.50;

/// Models tried for detection, in order, NOT for transcription.
///
/// Deliberately the small ones. `tiny` and `base` returned the same verdicts
/// as `large-v3-turbo` on every meeting measured — lower confidence, never a
/// wrong vote — for 3-4 s against 60 s. Naming which language it is asks far
/// less of a model than writing down what was said.
///
/// `base` is the fallback because onboarding already downloads it, so this
/// costs the user no new download: a feature that needed one would be inert
/// for everybody until they agreed to it.
pub const DETECT_MODELS: [&str; 2] = ["ggml-tiny-q8_0.bin", "ggml-base-q8_0.bin"];

/// Where each window starts, in seconds.
///
/// Spread evenly over everything after the head skip, so a meeting that
/// changes language halfway is decided by which language occupies more of it
/// rather than by whichever one happened to open.
pub fn window_offsets(total_secs: f32, windows: usize) -> Vec<f32> {
    assert!(windows > 0, "window_offsets: need at least one window");
    assert!(
        total_secs.is_finite() && total_secs > 0.0,
        "window_offsets: duration must be finite and positive"
    );
    if total_secs < MIN_AUDIO_SECS {
        return Vec::new();
    }

    let last_start = total_secs - WINDOW_SECS;
    let first = SKIP_HEAD_SECS.min(last_start);
    let span = last_start - first;
    let offsets: Vec<f32> = if windows == 1 || span <= 0.0 {
        vec![first]
    } else {
        (0..windows)
            .map(|i| first + span * (i as f32 / (windows - 1) as f32))
            .collect()
    };

    for o in &offsets {
        assert!(
            *o >= 0.0 && *o <= last_start + 0.001,
            "window_offsets: {o} outside [0, {last_start}]"
        );
    }
    offsets
}

/// The winning language, or `None` when the windows do not agree enough.
///
/// Returning `None` is a real answer: it means "we did not establish this",
/// and the caller then says nothing about language rather than naming one it
/// invented.
pub fn majority_verdict(votes: &[(String, f32)]) -> Option<String> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (lang, p) in votes {
        if lang.is_empty() || *p < MIN_WINDOW_CONFIDENCE {
            continue;
        }
        *counts.entry(lang.as_str()).or_insert(0) += 1;
    }

    let mut best: Option<(&str, usize)> = None;
    let mut runner_up = 0usize;
    for (lang, n) in counts {
        match best {
            Some((_, bn)) if bn >= n => runner_up = runner_up.max(n),
            Some((bl, bn)) => {
                runner_up = runner_up.max(bn);
                let _ = bl;
                best = Some((lang, n));
            }
            None => best = Some((lang, n)),
        }
    }

    match best {
        Some((lang, n)) if n >= MIN_AGREEING && n > runner_up => Some(lang.to_string()),
        _ => None,
    }
}

/// Ask whisper which language is spoken in `path`.
///
/// `None` whenever we cannot say so honestly — the detection model is not on
/// disk, the file will not decode, it is shorter than [`MIN_AUDIO_SECS`], or
/// the windows disagree. Callers treat that as "carry on as before"; nothing
/// here is allowed to fail a recap.
#[cfg(feature = "local-stt")]
pub fn detect_from_audio_file(path: &Path) -> Option<String> {
    use std::ffi::CStr;

    let Some(model) = DETECT_MODELS
        .iter()
        .map(|m| crate::local_stt::model_path(m))
        .find(|p| p.is_file())
    else {
        crate::log(&format!(
            "[LangDetect] skipped — none of {DETECT_MODELS:?} is on disk"
        ));
        return None;
    };

    let (mono, rate) = match crate::ffi::decode_via_symphonia(&path.to_string_lossy()) {
        Ok(v) => v,
        Err(e) => {
            crate::log(&format!("[LangDetect] cannot decode audio: {e}"));
            return None;
        }
    };
    let pcm = resample_to_16k(&mono, rate);
    drop(mono);
    let secs = pcm.len() as f32 / 16_000.0;

    let offsets = window_offsets(secs, WINDOWS);
    if offsets.is_empty() {
        crate::log(&format!(
            "[LangDetect] skipped — {secs:.0}s is under the {MIN_AUDIO_SECS:.0}s floor"
        ));
        return None;
    }

    let t0 = std::time::Instant::now();
    let cpath = std::ffi::CString::new(model.to_string_lossy().as_ref()).ok()?;
    let ctx = unsafe {
        let params = whisper_rs::whisper_rs_sys::whisper_context_default_params();
        whisper_rs::whisper_rs_sys::whisper_init_from_file_with_params(cpath.as_ptr(), params)
    };
    if ctx.is_null() {
        crate::log("[LangDetect] whisper context failed to load");
        return None;
    }

    let n_langs = unsafe { whisper_rs::whisper_rs_sys::whisper_lang_max_id() } + 1;
    assert!(n_langs > 0, "whisper reported no languages");
    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(4) as i32)
        .unwrap_or(2);

    let mut votes: Vec<(String, f32)> = Vec::with_capacity(offsets.len());
    for offset_s in &offsets {
        // Only the half-minute the detector will look at. Handing the whole
        // recording to pcm_to_mel and offsetting into it costs a full-length
        // mel per window — 155 s on a 34-minute meeting, measured, versus 1.4 s
        // this way.
        let from = (*offset_s * 16_000.0) as usize;
        let to = (from + (WINDOW_SECS as usize) * 16_000).min(pcm.len());
        if to <= from {
            continue;
        }
        let slice = &pcm[from..to];
        let mut probs = vec![0.0f32; n_langs as usize];

        let id = unsafe {
            let ok = whisper_rs::whisper_rs_sys::whisper_pcm_to_mel(
                ctx,
                slice.as_ptr(),
                slice.len() as i32,
                n_threads,
            );
            if ok != 0 {
                -1
            } else {
                whisper_rs::whisper_rs_sys::whisper_lang_auto_detect(
                    ctx,
                    0,
                    n_threads,
                    probs.as_mut_ptr(),
                )
            }
        };
        if id < 0 || id as usize >= probs.len() {
            continue;
        }
        let lang = unsafe {
            CStr::from_ptr(whisper_rs::whisper_rs_sys::whisper_lang_str(id))
                .to_string_lossy()
                .into_owned()
        };
        votes.push((lang, probs[id as usize]));
    }

    unsafe { whisper_rs::whisper_rs_sys::whisper_free(ctx) };

    let verdict = majority_verdict(&votes);
    crate::log(&format!(
        "[LangDetect] {} windows over {:.0}s -> {} in {:.1}s",
        votes.len(),
        secs,
        verdict.as_deref().unwrap_or("undecided"),
        t0.elapsed().as_secs_f32()
    ));
    verdict
}

#[cfg(not(feature = "local-stt"))]
pub fn detect_from_audio_file(_path: &Path) -> Option<String> {
    None
}

/// Whisper is a 16 kHz model. Linear interpolation is enough: the question is
/// which language it is, not what was said.
#[cfg(feature = "local-stt")]
fn resample_to_16k(input: &[f32], rate: u32) -> Vec<f32> {
    assert!(rate > 0, "resample_to_16k: source rate must be non-zero");
    if rate == 16_000 || input.is_empty() {
        return input.to_vec();
    }
    let ratio = rate as f64 / 16_000.0;
    let out_len = (input.len() as f64 / ratio) as usize;
    let out: Vec<f32> = (0..out_len)
        .map(|i| {
            let src = i as f64 * ratio;
            let a = src as usize;
            let b = (a + 1).min(input.len() - 1);
            let f = (src - a as f64) as f32;
            input[a] * (1.0 - f) + input[b] * f
        })
        .collect();
    assert!(
        out.iter().all(|s| s.is_finite()),
        "resample_to_16k produced non-finite samples"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(&str, f32)]) -> Vec<(String, f32)> {
        pairs.iter().map(|(l, p)| (l.to_string(), *p)).collect()
    }

    #[test]
    fn a_unanimous_meeting_is_decided() {
        let votes = v(&[
            ("it", 1.0),
            ("it", 1.0),
            ("it", 0.98),
            ("it", 1.0),
            ("it", 0.99),
        ]);
        assert_eq!(majority_verdict(&votes).as_deref(), Some("it"));
    }

    /// The measured shape: one window dips (0.61 on a real English meeting,
    /// 2026-09-07) while the rest are certain. The majority must carry it —
    /// that is the whole reason we sample more than one window.
    #[test]
    fn one_odd_window_does_not_overturn_the_rest() {
        let votes = v(&[
            ("en", 1.0),
            ("en", 1.0),
            ("it", 0.61),
            ("en", 1.0),
            ("en", 0.99),
        ]);
        assert_eq!(majority_verdict(&votes).as_deref(), Some("en"));
    }

    /// A meeting that genuinely changes language: no verdict is the honest
    /// answer, and the caller falls back to the wording it uses today.
    #[test]
    fn an_even_split_stays_undecided() {
        let votes = v(&[("it", 0.9), ("en", 0.9), ("it", 0.9), ("en", 0.9)]);
        assert_eq!(majority_verdict(&votes), None);
    }

    #[test]
    fn a_thin_majority_is_not_enough() {
        // Two agreeing windows out of five is a coincidence, not a verdict.
        let votes = v(&[("it", 0.9), ("it", 0.9), ("en", 0.8), ("fr", 0.8)]);
        assert_eq!(majority_verdict(&votes), None);
    }

    #[test]
    fn unconfident_windows_do_not_vote() {
        let votes = v(&[
            ("it", 0.2),
            ("it", 0.2),
            ("it", 0.2),
            ("en", 0.9),
            ("en", 0.9),
        ]);
        assert_eq!(
            majority_verdict(&votes),
            None,
            "three windows below the floor must not out-vote two confident ones"
        );
    }

    #[test]
    fn no_windows_means_no_verdict() {
        assert_eq!(majority_verdict(&[]), None);
    }

    #[test]
    fn offsets_skip_the_opening_and_stay_inside_the_file() {
        let offs = window_offsets(600.0, 5);
        assert_eq!(offs.len(), 5);
        assert!(offs[0] >= SKIP_HEAD_SECS, "the opening must be skipped");
        assert!(
            offs[4] + WINDOW_SECS <= 600.0 + 0.001,
            "the last window must fit inside the recording"
        );
        assert!(
            offs.windows(2).all(|w| w[1] > w[0]),
            "offsets must be spread, not stacked"
        );
    }

    #[test]
    fn a_short_recording_yields_no_windows() {
        assert!(window_offsets(MIN_AUDIO_SECS - 1.0, 5).is_empty());
    }

    /// Just past the floor there is no room to spread; one window is still
    /// better than refusing, and it must not run past the end.
    #[test]
    fn a_barely_long_enough_recording_still_gives_a_window() {
        let offs = window_offsets(MIN_AUDIO_SECS, 5);
        assert!(!offs.is_empty());
        for o in &offs {
            assert!(o + WINDOW_SECS <= MIN_AUDIO_SECS + 0.001);
        }
    }
}
