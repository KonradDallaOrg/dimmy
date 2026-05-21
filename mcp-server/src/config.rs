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

    pub fn meetings_dir(&self) -> PathBuf {
        self.config_dir.join("meetings")
    }

    /// Append-only audit log of tool calls. Each line is JSON
    /// `{"ts": ..., "tool": "...", "ok": true}`. Settings UI tails
    /// this to render "last used X min ago".
    pub fn calls_log(&self) -> PathBuf {
        self.config_dir.join("mcp.calls.log")
    }
}
