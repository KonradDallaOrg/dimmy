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
    // Stdout is shared between the request handler and the keepalive
    // task, so wrap it in a Mutex. Single-writer-at-a-time on the
    // newline-delimited JSON-RPC pipe is mandatory — overlapping
    // writes corrupt the frame boundary.
    let stdout = std::sync::Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));

    let server_state = std::sync::Arc::new(tokio::sync::Mutex::new(ServerState::default()));

    // Keepalive — send a `notifications/message` to Claude every 30 s
    // so the stdio pipe stays "active" and Claude's idle-kill timer
    // doesn't fire. Without this Claude kills the server after ~5 min
    // of no tool calls AND DOESN'T respawn it for subsequent tool
    // requests — the next call sits there until the 4-minute MCP
    // client timeout. Burned 2026-05-22 (every recap attempt after
    // the first failed for the next 12 minutes because Claude refused
    // to spawn a fresh server). `notifications/message` is the
    // standard MCP server-initiated log notification; Claude either
    // logs it or ignores it but counts it as server activity.
    {
        let stdout_clone = stdout.clone();
        let state_clone = server_state.clone();
        tokio::spawn(async move {
            keepalive_loop(stdout_clone, state_clone).await;
        });
    }

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
                let mut out = stdout.lock().await;
                out.write_all(serialized.as_bytes()).await?;
                out.write_all(b"\n").await?;
                out.flush().await?;
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
            // Keep this response SMALL. We used to embed a 35-KB
            // base64 PNG in `iconUrl` + `_meta.icon` as a
            // speculative attempt to feed Claude Desktop an icon
            // through serverInfo. It blew up the initialize
            // response to ~70 KB JSON and Claude Desktop choked
            // when parsing it — `dimmy_get_recap_template` timed
            // out after 4 minutes because Claude never finished
            // the connection handshake. Icon now ships only via
            // `Claude Extensions/dimmy/icon.png` (the canonical
            // DXT-manifest path Anthropic's own extensions use),
            // which Claude reads from disk on demand. Keep
            // serverInfo minimal.
            Ok(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "dimmy",
                    "title": "Dimmy",
                    "version": env!("CARGO_PKG_VERSION"),
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

/// Push `notifications/message` frames to Claude every 30 s so the
/// stdio pipe never goes "idle" from Claude's perspective. Without
/// this Claude Desktop kills the server after ~5 min idle AND
/// doesn't respawn for subsequent tool calls — they just sit there
/// until the 4-minute client timeout fires. Notification only fires
/// after `initialize` succeeded; before that, Claude isn't ready
/// to receive server-initiated messages.
async fn keepalive_loop(
    stdout: std::sync::Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    state: std::sync::Arc<tokio::sync::Mutex<ServerState>>,
) {
    // 5 s instead of 30 s: Claude Desktop kills connections that go
    // silent for >~10 s after the initialize handshake — even if the
    // server is technically alive. Burned 2026-05-22 (server killed
    // 11 s after spawn because the first keepalive at +30 s never
    // got a chance to fire). 5 s gives us a healthy margin under
    // Claude's tolerance AND stays well under the cache TTL that
    // Anthropic-side servers use for stale-connection detection.
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    // First tick fires immediately — skip it so we don't race with
    // initialize.
    tick.tick().await;
    loop {
        tick.tick().await;
        {
            let s = state.lock().await;
            if !s.initialized {
                continue;
            }
        }
        let frame = r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"debug","logger":"dimmy","data":"keepalive"}}"#;
        let mut out = stdout.lock().await;
        let _ = out.write_all(frame.as_bytes()).await;
        let _ = out.write_all(b"\n").await;
        let _ = out.flush().await;
    }
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

// `dimmy_icon_data_uri` was removed 2026-05-22 — it embedded a
// 35-KB base64 PNG in the `initialize` serverInfo response and that
// inflated the JSON-RPC frame to ~70 KB, which is enough to make
// Claude Desktop's parser stall on the first connect (next tool call
// times out after 4 minutes with no useful diagnostic). The
// extension's icon now ships only via `Claude Extensions/dimmy/
// icon.png` referenced from the DXT manifest, which is the same
// path Anthropic's own extensions use and is read from disk on
// demand. Don't reintroduce the data-URI inline — keep the
// initialize response under ~1 KB.
