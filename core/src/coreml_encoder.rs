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
//! - **The first load of a bundle is very slow.** Apple's ANE service compiles
//!   the model to a device-specific format, minutes for a large model, once
//!   per machine. Left to happen on first use it landed inside a Teams call on
//!   2026-09-14: 347 s with the transcriber holding whisper's lock, nothing
//!   transcribed and the whole Mac dragging. So a bundle is now PREPARED in the
//!   background right after download (or at the next launch, or after the
//!   meeting in progress), and until then whisper is opened through a hard
//!   link that hides the bundle and keeps the ordinary GPU encoder.

use crate::error::TranscribeError;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Beside the models: hard links to whisper models whose encoder is not
/// prepared yet. whisper.cpp looks for the bundle next to the file it opened,
/// so opening the link finds none and runs on the GPU.
const GPU_UNTIL_PREPARED_DIR: &str = "gpu-until-prepared";

static PREPARING: AtomicBool = AtomicBool::new(false);
/// A model whose preparation is waiting for the meeting in progress to end.
static DEFERRED: Mutex<Option<String>> = Mutex::new(None);

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
    encoder_beside(&crate::local_stt::model_path(model_filename))
}

/// The bundle whisper.cpp will look for beside `model`, wherever it lives.
fn encoder_beside(model: &Path) -> PathBuf {
    let stem = model
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let stem = strip_quant_suffix(stem);
    model.with_file_name(format!("{stem}-encoder.mlmodelc"))
}

/// The bundle's own file name (`ggml-large-v3-encoder.mlmodelc`), which is
/// what `coreml_prepare` events carry: every quantisation of an architecture
/// shares one.
pub fn bundle_name(model_filename: &str) -> String {
    bundle_path(model_filename)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Written beside the bundle (never inside it) once macOS has compiled it.
fn prepared_marker(model_filename: &str) -> PathBuf {
    bundle_path(model_filename).with_extension("prepared")
}

fn gpu_alias(model: &Path) -> Option<PathBuf> {
    Some(
        model
            .parent()?
            .join(GPU_UNTIL_PREPARED_DIR)
            .join(model.file_name()?),
    )
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

/// The bundle is on disk AND macOS has already compiled it for this machine.
pub fn bundle_prepared(model_filename: &str) -> bool {
    bundle_present(model_filename) && prepared_marker(model_filename).exists()
}

pub fn is_preparing() -> bool {
    PREPARING.load(Ordering::SeqCst)
}

/// The file whisper should open for `model`.
///
/// While the bundle is present but not prepared: a hard link to the same
/// model under `gpu-until-prepared/`, so the load keeps the GPU encoder and
/// never starts the multi-minute compile inside a meeting or a dictation.
/// Same inode, so no extra disk.
pub fn load_path(model: &Path) -> PathBuf {
    let Some(name) = model.file_name().and_then(|n| n.to_str()) else {
        return model.to_path_buf();
    };
    if !cfg!(feature = "local-stt-coreml") || !bundle_present(name) || bundle_prepared(name) {
        return model.to_path_buf();
    }
    let Some(alias) = gpu_alias(model) else {
        return model.to_path_buf();
    };
    let linked_already = match (std::fs::metadata(&alias), std::fs::metadata(model)) {
        (Ok(a), Ok(m)) => a.len() == m.len(),
        _ => false,
    };
    if !linked_already {
        let _ = std::fs::remove_file(&alias);
        let dir_ok = alias
            .parent()
            .map(|d| std::fs::create_dir_all(d).is_ok())
            .unwrap_or(false);
        if !dir_ok || std::fs::hard_link(model, &alias).is_err() {
            crate::log("[CoreML] could not link the model aside; this load compiles the encoder");
            return model.to_path_buf();
        }
    }
    alias
}

/// Compile the bundle for `model_filename` now, on a low-priority thread, so
/// the first meeting that needs it does not pay minutes of stall. No-op when
/// there is nothing to prepare or a preparation is already running; waits for
/// the end of a meeting in progress. Reports through `coreml_prepare`.
pub fn prepare_in_background(model_filename: &str) {
    if !cfg!(feature = "local-stt-coreml")
        || model_filename.is_empty()
        || !bundle_present(model_filename)
        || bundle_prepared(model_filename)
    {
        return;
    }
    if crate::ffi::meeting_is_active() {
        if let Ok(mut deferred) = DEFERRED.lock() {
            *deferred = Some(model_filename.to_string());
        }
        crate::log("[CoreML] encoder not prepared yet; preparing after the meeting ends");
        emit_prepare_state(model_filename, "deferred");
        return;
    }
    if PREPARING.swap(true, Ordering::SeqCst) {
        return;
    }
    let name = model_filename.to_string();
    let spawned = std::thread::Builder::new()
        .name("dimmy-coreml-prepare".to_string())
        .spawn(move || {
            lower_thread_priority();
            emit_prepare_state(&name, "preparing");
            crate::log(&format!(
                "[CoreML] preparing the Neural Engine encoder for {name} (once; takes minutes)"
            ));
            let started = std::time::Instant::now();
            let result =
                crate::local_stt::prepare_coreml_encoder(&crate::local_stt::model_path(&name))
                    .and_then(|()| {
                        std::fs::write(prepared_marker(&name), b"").map_err(|e| {
                            TranscribeError::LocalModel(format!("prepared marker: {e}"))
                        })
                    });
            match result {
                Ok(()) => {
                    crate::log(&format!(
                        "[CoreML] encoder prepared in {:.0}s",
                        started.elapsed().as_secs_f32()
                    ));
                    emit_prepare_state(&name, "ready");
                }
                Err(e) => {
                    crate::log(&format!("[CoreML] preparation failed: {e}"));
                    emit_prepare_state(&name, "failed");
                }
            }
            PREPARING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        PREPARING.store(false, Ordering::SeqCst);
    }
}

/// Start the preparation a meeting made wait. Called when a meeting stops.
pub fn run_deferred() {
    let pending = DEFERRED.lock().ok().and_then(|mut d| d.take());
    if let Some(name) = pending {
        prepare_in_background(&name);
    }
}

/// One encoder serves every quantisation of an architecture: `large-v3` q5
/// and q8 share `ggml-large-v3-encoder.mlmodelc`. The event therefore carries
/// the BUNDLE as well as the model that triggered the work, so a host showing
/// "ready" for the model it has selected does not miss a preparation that ran
/// under a sibling quant's name — which is exactly what left the Settings row
/// spinning on a finished encoder (2026-09-22).
fn emit_prepare_state(model_filename: &str, state: &str) {
    let bundle = bundle_path(model_filename)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    crate::ffi::emit_event(
        "coreml_prepare",
        &serde_json::json!({
            "filename": model_filename,
            "bundle": bundle,
            "state": state,
        })
        .to_string(),
    );
}

/// The compile runs mostly inside Apple's ANE service, which inherits the
/// requester's QoS; UTILITY keeps it from competing with a call app.
#[cfg(target_os = "macos")]
fn lower_thread_priority() {
    extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }
    // QOS_CLASS_UTILITY in <sys/qos.h>.
    const QOS_CLASS_UTILITY: u32 = 0x11;
    // SAFETY: changes only the calling thread's QoS; no pointers involved.
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn lower_thread_priority() {}

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
    // A new bundle has to be compiled again, whatever the old one was.
    let _ = std::fs::remove_file(prepared_marker(model_filename));
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

    #[test]
    fn quantisations_of_one_architecture_share_a_bundle() {
        let q8 = bundle_path("ggml-large-v3-q8_0.bin");
        let q5 = bundle_path("ggml-large-v3-q5_0.bin");
        assert_eq!(q8, q5, "a preparation under one quant covers the other");
        assert!(q8.ends_with("ggml-large-v3-encoder.mlmodelc"), "{q8:?}");
        // The turbo is a DIFFERENT architecture and keeps its own bundle.
        assert_ne!(q8, bundle_path("ggml-large-v3-turbo-q8_0.bin"));
    }

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
    fn the_gpu_alias_hides_the_encoder_from_whisper_cpp() {
        let model = crate::local_stt::model_path("ggml-large-v3-turbo-q8_0.bin");
        let alias = gpu_alias(&model).unwrap();
        assert_eq!(alias.file_name(), model.file_name());
        assert_ne!(encoder_beside(&alias), encoder_beside(&model));
        assert_eq!(encoder_beside(&alias).parent(), alias.parent());
    }

    #[test]
    fn the_prepared_marker_sits_beside_the_bundle_not_inside_it() {
        let marker = prepared_marker("ggml-large-v3-turbo-q8_0.bin");
        assert_eq!(
            marker.parent(),
            bundle_path("ggml-large-v3-turbo-q8_0.bin").parent()
        );
        assert_eq!(
            marker.file_name().unwrap(),
            "ggml-large-v3-turbo-encoder.prepared"
        );
    }

    #[test]
    fn without_a_bundle_whisper_opens_the_model_itself() {
        let model = crate::local_stt::model_path("ggml-nothing-here-q8_0.bin");
        assert_eq!(load_path(&model), model);
        assert!(!bundle_prepared("ggml-nothing-here-q8_0.bin"));
    }

    #[test]
    fn a_bare_directory_is_not_a_ready_bundle() {
        // Guards the half-extracted case: `is_dir` alone would say yes.
        assert!(!bundle_present("ggml-nothing-here-q8_0.bin"));
    }
}
