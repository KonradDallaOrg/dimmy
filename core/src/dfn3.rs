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
}
