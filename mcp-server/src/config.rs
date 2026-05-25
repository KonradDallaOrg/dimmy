//! Config-dir resolution. Mirror of the namespacing logic in
//! `core/src/lib.rs` so the MCP server reads from the same dir
//! Dimmy.app writes to.
//!
//! Order:
//!   1. `DIMMY_CONFIG_NAMESPACE` env var (set by the wizard in the
//!      `mcpServers.<name>.env` block of claude_desktop_config.json
//!      so prod + staging entries can coexist)
//!   2. Default `dimmy`
//!
//! Resolved dir:
//!   - macOS: `~/Library/Application Support/<namespace>/`
//!   - Linux: `~/.config/<namespace>/`
//!   - Windows: `%APPDATA%/<namespace>/`

use std::path::PathBuf;

pub struct Config {
    pub namespace: String,
    pub config_dir: PathBuf,
}

impl Config {
    pub fn resolve() -> Self {
        let namespace =
            std::env::var("DIMMY_CONFIG_NAMESPACE").unwrap_or_else(|_| "dimmy".to_string());

        let base = dirs::config_dir()
            // dirs::config_dir() is the same call core uses; on macOS
            // it returns ~/Library/Application Support which is what
            // Dimmy.app writes to.
            .unwrap_or_else(|| PathBuf::from("."));

        let config_dir = base.join(&namespace);

        Self {
            namespace,
            config_dir,
        }
    }

    /// Effective meeting storage dir. Mirrors `core/src/lib.rs`
    /// `meetings_dir()`: honour the user's `meeting_storage_path`
    /// override from config.json when set + the dir exists, else the
    /// default `<config_dir>/meetings`. The MCP server is a separate
    /// process (no FFI), so it reads the field straight from config.json.
    /// Read per-call — tool calls are infrequent and config.json is tiny;
    /// this keeps the value fresh if the user changes it mid-session.
    pub fn meetings_dir(&self) -> PathBuf {
        if let Some(p) = self.meeting_storage_override() {
            return p;
        }
        self.config_dir.join("meetings")
    }

    /// Read `meeting_storage_path` from config.json. Returns the override
    /// only when it's a non-empty string AND the directory exists (a
    /// stale path to a removed drive falls back to the default, matching
    /// the core's writability guard). Any parse / IO error → None.
    fn meeting_storage_override(&self) -> Option<PathBuf> {
        let raw = std::fs::read_to_string(self.config_dir.join("config.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let s = v.get("meeting_storage_path")?.as_str()?.trim();
        if s.is_empty() {
            return None;
        }
        let p = PathBuf::from(s);
        if p.is_dir() {
            Some(p)
        } else {
            None
        }
    }

    /// Append-only audit log of tool calls. Each line is JSON
    /// `{"ts": ..., "tool": "...", "ok": true}`. Settings UI tails
    /// this to render "last used X min ago".
    pub fn calls_log(&self) -> PathBuf {
        self.config_dir.join("mcp.calls.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_in(dir: &std::path::Path) -> Config {
        Config {
            namespace: "dimmy".to_string(),
            config_dir: dir.to_path_buf(),
        }
    }

    fn write_config(dir: &std::path::Path, json: &str) {
        std::fs::write(dir.join("config.json"), json).unwrap();
    }

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "dimmy_mcp_cfg_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn no_config_json_uses_default() {
        let dir = tmp();
        assert_eq!(cfg_in(&dir).meetings_dir(), dir.join("meetings"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_override_uses_default() {
        let dir = tmp();
        write_config(&dir, r#"{"meeting_storage_path": ""}"#);
        assert_eq!(cfg_in(&dir).meetings_dir(), dir.join("meetings"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_field_uses_default() {
        let dir = tmp();
        write_config(&dir, r#"{"language": "en"}"#);
        assert_eq!(cfg_in(&dir).meetings_dir(), dir.join("meetings"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_override_dir_wins() {
        let dir = tmp();
        let custom = tmp(); // a real, existing dir
        write_config(
            &dir,
            &format!(
                r#"{{"meeting_storage_path": {}}}"#,
                serde_json::to_string(&custom.to_string_lossy()).unwrap()
            ),
        );
        assert_eq!(cfg_in(&dir).meetings_dir(), custom);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&custom);
    }

    #[test]
    fn nonexistent_override_falls_back_to_default() {
        let dir = tmp();
        // Points at a dir that does not exist → guard rejects it.
        let bogus = dir.join("does-not-exist-xyz");
        write_config(
            &dir,
            &format!(
                r#"{{"meeting_storage_path": {}}}"#,
                serde_json::to_string(&bogus.to_string_lossy()).unwrap()
            ),
        );
        assert_eq!(cfg_in(&dir).meetings_dir(), dir.join("meetings"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_config_json_uses_default() {
        let dir = tmp();
        write_config(&dir, "not json {{{");
        assert_eq!(cfg_in(&dir).meetings_dir(), dir.join("meetings"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
