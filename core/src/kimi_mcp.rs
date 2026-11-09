//! Register Dimmy's MCP server (`dimmy-mcp`) with Kimi Code, so Kimi can
//! read the user's meetings and dictations the way Claude Desktop does.
//!
//! Kimi Code CLI and Kimi Code Desktop read the same file,
//! `~/.kimi-code/mcp.json` (`$KIMI_CODE_HOME`), shaped
//! `{"mcpServers": {"<name>": {command, args, env, enabled}}}`. New servers
//! apply to new sessions.
//!
//! The file belongs to the user. We touch one key, `mcpServers.<ours>`;
//! a file we cannot parse is left alone (an error, never an overwrite);
//! the first time we change an existing file we keep a copy next to it.
//!
//! One side effect worth knowing: Kimi loads the user's MCP servers in
//! every session, including the ones Dimmy opens for the Kimi
//! subscription (`kimi_cli.rs`), so with this registered each such call
//! also starts `dimmy-mcp`. It is a small local binary and its tools are
//! never approved there (permission requests are cancelled).

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

const BACKUP_SUFFIX: &str = "dimmy-backup";
/// `dimmy-mcp` writes `mcp.heartbeat.kimi` when started from our entry, so
/// the wizard can tell "Kimi loaded Dimmy" apart from a running Claude
/// Desktop (which writes plain `mcp.heartbeat`).
const HEARTBEAT_CLIENT: &str = "kimi";

/// Kimi Code Desktop, if installed. Windows: the NSIS installer (winget
/// `MoonshotAI.KimiCodeDesktop`) puts it under `%LOCALAPPDATA%\Programs` or
/// `%ProgramFiles%`; we take the first folder whose name has "kimi" in it
/// and that holds a "kimi" .exe. macOS: an app bundle in /Applications or
/// ~/Applications whose name has "Kimi Code".
pub fn detect_desktop() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let roots: Vec<PathBuf> = [
            std::env::var("LOCALAPPDATA")
                .ok()
                .map(|l| PathBuf::from(l).join("Programs")),
            std::env::var("ProgramFiles").ok().map(PathBuf::from),
        ]
        .into_iter()
        .flatten()
        .collect();
        for root in roots {
            let Ok(dirs) = std::fs::read_dir(&root) else {
                continue;
            };
            for d in dirs.flatten() {
                let dir = d.path();
                let name = d.file_name().to_string_lossy().to_lowercase();
                if !name.contains("kimi") || !dir.is_dir() {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(&dir) else {
                    continue;
                };
                let exe = files.flatten().map(|f| f.path()).find(|p| {
                    let n = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    n.ends_with(".exe") && n.contains("kimi") && !n.contains("uninstall")
                });
                if exe.is_some() {
                    return exe;
                }
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(h) = dirs::home_dir() {
            roots.push(h.join("Applications"));
        }
        roots.into_iter().find_map(|root| {
            std::fs::read_dir(root)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase());
                    n.is_some_and(|n| n.contains("kimi code") && n.ends_with(".app"))
                })
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// Seconds since `dimmy-mcp` last wrote its Kimi heartbeat, if ever.
fn heartbeat_age_secs() -> Option<u64> {
    let dir = crate::config_dir_path()?;
    let raw =
        std::fs::read_to_string(dir.join(format!("mcp.heartbeat.{HEARTBEAT_CLIENT}"))).ok()?;
    let ts = serde_json::from_str::<Value>(&raw).ok()?["timestamp"].as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now.saturating_sub(ts))
}

#[derive(Debug, PartialEq)]
pub enum McpError {
    /// No home directory to put `.kimi-code` in.
    NoHome,
    BinaryMissing,
    /// `mcp.json` exists but is not a JSON object we can merge into.
    Unreadable,
    WriteFailed,
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHome => write!(f, "no home directory for ~/.kimi-code"),
            Self::BinaryMissing => write!(f, "dimmy-mcp binary not found"),
            Self::Unreadable => {
                write!(f, "~/.kimi-code/mcp.json is not valid JSON; left untouched")
            }
            Self::WriteFailed => write!(f, "writing ~/.kimi-code/mcp.json failed"),
        }
    }
}

pub fn mcp_json_path() -> Option<PathBuf> {
    crate::kimi_cli::kimi_home().map(|h| h.join("mcp.json"))
}

/// The server name for this build's data namespace: `dimmy` for prod, so
/// a staging install registers beside it instead of over it.
fn server_name(namespace: &str) -> String {
    crate::claude_desktop::extension_id(namespace)
}

/// The parsed file, `{}` when it does not exist or is empty.
fn read_root(path: &Path) -> Result<Map<String, Value>, McpError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(_) => return Err(McpError::Unreadable),
    };
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(McpError::Unreadable),
    }
}

fn write_root(path: &Path, root: &Map<String, Value>) -> Result<(), McpError> {
    let dir = path.parent().ok_or(McpError::WriteFailed)?;
    std::fs::create_dir_all(dir).map_err(|_| McpError::WriteFailed)?;
    let body = serde_json::to_string_pretty(&Value::Object(root.clone()))
        .map_err(|_| McpError::WriteFailed)?;
    let tmp = path.with_extension("json.dimmy-tmp");
    std::fs::write(&tmp, body).map_err(|_| McpError::WriteFailed)?;
    std::fs::rename(&tmp, path).map_err(|_| {
        let _ = std::fs::remove_file(&tmp);
        McpError::WriteFailed
    })
}

/// Keep the user's original once: the first change we make to a file we
/// did not create.
fn backup_once(path: &Path) {
    let backup = path.with_extension(format!("json.{BACKUP_SUFFIX}"));
    if path.is_file() && !backup.exists() {
        if let Err(e) = std::fs::copy(path, &backup) {
            crate::log(&format!("[kimi-mcp] backup failed: {e}"));
        }
    }
}

fn servers_mut(root: &mut Map<String, Value>) -> Result<&mut Map<String, Value>, McpError> {
    let entry = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    entry.as_object_mut().ok_or(McpError::Unreadable)
}

pub fn install_at(path: &Path, binary: &Path, namespace: &str) -> Result<(), McpError> {
    assert!(!namespace.is_empty(), "namespace must not be empty");
    if !binary.is_file() {
        return Err(McpError::BinaryMissing);
    }
    let mut root = read_root(path)?;
    let name = server_name(namespace);
    let entry = json!({
        "command": binary.to_string_lossy(),
        "args": [],
        "env": { "DIMMY_CONFIG_NAMESPACE": namespace, "DIMMY_MCP_CLIENT": HEARTBEAT_CLIENT },
        "enabled": true,
    });
    let servers = servers_mut(&mut root)?;
    if servers.get(&name) == Some(&entry) {
        return Ok(());
    }
    servers.insert(name.clone(), entry);
    backup_once(path);
    write_root(path, &root)?;
    assert!(
        status_at(path, namespace).0,
        "dimmy entry must be present after install"
    );
    Ok(())
}

/// Ok(true) when our entry was there and is now gone.
pub fn uninstall_at(path: &Path, namespace: &str) -> Result<bool, McpError> {
    if !path.is_file() {
        return Ok(false);
    }
    let mut root = read_root(path)?;
    let removed = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .and_then(|m| m.remove(&server_name(namespace)))
        .is_some();
    if removed {
        write_root(path, &root)?;
    }
    Ok(removed)
}

/// (registered, command) for this namespace's entry.
pub fn status_at(path: &Path, namespace: &str) -> (bool, Option<String>) {
    let Ok(root) = read_root(path) else {
        return (false, None);
    };
    let entry = root
        .get("mcpServers")
        .and_then(|s| s.get(server_name(namespace)));
    let command = entry.and_then(|e| e["command"].as_str()).map(str::to_owned);
    (entry.is_some(), command)
}

/// `{"kimi_found", "registered", "binary_exists", "config_path",
/// "desktop_path", "heartbeat_age_secs"}` for the
/// Integrations card. Local UI only, never telemetry (it carries paths).
pub fn status_json(namespace: &str) -> String {
    let path = mcp_json_path();
    let (registered, command) = path
        .as_deref()
        .map(|p| status_at(p, namespace))
        .unwrap_or((false, None));
    json!({
        "kimi_found": crate::kimi_cli::kimi_home().is_some_and(|h| h.is_dir()),
        "registered": registered,
        "binary_exists": command.as_deref().is_some_and(|c| Path::new(c).is_file()),
        "config_path": path.map(|p| p.to_string_lossy().into_owned()),
        "desktop_path": detect_desktop().map(|p| p.to_string_lossy().into_owned()),
        "heartbeat_age_secs": heartbeat_age_secs(),
    })
    .to_string()
}

pub fn install(binary: &Path, namespace: &str) -> Result<(), McpError> {
    install_at(&mcp_json_path().ok_or(McpError::NoHome)?, binary, namespace)
}

pub fn uninstall(namespace: &str) -> Result<bool, McpError> {
    uninstall_at(&mcp_json_path().ok_or(McpError::NoHome)?, namespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "dimmy-kimi-mcp-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn mcp(&self) -> PathBuf {
            self.0.join(".kimi-code").join("mcp.json")
        }
        fn binary(&self) -> PathBuf {
            let b = self.0.join("dimmy-mcp");
            std::fs::write(&b, b"bin").unwrap();
            b
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn read(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn install_creates_the_file_when_there_is_none() {
        let s = Sandbox::new("create");
        let bin = s.binary();
        install_at(&s.mcp(), &bin, "dimmy").unwrap();
        let v = read(&s.mcp());
        assert_eq!(
            v["mcpServers"]["dimmy"]["command"],
            bin.to_string_lossy().as_ref()
        );
        assert_eq!(
            v["mcpServers"]["dimmy"]["env"]["DIMMY_CONFIG_NAMESPACE"],
            "dimmy"
        );
        assert_eq!(v["mcpServers"]["dimmy"]["env"]["DIMMY_MCP_CLIENT"], "kimi");
        assert!(
            !s.mcp().with_extension("json.dimmy-backup").exists(),
            "nothing to back up"
        );
    }

    #[test]
    fn install_keeps_other_servers_and_backs_up_once() {
        let s = Sandbox::new("merge");
        std::fs::create_dir_all(s.mcp().parent().unwrap()).unwrap();
        let original = r#"{"mcpServers":{"github":{"command":"gh-mcp"}},"other":1}"#;
        std::fs::write(s.mcp(), original).unwrap();
        install_at(&s.mcp(), &s.binary(), "dimmy").unwrap();
        let v = read(&s.mcp());
        assert_eq!(v["mcpServers"]["github"]["command"], "gh-mcp");
        assert_eq!(v["other"], 1);
        assert!(v["mcpServers"]["dimmy"].is_object());
        let backup = s.mcp().with_extension("json.dimmy-backup");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

        install_at(&s.mcp(), &s.binary(), "dimmy-staging").unwrap();
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            original,
            "the backup is the user's original, not our last write"
        );
        assert!(read(&s.mcp())["mcpServers"]["dimmy-staging"].is_object());
    }

    #[test]
    fn a_file_we_cannot_parse_is_never_overwritten() {
        let s = Sandbox::new("broken");
        std::fs::create_dir_all(s.mcp().parent().unwrap()).unwrap();
        std::fs::write(s.mcp(), "{ not json").unwrap();
        assert_eq!(
            install_at(&s.mcp(), &s.binary(), "dimmy"),
            Err(McpError::Unreadable)
        );
        assert_eq!(std::fs::read_to_string(s.mcp()).unwrap(), "{ not json");
        std::fs::write(s.mcp(), r#"{"mcpServers": []}"#).unwrap();
        assert_eq!(
            install_at(&s.mcp(), &s.binary(), "dimmy"),
            Err(McpError::Unreadable)
        );
    }

    #[test]
    fn a_missing_binary_is_refused() {
        let s = Sandbox::new("nobin");
        assert_eq!(
            install_at(&s.mcp(), &s.0.join("absent"), "dimmy"),
            Err(McpError::BinaryMissing)
        );
        assert!(!s.mcp().exists());
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let s = Sandbox::new("remove");
        std::fs::create_dir_all(s.mcp().parent().unwrap()).unwrap();
        std::fs::write(s.mcp(), r#"{"mcpServers":{"github":{"command":"gh-mcp"}}}"#).unwrap();
        install_at(&s.mcp(), &s.binary(), "dimmy").unwrap();
        assert_eq!(status_at(&s.mcp(), "dimmy").0, true);
        assert_eq!(uninstall_at(&s.mcp(), "dimmy"), Ok(true));
        assert_eq!(uninstall_at(&s.mcp(), "dimmy"), Ok(false));
        let v = read(&s.mcp());
        assert_eq!(v["mcpServers"]["github"]["command"], "gh-mcp");
        assert!(v["mcpServers"].get("dimmy").is_none());
        assert_eq!(status_at(&s.mcp(), "dimmy"), (false, None));
    }
}
