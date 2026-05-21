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

/// Locations where Claude Desktop installs the user-facing app.
///
/// Mac: signed `.app` bundle. Win: Squirrel-style per-user install
/// under `%LOCALAPPDATA%\Claude\`. Linux: not officially supported
/// by Anthropic as of 2026-05.
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
                v.push(base.join("Claude").join("Claude.exe"));
                v.push(base.join("Programs").join("Claude").join("Claude.exe"));
                // Squirrel installs use versioned dirs; check
                // %LOCALAPPDATA%\AnthropicClaude\app-x.y.z\Claude.exe.
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

/// Path to Claude Desktop's user config file. Always-known location
/// per Anthropic docs — we don't probe, we go straight to the
/// canonical path so the wizard can create the file if missing
/// (Claude creates it on first launch; if user hasn't launched yet
/// the wizard still works — we write the file ourselves).
pub fn config_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    #[cfg(target_os = "macos")]
    let dir = base.join("Claude");
    #[cfg(not(target_os = "macos"))]
    let dir = base.join("Claude");
    Some(dir.join("claude_desktop_config.json"))
}

// ── Config patch / unpatch ──────────────────────────────────────────

/// One server entry in Claude Desktop's `mcpServers` map. We keep the
/// shape minimal — the spec allows more keys (e.g. `disabled`,
/// `transport`) but the defaults are right for our stdio binary.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct McpServerEntry {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
}

/// Build the entry for our binary. `namespace` controls the env var
/// passed to the binary (DIMMY_CONFIG_NAMESPACE) so prod and staging
/// can register separate entries pointing at the same binary but
/// reading from separate config dirs.
pub fn build_entry(binary_path: &std::path::Path, namespace: &str) -> McpServerEntry {
    let mut env = std::collections::BTreeMap::new();
    env.insert("DIMMY_CONFIG_NAMESPACE".to_string(), namespace.to_string());
    McpServerEntry {
        command: binary_path.to_string_lossy().to_string(),
        args: vec![],
        env,
    }
}

/// The key under `mcpServers` we use. Differs by namespace so
/// prod=`dimmy` and staging=`dimmy-staging` coexist without
/// stepping on each other's entry.
pub fn entry_key(namespace: &str) -> String {
    if namespace == "dimmy" {
        "dimmy".to_string()
    } else {
        format!("dimmy-{}", namespace.trim_start_matches("dimmy-"))
    }
}

#[derive(Debug)]
pub enum PatchError {
    NoConfigDir,
    ReadFailed(String),
    WriteFailed(String),
    BackupFailed(String),
    ParseFailed(String),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // SECURITY: never embed the raw IO error message — paths
            // and user-content fragments might leak. Categorical
            // labels only.
            Self::NoConfigDir => write!(f, "Could not resolve Claude config dir"),
            Self::ReadFailed(_) => write!(f, "Reading claude_desktop_config.json failed"),
            Self::WriteFailed(_) => write!(f, "Writing claude_desktop_config.json failed"),
            Self::BackupFailed(_) => write!(f, "Backup of claude_desktop_config.json failed"),
            Self::ParseFailed(_) => write!(f, "Parsing claude_desktop_config.json failed"),
        }
    }
}

impl std::error::Error for PatchError {}

/// Add our MCP server entry to Claude Desktop's config, preserving
/// every other entry. Atomic: the existing file is copied to
/// `<path>.bak` BEFORE we touch the original. Creates the file +
/// parent dir if neither exists yet.
pub fn patch_config(binary_path: &std::path::Path, namespace: &str) -> Result<PathBuf, PatchError> {
    let path = config_path().ok_or(PatchError::NoConfigDir)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| PatchError::WriteFailed(e.to_string()))?;
    }

    let mut root: serde_json::Value = if path.exists() {
        // Back up the previous file in case our patch corrupts
        // something. Backup is overwritten on each patch (one slot,
        // no rotation — keeps the wizard simple).
        let bak = path.with_extension("json.bak");
        std::fs::copy(&path, &bak).map_err(|e| PatchError::BackupFailed(e.to_string()))?;
        let raw =
            std::fs::read_to_string(&path).map_err(|e| PatchError::ReadFailed(e.to_string()))?;
        if raw.trim().is_empty() {
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::from_str(&raw).map_err(|e| PatchError::ParseFailed(e.to_string()))?
        }
    } else {
        serde_json::Value::Object(Default::default())
    };

    let entry = build_entry(binary_path, namespace);
    let entry_value =
        serde_json::to_value(&entry).map_err(|e| PatchError::ParseFailed(e.to_string()))?;
    let key = entry_key(namespace);

    if !root.is_object() {
        return Err(PatchError::ParseFailed(
            "config root is not a JSON object".into(),
        ));
    }
    let obj = root.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    if !servers.is_object() {
        return Err(PatchError::ParseFailed(
            "mcpServers is not a JSON object".into(),
        ));
    }
    servers.as_object_mut().unwrap().insert(key, entry_value);

    let serialized =
        serde_json::to_string_pretty(&root).map_err(|e| PatchError::ParseFailed(e.to_string()))?;
    std::fs::write(&path, serialized).map_err(|e| PatchError::WriteFailed(e.to_string()))?;
    Ok(path)
}

/// Inverse of `patch_config`. Returns true if our entry was present
/// and got removed, false if it wasn't there (idempotent).
pub fn unpatch_config(namespace: &str) -> Result<bool, PatchError> {
    let path = config_path().ok_or(PatchError::NoConfigDir)?;
    if !path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| PatchError::ReadFailed(e.to_string()))?;
    if raw.trim().is_empty() {
        return Ok(false);
    }
    let mut root: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| PatchError::ParseFailed(e.to_string()))?;
    let key = entry_key(namespace);
    let removed = root
        .as_object_mut()
        .and_then(|obj| obj.get_mut("mcpServers"))
        .and_then(|s| s.as_object_mut())
        .and_then(|m| m.remove(&key))
        .is_some();
    if removed {
        let serialized = serde_json::to_string_pretty(&root)
            .map_err(|e| PatchError::ParseFailed(e.to_string()))?;
        std::fs::write(&path, serialized).map_err(|e| PatchError::WriteFailed(e.to_string()))?;
    }
    Ok(removed)
}

/// Check whether our entry is currently in the config. Returns the
/// entry if present so the Settings UI can show the registered path
/// (and detect path drift after a Velopack update — if entry.command
/// no longer points at the actual binary, wizard needs re-run).
pub fn read_current_entry(namespace: &str) -> Option<McpServerEntry> {
    let path = config_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let root: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let key = entry_key(namespace);
    let entry_val = root.get("mcpServers")?.get(&key)?;
    serde_json::from_value(entry_val.clone()).ok()
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
    fn entry_key_handles_namespaces() {
        assert_eq!(entry_key("dimmy"), "dimmy");
        assert_eq!(entry_key("dimmy-staging"), "dimmy-staging");
        // Defensive: a namespace like "staging" without the prefix
        // still gets a sensible key.
        assert_eq!(entry_key("staging"), "dimmy-staging");
    }

    #[test]
    fn build_entry_sets_env_var_with_namespace() {
        let entry = build_entry(std::path::Path::new("/x/dimmy-mcp"), "dimmy-staging");
        assert_eq!(entry.command, "/x/dimmy-mcp");
        assert!(entry.args.is_empty());
        assert_eq!(
            entry.env.get("DIMMY_CONFIG_NAMESPACE").map(String::as_str),
            Some("dimmy-staging")
        );
    }

    #[test]
    fn entry_serializes_minimally_when_no_args() {
        let entry = build_entry(std::path::Path::new("/x/dimmy-mcp"), "dimmy");
        let s = serde_json::to_string(&entry).unwrap();
        // `args` should be omitted (skip_serializing_if = Vec::is_empty)
        // so we don't pollute the user's config.
        assert!(!s.contains("\"args\""));
        assert!(s.contains("\"command\""));
        assert!(s.contains("\"env\""));
    }

    /// Pin that we preserve unrelated entries when patching — the
    /// user may have notion-mcp, slack-mcp, etc. already configured
    /// and we MUST NOT clobber them.
    #[test]
    fn patch_preserves_other_servers() {
        let initial = serde_json::json!({
            "mcpServers": {
                "notion": { "command": "/usr/local/bin/notion-mcp" },
                "slack": { "command": "/usr/local/bin/slack-mcp" }
            }
        });
        let mut root = initial.clone();
        let entry = build_entry(std::path::Path::new("/x/dimmy-mcp"), "dimmy");
        let key = entry_key("dimmy");
        root.as_object_mut()
            .unwrap()
            .get_mut("mcpServers")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key, serde_json::to_value(&entry).unwrap());

        let servers = root.get("mcpServers").unwrap().as_object().unwrap();
        assert_eq!(servers.len(), 3);
        assert!(servers.contains_key("notion"));
        assert!(servers.contains_key("slack"));
        assert!(servers.contains_key("dimmy"));
    }

    /// SECURITY: PatchError's Display impl must never leak the inner
    /// IO error message — paths / file contents from the user's
    /// machine could end up in Sentry / logs / telemetry.
    #[test]
    fn patch_error_display_redacts_inner_messages() {
        let cases = [
            PatchError::ReadFailed("/Users/k/secret.json".into()),
            PatchError::WriteFailed("permission denied to /home/u/.config".into()),
            PatchError::BackupFailed("disk full /mnt/...".into()),
            PatchError::ParseFailed("token 'API_KEY=sk-...' at line 3".into()),
        ];
        for c in cases {
            let s = format!("{}", c);
            assert!(!s.contains("secret"), "leaked path: {}", s);
            assert!(!s.contains("API_KEY"), "leaked key: {}", s);
            assert!(!s.contains("/Users"), "leaked home: {}", s);
            assert!(!s.contains("/home"), "leaked home: {}", s);
        }
    }
}
