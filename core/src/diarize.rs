//! Speaker diarization â€” "who spoke when" â€” with NVIDIA Nemotron-3-Diarization
//! (a 100 M-parameter streaming Sortformer), run through `ort` from the ONNX
//! export at `joosthel/Nemotron-3-Diarization-ONNX`.
//!
//! The graph only encodes one chunk. The speaker cache that carries speaker
//! identity from one chunk to the next â€” Arrival-Order Speaker Cache + FIFO,
//! `Nemotron3DiarizationSpeakerCache` in transformers â€” runs here, a port of
//! the export's numpy reference (`diarize.py`, `NumpySpeakerCache`). Speakers
//! are numbered in order of first appearance, up to [`NUM_SPEAKERS`].
//!
//! Only the OFFLINE preset is used (30.4 s chunks): diarization runs over a
//! finished recording, never in the capture path. Measured 2026-09-28 on an
//! i7-12700H: 45-78x realtime on CPU (an 11-minute meeting in 14.5 s). Italian
//! is not in the model's training languages; on Italian voices it still put
//! 99 % of words on the right speaker with 4 people and 94 % with 6.
//! See `docs/dev/diarization.md`.

// The inference half (cache, mel, pooling) only runs with the ONNX runtime;
// without the feature `diarize()` is a stub and those items are exercised by
// the tests alone.
#![cfg_attr(not(feature = "local-stt-parakeet"), allow(dead_code))]

use std::path::PathBuf;

pub const NUM_SPEAKERS: usize = 8;
/// Output resolution: one probability row per 10 ms mel frame.
pub const FRAME_SECS: f64 = 0.01;

pub const FILE_MODEL: &str = "model.int8.onnx";
pub const FILE_PREPROCESSOR: &str = "preprocessor_core.onnx";

/// Pinned to a revision so a re-upload can never change what is downloaded
/// under the same name; each file is also checked against its SHA-256.
const HF_BASE: &str = "https://huggingface.co/joosthel/Nemotron-3-Diarization-ONNX/resolve/4a911fc3ca821b76a99fffd5ce5135bd1efca540";
const FILES: &[(&str, &str)] = &[
    (
        FILE_PREPROCESSOR,
        "28e815a770a8cbe47c351cd36908dd129a0b36ab31461aa8f370a369419703e1",
    ),
    (
        FILE_MODEL,
        "17fd04215d655d2feee413a44f96e57d4ebd96cdd9cdbdf8c26da266e08bc835",
    ),
];
pub const MODEL_SIZE_MB: u32 = 104;

// Export constants (`constants.npz` of the pinned revision).
const HIDDEN: usize = 512;
const SUBSAMPLING: usize = 8;
const CHUNK_LEN: usize = 340;
const CHUNK_RIGHT_CONTEXT: usize = 40;
const FIFO_LEN: usize = 40;
const UPDATE_PERIOD: usize = 300;
const CACHE_LEN: usize = 264;
const SILENCE_FRAMES_PER_SPEAKER: usize = 1;
const PRED_SCORE_THRESHOLD: f32 = 0.25;
const LATEST_FRAMES_BOOST: f32 = 0.05;
const STRONG_BOOST_RATE: f64 = 0.75;
const WEAK_BOOST_RATE: f64 = 1.5;
const MIN_POSITIVE_SCORES_RATE: f64 = 0.5;
const SILENCE_EMBEDS: &[u8] = include_bytes!("../assets/diarization_silence_embeds.f32");

// Log-mel front end (NeMo): 16 kHz, n_fft 512, hop 160, preemphasis 0.97.
const HOP: usize = 160;
const N_FFT: usize = 512;
const N_MELS: usize = 128;
const PREEMPHASIS: f32 = 0.97;
/// Bounds the preprocessor's STFT buffer to ~60 s of audio per call, whatever
/// the recording's length (one whole-file call peaked at 3.4 GB on 50 min).
const MEL_WINDOW_FRAMES: usize = 6000;

const ACTIVE: f32 = 0.5;

pub fn model_dir() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join("diarization"))
}

pub fn model_present() -> bool {
    let Some(dir) = model_dir() else {
        return false;
    };
    FILES.iter().all(|(name, _)| {
        std::fs::metadata(dir.join(name))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    })
}

/// Download both graphs into [`model_dir`]. Resumable; every file is verified
/// against its pinned SHA-256 and deleted on mismatch so a retry starts clean.
pub async fn download(progress: impl Fn(u64, u64)) -> Result<(), String> {
    let dir = model_dir().ok_or("config dir unknown")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {:?}: {e}", dir))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let total: u64 = MODEL_SIZE_MB as u64 * 1024 * 1024;
    let mut base: u64 = 0;
    for (name, sha) in FILES {
        let dest = dir.join(name);
        if crate::download::verify_file(&dest, &[], Some(sha)).is_ok() {
            base += std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            progress(base, total);
            continue;
        }
        let _ = std::fs::remove_file(&dest);
        let url = format!("{HF_BASE}/{name}");
        crate::log(&format!("[Diarize] downloading {name}"));
        crate::download::download_resumable(&client, &url, &dest, &[], |done, _| {
            progress(base + done, total)
        })
        .await
        .map_err(|e| format!("{name}: {e}"))?;
        if let Err(e) = crate::download::verify_file(&dest, &[], Some(sha)) {
            let _ = std::fs::remove_file(&dest);
            return Err(format!("{name}: {e}"));
        }
        base += std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        progress(base, total);
    }
    assert!(model_present(), "both graphs must exist after a download");
    Ok(())
}

/// Per-speaker activity, one row per 10 ms of input audio.
#[derive(Debug, Clone, Default)]
pub struct Diarization {
    pub probs: Vec<[f32; NUM_SPEAKERS]>,
}

impl Diarization {
    /// The speaker who talks most inside `[start, end)` seconds, or â€” when
    /// nobody is active there (word timestamps land a few frames off the
    /// diarizer's edges) â€” within half a second either side. `None` when the
    /// span is silence to the diarizer.
    pub fn speaker_for_span(&self, start: f64, end: f64) -> Option<usize> {
        assert!(start.is_finite() && end.is_finite(), "span must be finite");
        let n = self.probs.len();
        let a = ((start.max(0.0) / FRAME_SECS) as usize).min(n);
        let b = ((end.max(start) / FRAME_SECS) as usize).max(a + 1).min(n);
        self.dominant(a, b)
            .or_else(|| self.dominant(a.saturating_sub(50), (b + 50).min(n)))
    }

    fn dominant(&self, a: usize, b: usize) -> Option<usize> {
        let mut tot = [0.0f32; NUM_SPEAKERS];
        for row in &self.probs[a..b] {
            for (t, &p) in tot.iter_mut().zip(row) {
                if p > ACTIVE {
                    *t += p;
                }
            }
        }
        let (best, &score) = tot
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
            .expect("NUM_SPEAKERS > 0");
        (score > 0.0).then_some(best)
    }

    /// Speakers active for at least `min_secs` in total, in arrival order.
    pub fn speakers(&self, min_secs: f64) -> Vec<usize> {
        let min_frames = (min_secs / FRAME_SECS) as usize;
        (0..NUM_SPEAKERS)
            .filter(|&s| self.probs.iter().filter(|r| r[s] > ACTIVE).count() >= min_frames)
            .collect()
    }

    /// Thresholded `(speaker, start_s, end_s)` spans, sorted by start.
    pub fn segments(&self) -> Vec<(usize, f64, f64)> {
        let mut out = Vec::new();
        for s in 0..NUM_SPEAKERS {
            let mut start: Option<usize> = None;
            for (i, row) in self.probs.iter().enumerate() {
                match (row[s] > ACTIVE, start) {
                    (true, None) => start = Some(i),
                    (false, Some(a)) => {
                        out.push((s, a as f64 * FRAME_SECS, i as f64 * FRAME_SECS));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(a) = start {
                out.push((
                    s,
                    a as f64 * FRAME_SECS,
                    self.probs.len() as f64 * FRAME_SECS,
                ));
            }
        }
        out.sort_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)));
        out
    }
}

/// A timed word from any ASR engine, in seconds from the start of the track.
#[derive(Debug, Clone)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Group words into speaker turns: `(start_ms, speaker, text)`. A turn ends on
/// a speaker change, a pause longer than `GAP_SECS`, or a long line. A word the
/// diarizer puts nowhere inherits the previous word's speaker, so a mumbled
/// word never breaks a sentence in two.
pub fn group_words(words: &[Word], d: &Diarization) -> Vec<(u128, Option<usize>, String)> {
    const GAP_SECS: f64 = 1.3;
    const MAX_CHARS: usize = 240;
    let mut out: Vec<(u128, Option<usize>, String)> = Vec::new();
    let mut prev_end = f64::NEG_INFINITY;
    let mut prev_spk: Option<usize> = None;
    for w in words {
        let text = w.text.trim();
        if text.is_empty() {
            continue;
        }
        let spk = d.speaker_for_span(w.start, w.end).or(prev_spk);
        let extend = matches!(out.last(), Some((_, s, t))
            if *s == spk && w.start - prev_end <= GAP_SECS && t.len() + text.len() < MAX_CHARS);
        if extend {
            let last = out.last_mut().expect("checked by matches!");
            last.2.push(' ');
            last.2.push_str(text);
        } else {
            out.push(((w.start.max(0.0) * 1000.0) as u128, spk, text.to_string()));
        }
        prev_end = w.end;
        prev_spk = spk;
    }
    out
}

/// One recorded track after transcription: its turns (start ms, speaker index
/// within this track, text) and the diarization they were labelled with.
pub struct BandTurns {
    pub band: &'static str,
    pub diar: Diarization,
    pub turns: Vec<(u128, Option<usize>, String)>,
}

/// A speaker as the host shows it: stable id, display name, the track it was
/// heard on, and when it spoke (drives the per-speaker waveform lanes).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SpeakerInfo {
    pub id: String,
    pub name: String,
    pub band: String,
    pub talk_secs: f64,
    pub segments: Vec<(f64, f64)>,
}

pub const SPEAKERS_FILE: &str = "speakers.json";
const RESERVED_LABELS: &[&str] = &["mic", "system", "paused"];
const MAX_NAME_CHARS: usize = 40;

pub fn default_name(n: usize) -> String {
    format!("Speaker {n}")
}

/// Turn per-track turns into transcript lines `(ms, label, text)` and the
/// speaker list. The system track (the other side of a call) is numbered
/// first; the mic track keeps its `mic` label unless the diarizer heard more
/// than one person on it â€” an in-person meeting â€” in which case its voices
/// get speaker ids too. `prior` carries names from an earlier run, by id.
pub fn label_bands(
    mut bands: Vec<BandTurns>,
    prior: &[SpeakerInfo],
) -> (Vec<(u128, String, String)>, Vec<SpeakerInfo>) {
    bands.sort_by_key(|b| if b.band == "system" { 0 } else { 1 });
    let mut lines = Vec::new();
    let mut speakers: Vec<SpeakerInfo> = Vec::new();
    for b in bands {
        let keep_band_label = b.band != "system" && b.diar.speakers(1.0).len() <= 1;
        let segments = b.diar.segments();
        let mut local_to_global: Vec<(usize, usize)> = Vec::new();
        for (ms, spk, text) in b.turns {
            let label = match spk {
                Some(local) if !keep_band_label => {
                    let g = match local_to_global.iter().find(|(l, _)| *l == local) {
                        Some(&(_, g)) => g,
                        None => {
                            let n = speakers.len() + 1;
                            let id = format!("S{n}");
                            let name = prior
                                .iter()
                                .find(|p| p.id == id)
                                .map(|p| p.name.clone())
                                .unwrap_or_else(|| default_name(n));
                            let segs = merge_segments(
                                segments.iter().filter(|s| s.0 == local).map(|s| (s.1, s.2)),
                            );
                            speakers.push(SpeakerInfo {
                                id,
                                name,
                                band: b.band.to_string(),
                                talk_secs: segs.iter().map(|(a, e)| e - a).sum(),
                                segments: segs,
                            });
                            local_to_global.push((local, speakers.len() - 1));
                            speakers.len() - 1
                        }
                    };
                    speakers[g].name.clone()
                }
                _ => b.band.to_string(),
            };
            lines.push((ms, label, text));
        }
    }
    lines.sort_by_key(|l| l.0);
    (lines, speakers)
}

/// Join spans separated by less than 0.3 s and round to centiseconds: the
/// waveform lanes need the shape of a turn, not every 10 ms flicker.
fn merge_segments(segs: impl Iterator<Item = (f64, f64)>) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = segs.collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (a, e) in v {
        match out.last_mut() {
            Some(last) if a - last.1 < 0.3 => last.1 = last.1.max(e),
            _ => out.push((a, e)),
        }
    }
    out.into_iter()
        .map(|(a, e)| ((a * 100.0).round() / 100.0, (e * 100.0).round() / 100.0))
        .collect()
}

pub fn load_speakers(dir: &std::path::Path) -> Vec<SpeakerInfo> {
    std::fs::read_to_string(dir.join(SPEAKERS_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<SpeakerInfo>>(&s).ok())
        .unwrap_or_default()
}

/// Write the list atomically; an empty list removes the file, so a transcript
/// regenerated without diarization never shows the speakers of an old run.
pub fn save_speakers(dir: &std::path::Path, speakers: &[SpeakerInfo]) -> std::io::Result<()> {
    let path = dir.join(SPEAKERS_FILE);
    if speakers.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let json = serde_json::to_string_pretty(speakers).expect("speakers serialize");
    let tmp = dir.join(format!("{SPEAKERS_FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, path)
}

#[derive(Debug, PartialEq)]
pub enum RenameError {
    NotFound,
    InvalidName,
    Duplicate,
    Io(String),
}

/// Rename a speaker everywhere it appears: `speakers.json` and every
/// `[name]` label in `transcripts.txt`, so the recap, search and exports all
/// read the real name. Names that could be mistaken for a track label or
/// break the line format are refused.
pub fn rename_speaker(
    dir: &std::path::Path,
    id: &str,
    new_name: &str,
) -> Result<String, RenameError> {
    let name: String = new_name.trim().to_string();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(|c| c == '[' || c == ']' || c.is_control())
        || RESERVED_LABELS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&name))
    {
        return Err(RenameError::InvalidName);
    }
    let mut speakers = load_speakers(dir);
    let idx = speakers
        .iter()
        .position(|s| s.id == id)
        .ok_or(RenameError::NotFound)?;
    if speakers
        .iter()
        .enumerate()
        .any(|(i, s)| i != idx && s.name.eq_ignore_ascii_case(&name))
    {
        return Err(RenameError::Duplicate);
    }
    let old = std::mem::replace(&mut speakers[idx].name, name.clone());
    if old == name {
        return Ok(name);
    }

    let tpath = dir.join("transcripts.txt");
    let text = std::fs::read_to_string(&tpath).map_err(|e| RenameError::Io(e.to_string()))?;
    std::fs::write(&tpath, relabel_lines(&text, &old, &name))
        .map_err(|e| RenameError::Io(e.to_string()))?;
    save_speakers(dir, &speakers).map_err(|e| RenameError::Io(e.to_string()))?;
    Ok(name)
}

/// Swap the speaker label of lines shaped `[ts] [old] text`; any other line â€”
/// including one whose TEXT mentions `[old]` â€” is left untouched.
fn relabel_lines(text: &str, old: &str, new: &str) -> String {
    let needle = format!("] [{old}] ");
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        match line.find(&needle) {
            Some(pos) if line.starts_with('[') && !line[..pos].contains("] ") => {
                out.push_str(&line[..pos]);
                out.push_str(&format!("] [{new}] "));
                out.push_str(&line[pos + needle.len()..]);
            }
            _ => out.push_str(line),
        }
    }
    out
}

/// Ownership-free core of the speaker cache: the FIFO of recent frames and the
/// compressed cache of the frames that best characterise each speaker. Both
/// are fed back to the model as context for the next chunk.
struct SpeakerCache {
    embeds: Vec<f32>,                // [n_cache, HIDDEN]
    probs: Vec<[f32; NUM_SPEAKERS]>, // [n_cache]
    fifo: Vec<f32>,                  // [n_fifo, HIDDEN]
    compressed: bool,
    silence: Vec<f32>,
    min_positive: usize,
    strong_boosted: usize,
    weak_boosted: usize,
}

impl SpeakerCache {
    fn new() -> Self {
        let silence: Vec<f32> = SILENCE_EMBEDS
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(
            silence.len(),
            HIDDEN,
            "silence embedding must be HIDDEN wide"
        );
        let budget = (CACHE_LEN / NUM_SPEAKERS - SILENCE_FRAMES_PER_SPEAKER) as f64;
        SpeakerCache {
            embeds: Vec::new(),
            probs: Vec::new(),
            fifo: Vec::new(),
            compressed: false,
            silence,
            min_positive: (budget * MIN_POSITIVE_SCORES_RATE).floor() as usize,
            strong_boosted: (budget * STRONG_BOOST_RATE).floor() as usize,
            weak_boosted: (budget * WEAK_BOOST_RATE).floor() as usize,
        }
    }

    fn n_cache(&self) -> usize {
        self.embeds.len() / HIDDEN
    }

    fn context(&self) -> Vec<f32> {
        [self.embeds.as_slice(), self.fifo.as_slice()].concat()
    }

    /// `embeds_out` is the graph's `embeds` ([context + chunk (+ right
    /// context), HIDDEN]); `logits` its logits at 8x that rate.
    fn update(&mut self, embeds_out: &[f32], logits: &[f32], n_chunk: usize) {
        let n_cache = self.n_cache();
        let n_fifo = self.fifo.len() / HIDDEN;
        let probs = pool_probs(logits);

        let chunk_start = n_cache + n_fifo;
        let mut fifo = std::mem::take(&mut self.fifo);
        fifo.extend_from_slice(&embeds_out[chunk_start * HIDDEN..(chunk_start + n_chunk) * HIDDEN]);
        let fifo_frames = fifo.len() / HIDDEN;

        let popped = if fifo_frames <= FIFO_LEN {
            0
        } else {
            UPDATE_PERIOD.max(fifo_frames - FIFO_LEN).min(fifo_frames)
        };
        if popped > 0 {
            let fifo_probs = &probs[n_cache..n_cache + fifo_frames];
            let mut cache_probs: Vec<[f32; NUM_SPEAKERS]> = if self.compressed {
                self.probs[..n_cache].to_vec()
            } else {
                probs[..n_cache].to_vec()
            };
            let mut cache_embeds = std::mem::take(&mut self.embeds);
            cache_embeds.extend_from_slice(&fifo[..popped * HIDDEN]);
            cache_probs.extend_from_slice(&fifo_probs[..popped]);
            fifo.drain(..popped * HIDDEN);

            if cache_probs.len() > CACHE_LEN {
                let (e, p) = self.compress(&cache_embeds, &cache_probs);
                cache_embeds = e;
                cache_probs = p;
                self.compressed = true;
            }
            assert!(
                cache_probs.len() <= CACHE_LEN,
                "cache must fit after compress"
            );
            self.embeds = cache_embeds;
            self.probs = cache_probs;
        }
        self.fifo = fifo;
    }

    fn frame_scores(&self, probs: &[[f32; NUM_SPEAKERS]]) -> Vec<[f32; NUM_SPEAKERS]> {
        let thr = PRED_SCORE_THRESHOLD;
        let mut scores: Vec<[f32; NUM_SPEAKERS]> = probs
            .iter()
            .map(|row| {
                let lc: Vec<f32> = row.iter().map(|&p| (1.0 - p).max(thr).ln()).collect();
                let sum_lc: f32 = lc.iter().sum();
                let mut s = [0.0f32; NUM_SPEAKERS];
                for k in 0..NUM_SPEAKERS {
                    s[k] = if row[k] > 0.5 {
                        row[k].max(thr).ln() - lc[k] + sum_lc - 0.5f32.ln()
                    } else {
                        f32::NEG_INFINITY
                    };
                }
                s
            })
            .collect();
        for k in 0..NUM_SPEAKERS {
            let positives = scores.iter().filter(|r| r[k] > 0.0).count();
            if positives >= self.min_positive {
                for (r, p) in scores.iter_mut().zip(probs) {
                    if r[k] <= 0.0 && p[k] > 0.5 {
                        r[k] = f32::NEG_INFINITY;
                    }
                }
            }
        }
        scores
    }

    fn compress(
        &self,
        embeds: &[f32],
        probs: &[[f32; NUM_SPEAKERS]],
    ) -> (Vec<f32>, Vec<[f32; NUM_SPEAKERS]>) {
        let n = probs.len();
        let mut scores = self.frame_scores(probs);
        for r in scores.iter_mut().skip(CACHE_LEN) {
            for v in r.iter_mut() {
                *v += LATEST_FRAMES_BOOST;
            }
        }
        let ln_half = 0.5f32.ln();
        boost(&mut scores, self.strong_boosted, -2.0 * ln_half);
        boost(&mut scores, self.weak_boosted, -ln_half);

        // Speaker-major flat scores over n real frames + the silence frame
        // (always +inf, so every speaker keeps one silence slot).
        let scored = n + SILENCE_FRAMES_PER_SPEAKER;
        let mut flat = Vec::with_capacity(scored * NUM_SPEAKERS);
        for k in 0..NUM_SPEAKERS {
            flat.extend(scores.iter().map(|r| r[k]));
            flat.extend(std::iter::repeat_n(
                f32::INFINITY,
                SILENCE_FRAMES_PER_SPEAKER,
            ));
        }
        let sentinel = scored * NUM_SPEAKERS;
        let mut picked: Vec<usize> = stable_topk(&flat, CACHE_LEN)
            .into_iter()
            .map(|i| {
                if flat[i] == f32::NEG_INFINITY {
                    sentinel
                } else {
                    i
                }
            })
            .collect();
        picked.sort_unstable();

        let mut out_e = Vec::with_capacity(CACHE_LEN * HIDDEN);
        let mut out_p = Vec::with_capacity(CACHE_LEN);
        for i in picked {
            let frame = if i == sentinel {
                n
            } else {
                (i % scored).min(n)
            };
            if frame == n {
                out_e.extend_from_slice(&self.silence);
                out_p.push([0.0; NUM_SPEAKERS]);
            } else {
                out_e.extend_from_slice(&embeds[frame * HIDDEN..(frame + 1) * HIDDEN]);
                out_p.push(probs[frame]);
            }
        }
        assert_eq!(
            out_p.len(),
            CACHE_LEN,
            "compress keeps exactly CACHE_LEN frames"
        );
        (out_e, out_p)
    }
}

/// Add `amount` to each speaker's `k` best frames.
fn boost(scores: &mut [[f32; NUM_SPEAKERS]], k: usize, amount: f32) {
    if k == 0 {
        return;
    }
    for s in 0..NUM_SPEAKERS {
        let col: Vec<f32> = scores.iter().map(|r| r[s]).collect();
        for i in stable_topk(&col, k.min(col.len())) {
            scores[i][s] += amount;
        }
    }
}

/// Indices of the `k` largest values, ties broken toward the lower index â€”
/// the reference's `argsort(-x, kind="stable")[:k]`. -inf sorts last.
fn stable_topk(x: &[f32], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..x.len()).collect();
    idx.sort_by(|&a, &b| x[b].total_cmp(&x[a]));
    idx.truncate(k);
    idx
}

fn sigmoid(x: f32) -> f32 {
    let z = (-x.abs()).exp();
    if x >= 0.0 {
        1.0 / (1.0 + z)
    } else {
        z / (1.0 + z)
    }
}

/// Sigmoid, then average each run of `SUBSAMPLING` logit rows: speaker
/// probabilities at the encoder frame rate.
fn pool_probs(logits: &[f32]) -> Vec<[f32; NUM_SPEAKERS]> {
    let rows = logits.len() / NUM_SPEAKERS;
    assert_eq!(rows % SUBSAMPLING, 0, "logit rows come in multiples of 8");
    (0..rows / SUBSAMPLING)
        .map(|f| {
            let mut acc = [0.0f32; NUM_SPEAKERS];
            for r in 0..SUBSAMPLING {
                let row = &logits[((f * SUBSAMPLING + r) * NUM_SPEAKERS)..][..NUM_SPEAKERS];
                for (a, &l) in acc.iter_mut().zip(row) {
                    *a += sigmoid(l);
                }
            }
            acc.map(|a| a / SUBSAMPLING as f32)
        })
        .collect()
}

/// The preemphasized, zero-padded input for mel frames `[first, last)`, built
/// from real samples only: each window reads one sample of true lookback, so
/// cutting the file into windows reproduces a whole-file `center=True` STFT.
fn mel_window_input(wave: &[f32], first: usize, last: usize) -> Vec<f32> {
    assert!(first < last, "window must cover at least one frame");
    let n = wave.len() as i64;
    let raw_start = (first * HOP) as i64 - (N_FFT / 2) as i64;
    let raw_end = ((last - 1) * HOP) as i64 - (N_FFT / 2) as i64 + N_FFT as i64;
    let lookback = raw_start - 1;
    let real_start = lookback.max(0);
    let real_end = real_start.max(n.min(raw_end));
    let seg = &wave[real_start as usize..real_end as usize];

    let (pre, content_start): (Vec<f32>, i64) = if lookback >= 0 {
        (
            seg.windows(2).map(|w| w[1] - PREEMPHASIS * w[0]).collect(),
            raw_start,
        )
    } else {
        let mut p = seg.to_vec();
        for i in (1..seg.len()).rev() {
            p[i] = seg[i] - PREEMPHASIS * seg[i - 1];
        }
        (p, real_start)
    };
    let left = (content_start - raw_start) as usize;
    let right = (raw_end - real_end) as usize;
    let mut out = vec![0.0f32; left];
    out.extend_from_slice(&pre);
    out.resize(left + pre.len() + right, 0.0);
    assert_eq!(out.len() as i64, raw_end - raw_start, "window length");
    out
}

#[cfg(feature = "local-stt-parakeet")]
pub fn diarize(pcm_16k: &[f32]) -> Result<Diarization, String> {
    use ort::session::{builder::GraphOptimizationLevel, Session};
    use ort::value::Tensor;

    assert!(
        pcm_16k.iter().all(|s| s.is_finite()),
        "diarize: pcm must be finite"
    );
    if pcm_16k.len() < N_FFT {
        return Ok(Diarization::default());
    }
    let dir = model_dir().ok_or("config dir unknown")?;
    if !model_present() {
        return Err("diarization model not downloaded".into());
    }
    let _no_throttle = crate::win_qos::NoThrottle::for_local_inference();

    // Four threads. ORT's default (one per physical core) was 5-7x SLOWER on
    // a 6P+8E i7-12700H: the pool straddles E-cores and every step waits for
    // the slowest one. 4 threads measured 78x realtime offline.
    const THREADS: usize = 4;
    let session = |name: &str| -> Result<Session, String> {
        Session::builder()
            .and_then(|b| b.with_intra_threads(THREADS))
            .and_then(|b| b.with_inter_threads(1))
            .and_then(|b| b.with_optimization_level(GraphOptimizationLevel::Level3))
            .and_then(|b| b.commit_from_file(dir.join(name)))
            .map_err(|e| format!("load {name}: {e}"))
    };
    let mut pre = session(FILE_PREPROCESSOR)?;
    let mut model = session(FILE_MODEL)?;

    // 1. Log-mel [n_mel, 128], windowed.
    let n_mel = 1 + pcm_16k.len() / HOP;
    let mut mel = vec![0.0f32; n_mel * N_MELS];
    let mut first = 0;
    while first < n_mel {
        let last = (first + MEL_WINDOW_FRAMES).min(n_mel);
        let input = mel_window_input(pcm_16k, first, last);
        let len = input.len() as i64;
        let t = Tensor::from_array((vec![1i64, len], input)).map_err(|e| format!("mel in: {e}"))?;
        let out = pre
            .run(ort::inputs! { "preemphasized" => t })
            .map_err(|e| format!("mel run: {e}"))?;
        let (_, data) = out["log_mel"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("mel out: {e}"))?;
        let frames = last - first;
        assert!(
            data.len() >= frames * N_MELS,
            "preprocessor returned too few frames"
        );
        mel[first * N_MELS..last * N_MELS].copy_from_slice(&data[..frames * N_MELS]);
        first = last;
    }
    // The reference zeroes the trailing frame and treats it as padding.
    mel[(n_mel - 1) * N_MELS..].fill(0.0);
    let valid_mel = n_mel - 1;

    // 2. Chunked encoding with the speaker cache threaded between chunks.
    let n_emb = n_mel.div_ceil(SUBSAMPLING);
    let mut cache = SpeakerCache::new();
    let mut logits_all: Vec<f32> = Vec::with_capacity(n_emb * SUBSAMPLING * NUM_SPEAKERS);
    let mut start = 0;
    while start < n_emb {
        let end = (start + CHUNK_LEN).min(n_emb);
        let n_chunk = end - start;
        let mel_start = start * SUBSAMPLING;
        let mel_end = ((end + CHUNK_RIGHT_CONTEXT) * SUBSAMPLING).min(n_mel);
        let chunk = mel[mel_start * N_MELS..mel_end * N_MELS].to_vec();
        let chunk_len = mel_end.min(valid_mel).saturating_sub(mel_start) as i64;
        let ctx = cache.context();
        let ctx_frames = ctx.len() / HIDDEN;

        // The first chunk has no context. `from_array` refuses a zero-length
        // dimension, so that one empty tensor comes from the allocator.
        let ctx_t = if ctx_frames == 0 {
            Tensor::<f32>::new(&ort::memory::Allocator::default(), [1usize, 0, HIDDEN])
        } else {
            Tensor::from_array((vec![1i64, ctx_frames as i64, HIDDEN as i64], ctx))
        }
        .map_err(|e| format!("ctx: {e}"))?;
        let chunk_t = Tensor::from_array((
            vec![1i64, (mel_end - mel_start) as i64, N_MELS as i64],
            chunk,
        ))
        .map_err(|e| format!("chunk: {e}"))?;
        let chunk_len_t = Tensor::from_array((Vec::<i64>::new(), vec![chunk_len]))
            .map_err(|e| format!("chunk len: {e}"))?;
        let ctx_len_t = Tensor::from_array((Vec::<i64>::new(), vec![ctx_frames as i64]))
            .map_err(|e| format!("ctx len: {e}"))?;
        let out = model
            .run(ort::inputs! {
                "chunk_mel" => chunk_t,
                "chunk_mel_length" => chunk_len_t,
                "context_embeds" => ctx_t,
                "context_length" => ctx_len_t,
            })
            .map_err(|e| format!("model run: {e}"))?;
        let (_, logits) = out["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("logits: {e}"))?;
        let (_, embeds) = out["embeds"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("embeds: {e}"))?;
        cache.update(embeds, logits, n_chunk);

        let a = ctx_frames * SUBSAMPLING * NUM_SPEAKERS;
        let b = (ctx_frames + n_chunk) * SUBSAMPLING * NUM_SPEAKERS;
        logits_all.extend_from_slice(&logits[a..b.min(logits.len())]);
        start = end;
    }

    let probs: Vec<[f32; NUM_SPEAKERS]> = logits_all
        .chunks_exact(NUM_SPEAKERS)
        .take(n_mel)
        .map(|r| std::array::from_fn(|k| sigmoid(r[k])))
        .collect();
    assert!(
        probs
            .iter()
            .flatten()
            .all(|p| p.is_finite() && (0.0..=1.0).contains(p)),
        "speaker probabilities must be finite and in [0, 1]"
    );
    Ok(Diarization { probs })
}

#[cfg(not(feature = "local-stt-parakeet"))]
pub fn diarize(_pcm_16k: &[f32]) -> Result<Diarization, String> {
    Err("diarization requires the local-stt-parakeet cargo feature".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diar(rows: &[(usize, usize)]) -> Diarization {
        // rows: (speaker, frames) runs back to back; speaker 99 = silence
        let mut probs = Vec::new();
        for &(s, n) in rows {
            for _ in 0..n {
                let mut r = [0.0; NUM_SPEAKERS];
                if s < NUM_SPEAKERS {
                    r[s] = 0.9;
                }
                probs.push(r);
            }
        }
        Diarization { probs }
    }

    fn w(start: f64, end: f64, t: &str) -> Word {
        Word {
            start,
            end,
            text: t.into(),
        }
    }

    fn band(
        band: &'static str,
        rows: &[(usize, usize)],
        turns: &[(u128, Option<usize>, &str)],
    ) -> BandTurns {
        BandTurns {
            band,
            diar: diar(rows),
            turns: turns
                .iter()
                .map(|(m, s, t)| (*m, *s, t.to_string()))
                .collect(),
        }
    }

    fn spk(id: &str, name: &str) -> SpeakerInfo {
        SpeakerInfo {
            id: id.into(),
            name: name.into(),
            band: "system".into(),
            talk_secs: 1.0,
            segments: vec![],
        }
    }

    #[test]
    fn system_voices_get_ids_and_a_lone_mic_stays_mic() {
        let sys = band(
            "system",
            &[(0, 200), (1, 200)],
            &[(0, Some(0), "ciao"), (2000, Some(1), "salve")],
        );
        let mic = band("mic", &[(0, 300)], &[(1000, Some(0), "eccomi")]);
        let (lines, speakers) = label_bands(vec![mic, sys], &[]);
        let labels: Vec<&str> = lines.iter().map(|l| l.1.as_str()).collect();
        assert_eq!(labels, vec!["Speaker 1", "mic", "Speaker 2"]);
        assert_eq!(speakers.len(), 2);
        assert_eq!(
            (speakers[0].id.as_str(), speakers[0].band.as_str()),
            ("S1", "system")
        );
        assert!((speakers[1].talk_secs - 2.0).abs() < 0.02);
    }

    #[test]
    fn an_in_room_mic_with_two_voices_is_split_too() {
        let mic = band(
            "mic",
            &[(0, 200), (1, 200)],
            &[(0, Some(0), "a"), (2000, Some(1), "b")],
        );
        let (lines, speakers) = label_bands(vec![mic], &[]);
        assert_eq!(lines[1].1, "Speaker 2");
        assert_eq!(speakers[1].band, "mic");
    }

    #[test]
    fn prior_names_survive_a_rerun() {
        let sys = band("system", &[(0, 200)], &[(0, Some(0), "ciao")]);
        let (lines, speakers) = label_bands(vec![sys], &[spk("S1", "Marco")]);
        assert_eq!(lines[0].1, "Marco");
        assert_eq!(speakers[0].name, "Marco");
    }

    #[test]
    fn relabel_touches_only_the_label_slot() {
        let t = "[00:00:01] [Speaker 1] ciao\n[00:00:02] [mic] ho detto [Speaker 1] ieri\n[00:00:03] [Speaker 10] no\n";
        assert_eq!(
            relabel_lines(t, "Speaker 1", "Marco"),
            "[00:00:01] [Marco] ciao\n[00:00:02] [mic] ho detto [Speaker 1] ieri\n[00:00:03] [Speaker 10] no\n"
        );
    }

    #[test]
    fn rename_rewrites_transcript_and_speakers_and_refuses_bad_names() {
        let dir = std::env::temp_dir().join(format!("dimmy-diar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("transcripts.txt"),
            "[00:00:01] [Speaker 1] ciao\n[00:00:02] [Speaker 2] salve\n",
        )
        .unwrap();
        save_speakers(&dir, &[spk("S1", "Speaker 1"), spk("S2", "Speaker 2")]).unwrap();

        assert_eq!(rename_speaker(&dir, "S1", "  Marco "), Ok("Marco".into()));
        assert_eq!(
            rename_speaker(&dir, "S2", "marco"),
            Err(RenameError::Duplicate)
        );
        assert_eq!(
            rename_speaker(&dir, "S2", "mic"),
            Err(RenameError::InvalidName)
        );
        assert_eq!(
            rename_speaker(&dir, "S2", "a]b"),
            Err(RenameError::InvalidName)
        );
        assert_eq!(
            rename_speaker(&dir, "S9", "Anna"),
            Err(RenameError::NotFound)
        );
        assert_eq!(
            rename_speaker(&dir, "S1", "Marco Rossi"),
            Ok("Marco Rossi".into())
        );

        let t = std::fs::read_to_string(dir.join("transcripts.txt")).unwrap();
        assert_eq!(
            t,
            "[00:00:01] [Marco Rossi] ciao\n[00:00:02] [Speaker 2] salve\n"
        );
        assert_eq!(load_speakers(&dir)[0].name, "Marco Rossi");
        save_speakers(&dir, &[]).unwrap();
        assert!(
            !dir.join(SPEAKERS_FILE).exists(),
            "an empty list removes the file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn span_goes_to_the_dominant_speaker() {
        let d = diar(&[(0, 100), (1, 300)]);
        assert_eq!(d.speaker_for_span(0.0, 0.5), Some(0));
        assert_eq!(d.speaker_for_span(0.8, 2.5), Some(1));
    }

    #[test]
    fn span_in_a_short_gap_borrows_the_busiest_neighbour() {
        let d = diar(&[(0, 100), (99, 30), (1, 100)]);
        assert_eq!(d.speaker_for_span(1.25, 1.29), Some(1));
        assert_eq!(d.speaker_for_span(1.01, 1.05), Some(0));
        let silent = diar(&[(99, 500)]);
        assert_eq!(silent.speaker_for_span(1.0, 2.0), None);
    }

    #[test]
    fn words_group_into_turns_on_speaker_change() {
        let d = diar(&[(0, 200), (1, 200)]);
        let words = [
            w(0.1, 0.4, "ciao"),
            w(0.5, 0.9, "Marco"),
            w(2.1, 2.5, "ciao"),
            w(2.6, 3.0, "Anna"),
        ];
        let g = group_words(&words, &d);
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].1, g[0].2.as_str()), (Some(0), "ciao Marco"));
        assert_eq!(
            (g[1].0, g[1].1, g[1].2.as_str()),
            (2100, Some(1), "ciao Anna")
        );
    }

    #[test]
    fn unplaced_word_keeps_the_previous_speaker() {
        let d = diar(&[(0, 100), (99, 400)]);
        let g = group_words(&[w(0.2, 0.6, "va"), w(3.0, 3.3, "bene")], &d);
        assert_eq!(g.len(), 2, "a long pause still starts a new line");
        assert_eq!(g[1].1, Some(0));
    }

    #[test]
    fn speakers_ignores_ghosts_below_the_floor() {
        let d = diar(&[(0, 300), (2, 20), (1, 300)]);
        assert_eq!(d.speakers(1.0), vec![0, 1]);
    }

    #[test]
    fn segments_cover_each_run() {
        let d = diar(&[(0, 100), (1, 50), (0, 20)]);
        let s = d.segments();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].0, 0);
        assert!((s[1].1 - 1.0).abs() < 1e-9 && (s[1].2 - 1.5).abs() < 1e-9);
    }

    #[test]
    fn stable_topk_breaks_ties_toward_lower_index() {
        assert_eq!(
            stable_topk(&[3.0, 3.0, 5.0, 3.0, f32::NEG_INFINITY], 3),
            vec![2, 0, 1]
        );
    }

    #[test]
    fn mel_windows_match_a_single_whole_file_window() {
        let wave: Vec<f32> = (0..16_000)
            .map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5)
            .collect();
        let n_mel = 1 + wave.len() / HOP;
        let whole = mel_window_input(&wave, 0, n_mel);
        let (a, b) = (
            mel_window_input(&wave, 0, 40),
            mel_window_input(&wave, 40, n_mel),
        );
        // Frame f of a window starts at (f - first) * HOP in that window.
        let f = 40;
        assert_eq!(&b[..N_FFT], &whole[f * HOP..f * HOP + N_FFT]);
        assert_eq!(&a[..N_FFT], &whole[..N_FFT]);
        assert_eq!(whole[N_FFT / 2], wave[0], "first real sample is kept as-is");
    }

    #[test]
    fn cache_compresses_to_capacity_and_keeps_silence_slots() {
        let mut c = SpeakerCache::new();
        // Two long synthetic chunks with alternating speakers.
        for chunk in 0..3 {
            let frames = CHUNK_LEN + CHUNK_RIGHT_CONTEXT;
            let ctx = c.context().len() / HIDDEN;
            let total = ctx + frames;
            let embeds: Vec<f32> = (0..total * HIDDEN)
                .map(|i| ((i + chunk) % 13) as f32)
                .collect();
            let logits: Vec<f32> = (0..total * SUBSAMPLING)
                .flat_map(|r| {
                    let mut row = [-5.0f32; NUM_SPEAKERS];
                    row[(r / 400) % 3] = 5.0;
                    row
                })
                .collect();
            c.update(&embeds, &logits, CHUNK_LEN);
        }
        assert_eq!(c.n_cache(), CACHE_LEN);
        assert!(c.compressed);
        assert_eq!(c.fifo.len() / HIDDEN, FIFO_LEN);
        let silent_rows = c
            .probs
            .iter()
            .filter(|r| r.iter().all(|&p| p == 0.0))
            .count();
        assert!(
            silent_rows >= NUM_SPEAKERS,
            "each speaker keeps a silence slot"
        );
    }

    #[test]
    fn silence_embedding_asset_is_well_formed() {
        assert_eq!(SILENCE_EMBEDS.len(), HIDDEN * 4);
        assert_eq!(SpeakerCache::new().silence.len(), HIDDEN);
    }
}
