//! Whistle (Cactus Compute) -- the fourth local STT backend.
//!
//! A 16.9 MB speech model that runs on the CPU alone: no GPU, no ONNX
//! Runtime, no llama.cpp. Measured 2026-10-09 on a real 62 s Italian
//! dictation (laptop CPU, 4 threads): about 5 s, 12x realtime, with text
//! close to whisper large-v3-turbo's. English, German, French, Spanish,
//! Italian, Dutch and Polish only.
//!
//! It runs in-process, through the engine's C API (`needle.h`). Cactus does
//! not publish the engine's source, only binaries, and the static library
//! for Windows is an llvm-mingw build (libc++, Itanium ABI) that does not
//! link into our MSVC DLL. Their shared library has a plain C surface and
//! depends on nothing but the C runtime, so it loads anywhere: it is fetched
//! with the model and opened with `libloading`, one code path for Windows,
//! macOS (both architectures) and Linux.
//!
//! The engine is code downloaded at run time, so it is pinned twice: to a
//! repository revision, and to a SHA-256 compiled in here. A file that
//! changed upstream fails the download instead of being loaded.

use crate::error::TranscribeError;
use std::ffi::{c_char, c_int, CStr, CString};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;

pub const MODEL_FILE: &str = "whistle.cact";
const MODEL_URL: &str = "https://huggingface.co/Cactus-Compute/whistle/resolve/b358ddadd89b7a713b5aa131f23032d3cca1b251/whistle.cact";
const MODEL_SHA256: &str = "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb";
const MODEL_BYTES: u64 = 16_919_407;
const MODEL_MAGIC: &[u8] = &[0x84, 0x2a, 0xe1, 0x05];

/// The engine ships inside Cactus's Python wheel, which is a zip: that is
/// the only place the shared library is published.
struct EngineWheel {
    /// Wheel platform tag, e.g. `win_amd64`.
    tag: &'static str,
    sha256: &'static str,
    bytes: u64,
    /// The library's path inside the wheel.
    member: &'static str,
    /// What it is called in the model directory.
    file: &'static str,
}

const ENGINE_VERSION: &str = "3.2.0";
const ENGINE_REPO: &str = "https://huggingface.co/Cactus-Compute/needle3/resolve/2ae11323dc000f5e70c49f7403efa6af12ba9e67/python";
const WHEEL_MAGIC: &[u8] = b"PK\x03\x04";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const ENGINE: Option<EngineWheel> = Some(EngineWheel {
    tag: "win_amd64",
    sha256: "0bd41dc812a4b7e504f6ba294de65a363a51b031f154d24aac748b07249dc952",
    bytes: 696_277,
    member: "needle/libneedle3.dll",
    file: "whistle-needle.dll",
});
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENGINE: Option<EngineWheel> = Some(EngineWheel {
    tag: "macosx_11_0_arm64",
    sha256: "3b0887a43cd6e9a99009fabf35b231c11bb3a978ab8af94b8119b2eb76e19832",
    bytes: 535_559,
    member: "needle/libneedle3.dylib",
    file: "whistle-needle.dylib",
});
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const ENGINE: Option<EngineWheel> = Some(EngineWheel {
    tag: "macosx_11_0_x86_64",
    sha256: "d2a60d538b138ecc5ac5a9b4d47ba8d078131d0af9e20e94cceb9329894d0f96",
    bytes: 588_391,
    member: "needle/libneedle3.dylib",
    file: "whistle-needle.dylib",
});
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const ENGINE: Option<EngineWheel> = Some(EngineWheel {
    tag: "manylinux2014_x86_64",
    sha256: "0e8a3bce4e52968ee14e8e9d47098e65ed06dd5b7c18cef3a7fa1375756736f9",
    bytes: 684_813,
    member: "needle/libneedle3.so",
    file: "whistle-needle.so",
});
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const ENGINE: Option<EngineWheel> = Some(EngineWheel {
    tag: "manylinux2014_aarch64",
    sha256: "80871585c84a277eff24b43a9366958996be38f6f920c9d1f8a0d2a07f3ae51f",
    bytes: 661_328,
    member: "needle/libneedle3.so",
    file: "whistle-needle.so",
});
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ),
    all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ),
)))]
const ENGINE: Option<EngineWheel> = None;

/// The languages the model was trained on. Anything else is left to its own
/// detection, which is the best it can do with a language it does not know.
pub const LANGUAGES: &[&str] = &["en", "de", "fr", "es", "it", "nl", "pl"];

const SAMPLE_RATE: usize = 16_000;

/// The engine refuses more than 30 s, but accuracy goes first: the same
/// Italian dictation that is clean at 20 and 25 s turns "aggiornati" into
/// "è giornata" at 28 s and "cerchi" into "Cherkey" at 30. Cut well short.
const MAX_CHUNK: usize = 24 * SAMPLE_RATE;
/// Earliest point a cut may land, so a pause is looked for over 8 s.
const MIN_CHUNK: usize = 16 * SAMPLE_RATE;
/// A cut never leaves less than this for the last chunk: a fraction of a
/// second on its own transcribes as nothing, or as a wrong word.
const MIN_TAIL: usize = 2 * SAMPLE_RATE;
const FRAME: usize = SAMPLE_RATE / 5;

/// Whether Cactus ships an engine for the platform this build targets.
pub fn engine_available() -> bool {
    ENGINE.is_some()
}

pub fn size_mb() -> u32 {
    let bytes = MODEL_BYTES + ENGINE.as_ref().map_or(0, |e| e.bytes);
    bytes.div_ceil(1024 * 1024) as u32
}

fn model_path() -> PathBuf {
    crate::local_stt::model_path(MODEL_FILE)
}

fn engine_path(engine: &EngineWheel) -> PathBuf {
    crate::local_stt::model_path(engine.file)
}

fn wheel_path() -> PathBuf {
    crate::local_stt::model_path("whistle-needle.whl")
}

/// The files a finished download leaves, for the download center to delete.
/// The engine library stays locked for as long as it is loaded.
pub fn bundle_files() -> Vec<PathBuf> {
    let mut files = vec![model_path(), wheel_path()];
    files.extend(ENGINE.as_ref().map(engine_path));
    files
}

/// True only when the model AND this platform's engine are on disk.
pub fn bundle_present() -> bool {
    ENGINE
        .as_ref()
        .is_some_and(|e| engine_path(e).is_file() && model_path().is_file())
}

/// Pull the engine library out of the wheel, through a `.part` so a crash
/// mid-write never leaves a truncated library under the final name.
fn extract_engine(engine: &EngineWheel) -> Result<(), String> {
    let wheel = std::fs::File::open(wheel_path()).map_err(|e| format!("open wheel: {e}"))?;
    let mut zip = zip::ZipArchive::new(wheel).map_err(|e| format!("read wheel: {e}"))?;
    let mut member = zip
        .by_name(engine.member)
        .map_err(|e| format!("{} in wheel: {e}", engine.member))?;
    let dest = engine_path(engine);
    let part = dest.with_extension("part");
    let mut out = std::fs::File::create(&part).map_err(|e| format!("create engine: {e}"))?;
    std::io::copy(&mut member, &mut out).map_err(|e| format!("extract engine: {e}"))?;
    drop(out);
    std::fs::rename(&part, &dest).map_err(|e| format!("install engine: {e}"))
}

/// Fetch the model and the engine, resumable, each checked against the
/// SHA-256 pinned above. Progress covers the pair.
pub async fn download_bundle<F>(on_progress: F) -> Result<(), TranscribeError>
where
    F: Fn(u64, u64) + Sync,
{
    let engine = ENGINE.as_ref().ok_or_else(|| {
        TranscribeError::LocalModel("Whistle has no engine for this platform".into())
    })?;
    let dir = crate::local_stt::model_directory();
    std::fs::create_dir_all(&dir)
        .map_err(|e| TranscribeError::LocalModel(format!("create {}: {}", dir.display(), e)))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| TranscribeError::LocalModel(format!("HTTP client: {}", e)))?;

    let total = MODEL_BYTES + engine.bytes;
    let wheel_url = format!(
        "{}/cactus_needle-{}-py3-none-{}.whl",
        ENGINE_REPO, ENGINE_VERSION, engine.tag
    );
    let mut base: u64 = 0;
    for (url, dest, magic, sha256, bytes, wanted) in [
        (
            MODEL_URL,
            model_path(),
            MODEL_MAGIC,
            MODEL_SHA256,
            MODEL_BYTES,
            !model_path().is_file(),
        ),
        (
            wheel_url.as_str(),
            wheel_path(),
            WHEEL_MAGIC,
            engine.sha256,
            engine.bytes,
            !engine_path(engine).is_file(),
        ),
    ] {
        if wanted {
            crate::log(&format!("[Whistle] Downloading {} ...", url));
            crate::download::download_resumable(&client, url, &dest, &[magic], |done, _| {
                on_progress(base + done, total)
            })
            .await
            .map_err(TranscribeError::LocalModel)?;
            if let Err(e) = crate::download::verify_file(&dest, &[magic], Some(sha256)) {
                let _ = std::fs::remove_file(&dest);
                return Err(TranscribeError::LocalModel(format!(
                    "Whistle download failed its pinned check: {}",
                    e
                )));
            }
        }
        base += bytes;
        on_progress(base, total);
    }

    if !engine_path(engine).is_file() {
        extract_engine(engine).map_err(TranscribeError::LocalModel)?;
    }
    let _ = std::fs::remove_file(wheel_path());

    assert!(
        bundle_present(),
        "model and engine must exist after a successful download"
    );
    Ok(())
}

/// Where to cut the audio so no chunk exceeds `MAX_CHUNK`, each cut in the
/// quietest 200 ms of the window it is allowed to land in.
fn chunk_ranges(pcm: &[f32]) -> Vec<Range<usize>> {
    assert!(!pcm.is_empty(), "chunk_ranges: pcm must not be empty");
    let energy = |at: usize| pcm[at..at + FRAME].iter().map(|s| s * s).sum::<f32>();
    let mut out = Vec::new();
    let mut start = 0;
    while pcm.len() - start > MAX_CHUNK {
        let lo = start + MIN_CHUNK;
        let hi = (start + MAX_CHUNK).min(pcm.len() - MIN_TAIL);
        let quietest = (lo..=hi - FRAME)
            .step_by(FRAME)
            .min_by(|a, b| energy(*a).total_cmp(&energy(*b)))
            .expect("the cut window is wider than one frame");
        let cut = quietest + FRAME / 2;
        out.push(start..cut);
        start = cut;
    }
    out.push(start..pcm.len());

    assert_eq!(out.last().map(|r| r.end), Some(pcm.len()));
    assert!(out.iter().all(|r| !r.is_empty() && r.len() <= MAX_CHUNK));
    out
}

/// The transcript out of the engine's JSON answer.
fn parse_output(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    Some(v["text"].as_str()?.trim().to_string())
}

type TranscribeFn = unsafe extern "C" fn(
    pcm: *const f32,
    samples: c_int,
    language: *const c_char,
    keywords: *const c_char,
    word_timestamps: c_int,
    out: *mut c_char,
    out_capacity: c_int,
) -> c_int;
type LastErrorFn = unsafe extern "C" fn() -> *const c_char;

/// The loaded engine. `needle.h`: "one process-global, non-thread-safe
/// model", hence the single mutex every call goes through.
struct Engine {
    transcribe: TranscribeFn,
    last_error: LastErrorFn,
    // Never unloaded: the engine runs its own worker threads, and freeing
    // the library under them would crash the process. The function pointers
    // above are only valid while this is alive.
    _library: libloading::Library,
}

static ENGINE_STATE: Mutex<Option<Engine>> = Mutex::new(None);

impl Engine {
    fn load() -> Result<Self, TranscribeError> {
        let err = |what: &str, e: &dyn std::fmt::Display| {
            TranscribeError::LocalModel(format!("Whistle {}: {}", what, e))
        };
        let engine = ENGINE
            .as_ref()
            .ok_or_else(|| err("engine", &"not available on this platform"))?;
        let model = std::fs::read(model_path()).map_err(|e| err("model", &e))?;
        assert!(
            model.starts_with(MODEL_MAGIC),
            "whistle.cact was verified at download and must still be a model"
        );

        // SAFETY: the library is the pinned, checksummed engine, and the
        // signatures below are the ones declared in its `needle.h`.
        unsafe {
            let library =
                libloading::Library::new(engine_path(engine)).map_err(|e| err("engine", &e))?;
            let load = *library
                .get::<unsafe extern "C" fn(*const u8, std::ffi::c_ulonglong) -> c_int>(
                    b"needle_load\0",
                )
                .map_err(|e| err("engine", &e))?;
            let transcribe = *library
                .get::<TranscribeFn>(b"needle_transcribe\0")
                .map_err(|e| err("engine", &e))?;
            let last_error = *library
                .get::<LastErrorFn>(b"needle_last_error\0")
                .map_err(|e| err("engine", &e))?;

            let rc = load(model.as_ptr(), model.len() as std::ffi::c_ulonglong);
            let loaded = Self {
                transcribe,
                last_error,
                _library: library,
            };
            if rc < 0 {
                let reason = loaded.last_error_text();
                // Leak rather than unload, for the reason on `_library`.
                std::mem::forget(loaded);
                return Err(err("model load", &reason));
            }
            // `needle.h` does not say whether the engine keeps pointing into
            // the buffer it was handed, so the buffer lives as long as it does.
            std::mem::forget(model);
            Ok(loaded)
        }
    }

    fn last_error_text(&self) -> String {
        // SAFETY: returns NULL or a NUL-terminated string the engine owns
        // until the next call; it is copied out before any other call.
        let raw = unsafe {
            let p = (self.last_error)();
            if p.is_null() {
                return "unknown error".into();
            }
            CStr::from_ptr(p).to_string_lossy().into_owned()
        };
        raw.chars().take(200).collect()
    }

    fn transcribe(
        &self,
        pcm: &[f32],
        language: Option<&CStr>,
        keywords: Option<&CStr>,
    ) -> Result<String, TranscribeError> {
        assert!(
            !pcm.is_empty() && pcm.len() <= MAX_CHUNK,
            "whistle: one call takes 1..={} samples, got {}",
            MAX_CHUNK,
            pcm.len()
        );
        let mut out = vec![0u8; 16 * 1024];
        // SAFETY: `pcm` and `out` are valid for the lengths passed, the
        // strings are NUL-terminated or NULL, and the mutex held by the
        // caller makes this the only call into the engine.
        let rc = unsafe {
            (self.transcribe)(
                pcm.as_ptr(),
                pcm.len() as c_int,
                language.map_or(std::ptr::null(), CStr::as_ptr),
                keywords.map_or(std::ptr::null(), CStr::as_ptr),
                0,
                out.as_mut_ptr().cast(),
                out.len() as c_int,
            )
        };
        if rc < 0 {
            return Err(TranscribeError::LocalModel(format!(
                "Whistle: {}",
                self.last_error_text()
            )));
        }
        let json = CStr::from_bytes_until_nul(&out)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        parse_output(&json)
            .ok_or_else(|| TranscribeError::LocalModel("Whistle returned no transcript".into()))
    }
}

/// Transcribe 16 kHz mono PCM of any length. `language` is an ISO code, or
/// anything else (empty, "auto", a language the model lacks) to detect it.
/// `keywords` biases the search toward the user's own terms. Silence gives
/// an empty string, not an error.
pub fn transcribe(
    pcm_16k: &[f32],
    language: &str,
    keywords: &[String],
) -> Result<String, TranscribeError> {
    assert!(!pcm_16k.is_empty(), "whistle: pcm must not be empty");
    assert!(
        pcm_16k.iter().all(|s| s.is_finite()),
        "whistle: all samples must be finite"
    );
    if !bundle_present() {
        return Err(TranscribeError::LocalModel(
            "Whistle is not downloaded".into(),
        ));
    }
    let language = LANGUAGES
        .iter()
        .find(|l| **l == language)
        .map(|l| CString::new(*l).expect("language codes have no NUL"));
    let keywords: Vec<&str> = keywords
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty() && !k.contains(['\n', '\r', '\0']))
        .collect();
    let keywords = (!keywords.is_empty())
        .then(|| CString::new(keywords.join("\n")).expect("NUL was filtered out"));

    let mut state = ENGINE_STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.is_none() {
        let started = std::time::Instant::now();
        *state = Some(Engine::load()?);
        crate::log(&format!(
            "[Whistle] engine {} loaded in {} ms",
            ENGINE_VERSION,
            started.elapsed().as_millis()
        ));
    }
    let engine = state.as_ref().expect("loaded above");

    let mut text = String::new();
    for range in chunk_ranges(pcm_16k) {
        let piece = engine.transcribe(&pcm_16k[range], language.as_deref(), keywords.as_deref())?;
        if !piece.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&piece);
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f32) -> usize {
        (s * SAMPLE_RATE as f32) as usize
    }

    #[test]
    fn short_audio_is_one_chunk() {
        let pcm = vec![0.1f32; secs(24.0)];
        assert_eq!(chunk_ranges(&pcm), vec![0..pcm.len()]);
    }

    #[test]
    fn long_audio_is_covered_without_gaps_or_overlap() {
        let pcm = vec![0.1f32; secs(125.3)];
        let ranges = chunk_ranges(&pcm);
        assert!(ranges.len() >= 6);
        assert_eq!(ranges[0].start, 0);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        assert_eq!(ranges.last().unwrap().end, pcm.len());
    }

    #[test]
    fn the_cut_lands_in_the_pause() {
        let mut pcm = vec![0.5f32; secs(40.0)];
        for s in &mut pcm[secs(19.0)..secs(19.6)] {
            *s = 0.0;
        }
        let cut = chunk_ranges(&pcm)[0].end;
        assert!((secs(19.0)..secs(19.6)).contains(&cut), "cut at {}", cut);
    }

    #[test]
    fn no_chunk_is_a_sliver() {
        // Just over one chunk: a cut at the limit would leave 50 ms behind.
        let pcm = vec![0.1f32; secs(24.05)];
        let ranges = chunk_ranges(&pcm);
        assert_eq!(ranges.len(), 2);
        assert!(ranges[1].len() >= MIN_TAIL);
    }

    #[test]
    fn output_is_the_text_field() {
        let out = r#"{"text":" ciao a tutti ","language":"it","ttft_ms":1.0,"decode_tps":2.0}"#;
        assert_eq!(parse_output(out).as_deref(), Some("ciao a tutti"));
        // Silence is an empty transcript, not a failure.
        assert_eq!(
            parse_output(r#"{"text":"","language":""}"#).as_deref(),
            Some("")
        );
        assert_eq!(parse_output(""), None);
    }

    #[test]
    fn everything_downloaded_is_pinned() {
        assert!(crate::download::is_sha256(MODEL_SHA256));
        assert!(!MODEL_URL.contains("/resolve/main/"), "pin a revision");
        assert!(!ENGINE_REPO.contains("/resolve/main/"), "pin a revision");
        if let Some(e) = ENGINE.as_ref() {
            assert!(crate::download::is_sha256(e.sha256), "{}", e.tag);
        }
    }

    /// End to end against the real engine. Needs the bundle on disk, so it
    /// is skipped on a machine (and on CI) that has not downloaded Whistle.
    #[test]
    fn transcribes_the_jfk_fixture_when_the_bundle_is_present() {
        if !bundle_present() {
            return;
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/jfk_16k_mono.wav"
        );
        let pcm: Vec<f32> = hound::WavReader::open(path)
            .unwrap()
            .samples::<i16>()
            .map(|s| f32::from(s.unwrap()) / 32768.0)
            .collect();
        let text = transcribe(&pcm, "en", &[]).unwrap().to_lowercase();
        assert!(text.contains("ask not what your country"), "{}", text);
        assert_eq!(transcribe(&vec![0.0; secs(2.0)], "", &[]).unwrap(), "");
    }
}
