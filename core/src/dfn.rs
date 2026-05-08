//! DeepFilterNet (libDF) wrapper for ML-based mic noise suppression.
//!
//! Plugs upstream of the AEC stage in the `dimmy-aec` worker: each mic
//! frame (480 samples @ 48 kHz mono) goes through DFN inference first,
//! then feeds AEC3 as the capture signal. DFN handles steady-state
//! noise (fan, HVAC, traffic, breath, keyboard typing) and even some
//! transient noise; AEC handles the speaker→mic acoustic loop. Stacked
//! the two together approximate what Krisp / NVIDIA Maxine / Zoom
//! Studio Quality offer commercially.
//!
//! Frame size + sample rate match AEC exactly so we can chain them
//! 1:1 without resampling. The DFN model is ~3 MB packed as a
//! tar.gz that contains encoder/decoder ONNX weights + a config.
//!
//! Loading strategy (in order):
//! 1. `DIMMY_DFN_MODEL_PATH` env var — power user / dev override.
//! 2. `<dimmy data dir>/models/dfn.tar.gz` — user-installed bundle.
//! 3. None → DFN disabled, mic passes through to AEC unchanged.
//!
//! The processor is created once per Mix-Start and lives for the
//! duration of the recording. Per-frame inference cost on a typical
//! laptop CPU is ~1-3 ms (vs 10 ms frame budget) so it stays
//! comfortably real-time.

// NOTE: the `local-dfn` feature is currently a no-op gate. The
// implementation below stubs out as `try_init() -> None`, which makes
// the AEC worker fall through to the AEC-only path. Once the upstream
// `deep_filter` crate becomes usable (either crates.io publishes the
// `tract` feature or we switch to `deepfilter-rt` riding the existing
// `ort` runtime), uncomment the `cfg(feature = "local-dfn")` block
// at the bottom of this file and re-enable the dependency in
// Cargo.toml.

pub struct DfnProcessor;

impl DfnProcessor {
    /// Always returns None for now (DFN deferred). When the upstream
    /// dependency issue is resolved, replace this with the real
    /// constructor under `#[cfg(feature = "local-dfn")]`.
    pub fn try_init() -> Option<Self> {
        None
    }

    #[allow(dead_code)]
    pub fn process_frame(&mut self, src: &[f32], dest: &mut [f32]) {
        dest.copy_from_slice(src);
    }
}

// ── Reference implementation (deferred — see top of file) ──────────
//
// #[cfg(feature = "local-dfn")]
// mod real {
//     use deep_filter::tract::{DfParams, DfTract, RuntimeParams};
//
//     pub struct DfnProcessor {
//         inner: DfTract,
//         scratch_in: ndarray::Array2<f32>,
//         scratch_out: ndarray::Array2<f32>,
//     }
//
//     impl DfnProcessor {
//         pub fn try_init() -> Option<Self> {
//             let path = resolve_model_path()?;
//             let params = DfParams::new(path).ok()?;
//             let runtime = RuntimeParams::default_with_ch(1);
//             let tract = DfTract::new(params, &runtime).ok()?;
//             Some(Self {
//                 inner: tract,
//                 scratch_in: ndarray::Array2::zeros((1, 480)),
//                 scratch_out: ndarray::Array2::zeros((1, 480)),
//             })
//         }
//
//         pub fn process_frame(&mut self, src: &[f32], dest: &mut [f32]) {
//             for (i, &s) in src.iter().enumerate() {
//                 self.scratch_in[(0, i)] = s;
//             }
//             if self.inner.process(self.scratch_in.view(), self.scratch_out.view_mut()).is_ok() {
//                 for (i, slot) in dest.iter_mut().enumerate() {
//                     *slot = self.scratch_out[(0, i)];
//                 }
//             } else {
//                 dest.copy_from_slice(src);
//             }
//         }
//     }
//
//     fn resolve_model_path() -> Option<std::path::PathBuf> {
//         if let Ok(p) = std::env::var("DIMMY_DFN_MODEL_PATH") {
//             let path = std::path::PathBuf::from(p);
//             if path.is_file() { return Some(path); }
//         }
//         let bundle = dirs::data_dir()?.join("dimmy").join("models").join("dfn.tar.gz");
//         if bundle.is_file() { Some(bundle) } else { None }
//     }
// }
