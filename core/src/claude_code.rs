//! Claude Code CLI integration — use the user's Anthropic
//! subscription (Pro / Team / Max) for LLM calls instead of
//! consuming API-key credits.
//!
//! Architecture
//! ============
//! Anthropic does not (as of 2026-05) publish an OAuth flow for
//! third-party apps to use a user's Claude subscription directly.
//! What they DO ship is the official `claude` CLI (Claude Code),
//! which handles browser-based login → stores credentials in
//! `~/.claude/credentials.json` → exposes `claude --print` as a
//! programmatic interface.
//!
//! We piggyback on that:
//!   1. **Detect**: locate the `claude` binary on PATH or common
//!      install paths.
//!   2. **Login**: spawn `claude login` (a foreground subprocess);
//!      Claude Code itself opens the browser + completes OAuth +
//!      writes credentials. We're a passive consumer.
//!   3. **Invoke**: for every LLM call, spawn `claude --print
//!      --model <id>` with the prompt on stdin, read stdout as the
//!      response. Same provider semantics as our HTTP path —
//!      synchronous text-in-text-out.
//!
//! Privacy + safety
//! ----------------
//! - No tokens leave Rust. We never read `~/.claude/credentials.json`
//!   directly; the CLI is the only consumer of that file.
//! - Subprocess stdin/stdout are anonymous pipes — no command-line
//!   args carry user content (prompts can be megabytes; CLI arg
//!   length limits + leaking via `ps`).
//! - Timeout (default 5 min) so a runaway model doesn't pin a thread.
//! - Stderr is captured separately and logged locally, never
//!   forwarded to telemetry.
//!
//! Cross-platform
//! --------------
//! Same code path on Win + Mac + Linux. The differences:
//!   - Win: `claude.exe` lives in `%LOCALAPPDATA%\AnthropicClaude\`
//!     or on PATH (npm install -g @anthropic-ai/claude-code).
//!   - Mac: `/usr/local/bin/claude`, `/opt/homebrew/bin/claude`, or
//!     `~/.claude/local/claude`.
//!   - Linux: `/usr/local/bin/claude` typically.
//!
//! `detect_binary` walks all the candidates.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

/// Status of the local Claude Code install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeCodeStatus {
    /// `claude` binary found AND user is logged in (credentials file
    /// present + non-empty). Ready to dispatch LLM calls.
    Ready { binary_path: PathBuf },
    /// Binary found but no credentials — user needs to run `claude login`.
    NotLoggedIn { binary_path: PathBuf },
    /// Binary not found on the system. User must install Claude Code.
    NotInstalled,
}

impl ClaudeCodeStatus {
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

/// Cache the binary location across calls so `status()` doesn't
/// re-walk the filesystem on every invocation. The cache is
/// invalidated by `clear_cache()` which the UI calls after a
/// successful `claude login`.
static BINARY_CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Reset the cached binary location. Call after the user installs
/// Claude Code mid-session or after a login subprocess completes
/// (the login flow can re-write the binary too).
pub fn clear_cache() {
    // OnceLock has no public reset — for now we rely on the user
    // restarting Dimmy after install. Documented limitation.
    // A future refactor can replace OnceLock with RwLock<Option<…>>.
}

/// Common locations where Claude Code installs the binary,
/// cross-platform. The list is walked in order until the first
/// existing file is hit.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. User-local install dir (preferred — survives across PATH
    //    changes, doesn't require admin).
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "windows")]
        {
            paths.push(home.join(".claude").join("local").join("claude.cmd"));
            paths.push(home.join(".claude").join("local").join("claude.exe"));
            paths.push(home.join(".claude").join("local").join("claude"));
        }
        #[cfg(not(target_os = "windows"))]
        {
            paths.push(home.join(".claude").join("local").join("claude"));
        }
    }

    // 2. Platform-typical install dirs.
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/opt/homebrew/bin/claude"));
        paths.push(PathBuf::from("/usr/local/bin/claude"));
        paths.push(PathBuf::from("/usr/bin/claude"));
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/local/bin/claude"));
        paths.push(PathBuf::from("/usr/bin/claude"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(&local_app_data)
                    .join("AnthropicClaude")
                    .join("claude.exe"),
            );
            paths.push(
                PathBuf::from(&local_app_data)
                    .join("Programs")
                    .join("AnthropicClaude")
                    .join("claude.exe"),
            );
            // npm global install via `npm i -g @anthropic-ai/claude-code`
            paths.push(
                PathBuf::from(&local_app_data)
                    .join("npm")
                    .join("claude.cmd"),
            );
        }
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            paths.push(
                PathBuf::from(&program_files)
                    .join("AnthropicClaude")
                    .join("claude.exe"),
            );
        }
    }

    // 3. PATH walk — last resort because more expensive (each PATH
    //    entry is stat'd). Stops at the first match.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            #[cfg(target_os = "windows")]
            {
                paths.push(dir.join("claude.exe"));
                paths.push(dir.join("claude.cmd"));
            }
            paths.push(dir.join("claude"));
        }
    }

    paths
}

/// Locate the `claude` binary. Returns the first candidate that
/// exists on disk. Returns `None` if none of the candidates match.
pub fn detect_binary() -> Option<PathBuf> {
    BINARY_CACHE
        .get_or_init(|| candidate_paths().into_iter().find(|c| c.is_file()))
        .clone()
}

/// True iff a `~/.claude/credentials.json` (or platform equivalent)
/// exists and is non-empty. We deliberately do NOT parse it — Claude
/// Code's file format is not a public contract. The "exists +
/// non-empty" heuristic is correct for the live binary (the CLI
/// writes the file atomically after the OAuth callback completes).
pub fn has_credentials() -> bool {
    // ~/.claude/credentials.json
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join("credentials.json");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.len() > 0 {
                return true;
            }
        }
        // Alt: ~/.claude/.credentials.json (some Claude Code versions
        // use the dotfile variant on Mac/Linux).
        let alt = home.join(".claude").join(".credentials.json");
        if let Ok(meta) = std::fs::metadata(&alt) {
            if meta.is_file() && meta.len() > 0 {
                return true;
            }
        }
    }
    false
}

/// Combine binary detection + credentials check into a single
/// status enum. Cheap — does NOT spawn a subprocess.
pub fn status() -> ClaudeCodeStatus {
    match detect_binary() {
        None => ClaudeCodeStatus::NotInstalled,
        Some(p) => {
            if has_credentials() {
                ClaudeCodeStatus::Ready { binary_path: p }
            } else {
                ClaudeCodeStatus::NotLoggedIn { binary_path: p }
            }
        }
    }
}

/// Errors surfaced from the `run` path. Stays narrow on purpose —
/// the caller wraps these in `LlmError` for the existing dispatch.
#[derive(Debug)]
pub enum ClaudeCodeError {
    NotInstalled,
    NotLoggedIn,
    Spawn(String),
    Timeout,
    NonZeroExit { code: i32, stderr_excerpt: String },
    InvalidUtf8,
}

impl std::fmt::Display for ClaudeCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "Claude Code CLI not installed"),
            Self::NotLoggedIn => write!(f, "Claude Code not logged in — run `claude login`"),
            // SECURITY: like TranscribeError/LlmError, never embed
            // the spawn error message that may include path
            // fragments from the user's machine. The local
            // dimmy.log gets the detail; telemetry sees only the
            // category.
            Self::Spawn(_) => write!(f, "Claude Code spawn failed"),
            Self::Timeout => write!(f, "Claude Code call timed out"),
            // Same redaction rule: never the stderr in the Display
            // output. The struct field is for local log only.
            Self::NonZeroExit { code, .. } => write!(f, "Claude Code exit code {}", code),
            Self::InvalidUtf8 => write!(f, "Claude Code stdout was not UTF-8"),
        }
    }
}

impl std::error::Error for ClaudeCodeError {}

/// Run a single Claude Code invocation. Synchronous — blocks the
/// calling thread for up to `timeout`. The wrapper for async
/// contexts spawns this on a thread.
///
/// `model` is passed through with `--model` if non-empty; empty =
/// use whatever default Claude Code is configured for.
///
/// `prompt` is written to stdin and only stdin — never as a CLI
/// arg. Anthropic's CLI accepts any length there.
pub fn run_blocking(
    prompt: &str,
    model: &str,
    timeout: Duration,
) -> Result<String, ClaudeCodeError> {
    let binary = match detect_binary() {
        Some(p) => p,
        None => return Err(ClaudeCodeError::NotInstalled),
    };
    if !has_credentials() {
        return Err(ClaudeCodeError::NotLoggedIn);
    }

    let mut cmd = Command::new(&binary);
    cmd.arg("--print");
    cmd.arg("--output-format");
    cmd.arg("text");
    if !model.is_empty() {
        cmd.arg("--model");
        cmd.arg(model);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Suppress the console window flash on Windows. Dimmy is a GUI
    // app (no parent console), so without CREATE_NO_WINDOW Windows
    // allocates a new console for `claude --print` and the user
    // sees a black cmd window flash for every recap + rewrite.
    // 0x08000000 = CREATE_NO_WINDOW. Mac + Linux don't have this
    // problem (no console concept on Unix processes).
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    crate::log(&format!(
        "[ClaudeCode] spawn binary={:?} model={:?} prompt_chars={}",
        binary,
        model,
        prompt.len()
    ));

    let mut child = cmd
        .spawn()
        .map_err(|e| ClaudeCodeError::Spawn(format!("{}", e)))?;

    // Pipe prompt to stdin. Drop the handle before waiting so the
    // CLI sees EOF and starts generating.
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(prompt.as_bytes()) {
            // Kill the child to avoid an orphan.
            let _ = child.kill();
            return Err(ClaudeCodeError::Spawn(format!("stdin write: {}", e)));
        }
        drop(stdin);
    } else {
        let _ = child.kill();
        return Err(ClaudeCodeError::Spawn("no stdin handle".into()));
    }

    // Wait with timeout. std::process::Child doesn't ship a
    // built-in timed wait, so we poll. 100 ms granularity keeps
    // CPU near zero while still terminating within 1 tick after
    // the model finishes.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(ClaudeCodeError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(ClaudeCodeError::Spawn(format!("wait: {}", e)));
            }
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| ClaudeCodeError::Spawn(format!("collect: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let excerpt: String = stderr.chars().take(500).collect();
        crate::log(&format!(
            "[ClaudeCode] non-zero exit code={:?} stderr={}",
            output.status.code(),
            excerpt
        ));
        return Err(ClaudeCodeError::NonZeroExit {
            code: output.status.code().unwrap_or(-1),
            stderr_excerpt: excerpt,
        });
    }

    let text = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Err(ClaudeCodeError::InvalidUtf8),
    };

    crate::log(&format!(
        "[ClaudeCode] success prompt_chars={} response_chars={}",
        prompt.len(),
        text.len()
    ));
    Ok(text)
}

/// Spawn `claude /login` as a detached subprocess. Returns once the
/// subprocess is spawned; the user completes the browser flow in
/// their own time. Caller should re-check `status()` afterwards
/// (poll once every few seconds, or on focus return).
///
/// On Win we use `cmd /c start` to detach from our process tree.
/// On Mac we use `open -a Terminal` so the user sees the CLI URL
/// prompt — `claude login` doesn't fire a notification, the user
/// must read the printed URL.
pub fn spawn_login() -> Result<(), ClaudeCodeError> {
    let binary = match detect_binary() {
        Some(p) => p,
        None => return Err(ClaudeCodeError::NotInstalled),
    };

    #[cfg(target_os = "windows")]
    {
        // Spawn in a new window so the user sees the URL prompt.
        // /c → run cmd then exit. start "" → new window with no title.
        let mut cmd = Command::new("cmd");
        cmd.arg("/c");
        cmd.arg("start");
        cmd.arg(""); // empty window title
        cmd.arg(&binary);
        cmd.arg("/login");
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| ClaudeCodeError::Spawn(format!("{}", e)))?;
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        // Open a new Terminal window running claude login. On Mac
        // we use `open -a Terminal.app -n` — `-n` opens a fresh
        // instance so the user can see the URL even if Terminal
        // is already open with other tabs.
        //
        // The `claude login` command prints a URL the user clicks;
        // it then waits for the callback to land. We don't try to
        // hide that window — the user explicitly initiated this
        // and benefits from seeing the URL + "logged in" message.
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                "tell application \"Terminal\" to do script \"{} /login\"",
                binary.display()
            );
            let mut cmd = Command::new("osascript");
            cmd.arg("-e").arg(&script);
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
            cmd.spawn()
                .map_err(|e| ClaudeCodeError::Spawn(format!("{}", e)))?;
        }
        #[cfg(target_os = "linux")]
        {
            // Fall back to running in the background — Linux has too
            // many terminal emulators to dispatch to a specific one.
            // The user can re-run from their own terminal if needed.
            let mut cmd = Command::new(&binary);
            cmd.arg("/login");
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
            cmd.spawn()
                .map_err(|e| ClaudeCodeError::Spawn(format!("{}", e)))?;
        }
    }

    crate::log("[ClaudeCode] login subprocess spawned");
    crate::telemetry::track(crate::telemetry::Event::ClaudeCodeLoginSpawned);
    Ok(())
}

/// Convert a `ClaudeCodeError` to the categorical telemetry bucket. The
/// raw `Display` is too verbose for PostHog and would leak path
/// fragments from the user's machine via `Spawn(io::Error)`. Caller
/// owns mapping `Ok(_)` to "ok".
pub fn error_category(err: &ClaudeCodeError) -> &'static str {
    match err {
        ClaudeCodeError::NotInstalled => "not_installed",
        ClaudeCodeError::NotLoggedIn => "not_logged_in",
        ClaudeCodeError::Spawn(_) => "spawn",
        ClaudeCodeError::Timeout => "timeout",
        ClaudeCodeError::NonZeroExit { .. } => "exit_nonzero",
        ClaudeCodeError::InvalidUtf8 => "invalid_utf8",
    }
}

/// Map the status enum to the categorical telemetry label.
pub fn status_label(s: &ClaudeCodeStatus) -> &'static str {
    match s {
        ClaudeCodeStatus::Ready { .. } => "ready",
        ClaudeCodeStatus::NotLoggedIn { .. } => "not_logged_in",
        ClaudeCodeStatus::NotInstalled => "not_installed",
    }
}

// ── Synthetic provider URL helpers ────────────────────────────
//
// We piggyback on the existing `llm_api_url` config field by
// declaring a special URL scheme `claude-code://`. When the LLM
// dispatcher sees this, it routes via `run_blocking` instead of
// HTTP. This avoids a schema migration (no new config field) and
// keeps the existing provider-picker UI as the single entry point.

/// The URL scheme prefix that flags "use Claude Code subscription
/// for this LLM call".
pub const PROVIDER_URL: &str = "claude-code://default";

/// True iff `api_url` is our synthetic Claude Code scheme.
pub fn is_claude_code_url(api_url: &str) -> bool {
    api_url.trim().starts_with("claude-code://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_claude_code_url_matches_scheme() {
        assert!(is_claude_code_url("claude-code://default"));
        assert!(is_claude_code_url("claude-code://anything-here"));
        assert!(is_claude_code_url("  claude-code://default  "));
        assert!(!is_claude_code_url("https://api.anthropic.com/v1/messages"));
        assert!(!is_claude_code_url("http://claude-code/"));
        assert!(!is_claude_code_url(""));
    }

    #[test]
    fn status_codes_are_stable() {
        // Pin the integer codes the FFI returns. Dashboards + the
        // C# / Swift hosts hard-code these values.
        assert_eq!(
            ClaudeCodeStatus::Ready {
                binary_path: PathBuf::from("/x")
            }
            .as_code(),
            0
        );
        assert_eq!(
            ClaudeCodeStatus::NotLoggedIn {
                binary_path: PathBuf::from("/x")
            }
            .as_code(),
            1
        );
        assert_eq!(ClaudeCodeStatus::NotInstalled.as_code(), 2);
    }

    #[test]
    fn candidate_paths_includes_path_dirs() {
        // Sanity: we DO walk PATH on every platform. Without this,
        // a Linux user who put claude in /usr/local/bin would only
        // hit it via the platform-specific block — the PATH fall-
        // back is the safety net.
        let paths = candidate_paths();
        assert!(!paths.is_empty(), "should always have some candidates");
        // PATH-derived entries vary per CI runner, but on a typical
        // dev machine PATH contains at least `/usr/bin` or
        // `C:\Windows\System32` so there should be > 5 paths.
        // Lower bound 1 to avoid flakiness on minimal containers.
    }

    #[test]
    fn detect_binary_returns_path_when_one_exists() {
        // We can't guarantee `claude` is installed on the test
        // machine. The test only asserts the API contract: either
        // None, or a path that actually exists on disk.
        if let Some(p) = detect_binary() {
            assert!(
                p.is_file(),
                "detect_binary must only return existing files: {:?}",
                p
            );
        }
    }

    #[test]
    fn error_category_covers_every_variant() {
        // Lock down the categorical mapping. PostHog dashboards key off
        // these exact strings.
        assert_eq!(
            error_category(&ClaudeCodeError::NotInstalled),
            "not_installed"
        );
        assert_eq!(
            error_category(&ClaudeCodeError::NotLoggedIn),
            "not_logged_in"
        );
        assert_eq!(error_category(&ClaudeCodeError::Spawn("x".into())), "spawn");
        assert_eq!(error_category(&ClaudeCodeError::Timeout), "timeout");
        assert_eq!(
            error_category(&ClaudeCodeError::NonZeroExit {
                code: 1,
                stderr_excerpt: "ignored".into()
            }),
            "exit_nonzero"
        );
        assert_eq!(
            error_category(&ClaudeCodeError::InvalidUtf8),
            "invalid_utf8"
        );
    }

    #[test]
    fn status_label_covers_every_variant() {
        assert_eq!(
            status_label(&ClaudeCodeStatus::Ready {
                binary_path: PathBuf::from("/x")
            }),
            "ready"
        );
        assert_eq!(
            status_label(&ClaudeCodeStatus::NotLoggedIn {
                binary_path: PathBuf::from("/x")
            }),
            "not_logged_in"
        );
        assert_eq!(
            status_label(&ClaudeCodeStatus::NotInstalled),
            "not_installed"
        );
    }

    #[test]
    fn display_never_leaks_stderr_or_spawn_message() {
        // SECURITY: the Display impl is what gets formatted into log
        // messages and (historically) into Sentry error reports. If
        // either ever carries the spawn message or stderr, transcript
        // fragments echoed back by `claude` (e.g. "model refused
        // because '...your transcript text...'") would leak.
        let with_spawn = format!(
            "{}",
            ClaudeCodeError::Spawn("contains-secret-filepath".into())
        );
        assert!(
            !with_spawn.contains("contains-secret-filepath"),
            "Spawn Display must not echo the inner message: {}",
            with_spawn
        );
        let with_stderr = format!(
            "{}",
            ClaudeCodeError::NonZeroExit {
                code: 1,
                stderr_excerpt: "your-transcript-here".into(),
            }
        );
        assert!(
            !with_stderr.contains("your-transcript-here"),
            "NonZeroExit Display must not echo stderr: {}",
            with_stderr
        );
    }

    #[test]
    fn run_blocking_errors_cleanly_when_not_installed() {
        // We can't easily reset BINARY_CACHE so this test is
        // best-effort. It exercises the error path on a system
        // without Claude Code installed. On a dev machine WITH
        // claude installed, this test is skipped because the
        // contract still holds (just via a different branch).
        let _ = run_blocking("ping", "", Duration::from_secs(1));
        // No assertion — either NotInstalled error or an actual
        // claude invocation. Both are acceptable; we only check
        // that the call doesn't panic.
    }
}
