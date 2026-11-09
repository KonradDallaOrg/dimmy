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
                // "Kimi Code", not the Kimi app (Programs\Kimi) next to it.
                if !name.replace(' ', "").contains("kimicode") || !dir.is_dir() {
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
fn heartbeat_age_secs(client: &str) -> Option<u64> {
    let dir = crate::config_dir_path()?;
    let raw = std::fs::read_to_string(dir.join(format!("mcp.heartbeat.{client}"))).ok()?;
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
        "heartbeat_age_secs": heartbeat_age_secs(HEARTBEAT_CLIENT),
        "work_app_path": detect_work_app().map(|p| p.to_string_lossy().into_owned()),
        "work_found": work_root().is_some(),
        "work_registered": work_root().is_some_and(|r| work_record_path(&r, namespace).is_file()),
        "work_installed": work_root().is_some_and(|r| work_installed(&r, namespace)),
        "work_heartbeat_age_secs": heartbeat_age_secs(WORK_HEARTBEAT_CLIENT),
    })
    .to_string()
}

/// Registers Dimmy everywhere Kimi can load it: `~/.kimi-code/mcp.json` (Kimi
/// Code CLI and Desktop) and, once the Kimi app has set itself up, a personal
/// plugin for Kimi Work. The user then presses Install in Kimi's Plugins page.
pub fn install(binary: &Path, namespace: &str) -> Result<(), McpError> {
    install_at(&mcp_json_path().ok_or(McpError::NoHome)?, binary, namespace)?;
    if let (Some(root), Some(cfg)) = (work_root(), crate::config_dir_path()) {
        register_work_plugin_at(&root, &cfg.join("kimi-plugin"), binary, namespace)?;
    }
    Ok(())
}

pub fn uninstall(namespace: &str) -> Result<bool, McpError> {
    let mut removed = uninstall_at(&mcp_json_path().ok_or(McpError::NoHome)?, namespace)?;
    if let (Some(root), Some(cfg)) = (work_root(), crate::config_dir_path()) {
        removed |= unregister_work_plugin_at(&root, &cfg.join("kimi-plugin"), namespace);
    }
    Ok(removed)
}

// ── Kimi Work (the Kimi app) ────────────────────────────────────────
//
// Kimi Work runs a Kimi Code engine of its own ("daimon") and starts LOCAL
// MCP servers declared by personal plugins. Verified on Kimi 3.2.24 for
// Windows (2026-11-09): a plugin whose kimi.plugin.json declares
// `mcpServers.dimmy = {command: <dimmy-mcp>}` started dimmy-mcp in a Work
// session. A plugin is "personal" when a record in
// `<daimon-share>/daimon/plugin-market/personal/<id>.json` points at its
// source folder: the one file Kimi's own Plugin Builder writes on import.
// Kimi copies the folder in when the user presses Install, so we never touch
// Kimi's installed plugins. (A plugin written straight into the installed
// list is parsed but never activated: Kimi's server-side list wins.)

const WORK_HEARTBEAT_CLIENT: &str = "kimi-work";
const PLUGIN_ICON: &[u8] = include_bytes!("../assets/dimmy-logo-128.png");

/// The Kimi app (Kimi Work), if installed.
pub fn detect_work_app() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let candidates: Vec<PathBuf> = std::env::var("LOCALAPPDATA")
        .ok()
        .map(|l| {
            vec![PathBuf::from(l)
                .join("Programs")
                .join("Kimi")
                .join("Kimi.exe")]
        })
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    let candidates: Vec<PathBuf> = {
        let mut v = vec![PathBuf::from("/Applications/Kimi.app")];
        if let Some(h) = dirs::home_dir() {
            v.push(h.join("Applications").join("Kimi.app"));
        }
        v
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let candidates: Vec<PathBuf> = Vec::new();
    candidates.into_iter().find(|p| p.exists())
}

/// `<app data>/kimi-desktop/daimon-share`, once Kimi Work has set itself up
/// (it appears on the app's first launch). The macOS path follows the same
/// layout under Application Support; not yet checked on a real Mac install.
fn work_root() -> Option<PathBuf> {
    let root = dirs::config_dir()?
        .join("kimi-desktop")
        .join("daimon-share");
    root.join("daimon").is_dir().then_some(root)
}

fn work_record_path(root: &Path, namespace: &str) -> PathBuf {
    root.join("daimon")
        .join("plugin-market")
        .join("personal")
        .join(format!("{}.json", server_name(namespace)))
}

/// Installed in Kimi Work: Kimi lists it in its own installed.json.
fn work_installed(root: &Path, namespace: &str) -> bool {
    let p = root
        .join("daimon")
        .join("runtime")
        .join("kimi-code")
        .join("home")
        .join("plugins")
        .join("installed.json");
    let id = server_name(namespace);
    read_root(&p)
        .ok()
        .and_then(|m| m.get("plugins").and_then(Value::as_array).cloned())
        .is_some_and(|ps| ps.iter().any(|x| x["id"] == id.as_str()))
}

/// Writes the plugin folder `<packages>/<id>` and the personal-market record
/// that points at it. Rewrites both every time, so a moved dimmy-mcp is
/// picked up on the next Connect.
pub fn register_work_plugin_at(
    root: &Path,
    packages: &Path,
    binary: &Path,
    namespace: &str,
) -> Result<(), McpError> {
    assert!(!namespace.is_empty(), "namespace must not be empty");
    if !binary.is_file() {
        return Err(McpError::BinaryMissing);
    }
    let id = server_name(namespace);
    let dir = packages.join(&id);
    let display = if namespace == "dimmy" {
        "Dimmy".to_string()
    } else {
        format!("Dimmy ({})", namespace.trim_start_matches("dimmy-"))
    };
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    };
    let description = "Your Dimmy meetings and dictations in Kimi: list, search and read transcripts, and save recaps back into Dimmy.";
    let mut overrides = Map::new();
    overrides.insert(
        id.clone(),
        json!({ "displayName": display, "description": "Meetings and dictations from Dimmy" }),
    );
    let mut servers = Map::new();
    servers.insert(
        id.clone(),
        json!({
            "command": binary.to_string_lossy(),
            "args": [],
            "env": { "DIMMY_CONFIG_NAMESPACE": namespace, "DIMMY_MCP_CLIENT": WORK_HEARTBEAT_CLIENT }
        }),
    );
    let manifest = json!({
        "$schema": "https://catalog.msh.team/misc/kimi.plugin.schema.json",
        "name": id,
        "version": env!("CARGO_PKG_VERSION"),
        "description": description,
        "keywords": ["dimmy", "meeting", "recap", "dictation", "transcript"],
        "author": "Dimmy",
        "homepage": "https://dimmy.app",
        "license": "Proprietary",
        "skillInstructions": "Use the Dimmy tools when the user asks about meetings, recordings, recaps or dictations made with Dimmy. For a recap: dimmy_get_recap_template, then dimmy_get_meeting, then dimmy_save_recap.",
        "interface": {
            "displayName": display,
            "shortDescription": "Your Dimmy meetings and dictations",
            "longDescription": "Read and search the meetings and dictations recorded with Dimmy on this computer, and save recaps back into Dimmy. Runs locally.",
            "developerName": "Dimmy",
            "websiteURL": "https://dimmy.app",
            "iconUrl": "https://dimmy.app/brand/icon-512.png",
            "category": "PRODUCTIVITY",
            "defaultLocale": "en-US",
            "locales": "./locales/",
            "tryPrompts": [
                "List my latest Dimmy meetings",
                "Recap my last Dimmy meeting",
                "Find what I dictated about the budget"
            ],
            "hostKind": "local",
            "platforms": [platform],
            "mcpOverrides": overrides.clone()
        },
        "mcpServers": servers
    });
    let mut it_overrides = Map::new();
    it_overrides.insert(
        id.clone(),
        json!({ "displayName": display, "description": "Riunioni e dettati da Dimmy" }),
    );
    let it = json!({
        "displayName": display,
        "shortDescription": "Le tue riunioni e dettati Dimmy",
        "longDescription": "Leggi e cerca le riunioni e i dettati registrati con Dimmy su questo computer, e salva i recap in Dimmy.",
        "developerName": "Dimmy",
        "tryPrompts": [
            "Elenca le mie ultime riunioni Dimmy",
            "Fai il recap della mia ultima riunione Dimmy",
            "Cerca cosa ho dettato sul budget"
        ],
        "mcpOverrides": it_overrides
    });
    let write = |p: &Path, bytes: &[u8]| -> Result<(), McpError> {
        std::fs::create_dir_all(p.parent().ok_or(McpError::WriteFailed)?)
            .map_err(|_| McpError::WriteFailed)?;
        std::fs::write(p, bytes).map_err(|_| McpError::WriteFailed)
    };
    let pretty = |v: &Value| serde_json::to_vec_pretty(v).map_err(|_| McpError::WriteFailed);
    write(&dir.join("kimi.plugin.json"), &pretty(&manifest)?)?;
    write(&dir.join("locales").join("it-IT.json"), &pretty(&it)?)?;
    write(&dir.join("icon.png"), PLUGIN_ICON)?;

    let now = rfc3339_utc_now();
    let src = dir.to_string_lossy().to_string();
    let record = json!({
        "schemaVersion": 1,
        "id": id,
        "market": "personal",
        "sourcePath": src,
        "approvedWorkspaceRoot": src,
        "displayName": display,
        "pluginVersion": env!("CARGO_PKG_VERSION"),
        "description": description,
        "developerName": "Dimmy",
        "defaultLocale": "en-US",
        "locales": { "it-IT": { "displayName": display, "description": "Le tue riunioni e dettati Dimmy" } },
        "skillNames": [],
        "mcpServerNames": [],
        "agentNames": [],
        "hookEvents": [],
        "commandNames": [],
        "hasSystemPrompt": false,
        "registeredBy": "cli",
        "registeredAt": now,
        "updatedAt": now
    });
    write(&work_record_path(root, namespace), &pretty(&record)?)?;
    assert!(
        work_record_path(root, namespace).is_file(),
        "personal record must exist after register"
    );
    Ok(())
}

/// Removes our record and package. A copy the user already installed stays
/// in Kimi until they remove it from Kimi's Plugins page.
pub fn unregister_work_plugin_at(root: &Path, packages: &Path, namespace: &str) -> bool {
    let record = work_record_path(root, namespace);
    let had = record.is_file();
    let _ = std::fs::remove_file(&record);
    let _ = std::fs::remove_dir_all(packages.join(server_name(namespace)));
    had
}

/// RFC 3339 UTC with milliseconds, the shape Kimi writes in its records.
fn rfc3339_utc_now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
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
    fn work_plugin_is_registered_as_personal_and_removed_cleanly() {
        let s = Sandbox::new("work");
        let root = s.0.join("daimon-share");
        let packages = s.0.join("kimi-plugin");
        std::fs::create_dir_all(root.join("daimon")).unwrap();
        let bin = s.binary();
        register_work_plugin_at(&root, &packages, &bin, "dimmy").unwrap();

        let rec = read(&work_record_path(&root, "dimmy"));
        assert_eq!(rec["id"], "dimmy");
        assert_eq!(rec["market"], "personal");
        assert_eq!(rec["registeredBy"], "cli");
        let src = packages.join("dimmy");
        assert_eq!(rec["sourcePath"], src.to_string_lossy().as_ref());
        let man = read(&src.join("kimi.plugin.json"));
        assert_eq!(man["name"], "dimmy");
        assert_eq!(
            man["mcpServers"]["dimmy"]["command"],
            bin.to_string_lossy().as_ref()
        );
        assert_eq!(
            man["mcpServers"]["dimmy"]["env"]["DIMMY_MCP_CLIENT"],
            "kimi-work"
        );
        assert_eq!(man["interface"]["hostKind"], "local");
        assert!(src.join("icon.png").is_file());
        assert!(src.join("locales").join("it-IT.json").is_file());

        assert!(!work_installed(&root, "dimmy"));
        assert!(unregister_work_plugin_at(&root, &packages, "dimmy"));
        assert!(!work_record_path(&root, "dimmy").exists());
        assert!(!src.exists());
        assert!(!unregister_work_plugin_at(&root, &packages, "dimmy"));
    }

    #[test]
    fn work_installed_reads_kimis_own_list() {
        let s = Sandbox::new("work-inst");
        let root = s.0.join("daimon-share");
        let list = root
            .join("daimon")
            .join("runtime")
            .join("kimi-code")
            .join("home")
            .join("plugins");
        std::fs::create_dir_all(&list).unwrap();
        std::fs::write(
            list.join("installed.json"),
            r#"{"version":1,"plugins":[{"id":"kimi-pdf"},{"id":"dimmy"}]}"#,
        )
        .unwrap();
        assert!(work_installed(&root, "dimmy"));
        assert!(!work_installed(&root, "dimmy-staging"));
    }

    #[test]
    fn a_staging_build_registers_beside_prod() {
        let s = Sandbox::new("work-ns");
        let root = s.0.join("daimon-share");
        std::fs::create_dir_all(root.join("daimon")).unwrap();
        register_work_plugin_at(&root, &s.0.join("p"), &s.binary(), "dimmy-staging").unwrap();
        assert!(work_record_path(&root, "dimmy-staging").is_file());
        assert!(!work_record_path(&root, "dimmy").exists());
    }

    #[test]
    fn the_timestamp_is_rfc3339_utc() {
        let t = rfc3339_utc_now();
        assert_eq!(t.len(), 24, "{t}");
        assert!(
            t.starts_with("20") && t.ends_with('Z') && &t[10..11] == "T",
            "{t}"
        );
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
