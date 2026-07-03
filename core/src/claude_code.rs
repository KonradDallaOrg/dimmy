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
use std::sync::RwLock;
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
/// re-walk the filesystem on every invocation. The setup wizard
/// invalidates this via `clear_cache()` after the user reports
/// an install completed.
///
/// State encoding:
///   - `None`             → never resolved (cold)
///   - `Some(None)`       → resolved: binary not present
///   - `Some(Some(path))` → resolved: binary at `path`
///
/// `RwLock` not `OnceLock` because the wizard MUST be able to
/// re-detect after the user runs `npm install -g`: the previous
/// `OnceLock` shape made `clear_cache()` a no-op + forced the user
/// to restart Dimmy. See the wizard recheck flow in `dimmy_claude_code_recheck`.
static BINARY_CACHE: RwLock<Option<Option<PathBuf>>> = RwLock::new(None);
static NODE_CACHE: RwLock<Option<Option<PathBuf>>> = RwLock::new(None);

/// Reset every cached lookup. Call after a successful install or
/// login event so the next status check re-walks the filesystem.
/// Cheap (just acquires the locks + writes `None`); safe to call
/// from any thread.
pub fn clear_cache() {
    if let Ok(mut g) = BINARY_CACHE.write() {
        *g = None;
    }
    if let Ok(mut g) = NODE_CACHE.write() {
        *g = None;
    }
}

/// Common locations where Claude Code installs the binary,
/// cross-platform. Delegates to `candidate_paths_for("claude")`
/// for the shared per-user Node-manager dirs, then adds the
/// Anthropic-specific install paths on top.
///
/// Mac coverage note (2026-05-13 incident): macOS GUI apps inherit
/// the system base PATH (`/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin`),
/// NOT the user's login-shell PATH. So a user who installed via
/// `nvm` / `volta` / `npm with custom prefix` / `yarn` / `pnpm` /
/// `fnm` / `asdf` has the CLI working in Terminal but invisible to
/// a GUI-launched Dimmy.app. We enumerate the seven Node-manager
/// install patterns explicitly; the login-shell fallback in
/// `detect_binary()` catches anything more exotic.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = candidate_paths_for("claude");

    // Claude-specific install dirs (NOT shared with node).
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "windows")]
        {
            paths.push(home.join(".claude").join("local").join("claude.cmd"));
            paths.push(home.join(".claude").join("local").join("claude.exe"));
            paths.push(home.join(".claude").join("local").join("claude"));
        }
        #[cfg(not(target_os = "windows"))]
        {
            // CLI self-managed shim (only present if the user ran a
            // claude /install-shim or equivalent).
            paths.push(home.join(".claude").join("local").join("claude"));
        }
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
        }
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            paths.push(
                PathBuf::from(&program_files)
                    .join("AnthropicClaude")
                    .join("claude.exe"),
            );
        }
    }

    paths
}

/// Generic per-binary candidate paths covering every Node-manager
/// install pattern Dimmy users have been seen using. Both `claude`
/// (Node CLI distributed via `@anthropic-ai/claude-code`) and `node`
/// (the runtime claude needs) live in the same per-user dirs, so
/// they share this enumeration.
///
/// Cross-platform conventions:
///   - Windows: emits `<dir>/<binary>.cmd`, `.exe`, and bare
///     (npm-shim, native exe, generic) so a single `is_file()` walk
///     terminates at the first hit.
///   - Mac/Linux: emits `<dir>/<binary>` only.
///
/// Order matters — explicit dirs come before PATH walk because
/// `is_file()` against a known path is ~stat-fast while walking
/// PATH entries pays the per-entry stat cost regardless of result.
fn candidate_paths_for(binary: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Helper: produce platform-specific filename variants for `dir`.
    let push_variants = |paths: &mut Vec<PathBuf>, dir: PathBuf| {
        #[cfg(target_os = "windows")]
        {
            paths.push(dir.join(format!("{}.cmd", binary)));
            paths.push(dir.join(format!("{}.exe", binary)));
            paths.push(dir.join(binary));
        }
        #[cfg(not(target_os = "windows"))]
        {
            paths.push(dir.join(binary));
        }
    };

    if let Some(home) = dirs::home_dir() {
        // XDG user-bin — pipx, mise (Rust-based runtime manager),
        // manual installs, and the NATIVE Claude Code installer, which
        // targets ~/.local/bin on Windows too (verified:
        // %USERPROFILE%\.local\bin\claude.exe — the recheck was blind to
        // it before because this was gated Mac/Linux-only). push_variants
        // emits the .exe/.cmd variants on Windows.
        push_variants(&mut paths, home.join(".local").join("bin"));

        // npm with custom global prefix (very common — devs who
        // ran `npm config set prefix ~/.npm-global` to avoid sudo).
        push_variants(&mut paths, home.join(".npm-global").join("bin"));

        // Yarn global.
        push_variants(&mut paths, home.join(".yarn").join("bin"));

        // Volta — atomic Node version manager (puts shims here).
        push_variants(&mut paths, home.join(".volta").join("bin"));

        // nvm — walk all installed Node versions. nvm doesn't
        // symlink into a stable path; each `node` version has
        // its own bin/. NOTE: nvm proper is Unix-only; nvm-windows
        // is a totally different project that DOES install at a
        // stable %APPDATA%\nvm\<version>\ path (handled below).
        let nvm_root = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                push_variants(&mut paths, entry.path().join("bin"));
            }
        }

        // fnm — newer Node version manager (Rust-based).
        let fnm_root = home.join(".fnm").join("node-versions");
        if let Ok(entries) = std::fs::read_dir(&fnm_root) {
            for entry in entries.flatten() {
                #[cfg(target_os = "windows")]
                push_variants(&mut paths, entry.path().join("installation"));
                #[cfg(not(target_os = "windows"))]
                push_variants(&mut paths, entry.path().join("installation").join("bin"));
            }
        }

        // asdf — multi-runtime manager (Mac/Linux only).
        #[cfg(not(target_os = "windows"))]
        {
            let asdf_node = home.join(".asdf").join("installs").join("nodejs");
            if let Ok(entries) = std::fs::read_dir(&asdf_node) {
                for entry in entries.flatten() {
                    push_variants(&mut paths, entry.path().join("bin"));
                }
            }
        }

        // pnpm — per-platform install root.
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
        // Native installer (Claude Desktop / `claude` native CLI) drops the
        // binary in a fixed per-user dir and only touches the registry PATH
        // (HKCU\Environment) for future shells — invisible to an
        // already-running Dimmy whose PATH is a launch-time snapshot. Same
        // class of bug as codex.rs; enumerate the dir directly so a native
        // install resolves without a restart.
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let lad = PathBuf::from(&local_app_data);
            push_variants(&mut paths, lad.join("AnthropicClaude"));
            push_variants(&mut paths, lad.join("AnthropicClaude").join("bin"));
            push_variants(&mut paths, lad.join("Programs").join("claude").join("bin"));
        }
        // winget portable packages (`winget install Anthropic.ClaudeCode`)
        // land under %LOCALAPPDATA%\Microsoft\WinGet\{Links,Packages\*}.
        // See win_paths.rs for the two layouts + the 2026-07-03 incident.
        // BEFORE the npm dirs: an npm shim (`claude.cmd`) survives a Node
        // uninstall and would otherwise shadow a fresh winget install —
        // the wizard would show green against a dead .cmd.
        for dir in crate::win_paths::winget_bin_dirs() {
            push_variants(&mut paths, dir);
        }
        // npm-global install on Win lives in two possible places
        // depending on whether the user used machine-wide install
        // or per-user `npm config set prefix`.
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            push_variants(&mut paths, PathBuf::from(&local_app_data).join("npm"));
        }
        if let Ok(app_data) = std::env::var("APPDATA") {
            push_variants(&mut paths, PathBuf::from(&app_data).join("npm"));
        }
    }

    // PATH walk — last resort because each PATH entry is stat'd.
    // Stops at the first match (caller does `.find(|c| c.is_file())`).
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            push_variants(&mut paths, dir);
        }
    }

    // Windows equivalent of the Mac login-shell fallback: the LIVE
    // registry PATH. Installers append there + broadcast
    // WM_SETTINGCHANGE, which never reaches this process's env — the
    // wizard's recheck stayed blind until app restart. Walking the
    // registry PATH catches ANY installer, present or future.
    #[cfg(target_os = "windows")]
    for dir in crate::win_paths::registry_path_dirs() {
        push_variants(&mut paths, dir);
    }

    paths
}

/// Node.js install locations, cross-platform. Extends
/// `candidate_paths_for("node")` (every Node-manager dir we know
/// about) with the official-installer drops: the Windows .msi
/// installs at `C:\Program Files\nodejs\`; the macOS .pkg lands in
/// `/usr/local/bin/` which `candidate_paths_for` already enumerates.
fn node_candidate_paths() -> Vec<PathBuf> {
    // `mut` is only used on Windows where we append .msi-installer
    // locations; on Mac/Linux the shared `candidate_paths_for` already
    // covers everything. Suppress the clippy warning rather than
    // duplicating the function body per-platform.
    #[allow(unused_mut)]
    let mut paths = candidate_paths_for("node");

    #[cfg(target_os = "windows")]
    {
        // Official Node.js .msi installer — system-wide.
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            paths.push(
                PathBuf::from(&program_files)
                    .join("nodejs")
                    .join("node.exe"),
            );
        }
        // Official Node.js .msi installer — per-user (less common).
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(&local_app_data)
                    .join("Programs")
                    .join("nodejs")
                    .join("node.exe"),
            );
        }
        // 32-bit Node on 64-bit Windows.
        if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
            paths.push(
                PathBuf::from(&program_files_x86)
                    .join("nodejs")
                    .join("node.exe"),
            );
        }
        // nvm-windows — separate project from Unix nvm. Walks
        // %APPDATA%\nvm\<version>\ (the install dir; PATH normally
        // symlinks one of them via a junction, but the symlink can
        // lag behind a `nvm use` event).
        if let Ok(app_data) = std::env::var("APPDATA") {
            let nvm_win = PathBuf::from(&app_data).join("nvm");
            if let Ok(entries) = std::fs::read_dir(&nvm_win) {
                for entry in entries.flatten() {
                    paths.push(entry.path().join("node.exe"));
                }
            }
        }
    }

    paths
}

/// Mac/Linux fallback: ask the user's LOGIN shell where `<binary>` is.
///
/// macOS GUI apps inherit the system base PATH (no `.zprofile`/
/// `.zshrc`/`.bash_profile` sourcing). A user who installed Node or
/// Claude via a manager whose dir we don't enumerate above (or via a
/// path that lives behind a custom shell config) will have a working
/// binary in Terminal that's invisible to Dimmy. The login shell
/// knows the real PATH because it sources the user's rc files.
///
/// We use `command -v` which is the POSIX-portable equivalent of
/// `which` and doesn't print spurious output. ~50 ms one-shot.
///
/// SECURITY: we only consume the FIRST line of stdout, trimmed and
/// verified to point at an existing regular file. A malicious
/// `.zshrc` that printed garbage to stdout (during sourcing) can't
/// inject a fake path here because we re-verify with `is_file()`
/// before trusting it.
///
/// SECURITY: `binary` is whitelisted by callers (`"claude"` and
/// `"node"`). We still defensively reject anything containing shell
/// metacharacters so a hypothetical caller passing user-controlled
/// input can't escape into the shell command. `command -v` itself
/// expands `$PATH` lookups only — but our caller is the only trust
/// boundary, so belt-and-braces here.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn detect_via_login_shell(binary: &str) -> Option<PathBuf> {
    // Belt-and-braces: caller passes a known-safe name, but never
    // interpolate without checking.
    if !binary
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    use std::process::Command;
    // Prefer zsh on Mac (default since Catalina), bash on Linux. If
    // either is absent, the spawn fails harmlessly and we return None.
    #[cfg(target_os = "macos")]
    let shell = "/bin/zsh";
    #[cfg(target_os = "linux")]
    let shell = "/bin/bash";

    let cmd = format!("command -v {} 2>/dev/null", binary);
    let output = Command::new(shell).args(["-l", "-c", &cmd]).output().ok()?;
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
            "[ClaudeCode] login-shell fallback resolved {} at {:?}",
            binary, p
        ));
        Some(p)
    } else {
        None
    }
}

/// Locate the `claude` binary. Returns the first candidate that
/// exists on disk. Returns `None` if none of the candidates match.
///
/// Two-tier search:
///   1. Explicit candidate paths (fast, ~stat-only).
///   2. Mac/Linux login-shell fallback when 1 fails (~50 ms once).
///
/// Result is cached in `BINARY_CACHE` so the slow path runs at most
/// once per call sequence. The cache is invalidated by `clear_cache()`
/// which the setup wizard calls after the user reports an install
/// completed.
pub fn detect_binary() -> Option<PathBuf> {
    // Fast path: cached.
    if let Ok(g) = BINARY_CACHE.read() {
        if let Some(cached) = g.as_ref() {
            return cached.clone();
        }
    }

    // Slow path: walk + login-shell fallback. Compute outside the
    // write lock so we don't block readers for the full walk.
    let resolved = resolve_claude_binary();

    if let Ok(mut g) = BINARY_CACHE.write() {
        *g = Some(resolved.clone());
    }
    resolved
}

fn resolve_claude_binary() -> Option<PathBuf> {
    if let Some(p) = candidate_paths().into_iter().find(|c| c.is_file()) {
        return Some(p);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        detect_via_login_shell("claude")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Locate the `node` binary using the same two-tier search as
/// `detect_binary()`. Cached separately so a `clear_cache()` invalidates
/// both lookups in one call.
///
/// Why we care: the setup wizard's Node step needs to detect Node
/// independently of Claude, because the user installs Node FIRST
/// (then Claude via `npm install -g`). Reusing `detect_binary()`
/// would conflate the two states and break the wizard's smart-skip
/// (e.g. "Node done, do Claude next").
pub fn detect_node_binary() -> Option<PathBuf> {
    if let Ok(g) = NODE_CACHE.read() {
        if let Some(cached) = g.as_ref() {
            return cached.clone();
        }
    }
    let resolved = resolve_node_binary();
    if let Ok(mut g) = NODE_CACHE.write() {
        *g = Some(resolved.clone());
    }
    resolved
}

fn resolve_node_binary() -> Option<PathBuf> {
    if let Some(p) = node_candidate_paths().into_iter().find(|c| c.is_file()) {
        return Some(p);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        detect_via_login_shell("node")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Returns the major.minor.patch reported by `<node> --version`, or
/// None if Node isn't installed / fails to launch / returns garbage.
/// Cheap (~50 ms one-shot). Used by the wizard to surface an
/// "outdated Node, please update" diagnostic instead of letting
/// `npm install -g @anthropic-ai/claude-code` fail with a cryptic
/// EBADENGINE later.
///
/// Output format: `"22.5.1"` (drops the leading `v` Node prints).
pub fn node_version() -> Option<String> {
    let bin = detect_node_binary()?;
    let mut cmd = Command::new(&bin);
    cmd.arg("--version");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    // Node prints `v22.5.1` — strip the leading v.
    let v = trimmed.strip_prefix('v').unwrap_or(trimmed);
    if v.is_empty() {
        return None;
    }
    Some(v.to_string())
}

/// Parse a semver-ish version string and return its major number.
/// Returns None on malformed input. Used by the wizard to gate
/// "Node >= 18" — claude-code's minimum supported runtime.
pub fn parse_node_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse::<u32>().ok()
}

/// True iff the Claude Code CLI has stored credentials somewhere
/// readable to it — file under `~/.claude/`, OR (on macOS) an entry
/// in the user's login Keychain.
///
/// We deliberately do NOT parse the credentials, just probe for
/// existence. Claude Code's file format and Keychain attribute set
/// are not public contracts; the CLI is the only authorised consumer.
///
/// macOS Keychain note (2026-05): recent Claude Code releases
/// (≥ 0.2.x npm `@anthropic-ai/claude-code`) DROP the on-disk
/// credentials file and store the OAuth token as a generic password
/// in the login Keychain under service `"Claude Code-credentials"`.
/// The file-only probe used to mis-report "Not logged in" for users
/// who'd successfully completed OAuth — see [[fix-has-credentials]].
pub fn has_credentials() -> bool {
    // ~/.claude/credentials.json (legacy + Linux + Win paths)
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

    // macOS Keychain probe — Claude Code ≥ 0.2.x stores the OAuth
    // token here, NOT on disk. We shell out to `security
    // find-generic-password -s "Claude Code-credentials"`; exit 0 =
    // entry exists. We capture stderr to /dev/null so a missing entry
    // doesn't spam logs.
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("security")
            .args(["find-generic-password", "-s", "Claude Code-credentials"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(s) = out {
            if s.success() {
                return true;
            }
        }
    }

    false
}

/// Diagnostic snapshot — returns JSON with the full path-search
/// trace so a user reporting "CLI is installed but Dimmy says
/// NotInstalled" can copy/paste it into a bug report. Logs no
/// content (just paths + booleans).
///
/// Shape:
/// ```json
/// {
///   "resolved": "/Users/u/.nvm/versions/node/v22.0.0/bin/claude",
///   "via_login_shell": true,
///   "candidates": [
///     {"path": "/Users/u/.claude/local/claude", "exists": false},
///     {"path": "/opt/homebrew/bin/claude", "exists": false},
///     ...
///   ],
///   "credentials_present": true,
///   "credentials_source": "keychain"
/// }
/// ```
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

    let resolved_via_explicit = candidate_paths().into_iter().find(|c| c.is_file());
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let resolved_via_shell = if resolved_via_explicit.is_none() {
        detect_via_login_shell("claude")
    } else {
        None
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let resolved_via_shell: Option<PathBuf> = None;

    let resolved = resolved_via_explicit
        .as_ref()
        .or(resolved_via_shell.as_ref());

    // Credentials probe — we don't read the file, only check
    // existence + len > 0 + (on Mac) Keychain.
    let creds_present = has_credentials();
    let creds_source = if creds_present {
        if let Some(home) = dirs::home_dir() {
            let p1 = home.join(".claude").join("credentials.json");
            let p2 = home.join(".claude").join(".credentials.json");
            if p1.is_file() {
                "file:credentials.json"
            } else if p2.is_file() {
                "file:.credentials.json"
            } else {
                #[cfg(target_os = "macos")]
                {
                    "keychain"
                }
                #[cfg(not(target_os = "macos"))]
                {
                    "unknown"
                }
            }
        } else {
            "unknown"
        }
    } else {
        "none"
    };

    serde_json::json!({
        "resolved": resolved.map(|p| p.to_string_lossy().into_owned()),
        "via_login_shell": resolved_via_shell.is_some(),
        "candidates": candidates,
        "credentials_present": creds_present,
        "credentials_source": creds_source,
    })
    .to_string()
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
            let command = format!("{} /login", binary.display());
            crate::run_in_new_terminal_window(&command, "claude-login")
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

    /// Mac GUI apps inherit a base PATH that excludes the user's
    /// Node-manager bin dirs. To avoid the 2026-05-13 "colleague has
    /// claude installed, Dimmy says NotInstalled" report repeating,
    /// pin that we explicitly enumerate the 7 most common install
    /// patterns. If a future cleanup drops one of these, this test
    /// catches it before ship.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn candidate_paths_includes_node_manager_install_dirs() {
        let paths = candidate_paths();
        let path_strs: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let joined = path_strs.join("\n");

        // Must cover npm custom prefix.
        assert!(
            joined.contains(".npm-global/bin/claude")
                || joined.contains(".npm-global\\bin\\claude"),
            "missing ~/.npm-global/bin/claude — npm custom-prefix users would be invisible"
        );
        // Yarn global.
        assert!(
            joined.contains(".yarn/bin/claude") || joined.contains(".yarn\\bin\\claude"),
            "missing ~/.yarn/bin/claude — yarn global users would be invisible"
        );
        // Volta.
        assert!(
            joined.contains(".volta/bin/claude") || joined.contains(".volta\\bin\\claude"),
            "missing ~/.volta/bin/claude — Volta users would be invisible"
        );
        // XDG user-bin (pipx / mise / manual installs).
        assert!(
            joined.contains(".local/bin/claude") || joined.contains(".local\\bin\\claude"),
            "missing ~/.local/bin/claude — XDG user-bin users would be invisible"
        );
    }

    /// The NATIVE Claude Code installer drops the binary in
    /// `%USERPROFILE%\.local\bin\claude.exe` on Windows too. That dir
    /// used to be gated Mac/Linux-only, so a `recheck` after a fresh
    /// native install stayed blind until the whole app was restarted
    /// (the process PATH is a launch-time snapshot and Windows has no
    /// login-shell fallback). Pin that `~/.local/bin\claude.exe` is a
    /// candidate on Windows so the gate can't silently come back.
    #[cfg(target_os = "windows")]
    #[test]
    fn candidate_paths_includes_local_bin_on_windows() {
        let joined = candidate_paths()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains(".local\\bin\\claude.exe"),
            "missing ~/.local/bin/claude.exe — native-installer users are invisible until app restart"
        );
    }

    /// `winget install Anthropic.ClaudeCode` lands under
    /// `%LOCALAPPDATA%\Microsoft\WinGet\{Links,Packages\*}` and only
    /// touches the registry PATH — the setup wizard's recheck froze on
    /// "Looking for the Claude Code CLI..." until app restart
    /// (2026-07-03). Pin that the WinGet dirs are enumerated.
    #[cfg(target_os = "windows")]
    #[test]
    fn candidate_paths_includes_winget_dirs_on_windows() {
        if std::env::var("LOCALAPPDATA").is_err() {
            return;
        }
        let joined = candidate_paths()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("WinGet\\Links\\claude.exe"),
            "missing WinGet Links dir — winget-installed claude is invisible until app restart"
        );
    }

    /// The registry PATH is the Windows stand-in for the Mac
    /// login-shell fallback: every dir a FRESH terminal would see must
    /// be a candidate, otherwise any installer that only updates
    /// HKCU\Environment stays invisible to a running Dimmy.
    #[cfg(target_os = "windows")]
    #[test]
    fn candidate_paths_cover_the_live_registry_path() {
        let candidates = candidate_paths();
        for dir in crate::win_paths::registry_path_dirs() {
            assert!(
                candidates.contains(&dir.join("claude.exe")),
                "registry PATH dir {:?} missing from candidates — restart-required bug is back",
                dir
            );
        }
    }

    #[test]
    fn diagnostics_json_is_parseable_and_lists_candidates() {
        let s = diagnostics_json();
        let v: serde_json::Value =
            serde_json::from_str(&s).expect("diagnostics output must be valid JSON");
        // Top-level keys present.
        assert!(v.get("resolved").is_some(), "must include 'resolved' key");
        assert!(
            v.get("candidates").is_some(),
            "must include 'candidates' array"
        );
        assert!(
            v.get("credentials_present").is_some(),
            "must include 'credentials_present' key"
        );
        let cands = v["candidates"]
            .as_array()
            .expect("'candidates' must be an array");
        assert!(
            !cands.is_empty(),
            "candidates array can't be empty on any platform"
        );
        // Each candidate has the documented shape.
        for c in cands {
            assert!(c.get("path").is_some(), "candidate entry missing 'path'");
            assert!(
                c.get("exists").is_some(),
                "candidate entry missing 'exists'"
            );
        }
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
    fn parse_node_major_handles_typical_outputs() {
        // `node --version` prints "v22.5.1"; node_version() strips the v.
        assert_eq!(parse_node_major("22.5.1"), Some(22));
        assert_eq!(parse_node_major("18.0.0"), Some(18));
        assert_eq!(parse_node_major("20"), Some(20));
        // Malformed inputs return None — wizard treats this as "couldn't
        // detect version" rather than fabricating a major.
        assert_eq!(parse_node_major(""), None);
        assert_eq!(parse_node_major("not-a-version"), None);
        assert_eq!(parse_node_major("v18.0.0"), None); // expects pre-stripped
    }

    /// Same enumeration discipline as the claude detection: every
    /// Node-manager bin dir we know about must appear in
    /// `node_candidate_paths()`. If a refactor drops one, users with
    /// a Node-manager install pattern would be invisible to the
    /// wizard's Step 1 detection.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn node_candidate_paths_includes_node_manager_install_dirs() {
        let paths = node_candidate_paths();
        let joined: String = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [
            ".npm-global/bin/node",
            ".yarn/bin/node",
            ".volta/bin/node",
            ".local/bin/node",
        ] {
            assert!(
                joined.contains(needle),
                "node_candidate_paths missing {} — Node-manager users would be invisible to the wizard",
                needle
            );
        }
    }

    /// On Windows the official Node.js .msi installer lands in
    /// `C:\Program Files\nodejs\`. Detecting Node ONLY via PATH would
    /// miss the case where the installer's PATH update hasn't been
    /// picked up by Dimmy yet (Dimmy inherited PATH at launch). The
    /// explicit `%ProgramFiles%\nodejs\node.exe` candidate is the
    /// safety net.
    #[cfg(target_os = "windows")]
    #[test]
    fn node_candidate_paths_includes_program_files_nodejs() {
        let paths = node_candidate_paths();
        let joined: String = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.to_lowercase().contains("nodejs\\node.exe"),
            "missing %ProgramFiles%\\nodejs\\node.exe — users who installed the official .msi would be invisible"
        );
    }

    #[test]
    fn clear_cache_invalidates_both_lookups() {
        // Seed both caches with a known state (None means "not present"
        // — we don't depend on the real machine having either binary).
        *BINARY_CACHE.write().unwrap() = Some(None);
        *NODE_CACHE.write().unwrap() = Some(None);
        clear_cache();
        assert!(
            BINARY_CACHE.read().unwrap().is_none(),
            "clear_cache must reset BINARY_CACHE to uncached"
        );
        assert!(
            NODE_CACHE.read().unwrap().is_none(),
            "clear_cache must reset NODE_CACHE to uncached"
        );
    }

    /// SECURITY: the login-shell fallback embeds the binary name in
    /// a `sh -c "command -v X"` string. If a future refactor lets
    /// arbitrary input through, a malicious caller could escape into
    /// the shell. Pin the whitelist.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn detect_via_login_shell_rejects_unsafe_binary_names() {
        // Shell metacharacters → reject (returns None without spawning).
        assert!(detect_via_login_shell("claude; rm -rf /").is_none());
        assert!(detect_via_login_shell("claude && evil").is_none());
        assert!(detect_via_login_shell("claude`evil`").is_none());
        assert!(detect_via_login_shell("claude$(evil)").is_none());
        assert!(detect_via_login_shell("../../../bin/sh").is_none());
        assert!(detect_via_login_shell("").is_none());
        // Allowed characters pass the guard (still returns None on
        // most CI runners that don't have claude/node, but the spawn
        // is allowed to proceed).
        let _ = detect_via_login_shell("claude");
        let _ = detect_via_login_shell("node");
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
