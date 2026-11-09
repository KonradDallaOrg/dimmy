//! Kimi Code CLI integration: use the user's Kimi membership (Plus and up)
//! for LLM calls (dictation rewrite, command mode, meeting recap) through
//! the official `kimi` binary, the way `claude_code.rs` and `codex.rs` use
//! theirs. Dimmy never reads Kimi's credentials.
//!
//! Why ACP and not `kimi -p`
//! -------------------------
//! Verified against the published `@moonshot-ai/kimi-code` 2.5.2 bundle
//! (2026-11-09). In that version `-p` takes the prompt from argv only (no
//! stdin), and a recap carries a whole transcript: argv has OS length limits
//! (8191 chars through an npm `.cmd` shim) and leaks via `ps`. `kimi acp`
//! speaks the Agent Client Protocol over stdio (ndjson JSON-RPC), so the
//! prompt goes on stdin at any length, and it gives us what `-p` doesn't:
//! - the models THIS account's tier serves (`configOptions.model`), so a
//!   picked model the tier lacks is dropped instead of failing the call
//!   (the Codex `HTTP 1` trap, see `docs/dev/codex-backend.md`);
//! - a `thinking` option we turn off for quick rewrites;
//! - `session/delete`, so dictations do not pile up in the user's Kimi
//!   history (the equivalent of Claude's `--no-session-persistence`).
//!
//! Safety
//! ------
//! Kimi is a coding agent. The default ACP mode is `manual` permission:
//! every tool that needs approval asks the client, and we answer every
//! `session/request_permission` with `cancelled`. We advertise no fs and no
//! terminal capability, and the session runs in a neutral temp dir, so it
//! never sees the user's project. A prompt that starts with `/` is a slash
//! command in ACP, so `guard_slash` makes sure ours never does.
//!
//! Terms
//! -----
//! Kimi Code subscriptions are "for personal interactive use only" (Kimi
//! Code Community Guidelines). Every call here comes from a user action:
//! one dictation, one command, one recap. Never call this from a loop over
//! many items. See `docs/dev/kimi-backend.md`.
//!
//! Privacy posture is the same as codex.rs: prompt on stdin only, stderr to
//! the local log only, `Display` redacts anything the CLI could echo back.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Status of the local Kimi Code CLI. Integer codes are pinned by
/// `as_code()` and consumed by the C# / Swift hosts; do not renumber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KimiStatus {
    Ready { binary_path: PathBuf },
    NotLoggedIn { binary_path: PathBuf },
    NotInstalled,
}

impl KimiStatus {
    pub fn as_code(&self) -> i32 {
        match self {
            Self::Ready { .. } => 0,
            Self::NotLoggedIn { .. } => 1,
            Self::NotInstalled => 2,
        }
    }

    pub fn binary_path(&self) -> Option<&Path> {
        match self {
            Self::Ready { binary_path } | Self::NotLoggedIn { binary_path } => Some(binary_path),
            Self::NotInstalled => None,
        }
    }
}

/// `None` = never resolved, `Some(None)` = resolved: absent,
/// `Some(Some(p))` = resolved at `p`. Same encoding as codex.rs.
static BINARY_CACHE: RwLock<Option<Option<PathBuf>>> = RwLock::new(None);

pub fn clear_cache() {
    if let Ok(mut g) = BINARY_CACHE.write() {
        *g = None;
    }
}

/// `$KIMI_CODE_HOME`, default `~/.kimi-code`: config, credentials, and
/// (native installer) `bin/kimi[.exe]`.
fn kimi_home() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("KIMI_CODE_HOME") {
        if !h.trim().is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    dirs::home_dir().map(|h| h.join(".kimi-code"))
}

/// Where `kimi` can live. The native installer (`install.ps1` /
/// `install.sh`, verified 2026-11-09) puts it in `$KIMI_INSTALL_DIR/bin`,
/// default `~/.kimi-code/bin`, and only edits the user PATH for future
/// shells, so that dir comes first. npm (`@moonshot-ai/kimi-code`) puts a
/// shim in the Node manager's bin dir. The archived Python `kimi-cli` also
/// installs a `kimi` (in `~/.local/bin`); it has no `acp` subcommand, so it
/// comes after the native and npm dirs.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let push_variants = |paths: &mut Vec<PathBuf>, dir: PathBuf| {
        #[cfg(target_os = "windows")]
        {
            paths.push(dir.join("kimi.exe"));
            paths.push(dir.join("kimi.cmd"));
        }
        #[cfg(not(target_os = "windows"))]
        {
            paths.push(dir.join("kimi"));
        }
    };

    if let Ok(dir) = std::env::var("KIMI_INSTALL_DIR") {
        if !dir.trim().is_empty() {
            push_variants(&mut paths, PathBuf::from(dir).join("bin"));
        }
    }
    if let Some(home) = kimi_home() {
        push_variants(&mut paths, home.join("bin"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(app_data) = std::env::var("APPDATA") {
            push_variants(&mut paths, PathBuf::from(&app_data).join("npm"));
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            push_variants(&mut paths, PathBuf::from(&local).join("npm"));
            push_variants(&mut paths, PathBuf::from(&local).join("pnpm"));
        }
    }

    if let Some(home) = dirs::home_dir() {
        push_variants(&mut paths, home.join(".npm-global").join("bin"));
        push_variants(&mut paths, home.join(".volta").join("bin"));
        let nvm_root = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                push_variants(&mut paths, entry.path().join("bin"));
            }
        }
        #[cfg(target_os = "macos")]
        push_variants(&mut paths, home.join("Library").join("pnpm"));
        #[cfg(target_os = "linux")]
        push_variants(&mut paths, home.join(".local").join("share").join("pnpm"));
    }

    #[cfg(target_os = "macos")]
    {
        push_variants(&mut paths, PathBuf::from("/opt/homebrew/bin"));
        push_variants(&mut paths, PathBuf::from("/usr/local/bin"));
    }
    #[cfg(target_os = "linux")]
    push_variants(&mut paths, PathBuf::from("/usr/local/bin"));

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            push_variants(&mut paths, dir);
        }
    }
    // The live registry PATH, so a just-installed kimi resolves without
    // restarting Dimmy (same reason as codex.rs).
    #[cfg(target_os = "windows")]
    for dir in crate::win_paths::registry_path_dirs() {
        push_variants(&mut paths, dir);
    }
    paths
}

/// macOS GUI apps inherit only the base PATH: ask the login shell.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn detect_via_login_shell() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let shell = "/bin/zsh";
    #[cfg(target_os = "linux")]
    let shell = "/bin/bash";
    let output = Command::new(shell)
        .args(["-l", "-c", "command -v kimi 2>/dev/null"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let p = PathBuf::from(stdout.lines().next()?.trim());
    p.is_file().then_some(p)
}

pub fn detect_binary() -> Option<PathBuf> {
    if let Ok(g) = BINARY_CACHE.read() {
        if let Some(cached) = g.as_ref() {
            return cached.clone();
        }
    }
    let resolved = candidate_paths().into_iter().find(|c| c.is_file());
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let resolved = resolved.or_else(detect_via_login_shell);
    if resolved.is_none() {
        crate::log("[Kimi] resolve_binary: no kimi binary found in any candidate path");
    }
    if let Ok(mut g) = BINARY_CACHE.write() {
        *g = Some(resolved.clone());
    }
    resolved
}

/// True iff `kimi login` has stored an OAuth token: a non-empty
/// `credentials/<name>.json` under the Kimi home. Existence only: the CLI
/// is the sole reader of that file. `credentials/mcp/` holds MCP server
/// tokens, which say nothing about the account, so it is skipped.
pub fn has_credentials() -> bool {
    let Some(home) = kimi_home() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(home.join("credentials")) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        p.extension().is_some_and(|x| x == "json")
            && std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() > 0)
    })
}

/// Binary + credentials. Cheap: spawns nothing.
pub fn status() -> KimiStatus {
    match detect_binary() {
        None => KimiStatus::NotInstalled,
        Some(p) if has_credentials() => KimiStatus::Ready { binary_path: p },
        Some(p) => KimiStatus::NotLoggedIn { binary_path: p },
    }
}

/// The Windows CLI runs its tools through Git Bash and refuses to start
/// without it (`locateWindowsGitBash` in the 2.5.2 bundle: `KIMI_SHELL_PATH`,
/// then next to a `git.exe` on PATH, then the Git for Windows install dirs).
/// The wizard checks this before offering the Kimi install. Always true
/// elsewhere.
pub fn git_bash_present() -> bool {
    #[cfg(target_os = "windows")]
    {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(p) = std::env::var("KIMI_SHELL_PATH") {
            if !p.trim().is_empty() {
                candidates.push(PathBuf::from(p.trim()));
            }
        }
        let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        for dir in path_dirs
            .into_iter()
            .chain(crate::win_paths::registry_path_dirs())
        {
            if dir.join("git.exe").is_file() {
                // <root>\cmd\git.exe or <root>\bin\git.exe -> <root>\bin\bash.exe
                if let Some(root) = dir.parent() {
                    candidates.push(root.join("bin").join("bash.exe"));
                    candidates.push(root.join("usr").join("bin").join("bash.exe"));
                }
            }
        }
        for base in ["C:\\Program Files\\Git", "C:\\Program Files (x86)\\Git"] {
            candidates.push(PathBuf::from(base).join("bin").join("bash.exe"));
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Git")
                    .join("bin")
                    .join("bash.exe"),
            );
        }
        candidates.iter().any(|c| c.is_file())
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

/// Paths + booleans only, for the Settings "diagnostics" view.
pub fn diagnostics_json() -> String {
    let candidates: Vec<Value> = candidate_paths()
        .into_iter()
        .map(|p| json!({ "path": p.to_string_lossy(), "exists": p.is_file() }))
        .collect();
    json!({
        "resolved": detect_binary().map(|p| p.to_string_lossy().into_owned()),
        "candidates": candidates,
        "credentials_present": has_credentials(),
        "git_bash_present": git_bash_present(),
    })
    .to_string()
}

#[derive(Debug)]
pub enum KimiError {
    NotInstalled,
    NotLoggedIn,
    Spawn(String),
    Timeout,
    /// The stream broke: the CLI exited, wrote no JSON-RPC, or answered
    /// something we can't read. Detail goes to the local log only.
    Protocol(String),
    /// The CLI answered with a JSON-RPC error (usage limit, bad model, …).
    Reported {
        code: i64,
        message: String,
    },
    /// The turn ended with `stopReason: refusal`.
    Refused,
    /// The turn ended cleanly with no text.
    Empty,
}

impl std::fmt::Display for KimiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "Kimi Code CLI not installed"),
            Self::NotLoggedIn => write!(f, "Kimi not signed in: run `kimi login`"),
            // SECURITY: spawn messages carry paths; protocol detail and the
            // reported message can echo transcript text. Category only.
            Self::Spawn(_) => write!(f, "Kimi spawn failed"),
            Self::Timeout => write!(f, "Kimi call timed out"),
            Self::Protocol(_) => write!(f, "Kimi CLI stream error"),
            Self::Reported { code, .. } => write!(f, "Kimi error {}", code),
            Self::Refused => write!(f, "Kimi refused the request"),
            Self::Empty => write!(f, "Kimi returned an empty answer"),
        }
    }
}

impl std::error::Error for KimiError {}

pub fn error_category(err: &KimiError) -> &'static str {
    match err {
        KimiError::NotInstalled => "not_installed",
        KimiError::NotLoggedIn => "not_logged_in",
        KimiError::Spawn(_) => "spawn",
        KimiError::Timeout => "timeout",
        KimiError::Protocol(_) => "protocol",
        KimiError::Reported { .. } => "reported",
        KimiError::Refused => "refused",
        KimiError::Empty => "empty",
    }
}

pub fn status_label(s: &KimiStatus) -> &'static str {
    match s {
        KimiStatus::Ready { .. } => "ready",
        KimiStatus::NotLoggedIn { .. } => "not_logged_in",
        KimiStatus::NotInstalled => "not_installed",
    }
}

// ── ACP conversation (pure: no process, testable) ───────────────────

/// ACP `RequestError.authRequired` code.
const ACP_AUTH_REQUIRED: i64 = -32000;

/// One line in, one line out. The process transport and the test script
/// both implement this.
trait Transport {
    fn send(&mut self, line: &str) -> Result<(), KimiError>;
    fn recv(&mut self, deadline: Instant) -> Result<String, KimiError>;
}

struct Conversation<'a, T: Transport> {
    t: &'a mut T,
    next_id: i64,
    deadline: Instant,
    /// Text of `agent_message_chunk` updates for the turn in flight.
    answer: String,
    tool_calls: usize,
}

impl<'a, T: Transport> Conversation<'a, T> {
    fn new(t: &'a mut T, deadline: Instant) -> Self {
        Self {
            t,
            next_id: 1,
            deadline,
            answer: String::new(),
            tool_calls: 0,
        }
    }

    /// Send a request and read until its response, answering whatever the
    /// agent asks in between and collecting the turn's text.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, KimiError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.t.send(&req.to_string())?;
        loop {
            let line = self.t.recv(self.deadline)?;
            let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
                continue; // the CLI's own non-JSON chatter
            };
            match (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
                (Some(rid), None) if rid.as_i64() == Some(id) => {
                    if let Some(err) = msg.get("error") {
                        return Err(KimiError::Reported {
                            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                            message: err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .chars()
                                .take(200)
                                .collect(),
                        });
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
                (Some(rid), Some(m)) => self.answer_agent_request(rid.clone(), m)?,
                (None, Some("session/update")) => self.on_update(&msg["params"]["update"]),
                _ => {}
            }
        }
    }

    /// The agent asks the client things mid-turn. We grant nothing.
    fn answer_agent_request(&mut self, id: Value, method: &str) -> Result<(), KimiError> {
        let reply = if method == "session/request_permission" {
            crate::log("[Kimi] tool permission requested: cancelled");
            json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "cancelled" } } })
        } else {
            crate::log(&format!("[Kimi] agent request {method}: refused"));
            json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "not supported by Dimmy" } })
        };
        self.t.send(&reply.to_string())
    }

    fn on_update(&mut self, update: &Value) {
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                if update["content"]["type"] == "text" {
                    if let Some(text) = update["content"]["text"].as_str() {
                        self.answer.push_str(text);
                    }
                }
            }
            Some("tool_call") => self.tool_calls += 1,
            _ => {}
        }
    }
}

/// The values of a `select` config option (`model`, `thinking`, …).
fn option_values(config_options: &Value, id: &str) -> Vec<String> {
    config_options
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["id"] == id)
        .and_then(|o| o["options"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v["value"].as_str().map(str::to_owned))
        .collect()
}

/// The option value for the model the user picked. The catalog id (`k3`)
/// and the CLI's alias (`kimi-code/k3`) differ in the prefix, so a match on
/// the last path segment counts. None = the tier doesn't offer it: the CLI
/// default answers instead of the call failing.
fn pick_model(offered: &[String], wanted: &str) -> Option<String> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return None;
    }
    offered
        .iter()
        .find(|v| v.as_str() == wanted)
        .or_else(|| {
            offered
                .iter()
                .find(|v| v.rsplit('/').next() == Some(wanted))
        })
        .cloned()
}

/// Thinking off for a quick rewrite where the model allows it, else the
/// lowest effort it declares. None = leave the model's default.
fn quick_thinking(offered: &[String]) -> Option<String> {
    ["off", "low"]
        .iter()
        .find(|v| offered.iter().any(|o| o == *v))
        .map(|v| v.to_string())
}

/// In ACP a prompt whose text starts with `/` runs a Kimi slash command
/// instead of a turn. A custom prompt may start with one.
fn guard_slash(prompt: &str) -> String {
    let out = if prompt.trim_start().starts_with('/') {
        format!("Instructions:\n\n{prompt}")
    } else {
        prompt.to_string()
    };
    assert!(!out.trim_start().starts_with('/'));
    out
}

struct Request<'a> {
    prompt: &'a str,
    model: &'a str,
    quick: bool,
    cwd: &'a Path,
}

/// The whole exchange: initialize, new session, options, one prompt turn,
/// delete the session. Returns the answer text.
fn converse<T: Transport>(
    t: &mut T,
    req: &Request,
    deadline: Instant,
) -> Result<String, KimiError> {
    assert!(!req.prompt.trim().is_empty(), "kimi: empty prompt");
    let mut c = Conversation::new(t, deadline);
    c.call(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false },
                                    "terminal": false },
            "clientInfo": { "name": "Dimmy", "version": env!("CARGO_PKG_VERSION") },
        }),
    )?;
    let session = match c.call(
        "session/new",
        json!({ "cwd": req.cwd.to_string_lossy(), "mcpServers": [] }),
    ) {
        Err(KimiError::Reported { code, .. }) if code == ACP_AUTH_REQUIRED => {
            return Err(KimiError::NotLoggedIn)
        }
        other => other?,
    };
    let Some(session_id) = session["sessionId"].as_str().map(str::to_owned) else {
        return Err(KimiError::Protocol("session/new without sessionId".into()));
    };
    let options = session["configOptions"].clone();

    let result = (|| {
        if let Some(model) = pick_model(&option_values(&options, "model"), req.model) {
            c.call(
                "session/set_config_option",
                json!({ "sessionId": session_id, "configId": "model", "value": model }),
            )?;
        } else if !req.model.trim().is_empty() {
            crate::log(&format!(
                "[Kimi] model {:?} not offered by this account; using the CLI default",
                req.model
            ));
        }
        if req.quick {
            if let Some(level) = quick_thinking(&option_values(&options, "thinking")) {
                c.call(
                    "session/set_config_option",
                    json!({ "sessionId": session_id, "configId": "thinking", "value": level }),
                )?;
            }
        }
        let turn = c.call(
            "session/prompt",
            json!({ "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": guard_slash(req.prompt) }] }),
        )?;
        match turn["stopReason"].as_str() {
            Some("refusal") => Err(KimiError::Refused),
            Some("end_turn") | Some("max_tokens") | Some("max_turn_requests") => {
                let text = std::mem::take(&mut c.answer);
                if text.trim().is_empty() {
                    Err(KimiError::Empty)
                } else {
                    Ok(text)
                }
            }
            other => Err(KimiError::Protocol(format!("stopReason {:?}", other))),
        }
    })();

    if c.tool_calls > 0 {
        crate::log(&format!(
            "[Kimi] turn attempted {} tool call(s)",
            c.tool_calls
        ));
    }
    // Best effort, short: the answer is already in hand.
    c.deadline = c.deadline.min(Instant::now() + Duration::from_secs(3));
    if let Err(e) = c.call("session/delete", json!({ "sessionId": session_id })) {
        crate::log(&format!("[Kimi] session/delete failed: {e:?}"));
    }
    result
}

// ── The process ─────────────────────────────────────────────────────

struct ProcessTransport {
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Transport for ProcessTransport {
    fn send(&mut self, line: &str) -> Result<(), KimiError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| KimiError::Protocol("stdin closed".into()))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| KimiError::Protocol(format!("stdin: {e}")))
    }

    fn recv(&mut self, deadline: Instant) -> Result<String, KimiError> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(wait) {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err(KimiError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                Err(KimiError::Protocol("kimi closed stdout".into()))
            }
        }
    }
}

/// A dedicated, empty working dir: Kimi reads `AGENTS.md` and project MCP
/// config from its cwd, and we want neither.
fn neutral_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join("dimmy-kimi");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::log(&format!("[Kimi] create {:?}: {e}", dir));
        return std::env::temp_dir();
    }
    dir
}

fn acp_command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.arg("acp");
    cmd.current_dir(neutral_cwd());
    // A Dimmy call must never trigger the CLI's self-update.
    cmd.env("KIMI_CODE_NO_AUTO_UPDATE", "1");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Close stdin, give the CLI a moment to exit on its own, then end the
/// tree. On a thread: the caller already has its answer.
fn retire(mut child: Child, tree: crate::cli_runner::Tree, transport: ProcessTransport) {
    drop(transport);
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let _ = crate::cli_runner::wait_until(&mut child, &tree, deadline);
    });
}

fn with_session<R>(
    timeout: Duration,
    f: impl FnOnce(&mut ProcessTransport, Instant) -> Result<R, KimiError>,
) -> Result<R, KimiError> {
    assert!(!timeout.is_zero(), "kimi: zero timeout");
    let binary = detect_binary().ok_or(KimiError::NotInstalled)?;
    if !has_credentials() {
        return Err(KimiError::NotLoggedIn);
    }
    let deadline = Instant::now() + timeout;
    let (mut child, tree) = crate::cli_runner::spawn(&mut acp_command(&binary))
        .map_err(|e| KimiError::Spawn(e.to_string()))?;
    let stderr_rx = crate::cli_runner::drain(child.stderr.take());
    let (tx, lines) = mpsc::channel();
    let stdout = child.stdout.take();
    std::thread::spawn(move || {
        let Some(out) = stdout else { return };
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut transport = ProcessTransport {
        stdin: child.stdin.take(),
        lines,
    };
    let result = f(&mut transport, deadline);
    if let Err(e) = &result {
        crate::log(&format!("[Kimi] call failed: {e:?}"));
        if matches!(e, KimiError::Protocol(_) | KimiError::Timeout) {
            tree.kill(&mut child);
            if let Ok(err) = stderr_rx.recv_timeout(Duration::from_secs(1)) {
                crate::log(&format!(
                    "[Kimi] stderr: {}",
                    crate::cli_runner::excerpt(&err)
                ));
            }
        }
    }
    retire(child, tree, transport);
    result
}

/// One LLM call through `kimi acp`. Blocks up to `timeout`.
/// `model` empty = the CLI default; `quick` = thinking off where the model
/// allows it (dictation rewrites, ping), never for recap or command mode.
pub fn run_blocking(
    prompt: &str,
    model: &str,
    timeout: Duration,
    quick: bool,
) -> Result<String, KimiError> {
    assert!(!prompt.trim().is_empty(), "kimi: empty prompt");
    let started = Instant::now();
    let cwd = neutral_cwd();
    crate::log(&format!(
        "[Kimi] call model={:?} quick={} prompt_chars={}",
        model,
        quick,
        prompt.len()
    ));
    let text = with_session(timeout, |t, deadline| {
        converse(
            t,
            &Request {
                prompt,
                model,
                quick,
                cwd: &cwd,
            },
            deadline,
        )
    })?;
    crate::log(&format!(
        "[Kimi] answered response_chars={} ms={}",
        text.len(),
        started.elapsed().as_millis()
    ));
    assert!(!text.trim().is_empty());
    Ok(text)
}

/// The models this account's tier serves, as `(value, name)`, read from a
/// throwaway session (deleted at once). Feeds the model picker so it only
/// shows ids the subscription accepts.
pub fn list_models(timeout: Duration) -> Result<Vec<(String, String)>, KimiError> {
    let cwd = neutral_cwd();
    with_session(timeout, |t, deadline| {
        let mut c = Conversation::new(t, deadline);
        c.call(
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {} }),
        )?;
        let session = match c.call(
            "session/new",
            json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
        ) {
            Err(KimiError::Reported { code, .. }) if code == ACP_AUTH_REQUIRED => {
                return Err(KimiError::NotLoggedIn)
            }
            other => other?,
        };
        let models = models_of(&session["configOptions"]);
        if let Some(id) = session["sessionId"].as_str() {
            c.deadline = c.deadline.min(Instant::now() + Duration::from_secs(3));
            let _ = c.call("session/delete", json!({ "sessionId": id }));
        }
        Ok(models)
    })
}

fn models_of(config_options: &Value) -> Vec<(String, String)> {
    config_options
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["id"] == "model")
        .and_then(|o| o["options"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let value = v["value"].as_str()?.to_owned();
            let name = v["name"].as_str().unwrap_or(&value).to_owned();
            Some((value, name))
        })
        .collect()
}

/// `kimi login` (device-code OAuth) in a visible terminal: the user needs
/// to see the code. Returns once spawned; the host re-checks `status()`.
pub fn spawn_login() -> Result<(), KimiError> {
    let binary = detect_binary().ok_or(KimiError::NotInstalled)?;

    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "start", ""]);
        cmd.arg(&binary);
        cmd.arg("login");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.spawn().map_err(|e| KimiError::Spawn(e.to_string()))?;
    }
    #[cfg(target_os = "macos")]
    {
        let command = format!("'{}' login", binary.display());
        crate::run_in_new_terminal_window(&command, "kimi-login")
            .map_err(|e| KimiError::Spawn(e.to_string()))?;
    }
    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new(&binary);
        cmd.arg("login");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.spawn().map_err(|e| KimiError::Spawn(e.to_string()))?;
    }

    crate::log("[Kimi] login subprocess spawned");
    crate::telemetry::track(crate::telemetry::Event::KimiCliLoginSpawned);
    Ok(())
}

/// The command the setup wizard shows (and can run) to install the CLI.
/// The vendor's own installer: native binary, no Node needed.
pub fn install_command() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "irm https://code.kimi.com/kimi-code/install.ps1 | iex"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash"
    }
}

/// Synthetic provider URL, same trick as `codex://`.
pub const PROVIDER_URL: &str = "kimi-cli://default";

pub fn is_kimi_cli_url(api_url: &str) -> bool {
    api_url.trim().starts_with("kimi-cli://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted Kimi: answers each request by method, records what we sent.
    struct Script {
        sent: Vec<Value>,
        inbox: VecDeque<String>,
        on: Box<dyn FnMut(&Value) -> Vec<Value>>,
    }

    impl Script {
        fn new(on: impl FnMut(&Value) -> Vec<Value> + 'static) -> Self {
            Self {
                sent: vec![],
                inbox: VecDeque::new(),
                on: Box::new(on),
            }
        }
        fn methods(&self) -> Vec<String> {
            self.sent
                .iter()
                .filter_map(|m| m["method"].as_str().map(str::to_owned))
                .collect()
        }
    }

    impl Transport for Script {
        fn send(&mut self, line: &str) -> Result<(), KimiError> {
            let msg: Value = serde_json::from_str(line).expect("we send JSON");
            for reply in (self.on)(&msg) {
                self.inbox.push_back(reply.to_string());
            }
            self.sent.push(msg);
            Ok(())
        }
        fn recv(&mut self, _deadline: Instant) -> Result<String, KimiError> {
            self.inbox
                .pop_front()
                .ok_or_else(|| KimiError::Protocol("script exhausted".into()))
        }
    }

    fn ok(id: &Value, result: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "result": result })
    }

    fn config_options() -> Value {
        json!([
            { "id": "model", "type": "select", "currentValue": "kimi-code/kimi-for-coding",
              "options": [ { "value": "kimi-code/kimi-for-coding", "name": "Kimi for Coding" },
                           { "value": "kimi-code/k3", "name": "K3" } ] },
            { "id": "thinking", "type": "select", "currentValue": "on",
              "options": [ { "value": "off" }, { "value": "on" } ] },
            { "id": "mode", "type": "select", "currentValue": "default", "options": [] }
        ])
    }

    /// A well-behaved Kimi that streams `answer` in two chunks.
    fn kimi(answer: &'static str) -> Script {
        Script::new(move |m| {
            let id = &m["id"];
            match m["method"].as_str() {
                Some("initialize") => vec![ok(id, json!({ "protocolVersion": 1 }))],
                Some("session/new") => vec![ok(
                    id,
                    json!({ "sessionId": "s1", "configOptions": config_options() }),
                )],
                Some("session/set_config_option") => vec![ok(id, json!({}))],
                Some("session/prompt") => {
                    let (a, b) = answer.split_at(answer.len() / 2);
                    let chunk = |t: &str| {
                        json!({ "jsonrpc": "2.0", "method": "session/update",
                                "params": { "sessionId": "s1", "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": { "type": "text", "text": t } } } })
                    };
                    vec![
                        json!("not json at all"),
                        chunk(a),
                        json!({ "jsonrpc": "2.0", "method": "session/update",
                                "params": { "update": { "sessionUpdate": "agent_thought_chunk",
                                    "content": { "type": "text", "text": "THINKING" } } } }),
                        chunk(b),
                        ok(id, json!({ "stopReason": "end_turn" })),
                    ]
                }
                Some("session/delete") => vec![ok(id, json!({}))],
                _ => vec![],
            }
        })
    }

    fn req<'a>(prompt: &'a str, model: &'a str, quick: bool) -> Request<'a> {
        Request {
            prompt,
            model,
            quick,
            cwd: Path::new("/tmp/dimmy-kimi"),
        }
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn a_turn_returns_the_message_chunks_and_not_the_thinking() {
        let mut t = kimi("Hello there, world.");
        let out = converse(&mut t, &req("rewrite this", "", false), far()).unwrap();
        assert_eq!(out, "Hello there, world.");
        assert_eq!(
            t.methods(),
            [
                "initialize",
                "session/new",
                "session/prompt",
                "session/delete"
            ]
        );
    }

    #[test]
    fn the_prompt_goes_in_the_prompt_turn_and_the_session_is_deleted() {
        let mut t = kimi("ok");
        converse(&mut t, &req("my transcript", "", false), far()).unwrap();
        let prompt = t
            .sent
            .iter()
            .find(|m| m["method"] == "session/prompt")
            .unwrap();
        assert_eq!(prompt["params"]["prompt"][0]["text"], "my transcript");
        assert_eq!(prompt["params"]["sessionId"], "s1");
        let new = t
            .sent
            .iter()
            .find(|m| m["method"] == "session/new")
            .unwrap();
        assert_eq!(new["params"]["mcpServers"], json!([]));
        let del = t
            .sent
            .iter()
            .find(|m| m["method"] == "session/delete")
            .unwrap();
        assert_eq!(del["params"]["sessionId"], "s1");
    }

    #[test]
    fn we_advertise_no_fs_and_no_terminal() {
        let mut t = kimi("ok");
        converse(&mut t, &req("x", "", false), far()).unwrap();
        let caps = &t.sent[0]["params"]["clientCapabilities"];
        assert_eq!(caps["terminal"], false);
        assert_eq!(caps["fs"]["readTextFile"], false);
        assert_eq!(caps["fs"]["writeTextFile"], false);
    }

    #[test]
    fn a_catalog_model_id_maps_to_the_cli_alias() {
        let mut t = kimi("ok");
        converse(&mut t, &req("x", "k3", false), far()).unwrap();
        let set = t
            .sent
            .iter()
            .find(|m| m["params"]["configId"] == "model")
            .expect("model set");
        assert_eq!(set["params"]["value"], "kimi-code/k3");
    }

    #[test]
    fn a_model_the_tier_lacks_is_dropped_not_sent() {
        let mut t = kimi("ok");
        let out = converse(&mut t, &req("x", "kimi-for-coding-highspeed", false), far());
        assert_eq!(out.unwrap(), "ok");
        assert!(!t.sent.iter().any(|m| m["params"]["configId"] == "model"));
    }

    #[test]
    fn quick_turns_thinking_off_and_slow_leaves_it() {
        let mut quick = kimi("ok");
        converse(&mut quick, &req("x", "", true), far()).unwrap();
        let set = quick
            .sent
            .iter()
            .find(|m| m["params"]["configId"] == "thinking")
            .expect("thinking set");
        assert_eq!(set["params"]["value"], "off");

        let mut slow = kimi("ok");
        converse(&mut slow, &req("x", "", false), far()).unwrap();
        assert!(!slow
            .sent
            .iter()
            .any(|m| m["params"]["configId"] == "thinking"));
    }

    #[test]
    fn quick_thinking_prefers_off_then_low_then_nothing() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(quick_thinking(&s(&["off", "on"])).as_deref(), Some("off"));
        assert_eq!(quick_thinking(&s(&["low", "high"])).as_deref(), Some("low"));
        assert_eq!(quick_thinking(&s(&["on"])), None);
    }

    #[test]
    fn auth_required_on_session_new_means_not_logged_in() {
        let mut t = Script::new(|m| {
            let id = &m["id"];
            match m["method"].as_str() {
                Some("initialize") => vec![ok(id, json!({}))],
                Some("session/new") => vec![json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32000, "message": "Authentication required" } })],
                _ => vec![],
            }
        });
        let r = converse(&mut t, &req("x", "", false), far());
        assert!(matches!(r, Err(KimiError::NotLoggedIn)), "{r:?}");
    }

    #[test]
    fn every_permission_request_is_cancelled() {
        let mut t = Script::new(|m| {
            let id = &m["id"];
            match m["method"].as_str() {
                Some("initialize") => vec![ok(id, json!({}))],
                Some("session/new") => vec![ok(id, json!({ "sessionId": "s1" }))],
                Some("session/prompt") => vec![
                    json!({ "jsonrpc": "2.0", "id": 900, "method": "session/request_permission",
                            "params": { "sessionId": "s1", "toolCall": { "title": "rm -rf" },
                                        "options": [ { "optionId": "allow", "kind": "allow_once" } ] } }),
                    json!({ "jsonrpc": "2.0", "id": 901, "method": "fs/read_text_file",
                            "params": { "path": "/etc/passwd" } }),
                    json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": "done" } } } }),
                    ok(id, json!({ "stopReason": "end_turn" })),
                ],
                Some("session/delete") => vec![ok(id, json!({}))],
                _ => vec![],
            }
        });
        assert_eq!(
            converse(&mut t, &req("x", "", false), far()).unwrap(),
            "done"
        );
        let perm = t.sent.iter().find(|m| m["id"] == 900).expect("answered");
        assert_eq!(perm["result"]["outcome"]["outcome"], "cancelled");
        let fs = t.sent.iter().find(|m| m["id"] == 901).expect("answered");
        assert_eq!(fs["error"]["code"], -32601);
    }

    #[test]
    fn a_refusal_and_an_empty_turn_are_errors() {
        for (stop, text) in [("refusal", "nope"), ("end_turn", "  ")] {
            let mut t = Script::new(move |m| {
                let id = &m["id"];
                match m["method"].as_str() {
                    Some("initialize") => vec![ok(id, json!({}))],
                    Some("session/new") => vec![ok(id, json!({ "sessionId": "s1" }))],
                    Some("session/prompt") => vec![
                        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": text } } } }),
                        ok(id, json!({ "stopReason": stop })),
                    ],
                    Some("session/delete") => vec![ok(id, json!({}))],
                    _ => vec![],
                }
            });
            let r = converse(&mut t, &req("x", "", false), far());
            match stop {
                "refusal" => assert!(matches!(r, Err(KimiError::Refused)), "{r:?}"),
                _ => assert!(matches!(r, Err(KimiError::Empty)), "{r:?}"),
            }
            assert!(t.methods().contains(&"session/delete".to_string()));
        }
    }

    #[test]
    fn a_reported_error_keeps_its_code_and_a_short_message() {
        let mut t = Script::new(|m| {
            let id = &m["id"];
            match m["method"].as_str() {
                Some("initialize") => vec![ok(id, json!({}))],
                Some("session/new") => vec![ok(id, json!({ "sessionId": "s1" }))],
                Some("session/prompt") => vec![json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": 429, "message": "x".repeat(500) } })],
                Some("session/delete") => vec![ok(id, json!({}))],
                _ => vec![],
            }
        });
        match converse(&mut t, &req("x", "", false), far()) {
            Err(KimiError::Reported { code, message }) => {
                assert_eq!(code, 429);
                assert_eq!(message.chars().count(), 200);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_stream_that_ends_early_is_a_protocol_error() {
        let mut t = Script::new(|m| match m["method"].as_str() {
            Some("initialize") => vec![ok(&m["id"], json!({}))],
            _ => vec![],
        });
        let r = converse(&mut t, &req("x", "", false), far());
        assert!(matches!(r, Err(KimiError::Protocol(_))), "{r:?}");
    }

    #[test]
    fn a_slash_prompt_never_reaches_kimi_as_a_command() {
        assert_eq!(guard_slash("hello"), "hello");
        assert!(!guard_slash("/logout now").starts_with('/'));
        assert!(!guard_slash("  /help").trim_start().starts_with('/'));
        let mut t = kimi("ok");
        converse(&mut t, &req("/clear everything", "", false), far()).unwrap();
        let prompt = t
            .sent
            .iter()
            .find(|m| m["method"] == "session/prompt")
            .unwrap();
        let text = prompt["params"]["prompt"][0]["text"].as_str().unwrap();
        assert!(!text.trim_start().starts_with('/'));
        assert!(text.contains("/clear everything"));
    }

    #[test]
    fn models_of_reads_the_model_select() {
        let m = models_of(&config_options());
        assert_eq!(
            m,
            vec![
                (
                    "kimi-code/kimi-for-coding".to_string(),
                    "Kimi for Coding".to_string()
                ),
                ("kimi-code/k3".to_string(), "K3".to_string()),
            ]
        );
        assert!(models_of(&json!(null)).is_empty());
    }

    #[test]
    fn is_kimi_cli_url_matches_scheme() {
        assert!(is_kimi_cli_url("kimi-cli://default"));
        assert!(is_kimi_cli_url("  kimi-cli://x "));
        assert!(!is_kimi_cli_url(
            "https://api.kimi.ai/coding/v1/chat/completions"
        ));
        assert!(!is_kimi_cli_url("codex://default"));
        assert!(!is_kimi_cli_url(""));
    }

    #[test]
    fn status_codes_are_stable() {
        let p = PathBuf::from("/x");
        assert_eq!(
            KimiStatus::Ready {
                binary_path: p.clone()
            }
            .as_code(),
            0
        );
        assert_eq!(KimiStatus::NotLoggedIn { binary_path: p }.as_code(), 1);
        assert_eq!(KimiStatus::NotInstalled.as_code(), 2);
    }

    #[test]
    fn display_never_leaks_cli_text() {
        let cases = [
            KimiError::Spawn("C:\\secret\\path".into()),
            KimiError::Protocol("your transcript".into()),
            KimiError::Reported {
                code: 1,
                message: "your transcript".into(),
            },
        ];
        for e in cases {
            let s = e.to_string();
            assert!(!s.contains("secret") && !s.contains("transcript"), "{s}");
        }
    }

    #[test]
    fn error_category_covers_every_variant() {
        assert_eq!(error_category(&KimiError::NotInstalled), "not_installed");
        assert_eq!(error_category(&KimiError::NotLoggedIn), "not_logged_in");
        assert_eq!(error_category(&KimiError::Spawn(String::new())), "spawn");
        assert_eq!(error_category(&KimiError::Timeout), "timeout");
        assert_eq!(
            error_category(&KimiError::Protocol(String::new())),
            "protocol"
        );
        assert_eq!(
            error_category(&KimiError::Reported {
                code: 0,
                message: String::new()
            }),
            "reported"
        );
        assert_eq!(error_category(&KimiError::Refused), "refused");
        assert_eq!(error_category(&KimiError::Empty), "empty");
    }

    #[test]
    fn candidate_paths_start_with_the_native_install_dir() {
        let first = candidate_paths().into_iter().next().expect("candidates");
        let s = first.to_string_lossy().to_lowercase();
        assert!(
            s.contains(".kimi-code") || std::env::var("KIMI_INSTALL_DIR").is_ok(),
            "{s}"
        );
    }

    #[test]
    fn detect_binary_returns_only_existing_files() {
        if let Some(p) = detect_binary() {
            assert!(p.is_file(), "{p:?}");
        }
    }
}
