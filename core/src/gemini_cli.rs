//! Google Gemini CLI integration — use the user's Google account (free
//! tier, AI Pro or AI Ultra) for LLM calls instead of consuming API-key
//! credits.
//!
//! Architecture
//! ============
//! Third sibling of `claude_code.rs` and `codex.rs`, and closest to the
//! latter. Google ships the official `gemini` CLI, which handles
//! browser-based login → caches credentials under `~/.gemini/` → exposes
//! a non-interactive mode that prints the answer to stdout. We piggyback
//! on that exactly as we do for `codex exec`:
//!   1. **Detect**: locate the `gemini` binary on PATH / common installs.
//!   2. **Login**: spawn `gemini` in a visible terminal; it shows the
//!      auth picker and opens the browser. We are passive.
//!   3. **Invoke**: pipe the prompt to stdin and read stdout.
//!
//! Why the desktop app is NOT what we integrate
//! --------------------------------------------
//! Google shipped a Gemini desktop app (macOS earlier in 2026, Windows on
//! 2026-09-10). It is a GUI with a global hotkey and publishes no local
//! API, so there is nothing to drive. The CLI is the integration surface,
//! the same way Claude Desktop is not how `claude_code.rs` works.
//!
//! Why these flags (verified against the CLI docs + source, 2026-09-12)
//! -------------------------------------------------------------------
//! - **prompt on stdin, never `-p`**: a recap prompt carries a whole
//!   transcript. argv has OS length limits and leaks via `ps`. The docs'
//!   own example is `echo "..." | gemini`.
//! - `--output-format json`: returns `{"response": ..., "stats": ...,
//!   "error": {...}}`. Strictly better than Codex, which prints bare text
//!   and leaves us guessing whether a short reply is an answer or a
//!   failure.
//! - `--approval-mode plan`: documented as read-only mode. Gemini CLI is
//!   an agent that can edit files and run shell commands; a recap needs
//!   none of that, and the blast radius of leaving it on is the user's
//!   whole disk. NEVER pass `--yolo` here.
//! - neutral temp cwd: so it never picks up the user's project, and never
//!   finds a `GEMINI.md` that would silently prepend someone else's
//!   instructions to our prompt.
//!
//! WHO THIS ACTUALLY WORKS FOR — read before promoting it
//! -------------------------------------------------------
//! **Personal Google accounts cannot use this.** On 2026-06-18 Google
//! ended Gemini CLI access for Gemini Code Assist for individuals, and
//! for Google AI Pro and Ultra with it. Signing in with a personal
//! account fails with "This client is no longer supported for Gemini
//! Code Assist for individuals", and no amount of reinstalling or
//! switching accounts changes that.
//!
//! What is left:
//!   - **Gemini Code Assist Standard / Enterprise** licences, i.e. a
//!     company seat. This is the real audience.
//!   - A paid API key — which is pointless through here, because
//!     `llm.rs` already calls the Gemini HTTP API directly, faster and
//!     without spawning a process.
//!
//! This module was built on 2026-09-12 on the belief that a personal
//! account still carried 1000 free requests a day. It did until June.
//! The figure came from blog posts that were three months stale, and our
//! own note from 2026-06-14 had already recorded the shutdown date.
//!
//! WHY THERE IS A GATE (`gemini_cli_enabled`, off by default)
//! -----------------------------------------------------------
//! Enterprise access is governed by the ORGANISATION's Google Cloud
//! agreement, not by anything we can read: the Antigravity Additional
//! Terms open with a carve-out saying that for Cloud / Enterprise access
//! "the terms below do not apply to you" and the admin's signed terms do
//! instead. So whether this is allowed is the customer's admin's call,
//! and the UI says so rather than implying we have cleared it.
//!
//! AND DO NOT "FIX" THIS BY SWITCHING TO ANTIGRAVITY
//! -------------------------------------------------
//! Antigravity is where Google points migrants, its headless mode is now
//! good (`agy -p --output-format json`, better than this CLI's), and it
//! is still the wrong answer. Antigravity Additional Terms §6:
//!
//!   "You must not abuse, harm, interfere with, or disrupt the Service.
//!    This includes, but is not limited to, using the Service in
//!    connection with products not provided by us."
//!
//! The sentence that matters is the second one. It is not about
//! credentials, so the "we never touch the token, we only drive Google's
//! own binary" argument — which is exactly what makes the Anthropic
//! integration legitimate — does not reach it: Dimmy is a product not
//! provided by Google. And the stated remedy falls on the USER's account,
//! with the incorporated Universal Terms allowing deletion of the whole
//! Google Account and a second strike being permanent. Google ran that
//! enforcement in Feb 2026 against paying subscribers, has never said
//! what the detector keys on, and left this precise question unanswered
//! when a developer asked it publicly in July 2026.
//!
//! Legal review 2026-09-12; earlier note 2026-06-14 said the same for a
//! reason that has since expired (headless was broken then). The reason
//! above has not expired.
//!
//! Privacy + safety (identical posture to codex.rs)
//! ------------------------------------------------
//! - No tokens leave Rust. We never read `~/.gemini/oauth_creds.json`;
//!   the CLI is the only consumer of that file.
//! - Prompt goes on stdin only, never argv.
//! - Timeout so a runaway model doesn't pin a thread.
//! - Stderr captured separately, logged locally, never to telemetry.
//! - `Display` for the error type redacts spawn messages + stderr so a
//!   transcript fragment echoed by the CLI can't leak.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::Duration;

/// Status of the local Gemini CLI install. Integer codes are pinned by
/// `as_code()` and consumed by the C# / Swift hosts — do not renumber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiStatus {
    /// `gemini` binary found AND credentials present. Ready to dispatch.
    Ready { binary_path: PathBuf },
    /// Binary found but no credentials — the user needs to sign in.
    NotLoggedIn { binary_path: PathBuf },
    /// Binary not found. The user must install the Gemini CLI.
    NotInstalled,
}

impl GeminiStatus {
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

/// Cache the binary location so `status()` doesn't re-walk the filesystem
/// on every call. The setup wizard invalidates this via `clear_cache()`
/// after the user reports an install / login completed.
///
/// State encoding mirrors codex.rs:
///   - `None`             → never resolved (cold)
///   - `Some(None)`       → resolved: binary not present
///   - `Some(Some(path))` → resolved: binary at `path`
static BINARY_CACHE: RwLock<Option<Option<PathBuf>>> = RwLock::new(None);

/// Reset the cached lookup. Call after a successful install / login so the
/// next status check re-walks the filesystem.
pub fn clear_cache() {
    if let Ok(mut g) = BINARY_CACHE.write() {
        *g = None;
    }
}

/// The CLI's config home: `~/.gemini`, or `$GEMINI_CLI_HOME` when set.
/// Both names come from the CLI's own `paths.ts` (`GEMINI_DIR`), not from
/// a blog post — the override exists and a user who set it would
/// otherwise read as "not logged in" forever.
fn gemini_home() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("GEMINI_CLI_HOME") {
        if !h.trim().is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    dirs::home_dir().map(|h| h.join(".gemini"))
}

/// Common locations where the `gemini` binary lives, cross-platform.
///
/// Gemini CLI ships as an npm package (`@google/gemini-cli`) and through
/// Homebrew, so the search is the Node-manager sweep rather than Codex's
/// standalone-installer dirs. Same shape as codex.rs so the two stay
/// comparable when one of them starts failing to detect.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    let push_variants = |paths: &mut Vec<PathBuf>, dir: PathBuf| {
        #[cfg(target_os = "windows")]
        {
            // npm global installs write a .cmd shim; the bare name is the
            // shell script, useless to CreateProcess.
            paths.push(dir.join("gemini.cmd"));
            paths.push(dir.join("gemini.exe"));
            paths.push(dir.join("gemini"));
        }
        #[cfg(not(target_os = "windows"))]
        {
            paths.push(dir.join("gemini"));
        }
    };

    if let Some(home) = dirs::home_dir() {
        push_variants(&mut paths, home.join(".local").join("bin"));
        // npm custom global prefix.
        push_variants(&mut paths, home.join(".npm-global").join("bin"));
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

    #[cfg(target_os = "windows")]
    {
        // The default npm global prefix on Windows.
        if let Ok(appdata) = std::env::var("APPDATA") {
            push_variants(&mut paths, PathBuf::from(appdata).join("npm"));
        }
    }
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

    // Whatever PATH this process inherited, last: an explicit install
    // location is a better answer than a shim we happen to see.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            push_variants(&mut paths, dir);
        }
    }

    paths
}

/// Resolve the binary, caching the answer.
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
    crate::log("[Gemini] resolve_binary: no gemini binary found in any candidate path");
    None
}

/// True iff the Gemini CLI has usable credentials.
///
/// Two shapes count, because both make the CLI work and a user with
/// either would be baffled to be told they are not signed in:
///   - `~/.gemini/oauth_creds.json` — the Google-account sign-in, which
///     is the point of this backend (free tier / AI Pro / Ultra quota).
///   - `GEMINI_API_KEY` / `GOOGLE_API_KEY` in the environment.
///
/// We only probe for existence + non-empty — never parse the file. Its
/// format is not a public contract and the CLI is its sole authorised
/// consumer.
pub fn has_credentials() -> bool {
    if let Some(home) = gemini_home() {
        let p = home.join("oauth_creds.json");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.len() > 0 {
                return true;
            }
        }
    }
    ["GEMINI_API_KEY", "GOOGLE_API_KEY"].iter().any(|k| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    })
}

/// Combine binary detection + credentials check into a single status
/// enum. Cheap — does NOT spawn a subprocess.
pub fn status() -> GeminiStatus {
    match detect_binary() {
        None => GeminiStatus::NotInstalled,
        Some(p) => {
            if has_credentials() {
                GeminiStatus::Ready { binary_path: p }
            } else {
                GeminiStatus::NotLoggedIn { binary_path: p }
            }
        }
    }
}

/// Diagnostic snapshot — JSON with the path-search trace + credential
/// presence. Logs no content (paths + booleans only).
pub fn diagnostics_json() -> String {
    let candidates: Vec<serde_json::Value> = candidate_paths()
        .into_iter()
        .take(60)
        .map(|p| {
            serde_json::json!({
                "path": p.to_string_lossy(),
                "exists": p.is_file(),
            })
        })
        .collect();

    let resolved = candidate_paths().into_iter().find(|c| c.is_file());

    serde_json::json!({
        "resolved": resolved.map(|p| p.to_string_lossy().into_owned()),
        "candidates": candidates,
        "credentials_present": has_credentials(),
        "config_home": gemini_home().map(|p| p.to_string_lossy().into_owned()),
    })
    .to_string()
}

/// Errors surfaced from the `run` path. Narrow on purpose; the caller
/// wraps these for the existing LLM dispatch.
#[derive(Debug)]
pub enum GeminiError {
    NotInstalled,
    NotLoggedIn,
    Spawn(String),
    Timeout,
    NonZeroExit {
        code: i32,
        stderr_excerpt: String,
    },
    InvalidUtf8,
    /// The CLI answered, and the answer was an error envelope.
    Reported(String),
    /// Exit 0 with nothing usable in it.
    EmptyResponse,
}

impl std::fmt::Display for GeminiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "Gemini CLI not installed"),
            Self::NotLoggedIn => write!(f, "Gemini CLI not signed in"),
            // SECURITY: never embed the spawn message (may include path
            // fragments) or stderr (may echo transcript text). The local
            // dimmy.log gets the detail; telemetry sees only the category.
            Self::Spawn(_) => write!(f, "Gemini spawn failed"),
            Self::Timeout => write!(f, "Gemini call timed out"),
            Self::NonZeroExit { code, .. } => write!(f, "Gemini exit code {}", code),
            Self::InvalidUtf8 => write!(f, "Gemini stdout was not UTF-8"),
            // The CLI's own error text: quota exhausted, model refused, and
            // so on. It is about the REQUEST, not the content, and the user
            // needs to read it to act on it.
            Self::Reported(msg) => write!(f, "Gemini: {}", msg),
            Self::EmptyResponse => write!(f, "Gemini returned an empty response"),
        }
    }
}

impl std::error::Error for GeminiError {}

/// Pull the answer out of `--output-format json`.
///
/// Shape: `{"response": "...", "stats": {...}}`, or an `error` object when
/// the call failed with exit 0 — which it does for quota and model errors,
/// so the status code alone is not enough to tell success from failure.
///
/// A body that is not JSON at all is treated as the answer: the flag is
/// young, and degrading to "the text it printed" beats failing a recap
/// over a format change.
fn parse_response(raw: &str) -> Result<String, GeminiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(GeminiError::EmptyResponse);
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(raw.to_string());
    };
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(GeminiError::Reported(msg.chars().take(300).collect()));
    }
    match v.get("response").and_then(|r| r.as_str()) {
        Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
        // Valid JSON, no response, no error: report it rather than pass an
        // empty string off as a recap.
        _ => Err(GeminiError::EmptyResponse),
    }
}

/// Run a single Gemini invocation. Synchronous — blocks the calling
/// thread for up to `timeout`. Async callers spawn this on a thread.
///
/// `model` is passed via `-m` if non-empty; empty = the CLI's configured
/// default. `prompt` is written to stdin, never argv.
pub fn run_blocking(prompt: &str, model: &str, timeout: Duration) -> Result<String, GeminiError> {
    assert!(!prompt.is_empty(), "gemini: prompt must not be empty");

    let binary = match detect_binary() {
        Some(p) => p,
        None => return Err(GeminiError::NotInstalled),
    };
    if !has_credentials() {
        return Err(GeminiError::NotLoggedIn);
    }

    let mut cmd = Command::new(&binary);
    // Read-only: this is a pure text task and the CLI is otherwise an
    // agent that can write files and run shell commands.
    cmd.arg("--approval-mode");
    cmd.arg("plan");
    cmd.arg("--output-format");
    cmd.arg("json");
    if !model.is_empty() {
        cmd.arg("-m");
        cmd.arg(model);
    }
    // No -p: the prompt goes on stdin. See the module header.

    // Run in the system temp dir so Gemini never inspects the user's
    // current project and never picks up a GEMINI.md that would prepend
    // instructions we did not write.
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
        "[Gemini] spawn binary={:?} model={:?} prompt_chars={}",
        binary,
        model,
        prompt.len()
    ));

    let mut child = cmd
        .spawn()
        .map_err(|e| GeminiError::Spawn(format!("{}", e)))?;

    // Pipe prompt to stdin, then drop the handle so Gemini sees EOF.
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(prompt.as_bytes()) {
            let _ = child.kill();
            return Err(GeminiError::Spawn(format!("stdin write: {}", e)));
        }
        drop(stdin);
    } else {
        let _ = child.kill();
        return Err(GeminiError::Spawn("no stdin handle".into()));
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
                    return Err(GeminiError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(GeminiError::Spawn(format!("wait: {}", e)));
            }
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| GeminiError::Spawn(format!("collect: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let excerpt: String = stderr.chars().take(500).collect();
        crate::log(&format!(
            "[Gemini] non-zero exit code={:?} stderr={}",
            output.status.code(),
            excerpt
        ));
        return Err(GeminiError::NonZeroExit {
            code: output.status.code().unwrap_or(-1),
            stderr_excerpt: excerpt,
        });
    }

    let raw = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Err(GeminiError::InvalidUtf8),
    };
    let text = parse_response(&raw)?;

    crate::log(&format!(
        "[Gemini] success prompt_chars={} response_chars={}",
        prompt.len(),
        text.len()
    ));
    Ok(text)
}

/// Spawn the CLI in a visible terminal so the user can pick an auth method
/// and complete the browser flow. Returns once spawned; the caller
/// re-checks `status()` afterwards.
///
/// There is no `gemini login` subcommand: a first run with no credentials
/// shows the auth picker itself, which is why this launches the CLI bare
/// rather than with a verb that does not exist.
pub fn spawn_login() -> Result<(), GeminiError> {
    let binary = match detect_binary() {
        Some(p) => p,
        None => return Err(GeminiError::NotInstalled),
    };

    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/c");
        cmd.arg("start");
        cmd.arg(""); // empty window title
        cmd.arg(&binary);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| GeminiError::Spawn(format!("{}", e)))?;
    }

    #[cfg(target_os = "macos")]
    {
        let command = format!("{}", binary.display());
        crate::run_in_new_terminal_window(&command, "gemini-login")
            .map_err(|e| GeminiError::Spawn(format!("{}", e)))?;
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new(&binary);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn()
            .map_err(|e| GeminiError::Spawn(format!("{}", e)))?;
    }

    crate::log("[Gemini] login subprocess spawned");
    crate::telemetry::track(crate::telemetry::Event::GeminiCliLoginSpawned);
    Ok(())
}

/// Categorical telemetry bucket for a `GeminiError`. The raw `Display` is
/// already redacted, but keep an explicit mapping so dashboards key off
/// stable strings.
pub fn error_category(err: &GeminiError) -> &'static str {
    match err {
        GeminiError::NotInstalled => "not_installed",
        GeminiError::NotLoggedIn => "not_logged_in",
        GeminiError::Spawn(_) => "spawn",
        GeminiError::Timeout => "timeout",
        GeminiError::NonZeroExit { .. } => "exit_nonzero",
        GeminiError::InvalidUtf8 => "invalid_utf8",
        GeminiError::Reported(_) => "reported",
        GeminiError::EmptyResponse => "empty",
    }
}

/// Map the status enum to the categorical telemetry label.
pub fn status_label(s: &GeminiStatus) -> &'static str {
    match s {
        GeminiStatus::Ready { .. } => "ready",
        GeminiStatus::NotLoggedIn { .. } => "not_logged_in",
        GeminiStatus::NotInstalled => "not_installed",
    }
}

// ── Synthetic provider URL helpers ────────────────────────────
//
// Same trick as claude-code:// and codex://: we overload the `llm_api_url`
// config field with a `gemini-cli://` scheme. The LLM dispatcher routes
// these via `run_blocking` instead of HTTP — no schema migration, the
// existing provider picker stays the single entry point.

/// The URL scheme that flags "use the Gemini CLI subscription".
///
/// `gemini-cli://`, not `gemini://`: the config already carries real
/// Google endpoints and a provider literally named Gemini, and a scheme
/// one character away from the API provider is a trap for whoever reads
/// the config next.
pub const PROVIDER_URL: &str = "gemini-cli://default";

/// True iff `api_url` is our synthetic Gemini CLI scheme.
pub fn is_gemini_cli_url(api_url: &str) -> bool {
    api_url.trim().starts_with("gemini-cli://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_gemini_cli_url_matches_scheme() {
        assert!(is_gemini_cli_url("gemini-cli://default"));
        assert!(is_gemini_cli_url("gemini-cli://anything"));
        assert!(is_gemini_cli_url("  gemini-cli://default  "));
        assert!(!is_gemini_cli_url("codex://default"));
        assert!(!is_gemini_cli_url("claude-code://default"));
        assert!(!is_gemini_cli_url(""));
    }

    /// The scheme must not collide with the real Gemini HTTP provider,
    /// which is the whole reason it is not called `gemini://`.
    #[test]
    fn the_real_gemini_api_is_not_mistaken_for_the_cli() {
        assert!(!is_gemini_cli_url(
            "https://generativelanguage.googleapis.com/v1beta/models"
        ));
        assert!(!is_gemini_cli_url("gemini://default"));
    }

    #[test]
    fn status_codes_are_stable() {
        // Pin the integer codes the FFI returns; hosts hard-code these.
        assert_eq!(
            GeminiStatus::Ready {
                binary_path: PathBuf::from("/x")
            }
            .as_code(),
            0
        );
        assert_eq!(
            GeminiStatus::NotLoggedIn {
                binary_path: PathBuf::from("/x")
            }
            .as_code(),
            1
        );
        assert_eq!(GeminiStatus::NotInstalled.as_code(), 2);
    }

    #[test]
    fn error_category_covers_every_variant() {
        assert_eq!(error_category(&GeminiError::NotInstalled), "not_installed");
        assert_eq!(error_category(&GeminiError::NotLoggedIn), "not_logged_in");
        assert_eq!(error_category(&GeminiError::Spawn("x".into())), "spawn");
        assert_eq!(error_category(&GeminiError::Timeout), "timeout");
        assert_eq!(
            error_category(&GeminiError::NonZeroExit {
                code: 1,
                stderr_excerpt: String::new()
            }),
            "exit_nonzero"
        );
        assert_eq!(error_category(&GeminiError::InvalidUtf8), "invalid_utf8");
        assert_eq!(
            error_category(&GeminiError::Reported("x".into())),
            "reported"
        );
        assert_eq!(error_category(&GeminiError::EmptyResponse), "empty");
    }

    #[test]
    fn parses_the_json_envelope() {
        let out = parse_response(r#"{"response":"ciao","stats":{"models":{}}}"#).unwrap();
        assert_eq!(out, "ciao");
    }

    /// The CLI reports quota and model errors with exit code 0 and an
    /// `error` object, so a success status is not proof of success.
    #[test]
    fn an_error_envelope_is_an_error_even_on_exit_zero() {
        let e = parse_response(r#"{"error":{"message":"quota exceeded","code":429}}"#).unwrap_err();
        assert_eq!(error_category(&e), "reported");
        assert!(format!("{e}").contains("quota exceeded"));
    }

    /// If the JSON shape ever changes under us, printing what it said
    /// beats failing the user's recap.
    #[test]
    fn non_json_output_is_passed_through() {
        assert_eq!(parse_response("just text").unwrap(), "just text");
    }

    #[test]
    fn empty_and_responseless_are_rejected() {
        assert_eq!(error_category(&parse_response("   ").unwrap_err()), "empty");
        assert_eq!(
            error_category(&parse_response(r#"{"stats":{}}"#).unwrap_err()),
            "empty"
        );
        assert_eq!(
            error_category(&parse_response(r#"{"response":"  "}"#).unwrap_err()),
            "empty"
        );
    }

    /// Redaction: a transcript fragment echoed on stderr must never reach
    /// the error string that telemetry and the UI can see.
    #[test]
    fn display_redacts_spawn_and_stderr() {
        let s = format!(
            "{}",
            GeminiError::Spawn("C:\\Users\\someone\\secret".into())
        );
        assert!(!s.contains("secret"), "spawn detail leaked: {s}");
        let e = format!(
            "{}",
            GeminiError::NonZeroExit {
                code: 2,
                stderr_excerpt: "the user said something private".into()
            }
        );
        assert!(!e.contains("private"), "stderr leaked: {e}");
    }
}
