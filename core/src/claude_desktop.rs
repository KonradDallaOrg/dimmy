//! Claude Desktop MCP bridge — detection + config patching.
//!
//! Distinct from `claude_code.rs` (which wraps the `claude` CLI via
//! `--print` subprocess for the dictation/recap LLM path). This
//! module integrates with the **Claude Desktop GUI app** as an MCP
//! server: we patch the user's `claude_desktop_config.json` to add
//! a `dimmy` entry, and Claude Desktop spawns our `dimmy-mcp` binary
//! on startup. Two-way bridge — Claude reads meetings + writes recaps
//! back via tool calls.
//!
//! See `mcp-server/` for the standalone binary.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Install detection ───────────────────────────────────────────────

/// Stable Microsoft Store / MSIX package family name for Claude
/// Desktop. Set at build time by Anthropic, never changes across
/// app versions — `Claude_<PublisherId>`. Confirmed against
/// 1.1.x → 1.1617.x by 2026-05.
#[cfg(target_os = "windows")]
const CLAUDE_MSIX_FAMILY: &str = "Claude_pzs8sxrjxfjjc";

/// Locations where Claude Desktop installs the user-facing app.
///
/// Mac: signed `.app` bundle. Win: two distribution channels —
/// (a) Microsoft Store / MSIX (default for new users 2026+) lives
/// under `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\` plus an
/// ACL-locked install in `C:\Program Files\WindowsApps\Claude_*`;
/// (b) Squirrel installer (legacy / direct download) under
/// `%LOCALAPPDATA%\AnthropicClaude\app-x.y.z\` or
/// `%LOCALAPPDATA%\Claude\`. The MSIX install path proper is read-
/// restricted, so we use the writable per-user Packages dir as the
/// detection marker — it's always created on first launch.
/// Linux: not officially supported by Anthropic as of 2026-05.
pub fn detect_claude_desktop() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = {
        #[cfg(target_os = "macos")]
        {
            let mut v = vec![PathBuf::from("/Applications/Claude.app")];
            if let Some(home) = dirs::home_dir() {
                v.push(home.join("Applications").join("Claude.app"));
            }
            v
        }
        #[cfg(target_os = "windows")]
        {
            let mut v = Vec::new();
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                let base = PathBuf::from(&local);
                // MSIX install marker. The Packages\<family>\ dir is
                // user-writable + always present after first launch,
                // unlike the ACL-locked WindowsApps install root.
                v.push(base.join("Packages").join(CLAUDE_MSIX_FAMILY));
                // Squirrel direct-download path.
                v.push(base.join("Claude").join("Claude.exe"));
                v.push(base.join("Programs").join("Claude").join("Claude.exe"));
                let anthropic = base.join("AnthropicClaude");
                if let Ok(entries) = std::fs::read_dir(&anthropic) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.starts_with("app-"))
                            .unwrap_or(false)
                        {
                            v.push(path.join("Claude.exe"));
                        }
                    }
                }
            }
            v
        }
        #[cfg(target_os = "linux")]
        {
            Vec::new()
        }
    };

    candidates.into_iter().find(|c| c.exists())
}

/// Path to Claude Desktop's user config file. On Win we prefer the
/// MSIX-containerized path when the package family dir exists —
/// the Store build reads from there and ignores the Roaming\Claude\
/// path entirely. Falling back to the standard Roaming path covers
/// the Squirrel direct-download install.
///
/// On Mac the config lives next to `~/Library/Application Support/`,
/// which `dirs::config_dir()` already resolves to.
pub fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // MSIX containerized config — has priority because it's the
        // shape Anthropic ships through Microsoft Store, where most
        // new Windows users land.
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let msix = PathBuf::from(&local)
                .join("Packages")
                .join(CLAUDE_MSIX_FAMILY)
                .join("LocalCache")
                .join("Roaming")
                .join("Claude");
            // Parent dir presence is the signal. The config file
            // itself may not exist yet (we'll create it), but the
            // containerized Roaming root is only present when the
            // Store app has run once — anti-false-positive guard
            // against picking the MSIX path on a machine that only
            // has the Squirrel build.
            if msix.exists() {
                return Some(msix.join("claude_desktop_config.json"));
            }
        }
    }
    let base = dirs::config_dir()?;
    let dir = base.join("Claude");
    Some(dir.join("claude_desktop_config.json"))
}

// ── Extension install / uninstall ──────────────────────────────────
//
// Claude Desktop discovers MCP servers from two sources:
//   1. `claude_desktop_config.json` → minimal stdio command/args
//   2. `Claude Extensions/<id>/manifest.json` → full DXT manifest
//      with icon, display name, description, declared tool list.
//
// Source #2 is what Anthropic's own connectors (PowerPoint, etc.)
// use, and it's the only path that produces a branded entry in the
// Connectors UI (icon + display name + tools list visible before
// the server is even spawned). We install via #2 so Dimmy appears
// as a first-class connector. The legacy patch_config/unpatch_config
// code shipped on this branch's earlier commits — uninstall_extension
// also wipes any leftover entry from those runs for cleanliness.

/// Bundled 128×128 PNG of the Dimmy app icon. Copied verbatim into
/// the extension directory as `icon.png` at install time — Claude
/// Desktop loads it relative to the manifest's `icon` field.
const EXTENSION_ICON_BYTES: &[u8] = include_bytes!("../assets/dimmy-logo-128.png");

/// Root dir under which Claude Desktop discovers extension folders.
/// Win: under the MSIX container (Store install) or the standard
/// Roaming path (Squirrel install). Mac: the canonical Anthropic
/// path inside `~/Library/Application Support/Claude/`.
pub fn extensions_root() -> Option<PathBuf> {
    let claude_root = claude_data_root()?;
    Some(claude_root.join("Claude Extensions"))
}

/// Sibling of `extensions_root` — Claude tracks per-extension enable
/// state in flat `<id>.json` files under this folder.
pub fn extensions_settings_root() -> Option<PathBuf> {
    let claude_root = claude_data_root()?;
    Some(claude_root.join("Claude Extensions Settings"))
}

/// Resolve the Roaming Claude data dir for whichever install variant
/// is present. Mirror of `config_path()`'s resolution but returns the
/// dir, not the file under it. Win prefers the MSIX containerized
/// path because that's where the Microsoft Store build of Claude
/// reads from (most new Windows users); falls back to standard
/// Roaming for Squirrel installs. Mac uses the standard config_dir().
fn claude_data_root() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let msix = PathBuf::from(&local)
                .join("Packages")
                .join(CLAUDE_MSIX_FAMILY)
                .join("LocalCache")
                .join("Roaming")
                .join("Claude");
            if msix.exists() {
                return Some(msix);
            }
        }
    }
    Some(dirs::config_dir()?.join("Claude"))
}

/// The extension dir id Claude uses on disk. Differs by namespace so
/// prod (`dimmy`) and staging (`dimmy-staging`) installs coexist with
/// independent enable state and binaries.
pub fn extension_id(namespace: &str) -> String {
    if namespace == "dimmy" {
        "dimmy".to_string()
    } else {
        format!("dimmy-{}", namespace.trim_start_matches("dimmy-"))
    }
}

#[derive(Debug)]
pub enum InstallError {
    NoExtensionsRoot,
    BinaryMissing,
    CopyFailed(String),
    WriteFailed(String),
    ParseFailed(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // SECURITY: redact inner messages — they carry paths and
            // raw IO errors that should not leak to telemetry.
            Self::NoExtensionsRoot => write!(f, "Could not resolve Claude extensions dir"),
            Self::BinaryMissing => write!(f, "dimmy-mcp binary not found at source path"),
            Self::CopyFailed(_) => write!(f, "Copying extension assets failed"),
            Self::WriteFailed(_) => write!(f, "Writing extension manifest/settings failed"),
            Self::ParseFailed(_) => write!(f, "Serializing extension manifest failed"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Build the DXT v0.3 manifest for our extension. `version` is the
/// Dimmy release version (`env!("CARGO_PKG_VERSION")` from the host's
/// FFI shim — keeps the manifest visibly in sync with the binary it
/// ships alongside).
fn render_manifest(version: &str, namespace: &str) -> serde_json::Value {
    let display = if namespace == "dimmy" {
        "Dimmy".to_string()
    } else {
        format!("Dimmy ({})", namespace.trim_start_matches("dimmy-"))
    };
    // Win and Mac binary names differ; the manifest's mcp_config
    // command runs through Claude's `${__dirname}` substitution, so
    // the bare filename is enough.
    let binary_name = mcp_binary_filename();
    let platform = if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        // Linux DXT support is documented in the spec but no shipping
        // Claude Desktop client renders Linux extensions yet (2026-05).
        "linux"
    };
    serde_json::json!({
        "manifest_version": "0.3",
        "name": display,
        "display_name": display,
        "version": version,
        "description": "Read Dimmy meetings + dictations, save recaps back",
        "long_description": "Two-way bridge between Dimmy (voice transcription overlay) and Claude Desktop. Lets Claude read your recent meetings and dictations, then write recaps back into Dimmy's history. Runs entirely on this device — no network calls between the bridge and Claude.",
        "author": { "name": "Dimmy", "url": "https://dimmy.app" },
        "homepage": "https://dimmy.app",
        "icon": "icon.png",
        "tools": [
            { "name": "dimmy_get_recent_meetings", "description": "List the most recent meetings recorded by Dimmy." },
            { "name": "dimmy_get_meeting", "description": "Retrieve the full transcript, metadata, and existing recap for a specific meeting." },
            { "name": "dimmy_save_recap", "description": "Write recap markdown back into a meeting directory as recap.md." },
            { "name": "dimmy_get_recent_dictations", "description": "List the most recent dictations from the Dimmy pill." }
        ],
        "server": {
            "type": "binary",
            "entry_point": binary_name,
            "mcp_config": {
                "command": format!("${{__dirname}}/{}", binary_name),
                "args": [],
                "env": { "DIMMY_CONFIG_NAMESPACE": namespace }
            }
        },
        "compatibility": {
            "claude_desktop": ">=0.10.0",
            "platforms": [platform]
        }
    })
}

fn mcp_binary_filename() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "dimmy-mcp.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "dimmy-mcp"
    }
}

/// Install Dimmy as a Claude Desktop extension: copy the binary into
/// `<extensions_root>/<id>/`, drop the icon + manifest next to it,
/// flip the per-extension enable flag. Idempotent — re-running just
/// overwrites the files (so updates "just work" after a fresh binary
/// is dropped next to Dimmy.exe).
///
/// `binary_src` is the path the host (C#/Swift) found for dimmy-mcp;
/// we copy it rather than reference it so a Velopack/Sparkle update
/// that moves Dimmy doesn't orphan Claude's reference.
pub fn install_extension(
    binary_src: &std::path::Path,
    version: &str,
    namespace: &str,
) -> Result<PathBuf, InstallError> {
    crate::log(&format!(
        "[mcp-install] enter binary_src={:?} version={} namespace={}",
        binary_src, version, namespace
    ));
    if !binary_src.exists() {
        crate::log("[mcp-install] FAIL: binary_src does not exist");
        return Err(InstallError::BinaryMissing);
    }
    let ext_root = match extensions_root() {
        Some(p) => p,
        None => {
            crate::log("[mcp-install] FAIL: extensions_root unresolved");
            return Err(InstallError::NoExtensionsRoot);
        }
    };
    crate::log(&format!("[mcp-install] ext_root={:?}", ext_root));
    let id = extension_id(namespace);
    let dir = ext_root.join(&id);
    crate::log(&format!("[mcp-install] target dir={:?}", dir));
    std::fs::create_dir_all(&dir).map_err(|e| {
        crate::log(&format!("[mcp-install] FAIL create_dir_all: {}", e));
        InstallError::WriteFailed(e.to_string())
    })?;

    // Copy binary first; on Windows the destination may be locked if
    // Claude has spawned an old copy. Caller is expected to ask the
    // user to quit Claude before install / disconnect — we still try
    // best-effort and surface the error category if locked.
    let dest_binary = dir.join(mcp_binary_filename());
    crate::log(&format!(
        "[mcp-install] copying binary to {:?}",
        dest_binary
    ));
    std::fs::copy(binary_src, &dest_binary).map_err(|e| {
        crate::log(&format!("[mcp-install] FAIL copy: {}", e));
        InstallError::CopyFailed(e.to_string())
    })?;

    // Icon and manifest. The manifest is rewritten on every install
    // so version + binary name stay in sync with the binary we just
    // dropped, even when a contributor renames either.
    std::fs::write(dir.join("icon.png"), EXTENSION_ICON_BYTES).map_err(|e| {
        crate::log(&format!("[mcp-install] FAIL write icon: {}", e));
        InstallError::WriteFailed(e.to_string())
    })?;
    let manifest = render_manifest(version, namespace);
    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
        crate::log(&format!("[mcp-install] FAIL serialize manifest: {}", e));
        InstallError::ParseFailed(e.to_string())
    })?;
    std::fs::write(dir.join("manifest.json"), manifest_json).map_err(|e| {
        crate::log(&format!("[mcp-install] FAIL write manifest: {}", e));
        InstallError::WriteFailed(e.to_string())
    })?;

    // Per-extension enable flag. Without this Claude renders the
    // extension in the UI but greyed out; the user would have to
    // flip it on manually after every install.
    if let Some(settings_root) = extensions_settings_root() {
        std::fs::create_dir_all(&settings_root).map_err(|e| {
            crate::log(&format!("[mcp-install] FAIL create settings dir: {}", e));
            InstallError::WriteFailed(e.to_string())
        })?;
        let settings_path = settings_root.join(format!("{}.json", id));
        std::fs::write(&settings_path, "{\n  \"isEnabled\": true\n}\n").map_err(|e| {
            crate::log(&format!("[mcp-install] FAIL write settings: {}", e));
            InstallError::WriteFailed(e.to_string())
        })?;
    }

    // Defensive: wipe any leftover entry from the legacy patch_config
    // path. On a fresh-from-this-branch user this is a no-op; on a
    // user who hit the wizard during the patch_config era it removes
    // a duplicate that would otherwise make Claude spawn dimmy-mcp
    // twice.
    let _ = remove_legacy_config_entry(namespace);

    crate::log(&format!("[mcp-install] OK dir={:?}", dir));
    Ok(dir)
}

/// Inverse of `install_extension`. Removes the extension dir + its
/// per-extension settings file. Idempotent — returns true iff the
/// extension dir was actually present.
pub fn uninstall_extension(namespace: &str) -> Result<bool, InstallError> {
    let ext_root = extensions_root().ok_or(InstallError::NoExtensionsRoot)?;
    let id = extension_id(namespace);
    let dir = ext_root.join(&id);
    let removed = dir.exists();
    if removed {
        std::fs::remove_dir_all(&dir).map_err(|e| InstallError::WriteFailed(e.to_string()))?;
    }
    if let Some(settings_root) = extensions_settings_root() {
        let settings_path = settings_root.join(format!("{}.json", id));
        let _ = std::fs::remove_file(&settings_path);
    }
    // Wipe any legacy config-patch entry too, so a user that hit
    // the old wizard and the new one in sequence ends up with a
    // single canonical state.
    let _ = remove_legacy_config_entry(namespace);
    Ok(removed)
}

/// Read the installed extension's manifest so the Settings UI can
/// show "installed at version 0.6.50". Returns None if not installed
/// or the manifest is unreadable/corrupt.
pub fn read_installed_manifest(namespace: &str) -> Option<serde_json::Value> {
    let dir = extensions_root()?.join(extension_id(namespace));
    let manifest = dir.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest).ok()?;
    serde_json::from_str(&raw).ok()
}

/// True iff the extension is registered AND enabled in its settings
/// file. UI distinguishes between "installed but disabled by user in
/// Claude's UI" vs "fully active".
pub fn is_extension_enabled(namespace: &str) -> bool {
    let settings_root = match extensions_settings_root() {
        Some(p) => p,
        None => return false,
    };
    let path = settings_root.join(format!("{}.json", extension_id(namespace)));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("isEnabled").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// Best-effort removal of any leftover `mcpServers.<key>` entry in
/// `claude_desktop_config.json`. Returns true iff an entry was
/// actually removed. Used by both install (to avoid double-register
/// when migrating from the patch-config wizard era) and uninstall
/// (to leave the user with a clean state).
fn remove_legacy_config_entry(namespace: &str) -> Result<bool, std::io::Error> {
    let path = match claude_data_root() {
        Some(root) => root.join("claude_desktop_config.json"),
        None => return Ok(false),
    };
    if !path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(&path)?;
    if raw.trim().is_empty() {
        return Ok(false);
    }
    let mut root: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let key = extension_id(namespace);
    let removed = root
        .as_object_mut()
        .and_then(|obj| obj.get_mut("mcpServers"))
        .and_then(|s| s.as_object_mut())
        .and_then(|m| m.remove(&key))
        .is_some();
    if removed {
        if let Ok(serialized) = serde_json::to_string_pretty(&root) {
            std::fs::write(&path, serialized)?;
        }
    }
    Ok(removed)
}

// ── Heartbeat + activity ────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HeartbeatPayload {
    pub timestamp: u64,
    pub version: String,
}

/// Read the heartbeat file written by `dimmy-mcp`. Returns None if
/// the file is missing or corrupt. Caller derives "alive" by comparing
/// `timestamp` to `now()` (typically <60 s = alive, server pings
/// every 30 s).
pub fn read_heartbeat(config_dir: &std::path::Path) -> Option<HeartbeatPayload> {
    let path = config_dir.join("mcp.heartbeat");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CallLogEntry {
    pub ts: u64,
    pub tool: String,
    pub ok: bool,
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// Read the last `limit` entries from the call log (most recent
/// first). Used by Settings UI to render "Last call: X min ago"
/// + an activity timeline.
pub fn read_recent_calls(config_dir: &std::path::Path, limit: usize) -> Vec<CallLogEntry> {
    let path = config_dir.join("mcp.calls.log");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out: Vec<CallLogEntry> = raw
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .take(limit)
        .collect();
    // Lines are written in chronological order so reverse-iter gives
    // newest-first; out is already in that order.
    out.truncate(limit);
    out
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_id_handles_namespaces() {
        assert_eq!(extension_id("dimmy"), "dimmy");
        assert_eq!(extension_id("dimmy-staging"), "dimmy-staging");
        // Defensive: a namespace like "staging" without the prefix
        // still gets a sensible id.
        assert_eq!(extension_id("staging"), "dimmy-staging");
    }

    #[test]
    fn manifest_has_required_dxt_fields() {
        // DXT spec v0.3 requires manifest_version, name, version,
        // server.{type,entry_point,mcp_config}. Claude Desktop
        // silently drops extensions missing any of these.
        let m = render_manifest("0.6.50", "dimmy");
        assert_eq!(m["manifest_version"], "0.3");
        assert_eq!(m["version"], "0.6.50");
        assert!(m["name"].is_string());
        let server = &m["server"];
        assert_eq!(server["type"], "binary");
        assert!(server["entry_point"]
            .as_str()
            .unwrap()
            .starts_with("dimmy-mcp"));
        let cfg = &server["mcp_config"];
        assert!(cfg["command"]
            .as_str()
            .unwrap()
            .starts_with("${__dirname}/"));
        assert_eq!(cfg["env"]["DIMMY_CONFIG_NAMESPACE"], "dimmy");
    }

    #[test]
    fn manifest_lists_all_tools() {
        // Tools array is what Claude renders in the connectors UI
        // before the server is spawned. Drift here = invisible tools
        // until first call. Pin by name to catch removal regressions.
        let m = render_manifest("0.6.50", "dimmy");
        let tools = m["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"dimmy_get_recent_meetings"));
        assert!(names.contains(&"dimmy_get_meeting"));
        assert!(names.contains(&"dimmy_save_recap"));
        assert!(names.contains(&"dimmy_get_recent_dictations"));
    }

    #[test]
    fn manifest_namespace_appears_in_env_and_display_name() {
        // Staging install must produce a distinguishable connector
        // entry — both visually (display name) and at runtime (the
        // env var routes the binary to the staging config dir).
        let m = render_manifest("0.6.50", "dimmy-staging");
        assert!(m["display_name"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("staging"));
        assert_eq!(
            m["server"]["mcp_config"]["env"]["DIMMY_CONFIG_NAMESPACE"],
            "dimmy-staging"
        );
    }

    #[test]
    fn manifest_declares_current_platform_only() {
        // Mixing platforms in `compatibility.platforms` is allowed
        // by the spec but the install-time host knows exactly which
        // OS it ran on, so we keep it precise (one entry) — easier
        // to reason about update behaviour across platforms.
        let m = render_manifest("0.6.50", "dimmy");
        let platforms = m["compatibility"]["platforms"].as_array().unwrap();
        assert_eq!(platforms.len(), 1);
        #[cfg(target_os = "windows")]
        assert_eq!(platforms[0], "win32");
        #[cfg(target_os = "macos")]
        assert_eq!(platforms[0], "darwin");
    }

    /// SECURITY: InstallError's Display impl must never leak the
    /// inner IO error message — paths and parse-error context from
    /// the user's machine could end up in Sentry / telemetry / logs.
    #[test]
    fn install_error_display_redacts_inner_messages() {
        let cases = [
            InstallError::CopyFailed("/Users/k/secret/dimmy-mcp.exe".into()),
            InstallError::WriteFailed("permission denied to /home/u/.config".into()),
            InstallError::ParseFailed("token 'API_KEY=sk-...' at line 3".into()),
        ];
        for c in cases {
            let s = format!("{}", c);
            assert!(!s.contains("secret"), "leaked path: {}", s);
            assert!(!s.contains("API_KEY"), "leaked key: {}", s);
            assert!(!s.contains("/Users"), "leaked home: {}", s);
            assert!(!s.contains("/home"), "leaked home: {}", s);
        }
    }

    #[test]
    fn icon_bytes_are_a_valid_png() {
        // Sanity check the embedded icon — if someone replaces the
        // file without re-encoding, ship-time render in Claude
        // Desktop would silently fall back to a generic puzzle
        // piece. Catch the corruption here instead.
        assert!(EXTENSION_ICON_BYTES.len() > 1000);
        // PNG magic header.
        assert_eq!(&EXTENSION_ICON_BYTES[..8], b"\x89PNG\r\n\x1a\n");
    }
}
