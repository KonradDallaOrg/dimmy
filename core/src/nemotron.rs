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
struct RecognizerConfig {
    size: usize,
    backend: *const BackendConfig,
    model: *const ModelConfig,
    streaming: *const std::ffi::c_void,
    decoder: *const std::ffi::c_void,
    vad: *const std::ffi::c_void,
    endpointing: *const std::ffi::c_void,
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
    stream_finish: unsafe extern "C" fn(*mut StreamHandle) -> Status,
    stream_next: unsafe extern "C" fn(*mut StreamHandle, *mut *mut ResultHandle) -> Status,
    stream_close: unsafe extern "C" fn(*mut StreamHandle),
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
            stream_finish: sym!("nemo_speech_asr_stream_finish"),
            stream_next: sym!("nemo_speech_asr_stream_next"),
            stream_close: sym!("nemo_speech_asr_stream_close"),
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
        let cfg = RecognizerConfig {
            size: std::mem::size_of::<RecognizerConfig>(),
            backend: &backend,
            model: &model,
            streaming: std::ptr::null(),
            decoder: std::ptr::null(),
            vad: std::ptr::null(),
            endpointing: std::ptr::null(),
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
            pushed: 0,
            finished: false,
        })
    }
}

impl Drop for Recognizer {
    fn drop(&mut self) {
        // Deliberately not destroyed: `nemo_speech_asr_destroy` tears down
        // worker threads that may still be finishing a stream another thread
        // just closed, and a recognizer lives for the whole process anyway.
        // The handle and the library are reclaimed at exit.
    }
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
    pushed: usize,
    finished: bool,
}

// SAFETY: a stream is driven by one thread at a time (asr.h: "single-threaded
// by contract"); moving it to the thread that drives it is fine, sharing is not.
unsafe impl Send for Stream {}

impl Stream {
    /// Buffer 16 kHz mono audio. Decoding happens in `poll`.
    pub fn push(&mut self, pcm_16k: &[f32]) -> Result<(), TranscribeError> {
        assert!(!self.finished, "push after finish");
        if pcm_16k.is_empty() {
            return Ok(());
        }
        assert!(
            pcm_16k.iter().all(|s| s.is_finite()),
            "nemotron: samples must be finite"
        );
        let api = &self.recognizer.api;
        // SAFETY: the slice outlives the call; the stream copies it.
        let rc = unsafe {
            (api.stream_push_f32)(
                self.handle,
                pcm_16k.as_ptr(),
                pcm_16k.len(),
                SAMPLE_RATE as i32,
            )
        };
        api.check("push", rc)?;
        self.pushed += pcm_16k.len();
        Ok(())
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
        self.pushed as f64 / SAMPLE_RATE as f64
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: closed exactly once, here.
        unsafe { (self.recognizer.api.stream_close)(self.handle) };
    }
}

// ---- Process-wide recognizer -------------------------------------------

static RECOGNIZER: Mutex<Option<Arc<Recognizer>>> = Mutex::new(None);

/// The recognizer for `spec`, loaded on first use and kept: loading the
/// model costs seconds and a dictation must not pay it twice. A different
/// spec (model variant, device, diarizer) replaces it.
pub fn recognizer(spec: &EngineSpec) -> Result<Arc<Recognizer>, TranscribeError> {
    let mut slot = RECOGNIZER.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(r) = slot.as_ref().filter(|r| r.spec() == spec) {
        return Ok(Arc::clone(r));
    }
    let r = Arc::new(Recognizer::create(spec.clone())?);
    warm_up(&r)?;
    *slot = Some(Arc::clone(&r));
    Ok(r)
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

// ---- Turning results into text the app can use --------------------------

/// Keeps the committed text of a stream and turns each result into the
/// delta the host types at the cursor. Append-only: a word, once typed,
/// is never taken back, so the delta is only ever text past what was sent.
#[derive(Debug, Default)]
pub struct Transcript {
    finals: Vec<String>,
    sent: String,
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

    /// Apply one update; returns (delta, cumulative). The delta extends the
    /// text already sent only where the new text still starts with it: a
    /// partial the engine revised is held back until it settles again, so
    /// nothing typed has to be erased.
    pub fn apply(&mut self, u: &Update) -> (String, String) {
        let cumulative = self.cumulative(&u.text);
        if u.is_final && !u.text.is_empty() {
            self.finals.push(u.text.clone());
        }
        let target = if u.is_final {
            self.cumulative("")
        } else {
            cumulative.clone()
        };
        let delta = if let Some(rest) = target.strip_prefix(self.sent.as_str()) {
            let d = rest.to_string();
            self.sent = target;
            d
        } else {
            String::new()
        };
        (delta, cumulative)
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

    #[test]
    fn deltas_only_ever_append() {
        let mut t = Transcript::default();
        assert_eq!(t.apply(&up("Mi dici", false)).0, "Mi dici");
        assert_eq!(t.apply(&up("Mi dici che cosa", false)).0, " che cosa");
        assert_eq!(t.apply(&up("Mi dici che cosa c'è.", true)).0, " c'è.");
        assert_eq!(t.apply(&up("Prova", false)).0, " Prova");
        assert_eq!(t.text(), "Mi dici che cosa c'è.");
    }

    #[test]
    fn a_revised_partial_is_held_back_not_erased() {
        let mut t = Transcript::default();
        assert_eq!(t.apply(&up("chiavi PI", false)).0, "chiavi PI");
        // The engine changes its mind: nothing is typed until the text
        // again extends what is already on screen.
        assert_eq!(t.apply(&up("chiavi API", false)).0, "");
        let (delta, cumulative) = t.apply(&up("chiavi API in un file", true));
        assert_eq!(delta, "");
        assert_eq!(cumulative, "chiavi API in un file");
        assert_eq!(t.text(), "chiavi API in un file");
    }

    #[test]
    fn empty_finals_add_nothing() {
        let mut t = Transcript::default();
        t.apply(&up("ciao", true));
        assert_eq!(t.apply(&up("", true)).0, "");
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
