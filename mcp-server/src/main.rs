//! Dimmy ↔ Claude Desktop bridge via the Model Context Protocol.
//!
//! Architecture
//! ============
//! Claude Desktop spawns this binary as a stdio subprocess when it
//! starts up (config entry in `claude_desktop_config.json`). The
//! protocol is JSON-RPC 2.0 over newline-delimited frames; we read
//! stdin, dispatch, write to stdout. No network, no threads beyond
//! the heartbeat task.
//!
//! Tools exposed:
//!   • `dimmy_get_recent_meetings(limit)` — list recent meetings
//!   • `dimmy_get_meeting(id)`           — transcript + metadata
//!   • `dimmy_save_recap(id, markdown)`   — write recap.md back into
//!                                          the meeting dir
//!   • `dimmy_get_recent_dictations(limit)` — last N dictations
//!
//! Side effects
//! ============
//! - `<config_dir>/mcp.heartbeat` : touched every 30 s (Dimmy uses
//!   mtime to render status)
//! - `<config_dir>/mcp.calls.log` : append-only audit log of every
//!   tool call (ts + name + success). Used by Settings UI.
//!
//! Config dir resolution honours the `DIMMY_CONFIG_NAMESPACE` env
//! var so prod (default `dimmy`) and staging (`dimmy-staging`) can
//! coexist with separate MCP entries pointing at the same binary.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

mod config;
mod protocol;
mod tools;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    // Initialize tracing — Claude Desktop captures stderr into its
    // MCP log file, so a `tracing::info!` from here is immediately
    // visible to the user as `~/Library/Logs/Claude/mcp-server-dimmy.log`.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = config::Config::resolve();
    tracing::info!(
        "dimmy-mcp starting, namespace={}, config_dir={:?}",
        cfg.namespace,
        cfg.config_dir
    );

    // Heartbeat — touches a file every 30 s so the Dimmy UI can
    // tell whether Claude Desktop has loaded us. Spawned as a
    // background task; cancelled on process exit.
    let heartbeat_path = cfg.config_dir.join("mcp.heartbeat");
    tokio::spawn(heartbeat_loop(heartbeat_path));

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    let server_state = std::sync::Arc::new(tokio::sync::Mutex::new(ServerState::default()));

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: protocol::Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("bad JSON-RPC frame: {} — payload: {}", e, line);
                // Per JSON-RPC 2.0, malformed input on a notification
                // is silent; on a request we'd reply with error -32700.
                // Since we can't parse the id, the safest behaviour is
                // log + drop.
                continue;
            }
        };

        let response = handle_request(request, &cfg, &server_state).await;
        if let Some(resp) = response {
            let serialized = serde_json::to_string(&resp).unwrap_or_else(|e| {
                tracing::error!("response serialize failed: {}", e);
                String::new()
            });
            if !serialized.is_empty() {
                stdout.write_all(serialized.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
    }

    tracing::info!("dimmy-mcp shutting down (stdin closed)");
    Ok(())
}

/// Per-session state mutated across requests. Currently tracks whether
/// the client has completed the `initialize` handshake; we silently
/// reject `tools/*` calls before initialization to match the spec.
#[derive(Default)]
struct ServerState {
    initialized: bool,
}

async fn handle_request(
    req: protocol::Request,
    cfg: &config::Config,
    state: &std::sync::Arc<tokio::sync::Mutex<ServerState>>,
) -> Option<protocol::Response> {
    let id = req.id.clone();
    let is_notification = id.is_none();

    let result = match req.method.as_str() {
        "initialize" => {
            let mut s = state.lock().await;
            s.initialized = true;
            Ok(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "dimmy",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }))
        }
        // Per spec, `initialized` and `notifications/initialized` are
        // both seen in the wild; the second is the canonical name.
        // Both are notifications (no id, no response).
        "initialized" | "notifications/initialized" => return None,
        "tools/list" => {
            let s = state.lock().await;
            if !s.initialized {
                Err(protocol::Error::not_initialized())
            } else {
                Ok(serde_json::json!({ "tools": tools::list() }))
            }
        }
        "tools/call" => {
            let s = state.lock().await;
            if !s.initialized {
                Err(protocol::Error::not_initialized())
            } else {
                drop(s);
                tools::dispatch(req.params.unwrap_or_default(), cfg).await
            }
        }
        // Spec-defined utility methods we don't implement — return
        // `method not found` so the client can fall back.
        "ping" => Ok(serde_json::json!({})),
        other => {
            tracing::debug!("unknown method: {}", other);
            Err(protocol::Error::method_not_found(other))
        }
    };

    if is_notification {
        return None;
    }

    Some(match result {
        Ok(result) => protocol::Response::success(id, result),
        Err(err) => protocol::Response::error(id, err),
    })
}

async fn heartbeat_loop(path: PathBuf) {
    // Ensure the parent dir exists. The config dir is normally
    // created by Dimmy on first launch; on a Claude-Desktop-first
    // boot order it may not exist yet.
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!("heartbeat parent dir create failed: {}", e);
        }
    }
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let payload = HeartbeatPayload {
            timestamp: now,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let json = serde_json::to_string(&payload).unwrap_or_default();
        if let Err(e) = tokio::fs::write(&path, json).await {
            tracing::warn!("heartbeat write failed: {}", e);
        }
    }
}

#[derive(Serialize, Deserialize)]
struct HeartbeatPayload {
    timestamp: u64,
    version: String,
}
