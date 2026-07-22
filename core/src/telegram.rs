//! Telegram user-account inbox source (cargo feature `telegram`).
//!
//! Lets a user record audio on their phone with any app, share it to their
//! Telegram **Saved Messages**, and have Dimmy pick it up and run the normal
//! file-load -> transcribe -> recap pipeline. Dimmy logs into the user's OWN
//! account via `grammers` (pure-Rust MTProto — no libtdjson, no OpenSSL), so
//! there is no 20 MB bot-download cap (user clients fetch up to 2 GB) and no
//! 24 h queue drop (Saved Messages persist in the cloud until processed).
//!
//! Design (CUPID): this module's job ENDS at "here is a local audio file
//! path". It never transcribes — it downloads and hands the host a path, and
//! the host runs its existing file-load pipeline (`dimmy_transcribe_file` +
//! optional recap). One thin inbox source, nothing more.
//!
//! Boundaries:
//! - OFF by default. Gated by the `telegram` cargo feature AND the
//!   `telegram_enabled` config AND the user explicitly connecting an account.
//! - Event-driven, no polling: the worker emits `telegram_state`,
//!   `telegram_pending` and `telegram_audio` via `crate::ffi::emit_event`.
//! - Read-only + download-only. It never sends messages, joins groups, or adds
//!   contacts — that keeps the account outside Telegram's anti-spam triggers
//!   (see `docs/dev/`). ToS 1.5 (AI/ML on Telegram data) is a product decision
//!   documented alongside this feature; all inference stays local + per-user.

use std::path::PathBuf;

/// Auth / connection phase reported to the host as `telegram_state.phase`.
pub const PHASE_DISABLED: &str = "disabled";
pub const PHASE_NO_CREDENTIALS: &str = "no_credentials";
pub const PHASE_LOGGED_OUT: &str = "logged_out";
pub const PHASE_WAIT_CODE: &str = "wait_code";
pub const PHASE_WAIT_PASSWORD: &str = "wait_password";
pub const PHASE_CONNECTED: &str = "connected";
pub const PHASE_ERROR: &str = "error";

/// Developer-registered app credentials from <https://my.telegram.org>.
/// Embedded at compile time via `DIMMY_TELEGRAM_API_ID`/`_HASH`, with a
/// runtime env override so a tester can supply them without a rebuild. The end
/// user never touches my.telegram.org — they only log in with phone + code.
pub fn api_credentials() -> Option<(i32, String)> {
    let id_raw = std::env::var("DIMMY_TELEGRAM_API_ID")
        .ok()
        .or_else(|| option_env!("DIMMY_TELEGRAM_API_ID").map(str::to_string))?;
    let hash = std::env::var("DIMMY_TELEGRAM_API_HASH")
        .ok()
        .or_else(|| option_env!("DIMMY_TELEGRAM_API_HASH").map(str::to_string))?;
    let id = id_raw.trim().parse::<i32>().ok()?;
    let hash = hash.trim().to_string();
    if id == 0 || hash.is_empty() {
        return None;
    }
    Some((id, hash))
}

/// `<config>/telegram/` — session file, processed-id set, downloaded inbox.
pub fn telegram_dir() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join("telegram"))
}

// ─────────────────────────────────────────────────────────────────────────
// Public API — called from ffi.rs. Feature-gated impl; stubs when off so the
// FFI surface (and thus the C ABI) is identical regardless of the feature.
// ─────────────────────────────────────────────────────────────────────────

/// Whether the crate was built with the `telegram` feature.
pub fn is_compiled() -> bool {
    cfg!(feature = "telegram")
}

#[cfg(feature = "telegram")]
pub use imp::{
    dismiss, list_pending_json, logout, mark_processed, process, set_enabled, start_login,
    status_json, submit_code, submit_password,
};

#[cfg(not(feature = "telegram"))]
mod stub {
    pub fn set_enabled(_enabled: bool) {}
    pub fn start_login(_phone: &str) -> i32 {
        -100
    }
    pub fn submit_code(_code: &str) -> i32 {
        -100
    }
    pub fn submit_password(_password: &str) -> i32 {
        -100
    }
    pub fn logout() -> i32 {
        -100
    }
    pub fn process(_msg_id: i32) -> i32 {
        -100
    }
    pub fn dismiss(_msg_id: i32) -> i32 {
        -100
    }
    pub fn mark_processed(_msg_id: i32) -> i32 {
        -100
    }
    pub fn list_pending_json() -> String {
        "[]".to_string()
    }
    pub fn status_json() -> String {
        format!(
            "{{\"compiled\":false,\"phase\":\"{}\"}}",
            super::PHASE_DISABLED
        )
    }
}
#[cfg(not(feature = "telegram"))]
pub use stub::*;

// ─────────────────────────────────────────────────────────────────────────
// Implementation
// ─────────────────────────────────────────────────────────────────────────

#[cfg(feature = "telegram")]
mod imp {
    use super::*;
    use grammers_client::client::{LoginToken, PasswordToken, SignInError, UpdatesConfiguration};
    use grammers_client::media::Media;
    use grammers_client::message::Message;
    use grammers_client::peer::User;
    use grammers_client::update::Update;
    use grammers_client::Client;
    use grammers_mtsender::SenderPool;
    use grammers_session::types::{
        ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState,
    };
    use grammers_session::{BoxFuture, Session, SessionData};
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

    /// Commands the FFI layer sends to the worker task.
    enum Cmd {
        StartLogin(String),
        SubmitCode(String),
        SubmitPassword(String),
        Logout,
        Process(i32),
        Dismiss(i32),
        MarkProcessed(i32),
        Shutdown,
    }

    static TX: Mutex<Option<UnboundedSender<Cmd>>> = Mutex::new(None);

    /// Snapshot of the last state we published, so `status_json()` (a sync FFI
    /// call) can answer without reaching into the async worker.
    static STATE: Mutex<Option<StateSnapshot>> = Mutex::new(None);

    #[derive(Clone, Default)]
    struct StateSnapshot {
        phase: String,
        account: String,
        pending: usize,
    }

    fn set_state(phase: &str, account: &str, pending: usize) {
        if let Ok(mut g) = STATE.lock() {
            *g = Some(StateSnapshot {
                phase: phase.to_string(),
                account: account.to_string(),
                pending,
            });
        }
        crate::ffi::emit_event(
            "telegram_state",
            &json!({ "phase": phase, "account": account, "pending": pending }).to_string(),
        );
    }

    fn send(cmd: Cmd) -> bool {
        if let Ok(g) = TX.lock() {
            if let Some(tx) = g.as_ref() {
                return tx.send(cmd).is_ok();
            }
        }
        false
    }

    pub fn set_enabled(enabled: bool) {
        if enabled {
            ensure_started();
        } else {
            let _ = send(Cmd::Shutdown);
        }
    }

    pub fn start_login(phone: &str) -> i32 {
        ensure_started();
        if send(Cmd::StartLogin(phone.to_string())) {
            0
        } else {
            -1
        }
    }
    pub fn submit_code(code: &str) -> i32 {
        if send(Cmd::SubmitCode(code.to_string())) {
            0
        } else {
            -1
        }
    }
    pub fn submit_password(password: &str) -> i32 {
        if send(Cmd::SubmitPassword(password.to_string())) {
            0
        } else {
            -1
        }
    }
    pub fn logout() -> i32 {
        if send(Cmd::Logout) {
            0
        } else {
            -1
        }
    }
    pub fn process(msg_id: i32) -> i32 {
        if send(Cmd::Process(msg_id)) {
            0
        } else {
            -1
        }
    }
    pub fn dismiss(msg_id: i32) -> i32 {
        if send(Cmd::Dismiss(msg_id)) {
            0
        } else {
            -1
        }
    }
    pub fn mark_processed(msg_id: i32) -> i32 {
        if send(Cmd::MarkProcessed(msg_id)) {
            0
        } else {
            -1
        }
    }

    pub fn status_json() -> String {
        let snap = STATE
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default();
        let phase = if snap.phase.is_empty() {
            PHASE_DISABLED.to_string()
        } else {
            snap.phase
        };
        json!({
            "compiled": true,
            "has_credentials": api_credentials().is_some(),
            "phase": phase,
            "account": snap.account,
            "pending": snap.pending,
        })
        .to_string()
    }

    pub fn list_pending_json() -> String {
        // The authoritative pending list lives in the worker; it is mirrored
        // into PENDING_META for this sync accessor.
        if let Ok(g) = PENDING_META.lock() {
            return serde_json::to_string(&*g).unwrap_or_else(|_| "[]".to_string());
        }
        "[]".to_string()
    }

    #[derive(Clone, serde::Serialize)]
    struct PendingMeta {
        msg_id: i32,
        filename: String,
        date: i64,
        size: i64,
    }
    static PENDING_META: Mutex<Vec<PendingMeta>> = Mutex::new(Vec::new());

    /// Spawn the worker thread + tokio runtime if not already running.
    fn ensure_started() {
        {
            let g = TX.lock().unwrap();
            if g.is_some() {
                return;
            }
        }
        let (api_id, api_hash) = match api_credentials() {
            Some(v) => v,
            None => {
                set_state(PHASE_NO_CREDENTIALS, "", 0);
                return;
            }
        };
        let (tx, rx) = unbounded_channel::<Cmd>();
        {
            let mut g = TX.lock().unwrap();
            *g = Some(tx);
        }
        std::thread::Builder::new()
            .name("dimmy-telegram".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        crate::log(&format!("[Telegram] runtime build failed: {e}"));
                        if let Ok(mut g) = TX.lock() {
                            *g = None;
                        }
                        return;
                    }
                };
                rt.block_on(async move {
                    if let Err(e) = run(rx, api_id, api_hash).await {
                        crate::log(&format!("[Telegram] worker exited: {e}"));
                        set_state(PHASE_ERROR, "", 0);
                    }
                });
                if let Ok(mut g) = TX.lock() {
                    *g = None;
                }
            })
            .ok();
    }

    fn session_path() -> PathBuf {
        telegram_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("session.json")
    }
    fn inbox_dir() -> PathBuf {
        telegram_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("inbox")
    }
    fn processed_path() -> PathBuf {
        telegram_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("processed.json")
    }

    fn load_processed() -> HashSet<i32> {
        std::fs::read_to_string(processed_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<i32>>(&s).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default()
    }
    fn save_processed(set: &HashSet<i32>) {
        if let Some(dir) = telegram_dir() {
            let _ = std::fs::create_dir_all(&dir);
        }
        let v: Vec<i32> = set.iter().copied().collect();
        if let Ok(s) = serde_json::to_string(&v) {
            let _ = std::fs::write(processed_path(), s);
        }
    }

    // ── File-backed session (JSON), replacing grammers' SqliteSession ──
    // SqliteSession uses libsql, whose global sqlite init PANICS when
    // Dimmy's history (rusqlite) has already initialized sqlite in the same
    // process (`SQLITE_CONFIG_SERIALIZED` assert in libsql). We persist the
    // session ourselves via serde so libsql is never opened. This carries the
    // auth key + home DC + self peer across launches, so the user logs in once.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct PersistedSession {
        home_dc: i32,
        dc_options: Vec<DcOption>,
        peer_infos: Vec<PeerInfo>,
        updates_state: UpdatesState,
    }

    impl PersistedSession {
        fn from_data(d: &SessionData) -> Self {
            Self {
                home_dc: d.home_dc,
                dc_options: d.dc_options.values().cloned().collect(),
                peer_infos: d.peer_infos.values().cloned().collect(),
                updates_state: d.updates_state.clone(),
            }
        }
        fn into_data(self) -> SessionData {
            SessionData {
                home_dc: self.home_dc,
                dc_options: self.dc_options.into_iter().map(|o| (o.id, o)).collect(),
                peer_infos: self.peer_infos.into_iter().map(|i| (i.id(), i)).collect(),
                updates_state: self.updates_state,
            }
        }
    }

    #[derive(Debug)]
    struct FileSessionError;
    impl std::error::Error for FileSessionError {}
    impl std::fmt::Display for FileSessionError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "telegram session lock poisoned")
        }
    }

    struct FileSession {
        data: Mutex<SessionData>,
        path: PathBuf,
    }

    impl FileSession {
        fn load(path: PathBuf) -> Self {
            let data = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<PersistedSession>(&b).ok())
                .map(PersistedSession::into_data)
                .unwrap_or_default();
            Self {
                data: Mutex::new(data),
                path,
            }
        }
        fn lock(&self) -> Result<std::sync::MutexGuard<'_, SessionData>, FileSessionError> {
            self.data.lock().map_err(|_| FileSessionError)
        }
        fn save(&self, d: &SessionData) {
            if let Ok(bytes) = serde_json::to_vec(&PersistedSession::from_data(d)) {
                let _ = std::fs::write(&self.path, bytes);
            }
        }
    }

    impl Session for FileSession {
        type Error = FileSessionError;

        fn home_dc_id(&self) -> Result<i32, FileSessionError> {
            Ok(self.lock()?.home_dc)
        }

        fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), FileSessionError>> {
            Box::pin(async move {
                let mut d = self.lock()?;
                d.home_dc = dc_id;
                self.save(&d);
                Ok(())
            })
        }

        fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, FileSessionError> {
            Ok(self.lock()?.dc_options.get(&dc_id).cloned())
        }

        fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), FileSessionError>> {
            let dc_option = dc_option.clone();
            Box::pin(async move {
                let mut d = self.lock()?;
                d.dc_options.insert(dc_option.id, dc_option);
                self.save(&d);
                Ok(())
            })
        }

        fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, FileSessionError>> {
            Box::pin(async move { Ok(self.lock()?.peer_infos.get(&peer).cloned()) })
        }

        fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), FileSessionError>> {
            let peer = peer.clone();
            Box::pin(async move {
                let mut d = self.lock()?;
                d.peer_infos
                    .entry(peer.id())
                    .or_insert_with(|| peer.clone())
                    .extend_info(&peer);
                self.save(&d);
                Ok(())
            })
        }

        fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, FileSessionError>> {
            Box::pin(async move { Ok(self.lock()?.updates_state.clone()) })
        }

        fn set_update_state(
            &self,
            update: UpdateState,
        ) -> BoxFuture<'_, Result<(), FileSessionError>> {
            Box::pin(async move {
                let mut d = self.lock()?;
                match update {
                    UpdateState::All(updates_state) => {
                        d.updates_state = updates_state;
                    }
                    UpdateState::Primary { pts, date, seq } => {
                        d.updates_state.pts = pts;
                        d.updates_state.date = date;
                        d.updates_state.seq = seq;
                    }
                    UpdateState::Secondary { qts } => {
                        d.updates_state.qts = qts;
                    }
                    UpdateState::Channel { id, pts } => {
                        d.updates_state.channels.retain(|c| c.id != id);
                        d.updates_state.channels.push(ChannelState { id, pts });
                    }
                }
                self.save(&d);
                Ok(())
            })
        }
    }

    /// The worker: owns the grammers client + all mutable session state.
    async fn run(
        mut rx: UnboundedReceiver<Cmd>,
        api_id: i32,
        api_hash: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(dir) = telegram_dir() {
            let _ = std::fs::create_dir_all(&dir);
        }
        let _ = std::fs::create_dir_all(inbox_dir());

        let session = Arc::new(FileSession::load(session_path()));
        let SenderPool {
            runner,
            updates,
            handle,
        } = SenderPool::new(Arc::clone(&session), api_id);
        let client = Client::new(handle.clone());
        // The pool task drives the MTProto connection; it must outlive the loop.
        let _pool = tokio::spawn(runner.run());
        // Start the update stream immediately — pre-auth it yields nothing, and
        // `catch_up` replays anything missed while offline once authorized (this
        // is what realises "PC was off -> surface on next launch").
        let mut stream = client
            .stream_updates(
                updates,
                UpdatesConfiguration {
                    catch_up: true,
                    ..Default::default()
                },
            )
            .await?;

        let mut processed = load_processed();
        let mut pending: HashMap<i32, Message> = HashMap::new();
        let mut login_token: Option<LoginToken> = None;
        let mut password_token: Option<PasswordToken> = None;
        // The peer id of the logged-in account; a Saved-Messages message is one
        // whose peer is ourselves. `None` until authorized. Type is grammers'
        // private `PeerId`, so it is held by inference rather than named.
        let mut me_id: Option<_> = None;

        if client.is_authorized().await.unwrap_or(false) {
            if let Ok(me) = client.get_me().await {
                me_id = Some(me.id());
                set_state(PHASE_CONNECTED, &display_name(&me), pending.len());
            }
        } else {
            set_state(PHASE_LOGGED_OUT, "", 0);
        }

        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        Cmd::Shutdown => break,
                        Cmd::StartLogin(phone) => {
                            match client.request_login_code(&phone, &api_hash).await {
                                Ok(tok) => { login_token = Some(tok); set_state(PHASE_WAIT_CODE, "", pending.len()); }
                                Err(e) => emit_error(&format!("login code request failed: {e}")),
                            }
                        }
                        Cmd::SubmitCode(code) => {
                            let Some(tok) = login_token.as_ref() else {
                                emit_error("no login in progress"); continue;
                            };
                            match client.sign_in(tok, code.trim()).await {
                                Ok(user) => {
                                    login_token = None;
                                    me_id = Some(user.id());
                                    set_state(PHASE_CONNECTED, &display_name(&user), pending.len());
                                }
                                Err(SignInError::PasswordRequired(pt)) => {
                                    password_token = Some(pt);
                                    set_state(PHASE_WAIT_PASSWORD, "", pending.len());
                                }
                                Err(SignInError::InvalidCode) => emit_error("invalid code"),
                                Err(e) => emit_error(&format!("sign-in failed: {e}")),
                            }
                        }
                        Cmd::SubmitPassword(pw) => {
                            let Some(pt) = password_token.take() else {
                                emit_error("no 2FA password expected"); continue;
                            };
                            match client.check_password(pt, pw.trim()).await {
                                Ok(user) => {
                                    me_id = Some(user.id());
                                    set_state(PHASE_CONNECTED, &display_name(&user), pending.len());
                                }
                                Err(SignInError::InvalidPassword(_)) => emit_error("invalid password"),
                                Err(e) => emit_error(&format!("2FA failed: {e}")),
                            }
                        }
                        Cmd::Logout => {
                            let _ = client.sign_out().await;
                            me_id = None; pending.clear();
                            if let Ok(mut g) = PENDING_META.lock() { g.clear(); }
                            set_state(PHASE_LOGGED_OUT, "", 0);
                        }
                        Cmd::Process(id) => {
                            if let Some(msg) = pending.get(&id).cloned() {
                                download_and_emit(&client, &msg).await;
                            } else {
                                emit_error("message no longer available");
                            }
                        }
                        Cmd::Dismiss(id) => {
                            pending.remove(&id);
                            processed.insert(id);
                            save_processed(&processed);
                            refresh_pending_meta(&pending);
                            set_state(PHASE_CONNECTED, "", pending.len());
                        }
                        Cmd::MarkProcessed(id) => {
                            pending.remove(&id);
                            processed.insert(id);
                            save_processed(&processed);
                            refresh_pending_meta(&pending);
                        }
                    }
                }
                upd = stream.next() => {
                    match upd {
                        Ok(Update::NewMessage(msg)) => {
                            if me_id == Some(msg.peer_id()) {
                                handle_incoming(&msg, &processed, &mut pending);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => crate::log(&format!("[Telegram] update error: {e}")),
                    }
                }
            }
        }
        Ok(())
    }

    fn display_name(user: &User) -> String {
        user.username().map(|u| u.to_string()).unwrap_or_else(|| {
            format!(
                "{} {}",
                user.first_name().unwrap_or(""),
                user.last_name().unwrap_or("")
            )
            .trim()
            .to_string()
        })
    }

    /// One incoming Saved-Messages message: if it carries audio and hasn't been
    /// handled, add it to pending and tell the host (ask, or auto-process).
    fn handle_incoming(
        msg: &Message,
        processed: &HashSet<i32>,
        pending: &mut HashMap<i32, Message>,
    ) {
        let id = msg.id();
        if processed.contains(&id) || pending.contains_key(&id) {
            return;
        }
        if !is_audio(msg) {
            return;
        }
        pending.insert(id, msg.clone());
        refresh_pending_meta(pending);
        let (filename, size) = audio_meta(msg);
        crate::ffi::emit_event(
            "telegram_pending",
            &json!({ "msg_id": id, "filename": filename, "date": msg.date().timestamp(), "size": size })
                .to_string(),
        );
    }

    // Backlog on connect is handled by `stream_updates(catch_up: true)`, which
    // replays messages received while the client was offline as ordinary
    // `Update::NewMessage`. An explicit `iter_messages` history scan (for audio
    // shared BEFORE the account was ever connected, which catch-up can't cover)
    // is a v2 follow-up once the 0.10 PeerRef API for the self-chat is settled.

    /// Download the message's audio to the inbox and hand the host a path.
    async fn download_and_emit(client: &Client, msg: &Message) {
        let Some(media) = msg.media() else {
            emit_error("message has no media");
            return;
        };
        let id = msg.id();
        let (filename, _size) = audio_meta(msg);
        let ext = ext_for(&filename, &media);
        let dest = inbox_dir().join(format!("{id}.{ext}"));
        match client.download_media(&media, &dest).await {
            Ok(_) => {
                crate::ffi::emit_event(
                    "telegram_audio",
                    &json!({
                        "msg_id": id,
                        "path": dest.to_string_lossy(),
                        "filename": filename,
                        "date": msg.date().timestamp(),
                    })
                    .to_string(),
                );
            }
            Err(e) => emit_error(&format!("download failed: {e}")),
        }
    }

    fn refresh_pending_meta(pending: &HashMap<i32, Message>) {
        let mut v: Vec<PendingMeta> = pending
            .iter()
            .map(|(id, msg)| {
                let (filename, size) = audio_meta(msg);
                PendingMeta {
                    msg_id: *id,
                    filename,
                    date: msg.date().timestamp(),
                    size,
                }
            })
            .collect();
        v.sort_by_key(|p| p.date);
        if let Ok(mut g) = PENDING_META.lock() {
            *g = v;
        }
    }

    /// Is this message an audio/voice document we should ingest?
    fn is_audio(msg: &Message) -> bool {
        match msg.media() {
            Some(Media::Document(doc)) => {
                let mime = doc.mime_type().unwrap_or("");
                if mime.starts_with("audio") {
                    return true;
                }
                let name = doc.name().unwrap_or("").to_ascii_lowercase();
                [
                    ".ogg", ".oga", ".opus", ".m4a", ".mp3", ".wav", ".aac", ".flac", ".webm",
                ]
                .iter()
                .any(|e| name.ends_with(e))
            }
            _ => false,
        }
    }

    fn audio_meta(msg: &Message) -> (String, i64) {
        if let Some(Media::Document(doc)) = msg.media() {
            let name = doc.name().unwrap_or("");
            let filename = if name.is_empty() {
                format!("voice-{}.ogg", msg.id())
            } else {
                name.to_string()
            };
            return (filename, doc.size().unwrap_or(0) as i64);
        }
        (format!("audio-{}.bin", msg.id()), 0)
    }

    fn ext_for(filename: &str, media: &Media) -> String {
        if let Some(dot) = filename.rfind('.') {
            let e = &filename[dot + 1..];
            if !e.is_empty() && e.len() <= 5 {
                return e.to_ascii_lowercase();
            }
        }
        if let Media::Document(doc) = media {
            let mime = doc.mime_type().unwrap_or("");
            return match mime {
                "audio/ogg" | "audio/opus" => "ogg",
                "audio/mpeg" => "mp3",
                "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
                "audio/wav" | "audio/x-wav" => "wav",
                "audio/aac" => "aac",
                "audio/flac" => "flac",
                _ => "ogg",
            }
            .to_string();
        }
        "ogg".to_string()
    }

    fn emit_error(msg: &str) {
        crate::log(&format!("[Telegram] {msg}"));
        crate::ffi::emit_event("telegram_error", &json!({ "message": msg }).to_string());
    }
}
