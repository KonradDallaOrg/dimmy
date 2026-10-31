//! Nemotron 3.5 ASR Streaming 0.6B -- the fifth local STT backend.
//!
//! NVIDIA's cache-aware streaming FastConformer-RNNT: 40 languages from one
//! checkpoint, language auto-detected, text that grows while the person is
//! still speaking. Measured 2026-10-29 on this project's own Italian
//! dictations (NVIDIA T600, i7-12700H): GGUF Q8_0 transcribes at 6x realtime
//! on CUDA and 3.6x on the CPU, with text indistinguishable from the FP32
//! model; F16 gives the same text three times slower and is not offered.
//!
//! It runs in-process through NeMo-Speech.cpp's stable C ABI (`asr.h`,
//! "v1": opaque handles, POD structs that start with `size` and only ever
//! grow). The library is built by this project from a pinned NVIDIA commit
//! with ggml linked statically (`BUILD_SHARED_LIBS=OFF`): NVIDIA's own
//! release ships `ggml.dll` and `llama.dll`, the same names as the ones
//! `local-stt-qwen` and `local-llm` already load into this process, and two
//! DLLs with one name in one process is a crash waiting for a user.
//!
//! Like Whistle's engine, the library is code fetched at run time, so it is
//! pinned to a SHA-256 compiled in here and refused if it changed.

use crate::error::TranscribeError;
use std::ffi::{c_char, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const SAMPLE_RATE: usize = 16_000;

// ---- What gets downloaded ------------------------------------------------

/// One pinned download: a revision-pinned URL, its SHA-256 and size.
struct Pinned {
    file: &'static str,
    url: &'static str,
    sha256: &'static str,
    bytes: u64,
    magic: &'static [u8],
}

const F32_URL: &str = "https://huggingface.co/KonradDallaOrg/dimmy-nemotron-3.5-asr-gguf/resolve/76599da3898c6152ab783eddc4f2625b94aa01aa/nemotron-3.5-asr-streaming-0.6b.f32.gguf";
const F32_SHA256: &str = "2223d6e5d3f9e89ff54928f91b524bd827b35cb28b8f5951bfef4c1fb30b44cd";
const F32_BYTES: u64 = 2_576_356_832;
const GGUF_MAGIC: &[u8] = b"GGUF";
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

/// NVIDIA's own Q8_0 conversion, revision `ea30d66`.
const MODEL: Pinned = Pinned {
    file: "nemotron-3.5-asr-streaming-0.6b.q8_0.gguf",
    url: "https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/resolve/ea30d66debe3740a08b573244286791d423d6b3e/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf",
    sha256: "3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1",
    bytes: 742_090_464,
    magic: GGUF_MAGIC,
};

/// The same checkpoint at full precision, for whoever has the GPU for it:
/// NVIDIA's `convert_model.py` at `--outtype f32` (its table lists F32 for
/// other model families only; the weights are the checkpoint's own). NVIDIA
/// publishes no such file, so it is mirrored on the project's organization.
const MODEL_F32: Pinned = Pinned {
    file: "nemotron-3.5-asr-streaming-0.6b.f32.gguf",
    url: F32_URL,
    sha256: F32_SHA256,
    bytes: F32_BYTES,
    magic: GGUF_MAGIC,
};

/// Nemotron-3-Diarization (Sortformer, eight speakers), NVIDIA's Q8_0,
/// revision `f667ed7`. The same model `diarize.rs` runs as ONNX after a
/// meeting; here the engine runs it inside the stream, word by word.
const DIARIZER: Pinned = Pinned {
    file: "Nemotron-3-Diarization.q8_0.gguf",
    url: "https://huggingface.co/nvidia/Nemotron-3-Diarization/resolve/f667ed73aee57d40cc39428eb768b4fd87a0a29e/Nemotron-3-Diarization.q8_0.gguf",
    sha256: "08456d9e22cd9a323c0364d98375f3746d6e68507ebb705cd46438c534c7a3a1",
    bytes: 107_012_128,
    magic: GGUF_MAGIC,
};

/// Which build of the engine: the CPU one runs everywhere; a GPU one is used
/// when the machine has the hardware and falls back to the CPU one when it
/// will not load (an old NVIDIA driver, say).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Cpu,
    Cuda,
    Metal,
}

impl Flavor {
    fn dir(self) -> &'static str {
        match self {
            Flavor::Cpu => "cpu",
            Flavor::Cuda => "cuda",
            Flavor::Metal => "metal",
        }
    }

    fn is_gpu(self) -> bool {
        self != Flavor::Cpu
    }
}

const ENGINE_RELEASE: &str =
    "https://github.com/KonradDallaOrg/dimmy/releases/download/engine-nemotron-66d50d9";

struct EngineArchive {
    flavor: Flavor,
    archive: &'static str,
    sha256: &'static str,
    bytes: u64,
}

// The archives `engine-nemotron.yml` publishes. An empty `sha256` is a build
// that has not been published and pinned yet: offered as nothing, never
// downloaded unverified.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const ENGINES: &[EngineArchive] = &[
    EngineArchive {
        flavor: Flavor::Cpu,
        archive: "nemotron-engine-66d50d9-windows-x86_64-cpu.zip",
        sha256: "46e0543088161bd34eef8d2a2dd3c819485fa56ab4cc606dc8a517937554acc2",
        bytes: 1618448,
    },
    EngineArchive {
        flavor: Flavor::Cuda,
        archive: "nemotron-engine-66d50d9-windows-x86_64-cuda.zip",
        sha256: "487118b7a93db4d24e89da369918a03e1de59c1dd20783d2fbc3c0aa0b90259a",
        bytes: 97587904,
    },
];
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const ENGINES: &[EngineArchive] = &[
    EngineArchive {
        flavor: Flavor::Cpu,
        archive: "nemotron-engine-66d50d9-linux-x86_64-cpu.zip",
        sha256: "be68bb7a9673d1f9e756d8da68a45323698b9a823eac5e42fd5ccdd82fcfdd8c",
        bytes: 1836474,
    },
    EngineArchive {
        flavor: Flavor::Cuda,
        archive: "nemotron-engine-66d50d9-linux-x86_64-cuda.zip",
        sha256: "42e092c679e7e74d2078ccf96ecf9206437b63dbfe096b9c0a6af289956c852b",
        bytes: 98731104,
    },
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENGINES: &[EngineArchive] = &[EngineArchive {
    flavor: Flavor::Metal,
    archive: "nemotron-engine-66d50d9-macos-aarch64-metal.zip",
    sha256: "b4b048ac72beac4c5684310fe62c211bd64102060e75e45c66619c6e6f01ea55",
    bytes: 1847040,
}];
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
)))]
const ENGINES: &[EngineArchive] = &[];

#[cfg(windows)]
const LIBRARY_FILE: &str = "nemo_speech_asr_c.dll";
#[cfg(target_os = "macos")]
const LIBRARY_FILE: &str = "libnemo_speech_asr_c.dylib";
#[cfg(all(unix, not(target_os = "macos")))]
const LIBRARY_FILE: &str = "libnemo_speech_asr_c.so";

fn engine_root() -> PathBuf {
    crate::local_stt::model_path("nemotron-engine")
}

fn library_path(flavor: Flavor) -> PathBuf {
    engine_root().join(flavor.dir()).join(LIBRARY_FILE)
}

/// Free memory a model load must leave: the engine aborts the whole process
/// when an allocation fails, and in a meeting that process is the recording.
const LOAD_HEADROOM_BYTES: u64 = 256 * 1024 * 1024;
/// Commit charge of the GPU runtime on top of the weights: 7.56 GB peak with
/// 0.85 GB of Q8_0 and diarizer (CUDA, T600, 2026-10-30).
const GPU_RUNTIME_BYTES: u64 = 7 * 1024 * 1024 * 1024;
/// Commit charge of a CPU recognizer on top of the weights: 3.1 GB in all at
/// Q8_0 without the diarizer, 4.4 GB with it (i7-12700H, 2026-10-30). Counting
/// the weights alone let a load through with a third of what it takes.
const CPU_RUNTIME_BYTES: u64 = 3686 * 1024 * 1024;
/// What F32 must find free before it is preferred over Q8_0.
const F32_HEADROOM_BYTES: u64 = GPU_RUNTIME_BYTES + 1024 * 1024 * 1024;

/// Values of the `nemotron_model` config field.
pub const MODEL_CHOICE_Q8: &str = "q8";
pub const MODEL_CHOICE_F32: &str = "f32";

/// The choice is made in Settings. F32 needs a GPU, since a CPU could not keep
/// up (0.6-1.3x realtime for one stream), and the memory to load it, or Q8_0
/// does the job. A model already on the GPU stays until the next launch: it
/// could not be freed for the other one, and two do not fit (see `recognizer`).
fn pick_model(
    wanted: bool,
    loaded_on_gpu: Option<bool>,
    available: Option<u64>,
) -> &'static Pinned {
    let fits = available.is_none_or(|free| free >= MODEL_F32.bytes + F32_HEADROOM_BYTES);
    let f32 = match loaded_on_gpu {
        Some(loaded_is_f32) => loaded_is_f32,
        None => wanted && fits,
    };
    if f32 {
        &MODEL_F32
    } else {
        &MODEL
    }
}

fn f32_wanted() -> bool {
    full_precision_available()
        && full_precision_present()
        && std::env::var_os("DIMMY_NEMOTRON_Q8").is_none()
        && (crate::ffi::nemotron_f32_selected() || std::env::var_os("DIMMY_NEMOTRON_F32").is_some())
}

/// Whether the model on the GPU is F32, when one is loaded.
fn loaded_on_gpu() -> Option<bool> {
    let f32_path = full_precision_path();
    RECOGNIZERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .find(|r| r.spec().gpu.is_some())
        .map(|r| r.spec().model == f32_path)
}

fn running_model() -> &'static Pinned {
    pick_model(
        f32_wanted(),
        loaded_on_gpu(),
        crate::setup::available_memory_bytes(),
    )
}

/// What the pass after a meeting adds to a loaded model: 3.9 GB at Q8_0 and
/// 5.8 GB at F32 on a GPU, 2.2 GB on a CPU (2026-10-30). It loads nothing, so
/// the check in `recognizer` does not see it, and an allocation that fails aborts.
const PASS_GPU_BYTES: u64 = 6 * 1024 * 1024 * 1024;
const PASS_CPU_BYTES: u64 = 2500 * 1024 * 1024;

pub fn pass_fits() -> bool {
    let needed = if loaded_on_gpu().is_some() {
        PASS_GPU_BYTES
    } else {
        PASS_CPU_BYTES
    };
    enough_memory(crate::setup::available_memory_bytes(), needed)
}

/// True when Settings asks for a model other than the one loaded on the GPU.
pub fn restart_needed() -> bool {
    loaded_on_gpu().is_some_and(|f32| f32 != f32_wanted())
}

pub fn model_path() -> PathBuf {
    crate::local_stt::model_path(running_model().file)
}

fn full_precision_path() -> PathBuf {
    crate::local_stt::model_path(MODEL_F32.file)
}

/// True when this machine runs a GPU build, the only place F32 is used.
pub fn full_precision_available() -> bool {
    wanted_flavors().first().is_some_and(|f| f.is_gpu())
}

pub fn full_precision_present() -> bool {
    full_precision_path().is_file()
}

pub fn full_precision_size_mb() -> u32 {
    MODEL_F32.bytes.div_ceil(1024 * 1024) as u32
}

pub fn full_precision_files() -> Vec<PathBuf> {
    vec![full_precision_path()]
}

pub fn diarizer_path() -> PathBuf {
    crate::local_stt::model_path(DIARIZER.file)
}

/// The engine builds this machine should have, best first. A GPU build is
/// listed only when the hardware for it is there; the CPU build always is,
/// as the fallback.
fn wanted_flavors() -> Vec<Flavor> {
    let force_cpu = std::env::var_os("DIMMY_FORCE_CPU").is_some();
    let gpu = ENGINES
        .iter()
        .map(|e| e.flavor)
        .filter(|_| !force_cpu)
        .find(|f| match f {
            Flavor::Metal => true,
            Flavor::Cuda => has_nvidia_gpu(),
            Flavor::Cpu => false,
        });
    let mut v: Vec<Flavor> = gpu.into_iter().collect();
    if ENGINES.iter().any(|e| e.flavor == Flavor::Cpu) {
        v.push(Flavor::Cpu);
    }
    v
}

fn has_nvidia_gpu() -> bool {
    #[cfg(target_os = "linux")]
    {
        // hardware::detect reads AMD/Intel sysfs only; the NVIDIA driver
        // announces itself here whenever it is loaded.
        std::path::Path::new("/proc/driver/nvidia/version").exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        crate::hardware::detect()
            .name
            .is_some_and(|n| n.to_ascii_uppercase().contains("NVIDIA"))
    }
}

/// Whether this platform has any published engine build to fetch, or a
/// build already on disk (a developer's own).
pub fn engine_available() -> bool {
    ENGINES.iter().any(|e| !e.sha256.is_empty()) || engine_installed(Flavor::Cpu)
}

/// True when the model and at least one engine build are on disk.
pub fn bundle_present() -> bool {
    crate::local_stt::model_path(MODEL.file).is_file()
        && wanted_flavors().iter().any(|f| engine_installed(*f))
}

pub fn diarizer_present() -> bool {
    diarizer_path().is_file()
}

/// Megabytes a download would fetch: model, diarizer and the engine builds
/// this machine wants.
pub fn size_mb() -> u32 {
    let engines: u64 = wanted_flavors()
        .iter()
        .filter_map(|f| ENGINES.iter().find(|e| e.flavor == *f))
        .map(|e| e.bytes)
        .sum();
    (MODEL.bytes + DIARIZER.bytes + engines).div_ceil(1024 * 1024) as u32
}

/// Every file and folder a finished download leaves, for the download
/// center to delete. A loaded engine library stays locked until exit.
pub fn bundle_files() -> Vec<PathBuf> {
    vec![
        crate::local_stt::model_path(MODEL.file),
        full_precision_path(),
        diarizer_path(),
        engine_root(),
    ]
}

/// The spec the app runs with: the best engine build on disk, the model,
/// and the diarizer only when the caller wants speakers. A recognizer that
/// carries the diarizer runs it on EVERY stream, asked for speakers or not:
/// a dictation measured 2.6x realtime without it and 1.4-1.6x with it loaded
/// and unused (CPU, 2026-10-30). So speakers get a recognizer of their own.
pub fn default_spec(speakers: bool) -> Option<EngineSpec> {
    let flavor = wanted_flavors()
        .into_iter()
        .find(|f| engine_installed(*f))?;
    Some(spec_for(flavor, speakers))
}

fn spec_for(flavor: Flavor, speakers: bool) -> EngineSpec {
    EngineSpec {
        library: library_path(flavor),
        // F32 on a CPU falls behind on a single stream: the fallback from a
        // refused GPU load must not bring it along.
        model: if flavor.is_gpu() {
            model_path()
        } else {
            crate::local_stt::model_path(MODEL.file)
        },
        gpu: flavor.is_gpu().then_some(0),
        // A GPU has the headroom and not the memory: one model serves every
        // stream there (T600, two tracks: load 0.38 at Q8, 0.54 at F32), where
        // two F32 recognizers do not fit and the second load aborts.
        diarizer: ((speakers || flavor.is_gpu()) && diarizer_present()).then(diarizer_path),
    }
}

/// The recognizer for this machine: the GPU build when it loads, the CPU
/// build when the GPU one does not (driver too old, no device after all).
/// `speakers` picks the one that carries the diarizer.
pub fn app_recognizer(speakers: bool) -> Result<Arc<Recognizer>, TranscribeError> {
    let not_installed = || TranscribeError::LocalModel("Nemotron is not downloaded".into());
    let spec = default_spec(speakers).ok_or_else(not_installed)?;
    match recognizer(&spec) {
        Ok(r) => Ok(r),
        Err(e) if spec.gpu.is_some() && engine_installed(Flavor::Cpu) => {
            crate::log(&format!(
                "[Nemotron] GPU engine failed ({e}); using the CPU engine"
            ));
            // Once: this path runs for every track of every meeting. Without
            // it the only symptom of a refused GPU is a slow transcript.
            static TOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !TOLD.swap(true, std::sync::atomic::Ordering::Relaxed) {
                crate::ffi::emit_event("nemotron_cpu_fallback", "{}");
            }
            recognizer(&spec_for(Flavor::Cpu, speakers))
        }
        Err(e) => Err(e),
    }
}

/// Fetch the model, the diarizer and the engine builds this machine wants,
/// resumable, each checked against the SHA-256 pinned above. Progress
/// covers everything.
pub async fn download_bundle<F>(on_progress: F) -> Result<(), TranscribeError>
where
    F: Fn(u64, u64) + Sync,
{
    let err = |e: String| TranscribeError::LocalModel(format!("Nemotron download: {e}"));
    let engines: Vec<&EngineArchive> = wanted_flavors()
        .iter()
        .filter_map(|f| {
            ENGINES
                .iter()
                .find(|e| e.flavor == *f && !e.sha256.is_empty())
        })
        .collect();
    if engines.is_empty() && !engine_installed(Flavor::Cpu) {
        return Err(err(
            "no engine build is published for this platform yet".into()
        ));
    }
    let dir = crate::local_stt::model_directory();
    std::fs::create_dir_all(&dir).map_err(|e| err(format!("create {}: {e}", dir.display())))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| err(format!("HTTP client: {e}")))?;

    let archive_url: Vec<String> = engines
        .iter()
        .map(|e| format!("{ENGINE_RELEASE}/{}", e.archive))
        .collect();
    let mut jobs = vec![
        Job::new(
            MODEL.url,
            crate::local_stt::model_path(MODEL.file),
            &MODEL,
            !crate::local_stt::model_path(MODEL.file).is_file(),
        ),
        Job::new(
            DIARIZER.url,
            diarizer_path(),
            &DIARIZER,
            !diarizer_present(),
        ),
    ];
    for (e, url) in engines.iter().zip(&archive_url) {
        jobs.push(Job {
            url,
            dest: crate::local_stt::model_path(e.archive),
            magic: ZIP_MAGIC,
            sha256: e.sha256,
            bytes: e.bytes,
            wanted: !engine_installed(e.flavor),
        });
    }
    let total: u64 = jobs.iter().map(|j| j.bytes).sum();
    let mut base = 0u64;
    for Job {
        url,
        dest,
        magic,
        sha256,
        bytes,
        wanted,
    } in jobs
    {
        if wanted {
            crate::log(&format!("[Nemotron] Downloading {url} ..."));
            crate::download::download_resumable(&client, url, &dest, &[magic], |done, _| {
                on_progress(base + done, total)
            })
            .await
            .map_err(err)?;
            if let Err(e) = crate::download::verify_file(&dest, &[magic], Some(sha256)) {
                let _ = std::fs::remove_file(&dest);
                return Err(err(format!("failed its pinned check: {e}")));
            }
        }
        base += bytes;
        on_progress(base, total);
    }
    for e in &engines {
        let zip = crate::local_stt::model_path(e.archive);
        if !engine_installed(e.flavor) {
            extract_engine(&zip, e.flavor).map_err(err)?;
        }
        let _ = std::fs::remove_file(&zip);
    }
    assert!(
        bundle_present(),
        "model and an engine build must exist after a successful download"
    );
    Ok(())
}

/// Fetch the full-precision model. It comes on top of the bundle, which
/// stays the fallback when memory is short.
pub async fn download_full_precision<F>(on_progress: F) -> Result<(), TranscribeError>
where
    F: Fn(u64, u64) + Sync,
{
    let err = |e: String| TranscribeError::LocalModel(format!("Nemotron F32 download: {e}"));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| err(format!("HTTP client: {e}")))?;
    let dest = full_precision_path();
    crate::download::download_resumable(&client, MODEL_F32.url, &dest, &[GGUF_MAGIC], |done, _| {
        on_progress(done, MODEL_F32.bytes)
    })
    .await
    .map_err(err)?;
    if let Err(e) = crate::download::verify_file(&dest, &[GGUF_MAGIC], Some(MODEL_F32.sha256)) {
        let _ = std::fs::remove_file(&dest);
        return Err(err(format!("failed its pinned check: {e}")));
    }
    assert!(full_precision_present(), "verified file must be on disk");
    Ok(())
}

struct Job<'a> {
    url: &'a str,
    dest: PathBuf,
    magic: &'static [u8],
    sha256: &'static str,
    bytes: u64,
    wanted: bool,
}

impl<'a> Job<'a> {
    fn new(url: &'a str, dest: PathBuf, p: &Pinned, wanted: bool) -> Self {
        Self {
            url,
            dest,
            magic: p.magic,
            sha256: p.sha256,
            bytes: p.bytes,
            wanted,
        }
    }
}

/// Unpack an engine archive into `nemotron-engine/<flavor>/` through a
/// staging folder renamed into place, so a crash mid-way never leaves half
/// a library under the name the loader looks for. Only library files and
/// licence texts are taken, flattened: nothing in an archive can land
/// outside the engine folder.
fn extract_engine(zip_path: &Path, flavor: Flavor) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("open archive: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("read archive: {e}"))?;
    let dest = engine_root().join(flavor.dir());
    let staging = engine_root().join(format!("{}.part", flavor.dir()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
    let libraries = match unpack_engine(&mut zip, &staging) {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    if !staging.join(LIBRARY_FILE).is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "archive has no {LIBRARY_FILE} ({libraries} libraries)"
        ));
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| format!("install engine: {e}"))
}

fn unpack_engine(
    zip: &mut zip::ZipArchive<std::fs::File>,
    staging: &Path,
) -> Result<usize, String> {
    let mut libraries = 0;
    use std::io::Read;
    let mut links = Vec::new();
    for i in 0..zip.len() {
        let mut member = zip
            .by_index(i)
            .map_err(|e| format!("archive entry {i}: {e}"))?;
        let Some(name) = member
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_owned()))
        else {
            continue;
        };
        let n = name.to_string_lossy();
        let is_lib = n.ends_with(".dll") || n.ends_with(".dylib") || n.contains(".so");
        let is_notice = n == "LICENSE" || n == "NOTICE" || n == "THIRD_PARTY_NOTICES.md";
        if member.is_dir() || !(is_lib || is_notice) {
            continue;
        }
        if member.is_symlink() {
            let mut target = String::new();
            member
                .read_to_string(&mut target)
                .map_err(|e| format!("read link {n}: {e}"))?;
            links.push((name.clone(), target));
            continue;
        }
        let mut out =
            std::fs::File::create(staging.join(&name)).map_err(|e| format!("create {n}: {e}"))?;
        std::io::copy(&mut member, &mut out).map_err(|e| format!("extract {n}: {e}"))?;
        libraries += usize::from(is_lib);
    }
    // Mac and Linux archives name the library through a link. It arrives as a
    // copy: a link needs privileges on Windows, and the folder is flat anyway,
    // so a target with a path in it has no business being here.
    for (name, target) in links {
        let n = name.to_string_lossy();
        let bare = Path::new(&target)
            .file_name()
            .is_some_and(|f| f == target.as_str());
        if !bare {
            return Err(format!("link {n} points outside the engine: {target}"));
        }
        std::fs::copy(staging.join(&target), staging.join(&name))
            .map_err(|e| format!("resolve link {n} -> {target}: {e}"))?;
        libraries += 1;
    }
    Ok(libraries)
}

/// Present and a real library. A link written out as text by an earlier build
/// is a few bytes under the right name, and only a new download mends it.
fn is_library(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() >= 4096)
}

fn engine_installed(flavor: Flavor) -> bool {
    is_library(&library_path(flavor))
}

// ---- C ABI (asr.h, NeMo-Speech.cpp v1) ----------------------------------

#[repr(C)]
struct RecognizerHandle {
    _p: [u8; 0],
}
#[repr(C)]
struct StreamHandle {
    _p: [u8; 0],
}
#[repr(C)]
struct ResultHandle {
    _p: [u8; 0],
}

const STATUS_OK: i32 = 0;

#[repr(C)]
struct BackendConfig {
    size: usize,
    gpu: i32,
}

#[repr(C)]
struct ModelConfig {
    size: usize,
    path: *const c_char,
    name: *const c_char,
}

#[repr(C)]
struct DiarConfig {
    size: usize,
    model_path: *const c_char,
    chunk_frames: i32,
    right_context_frames: i32,
    left_context_frames: i32,
    fifo_frames: i32,
    spkcache_frames: i32,
    update_period_frames: i32,
}

#[repr(C)]
struct EndpointingConfig {
    size: usize,
    enable: bool,
    vad_based: bool,
    stop_history_eou_ms: i32,
}

#[repr(C)]
struct RecognizerConfig {
    size: usize,
    backend: *const BackendConfig,
    model: *const ModelConfig,
    streaming: *const std::ffi::c_void,
    decoder: *const std::ffi::c_void,
    vad: *const std::ffi::c_void,
    endpointing: *const EndpointingConfig,
    postproc: *const std::ffi::c_void,
    diar: *const DiarConfig,
    batching: *const std::ffi::c_void,
    salm: *const std::ffi::c_void,
}

#[repr(C)]
struct SpeechContext {
    size: usize,
    phrases: *const *const c_char,
    phrase_count: usize,
    boost: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RecognitionOptions {
    size: usize,
    request_id: *const c_char,
    language_code: *const c_char,
    interim_results: bool,
    enable_word_time_offsets: bool,
    enable_automatic_punctuation: bool,
    verbatim_transcripts: bool,
    profanity_filter: bool,
    stop_history_eou_ms: i32,
    speech_contexts: *const SpeechContext,
    speech_context_count: usize,
    max_alternatives: i32,
    enable_speaker_diarization: bool,
    max_speaker_count: i32,
}

type Status = i32;

struct Api {
    options_default: unsafe extern "C" fn() -> RecognitionOptions,
    create: unsafe extern "C" fn(*const RecognizerConfig, *mut *mut RecognizerHandle) -> Status,
    recognize_f32: unsafe extern "C" fn(
        *mut RecognizerHandle,
        *const RecognitionOptions,
        *const f32,
        usize,
        i32,
        *mut *mut ResultHandle,
    ) -> Status,
    streaming_recognize: unsafe extern "C" fn(
        *mut RecognizerHandle,
        *const RecognitionOptions,
        *mut *mut StreamHandle,
    ) -> Status,
    stream_push_f32: unsafe extern "C" fn(*mut StreamHandle, *const f32, usize, i32) -> Status,
    stream_force_endpoint: unsafe extern "C" fn(*mut StreamHandle) -> Status,
    stream_finish: unsafe extern "C" fn(*mut StreamHandle) -> Status,
    stream_next: unsafe extern "C" fn(*mut StreamHandle, *mut *mut ResultHandle) -> Status,
    stream_close: unsafe extern "C" fn(*mut StreamHandle),
    destroy: unsafe extern "C" fn(*mut RecognizerHandle),
    result_is_final: unsafe extern "C" fn(*const ResultHandle) -> bool,
    result_transcript: unsafe extern "C" fn(*const ResultHandle, usize) -> *const c_char,
    result_word_count: unsafe extern "C" fn(*const ResultHandle, usize) -> usize,
    result_word_text: unsafe extern "C" fn(*const ResultHandle, usize, usize) -> *const c_char,
    result_word_start: unsafe extern "C" fn(*const ResultHandle, usize, usize) -> i32,
    result_word_end: unsafe extern "C" fn(*const ResultHandle, usize, usize) -> i32,
    result_word_speaker: unsafe extern "C" fn(*const ResultHandle, usize, usize) -> i32,
    result_destroy: unsafe extern "C" fn(*mut ResultHandle),
    last_error: unsafe extern "C" fn() -> *const c_char,
    version: unsafe extern "C" fn() -> *const c_char,
    // Never unloaded: the library runs its own worker threads and holds the
    // model in memory; the pointers above are only valid while this lives.
    _library: libloading::Library,
}

// SAFETY: plain function pointers into a library that is never unloaded.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    // The C library imports `nemo_speech_asr.dll` from its own folder, which
    // is not on the default search path of a host that lives elsewhere.
    #[cfg(windows)]
    {
        use libloading::os::windows::{Library, LOAD_WITH_ALTERED_SEARCH_PATH};
        // SAFETY: loading the pinned, checksummed engine.
        unsafe { Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH).map(Into::into) }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: as above; the install name / rpath is `@loader_path` / `$ORIGIN`.
        unsafe { libloading::Library::new(path) }
    }
}

impl Api {
    fn load(path: &Path) -> Result<Self, TranscribeError> {
        let err = |e: &dyn std::fmt::Display| {
            TranscribeError::LocalModel(format!("Nemotron engine {}: {}", path.display(), e))
        };
        let library = open_library(path).map_err(|e| err(&e))?;
        macro_rules! sym {
            ($name:literal) => {
                // SAFETY: the signatures are the ones declared in asr.h v1.
                unsafe {
                    *library
                        .get(concat!($name, "\0").as_bytes())
                        .map_err(|e| err(&e))?
                }
            };
        }
        Ok(Self {
            options_default: sym!("nemo_speech_asr_recognition_options_default"),
            create: sym!("nemo_speech_asr_create"),
            recognize_f32: sym!("nemo_speech_asr_recognize_f32"),
            streaming_recognize: sym!("nemo_speech_asr_streaming_recognize"),
            stream_push_f32: sym!("nemo_speech_asr_stream_push_f32"),
            stream_force_endpoint: sym!("nemo_speech_asr_stream_force_endpoint"),
            stream_finish: sym!("nemo_speech_asr_stream_finish"),
            stream_next: sym!("nemo_speech_asr_stream_next"),
            stream_close: sym!("nemo_speech_asr_stream_close"),
            destroy: sym!("nemo_speech_asr_destroy"),
            result_is_final: sym!("nemo_speech_asr_result_is_final"),
            result_transcript: sym!("nemo_speech_asr_result_transcript"),
            result_word_count: sym!("nemo_speech_asr_result_word_count"),
            result_word_text: sym!("nemo_speech_asr_result_word_text"),
            result_word_start: sym!("nemo_speech_asr_result_word_start_time"),
            result_word_end: sym!("nemo_speech_asr_result_word_end_time"),
            result_word_speaker: sym!("nemo_speech_asr_result_word_speaker_tag"),
            result_destroy: sym!("nemo_speech_asr_result_destroy"),
            last_error: sym!("nemo_speech_asr_last_error"),
            version: sym!("nemo_speech_asr_version"),
            _library: library,
        })
    }

    /// The thread-local reason for the last failed call, capped at 200
    /// characters like every error body this project surfaces.
    fn last_error(&self) -> String {
        // SAFETY: NULL or a NUL-terminated string valid until the next call
        // on this thread; copied out immediately.
        let s = unsafe { cstr_or_empty((self.last_error)()) };
        if s.is_empty() {
            "unknown error".into()
        } else {
            s.chars().take(200).collect()
        }
    }

    fn check(&self, what: &str, rc: Status) -> Result<(), TranscribeError> {
        if rc == STATUS_OK {
            Ok(())
        } else {
            Err(TranscribeError::LocalModel(format!(
                "Nemotron {what}: status {rc}: {}",
                self.last_error()
            )))
        }
    }
}

/// # Safety
/// `p` is NULL or a valid NUL-terminated string.
unsafe fn cstr_or_empty(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

// ---- Results ------------------------------------------------------------

/// One recognized word. Times are seconds from the start of the stream (or
/// of the utterance, for a one-shot call). `speaker` is 1-based, 0 when the
/// recognizer has no diarizer or could not tell.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub speaker: u8,
}

/// One result off a stream: the text of the current utterance so far, and
/// whether the engine has closed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub text: String,
    pub is_final: bool,
    pub words: Vec<Word>,
}

fn read_result(api: &Api, r: *mut ResultHandle) -> Update {
    assert!(!r.is_null());
    // SAFETY: `r` is a live result handle; every accessor reads memory the
    // result owns, copied out before `result_destroy`.
    unsafe {
        let text = cstr_or_empty((api.result_transcript)(r, 0))
            .trim()
            .to_string();
        let n = (api.result_word_count)(r, 0);
        let mut words = Vec::with_capacity(n);
        for i in 0..n {
            let start = (api.result_word_start)(r, 0, i) as f64 / 1000.0;
            let end = (api.result_word_end)(r, 0, i) as f64 / 1000.0;
            let speaker = (api.result_word_speaker)(r, 0, i).clamp(0, u8::MAX as i32) as u8;
            words.push(Word {
                text: cstr_or_empty((api.result_word_text)(r, 0, i)),
                start: start.max(0.0),
                end: end.max(start.max(0.0)),
                speaker,
            });
        }
        let is_final = (api.result_is_final)(r);
        (api.result_destroy)(r);
        Update {
            text,
            is_final,
            words,
        }
    }
}

// ---- Recognizer ---------------------------------------------------------

/// What a recognizer is built from. Two configs that are equal share one
/// loaded model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSpec {
    pub library: PathBuf,
    pub model: PathBuf,
    /// `None` = CPU, `Some(n)` = GPU device `n` of the build's backend.
    pub gpu: Option<i32>,
    /// A Sortformer diarization GGUF: words then carry speaker tags.
    pub diarizer: Option<PathBuf>,
}

/// The engine's default, the length of a pause between two sentences.
const PAUSE_CLOSES_UTTERANCE_MS: i32 = 800;

pub struct Recognizer {
    api: Arc<Api>,
    handle: *mut RecognizerHandle,
    spec: EngineSpec,
}

// SAFETY: asr.h: "A single recognizer owns one model; callers submit
// independent utterances from multiple threads". Streams are the
// single-threaded part, and `Stream` is not `Sync`.
unsafe impl Send for Recognizer {}
unsafe impl Sync for Recognizer {}

impl Recognizer {
    pub fn create(spec: EngineSpec) -> Result<Self, TranscribeError> {
        assert!(
            spec.model.is_file(),
            "nemotron model missing: {}",
            spec.model.display()
        );
        let api = Arc::new(Api::load(&spec.library)?);
        let model_c = path_cstring(&spec.model)?;
        let diar_c = spec.diarizer.as_deref().map(path_cstring).transpose()?;
        let backend = BackendConfig {
            size: std::mem::size_of::<BackendConfig>(),
            gpu: spec.gpu.unwrap_or(-1),
        };
        let model = ModelConfig {
            size: std::mem::size_of::<ModelConfig>(),
            path: model_c.as_ptr(),
            name: std::ptr::null(),
        };
        let diar = diar_c.as_ref().map(|p| DiarConfig {
            size: std::mem::size_of::<DiarConfig>(),
            model_path: p.as_ptr(),
            chunk_frames: 0,
            right_context_frames: 0,
            left_context_frames: -1,
            fifo_frames: 0,
            spkcache_frames: 0,
            update_period_frames: 0,
        });
        // Off by default in the engine, and then its only final comes when the
        // stream closes: a whole meeting appeared at stop. On, a pause closes
        // the utterance as it happens.
        let endpointing = EndpointingConfig {
            size: std::mem::size_of::<EndpointingConfig>(),
            enable: true,
            vad_based: false,
            stop_history_eou_ms: PAUSE_CLOSES_UTTERANCE_MS,
        };
        let cfg = RecognizerConfig {
            size: std::mem::size_of::<RecognizerConfig>(),
            backend: &backend,
            model: &model,
            streaming: std::ptr::null(),
            decoder: std::ptr::null(),
            vad: std::ptr::null(),
            endpointing: &endpointing,
            postproc: std::ptr::null(),
            diar: diar.as_ref().map_or(std::ptr::null(), |d| d as *const _),
            batching: std::ptr::null(),
            salm: std::ptr::null(),
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: every pointer in `cfg` outlives the call; the library copies
        // what it keeps.
        let rc = unsafe { (api.create)(&cfg, &mut handle) };
        api.check("create", rc)?;
        assert!(
            !handle.is_null(),
            "nemo_speech_asr_create returned OK without a handle"
        );
        crate::log(&format!(
            "[Nemotron] loaded {} on {} (engine {})",
            spec.model
                .file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned()),
            spec.gpu
                .map_or_else(|| "CPU".to_string(), |g| format!("GPU {g}")),
            // SAFETY: a static NUL-terminated string.
            unsafe { cstr_or_empty((api.version)()) },
        ));
        Ok(Self { api, handle, spec })
    }

    pub fn spec(&self) -> &EngineSpec {
        &self.spec
    }

    fn options(&self, req: &Request) -> RecognitionOptions {
        // SAFETY: returns a struct by value with `size` set by the library.
        let mut o = unsafe { (self.api.options_default)() };
        o.language_code = req
            .language
            .as_ref()
            .map_or(std::ptr::null(), |l| l.as_ptr());
        o.interim_results = true;
        o.enable_word_time_offsets = true;
        o.enable_automatic_punctuation = true;
        o.enable_speaker_diarization = req.speakers && self.spec.diarizer.is_some();
        o.speech_contexts = req
            .context
            .as_ref()
            .map_or(std::ptr::null(), |c| c as *const _);
        o.speech_context_count = usize::from(req.context.is_some());
        o
    }

    /// One whole utterance, for the paths that hand over finished audio
    /// (file load, the end-of-dictation pass, meeting windows).
    pub fn transcribe(
        &self,
        pcm_16k: &[f32],
        language: &str,
        keywords: &[String],
    ) -> Result<(String, Vec<Word>), TranscribeError> {
        assert!(!pcm_16k.is_empty(), "nemotron: pcm must not be empty");
        assert!(
            pcm_16k.iter().all(|s| s.is_finite()),
            "nemotron: samples must be finite"
        );
        let req = Request::new(language, keywords, false)?;
        let opts = self.options(&req);
        let mut out = std::ptr::null_mut();
        // SAFETY: `pcm_16k`, `opts` and the strings `req` owns outlive the call.
        let rc = unsafe {
            (self.api.recognize_f32)(
                self.handle,
                &opts,
                pcm_16k.as_ptr(),
                pcm_16k.len(),
                SAMPLE_RATE as i32,
                &mut out,
            )
        };
        self.api.check("recognize", rc)?;
        if out.is_null() {
            return Ok((String::new(), Vec::new()));
        }
        let u = read_result(&self.api, out);
        Ok((u.text, u.words))
    }

    /// A live stream: push audio as it arrives, pull results as they form.
    pub fn stream(
        self: &Arc<Self>,
        language: &str,
        keywords: &[String],
        speakers: bool,
    ) -> Result<Stream, TranscribeError> {
        let req = Request::new(language, keywords, speakers)?;
        let opts = self.options(&req);
        let mut handle = std::ptr::null_mut();
        // SAFETY: as in `transcribe`; the stream copies its options.
        let rc = unsafe { (self.api.streaming_recognize)(self.handle, &opts, &mut handle) };
        self.api.check("stream open", rc)?;
        assert!(!handle.is_null());
        Ok(Stream {
            recognizer: Arc::clone(self),
            handle,
            _req: req,
            pushed_secs: 0.0,
            finished: false,
        })
    }
}

impl Drop for Recognizer {
    fn drop(&mut self) {
        // Deliberately not destroyed: `nemo_speech_asr_destroy` tears down
        // worker threads that may still be finishing a stream another thread
        // just closed, unless `release_idle` already did it after a grace period.
        // The library stays: its threads run from it, and unloading it under
        // them crashes. A CPU recognizer replaced by a later GPU load gets here.
        std::mem::forget(Arc::clone(&self.api));
    }
}

/// A loaded model holds 7-9 GB of commit and 1-3 GB of VRAM on a GPU, whether
/// anyone is talking or not. Loading it again costs 1-3 s, once.
const RELEASE_AFTER_IDLE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

static LAST_USE: Mutex<Option<std::time::Instant>> = Mutex::new(None);
static RELEASE_THREAD_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn note_use() {
    *LAST_USE.lock().unwrap_or_else(|p| p.into_inner()) = Some(std::time::Instant::now());
}

fn idle_for_long_enough(last_use: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last_use.is_none_or(|t| now.duration_since(t) >= RELEASE_AFTER_IDLE)
}

/// Sleeps while a model is loaded, and only then: no thread is left once the
/// slot is empty, and the next load starts another.
fn keep_release_thread_running() {
    use std::sync::atomic::Ordering;
    if RELEASE_THREAD_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let last_use = *LAST_USE.lock().unwrap_or_else(|p| p.into_inner());
        if !idle_for_long_enough(last_use, std::time::Instant::now()) {
            continue;
        }
        let released = release_idle();
        if released > 0 {
            crate::log(&format!(
                "[Nemotron] idle for {} min: released {released} recognizer(s)",
                RELEASE_AFTER_IDLE.as_secs() / 60
            ));
        }
        // Under the lock, so a load cannot slip between the check and the exit.
        let slot = RECOGNIZERS.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_empty() {
            RELEASE_THREAD_RUNNING.store(false, Ordering::SeqCst);
            return;
        }
    });
}

/// Destroys the recognizers no stream uses, and returns how many.
pub fn release_idle() -> usize {
    let mut slot = RECOGNIZERS.lock().unwrap_or_else(|p| p.into_inner());
    let before = slot.len();
    slot.retain(|r| {
        if Arc::strong_count(r) > 1 {
            return true;
        }
        // SAFETY: the slot held the last reference, so no stream or caller can
        // reach this handle, and it leaves the slot here, so once only.
        unsafe { (r.api.destroy)(r.handle) };
        false
    });
    before - slot.len()
}

/// The strings a request points at, kept alive for as long as the C side
/// may read them.
struct Request {
    language: Option<CString>,
    _phrases: Vec<CString>,
    _phrase_ptrs: Vec<*const c_char>,
    context: Option<SpeechContext>,
    speakers: bool,
}

/// How hard a custom-dictionary word is pushed. The library caps it at
/// `max_boost` (10) because an unbounded boost makes words repeat.
const DICTIONARY_BOOST: f32 = 4.0;

impl Request {
    fn new(language: &str, keywords: &[String], speakers: bool) -> Result<Self, TranscribeError> {
        let language = locale_for(language).map(|l| CString::new(l).expect("locale has no NUL"));
        let phrases: Vec<CString> = keywords
            .iter()
            .map(|k| k.trim())
            .filter(|k| !k.is_empty())
            .filter_map(|k| CString::new(k).ok())
            .collect();
        let phrase_ptrs: Vec<*const c_char> = phrases.iter().map(|p| p.as_ptr()).collect();
        let context = (!phrase_ptrs.is_empty()).then_some(SpeechContext {
            size: std::mem::size_of::<SpeechContext>(),
            phrases: phrase_ptrs.as_ptr(),
            phrase_count: phrase_ptrs.len(),
            boost: DICTIONARY_BOOST,
        });
        Ok(Self {
            language,
            _phrases: phrases,
            _phrase_ptrs: phrase_ptrs,
            context,
            speakers,
        })
    }
}

/// Dimmy's language setting is an ISO code or empty (= auto). The model's
/// prompt table takes locales ("it-IT") and bare codes ("it"); empty and
/// "auto" leave detection to the model, which on mixed Italian/English
/// meetings is what keeps the English from being forced into Italian.
fn locale_for(language: &str) -> Option<&str> {
    let l = language.trim();
    (!l.is_empty() && !l.eq_ignore_ascii_case("auto")).then_some(l)
}

fn path_cstring(p: &Path) -> Result<CString, TranscribeError> {
    CString::new(p.to_string_lossy().into_owned())
        .map_err(|_| TranscribeError::LocalModel(format!("path has a NUL: {}", p.display())))
}

// ---- Stream -------------------------------------------------------------

pub struct Stream {
    recognizer: Arc<Recognizer>,
    handle: *mut StreamHandle,
    _req: Request,
    pushed_secs: f64,
    finished: bool,
}

// SAFETY: a stream is driven by one thread at a time (asr.h: "single-threaded
// by contract"); moving it to the thread that drives it is fine, sharing is not.
unsafe impl Send for Stream {}

impl Stream {
    /// Buffer 16 kHz mono audio. Decoding happens in `poll`.
    pub fn push(&mut self, pcm_16k: &[f32]) -> Result<(), TranscribeError> {
        self.push_at(pcm_16k, SAMPLE_RATE as u32)
    }

    /// Buffer mono audio at `rate` (8-96 kHz). The engine resamples inside
    /// the stream, continuously, which resampling each 100 ms piece on our
    /// side would not be. The rate must not change within a stream.
    pub fn push_at(&mut self, pcm: &[f32], rate: u32) -> Result<(), TranscribeError> {
        assert!(!self.finished, "push after finish");
        assert!(
            (8_000..=96_000).contains(&rate),
            "nemotron: rate {rate} out of 8-96 kHz"
        );
        if pcm.is_empty() {
            return Ok(());
        }
        assert!(
            pcm.iter().all(|s| s.is_finite()),
            "nemotron: samples must be finite"
        );
        let api = &self.recognizer.api;
        // SAFETY: the slice outlives the call; the stream copies it.
        let rc =
            unsafe { (api.stream_push_f32)(self.handle, pcm.as_ptr(), pcm.len(), rate as i32) };
        api.check("push", rc)?;
        self.pushed_secs += pcm.len() as f64 / rate as f64;
        Ok(())
    }

    /// Close the open utterance now; its final comes on the next `poll`.
    pub fn force_endpoint(&mut self) -> Result<(), TranscribeError> {
        let api = &self.recognizer.api;
        // SAFETY: live stream handle.
        let rc = unsafe { (api.stream_force_endpoint)(self.handle) };
        api.check("force endpoint", rc)
    }

    /// Every result that is ready now, oldest first.
    pub fn poll(&mut self) -> Result<Vec<Update>, TranscribeError> {
        let api = &self.recognizer.api;
        let mut out = Vec::new();
        loop {
            let mut r = std::ptr::null_mut();
            // SAFETY: live stream handle; a non-NULL result is ours to destroy.
            let rc = unsafe { (api.stream_next)(self.handle, &mut r) };
            api.check("next", rc)?;
            if r.is_null() {
                return Ok(out);
            }
            out.push(read_result(api, r));
        }
    }

    /// No more audio: flush the tail and return what it produced.
    pub fn finish(&mut self) -> Result<Vec<Update>, TranscribeError> {
        if !self.finished {
            let api = &self.recognizer.api;
            // SAFETY: live stream handle.
            let rc = unsafe { (api.stream_finish)(self.handle) };
            api.check("finish", rc)?;
            self.finished = true;
        }
        self.poll()
    }

    /// Seconds of audio pushed so far.
    pub fn pushed_secs(&self) -> f64 {
        self.pushed_secs
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: closed exactly once, here.
        unsafe { (self.recognizer.api.stream_close)(self.handle) };
        note_use();
    }
}

// ---- Process-wide recognizer -------------------------------------------

/// At most two: one without the diarizer (dictation, the mic of a call,
/// one-shot transcription) and one with it (a track that wants speakers).
static RECOGNIZERS: Mutex<Vec<Arc<Recognizer>>> = Mutex::new(Vec::new());

/// The recognizer for `spec`, loaded on first use and kept: loading the
/// model costs seconds and a dictation must not pay it twice. A new spec of
/// the same kind (with or without diarizer) replaces the old one.
pub fn recognizer(spec: &EngineSpec) -> Result<Arc<Recognizer>, TranscribeError> {
    let mut slot = RECOGNIZERS.lock().unwrap_or_else(|p| p.into_inner());
    let available = crate::setup::available_memory_bytes();
    let r = load_into(&mut slot, spec, available, Recognizer::spec, || {
        let r = Arc::new(Recognizer::create(spec.clone())?);
        warm_up(&r)?;
        release_before_ggml_exits();
        Ok(r)
    })?;
    note_use();
    keep_release_thread_running();
    Ok(r)
}

/// Touches the slot only after the new recognizer exists, because what leaves
/// the slot is forgotten and not freed (see `Drop for Recognizer`): a GPU load
/// refused for memory must not throw away the CPU models the fallback is about
/// to ask for, or every call leaks one and loads another (15 GB and an abort
/// in a 3-minute meeting, 2026-10-30).
fn load_into<R>(
    slot: &mut Vec<Arc<R>>,
    spec: &EngineSpec,
    available: Option<u64>,
    spec_of: impl Fn(&R) -> &EngineSpec,
    create: impl FnOnce() -> Result<Arc<R>, TranscribeError>,
) -> Result<Arc<R>, TranscribeError> {
    if let Some(r) = slot.iter().find(|r| spec_of(r) == spec) {
        return Ok(Arc::clone(r));
    }
    let runtime = if spec.gpu.is_some() {
        GPU_RUNTIME_BYTES
    } else {
        CPU_RUNTIME_BYTES
    };
    let needed = spec_bytes(spec) + runtime + LOAD_HEADROOM_BYTES;
    if !enough_memory(available, needed) {
        return Err(TranscribeError::LocalModel(format!(
            "not enough free memory to load Nemotron ({} MB needed)",
            needed / (1024 * 1024)
        )));
    }
    let r = create()?;
    if spec.gpu.is_some() {
        slot.clear();
    } else {
        slot.retain(|old| spec_of(old).diarizer.is_some() != spec.diarizer.is_some());
    }
    slot.push(Arc::clone(&r));
    assert!(
        slot.len() <= 2,
        "one recognizer with and one without the diarizer"
    );
    Ok(r)
}

/// ggml-metal's device is torn down by an exit handler and asserts that no
/// buffer is left (`GGML_ASSERT([rsets->data count] == 0)`, exit 134), so a
/// Dimmy that quit with a model loaded would report a crash on every quit.
/// Exit handlers run last-registered first, and the device registers its own
/// while the first recognizer is created: this one, registered after it, runs
/// before it. Metal only; CUDA exits clean.
#[cfg(target_os = "macos")]
fn release_before_ggml_exits() {
    extern "C" fn release() {
        if let Ok(mut slot) = RECOGNIZERS.try_lock() {
            for r in slot.drain(..) {
                // SAFETY: the process is exiting and nothing will use it again.
                unsafe { (r.api.destroy)(r.handle) };
            }
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: registers a function with no arguments that lives for the process.
    ONCE.call_once(|| unsafe {
        libc::atexit(release);
    });
}

#[cfg(not(target_os = "macos"))]
fn release_before_ggml_exits() {}

fn spec_bytes(spec: &EngineSpec) -> u64 {
    [Some(&spec.model), spec.diarizer.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// No reading (macOS, which grows its swap on demand) is not a refusal.
fn enough_memory(available: Option<u64>, needed: u64) -> bool {
    available.is_none_or(|free| free >= needed)
}

/// `create` returns before the weights are resident: the first push of the
/// first stream then stalls for about a second (measured 1.1 s, CPU), which
/// in a dictation is a second of nothing on screen after the person starts
/// talking. Half a second of silence through a throwaway stream pays it here,
/// while the hotkey is still being pressed.
fn warm_up(r: &Arc<Recognizer>) -> Result<(), TranscribeError> {
    let started = std::time::Instant::now();
    let mut s = Arc::clone(r).stream("", &[], false)?;
    s.push(&vec![0.0; SAMPLE_RATE / 2])?;
    s.finish()?;
    crate::log(&format!(
        "[Nemotron] warm-up {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    ));
    Ok(())
}

// ---- Live dictation ------------------------------------------------------

/// `(backspace, delta, cumulative, is_final)`, fanned out as `stt_chunk`:
/// the cloud streamers' shape plus how many characters to delete first.
pub type ChunkCallback = dyn Fn(usize, &str, &str, bool) + Send + Sync + 'static;

/// How often new capture is handed to the engine. The model consumes 320 ms
/// chunks; 100 ms keeps the text within a chunk of the voice, the cadence
/// every other live engine in the app uses.
const PUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Live dictation: a thread that reads the capture buffer as it fills, the
/// way the Deepgram and OpenAI streamers do, and pushes it into one stream
/// for the whole dictation. One stream, never reset: a reset throws away
/// the encoder's context and the detected language, and the first word of
/// the next sentence went with them (measured on these recordings).
pub struct Streamer {
    cancel: Arc<std::sync::atomic::AtomicBool>,
    final_text: Arc<Mutex<String>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Streamer {
    /// Returns at once: the model loads on the streamer's thread while the
    /// capture keeps filling `buffer`, so nothing said during the load is
    /// lost, only shown a moment later.
    pub fn start(
        buffer: Arc<Mutex<Vec<f32>>>,
        rate: u32,
        language: String,
        keywords: Vec<String>,
        on_chunk: Arc<ChunkCallback>,
    ) -> Self {
        assert!(rate > 0, "capture rate must be positive");
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let final_text = Arc::new(Mutex::new(String::new()));
        let (c, f) = (Arc::clone(&cancel), Arc::clone(&final_text));
        let handle = std::thread::Builder::new()
            .name("nemotron-stream".into())
            .spawn(move || {
                if let Err(e) = run_stream(
                    &buffer,
                    rate,
                    &language,
                    &keywords,
                    &c,
                    &f,
                    on_chunk.as_ref(),
                ) {
                    crate::log(&format!("[Nemotron] live dictation stopped: {e}"));
                }
            })
            .expect("spawn nemotron-stream");
        Self {
            cancel,
            final_text,
            handle: Some(handle),
        }
    }

    /// Flush the tail and return the whole dictation.
    pub fn stop(mut self) -> String {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.final_text
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }
}

fn run_stream(
    buffer: &Arc<Mutex<Vec<f32>>>,
    rate: u32,
    language: &str,
    keywords: &[String],
    cancel: &std::sync::atomic::AtomicBool,
    final_text: &Mutex<String>,
    on_chunk: &ChunkCallback,
) -> Result<(), TranscribeError> {
    let recognizer = app_recognizer(false)?;
    let mut stream = recognizer.stream(language, keywords, false)?;
    let mut transcript = Transcript::default();
    let mut cursor = 0usize;
    let emit = |updates: Vec<Update>, transcript: &mut Transcript| {
        for u in updates {
            let t = transcript.apply(&u);
            if !t.delta.is_empty() || t.backspace > 0 || u.is_final {
                on_chunk(t.backspace, &t.delta, &t.cumulative, u.is_final);
            }
        }
    };
    loop {
        let stopping = cancel.load(std::sync::atomic::Ordering::SeqCst);
        let fresh = match buffer.lock() {
            Ok(b) if b.len() > cursor => {
                let s = b[cursor..].to_vec();
                cursor = b.len();
                s
            }
            // A buffer cleared under us (a new dictation) restarts the cursor.
            Ok(b) => {
                cursor = cursor.min(b.len());
                Vec::new()
            }
            Err(_) => Vec::new(),
        };
        stream.push_at(&fresh, rate)?;
        if stopping {
            break;
        }
        emit(stream.poll()?, &mut transcript);
        std::thread::sleep(PUSH_INTERVAL);
    }
    emit(stream.finish()?, &mut transcript);
    let text = transcript.text();
    if let Ok(mut f) = final_text.lock() {
        *f = text;
    }
    Ok(())
}

// ---- Meetings -----------------------------------------------------------

/// One line of a meeting transcript: who, from when, what.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Seconds from the start of the track (pauses excluded, like the files
    /// on disk).
    pub start: f64,
    /// 1-based engine speaker, `None` on a track without diarization.
    pub speaker: Option<u8>,
    pub text: String,
}

/// A turn shorter than this many words is folded into its neighbour. On
/// fast exchanges the diarizer flips single words between voices ("Che
/// cazzo ci" / "fanno i federali qui" on the 2026-10-29 clip); a two-word
/// "turn" is almost always that, not a real change of speaker.
const MIN_TURN_WORDS: usize = 3;

/// Split one final utterance into speaker turns. Untagged words (speaker 0)
/// join the turn they sit in; short turns are folded into the previous
/// one (or the next, at the start), so the transcript changes speaker only
/// where somebody actually took the floor.
pub fn turns(words: &[Word], fallback_text: &str, offset: f64) -> Vec<Line> {
    if words.is_empty() {
        let text = fallback_text.trim();
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![Line {
                start: offset,
                speaker: None,
                text: text.into(),
            }]
        };
    }
    let mut raw: Vec<(u8, f64, Vec<&str>)> = Vec::new();
    for w in words {
        let text = w.text.trim();
        if text.is_empty() {
            continue;
        }
        match raw.last_mut() {
            Some(t) if t.0 == w.speaker || w.speaker == 0 => t.2.push(text),
            _ => raw.push((w.speaker, w.start, vec![text])),
        }
    }
    let mut merged: Vec<(u8, f64, Vec<&str>)> = Vec::new();
    for t in raw {
        match merged.last_mut() {
            Some(prev) if t.2.len() < MIN_TURN_WORDS || prev.0 == t.0 => prev.2.extend(t.2),
            Some(prev) if prev.2.len() < MIN_TURN_WORDS => {
                // A short opening turn belongs to whoever speaks next.
                let mut words = std::mem::take(&mut prev.2);
                words.extend(t.2);
                *prev = (t.0, prev.1, words);
            }
            _ => merged.push(t),
        }
    }
    merged
        .into_iter()
        .map(|(speaker, start, words)| Line {
            start: offset + start,
            speaker: (speaker > 0).then_some(speaker),
            text: words.join(" "),
        })
        .collect()
}

/// What a meeting track heard, for `speakers.json`.
#[derive(Debug, Default)]
pub struct Spoken {
    /// (speaker, start, end) per tagged word, track seconds.
    pub segments: Vec<(u8, f64, f64)>,
    /// (start, end) per word on a track without a diarizer, track seconds.
    pub voiced: Vec<(f64, f64)>,
    /// The diarizer ran for the whole track, so the labels already in the
    /// transcript are complete and no pass after the meeting is needed.
    pub covered: bool,
}

/// The speaker list for `speakers.json`, in the names the transcript
/// already uses ("Speaker N", N the engine's number), so renaming one
/// rewrites the same labels. Words closer than 0.3 s join into one span.
pub fn speaker_list(band: &str, segments: &[(u8, f64, f64)]) -> Vec<crate::diarize::SpeakerInfo> {
    let mut ids: Vec<u8> = segments.iter().map(|s| s.0).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .map(|n| {
            let spans = join_spans(
                segments
                    .iter()
                    .filter(|s| s.0 == n)
                    .map(|&(_, a, e)| (a, e)),
            );
            crate::diarize::SpeakerInfo {
                id: format!("S{n}"),
                name: crate::diarize::default_name(n as usize),
                band: band.to_string(),
                talk_secs: spans.iter().map(|(a, e)| e - a).sum(),
                segments: spans,
            }
        })
        .collect()
}

/// The one lane of a track without a diarizer (the mic on a call), under the
/// label its lines already carry. `None` when nobody spoke on it.
pub fn track_lane(band: &str, voiced: &[(f64, f64)]) -> Option<crate::diarize::SpeakerInfo> {
    let spans = join_spans(voiced.iter().copied());
    if spans.is_empty() {
        return None;
    }
    Some(crate::diarize::SpeakerInfo {
        id: band.to_string(),
        name: band.to_string(),
        band: band.to_string(),
        talk_secs: spans.iter().map(|(a, e)| e - a).sum(),
        segments: spans,
    })
}

/// Words closer than 0.3 s join into one span, rounded to centiseconds.
fn join_spans(words: impl Iterator<Item = (f64, f64)>) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for (a, e) in words {
        match spans.last_mut() {
            Some(last) if a - last.1 < 0.3 => last.1 = last.1.max(e),
            _ => spans.push((a, e)),
        }
    }
    spans
        .into_iter()
        .map(|(a, e)| ((a * 100.0).round() / 100.0, (e * 100.0).round() / 100.0))
        .collect()
}

/// What one window of a meeting track produced.
#[derive(Debug, Default)]
pub struct Fed {
    /// Utterances the engine closed.
    pub lines: Vec<Line>,
    /// The open utterance as it stands, when it changed: the sentence still
    /// being said, for the live view.
    pub partial: Option<String>,
    /// Seconds of new audio this window carried, past the overlap.
    pub fresh_secs: f64,
}

/// A speaker who never pauses for 800 ms would keep one utterance open for
/// minutes, and with it a line nobody can read yet and speakers nobody can
/// see (they are tagged at the final). Past this many words the next
/// sentence end closes it; past twice this many, whatever comes.
const LONG_UTTERANCE_WORDS: usize = 30;

fn should_close(partial: &str) -> bool {
    let words = partial.split_whitespace().count();
    words >= LONG_UTTERANCE_WORDS * 2
        || (words >= LONG_UTTERANCE_WORDS && partial.trim_end().ends_with(['.', '?', '!']))
}

/// One track of a meeting (mic or system), streamed as the recording goes.
/// The meeting hands over overlapping windows; only the part past what was
/// already pushed goes in, so the stream sees the track exactly once.
pub struct MeetingTrack {
    stream: Stream,
    rate: u32,
    /// Track sample (at `rate`) up to which audio has been pushed.
    pushed_until: usize,
    speakers: bool,
    language: String,
    keywords: Vec<String>,
    /// Maps the stream's clock to the track's: `(stream second, track
    /// second)` where each stretch began. More than one entry means a window
    /// was dropped on the way here and the stream never saw that audio.
    clock: Vec<(f64, f64)>,
    /// Seconds of audio pushed into the current stream.
    stream_secs: f64,
    last_partial: String,
    /// Every tagged word as (speaker, start, end), track seconds: the lanes
    /// and talk time `speakers.json` carries.
    spoken: Vec<(u8, f64, f64)>,
    /// Every word as (start, end) while the track has no diarizer.
    voiced: Vec<(f64, f64)>,
    /// Set once the diarizer was dropped mid-meeting: the labels then cover
    /// only part of it.
    dropped: bool,
}

/// Map a stream second to a track second through the stream's clock.
fn clock_to_track(clock: &[(f64, f64)], t: f64) -> f64 {
    assert!(!clock.is_empty(), "a clock has at least its origin");
    let &(s, k) = clock
        .iter()
        .rev()
        .find(|(s, _)| *s <= t)
        .unwrap_or(&clock[0]);
    k + (t - s)
}

impl MeetingTrack {
    pub fn open(
        rate: u32,
        language: &str,
        keywords: &[String],
        speakers: bool,
    ) -> Result<Self, TranscribeError> {
        let r = app_recognizer(speakers)?;
        let speakers = speakers && r.spec().diarizer.is_some();
        Ok(Self {
            stream: r.stream(language, keywords, speakers)?,
            rate,
            pushed_until: 0,
            speakers,
            language: language.into(),
            keywords: keywords.to_vec(),
            clock: vec![(0.0, 0.0)],
            stream_secs: 0.0,
            last_partial: String::new(),
            spoken: Vec::new(),
            voiced: Vec::new(),
            dropped: false,
        })
    }

    pub fn has_speakers(&self) -> bool {
        self.speakers
    }

    pub fn on_gpu(&self) -> bool {
        self.stream.recognizer.spec().gpu.is_some()
    }

    /// Push the new part of a window that starts at `track_start`, and
    /// return what it produced.
    pub fn feed(&mut self, window: &[f32], track_start: usize) -> Result<Fed, TranscribeError> {
        let skip = self
            .pushed_until
            .saturating_sub(track_start)
            .min(window.len());
        let fresh = &window[skip..];
        let fresh_start = track_start + skip;
        if fresh_start > self.pushed_until {
            self.clock
                .push((self.stream_secs, fresh_start as f64 / self.rate as f64));
        }
        let fresh_secs = fresh.len() as f64 / self.rate as f64;
        self.stream.push_at(fresh, self.rate)?;
        self.stream_secs += fresh_secs;
        self.pushed_until = self.pushed_until.max(track_start + window.len());
        let updates = self.stream.poll()?;
        let mut partial = None;
        for u in &updates {
            if u.is_final {
                self.last_partial.clear();
                partial = Some(String::new());
            } else if u.text != self.last_partial {
                self.last_partial = u.text.clone();
                partial = Some(u.text.clone());
            }
        }
        if should_close(&self.last_partial) {
            self.stream.force_endpoint()?;
        }
        Ok(Fed {
            lines: self.lines(updates),
            partial,
            fresh_secs,
        })
    }

    /// Live speakers cost more than the CPU of a slow machine has: keep the
    /// text live and leave who-said-what to the pass after the meeting.
    /// Reopens the stream without the diarizer from this point on.
    pub fn drop_speakers(&mut self) -> Result<Vec<Line>, TranscribeError> {
        if !self.speakers {
            return Ok(Vec::new());
        }
        let tail = self.stream.finish()?;
        let lines = self.lines(tail);
        let r = app_recognizer(false)?;
        self.stream = r.stream(&self.language, &self.keywords, false)?;
        self.clock = vec![(0.0, self.pushed_until as f64 / self.rate as f64)];
        self.stream_secs = 0.0;
        self.last_partial.clear();
        self.speakers = false;
        self.dropped = true;
        Ok(lines)
    }

    /// Flush the tail. Returns its lines and what the track heard: who
    /// spoke when, and whether those labels cover the whole track.
    pub fn finish(mut self) -> Result<(Vec<Line>, Spoken), TranscribeError> {
        let tail = self.stream.finish()?;
        let lines = self.lines(tail);
        let covered = self.speakers && !self.dropped;
        Ok((
            lines,
            Spoken {
                segments: self.spoken,
                voiced: self.voiced,
                covered,
            },
        ))
    }

    fn lines(&mut self, updates: Vec<Update>) -> Vec<Line> {
        let mut out = Vec::new();
        for u in updates.into_iter().filter(|u| u.is_final) {
            let words: Vec<Word> = u
                .words
                .into_iter()
                .map(|w| Word {
                    start: clock_to_track(&self.clock, w.start),
                    end: clock_to_track(&self.clock, w.end),
                    speaker: if self.speakers { w.speaker } else { 0 },
                    text: w.text,
                })
                .collect();
            if self.speakers {
                self.spoken.extend(
                    words
                        .iter()
                        .filter(|w| w.speaker > 0)
                        .map(|w| (w.speaker, w.start, w.end)),
                );
            } else {
                self.voiced.extend(words.iter().map(|w| (w.start, w.end)));
            }
            // Word times are already on the track's clock; a result without
            // words is placed where the current stream began.
            let offset = if words.is_empty() {
                clock_to_track(&self.clock, 0.0)
            } else {
                0.0
            };
            out.extend(turns(&words, &u.text, offset));
        }
        out
    }
}

// ---- Turning results into text the app can use --------------------------

/// The furthest back a correction reaches: the words of the open
/// utterance that were already typed and that the engine has since
/// changed. Past this many, the words on screen stay as they are rather
/// than have a long run of text deleted under the person's cursor.
const MAX_REVISE_WORDS: usize = 8;

/// What to do at the cursor for one update.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Typed {
    /// Characters to delete before typing `delta`: the revised words and
    /// the space the host typed after each.
    pub backspace: usize,
    /// Whole words, space-separated, no leading or trailing space.
    pub delta: String,
    /// Everything recognised so far, for captions and history.
    pub cumulative: String,
}

/// Keeps what was typed at the cursor and turns each result into the edit
/// the host makes there.
///
/// Whole words only. The engine's partials grow inside a word ("informa",
/// then "informazioni"), and the hosts type each delta followed by a space,
/// the contract the cloud streamers keep: handing them pieces put a space
/// in the middle of nearly every word. So the last word of an open
/// utterance is held back until the next one starts, and a closed one is
/// typed out whole, punctuation included.
///
/// When the engine changes a word already typed ("PI" becomes "API"), the
/// edit deletes back to it and retypes from there, within the last
/// `MAX_REVISE_WORDS` words. Closed utterances are never touched: the
/// engine does not change them, and nobody wants a sentence from a minute
/// ago rewritten under their cursor.
#[derive(Debug, Default)]
pub struct Transcript {
    finals: Vec<String>,
    /// Words already handed to the host, in order.
    sent: Vec<String>,
    /// How many of `sent` belong to closed utterances: fixed for good.
    fixed: usize,
}

impl Transcript {
    /// Text of every closed utterance plus the open one.
    fn cumulative(&self, partial: &str) -> String {
        let mut parts: Vec<&str> = self.finals.iter().map(String::as_str).collect();
        if !partial.is_empty() {
            parts.push(partial);
        }
        parts.join(" ")
    }

    pub fn apply(&mut self, u: &Update) -> Typed {
        let cumulative = self.cumulative(&u.text);
        if u.is_final && !u.text.is_empty() {
            self.finals.push(u.text.clone());
        }
        let target = if u.is_final {
            self.cumulative("")
        } else {
            cumulative.clone()
        };
        let words: Vec<&str> = target.split_whitespace().collect();
        let stable = if u.is_final {
            words.len()
        } else {
            words.len().saturating_sub(1)
        };
        let agree = self
            .sent
            .iter()
            .zip(&words)
            .take_while(|(a, b)| a.as_str() == **b)
            .count();
        // Delete back to the first changed word when it is recent and the
        // replacement is already settled; otherwise keep the screen and
        // carry on after it.
        let revisable =
            agree >= self.fixed && self.sent.len() - agree <= MAX_REVISE_WORDS && stable > agree;
        let from = if agree == self.sent.len() || revisable {
            agree
        } else {
            self.sent.len()
        };
        let backspace: usize = self.sent[from..]
            .iter()
            .map(|w| w.chars().count() + 1)
            .sum();
        let mut typed = Typed {
            backspace,
            delta: String::new(),
            cumulative,
        };
        let can_type = from == agree || u.is_final;
        if can_type && stable > from {
            self.sent.truncate(from);
            let fresh = &words[from..stable];
            self.sent.extend(fresh.iter().map(|w| w.to_string()));
            typed.delta = fresh.join(" ");
        } else {
            typed.backspace = 0;
        }
        if u.is_final {
            self.fixed = self.sent.len();
        }
        typed
    }

    /// Everything said, closed utterances joined.
    pub fn text(&self) -> String {
        self.cumulative("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(text: &str, is_final: bool) -> Update {
        Update {
            text: text.into(),
            is_final,
            words: Vec::new(),
        }
    }

    fn w(text: &str, start: f64, speaker: u8) -> Word {
        Word {
            text: text.into(),
            start,
            end: start + 0.2,
            speaker,
        }
    }

    #[test]
    fn turns_follow_the_speaker_and_fold_flickers() {
        let words = [
            w("Mi", 28.2, 2),
            w("sembra", 28.4, 2),
            w("un", 28.6, 2),
            w("buon", 28.8, 2),
            w("momento", 29.0, 2),
            w("Giustificato", 34.0, 3),
            w("è", 34.3, 3),
            w("giusto", 34.5, 3),
            // A two-word flicker to speaker 1 inside speaker 3's turn.
            w("non", 34.9, 1),
            w("sempre", 35.1, 1),
            w("coincidono", 35.4, 3),
        ];
        let lines = turns(&words, "", 0.0);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].speaker, Some(2));
        assert_eq!(lines[0].start, 28.2);
        assert_eq!(lines[1].speaker, Some(3));
        assert_eq!(lines[1].text, "Giustificato è giusto non sempre coincidono");
    }

    #[test]
    fn a_short_opening_turn_goes_to_the_next_speaker() {
        let words = [
            w("Che", 116.0, 2),
            w("cazzo", 116.2, 2),
            w("fanno", 116.5, 3),
            w("i", 116.7, 3),
            w("federali", 116.9, 3),
            w("qui", 117.2, 3),
        ];
        let lines = turns(&words, "", 0.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].speaker, Some(3));
        assert_eq!(lines[0].start, 116.0);
        assert_eq!(lines[0].text, "Che cazzo fanno i federali qui");
    }

    #[test]
    fn untagged_words_and_wordless_results_still_make_lines() {
        let lines = turns(
            &[w("ciao", 1.0, 0), w("a", 1.2, 0), w("tutti", 1.4, 0)],
            "",
            10.0,
        );
        assert_eq!(
            lines,
            vec![Line {
                start: 11.0,
                speaker: None,
                text: "ciao a tutti".into()
            }]
        );
        let lines = turns(&[], " solo testo ", 5.0);
        assert_eq!(
            lines,
            vec![Line {
                start: 5.0,
                speaker: None,
                text: "solo testo".into()
            }]
        );
        assert!(turns(&[], "  ", 0.0).is_empty());
    }

    fn engine_zip(name: &str, entries: &[(&str, &[u8])], links: &[(&str, &str)]) -> PathBuf {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("dimmy-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("engine.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (n, data) in entries {
            zip.start_file(*n, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        for (n, target) in links {
            zip.add_symlink(*n, *target, opts).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn unpack(zip_path: &Path) -> (PathBuf, Result<usize, String>) {
        let out = zip_path.parent().unwrap().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path).unwrap()).unwrap();
        let r = unpack_engine(&mut zip, &out);
        (out, r)
    }

    #[test]
    fn a_linked_library_arrives_as_the_library_not_as_the_link_text() {
        // Mac and Linux archives carry libX.dylib -> libX.1.dylib. Written out
        // as a plain file it held the 28 bytes of the target's name, and
        // dlopen said "not an object file": Nemotron never ran there.
        let path = engine_zip(
            "link",
            &[("e/libnemo_speech_asr_c.1.dylib", b"real library bytes")],
            &[(
                "e/libnemo_speech_asr_c.dylib",
                "libnemo_speech_asr_c.1.dylib",
            )],
        );
        let (out, r) = unpack(&path);
        r.unwrap();
        assert_eq!(
            std::fs::read(out.join("libnemo_speech_asr_c.dylib")).unwrap(),
            b"real library bytes"
        );
    }

    #[test]
    fn link_text_left_by_an_earlier_build_does_not_count_as_an_engine() {
        let dir = std::env::temp_dir().join(format!("dimmy-islib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("text.dylib");
        std::fs::write(&text, "libnemo_speech_asr_c.1.dylib").unwrap();
        let real = dir.join("real.dylib");
        std::fs::write(&real, vec![0u8; 4096]).unwrap();
        assert!(!is_library(&text));
        assert!(is_library(&real));
        assert!(!is_library(&dir.join("missing.dylib")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_cannot_reach_outside_the_engine_folder() {
        for target in ["../secret.dylib", "/etc/passwd", "sub/x.dylib"] {
            let path = engine_zip(
                "escape",
                &[("e/real.dylib", b"bytes")],
                &[("e/libnemo_speech_asr_c.dylib", target)],
            );
            let (out, r) = unpack(&path);
            assert!(r.is_err(), "{target} must be refused");
            assert!(!out.join("libnemo_speech_asr_c.dylib").exists());
        }
    }

    #[test]
    fn full_precision_runs_only_when_wanted_and_it_fits() {
        let plenty = Some(MODEL_F32.bytes + F32_HEADROOM_BYTES);
        let short = Some(MODEL_F32.bytes);
        assert_eq!(pick_model(false, None, plenty).file, MODEL.file);
        assert_eq!(pick_model(true, None, plenty).file, MODEL_F32.file);
        assert_eq!(pick_model(true, None, short).file, MODEL.file);
        assert_eq!(pick_model(true, None, None).file, MODEL_F32.file);
        assert_eq!(
            pick_model(true, Some(true), short).file,
            MODEL_F32.file,
            "already loaded: its own weights are why memory reads short"
        );
    }

    #[test]
    fn the_model_on_the_gpu_stays_whatever_settings_now_say() {
        let plenty = Some(MODEL_F32.bytes + F32_HEADROOM_BYTES);
        assert_eq!(pick_model(true, Some(false), plenty).file, MODEL.file);
        assert_eq!(pick_model(false, Some(true), plenty).file, MODEL_F32.file);
    }

    #[test]
    fn a_load_that_does_not_fit_is_refused_instead_of_aborting() {
        assert!(enough_memory(Some(100), 100));
        assert!(!enough_memory(Some(99), 100));
        assert!(enough_memory(None, u64::MAX));
    }

    fn fake_spec(gpu: bool, diarizer: bool) -> EngineSpec {
        EngineSpec {
            library: "engine".into(),
            model: "model".into(),
            gpu: gpu.then_some(0),
            diarizer: diarizer.then(|| "diarizer".into()),
        }
    }

    fn fake_load(
        slot: &mut Vec<Arc<EngineSpec>>,
        spec: &EngineSpec,
        available: Option<u64>,
        loads: &mut usize,
    ) -> Result<Arc<EngineSpec>, TranscribeError> {
        load_into(
            slot,
            spec,
            available,
            |s| s,
            || {
                *loads += 1;
                Ok(Arc::new(spec.clone()))
            },
        )
    }

    #[test]
    fn a_refused_gpu_load_keeps_the_cpu_recognizers_it_falls_back_to() {
        let (mut slot, mut loads) = (Vec::new(), 0);
        let (gpu, cpu) = (fake_spec(true, true), fake_spec(false, false));
        for _ in 0..4 {
            let refused = fake_load(&mut slot, &gpu, Some(0), &mut loads);
            assert!(refused.is_err(), "no memory, no GPU");
            fake_load(&mut slot, &cpu, None, &mut loads).expect("CPU fallback");
        }
        assert_eq!(loads, 1, "the fallback reuses the model it already loaded");
        assert_eq!(slot.len(), 1);
    }

    #[test]
    fn a_gpu_load_that_fails_keeps_the_cpu_recognizers_too() {
        let (mut slot, mut loads) = (Vec::new(), 0);
        let (gpu, cpu) = (fake_spec(true, true), fake_spec(false, false));
        fake_load(&mut slot, &cpu, None, &mut loads).expect("CPU");
        let failed = load_into(
            &mut slot,
            &gpu,
            None,
            |s| s,
            || Err(TranscribeError::LocalModel("driver too old".into())),
        );
        assert!(failed.is_err());
        assert_eq!(slot.len(), 1);
    }

    #[test]
    fn a_gpu_recognizer_replaces_the_cpu_ones_once_it_exists() {
        let (mut slot, mut loads) = (Vec::new(), 0);
        fake_load(&mut slot, &fake_spec(false, false), None, &mut loads).expect("CPU");
        fake_load(&mut slot, &fake_spec(false, true), None, &mut loads).expect("CPU, speakers");
        assert_eq!(slot.len(), 2, "one with and one without the diarizer");
        fake_load(&mut slot, &fake_spec(true, true), None, &mut loads).expect("GPU");
        assert_eq!(slot.len(), 1, "one model serves every stream on a GPU");
    }

    #[test]
    fn a_model_is_released_only_after_ten_idle_minutes() {
        let now = std::time::Instant::now() + RELEASE_AFTER_IDLE * 2;
        assert!(idle_for_long_enough(None, now));
        assert!(idle_for_long_enough(Some(now - RELEASE_AFTER_IDLE), now));
        let recent = now - RELEASE_AFTER_IDLE + std::time::Duration::from_secs(1);
        assert!(!idle_for_long_enough(Some(recent), now));
    }

    #[test]
    fn the_cpu_engine_never_runs_f32() {
        let spec = spec_for(Flavor::Cpu, false);
        assert!(spec.model.ends_with(MODEL.file), "{:?}", spec.model);
    }

    #[test]
    fn a_dropped_window_keeps_word_times_on_the_track() {
        // 10 s pushed, 25 s lost on the way here, then pushed from track 35.
        let clock = [(0.0, 0.0), (10.0, 35.0)];
        assert_eq!(clock_to_track(&clock, 4.0), 4.0);
        assert_eq!(clock_to_track(&clock, 10.0), 35.0);
        assert_eq!(clock_to_track(&clock, 12.5), 37.5);
        assert_eq!(clock_to_track(&[(0.0, 100.0)], 3.0), 103.0);
    }

    #[test]
    fn a_long_utterance_closes_at_a_sentence_end_or_past_twice_the_limit() {
        let words = |n: usize| vec!["parola"; n].join(" ");
        assert!(!should_close(&words(29)));
        assert!(!should_close(&words(30)), "no sentence end yet");
        assert!(should_close(&format!("{}.", words(30))));
        assert!(should_close(&format!("{}?", words(30))));
        assert!(
            !should_close(&format!("{}.", words(10))),
            "a short sentence waits for its pause"
        );
        assert!(should_close(&words(60)), "someone who never stops");
    }

    #[test]
    fn a_track_without_diarizer_gets_one_lane_under_its_label() {
        let lane = track_lane("mic", &[(1.0, 1.5), (1.6, 2.0), (5.0, 6.0)]).expect("a lane");
        assert_eq!(lane.id, "mic");
        assert_eq!(lane.name, "mic");
        assert_eq!(lane.band, "mic");
        assert_eq!(lane.segments, vec![(1.0, 2.0), (5.0, 6.0)]);
        assert!((lane.talk_secs - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_silent_track_gets_no_lane() {
        assert!(track_lane("mic", &[]).is_none());
    }

    #[test]
    fn speaker_list_uses_the_transcript_names_and_joins_close_words() {
        let segs = [(2, 1.0, 1.2), (2, 1.3, 1.5), (3, 4.0, 4.4), (2, 9.0, 9.5)];
        let list = speaker_list("system", &segs);
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].id.as_str(), list[0].name.as_str()),
            ("S2", "Speaker 2")
        );
        assert_eq!(list[0].segments, vec![(1.0, 1.5), (9.0, 9.5)]);
        assert!((list[0].talk_secs - 1.0).abs() < 1e-9);
        assert_eq!(list[1].band, "system");
        assert!(speaker_list("system", &[]).is_empty());
    }

    fn typed(t: &mut Transcript, text: &str, is_final: bool) -> (usize, String) {
        let r = t.apply(&up(text, is_final));
        (r.backspace, r.delta)
    }

    #[test]
    fn deltas_only_ever_append_whole_words() {
        let mut t = Transcript::default();
        assert_eq!(typed(&mut t, "Mi dici", false), (0, "Mi".into()));
        assert_eq!(
            typed(&mut t, "Mi dici che cosa", false),
            (0, "dici che".into())
        );
        assert_eq!(
            typed(&mut t, "Mi dici che cosa c'è.", true),
            (0, "cosa c'è.".into())
        );
        assert_eq!(typed(&mut t, "Prova", false), (0, String::new()));
        assert_eq!(t.text(), "Mi dici che cosa c'è.");
    }

    #[test]
    fn a_word_still_forming_is_never_typed_in_pieces() {
        // The 2026-10-30 report: "informa" + "zioni" typed as "informa zioni".
        let mut t = Transcript::default();
        assert_eq!(typed(&mut t, "informa", false), (0, String::new()));
        assert_eq!(typed(&mut t, "informazioni", false), (0, String::new()));
        assert_eq!(
            typed(&mut t, "informazioni sul", false),
            (0, "informazioni".into())
        );
        assert_eq!(
            typed(&mut t, "informazioni sul file.", true),
            (0, "sul file.".into())
        );
        assert_eq!(t.text(), "informazioni sul file.");
    }

    #[test]
    fn a_revised_word_is_deleted_and_retyped() {
        // The 2026-10-30 report: a word typed wrong and then corrected
        // stayed wrong on screen.
        let mut t = Transcript::default();
        assert_eq!(
            typed(&mut t, "chiavi PI in", false),
            (0, "chiavi PI".into())
        );
        // "PI " (3 characters with the space the host added) goes, and the
        // settled words come back in its place.
        assert_eq!(
            typed(&mut t, "chiavi API in un", false),
            (3, "API in".into())
        );
        assert_eq!(
            typed(&mut t, "chiavi API in un file.", true),
            (0, "un file.".into())
        );
        assert_eq!(t.text(), "chiavi API in un file.");
    }

    #[test]
    fn a_closed_sentence_is_never_rewritten() {
        let mut t = Transcript::default();
        assert_eq!(
            typed(&mut t, "Ciao a tutti.", true),
            (0, "Ciao a tutti.".into())
        );
        // After a final the engine sends only the next utterance; nothing it
        // says from here on may delete the closed one.
        assert_eq!(typed(&mut t, "Come", false), (0, String::new()));
        assert_eq!(typed(&mut t, "Come va", false), (0, "Come".into()));
        assert_eq!(typed(&mut t, "Cosa va?", true), (5, "Cosa va?".into()));
        assert_eq!(t.text(), "Ciao a tutti. Cosa va?");
    }

    #[test]
    fn a_correction_too_far_back_leaves_the_screen_alone() {
        let mut t = Transcript::default();
        let long = "uno due tre quattro cinque sei sette otto nove dieci undici";
        typed(&mut t, long, false);
        let changed = "UNO due tre quattro cinque sei sette otto nove dieci undici dodici";
        // Ten words back is past the limit: nothing deleted, nothing typed
        // until the utterance closes.
        assert_eq!(typed(&mut t, changed, false), (0, String::new()));
        let (bs, delta) = typed(&mut t, &format!("{changed} tredici."), true);
        assert_eq!(bs, 0);
        assert_eq!(delta, "undici dodici tredici.");
    }

    #[test]
    fn backspace_counts_characters_not_bytes() {
        let mut t = Transcript::default();
        typed(&mut t, "perché così no", false);
        // "così " is five characters and six bytes.
        assert_eq!(
            typed(&mut t, "perché cosa no ma", false),
            (5, "cosa no".into())
        );
    }

    #[test]
    fn empty_finals_add_nothing() {
        let mut t = Transcript::default();
        assert_eq!(typed(&mut t, "ciao", true), (0, "ciao".into()));
        assert_eq!(typed(&mut t, "", true), (0, String::new()));
        assert_eq!(t.text(), "ciao");
    }

    #[test]
    fn auto_and_empty_leave_language_to_the_model() {
        assert_eq!(locale_for(""), None);
        assert_eq!(locale_for("auto"), None);
        assert_eq!(locale_for(" AUTO "), None);
        assert_eq!(locale_for("it"), Some("it"));
        assert_eq!(locale_for("it-IT"), Some("it-IT"));
    }

    #[test]
    fn dictionary_words_become_one_context() {
        let r = Request::new("", &["WSL".into(), " ".into(), "MacinCloud".into()], false).unwrap();
        let c = r.context.as_ref().unwrap();
        assert_eq!(c.phrase_count, 2);
        assert!(c.boost > 0.0 && c.boost <= 10.0);
        assert!(Request::new("", &[], false).unwrap().context.is_none());
    }

    #[test]
    fn struct_sizes_match_asr_h_on_64_bit() {
        // asr.h v1 on LP64/LLP64: tolerating a smaller `size` is the
        // library's job, but a LARGER one with a wrong layout would be read
        // as garbage. These pin our side of the ABI.
        assert_eq!(std::mem::size_of::<BackendConfig>(), 16);
        assert_eq!(std::mem::size_of::<ModelConfig>(), 24);
        assert_eq!(std::mem::size_of::<DiarConfig>(), 40);
        assert_eq!(std::mem::size_of::<RecognizerConfig>(), 88);
        assert_eq!(std::mem::size_of::<EndpointingConfig>(), 16);
        assert_eq!(std::mem::size_of::<SpeechContext>(), 32);
        assert_eq!(std::mem::size_of::<RecognitionOptions>(), 72);
        // Offsets printed by MSVC from the pinned asr.h (2026-10-30).
        use std::mem::offset_of;
        assert_eq!(offset_of!(RecognitionOptions, language_code), 16);
        assert_eq!(offset_of!(RecognitionOptions, interim_results), 24);
        assert_eq!(offset_of!(RecognitionOptions, stop_history_eou_ms), 32);
        assert_eq!(offset_of!(RecognitionOptions, speech_contexts), 40);
        assert_eq!(offset_of!(RecognitionOptions, max_alternatives), 56);
        assert_eq!(
            offset_of!(RecognitionOptions, enable_speaker_diarization),
            60
        );
        assert_eq!(offset_of!(RecognitionOptions, max_speaker_count), 64);
    }
}
