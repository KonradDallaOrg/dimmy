//! DeepFilterNet3 backend for the noise-suppression compare tool.
//!
//! Wraps the upstream `deep_filter` crate (DFN3 model + tract ONNX
//! runtime) behind a small, hop-size-streaming API matching the
//! shape `denoise_offline` expects.
//!
//! Gated on the `local-dfn` cargo feature so non-DFN3 builds (Mac
//! local-dev, headless CI) don't pull the ~50 MB tract dependency
//! tree. The `default-model` feature of `deep_filter` embeds the
//! DFN3 model bytes (~7 MB) inside the binary — zero installer
//! changes, no first-run download.

#![cfg(feature = "local-dfn")]

use anyhow::Result;
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{Array2, ArrayView2, ArrayViewMut2, Axis};

/// Stateful DFN3 wrapper. One instance handles a single audio
/// stream; reuse the same instance across `process_buffer` calls to
/// preserve STFT history.
pub struct Dfn3Processor {
    model: DfTract,
}

impl Dfn3Processor {
    /// Construct a DFN3 processor with the default embedded model
    /// and DFN3-recommended runtime params (full noise reduction,
    /// no post-filter, default per-decoder SNR thresholds).
    pub fn new() -> Result<Self> {
        let r_params = RuntimeParams::default()
            .with_atten_lim(100.0)
            .with_thresholds(-15.0, 35.0, 35.0);
        let df_params = DfParams::default();
        let model = DfTract::new(df_params, &r_params)?;
        Ok(Self { model })
    }

    /// Sample rate the model expects (always 48 000 Hz for DFN3).
    pub fn sample_rate(&self) -> u32 {
        self.model.sr as u32
    }

    /// STFT hop size — caller must feed buffers in multiples of
    /// this for full-frame processing. The model returns one
    /// `hop_size` block of enhanced samples per `hop_size` block
    /// of input. Typically 480 for DFN3 @ 48 kHz (10 ms).
    pub fn hop_size(&self) -> usize {
        self.model.hop_size
    }

    /// Process a mono buffer end-to-end. Input length must be a
    /// multiple of `hop_size`; the function tiles the buffer into
    /// per-hop chunks and runs the DFN3 inference inline. Returns
    /// a fresh Vec with the enhanced samples in the same range
    /// `[-1.0, 1.0]` as the input.
    pub fn process_mono(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        let hop = self.hop_size();
        let n_hops = input.len() / hop;
        let mut input_2d: Array2<f32> = Array2::zeros((1, n_hops * hop));
        for (i, &s) in input.iter().take(n_hops * hop).enumerate() {
            input_2d[(0, i)] = s.clamp(-1.0, 1.0);
        }
        let mut enh: Array2<f32> = Array2::zeros((1, n_hops * hop));
        for (ns_f, enh_f) in input_2d
            .view()
            .axis_chunks_iter(Axis(1), hop)
            .zip(enh.view_mut().axis_chunks_iter_mut(Axis(1), hop))
        {
            self.process_chunk_view(ns_f, enh_f)?;
        }
        let row = enh.row(0).to_vec();
        Ok(row)
    }

    fn process_chunk_view(&mut self, ns: ArrayView2<f32>, enh: ArrayViewMut2<f32>) -> Result<()> {
        self.model.process(ns, enh)?;
        Ok(())
    }

    /// Process exactly one `hop_size`-sample frame in `[-1.0, 1.0]`
    /// float. Mirror of `dfn::DfnProcessor::process_frame` so the
    /// AEC worker can swap the two implementations behind a
    /// compile-time `cfg` switch.
    pub fn process_frame(&mut self, src: &[f32], dest: &mut [f32]) -> Result<()> {
        let hop = self.hop_size();
        assert_eq!(src.len(), hop, "DFN3 frame must be {} samples", hop);
        assert_eq!(dest.len(), hop, "DFN3 frame must be {} samples", hop);
        let src_arr = Array2::from_shape_vec((1, hop), src.to_vec())
            .map_err(|e| anyhow::anyhow!("DFN3 input reshape: {}", e))?;
        let mut dest_arr: Array2<f32> = Array2::zeros((1, hop));
        self.model.process(src_arr.view(), dest_arr.view_mut())?;
        // Clamp + NaN-guard on copy-out (assert!() upstream in AEC).
        for (i, &v) in dest_arr.row(0).iter().enumerate() {
            dest[i] = if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        Ok(())
    }

    /// Honour the runtime `DENOISE_ENABLED` toggle (same gate as
    /// `DfnProcessor::try_init`). Returns `None` when denoise is
    /// disabled OR the DFN3 model fails to initialise — caller
    /// falls through to the AEC-only path.
    pub fn try_init_runtime() -> Option<Self> {
        if !crate::dfn::current_denoise_enabled() {
            crate::log("[Denoise] DFN3 disabled by runtime toggle");
            return None;
        }
        match Self::new() {
            Ok(p) => {
                crate::log(&format!(
                    "[Denoise] DeepFilterNet3 active on mic capture ({}-sample hops @ {} Hz)",
                    p.hop_size(),
                    p.sample_rate()
                ));
                Some(p)
            }
            Err(e) => {
                crate::log(&format!(
                    "[Denoise] DFN3 init failed: {} — AEC mic path runs DFN-bypass",
                    e
                ));
                None
            }
        }
    }
}
