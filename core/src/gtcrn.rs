//! GTCRN speech enhancement — streaming denoiser over ONNX Runtime.
//!
//! Candidate replacement for the RNNoise (2017) suppressor we run today.
//! 48.2 K parameters and 33 MMACs/s, so the cost is close to nothing; the
//! `gtcrn_simple.onnx` file is ~535 KB, small enough to bundle the way we
//! already bundle the Silero VAD model.
//!
//! The model is frame-by-frame with carried state, which is what makes it
//! usable on the realtime path and not just offline:
//!
//! | tensor        | shape                | role                       |
//! |---------------|----------------------|----------------------------|
//! | `mix`         | `[1, 257, 1, 2]`     | one STFT frame, (re, im)   |
//! | `conv_cache`  | `[2, 1, 16, 16, 33]` | carried between frames     |
//! | `tra_cache`   | `[2, 3, 1, 1, 16]`   | carried between frames     |
//! | `inter_cache` | `[2, 1, 33, 16]`     | carried between frames     |
//!
//! Analysis parameters come from the model's own ONNX metadata rather than
//! from a blog post: `n_fft=512`, `hop_length=256`, `window_type=hann_sqrt`,
//! `sample_rate=16000`. The sqrt-Hann window is applied on BOTH analysis and
//! synthesis, so the two passes multiply to a plain Hann, which sums to unity
//! at 50 % overlap. Get that wrong and the output is amplitude-modulated at
//! the frame rate — audible as a 62.5 Hz buzz, not as "slightly worse".
//!
//! 16 kHz is not a limitation for us: STT already consumes 16 kHz, so this
//! sits naturally between the resampler and the model. It is NOT a drop-in
//! for the 48 kHz capture path, and deliberately so.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const MODEL_FILE: &str = "gtcrn_simple.onnx";
pub const REQUIRED_RATE: u32 = 16_000;

const N_FFT: usize = 512;
const HOP: usize = 256;
const BINS: usize = N_FFT / 2 + 1; // 257

/// ONNX Runtime intra-op threads for this session. See the rationale where
/// the session is built: more threads is measurably SLOWER here, because the
/// graph is tiny and runs once per frame.
const INTRA_THREADS: usize = 2;

/// Cache shapes exactly as the ONNX graph declares them. Kept as arrays so
/// the tensor construction below and the buffer sizes cannot drift apart.
const CONV_CACHE_SHAPE: [i64; 5] = [2, 1, 16, 16, 33];
const TRA_CACHE_SHAPE: [i64; 5] = [2, 3, 1, 1, 16];
const INTER_CACHE_SHAPE: [i64; 4] = [2, 1, 33, 16];

const fn numel(shape: &[i64]) -> usize {
    let mut n = 1usize;
    let mut i = 0;
    while i < shape.len() {
        n *= shape[i] as usize;
        i += 1;
    }
    n
}

const CONV_CACHE_LEN: usize = numel(&CONV_CACHE_SHAPE);
const TRA_CACHE_LEN: usize = numel(&TRA_CACHE_SHAPE);
const INTER_CACHE_LEN: usize = numel(&INTER_CACHE_SHAPE);

pub struct GtcrnDenoiser {
    session: Session,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    window: Vec<f32>,
    conv_cache: Vec<f32>,
    tra_cache: Vec<f32>,
    inter_cache: Vec<f32>,

    // ── Streaming state ────────────────────────────────────────────────
    // The denoiser is frame-by-frame by construction (see `run_frame`), so
    // the only thing standing between it and a live stream is the STFT
    // framing. Keeping that framing here — instead of rebuilding it per call
    // — is what makes `push` chunk-size-independent: feeding a recording in
    // 100 ms slices produces the SAME samples as feeding it in one go, which
    // `process` relies on and `streaming_matches_batch_exactly` pins.
    //
    // `buf` and `ola` are windows onto an absolute timeline that starts with
    // `HOP` samples of zero padding, so absolute index `HOP` is input sample 0.
    /// Padded input still needed by a future frame, starting at `base`.
    buf: Vec<f32>,
    /// Overlap-add accumulator covering the same range as `buf`.
    ola: Vec<f32>,
    /// Absolute index of `buf[0]` / `ola[0]`.
    base: usize,
    /// Absolute position of the next frame to run.
    next_frame: usize,
    /// Absolute index of the next output sample to emit.
    produced: usize,
    /// Input samples pushed since the last `reset`.
    pushed: usize,
}

impl GtcrnDenoiser {
    pub fn load(model_path: &Path) -> Result<Self, String> {
        // NOT an assert. A missing model file is an environment state, not a
        // broken invariant: the documented contract is that `maybe_denoise_16k`
        // passes audio through when the model is absent. An assert here panics
        // across the `extern "C"` FFI boundary — which cannot unwind — so the
        // whole host process aborts mid-transcription instead of degrading.
        // Burned 2026-08-11: a local build without the asset copied next to the
        // exe took the app down on the first recording.
        if !model_path.is_file() {
            return Err(format!("gtcrn model missing at {}", model_path.display()));
        }

        // No explicit optimisation level: this ONNX Runtime build rejects the
        // setting, and at 48.2K parameters graph optimisation buys nothing
        // worth a compatibility risk.
        //
        // The thread cap is NOT a micro-optimisation. This graph is invoked
        // once per 16 ms STFT frame (see `process`), so a 25 s dictation is
        // ~1560 separate `run()` calls of ~0.5 MMACs each. At that size the
        // arithmetic is free and the whole cost is thread-pool wake/park per
        // call, which grows with the pool. ORT defaults to one thread per
        // physical core; measured on an i7-12700H (14 cores):
        //
        //   threads=14 -> 1.95 ms/frame     threads=2 -> 1.11 ms/frame
        //   threads=8  -> 1.26 ms/frame     threads=1 -> 1.22 ms/frame
        //
        // The default is the worst value available, and the wide pool also
        // makes the cost hostage to whatever else is scheduling on the box:
        // the same binary, model and audio measured RTF 0.11 one day and
        // RTF 0.87 three days later, a 21.7 s stall in front of a 2 s
        // whisper pass. Two threads is the measured floor and is stable.
        let session = Session::builder()
            .map_err(|e| format!("gtcrn session builder: {e}"))?
            .with_intra_threads(INTRA_THREADS)
            .map_err(|e| format!("gtcrn intra threads: {e}"))?
            .with_inter_threads(1)
            .map_err(|e| format!("gtcrn inter threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("gtcrn load {}: {e}", model_path.display()))?;

        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N_FFT);
        let ifft = planner.plan_fft_inverse(N_FFT);

        // sqrt-Hann, applied on analysis AND synthesis (see module docs).
        let window: Vec<f32> = (0..N_FFT)
            .map(|i| {
                let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N_FFT as f32).cos();
                hann.max(0.0).sqrt()
            })
            .collect();
        assert_eq!(window.len(), N_FFT);
        assert!(window.iter().all(|w| w.is_finite()));

        Ok(Self {
            session,
            fft,
            ifft,
            window,
            conv_cache: vec![0.0; CONV_CACHE_LEN],
            tra_cache: vec![0.0; TRA_CACHE_LEN],
            inter_cache: vec![0.0; INTER_CACHE_LEN],
            buf: vec![0.0; HOP],
            ola: vec![0.0; HOP],
            base: 0,
            next_frame: 0,
            produced: HOP,
            pushed: 0,
        })
    }

    /// Drop carried state. Call between unrelated recordings, otherwise the
    /// first frames of the new one are denoised against the old one's noise
    /// profile.
    pub fn reset(&mut self) {
        self.conv_cache.fill(0.0);
        self.tra_cache.fill(0.0);
        self.inter_cache.fill(0.0);
        // One hop of leading context so frame 0 sees a full window.
        self.buf.clear();
        self.buf.resize(HOP, 0.0);
        self.ola.clear();
        self.ola.resize(HOP, 0.0);
        self.base = 0;
        self.next_frame = 0;
        self.produced = HOP;
        self.pushed = 0;
    }

    /// Enhance a 16 kHz mono buffer in one shot. Returns the same number of
    /// samples.
    ///
    /// A thin wrapper over the streaming API so there is exactly one framing
    /// implementation: whatever this returns, feeding the same audio through
    /// `push`/`flush` in arbitrary slices returns byte-for-byte the same thing.
    pub fn process(&mut self, samples_16k: &[f32]) -> Result<Vec<f32>, String> {
        assert!(
            !samples_16k.is_empty(),
            "gtcrn: refusing to process an empty buffer"
        );
        assert!(
            samples_16k.iter().all(|s| s.is_finite()),
            "gtcrn: input contains NaN/Inf"
        );

        self.reset();
        let mut enhanced = self.push(samples_16k)?;
        enhanced.extend_from_slice(&self.flush()?);

        assert_eq!(
            enhanced.len(),
            samples_16k.len(),
            "gtcrn: output length must match input length"
        );
        assert!(
            enhanced.iter().all(|s| s.is_finite()),
            "gtcrn: produced NaN/Inf"
        );
        Ok(enhanced)
    }

    /// Feed the next slice of a live recording; returns whatever output has
    /// become final. Output lags the input by up to one window, which at
    /// 16 kHz is 32 ms — the rest arrives from `flush`.
    ///
    /// Chunk size is free: the caller may push 10 ms or 10 s at a time and the
    /// concatenated result is identical, because the framing state lives in
    /// `self` rather than in the call.
    pub fn push(&mut self, samples_16k: &[f32]) -> Result<Vec<f32>, String> {
        assert!(
            samples_16k.iter().all(|s| s.is_finite()),
            "gtcrn: pushed audio contains NaN/Inf"
        );
        self.buf.extend_from_slice(samples_16k);
        self.ola.resize(self.buf.len(), 0.0);
        self.pushed += samples_16k.len();
        self.drain_ready()
    }

    /// Close the stream: flush the frames still holding the tail and return the
    /// last samples. After this the denoiser is empty but its noise state is
    /// intact; call `reset` before an unrelated recording.
    pub fn flush(&mut self) -> Result<Vec<f32>, String> {
        // Enough trailing silence for the final hops to overlap-add out.
        self.buf.resize(self.buf.len() + N_FFT, 0.0);
        self.ola.resize(self.buf.len(), 0.0);
        let tail = self.drain_ready()?;
        assert_eq!(
            self.produced,
            HOP + self.pushed,
            "gtcrn: flush must emit exactly as many samples as were pushed"
        );
        Ok(tail)
    }

    /// Run every frame whose window is fully buffered, then hand back the
    /// output samples no later frame can still touch.
    fn drain_ready(&mut self) -> Result<Vec<f32>, String> {
        let mut spectrum = self.fft.make_output_vec();
        let mut frame = vec![0.0f32; N_FFT];
        let mut synth = vec![0.0f32; N_FFT];

        while self.next_frame + N_FFT <= self.base + self.buf.len() {
            let off = self.next_frame - self.base;
            for (windowed, (sample, w)) in frame
                .iter_mut()
                .zip(self.buf[off..off + N_FFT].iter().zip(&self.window))
            {
                *windowed = sample * w;
            }
            self.fft
                .process(&mut frame, &mut spectrum)
                .map_err(|e| format!("gtcrn fft: {e:?}"))?;
            debug_assert_eq!(spectrum.len(), BINS);

            let mut mix = Vec::with_capacity(BINS * 2);
            for c in spectrum.iter() {
                mix.push(c.re);
                mix.push(c.im);
            }

            let enhanced = self.run_frame(&mix)?;

            for (i, c) in spectrum.iter_mut().enumerate() {
                c.re = enhanced[i * 2];
                c.im = enhanced[i * 2 + 1];
            }
            // A real signal's DC and Nyquist bins are real by definition, but
            // the network emits a small residue there. realfft rejects the
            // buffer outright rather than ignoring it, so zero them.
            spectrum[0].im = 0.0;
            spectrum[BINS - 1].im = 0.0;
            self.ifft
                .process(&mut spectrum.clone(), &mut synth)
                .map_err(|e| format!("gtcrn ifft: {e:?}"))?;

            // realfft's inverse is unnormalised.
            let norm = 1.0 / N_FFT as f32;
            for ((acc, s), w) in self.ola[off..off + N_FFT]
                .iter_mut()
                .zip(&synth)
                .zip(&self.window)
            {
                *acc += s * norm * w;
            }
            self.next_frame += HOP;
        }

        // Frames run in order and each one starts a hop later than the last, so
        // once the frame at `pos` has run nothing can still write below
        // `pos + HOP` — which is exactly `next_frame`.
        let limit = (HOP + self.pushed).min(self.next_frame);
        let out: Vec<f32> = if limit > self.produced {
            self.ola[self.produced - self.base..limit - self.base]
                .iter()
                .map(|s| s.clamp(-1.0, 1.0))
                .collect()
        } else {
            Vec::new()
        };
        self.produced = self.produced.max(limit);

        // Retire the prefix no future frame reads and no future output needs.
        let retire = self.produced.min(self.next_frame);
        if retire > self.base {
            let n = retire - self.base;
            self.buf.drain(..n);
            self.ola.drain(..n);
            self.base = retire;
        }

        assert!(out.iter().all(|s| s.is_finite()), "gtcrn: produced NaN/Inf");
        Ok(out)
    }

    fn run_frame(&mut self, mix: &[f32]) -> Result<Vec<f32>, String> {
        debug_assert_eq!(mix.len(), BINS * 2);

        let mix_t = Tensor::from_array((vec![1i64, BINS as i64, 1, 2], mix.to_vec()))
            .map_err(|e| format!("gtcrn mk mix: {e}"))?;
        let conv_t = Tensor::from_array((
            CONV_CACHE_SHAPE.to_vec(),
            std::mem::take(&mut self.conv_cache),
        ))
        .map_err(|e| format!("gtcrn mk conv_cache: {e}"))?;
        let tra_t = Tensor::from_array((
            TRA_CACHE_SHAPE.to_vec(),
            std::mem::take(&mut self.tra_cache),
        ))
        .map_err(|e| format!("gtcrn mk tra_cache: {e}"))?;
        let inter_t = Tensor::from_array((
            INTER_CACHE_SHAPE.to_vec(),
            std::mem::take(&mut self.inter_cache),
        ))
        .map_err(|e| format!("gtcrn mk inter_cache: {e}"))?;

        let outs = self
            .session
            .run(ort::inputs! {
                "mix" => mix_t,
                "conv_cache" => conv_t,
                "tra_cache" => tra_t,
                "inter_cache" => inter_t,
            })
            .map_err(|e| format!("gtcrn run: {e}"))?;

        let take = |name: &str| -> Result<Vec<f32>, String> {
            let (_, data) = outs[name]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("gtcrn extract {name}: {e}"))?;
            Ok(data.to_vec())
        };

        let enh = take("enh")?;
        self.conv_cache = take("conv_cache_out")?;
        self.tra_cache = take("tra_cache_out")?;
        self.inter_cache = take("inter_cache_out")?;

        assert_eq!(enh.len(), BINS * 2, "gtcrn: unexpected 'enh' length");
        assert_eq!(self.conv_cache.len(), CONV_CACHE_LEN);
        assert_eq!(self.tra_cache.len(), TRA_CACHE_LEN);
        assert_eq!(self.inter_cache.len(), INTER_CACHE_LEN);
        Ok(enh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_err_for_a_missing_model_instead_of_panicking() {
        // Regression: this used to be an assert!, so a tree without the
        // bundled asset panicked across the extern "C" FFI boundary — which
        // cannot unwind — and aborted the whole host process on the first
        // recording. A missing optional asset must degrade, not crash.
        let missing = std::path::Path::new("definitely-not-a-real-gtcrn-model.onnx");
        assert!(!missing.is_file(), "test fixture must not exist");
        let err = GtcrnDenoiser::load(missing)
            .err()
            .expect("missing model must be an Err, never a panic");
        assert!(err.contains("missing"), "unexpected error text: {err}");
    }

    #[test]
    fn missing_model_passes_audio_through_unchanged() {
        // The documented contract of the whole module: no model, no change.
        // Guards the wiring between load()'s Err and the passthrough branch.
        let input: Vec<f32> = (0..1600).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = maybe_denoise_16k(&input);
        if matches!(out, std::borrow::Cow::Borrowed(_)) {
            assert_eq!(&*out, &input[..], "passthrough must not alter samples");
        }
        // When the asset IS present the denoiser legitimately returns Owned
        // audio, so only the borrowed case is asserted — the point of this
        // test is that neither path aborts the process.
    }

    #[test]
    fn the_denoise_is_off_unless_explicitly_asked_for() {
        // Measured on 70 real dictations 2026-08-31: denoising cost half the
        // recordings nothing at all and cost the other half punctuation,
        // capitalisation and occasionally whole words. Silence is not a
        // neutral default here — it is the measured better one, and a stray
        // value in the environment must not quietly switch it back on.
        assert!(!denoise_enabled_from(None), "unset must mean off");
        assert!(!denoise_enabled_from(Some("")), "empty must mean off");
        assert!(!denoise_enabled_from(Some("0")), "0 must mean off");
        // Only the one explicit opt-in turns it on. Not "true", not "yes":
        // a typo must fail closed, toward the default that measured better.
        assert!(denoise_enabled_from(Some("1")), "1 must mean on");
        for typo in ["true", "TRUE", "yes", "on", "2", " 1"] {
            assert!(
                !denoise_enabled_from(Some(typo)),
                "{typo:?} must not enable the denoise"
            );
        }
    }

    #[test]
    fn window_squared_sums_to_unity_at_fifty_percent_overlap() {
        // The COLA property the overlap-add relies on. If this drifts the
        // output gets amplitude-modulated at the frame rate rather than
        // sounding "a bit off", so it is worth pinning independently of the
        // model being present.
        let window: Vec<f32> = (0..N_FFT)
            .map(|i| {
                let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N_FFT as f32).cos();
                hann.max(0.0).sqrt()
            })
            .collect();
        for i in 0..HOP {
            let sum = window[i] * window[i] + window[i + HOP] * window[i + HOP];
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "COLA violated at {i}: {sum} (analysis*synthesis must sum to 1)"
            );
        }
    }

    #[test]
    fn intra_threads_is_capped_well_below_the_ort_default() {
        // ORT defaults the intra-op pool to one thread per physical core.
        // That default is actively harmful for this graph: it is invoked once
        // per 16 ms frame, so pool wake/park dominates and MORE threads means
        // MORE time per frame (1.95 ms at 14 threads vs 1.11 ms at 2 on the
        // machine this was measured on). Pinning the constant so a future
        // "let ORT decide" cleanup has to argue with the measurement.
        assert!(
            INTRA_THREADS >= 1,
            "an intra-op pool of 0 threads is not a valid ORT configuration"
        );
        let default_pool = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        assert!(
            INTRA_THREADS <= default_pool,
            "cap must not EXCEED what ORT would have chosen ({default_pool}),              otherwise it is not a cap"
        );
        assert!(
            INTRA_THREADS <= 4,
            "measured optimum is 2; anything above 4 is back in the regime              where the pool costs more than the arithmetic"
        );
    }

    #[test]
    fn cache_lengths_match_the_documented_shapes() {
        assert_eq!(CONV_CACHE_LEN, 16_896);
        assert_eq!(TRA_CACHE_LEN, 96);
        assert_eq!(INTER_CACHE_LEN, 1_056);
        assert_eq!(BINS, 257);
    }
}

/// Is the denoise pass switched on? OFF by default, `DIMMY_GTCRN=1` to enable.
///
/// **It was default-ON from 2026-08-10 to 2026-08-31, and that was wrong.**
/// The original reasoning — that it REPLACED an RNNoise stage, so leaving it
/// off would be a silent removal of noise suppression rather than a swap — only
/// ever weighed levels and latency. Nobody measured what it did to the TEXT,
/// and the comment here said the default would be settled "once we know which
/// default users actually want". It was settled on 2026-08-31 by measuring, on
/// 70 real dictations, with `core/src/bin/denoise_ab.rs`:
///
/// - 38 of 70 transcripts came out **identical** — half the recordings paid for
///   nothing at all;
/// - across the 32 that differed, denoised audio lost **33% of the punctuation
///   and 53% of the capitalisation**;
/// - the 8 "Grazie"-shaped hallucinations on near-silent clips appeared in
///   BOTH arms — it removed none of the failures it was partly there for;
/// - reading the 10 most divergent pairs by hand: raw won 6, tied 2, one was
///   ambiguous, denoised won 0, and one clean sentence came back as garbage
///   ("Non ho registrato proprio niente" → "Uzzanno nel registrato
///   popioniente").
///
/// That matches the published result that speech enhancement in front of a
/// modern ASR usually hurts (arXiv:2512.17562 found raw beating enhanced in
/// 40 of 40 configurations): whisper trained on vast noisy audio is already
/// noise-robust, while the enhancer's artifacts are a distribution it has
/// never seen.
///
/// The code stays, and so does the switch: a genuinely loud room, or the
/// meeting path's system audio, was never covered by that measurement. It just
/// no longer runs unless asked.
fn denoise_enabled() -> bool {
    denoise_enabled_from(std::env::var("DIMMY_GTCRN").ok().as_deref())
}

/// The decision itself, split out so it can be tested without mutating a
/// process-global env var underneath tests running in parallel.
fn denoise_enabled_from(value: Option<&str>) -> bool {
    value == Some("1")
}

/// The denoise pass for the STT path. See [`denoise_enabled`] for why it is
/// off unless asked.
///
/// Placed on the 16 kHz STT input rather than in the capture chain, so the
/// audio we ARCHIVE keeps its full band and stays re-transcribable from the
/// original signal.
///
/// Returns the input unchanged whenever it is off, the model is missing, or
/// the model errors. Silently degrading to the original audio is the right
/// failure mode here: a denoiser is an enhancement, never a gate.
pub fn maybe_denoise_16k(samples_16k: &[f32]) -> std::borrow::Cow<'_, [f32]> {
    use std::borrow::Cow;
    use std::sync::Mutex;

    if !denoise_enabled() {
        return Cow::Borrowed(samples_16k);
    }
    if samples_16k.is_empty() {
        return Cow::Borrowed(samples_16k);
    }

    static DENOISER: Mutex<Option<GtcrnDenoiser>> = Mutex::new(None);
    let Ok(mut guard) = DENOISER.lock() else {
        crate::log("[GTCRN] mutex poisoned — passing audio through");
        return Cow::Borrowed(samples_16k);
    };
    if guard.is_none() {
        let path = model_path();
        match GtcrnDenoiser::load(&path) {
            Ok(d) => {
                crate::log(&format!("[GTCRN] denoiser loaded from {}", path.display()));
                *guard = Some(d);
            }
            Err(e) => {
                crate::log(&format!("[GTCRN] load failed ({e}) — passing through"));
                return Cow::Borrowed(samples_16k);
            }
        }
    }
    let d = guard.as_mut().expect("just initialised");
    // Each call is an independent recording; carrying state across them would
    // denoise the start of this one against the previous one's noise.
    d.reset();
    // One ONNX call per 16 ms frame is the exact shape EcoQoS punishes hardest:
    // 5.72 ms/frame throttled against 1.02 ms/frame exempt, measured. See
    // `win_qos`.
    let _no_throttle = crate::win_qos::NoThrottle::for_local_inference();
    let t0 = std::time::Instant::now();
    match d.process(samples_16k) {
        Ok(out) => {
            crate::log(&format!(
                "[GTCRN] denoised {:.1}s of audio in {} ms",
                samples_16k.len() as f32 / REQUIRED_RATE as f32,
                t0.elapsed().as_millis()
            ));
            Cow::Owned(out)
        }
        Err(e) => {
            crate::log(&format!("[GTCRN] process failed ({e}) — passing through"));
            Cow::Borrowed(samples_16k)
        }
    }
}

/// Beside the executable first (bundled, like the Silero VAD model), then the
/// config dir. Mirrors `silero::model_path` so the two behave the same.
pub fn model_path() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join(MODEL_FILE);
            if beside.is_file() {
                return beside;
            }
            let resources = dir.join("../Resources").join(MODEL_FILE);
            if resources.is_file() {
                return resources;
            }
        }
    }
    crate::local_stt::model_path(MODEL_FILE)
}

/// Denoising a recording while it is still being captured.
///
/// The denoiser is bit-for-bit indifferent to how the audio is sliced (pinned
/// by `streaming_matches_batch_exactly`), so the work can be spread across the
/// recording instead of landing on the user at the end. At the measured RTF of
/// 0.057 that is roughly 6% of one core while you speak, against 5 seconds of
/// dead wait after an 87-second dictation.
///
/// Armed only for the `Raw` preprocess route. The other routes run a VAD and an
/// AGC across the WHOLE 48 kHz buffer before this stage sees anything, so the
/// denoiser input does not exist until the recording stops and there is nothing
/// to overlap; those recordings denoise at the end exactly as before. Lifting
/// that restriction means moving the VAD to 16 kHz (`silero.rs` is already in
/// the tree), which is a change to the audio pipeline in its own right.
pub mod live {
    use super::{GtcrnDenoiser, REQUIRED_RATE};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// How often the worker folds newly captured audio into the denoiser. Sets
    /// the worst-case tail left for `finish`: at RTF 0.057 a 250 ms backlog is
    /// about 14 ms of work.
    const POLL: std::time::Duration = std::time::Duration::from_millis(250);

    struct Session {
        denoiser: GtcrnDenoiser,
        downsampler: crate::preprocess::StreamingDownsampler16k,
        /// Capture-rate samples already folded in.
        consumed: usize,
        /// Denoised 16 kHz output so far.
        out: Vec<f32>,
        stop: Arc<AtomicBool>,
    }

    static SESSION: Mutex<Option<Session>> = Mutex::new(None);
    /// Result waiting for `stt_input_16k`, tagged with the capture-rate length
    /// it was built from so it can never be applied to a different recording.
    static PREPARED: Mutex<Option<(usize, Vec<f32>)>> = Mutex::new(None);

    /// Arm live denoising and start the worker that feeds it. Returns false â€”
    /// harmlessly â€” when the model is missing, the capture rate is not one the
    /// streaming downsampler can match exactly, or denoising is switched off.
    /// The recording then denoises at the end, as before.
    pub fn begin(sample_rate: u32, buffer: Arc<Mutex<Vec<f32>>>) -> bool {
        assert!(sample_rate > 0, "gtcrn live: sample_rate must be > 0");
        if !super::denoise_enabled() {
            return false;
        }
        let Some(downsampler) = crate::preprocess::StreamingDownsampler16k::new(sample_rate) else {
            crate::log(&format!(
                "[GTCRN] live denoise off: {sample_rate} Hz is not an exact multiple of {REQUIRED_RATE}"
            ));
            return false;
        };
        let mut denoiser = match GtcrnDenoiser::load(&super::model_path()) {
            Ok(d) => d,
            Err(e) => {
                crate::log(&format!("[GTCRN] live denoise off: {e}"));
                return false;
            }
        };
        denoiser.reset();

        let stop = Arc::new(AtomicBool::new(false));
        {
            let Ok(mut slot) = SESSION.lock() else {
                return false;
            };
            *slot = Some(Session {
                denoiser,
                downsampler,
                consumed: 0,
                out: Vec::new(),
                stop: stop.clone(),
            });
        }
        if let Ok(mut prepared) = PREPARED.lock() {
            *prepared = None;
        }

        std::thread::Builder::new()
            .name("dimmy-denoise".to_string())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(POLL);
                    feed_from(&buffer);
                }
            })
            .expect("denoise thread spawn must succeed");
        crate::log("[GTCRN] live denoise armed");
        true
    }

    /// Fold whatever the capture thread has appended since last time.
    fn feed_from(buffer: &Mutex<Vec<f32>>) {
        let Ok(mut slot) = SESSION.lock() else { return };
        let Some(session) = slot.as_mut() else { return };

        // Hold the capture lock only long enough to copy the tail: the mic
        // callback appends under it, and ONNX inference must never stall it.
        let tail: Vec<f32> = match buffer.lock() {
            Ok(b) if b.len() > session.consumed => b[session.consumed..].to_vec(),
            _ => return,
        };
        session.consumed += tail.len();

        let _no_throttle = crate::win_qos::NoThrottle::for_local_inference();
        let sixteen_k = session.downsampler.push(&tail);
        if sixteen_k.is_empty() {
            return;
        }
        match session.denoiser.push(&sixteen_k) {
            Ok(enhanced) => session.out.extend_from_slice(&enhanced),
            Err(e) => {
                crate::log(&format!(
                    "[GTCRN] live denoise failed ({e}) â€” falling back"
                ));
                session.stop.store(true, Ordering::Relaxed);
                *slot = None;
            }
        }
    }

    /// Close the recording: fold the tail the worker has not reached, flush,
    /// and park the result for `stt_input_16k`. A no-op when live denoising was
    /// never armed or has already given up.
    pub fn finish(raw: &[f32]) {
        let Ok(mut slot) = SESSION.lock() else { return };
        let Some(mut session) = slot.take() else {
            return;
        };
        session.stop.store(true, Ordering::Relaxed);

        let backlog = raw.len().saturating_sub(session.consumed);
        let t0 = std::time::Instant::now();
        let _no_throttle = crate::win_qos::NoThrottle::for_local_inference();

        if backlog > 0 {
            let sixteen_k = session.downsampler.push(&raw[session.consumed..]);
            session.consumed = raw.len();
            if !sixteen_k.is_empty() {
                match session.denoiser.push(&sixteen_k) {
                    Ok(enhanced) => session.out.extend_from_slice(&enhanced),
                    Err(e) => {
                        crate::log(&format!("[GTCRN] live tail failed ({e}) â€” falling back"));
                        return;
                    }
                }
            }
        }
        // The worker reads the same buffer the caller just drained, so a `raw`
        // shorter than what we consumed means the two disagree about which
        // recording this is. Fall back rather than guess.
        if session.consumed != raw.len() {
            crate::log(&format!(
                "[GTCRN] live denoise saw {} samples but the recording is {} â€” falling back",
                session.consumed,
                raw.len()
            ));
            return;
        }
        match session.denoiser.flush() {
            Ok(tail) => session.out.extend_from_slice(&tail),
            Err(e) => {
                crate::log(&format!("[GTCRN] live flush failed ({e}) â€” falling back"));
                return;
            }
        }

        let rate = capture_rate(raw.len(), session.out.len());
        crate::log(&format!(
            "[GTCRN] live denoise done: {:.1}s covered during capture, {:.1}s tail in {} ms",
            (raw.len() - backlog) as f32 / rate,
            backlog as f32 / rate,
            t0.elapsed().as_millis()
        ));

        if let Ok(mut prepared) = PREPARED.lock() {
            *prepared = Some((raw.len(), session.out));
        }
    }

    /// Capture rate implied by how much 16 kHz output a given input produced.
    /// Only ever used to make the log line read in seconds.
    fn capture_rate(raw_len: usize, out_len: usize) -> f32 {
        if out_len == 0 {
            return REQUIRED_RATE as f32;
        }
        (raw_len as f32 / out_len as f32) * REQUIRED_RATE as f32
    }

    /// Drop any armed session and any parked result. For recordings that end
    /// without reaching transcription.
    pub fn abandon() {
        if let Ok(mut slot) = SESSION.lock() {
            if let Some(session) = slot.take() {
                session.stop.store(true, Ordering::Relaxed);
            }
        }
        if let Ok(mut prepared) = PREPARED.lock() {
            *prepared = None;
        }
    }

    /// The denoised 16 kHz stream for a recording of exactly `raw_len`
    /// capture-rate samples, if one was prepared during capture. Consumed on
    /// take: a second caller gets `None` and denoises normally.
    pub fn take_prepared(raw_len: usize) -> Option<Vec<f32>> {
        let mut prepared = PREPARED.lock().ok()?;
        match prepared.take() {
            Some((len, out)) if len == raw_len => Some(out),
            Some((len, _)) => {
                crate::log(&format!(
                    "[GTCRN] parked denoise is for {len} samples, asked for {raw_len} â€” ignoring"
                ));
                None
            }
            None => None,
        }
    }
}
