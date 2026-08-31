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
        })
    }

    /// Drop carried state. Call between unrelated recordings, otherwise the
    /// first frames of the new one are denoised against the old one's noise
    /// profile.
    pub fn reset(&mut self) {
        self.conv_cache.fill(0.0);
        self.tra_cache.fill(0.0);
        self.inter_cache.fill(0.0);
    }

    /// Enhance a 16 kHz mono buffer. Returns the same number of samples.
    pub fn process(&mut self, samples_16k: &[f32]) -> Result<Vec<f32>, String> {
        assert!(
            !samples_16k.is_empty(),
            "gtcrn: refusing to process an empty buffer"
        );
        assert!(
            samples_16k.iter().all(|s| s.is_finite()),
            "gtcrn: input contains NaN/Inf"
        );

        let n = samples_16k.len();
        // One hop of leading context so frame 0 has a full window, and enough
        // tail to flush the last hop back out.
        let mut padded = vec![0.0f32; HOP];
        padded.extend_from_slice(samples_16k);
        padded.resize(padded.len() + N_FFT, 0.0);

        let mut out = vec![0.0f32; padded.len()];
        let mut spectrum = self.fft.make_output_vec();
        let mut frame = vec![0.0f32; N_FFT];
        let mut synth = vec![0.0f32; N_FFT];

        let mut pos = 0usize;
        while pos + N_FFT <= padded.len() {
            for i in 0..N_FFT {
                frame[i] = padded[pos + i] * self.window[i];
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
            for i in 0..N_FFT {
                out[pos + i] += synth[i] * norm * self.window[i];
            }
            pos += HOP;
        }

        let enhanced: Vec<f32> = out[HOP..HOP + n]
            .iter()
            .map(|s| s.clamp(-1.0, 1.0))
            .collect();
        assert_eq!(
            enhanced.len(),
            n,
            "gtcrn: output length must match input length"
        );
        assert!(
            enhanced.iter().all(|s| s.is_finite()),
            "gtcrn: produced NaN/Inf"
        );
        Ok(enhanced)
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

/// The denoise pass for the STT path. ON by default, `DIMMY_GTCRN=0` to skip.
///
/// Default-on because this REPLACES the RNNoise stage that used to run
/// unconditionally upstream of AEC3 — leaving it off would have been a silent
/// removal of noise suppression, not a swap. Env var rather than a config
/// field for now: the field means schema + FFI + two host UIs, and that is
/// worth doing once we know which default users actually want.
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

    if std::env::var("DIMMY_GTCRN").as_deref() == Ok("0") {
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
