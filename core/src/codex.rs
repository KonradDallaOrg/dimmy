//! OpenAI Codex CLI integration — use the user's ChatGPT
//! subscription (Plus / Pro / Team / Enterprise) for LLM calls
//! instead of consuming API-key credits.
//!
//! Architecture
//! ============
//! Direct sibling of `claude_code.rs`. OpenAI ships the official
//! `codex` CLI, which handles browser-based login → stores credentials
//! under `$CODEX_HOME` (default `~/.codex/`) → exposes a non-interactive
//! `codex exec` mode that prints only the final assistant message to
//! stdout. We piggyback on that exactly as we do for `claude --print`:
//!   1. **Detect**: locate the `codex` binary on PATH / common installs.
//!   2. **Login**: spawn `codex login`; Codex opens the browser, does
//!      the ChatGPT OAuth, writes its own credentials. We're passive.
//!   3. **Invoke**: spawn `codex exec` with the prompt on stdin and read
//!      stdout. Codex streams progress to stderr and prints only the
//!      final message to stdout, so stdout == the response.
//!
//! Why the specific exec flags (verified against OpenAI's CLI reference,
//! 2026-06-13):
//!   - `exec -`           : read the prompt from stdin (not argv — the
//!                          transcript can be megabytes and argv has OS
//!                          length limits + leaks via `ps`).
//!   - `--skip-git-repo-check` : Codex refuses to run outside a git repo
//!                          by default. Our recap/rewrite has no repo.
//!   - `--sandbox read-only`   : Codex is a *coding* agent; this is a
//!                          pure text task, so deny all file writes +
//!                          command exec. read-only still lets it answer.
//!   - `--skip-git-repo-check` + neutral temp cwd → it never touches the
//!                          user's project.
//!   - `-m <model>`       : override the model when the caller pins one.
//!
//! Privacy + safety (identical posture to claude_code.rs)
//! ------------------------------------------------------
//! - No tokens leave Rust. We never read `~/.codex/auth.json`; the CLI
//!   is the only consumer of that file.
//! - Prompt goes on stdin only, never argv.
//! - Timeout (default 5 min) so a runaway model doesn't pin a thread.
//! - Stderr captured separately, logged locally, never to telemetry.
//! - `Display` for the error type redacts spawn messages + stderr so a
//!   transcript fragment echoed by the CLI can't leak.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::Duration;

/// Status of the local Codex CLI install. Integer codes are pinned by
/// `as_code()` and consumed by the C# / Swift hosts — do not renumber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexStatus {
    /// `codex` binary found AND credentials present. Ready to dispatch.
    Ready { binary_path: PathBuf },
    /// Binary found but no credentials — user needs to run `codex login`.
    NotLoggedIn { binary_path: PathBuf },
    /// Binary not found. User must install the Codex CLI.
    NotInstalled,
}

impl CodexStatus {
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

/// Cache the binary location so `status()` doesn't re-walk the
/// filesystem on every call. The setup wizard invalidates this via
/// `clear_cache()` after the user reports an install / login completed.
///
/// State encoding mirrors claude_code.rs:
///   - `None`             → never resolved (cold)
///   - `Some(None)`       → resolved: binary not present
///   - `Some(Some(path))` → resolved: binary at `path`
static BINARY_CACHE: RwLock<Option<Option<PathBuf>>> = RwLock::new(None);

/// Reset the cached lookup. Call after a successful install / login so
/// the next status check re-walks the filesystem.
pub fn clear_cache() {
    if let Ok(mut g) = BINARY_CACHE.write() {
        *g = None;
    }
}

/// `$CODEX_HOME` (default `~/.codex`). Honour the env override so a
/// user with a relocated Codex home is still detected.
fn codex_home() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("CODEX_HOME") {
        if !h.trim().is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

/// Common locations where the `codex` binary lives, cross-platform.
/// Codex ships two ways: a self-contained native binary (install
/// script / brew → `~/.codex/bin`, `/usr/local/bin`, `/opt/homebrew/bin`)
/// and an npm package (`@openai/codex` → the per-user Node-manager bin
/// dirs). We enumerate both, then fall back to a PATH walk.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    let push_variants = |paths: &mut Vec<PathBuf>, dir: PathBuf| {
        #[cfg(target_os = "windows")]
        {
            paths.push(dir.join("codex.cmd"));
            paths.push(dir.join("codex.exe"));
            paths.push(dir.join("codex"));
        }
        #[cfg(not(target_os = "windows"))]
        {
            paths.push(dir.join("codex"));
        }
    };

    // Codex's own install dir.
    if let Some(home) = codex_home() {
        push_variants(&mut paths, home.join("bin"));
    }

    if let Some(home) = dirs::home_dir() {
        // XDG user-bin (install script default on Mac/Linux).
        #[cfg(not(target_os = "windows"))]
        push_variants(&mut paths, home.join(".local").join("bin"));

        // npm custom global prefix.
        push_variants(&mut paths, home.join(".npm-global").join("bin"));
        // Yarn / Volta global.
        push_variants(&mut paths, home.join(".yarn").join("bin"));
        push_variants(&mut paths, home.join(".volta").join("bin"));

        // nvm — each Node version has its own bin/.
        let nvm_root = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                push_variants(&mut paths, entry.path().join("bin"));
            }
        }
        // fnm.
        let fnm_root = home.join(".fnm").join("node-versions");
        if let Ok(entries) = std::fs::read_dir(&fnm_root) {
            for entry in entries.flatten() {
                #[cfg(target_os = "windows")]
                push_variants(&mut paths, entry.path().join("installation"));
                #[cfg(not(target_os = "windows"))]
                push_variants(&mut paths, entry.path().join("installation").join("bin"));
            }
        }
        // asdf (Mac/Linux).
        #[cfg(not(target_os = "windows"))]
        {
            let asdf_node = home.join(".asdf").join("installs").join("nodejs");
            if let Ok(entries) = std::fs::read_dir(&asdf_node) {
                for entry in entries.flatten() {
                    push_variants(&mut paths, entry.path().join("bin"));
                }
            }
        }
        // pnpm.
        #[cfg(target_os = "macos")]
        push_variants(&mut paths, home.join("Library").join("pnpm"));
        #[cfg(target_os = "linux")]
        push_variants(&mut paths, home.join(".local").join("share").join("pnpm"));
    }

    // Platform-typical system install dirs.
    #[cfg(target_os = "macos")]
    {
        push_variants(&mut paths, PathBuf::from("/opt/homebrew/bin"));
        push_variants(&mut paths, PathBuf::from("/usr/local/bin"));
        push_variants(&mut paths, PathBuf::from("/usr/bin"));
    }
    #[cfg(target_os = "linux")]
    {
        push_variants(&mut paths, PathBuf::from("/usr/local/bin"));
        push_variants(&mut paths, PathBuf::from("/usr/bin"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            push_variants(&mut paths, PathBuf::from(&local_app_data).join("npm"));
        }
        if let Ok(app_data) = std::env::var("APPDATA") {
            push_variants(&mut paths, PathBuf::from(&app_data).join("npm"));
        }
    }

    // PATH walk — last resort (each entry is stat'd by the caller).
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            push_variants(&mut paths, dir);
        }
    }

    paths
}

/// Mac/Linux fallback: ask the user's LOGIN shell where `codex` is.
/// macOS GUI apps inherit only the system base PATH, so a Codex
/// installed behind a custom shell config is invisible to a
/// GUI-launched Dimmy.app unless we ask the login shell. Same
/// security discipline as claude_code.rs (whitelist + re-verify file).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn detect_via_login_shell() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let shell = "/bin/zsh";
    #[cfg(target_os = "linux")]
    let shell = "/bin/bash";

    let output = Command::new(shell)
        .args(["-l", "-c", "command -v codex 2>/dev/null"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let p = PathBuf::from(first);
    if p.is_file() {
        crate::log(&format!(
            "[Codex] login-shell fallback resolved codex at {:?}",
            p
        ));
        Some(p)
    } else {
        None
    }
}

/// Locate the `codex` binary. First candidate that exists on disk,
/// else (Mac/Linux) the login-shell fallback. Result cached.
pub fn detect_binary() -> Option<PathBuf> {
    if let Ok(g) = BINARY_CACHE.read() {
        if let Some(cached) = g.as_ref() {
            return cached.clone();
        }
    }
    let resolved = resolve_binary();
    if let Ok(mut g) = BINARY_CACHE.write() {
        *g = Some(resolved.clone());
    }
    resolved
}

fn resolve_binary() -> Option<PathBuf> {
    if let Some(p) = candidate_paths().into_iter().find(|c| c.is_file()) {
        return Some(p);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        detect_via_login_shell()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// True iff the Codex CLI has stored credentials. Codex writes its
/// OAuth token to `$CODEX_HOME/auth.json` after `codex login`. We only
/// probe for existence + non-empty — never parse it (the format is not
/// a public contract; the CLI is the sole authorised consumer).
pub fn has_credentials() -> bool {
    if let Some(home) = codex_home() {
        let p = home.join("auth.json");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.len() > 0 {
                return true;
            }
        }
    }
    false
}

/// Combine binary detection + credentials check into a single status
/// enum. Cheap — does NOT spawn a subprocess.
pub fn status() -> CodexStatus {
    match detect_binary() {
        None => CodexStatus::NotInstalled,
        Some(p) => {
            if has_credentials() {
                CodexStatus::Ready { binary_path: p }
            } else {
                CodexStatus::NotLoggedIn { binary_path: p }
            }
        }
    }
}

/// Diagnostic snapshot — JSON with the path-search trace + credential
/// presence. Logs no content (paths + booleans only).
pub fn diagnostics_json() -> String {
    let candidates: Vec<serde_json::Value> = candidate_paths()
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "path": p.to_string_lossy(),
                "exists": p.is_file(),
            })
        })
        .collect();

    let resolved = candidate_paths().into_iter().find(|c| c.is_file());
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let via_shell = if resolved.is_none() {
        detect_via_login_shell()
    } else {
        None
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let via_shell: Option<PathBuf> = None;

    let final_resolved = resolved.as_ref().or(via_shell.as_ref());

    serde_json::json!({
        "resolved": final_resolved.map(|p| p.to_string_lossy().into_owned()),
        "via_login_shell": via_shell.is_some(),
        "candidates": candidates,
        "credentials_present": has_credentials(),
    })
    .to_string()
}

/// Errors surfaced from the `run` path. Narrow on purpose; the caller
/// wraps these for the existing LLM dispatch.
#[derive(Debug)]
pub enum CodexError {
    NotInstalled,
    NotLoggedIn,
    Spawn(String),
    Timeout,
    NonZeroExit { code: i32, stderr_excerpt: String },
    InvalidUtf8,
}

impl std::fmt::Display for CodexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "Codex CLI not installed"),
            Self::NotLoggedIn => write!(f, "Codex not logged in — run `codex login`"),
            // SECURITY: never embed the spawn message (may include path
            // fragments) or stderr (may echo transcript text). The local
            // dimmy.log gets the detail; telemetry sees only the category.
            Self::Spawn(_) => write!(f, "Codex spawn failed"),
            Self::Timeout => write!(f, "Codex call timed out"),
            Self::NonZeroExit { code, .. } => write!(f, "Codex exit code {}", code),
            Self::InvalidUtf8 => write!(f, "Codex stdout was not UTF-8"),
        }
    }
}

impl std::error::Error for CodexError {}

/// Run a single Codex invocation. Synchronous — blocks the calling
/// thread for up to `timeout`. Async callers spawn this on a thread.
///
/// `model` is passed via `-m` if non-empty; empty = Codex's configured
/// default. `prompt` is written to stdin (`codex exec -`), never argv.
pub fn run_blocking(prompt: &str, model: &str, timeout: Duration) -> Result<String, CodexError> {
    let binary = match detect_binary() {
        Some(p) => p,
        None => return Err(CodexError::NotInstalled),
    };
    if !has_credentials() {
        return Err(CodexError::NotLoggedIn);
    }

    let mut cmd = Command::new(&binary);
    cmd.arg("exec");
    // Pure text task: deny file writes + command exec, and allow running
    // outside a git repo (recap/rewrite has none).
    cmd.arg("--sandbox");
    cmd.arg("read-only");
    cmd.arg("--skip-git-repo-check");
    if !model.is_empty() {
        cmd.arg("-m");
        cmd.arg(model);
    }
    // Read the prompt from stdin.
    cmd.arg("-");

    // Run in the system temp dir so Codex never inspects the user's
    // current project (it's a coding agent; a neutral cwd + read-only
    // sandbox keeps it a pure text transformer).
    cmd.current_dir(std::env::temp_dir());

    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Suppress the console window flash on Windows (Dimmy is a GUI app).
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    crate::log(&format!(
        "[Codex] spawn binary={:?} model={:?} prompt_chars={}",
        binary,
        model,
        prompt.len()
    ));

    let mut child = cmd
        .spawn()
        .map_err(|e| CodexError::Spawn(format!("{}", e)))?;

    // Pipe prompt to stdin, then drop the handle so Codex sees EOF.
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(prompt.as_bytes()) {
            let _ = child.kill();
            return Err(CodexError::Spawn(format!("stdin write: {}", e)));
        }
        drop(stdin);
    } else {
        let _ = child.kill();
        return Err(CodexError::Spawn("no stdin handle".into()));
    }

    // Poll with timeout (std Child has no timed wait). 100 ms keeps CPU
    // near zero while terminating within a tick of completion.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(CodexError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(CodexError::Spawn(format!("wait: {}", e)));
            }
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| CodexError::Spawn(format!("collect: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let excerpt: String = stderr.chars().take(500).collect();
        crate::log(&format!(
            "[Codex] non-zero exit code={:?} stderr={}",
            output.status.code(),
            excerpt
        ));
        return Err(CodexError::NonZeroExit {
            code: output.status.code().unwrap_or(-1),
            stderr_excerpt: excerpt,
        });
    }

    let text = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Err(CodexError::InvalidUtf8),
    };

    crate::log(&format!(
        "[Codex] success prompt_chars={} response_chars={}",
        prompt.len(),
        text.len()
    ));
    Ok(text)
}

/// Spawn `codex login` in a visible terminal so the user sees the URL
/// prompt. Returns once spawned; the user completes the browser flow.
/// Caller re-checks `status()` afterwards. Mirrors claude_code::spawn_login.
pub fn spawn_login() -> Result<(), CodexError> {
    let binary = match detect_binary() {
        Some(p) => p,
        None => return Err(CodexError::NotInstalled),
    };

    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/c");
        cmd.arg("start");
        cmd.arg(""); // empty window title
        cmd.arg(&binary);
        cmd.arg("login");
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| CodexError::Spawn(format!("{}", e)))?;
    }

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\" to do script \"{} login\"",
            binary.display()
        );
        let mut cmd = Command::new("osascript");
        cmd.arg("-e").arg(&script);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| CodexError::Spawn(format!("{}", e)))?;
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new(&binary);
        cmd.arg("login");
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| CodexError::Spawn(format!("{}", e)))?;
    }

    crate::log("[Codex] login subprocess spawned");
    Ok(())
}

/// Categorical telemetry bucket for a `CodexError`. The raw `Display` is
/// already redacted, but keep an explicit mapping so PostHog dashboards
/// key off stable strings.
pub fn error_category(err: &CodexError) -> &'static str {
    match err {
        CodexError::NotInstalled => "not_installed",
        CodexError::NotLoggedIn => "not_logged_in",
        CodexError::Spawn(_) => "spawn",
        CodexError::Timeout => "timeout",
        CodexError::NonZeroExit { .. } => "exit_nonzero",
        CodexError::InvalidUtf8 => "invalid_utf8",
    }
}

/// Map the status enum to the categorical telemetry label.
pub fn status_label(s: &CodexStatus) -> &'static str {
    match s {
        CodexStatus::Ready { .. } => "ready",
        CodexStatus::NotLoggedIn { .. } => "not_logged_in",
        CodexStatus::NotInstalled => "not_installed",
    }
}

// ── Synthetic provider URL helpers ────────────────────────────
//
// Same trick as claude-code://: we overload the `llm_api_url` config
// field with a `codex://` scheme. The LLM dispatcher routes these via
// `run_blocking` instead of HTTP — no schema migration, the existing
// provider picker stays the single entry point.

/// The URL scheme that flags "use the Codex / ChatGPT subscription".
pub const PROVIDER_URL: &str = "codex://default";

/// True iff `api_url` is our synthetic Codex scheme.
pub fn is_codex_url(api_url: &str) -> bool {
    api_url.trim().starts_with("codex://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_codex_url_matches_scheme() {
        assert!(is_codex_url("codex://default"));
        assert!(is_codex_url("codex://anything"));
        assert!(is_codex_url("  codex://default  "));
        assert!(!is_codex_url("https://api.openai.com/v1/chat/completions"));
        assert!(!is_codex_url("claude-code://default"));
        assert!(!is_codex_url(""));
    }

    #[test]
    fn status_codes_are_stable() {
        // Pin the integer codes the FFI returns; hosts hard-code these.
        assert_eq!(
            CodexStatus::Ready {
                binary_path: PathBuf::from("/x")
            }
            .as_code(),
            0
        );
        assert_eq!(
            CodexStatus::NotLoggedIn {
                binary_path: PathBuf::from("/x")
            }
            .as_code(),
            1
        );
        assert_eq!(CodexStatus::NotInstalled.as_code(), 2);
    }

    #[test]
    fn error_category_covers_every_variant() {
        assert_eq!(error_category(&CodexError::NotInstalled), "not_installed");
        assert_eq!(error_category(&CodexError::NotLoggedIn), "not_logged_in");
        assert_eq!(error_category(&CodexError::Spawn("x".into())), "spawn");
        assert_eq!(error_category(&CodexError::Timeout), "timeout");
        assert_eq!(
            error_category(&CodexError::NonZeroExit {
                code: 1,
                stderr_excerpt: "ignored".into()
            }),
            "exit_nonzero"
        );
        assert_eq!(error_category(&CodexError::InvalidUtf8), "invalid_utf8");
    }

    #[test]
    fn status_label_covers_every_variant() {
        assert_eq!(
            status_label(&CodexStatus::Ready {
                binary_path: PathBuf::from("/x")
            }),
            "ready"
        );
        assert_eq!(
            status_label(&CodexStatus::NotLoggedIn {
                binary_path: PathBuf::from("/x")
            }),
            "not_logged_in"
        );
        assert_eq!(status_label(&CodexStatus::NotInstalled), "not_installed");
    }

    #[test]
    fn display_never_leaks_stderr_or_spawn_message() {
        // SECURITY: the same leak guard as claude_code — a transcript
        // fragment echoed by the CLI must never reach the Display output
        // (which feeds logs + historically Sentry).
        let with_spawn = format!("{}", CodexError::Spawn("contains-secret-filepath".into()));
        assert!(
            !with_spawn.contains("contains-secret-filepath"),
            "Spawn Display must not echo the inner message: {}",
            with_spawn
        );
        let with_stderr = format!(
            "{}",
            CodexError::NonZeroExit {
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
    fn detect_binary_returns_path_when_one_exists() {
        // Contract: either None, or a path that actually exists on disk.
        if let Some(p) = detect_binary() {
            assert!(
                p.is_file(),
                "detect_binary must only return existing files: {:?}",
                p
            );
        }
    }

    #[test]
    fn candidate_paths_includes_codex_home_and_path() {
        let paths = candidate_paths();
        assert!(!paths.is_empty(), "should always have some candidates");
        let joined: String = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        // Codex's own install dir must be enumerated.
        assert!(
            joined.contains(".codex") || std::env::var("CODEX_HOME").is_ok(),
            "candidate_paths must include $CODEX_HOME/bin"
        );
    }

    #[test]
    fn diagnostics_json_is_parseable() {
        let s = diagnostics_json();
        let v: serde_json::Value =
            serde_json::from_str(&s).expect("diagnostics output must be valid JSON");
        assert!(v.get("resolved").is_some());
        assert!(v.get("candidates").is_some());
        assert!(v.get("credentials_present").is_some());
    }

    #[test]
    fn run_blocking_errors_cleanly_when_not_installed() {
        // Best-effort: on a machine without codex this exercises the
        // NotInstalled path; with codex it's a real call. Either is fine —
        // we only assert it doesn't panic.
        let _ = run_blocking("ping", "", Duration::from_secs(1));
    }
}
