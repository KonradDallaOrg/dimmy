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
pub const PHASE_WAIT_QR: &str = "wait_qr";
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

/// How long a login-code request may take before the user is told.
pub const LOGIN_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A worker whose receiver is gone (it exited or panicked) is not running,
/// even though its sender is still stored. Treating it as running left every
/// later login attempt failing until the app was restarted.
#[cfg_attr(not(feature = "telegram"), allow(dead_code))]
fn worker_running<T>(tx: Option<&tokio::sync::mpsc::UnboundedSender<T>>) -> bool {
    tx.is_some_and(|tx| !tx.is_closed())
}

/// `tg://login?token=<base64url>`, the URL a phone's Telegram app scans to
/// approve a QR login (core.telegram.org/api/qr-login).
#[cfg_attr(not(feature = "telegram"), allow(dead_code))]
fn qr_login_url(token: &[u8]) -> String {
    use base64::Engine as _;
    assert!(!token.is_empty(), "Telegram returned an empty login token");
    format!(
        "tg://login?token={}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
    )
}

/// The QR code for `text` as `(side, modules)`: `side * side` characters,
/// row-major, `'1'` dark. The hosts paint it, so both draw the same code.
#[cfg(feature = "telegram")]
fn qr_modules(text: &str) -> Option<(usize, String)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let side = code.width();
    let modules: String = code
        .to_colors()
        .iter()
        .map(|c| if *c == qrcode::Color::Dark { '1' } else { '0' })
        .collect();
    assert_eq!(modules.len(), side * side, "QR grid is not square");
    Some((side, modules))
}

#[cfg(feature = "telegram")]
pub use imp::{
    cancel_login, dismiss, list_pending_json, logout, mark_processed, process, reply, set_enabled,
    start_login, start_qr_login, status_json, submit_code, submit_password,
};

#[cfg(not(feature = "telegram"))]
mod stub {
    pub fn set_enabled(_enabled: bool) {}
    pub fn start_login(_phone: &str) -> i32 {
        -100
    }
    pub fn start_qr_login() -> i32 {
        -100
    }
    pub fn cancel_login() -> i32 {
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
    pub fn reply(_msg_id: i32, _text: &str) -> i32 {
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
    use grammers_client::message::{InputMessage, Message};
    use grammers_client::peer::User;
    use grammers_client::update::Update;
    use grammers_client::{tl, Client};
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
    const LOGIN_TIMEOUT_MSG: &str =
        "Telegram did not answer within 30 seconds. Check your connection and try again.";

    enum Cmd {
        StartLogin(String),
        StartQrLogin,
        CancelLogin,
        SubmitCode(String),
        SubmitPassword(String),
        Logout,
        Process(i32),
        Dismiss(i32),
        MarkProcessed(i32),
        Reply(i32, String),
        Shutdown,
    }

    /// How many handled messages stay replyable. The host answers after the
    /// recap, which is long after `MarkProcessed` dropped the message from
    /// `pending`, so a few are kept aside to reply to.
    const REPLYABLE_HISTORY: usize = 16;

    static TX: Mutex<Option<UnboundedSender<Cmd>>> = Mutex::new(None);

    /// Set when the worker leaves because its session is dead (logged out, or
    /// revoked by Telegram). The session file is removed once the worker's
    /// connection is gone, so the next login starts from a fresh key.
    static DISCARD_SESSION: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Update-stream errors that mean this session can never work again. With
    /// the old loop each one came back instantly, about 20 times a second, at
    /// Telegram's servers and into the log.
    const DEAD_SESSION_ERRORS: &[&str] = &[
        "AUTH_KEY_UNREGISTERED",
        "SESSION_REVOKED",
        "SESSION_EXPIRED",
        "USER_DEACTIVATED*",
    ];

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
    pub fn start_qr_login() -> i32 {
        ensure_started();
        if send(Cmd::StartQrLogin) {
            0
        } else {
            -1
        }
    }
    pub fn cancel_login() -> i32 {
        if send(Cmd::CancelLogin) {
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
    pub fn reply(msg_id: i32, text: &str) -> i32 {
        if text.trim().is_empty() {
            return -1;
        }
        if send(Cmd::Reply(msg_id, text.to_string())) {
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
            if worker_running(g.as_ref()) {
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
                // Dropping the runtime ends the connection task, which could
                // otherwise write the dead key back after the file is gone.
                drop(rt);
                if DISCARD_SESSION.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let _ = std::fs::remove_file(session_path());
                }
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

        fn set_dc_option(
            &self,
            dc_option: &DcOption,
        ) -> BoxFuture<'_, Result<(), FileSessionError>> {
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
        // Handled messages we can still reply to, oldest first.
        let mut replyable: Vec<(i32, Message)> = Vec::new();
        let mut login_token: Option<LoginToken> = None;
        let mut password_token: Option<PasswordToken> = None;
        // When to ask Telegram for the QR login state again: at the token's
        // expiry, or immediately once the phone approves it. `None` = no QR
        // login in progress.
        let mut qr_refresh: Option<tokio::time::Instant> = None;
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
            let qr_deadline = qr_refresh;
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        Cmd::Shutdown => break,
                        Cmd::StartLogin(phone) => {
                            qr_refresh = None;
                            // Its own task, so a panic inside grammers comes back
                            // as a JoinError instead of killing this worker.
                            let (c, hash) = (client.clone(), api_hash.clone());
                            let mut task = tokio::spawn(async move {
                                c.request_login_code(&phone, &hash).await
                            });
                            match tokio::time::timeout(LOGIN_REQUEST_TIMEOUT, &mut task).await {
                                Ok(Ok(Ok(tok))) => { login_token = Some(tok); set_state(PHASE_WAIT_CODE, "", pending.len()); }
                                Ok(Ok(Err(e))) => emit_error(&format!("login code request failed: {e}")),
                                Ok(Err(_)) => emit_error("Telegram sent an answer Dimmy cannot handle. Try logging in with the QR code instead."),
                                Err(_) => {
                                    task.abort();
                                    emit_error(LOGIN_TIMEOUT_MSG);
                                }
                            }
                        }
                        Cmd::StartQrLogin => {
                            login_token = None;
                            password_token = None;
                            qr_refresh = Some(tokio::time::Instant::now());
                        }
                        Cmd::CancelLogin => {
                            login_token = None;
                            password_token = None;
                            qr_refresh = None;
                            if me_id.is_none() {
                                set_state(PHASE_LOGGED_OUT, "", pending.len());
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
                            if let Ok(mut g) = PENDING_META.lock() { g.clear(); }
                            // The key is revoked now; staying on it spins the
                            // update stream. Leave, and the next login starts
                            // a worker with a fresh session.
                            let _ = std::fs::remove_file(session_path());
                            DISCARD_SESSION.store(true, std::sync::atomic::Ordering::SeqCst);
                            set_state(PHASE_LOGGED_OUT, "", 0);
                            break;
                        }
                        Cmd::Process(id) => {
                            if let Some(msg) = pending.get(&id).cloned() {
                                if !replyable.iter().any(|(mid, _)| *mid == id) {
                                    replyable.push((id, msg.clone()));
                                    if replyable.len() > REPLYABLE_HISTORY {
                                        replyable.remove(0);
                                    }
                                }
                                download_and_emit(&client, &msg).await;
                            } else {
                                emit_error("message no longer available");
                            }
                        }
                        Cmd::Reply(id, text) => {
                            let target = replyable.iter().find(|(mid, _)| *mid == id)
                                .map(|(_, m)| m.clone())
                                .or_else(|| pending.get(&id).cloned());
                            match target {
                                Some(msg) => {
                                    if let Err(e) = msg.reply(InputMessage::default().text(text)).await {
                                        crate::log(&format!("[Telegram] reply failed: {e}"));
                                    }
                                }
                                None => crate::log("[Telegram] reply: message no longer available"),
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
                _ = async { tokio::time::sleep_until(qr_deadline.unwrap_or_else(tokio::time::Instant::now)).await }, if qr_deadline.is_some() => {
                    match qr_export(&client, &session, api_id, &api_hash).await {
                        QrOutcome::Show(next) => {
                            qr_refresh = Some(next);
                            set_state(PHASE_WAIT_QR, "", pending.len());
                        }
                        QrOutcome::Password(pt) => {
                            qr_refresh = None;
                            password_token = Some(*pt);
                            set_state(PHASE_WAIT_PASSWORD, "", pending.len());
                        }
                        QrOutcome::Connected(user) => {
                            qr_refresh = None;
                            me_id = Some(user.id());
                            set_state(PHASE_CONNECTED, &display_name(&user), pending.len());
                        }
                        QrOutcome::Failed(msg) => {
                            qr_refresh = None;
                            emit_error(&msg);
                            set_state(PHASE_LOGGED_OUT, "", pending.len());
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
                        // The phone approved the QR code: ask for the result now
                        // rather than at the token's expiry.
                        Ok(Update::Raw(raw)) if matches!(raw.raw, tl::enums::Update::LoginToken) => {
                            crate::log("[Telegram] QR login approved on another device");
                            if qr_refresh.is_some() {
                                qr_refresh = Some(tokio::time::Instant::now());
                            }
                        }
                        Ok(_) => {}
                        Err(e) if DEAD_SESSION_ERRORS.iter().any(|name| e.is(name)) => {
                            crate::log(&format!("[Telegram] session is no longer valid: {e}"));
                            let _ = std::fs::remove_file(session_path());
                            DISCARD_SESSION.store(true, std::sync::atomic::Ordering::SeqCst);
                            if me_id.is_some() {
                                emit_error("Telegram signed this device out. Log in again.");
                            }
                            set_state(PHASE_LOGGED_OUT, "", 0);
                            break;
                        }
                        Err(e) => {
                            crate::log(&format!("[Telegram] update error: {e}"));
                            // A dropped connection fails every call at once too.
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    enum QrOutcome {
        /// A fresh code was sent to the host; look again at this instant.
        Show(tokio::time::Instant),
        Password(Box<PasswordToken>),
        Connected(Box<User>),
        Failed(String),
    }

    /// One round of `auth.exportLoginToken`: a new code to show, or the
    /// outcome once the phone has approved the previous one. Follows the
    /// documented flow, including the move to another data center.
    async fn qr_export(
        client: &Client,
        session: &FileSession,
        api_id: i32,
        api_hash: &str,
    ) -> QrOutcome {
        let request = tl::functions::auth::ExportLoginToken {
            api_id,
            api_hash: api_hash.to_string(),
            except_ids: Vec::new(),
        };
        let Ok(first) = tokio::time::timeout(LOGIN_REQUEST_TIMEOUT, client.invoke(&request)).await
        else {
            return QrOutcome::Failed(LOGIN_TIMEOUT_MSG.to_string());
        };
        let answer = match first {
            Ok(tl::enums::auth::LoginToken::MigrateTo(m)) => {
                if let Err(e) = session.set_home_dc_id(m.dc_id).await {
                    return QrOutcome::Failed(format!("QR login failed: {e}"));
                }
                client
                    .invoke(&tl::functions::auth::ImportLoginToken { token: m.token })
                    .await
            }
            other => other,
        };
        match answer {
            Ok(tl::enums::auth::LoginToken::Token(t)) => {
                let Some((size, modules)) = qr_modules(&qr_login_url(&t.token)) else {
                    return QrOutcome::Failed("Could not draw the QR code.".to_string());
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let secs = (t.expires as i64 - now).clamp(5, 60) as u64;
                crate::ffi::emit_event(
                    "telegram_qr",
                    &json!({ "size": size, "modules": modules, "expires_in": secs }).to_string(),
                );
                QrOutcome::Show(tokio::time::Instant::now() + std::time::Duration::from_secs(secs))
            }
            Ok(tl::enums::auth::LoginToken::Success(s)) => match s.authorization {
                tl::enums::auth::Authorization::Authorization(a) => {
                    match complete_qr_login(client, session, a).await {
                        Ok(user) => QrOutcome::Connected(Box::new(user)),
                        Err(e) => QrOutcome::Failed(format!("QR login failed: {e}")),
                    }
                }
                tl::enums::auth::Authorization::SignUpRequired(_) => QrOutcome::Failed(
                    "This number has no Telegram account yet. Create it in the Telegram app first."
                        .to_string(),
                ),
            },
            Ok(tl::enums::auth::LoginToken::MigrateTo(_)) => {
                QrOutcome::Failed("Telegram moved the login twice. Try again.".to_string())
            }
            Err(e) if e.is("SESSION_PASSWORD_NEEDED") => {
                match client.invoke(&tl::functions::account::GetPassword {}).await {
                    Ok(pw) => QrOutcome::Password(Box::new(PasswordToken::new(pw.into()))),
                    Err(e) => QrOutcome::Failed(format!("QR login failed: {e}")),
                }
            }
            Err(e) => QrOutcome::Failed(format!("QR login failed: {e}")),
        }
    }

    /// What grammers' private `complete_login` does after `sign_in`, for a
    /// login that came through a QR code instead: remember who we are and
    /// where the update stream starts.
    async fn complete_qr_login(
        client: &Client,
        session: &FileSession,
        auth: tl::types::auth::Authorization,
    ) -> Result<User, Box<dyn std::error::Error + Send + Sync>> {
        let update_state = client
            .invoke(&tl::functions::updates::GetState {})
            .await
            .ok();
        let user = User::from_raw(client, auth.user);
        let peer_auth = user.to_ref().await?.map(|r| r.auth);
        session
            .cache_peer(&PeerInfo::User {
                id: user.id().bare_id_unchecked(),
                auth: peer_auth,
                bot: Some(user.is_bot()),
                is_self: Some(true),
            })
            .await?;
        if let Some(tl::enums::updates::State::State(s)) = update_state {
            session
                .set_update_state(UpdateState::All(UpdatesState {
                    pts: s.pts,
                    qts: s.qts,
                    date: s.date,
                    seq: s.seq,
                    channels: Vec::new(),
                }))
                .await?;
        }
        Ok(user)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_whose_receiver_is_gone_is_not_running() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        assert!(worker_running(Some(&tx)));
        drop(rx);
        assert!(!worker_running(Some(&tx)));
        assert!(!worker_running::<()>(None));
    }

    #[test]
    fn the_qr_url_uses_base64url_without_padding() {
        // 0xfb 0xff is "+/8=" in standard base64: both substitutions and the
        // dropped padding show up in one token.
        assert_eq!(qr_login_url(&[0xfb, 0xff]), "tg://login?token=-_8");
    }

    #[cfg(feature = "telegram")]
    #[test]
    fn the_qr_grid_is_square_and_binary() {
        let (side, modules) = qr_modules(&qr_login_url(&[7u8; 32])).expect("encodes");
        assert!(side >= 21, "smallest QR version is 21 modules, got {side}");
        assert_eq!(modules.len(), side * side);
        assert!(modules.bytes().all(|b| b == b'0' || b == b'1'));
        assert!(modules.contains('1') && modules.contains('0'));
    }
}
