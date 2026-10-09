//! One queue for every on-device model download.
//!
//! Each Settings page used to own its downloads: leave the page and the
//! progress was gone, come back and the model looked absent until it landed.
//! The queue lives here instead, so any page can enqueue, every page sees the
//! same state through the `model_download` event, and a page opened mid-way
//! reads where things stand once from the snapshot.
//!
//! Job ids are the ones the On-device rows already carry: a whisper `.bin`,
//! an LLM `.gguf`, `parakeet:fp32`, `whistle`, or `qwen:<file>.gguf`. One worker thread
//! runs one job at a time — several multi-gigabyte downloads in parallel only
//! make each of them slower — and exits when the queue is empty.
//!
//! A failed attempt is retried after a backoff; `download::download_resumable`
//! picks up from the `.part`, so a dropped connection costs a pause, not the
//! bytes already on disk. Cancelling drops the running future and leaves the
//! `.part` in place for the next try.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

pub const EVENT: &str = "model_download";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Queued,
    Downloading,
    Done,
    Failed,
    Cancelled,
    /// Removed from disk, `.part` included: the row offers a fresh download.
    Deleted,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Downloading => "downloading",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::Deleted => "deleted",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, State::Queued | State::Downloading)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Enqueued {
    New,
    AlreadyActive,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Deleted {
    Removed,
    NothingOnDisk,
    /// Queued or downloading: cancel first, so a delete never races a write.
    Busy,
    Failed(String),
}

/// Set by `cancel`, observed by the running fetch both synchronously and as
/// an awaitable, so an in-flight HTTP read is dropped rather than finished.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Cancel {
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    fn fire(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

pub type Fetch =
    dyn Fn(&str, &(dyn Fn(u64, u64) + Sync), &Cancel) -> Result<(), String> + Send + Sync;
pub type Emit = dyn Fn(&str) + Send + Sync;
/// Ok(true) when something was on disk and is gone now.
pub type Remove = dyn Fn(&str) -> Result<bool, String> + Send + Sync;

struct Job {
    state: State,
    done: u64,
    total: u64,
    error: Option<String>,
    cancel: Arc<Cancel>,
}

impl Job {
    fn queued() -> Self {
        Job {
            state: State::Queued,
            done: 0,
            total: 0,
            error: None,
            cancel: Arc::new(Cancel::default()),
        }
    }
}

#[derive(Default)]
struct Inner {
    order: VecDeque<String>,
    jobs: HashMap<String, Job>,
    worker_running: bool,
}

pub struct Center {
    inner: Mutex<Inner>,
    /// Taken before `inner` is released and held across the host callback,
    /// so events leave in the order the state changed without the callback
    /// running under `inner` (a host may read the snapshot from inside it).
    emit_order: Mutex<()>,
    wake: Condvar,
    fetch: Box<Fetch>,
    remove: Box<Remove>,
    emit: Box<Emit>,
    /// One entry per retry; its length is the retry count.
    backoff: Vec<Duration>,
}

const ERROR_MAX_CHARS: usize = 200;

impl Center {
    pub fn new(
        fetch: Box<Fetch>,
        remove: Box<Remove>,
        emit: Box<Emit>,
        backoff: Vec<Duration>,
    ) -> Arc<Self> {
        Arc::new(Center {
            inner: Mutex::new(Inner::default()),
            emit_order: Mutex::new(()),
            wake: Condvar::new(),
            fetch,
            remove,
            emit,
            backoff,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn enqueue(self: &Arc<Self>, id: &str) -> Enqueued {
        assert!(!id.is_empty(), "download id must not be empty");
        let mut inner = self.lock();
        if inner.jobs.get(id).is_some_and(|j| j.state.is_active()) {
            return Enqueued::AlreadyActive;
        }
        inner.jobs.insert(id.to_string(), Job::queued());
        inner.order.push_back(id.to_string());
        if !inner.worker_running {
            inner.worker_running = true;
            let me = Arc::clone(self);
            std::thread::Builder::new()
                .name("model-download".into())
                .spawn(move || me.worker())
                .expect("spawn model-download worker");
        }
        assert!(inner.jobs.contains_key(id));
        self.publish(inner, id);
        Enqueued::New
    }

    /// false when there is nothing queued or running under `id`.
    pub fn cancel(&self, id: &str) -> bool {
        assert!(!id.is_empty(), "download id must not be empty");
        let mut inner = self.lock();
        let Some(job) = inner.jobs.get_mut(id) else {
            return false;
        };
        match job.state {
            State::Queued => {
                job.state = State::Cancelled;
                inner.order.retain(|q| q != id);
                self.publish(inner, id);
                true
            }
            State::Downloading => {
                // The worker reports the terminal state once the fetch returns.
                job.cancel.fire();
                self.wake.notify_all();
                true
            }
            _ => false,
        }
    }

    /// Remove the model and any partial download from disk. Blocks while a
    /// loaded copy is dropped from memory (an inference in progress finishes
    /// first), so hosts call it off the UI thread.
    pub fn delete(&self, id: &str) -> Deleted {
        assert!(!id.is_empty(), "download id must not be empty");
        if self
            .lock()
            .jobs
            .get(id)
            .is_some_and(|j| j.state.is_active())
        {
            return Deleted::Busy;
        }
        // Not under `inner`: the remove waits on model caches. A download
        // enqueued meanwhile is serialised by the per-model slot both take.
        let removed = match (self.remove)(id) {
            Ok(removed) => removed,
            Err(e) => return Deleted::Failed(e.chars().take(ERROR_MAX_CHARS).collect()),
        };
        let mut inner = self.lock();
        if !inner.jobs.get(id).is_some_and(|j| j.state.is_active()) {
            let mut job = Job::queued();
            job.state = State::Deleted;
            inner.jobs.insert(id.to_string(), job);
            self.publish(inner, id);
        }
        if removed {
            Deleted::Removed
        } else {
            Deleted::NothingOnDisk
        }
    }

    pub fn snapshot_json(&self) -> String {
        let inner = self.lock();
        let mut ids: Vec<&String> = inner.jobs.keys().collect();
        ids.sort();
        let jobs: Vec<serde_json::Value> = ids.iter().map(|id| job_json(id, &inner)).collect();
        serde_json::Value::Array(jobs).to_string()
    }

    fn publish(&self, inner: std::sync::MutexGuard<'_, Inner>, id: &str) {
        let payload = job_json(id, &inner).to_string();
        let _order = self.emit_order.lock().unwrap_or_else(|e| e.into_inner());
        drop(inner);
        (self.emit)(&payload);
    }

    fn worker(self: Arc<Self>) {
        loop {
            let id = {
                let mut inner = self.lock();
                match inner.order.pop_front() {
                    Some(id) => id,
                    None => {
                        inner.worker_running = false;
                        return;
                    }
                }
            };
            self.run_job(&id);
        }
    }

    fn run_job(&self, id: &str) {
        let cancel = {
            let mut inner = self.lock();
            let job = inner.jobs.get_mut(id).expect("queued id has a job");
            assert!(job.state == State::Queued, "only a queued job starts");
            job.state = State::Downloading;
            let cancel = Arc::clone(&job.cancel);
            self.publish(inner, id);
            cancel
        };

        let last_bucket = std::sync::atomic::AtomicU64::new(u64::MAX);
        let progress = |done: u64, total: u64| {
            // At most one event per percent (per MiB when the size is
            // unknown): a multi-GB file otherwise floods the host UI thread.
            let bucket = if total > 0 {
                done.min(total) * 100 / total
            } else {
                done >> 20
            };
            let mut inner = self.lock();
            if let Some(job) = inner.jobs.get_mut(id) {
                job.done = done;
                job.total = total;
            }
            if last_bucket.swap(bucket, Ordering::Relaxed) != bucket {
                self.publish(inner, id);
            }
        };

        let mut outcome: Result<(), String> = Err(String::new());
        for attempt in 0..=self.backoff.len() {
            if cancel.is_cancelled() {
                break;
            }
            outcome = (self.fetch)(id, &progress, &cancel);
            if outcome.is_ok() || cancel.is_cancelled() {
                break;
            }
            if let Some(wait) = self.backoff.get(attempt) {
                let inner = self.lock();
                let _unused = self
                    .wake
                    .wait_timeout_while(inner, *wait, |_| !cancel.is_cancelled())
                    .unwrap_or_else(|e| e.into_inner());
            }
        }

        let mut inner = self.lock();
        let job = inner.jobs.get_mut(id).expect("running id has a job");
        assert!(job.state == State::Downloading);
        match outcome {
            _ if cancel.is_cancelled() => job.state = State::Cancelled,
            Ok(()) => {
                job.state = State::Done;
                job.done = job.done.max(job.total);
            }
            Err(e) => {
                job.state = State::Failed;
                job.error = Some(e.chars().take(ERROR_MAX_CHARS).collect());
            }
        }
        assert!(!job.state.is_active(), "a finished job is terminal");
        self.publish(inner, id);
    }
}

fn job_json(id: &str, inner: &Inner) -> serde_json::Value {
    let job = &inner.jobs[id];
    let mut v = serde_json::json!({
        "id": id,
        "state": job.state.as_str(),
        "done": job.done,
        "total": job.total,
    });
    if let Some(e) = &job.error {
        v["error"] = serde_json::Value::String(e.clone());
    }
    v
}

// ── The real downloads ──────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Kind<'a> {
    Whisper(&'a str),
    Llm(&'a str),
    Parakeet,
    Qwen(&'a str),
    Whistle,
}

pub const PARAKEET_ID: &str = "parakeet:fp32";
pub const WHISTLE_ID: &str = "whistle";

/// Only ids of models Dimmy ships: the id becomes a path under the model
/// directory, so anything outside the catalogs is refused.
fn parse(id: &str) -> Option<Kind<'_>> {
    if id == PARAKEET_ID {
        return Some(Kind::Parakeet);
    }
    if id == WHISTLE_ID {
        return crate::whistle::engine_available().then_some(Kind::Whistle);
    }
    if let Some(file) = id.strip_prefix("qwen:") {
        return crate::qwen_asr::find(file).map(|_| Kind::Qwen(file));
    }
    if crate::local_llm::AVAILABLE_LLM_MODELS
        .iter()
        .any(|m| m.filename == id)
    {
        return Some(Kind::Llm(id));
    }
    if crate::local_stt::AVAILABLE_MODELS
        .iter()
        .any(|m| m.filename == id)
    {
        return Some(Kind::Whisper(id));
    }
    None
}

pub fn is_known_id(id: &str) -> bool {
    parse(id).is_some()
}

/// Qwen on the Neural Engine is stored by FluidAudio, in folders it names
/// itself; rather than guess at them, Dimmy leaves those to the user.
pub fn is_deletable_id(id: &str) -> bool {
    match parse(id) {
        Some(Kind::Qwen(f)) => crate::qwen_asr::find(f)
            .is_some_and(|m| m.runtime != crate::qwen_asr::Runtime::NeuralEngine),
        Some(_) => true,
        None => false,
    }
}

fn present(kind: Kind<'_>) -> bool {
    match kind {
        Kind::Whisper(f) => crate::local_stt::model_exists(f),
        Kind::Llm(f) => crate::local_llm::model_exists(f),
        Kind::Parakeet => crate::parakeet::active_bundle_present(),
        Kind::Qwen(f) => crate::qwen_asr::bundle_present(f),
        Kind::Whistle => crate::whistle::bundle_present(),
    }
}

fn real_fetch(
    id: &str,
    progress: &(dyn Fn(u64, u64) + Sync),
    cancel: &Cancel,
) -> Result<(), String> {
    let kind = parse(id).expect("enqueue only accepts known ids");
    // Same slot the per-page download buttons take, so the two can't append
    // to one `.part` from different offsets.
    let slot = crate::ffi::model_download_slot(id);
    let _busy = slot.lock().unwrap_or_else(|e| e.into_inner());
    if present(kind) {
        return Ok(());
    }

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let (telemetry_kind, result) = rt.block_on(async {
        let download = async {
            match kind {
                Kind::Whisper(f) => (
                    "whisper",
                    crate::local_stt::download_model(f, progress)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                ),
                Kind::Llm(f) => (
                    "llm",
                    crate::local_llm::download_model(f, progress)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                ),
                Kind::Parakeet => (
                    "parakeet",
                    crate::parakeet::download_active_bundle(progress)
                        .await
                        .map_err(|e| e.to_string()),
                ),
                Kind::Qwen(f) => (
                    "qwen-asr",
                    crate::qwen_asr::download_bundle(f, progress)
                        .await
                        .map_err(|e| e.to_string()),
                ),
                Kind::Whistle => (
                    "whistle",
                    crate::whistle::download_bundle(progress)
                        .await
                        .map_err(|e| e.to_string()),
                ),
            }
        };
        tokio::select! {
            r = download => r,
            _ = cancel.cancelled() => ("", Err("cancelled".to_string())),
        }
    });
    if !telemetry_kind.is_empty() {
        crate::telemetry::track(crate::telemetry::Event::ModelDownloadCompleted {
            kind: telemetry_kind,
            success: result.is_ok(),
        });
    }
    if let Err(e) = &result {
        crate::log(&format!(
            "[Download center] {} attempt failed: {}",
            id,
            e.chars().take(ERROR_MAX_CHARS).collect::<String>()
        ));
    }
    result
}

/// A file and the two a resumable download leaves beside it.
fn with_partials(file: std::path::PathBuf) -> [std::path::PathBuf; 3] {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert!(!name.is_empty(), "model path has a file name");
    let part = file.with_file_name(format!("{name}.part"));
    let etag = file.with_file_name(format!("{name}.part.etag"));
    [file, part, etag]
}

/// Ok(false) when nothing is there.
fn remove_path(path: &std::path::Path) -> Result<bool, String> {
    let result = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    };
    result.map(|()| true).map_err(|e| {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!("{name} is in use; restart Dimmy and try again")
        } else {
            format!("{name}: {e}")
        }
    })
}

fn real_remove(id: &str) -> Result<bool, String> {
    let kind = parse(id).expect("delete only accepts known ids");
    assert!(is_deletable_id(id), "delete only accepts deletable ids");
    let slot = crate::ffi::model_download_slot(id);
    let _busy = slot.lock().unwrap_or_else(|e| e.into_inner());

    // A loaded model holds its file open (llama.cpp maps it), and Windows
    // refuses to delete an open file: drop it from memory first.
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    match kind {
        Kind::Whisper(f) => {
            crate::local_stt::clear_model_cache();
            paths.extend(with_partials(crate::local_stt::model_path(f)));
            paths.extend(crate::coreml_encoder::artifacts(f));
        }
        Kind::Llm(f) => {
            crate::local_llm::clear_llm_cache();
            paths.extend(with_partials(crate::local_llm::model_path(f)));
        }
        Kind::Qwen(f) => {
            crate::qwen_asr::clear_model_cache();
            let m = crate::qwen_asr::find(f).expect("parsed id is in the catalog");
            for file in [m.model_file, m.mmproj_file] {
                paths.extend(with_partials(crate::qwen_asr::file_path(file)));
            }
        }
        // ONNX Runtime keeps its session for the process, so a loaded
        // Parakeet on Windows reports "in use" until Dimmy restarts.
        Kind::Parakeet => paths.extend(crate::parakeet::active_bundle_dir()),
        // Same for a loaded Whistle engine library.
        Kind::Whistle => {
            for file in crate::whistle::bundle_files() {
                paths.extend(with_partials(file));
            }
        }
    }

    let mut removed = false;
    for path in &paths {
        removed |= remove_path(path)?;
    }
    assert!(
        !present(kind),
        "{id} still present after its files were removed"
    );
    crate::log(&format!(
        "[Download center] deleted {id} (removed={removed})"
    ));
    Ok(removed)
}

pub fn global() -> &'static Arc<Center> {
    static CENTER: OnceLock<Arc<Center>> = OnceLock::new();
    CENTER.get_or_init(|| {
        Center::new(
            Box::new(real_fetch),
            Box::new(real_remove),
            Box::new(|payload| crate::ffi::emit_event(EVENT, payload)),
            vec![
                Duration::from_secs(2),
                Duration::from_secs(8),
                Duration::from_secs(30),
            ],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    type Events = Arc<Mutex<Vec<serde_json::Value>>>;

    fn center(fetch: Box<Fetch>, backoff: Vec<Duration>) -> (Arc<Center>, Events) {
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let c = Center::new(
            fetch,
            Box::new(|_| Ok(false)),
            Box::new(move |p| sink.lock().unwrap().push(serde_json::from_str(p).unwrap())),
            backoff,
        );
        (c, events)
    }

    fn states_of(events: &Events, id: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in events.lock().unwrap().iter().filter(|e| e["id"] == id) {
            let s = e["state"].as_str().unwrap().to_string();
            if out.last() != Some(&s) {
                out.push(s);
            }
        }
        out
    }

    fn wait_terminal(c: &Center, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let inner = c.lock();
                if inner.jobs.get(id).is_some_and(|j| !j.state.is_active()) {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "job {id} never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn runs_jobs_in_enqueue_order() {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&ran);
        let (c, events) = center(
            Box::new(move |id, _, _| {
                log.lock().unwrap().push(id.to_string());
                Ok(())
            }),
            vec![],
        );
        for id in ["a", "b", "c"] {
            assert_eq!(c.enqueue(id), Enqueued::New);
        }
        for id in ["a", "b", "c"] {
            wait_terminal(&c, id);
        }
        assert_eq!(*ran.lock().unwrap(), ["a", "b", "c"]);
        assert_eq!(states_of(&events, "b"), ["queued", "downloading", "done"]);
    }

    #[test]
    fn enqueuing_an_active_id_twice_is_one_job() {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate = Mutex::new(gate_rx);
        let calls = Arc::new(Mutex::new(0));
        let n = Arc::clone(&calls);
        let (c, _) = center(
            Box::new(move |_, _, _| {
                *n.lock().unwrap() += 1;
                gate.lock().unwrap().recv().ok();
                Ok(())
            }),
            vec![],
        );
        assert_eq!(c.enqueue("m.bin"), Enqueued::New);
        assert_eq!(c.enqueue("m.bin"), Enqueued::AlreadyActive);
        gate_tx.send(()).unwrap();
        wait_terminal(&c, "m.bin");
        assert_eq!(*calls.lock().unwrap(), 1);
        // Finished: a new enqueue is a new job again.
        assert_eq!(c.enqueue("m.bin"), Enqueued::New);
        gate_tx.send(()).unwrap();
        wait_terminal(&c, "m.bin");
    }

    #[test]
    fn retries_then_succeeds() {
        let attempts = Arc::new(Mutex::new(0));
        let n = Arc::clone(&attempts);
        let (c, events) = center(
            Box::new(move |_, _, _| {
                let mut n = n.lock().unwrap();
                *n += 1;
                if *n < 3 {
                    Err("connection reset".into())
                } else {
                    Ok(())
                }
            }),
            vec![Duration::ZERO; 3],
        );
        c.enqueue("m.gguf");
        wait_terminal(&c, "m.gguf");
        assert_eq!(*attempts.lock().unwrap(), 3);
        assert_eq!(
            states_of(&events, "m.gguf"),
            ["queued", "downloading", "done"]
        );
    }

    #[test]
    fn gives_up_after_the_last_retry_with_a_truncated_error() {
        let attempts = Arc::new(Mutex::new(0));
        let n = Arc::clone(&attempts);
        let (c, events) = center(
            Box::new(move |_, _, _| {
                *n.lock().unwrap() += 1;
                Err("x".repeat(500))
            }),
            vec![Duration::ZERO; 3],
        );
        c.enqueue("m.gguf");
        wait_terminal(&c, "m.gguf");
        assert_eq!(*attempts.lock().unwrap(), 4, "one try plus three retries");
        let last = events.lock().unwrap().last().unwrap().clone();
        assert_eq!(last["state"], "failed");
        assert_eq!(
            last["error"].as_str().unwrap().chars().count(),
            ERROR_MAX_CHARS
        );
    }

    #[test]
    fn cancel_a_queued_job_never_runs_it() {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate = Mutex::new(gate_rx);
        let ran = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&ran);
        let (c, events) = center(
            Box::new(move |id, _, _| {
                log.lock().unwrap().push(id.to_string());
                gate.lock().unwrap().recv().ok();
                Ok(())
            }),
            vec![],
        );
        c.enqueue("first");
        c.enqueue("second");
        assert!(c.cancel("second"));
        gate_tx.send(()).unwrap();
        wait_terminal(&c, "first");
        assert_eq!(*ran.lock().unwrap(), ["first"]);
        assert_eq!(states_of(&events, "second"), ["queued", "cancelled"]);
        assert!(!c.cancel("second"), "nothing left to cancel");
        assert!(!c.cancel("never-enqueued"));
    }

    #[test]
    fn cancel_a_running_job_stops_its_fetch() {
        let (c, events) = center(
            Box::new(|_, _, cancel| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(30)) => Ok(()),
                        _ = cancel.cancelled() => Err("cancelled".into()),
                    }
                })
            }),
            vec![Duration::from_secs(30); 3],
        );
        c.enqueue("big.bin");
        while states_of(&events, "big.bin").last().map(String::as_str) != Some("downloading") {
            std::thread::sleep(Duration::from_millis(2));
        }
        let t = Instant::now();
        assert!(c.cancel("big.bin"));
        wait_terminal(&c, "big.bin");
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "no retry after a cancel"
        );
        assert_eq!(
            states_of(&events, "big.bin"),
            ["queued", "downloading", "cancelled"]
        );
    }

    #[test]
    fn progress_is_emitted_once_per_percent() {
        let (c, events) = center(
            Box::new(|_, progress, _| {
                for done in 0..=10_000u64 {
                    progress(done, 10_000);
                }
                Ok(())
            }),
            vec![],
        );
        c.enqueue("m.bin");
        wait_terminal(&c, "m.bin");
        let downloading = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["state"] == "downloading")
            .count();
        // One for the transition, one per percent 0..=100.
        assert_eq!(downloading, 1 + 101);
    }

    #[test]
    fn event_and_snapshot_share_one_shape() {
        let (c, events) = center(
            Box::new(|_, progress, _| {
                progress(5, 10);
                Ok(())
            }),
            vec![],
        );
        c.enqueue("m.bin");
        wait_terminal(&c, "m.bin");
        let done = events.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            done,
            serde_json::json!({"id": "m.bin", "state": "done", "done": 10, "total": 10})
        );
        let snap: serde_json::Value = serde_json::from_str(&c.snapshot_json()).unwrap();
        assert_eq!(snap, serde_json::json!([done]));
    }

    #[test]
    fn worker_exits_when_idle_and_restarts_on_enqueue() {
        let (c, _) = center(Box::new(|_, _, _| Ok(())), vec![]);
        c.enqueue("a");
        wait_terminal(&c, "a");
        let deadline = Instant::now() + Duration::from_secs(5);
        while c.lock().worker_running {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        c.enqueue("b");
        wait_terminal(&c, "b");
    }

    #[test]
    fn only_catalog_ids_are_known() {
        assert!(is_known_id(PARAKEET_ID));
        assert!(is_known_id(crate::local_stt::AVAILABLE_MODELS[0].filename));
        assert!(is_known_id(
            crate::local_llm::AVAILABLE_LLM_MODELS[0].filename
        ));
        let qwen = format!("qwen:{}", crate::qwen_asr::AVAILABLE_MODELS[0].model_file);
        assert!(is_known_id(&qwen));
        assert!(!is_known_id("../../evil.bin"));
        assert!(!is_known_id("qwen:nope.gguf"));
        assert!(!is_known_id(""));
    }

    fn center_with_remove(remove: Box<Remove>) -> (Arc<Center>, Events) {
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let c = Center::new(
            Box::new(|_, _, _| Ok(())),
            remove,
            Box::new(move |p| sink.lock().unwrap().push(serde_json::from_str(p).unwrap())),
            vec![],
        );
        (c, events)
    }

    #[test]
    fn delete_removes_and_reports_deleted() {
        let (c, events) = center_with_remove(Box::new(|_| Ok(true)));
        assert_eq!(c.delete("m.bin"), Deleted::Removed);
        let last = events.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            last,
            serde_json::json!({"id": "m.bin", "state": "deleted", "done": 0, "total": 0})
        );
    }

    #[test]
    fn delete_with_nothing_on_disk_still_clears_a_stale_job() {
        let (c, events) = center_with_remove(Box::new(|_| Ok(false)));
        c.enqueue("m.bin");
        wait_terminal(&c, "m.bin");
        assert_eq!(c.delete("m.bin"), Deleted::NothingOnDisk);
        assert_eq!(states_of(&events, "m.bin").last().unwrap(), "deleted");
    }

    #[test]
    fn delete_refuses_a_download_in_progress() {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate = Mutex::new(gate_rx);
        let removes = Arc::new(Mutex::new(0));
        let n = Arc::clone(&removes);
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let c = Center::new(
            Box::new(move |_, _, _| {
                gate.lock().unwrap().recv().ok();
                Ok(())
            }),
            Box::new(move |_| {
                *n.lock().unwrap() += 1;
                Ok(true)
            }),
            Box::new(move |p| sink.lock().unwrap().push(serde_json::from_str(p).unwrap())),
            vec![],
        );
        c.enqueue("m.bin");
        assert_eq!(c.delete("m.bin"), Deleted::Busy);
        assert_eq!(*removes.lock().unwrap(), 0, "nothing touched the disk");
        gate_tx.send(()).unwrap();
        wait_terminal(&c, "m.bin");
        assert_eq!(states_of(&events, "m.bin").last().unwrap(), "done");
    }

    #[test]
    fn a_failed_delete_keeps_the_state_and_truncates_the_reason() {
        let (c, events) = center_with_remove(Box::new(|_| Err("y".repeat(500))));
        let Deleted::Failed(reason) = c.delete("m.bin") else {
            panic!("expected Failed");
        };
        assert_eq!(reason.chars().count(), ERROR_MAX_CHARS);
        assert!(
            events.lock().unwrap().is_empty(),
            "no state change to report"
        );
    }

    #[test]
    fn remove_path_takes_files_and_folders_and_skips_what_is_absent() {
        let root = std::env::temp_dir().join(format!("dimmy-dc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bundle")).unwrap();
        std::fs::write(root.join("bundle").join("a.onnx"), b"x").unwrap();
        std::fs::write(root.join("m.bin.part"), b"x").unwrap();

        assert_eq!(remove_path(&root.join("m.bin.part")), Ok(true));
        assert_eq!(remove_path(&root.join("bundle")), Ok(true));
        assert_eq!(remove_path(&root.join("never-there")), Ok(false));
        assert!(!root.join("bundle").exists() && !root.join("m.bin.part").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn with_partials_covers_the_resume_files() {
        let p = std::path::Path::new("models").join("m.bin");
        let names: Vec<String> = with_partials(p)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["m.bin", "m.bin.part", "m.bin.part.etag"]);
    }

    #[test]
    fn whistle_is_a_known_id_only_where_an_engine_exists() {
        let available = crate::whistle::engine_available();
        assert_eq!(is_known_id(WHISTLE_ID), available);
        assert_eq!(is_deletable_id(WHISTLE_ID), available);
    }

    #[test]
    fn neural_engine_qwen_is_not_deletable_here() {
        for m in crate::qwen_asr::AVAILABLE_MODELS {
            let id = format!("qwen:{}", m.model_file);
            let fluid = m.runtime == crate::qwen_asr::Runtime::NeuralEngine;
            assert_eq!(is_deletable_id(&id), !fluid, "{id}");
        }
        assert!(is_deletable_id(PARAKEET_ID));
        assert!(!is_deletable_id("../../evil.bin"));
    }
}
