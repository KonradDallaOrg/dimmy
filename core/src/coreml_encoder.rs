//! whisper's Core ML encoder bundle: where it lives, and how it gets there.
//!
//! With `local-stt-coreml` compiled in, whisper.cpp looks next to the `.bin`
//! for a directory named after it with `.bin` swapped for `-encoder.mlmodelc`.
//! Finding it moves the encoder off the GPU and onto the Apple Neural Engine;
//! not finding it costs one log line and the ordinary encoder runs (the sys
//! crate builds with `WHISPER_COREML_ALLOW_FALLBACK=1`). So nothing in this
//! module is on a path that must work.
//!
//! Two things about it are not obvious:
//!
//! - **The bundle is chosen by ARCHITECTURE, not by quantisation.** Core ML
//!   replaces the encoder wholesale with its own fp16 weights, so the `q5_0`
//!   and `q8_0` builds of large-v3-turbo take the same archive -- which is
//!   just as well, because upstream publishes one per architecture. It is
//!   then saved under the QUANTISED model's name, since that is the only name
//!   whisper.cpp will look for.
//! - **The first transcription after installing one is very slow.** Apple's
//!   ANE service compiles the model to a device-specific format on first load,
//!   minutes for a large model, once per machine. A caller that reports
//!   progress should say so, or it reads as a hang.

use crate::error::TranscribeError;
use std::path::{Path, PathBuf};

/// Where upstream publishes the bundles. Same repo as the `.bin` models.
const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Architecture stem for a model filename, or None when upstream publishes no
/// Core ML encoder for it.
///
/// Matched longest-first: `large-v3-turbo` has to win over `large-v3`, which
/// is a substring of it.
fn architecture(model_filename: &str) -> Option<&'static str> {
    const ARCHITECTURES: &[&str] = &[
        "large-v3-turbo",
        "large-v3",
        "large-v2",
        "medium",
        "small",
        "base",
        "tiny",
    ];
    // Distil has to be rejected BEFORE the substring scan, not merely left
    // out of the list: "ggml-distil-large-v3.5" contains "large-v3", so the
    // scan happily pairs it with the plain large-v3 encoder. Upstream
    // publishes no distil encoder, and a wrong one is worse than none -- the
    // architectures differ (distil has 2 decoder layers), so it would
    // download 1.1 GB and then produce nothing.
    if model_filename.contains("distil") {
        return None;
    }
    ARCHITECTURES
        .iter()
        .find(|a| model_filename.contains(*a))
        .copied()
}

/// The `.mlmodelc` directory whisper.cpp will look for, given a model file.
///
/// Mirrors `whisper_get_coreml_path_encoder`: drop the extension, strip a
/// trailing `-qX_X` quantisation suffix, append `-encoder.mlmodelc`.
pub fn bundle_path(model_filename: &str) -> PathBuf {
    let model = crate::local_stt::model_path(model_filename);
    let stem = model
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(model_filename);
    let stem = strip_quant_suffix(stem);
    model.with_file_name(format!("{stem}-encoder.mlmodelc"))
}

/// Strip a trailing `-qX_X` quantisation suffix (`-` + `q` + digit + `_` +
/// digit, 5 bytes), mirroring whisper.cpp's own `whisper_get_coreml_path_encoder`.
/// whisper.cpp computes the Core ML path itself from `ctx->path_model` with
/// this exact stripping rule, so a bundle saved under the quantised name
/// (e.g. `ggml-large-v3-turbo-q5_0-encoder.mlmodelc`) is invisible to it --
/// verified live 2026-09-11: whisper.cpp requested
/// `ggml-large-v3-turbo-encoder.mlmodelc`, not the quantised name, and
/// failed to load until the bundle was renamed to match.
fn strip_quant_suffix(stem: &str) -> &str {
    let Some(pos) = stem.rfind('-') else {
        return stem;
    };
    let sub = &stem.as_bytes()[pos..];
    if sub.len() == 5 && sub[1] == b'q' && sub[3] == b'_' {
        &stem[..pos]
    } else {
        stem
    }
}

/// True when the bundle is on disk AND looks like a compiled model rather than
/// an interrupted unpack. `coremldata.bin` is the file every `.mlmodelc`
/// carries; checking only for the directory would call a half-extracted bundle
/// ready, and whisper would then fail to load it on every single window.
pub fn bundle_present(model_filename: &str) -> bool {
    let dir = bundle_path(model_filename);
    dir.is_dir() && dir.join("coremldata.bin").exists()
}

/// Whether a Core ML encoder exists upstream for this model at all. The UI
/// uses it to decide between offering the download and saying nothing.
pub fn bundle_available(model_filename: &str) -> bool {
    architecture(model_filename).is_some()
}

/// Download and unpack the Core ML encoder for `model_filename`.
///
/// Progress is forwarded from the download; the unpack that follows is not
/// reported, because `ditto` gives us nothing to report.
pub async fn download(
    model_filename: &str,
    on_progress: impl Fn(u64, u64),
) -> Result<(), TranscribeError> {
    assert!(
        !model_filename.is_empty(),
        "model filename must not be empty"
    );
    if bundle_present(model_filename) {
        return Ok(());
    }
    let arch = architecture(model_filename).ok_or_else(|| {
        TranscribeError::LocalModel(format!(
            "no Core ML encoder is published for {model_filename}"
        ))
    })?;

    let dest_dir = bundle_path(model_filename);
    let zip_path = dest_dir.with_extension("mlmodelc.zip");
    let url = format!("{BASE_URL}/ggml-{arch}-encoder.mlmodelc.zip");
    crate::log(&format!("[CoreML] downloading {url}"));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| TranscribeError::LocalModel(format!("HTTP client error: {e}")))?;
    // "PK\x03\x04" is the zip local-file header: the same integrity idea the
    // .bin downloads apply with the ggml magic, and it catches an HTML error
    // page saved under a .zip name.
    crate::download::download_resumable(&client, &url, &zip_path, &[b"PK\x03\x04"], on_progress)
        .await
        .map_err(TranscribeError::LocalModel)?;

    let unpacked = unpack(&zip_path, &dest_dir);
    // The archive is worth nothing once unpacked, and leaving several hundred
    // MB behind on a machine we just asked to free space for a model is rude.
    let _ = std::fs::remove_file(&zip_path);
    unpacked?;

    if !bundle_present(model_filename) {
        return Err(TranscribeError::LocalModel(format!(
            "unpacked archive has no compiled model at {}",
            dest_dir.display()
        )));
    }
    crate::log(&format!("[CoreML] ready at {}", dest_dir.display()));
    Ok(())
}

/// Unpack with `ditto`, which ships with macOS and is what Apple uses for its
/// own archives. Not `unzip`: these bundles carry extended attributes, and
/// ditto is the tool that preserves them.
#[cfg(target_os = "macos")]
fn unpack(zip: &Path, dest_dir: &Path) -> Result<(), TranscribeError> {
    // Extract beside the destination and move into place, so an interrupted
    // run never leaves a directory `bundle_present` would accept.
    let staging = dest_dir.with_extension("mlmodelc.unpacking");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| TranscribeError::LocalModel(format!("create {}: {e}", staging.display())))?;

    let out = std::process::Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(zip)
        .arg(&staging)
        .output()
        .map_err(|e| TranscribeError::LocalModel(format!("ditto: {e}")))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        // ditto's stderr can carry a full path; keep the status only.
        return Err(TranscribeError::LocalModel(format!(
            "ditto failed with {}",
            out.status
        )));
    }

    // The archive holds one top-level `ggml-<arch>-encoder.mlmodelc`, and we
    // need it under the quantised model's name, so move the INNER directory
    // rather than the staging dir itself.
    let inner = std::fs::read_dir(&staging)
        .map_err(|e| TranscribeError::LocalModel(format!("read {}: {e}", staging.display())))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join("coremldata.bin").exists())
        .ok_or_else(|| {
            TranscribeError::LocalModel("archive contains no .mlmodelc directory".to_string())
        })?;

    let _ = std::fs::remove_dir_all(dest_dir);
    std::fs::rename(&inner, dest_dir)
        .map_err(|e| TranscribeError::LocalModel(format!("rename into place: {e}")))?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn unpack(_zip: &Path, _dest_dir: &Path) -> Result<(), TranscribeError> {
    Err(TranscribeError::LocalModel(
        "the Core ML encoder is macOS-only".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turbo_wins_over_the_name_it_contains() {
        // "large-v3" is a substring of "large-v3-turbo"; a declaration-order
        // or shortest-first match pairs turbo with the wrong encoder, which
        // downloads 1.1 GB and then transcribes nothing.
        assert_eq!(
            architecture("ggml-large-v3-turbo-q8_0.bin"),
            Some("large-v3-turbo")
        );
        assert_eq!(architecture("ggml-large-v3-q5_0.bin"), Some("large-v3"));
    }

    #[test]
    fn quantisation_does_not_change_the_bundle() {
        // Core ML brings its own fp16 encoder weights, so both builds of an
        // architecture take the same archive.
        assert_eq!(
            architecture("ggml-large-v3-turbo-q5_0.bin"),
            architecture("ggml-large-v3-turbo-q8_0.bin")
        );
    }

    #[test]
    fn models_without_a_published_encoder_say_so() {
        assert_eq!(architecture("ggml-distil-large-v3.5-q8_0.bin"), None);
        assert!(!bundle_available("ggml-distil-large-v3.5-q8_0.bin"));
    }

    #[test]
    fn bundle_is_named_the_way_whisper_cpp_looks_for_it() {
        // whisper.cpp's own whisper_get_coreml_path_encoder() drops the
        // extension AND strips a trailing "-qX_X" quantisation suffix
        // before appending "-encoder.mlmodelc" (see whisper.cpp source,
        // `whisper_get_coreml_path_encoder`). A bundle saved under the
        // quantised name is therefore invisible to it -- verified live on
        // 2026-09-11: whisper.cpp requested
        // '.../ggml-large-v3-turbo-encoder.mlmodelc', not
        // '.../ggml-large-v3-turbo-q8_0-encoder.mlmodelc'.
        let p = bundle_path("ggml-large-v3-turbo-q8_0.bin");
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            "ggml-large-v3-turbo-encoder.mlmodelc"
        );
        // And beside the model, because that is where whisper.cpp looks.
        assert_eq!(p.parent(), crate::local_stt::model_path("x.bin").parent());
    }

    #[test]
    fn quant_suffix_stripped_for_every_shipped_quantisation() {
        // whisper.cpp's check is structural (sub.size()==5, sub[1]=='q',
        // sub[3]=='_'), not a specific digit pair -- q5_0, q8_0 and q5_1
        // (every quantisation Dimmy actually ships) must all strip.
        for name in [
            "ggml-base-q8_0.bin",
            "ggml-small-q5_1.bin",
            "ggml-medium-q5_0.bin",
        ] {
            let p = bundle_path(name);
            let got = p.file_name().unwrap().to_str().unwrap();
            assert!(
                !got.contains("-q5_") && !got.contains("-q8_"),
                "{name} -> {got} still carries a quant suffix whisper.cpp will not look for"
            );
        }
    }

    #[test]
    fn a_bare_directory_is_not_a_ready_bundle() {
        // Guards the half-extracted case: `is_dir` alone would say yes.
        assert!(!bundle_present("ggml-nothing-here-q8_0.bin"));
    }
}
