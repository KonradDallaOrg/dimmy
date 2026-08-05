//! Tool definitions + dispatch.
//!
//! Each tool is a pure function over the filesystem state:
//! Claude calls it with JSON args, we read/write the meetings dir,
//! return JSON. No mutation of Dimmy's running state — the
//! MeetingWindow filesystem watcher picks up `recap.md` changes
//! independently.
//!
//! Privacy: tools only expose meetings (not dictations by default —
//! `get_recent_dictations` is a separate explicit tool the user can
//! see in Claude's tools list and decide whether to allow).

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::config::Config;
use crate::protocol::Error;

/// Tool definitions in the format Claude Desktop expects for the
/// `tools/list` response. Static — never changes at runtime.
pub fn list() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "dimmy_get_recent_meetings",
            "description": "List the most recent meetings recorded by Dimmy. Returns id, title, timestamp, duration. Use this when the user asks about \"recent meetings\", \"today's meetings\", or wants to pick one to recap.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of meetings to return (default 10, max 50).",
                        "minimum": 1,
                        "maximum": 50
                    }
                }
            }
        }),
        json!({
            "name": "dimmy_search",
            "description": "Search across ALL recorded meetings (titles + transcripts + recaps) by keyword, topic, person, or phrase. Forgiving: tolerates typos, word forms (plural/singular), accents, and partial words. Returns compact ranked results — id, title, date, a short highlighted snippet (the match is wrapped in « »), and whether all your words matched — NOT the full transcript. Use this whenever the user references a TOPIC/keyword rather than \"recent\" (e.g. \"the meeting about NFC tags\", \"when did we discuss pricing\"); then call dimmy_get_meeting with the chosen id for the full content. Much cheaper than fetching meetings one by one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Free-text search terms (one or more words). Word order does not matter."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results to return (default 8, max 25).",
                        "minimum": 1,
                        "maximum": 25
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "dimmy_get_meeting",
            "description": "Retrieve the full transcript, metadata, and any existing recap for a specific meeting. Call this after dimmy_get_recent_meetings to fetch the content for the meeting the user wants to discuss. The transcript is plain text with [elapsed_ms] [speaker] line format.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Meeting directory name (UUID-like, returned by dimmy_get_recent_meetings)."
                    }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "dimmy_save_recap",
            "description": "Write the recap markdown back into the meeting directory as recap.md. Dimmy's UI will pick up the file automatically and display it in the Done view. Call this after generating a recap so the user finds it persisted next time they open the meeting.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Meeting directory name."
                    },
                    "markdown": {
                        "type": "string",
                        "description": "Recap content as Markdown. Suggested structure: ## TLDR / ## Decisions / ## Action items."
                    }
                },
                "required": ["id", "markdown"]
            }
        }),
        json!({
            "name": "dimmy_get_recent_dictations",
            "description": "List the most recent dictations (single-shot transcribed text from the Dimmy pill). Returns id, text, language, timestamp. Use when the user asks about \"what did I dictate\" or wants to summarise short notes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of dictations to return (default 20, max 100).",
                        "minimum": 1,
                        "maximum": 100
                    }
                }
            }
        }),
        json!({
            "name": "dimmy_get_recap_template",
            "description": "Return the meeting-recap Markdown template Dimmy expects. Call this BEFORE producing a recap so your output uses Dimmy's house format (first line is a short H1 title, then TLDR / Decisions / Action items / Key notes sections in the transcript's language). The user can customize the template by editing `<config_dir>/recap-template.md`; this tool returns that file when present, else the shipped default.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

/// Top-level dispatch for `tools/call`. Reads the tool name from
/// params, validates, runs, and returns a content-array result OR
/// a JSON-RPC error.
pub async fn dispatch(params: serde_json::Value, cfg: &Config) -> Result<serde_json::Value, Error> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::invalid_params("missing `name`"))?
        .to_string();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    let started = std::time::Instant::now();
    let result = match name.as_str() {
        "dimmy_get_recent_meetings" => get_recent_meetings(args, cfg).await,
        "dimmy_search" => search_meetings(args, cfg).await,
        "dimmy_get_meeting" => get_meeting(args, cfg).await,
        "dimmy_save_recap" => save_recap(args, cfg).await,
        "dimmy_get_recent_dictations" => get_recent_dictations(args, cfg).await,
        "dimmy_get_recap_template" => get_recap_template(cfg).await,
        other => Err(Error::invalid_params(&format!("unknown tool: {}", other))),
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;

    log_call(cfg, &name, result.is_ok(), elapsed_ms).await;

    result
}

#[derive(Serialize, Deserialize)]
struct MeetingSummary {
    id: String,
    title: String,
    started_at: String,
    duration_secs: f64,
    has_recap: bool,
}

async fn get_recent_meetings(
    args: serde_json::Value,
    cfg: &Config,
) -> Result<serde_json::Value, Error> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .min(50) as usize;

    let dir = cfg.meetings_dir();
    let mut entries = vec![];
    let mut read = match tokio::fs::read_dir(&dir).await {
        Ok(r) => r,
        Err(e) => {
            return Ok(text_result(&format!(
                "No meetings directory found at {:?}: {}. Dimmy may not have recorded any meetings yet.",
                dir, e
            )));
        }
    };
    while let Ok(Some(entry)) = read.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let mtime = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        entries.push((path, mtime));
    }
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    entries.truncate(limit);

    let mut summaries = Vec::with_capacity(entries.len());
    for (path, _) in entries {
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let meta = read_meta_json(&path).await;
        let has_recap = tokio::fs::metadata(path.join("recap.md")).await.is_ok();
        summaries.push(MeetingSummary {
            id,
            title: meta
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Meeting")
                .to_string(),
            started_at: meta
                .get("started_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            duration_secs: meta
                .get("duration_secs")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            has_recap,
        });
    }

    let body = serde_json::to_string_pretty(&summaries)
        .map_err(|e| Error::internal(&format!("serialize: {}", e)))?;
    Ok(text_result(&body))
}

async fn search_meetings(
    args: serde_json::Value,
    cfg: &Config,
) -> Result<serde_json::Value, Error> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::invalid_params("missing `query`"))?
        .to_string();
    if query.trim().is_empty() {
        return Err(Error::invalid_params("query cannot be empty"));
    }
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;

    // The index build + ranking is synchronous filesystem work; run it on
    // a blocking thread so the (current_thread) async runtime never stalls.
    let dir = cfg.meetings_dir();
    let q = query.clone();
    let hits = tokio::task::spawn_blocking(move || crate::search::run(&dir, &q, limit))
        .await
        .map_err(|e| Error::internal(&format!("search task: {}", e)))?;

    if hits.is_empty() {
        return Ok(text_result(&format!(
            "No meetings matched \"{}\". Try fewer or different words, or use dimmy_get_recent_meetings to list everything.",
            query
        )));
    }
    let body = serde_json::to_string_pretty(&hits)
        .map_err(|e| Error::internal(&format!("serialize: {}", e)))?;
    Ok(text_result(&body))
}

async fn get_meeting(args: serde_json::Value, cfg: &Config) -> Result<serde_json::Value, Error> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::invalid_params("missing `id`"))?;
    validate_meeting_id(id)?;

    let dir = cfg.meetings_dir().join(id);
    if tokio::fs::metadata(&dir).await.is_err() {
        return Ok(text_result_error(&format!("Meeting `{}` not found.", id)));
    }

    let meta = read_meta_json(&dir).await;
    let transcript = tokio::fs::read_to_string(dir.join("transcripts.txt"))
        .await
        .unwrap_or_default();
    let recap = tokio::fs::read_to_string(dir.join("recap.md")).await.ok();
    let notes = tokio::fs::read_to_string(dir.join("notes.md")).await.ok();
    let audio_path = if tokio::fs::metadata(dir.join("audio.wav")).await.is_ok() {
        Some(dir.join("audio.wav").to_string_lossy().to_string())
    } else {
        None
    };

    let payload = json!({
        "id": id,
        "title": meta.get("title").and_then(|v| v.as_str()).unwrap_or("Meeting"),
        "started_at": meta.get("started_at").and_then(|v| v.as_str()).unwrap_or(""),
        "duration_secs": meta.get("duration_secs").and_then(|v| v.as_f64()).unwrap_or(0.0),
        "transcript": transcript,
        "existing_recap": recap,
        "user_notes": notes,
        "audio_path": audio_path,
    });

    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| Error::internal(&format!("serialize: {}", e)))?;
    Ok(text_result(&body))
}

async fn save_recap(args: serde_json::Value, cfg: &Config) -> Result<serde_json::Value, Error> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::invalid_params("missing `id`"))?;
    let markdown = args
        .get("markdown")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::invalid_params("missing `markdown`"))?;
    validate_meeting_id(id)?;

    if markdown.is_empty() {
        return Err(Error::invalid_params("markdown cannot be empty"));
    }

    let dir = cfg.meetings_dir().join(id);
    if tokio::fs::metadata(&dir).await.is_err() {
        return Ok(text_result_error(&format!(
            "Meeting `{}` not found. Cannot save recap.",
            id
        )));
    }

    let path = dir.join("recap.md");
    let marked = mark_ai_generated(markdown);
    tokio::fs::write(&path, &marked)
        .await
        .map_err(|e| Error::internal(&format!("write recap.md: {}", e)))?;

    // Parse the first H1 from the recap and patch meta.json so the
    // Dimmy UI shows the LLM-chosen title instead of the meeting id.
    // Mirrors the FFI path in core::meeting::save_post_process —
    // same rules, intentionally duplicated here so mcp-server stays
    // dependency-free from the core crate.
    if let Some(title) = parse_first_h1(markdown) {
        let meta_path = dir.join("meta.json");
        let mut obj: serde_json::Map<String, serde_json::Value> =
            tokio::fs::read_to_string(&meta_path)
                .await
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        obj.insert("title".into(), serde_json::Value::String(title.clone()));
        if let Ok(serialized) = serde_json::to_string_pretty(&obj) {
            let _ = tokio::fs::write(&meta_path, serialized).await;
        }
        tracing::info!("[save_recap] meeting={} title='{}'", id, title);
    }

    Ok(text_result(&format!(
        "Recap saved to {:?} ({} chars). Dimmy will pick it up in the meeting Done view.",
        path,
        markdown.len()
    )))
}

async fn get_recap_template(cfg: &Config) -> Result<serde_json::Value, Error> {
    // User override lives next to the calls log so power users can
    // tweak the prompt without rebuilding the binary. Falls back to
    // the shipped default embedded at compile time.
    const DEFAULT_TEMPLATE: &str = include_str!("../templates/recap.md");
    let override_path = cfg.config_dir.join("recap-template.md");
    let body = match tokio::fs::read_to_string(&override_path).await {
        Ok(s) if !s.trim().is_empty() => s,
        _ => DEFAULT_TEMPLATE.to_string(),
    };
    Ok(text_result(&body))
}

/// Mirror of `core::meeting::AI_GENERATED_TAG`. See `mark_ai_generated` below.
const AI_GENERATED_TAG: &str = "<!-- dimmy-ai-generated: true; by: Dimmy -->";

/// Mirror of `core::meeting::mark_ai_generated`. Duplicated for the same
/// reason as `parse_first_h1`: mcp-server stays dependency-free from the core
/// crate. The two implementations MUST stay in sync.
///
/// A recap arriving through this tool is written by an LLM in Claude Desktop,
/// so it is every bit as synthetic as one Dimmy generates itself and carries
/// the same AI Act art. 50(2) marking obligation. Forgetting it here would
/// leave a hole exactly where the text is MOST likely to be pasted somewhere
/// public.
fn mark_ai_generated(md: &str) -> String {
    if md.contains("dimmy-ai-generated:") || md.trim().is_empty() {
        return md.to_string();
    }
    let mut out = String::with_capacity(md.len() + AI_GENERATED_TAG.len() + 2);
    let mut lines = md.lines();
    let mut inserted = false;
    for line in lines.by_ref() {
        out.push_str(line);
        out.push('\n');
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with("# ") {
            out.push_str(AI_GENERATED_TAG);
            out.push('\n');
        } else {
            out.clear();
            out.push_str(AI_GENERATED_TAG);
            out.push('\n');
            out.push_str(line);
            out.push('\n');
        }
        inserted = true;
        break;
    }
    if !inserted {
        return md.to_string();
    }
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    if !md.ends_with('\n') {
        out.pop();
    }
    out
}

/// Mirror of `core::meeting::parse_recap_title`. Duplicated to avoid
/// depending on the core crate (mcp-server is intentionally light:
/// no whisper, no llama, no audio runtimes). The two implementations
/// MUST stay in sync — if you change the rule here, change it in
/// core/src/meeting.rs too. Tested below.
fn parse_first_h1(md: &str) -> Option<String> {
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest
                .trim()
                .trim_matches(|c: char| c == '.' || c == ':' || c == ',' || c.is_whitespace());
            if title.is_empty() || title.len() > 200 {
                return None;
            }
            return Some(title.to_string());
        }
        return None;
    }
    None
}

async fn get_recent_dictations(
    args: serde_json::Value,
    _cfg: &Config,
) -> Result<serde_json::Value, Error> {
    let _limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);
    // Dictations live in the SQLite history database (`history.db`).
    // Reading SQLite from outside the core process is doable via
    // rusqlite but pulls a non-trivial dep into this minimal binary.
    // Phase 1: stub with a clear message; phase 2 (follow-up) wires
    // it up via a small fileformat shim or by having Dimmy core dump
    // a JSON snapshot on every dictation completion.
    Ok(text_result(
        "Dictation history is not exposed in this version of the Dimmy MCP bridge. \
         Only meetings are queryable for now. Tell the user to use the meetings tools instead.",
    ))
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Reject path-traversal attempts on the meeting id. The id is the
/// directory name; anything containing `..` or `/` could escape the
/// meetings root and read arbitrary files. Defensive belt-and-braces
/// — Claude is a remote model and the id comes from its tool call.
fn validate_meeting_id(id: &str) -> Result<(), Error> {
    if id.is_empty() {
        return Err(Error::invalid_params("id cannot be empty"));
    }
    if id.contains("..") || id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(Error::invalid_params(
            "id must be a plain directory name (no path separators or traversal)",
        ));
    }
    Ok(())
}

async fn read_meta_json(dir: &std::path::Path) -> serde_json::Value {
    let path = dir.join("meta.json");
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Object(Default::default())),
        Err(_) => serde_json::Value::Object(Default::default()),
    }
}

/// MCP tool-call response: `{"content": [{"type": "text", "text": ...}]}`.
fn text_result(text: &str) -> serde_json::Value {
    json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn text_result_error(text: &str) -> serde_json::Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
}

async fn log_call(cfg: &Config, tool: &str, ok: bool, elapsed_ms: u64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = json!({
        "ts": now,
        "tool": tool,
        "ok": ok,
        "elapsed_ms": elapsed_ms,
    });
    let mut line = entry.to_string();
    line.push('\n');

    let path = cfg.calls_log();
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        let _ = file.write_all(line.as_bytes()).await;
        let _ = file.flush().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_meeting_id_rejects_traversal() {
        assert!(validate_meeting_id("../../etc/passwd").is_err());
        assert!(validate_meeting_id("foo/bar").is_err());
        assert!(validate_meeting_id("foo\\bar").is_err());
        assert!(validate_meeting_id("foo\0bar").is_err());
        assert!(validate_meeting_id("").is_err());
        assert!(validate_meeting_id("163c8b5b-19a9-4129-98a5-b4a4e53f7436").is_ok());
        assert!(validate_meeting_id("20260520T211149-uuid").is_ok());
    }

    #[test]
    fn list_returns_six_tools_with_required_fields() {
        let tools = list();
        assert_eq!(tools.len(), 6);
        for t in &tools {
            assert!(t.get("name").and_then(|v| v.as_str()).is_some());
            assert!(t.get("description").and_then(|v| v.as_str()).is_some());
            assert!(t.get("inputSchema").is_some());
        }
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"dimmy_get_recent_meetings"));
        assert!(names.contains(&"dimmy_search"));
        assert!(names.contains(&"dimmy_get_meeting"));
        assert!(names.contains(&"dimmy_save_recap"));
        assert!(names.contains(&"dimmy_get_recent_dictations"));
        assert!(names.contains(&"dimmy_get_recap_template"));
    }

    #[test]
    fn parse_first_h1_happy_path() {
        let md = "# Sprint planning kick-off\n\n## TLDR\nWe agreed on the goals.";
        assert_eq!(
            parse_first_h1(md).as_deref(),
            Some("Sprint planning kick-off")
        );
    }

    #[test]
    fn parse_first_h1_strips_trailing_punctuation() {
        // LLMs sometimes emit "# Title." or "# Title :" — strip.
        assert_eq!(parse_first_h1("# Demo call.").as_deref(), Some("Demo call"));
        assert_eq!(
            parse_first_h1("# Q3 review :").as_deref(),
            Some("Q3 review")
        );
    }

    #[test]
    fn parse_first_h1_skips_leading_blank_lines() {
        // Some providers prepend newlines — tolerate.
        assert_eq!(
            parse_first_h1("\n\n# Title\nbody").as_deref(),
            Some("Title")
        );
    }

    #[test]
    fn parse_first_h1_rejects_non_h1_first_line() {
        // If the first non-empty line isn't an H1, we don't scan
        // further — the convention is "title is first or nothing".
        assert!(parse_first_h1("## TLDR\n# Title").is_none());
        assert!(parse_first_h1("Some preamble\n# Title").is_none());
    }

    #[test]
    fn parse_first_h1_rejects_pathological_lengths() {
        // Guard against the LLM dumping the whole recap on a single
        // H1 line (would otherwise become the "title").
        let long_title = "a".repeat(300);
        let md = format!("# {}", long_title);
        assert!(parse_first_h1(&md).is_none());
        // Empty after the `# ` also rejected.
        assert!(parse_first_h1("# ").is_none());
    }

    #[test]
    fn mark_ai_generated_matches_the_core_rules() {
        // Must stay byte-identical in behaviour to core::meeting::
        // mark_ai_generated. Title-bearing recap: tag goes on line 2 so the
        // H1 stays first and parse_first_h1 still finds it.
        let with_title = "# Titolo\n\n## Context\n\nTesto.\n";
        let out = mark_ai_generated(with_title);
        let mut lines = out.lines();
        assert_eq!(lines.next(), Some("# Titolo"));
        assert_eq!(lines.next(), Some(AI_GENERATED_TAG));
        assert_eq!(parse_first_h1(&out).as_deref(), Some("Titolo"));

        // No title: tag on top, which costs nothing since parse_first_h1
        // already returns None there.
        let no_title = "## Context\n\nTesto.\n";
        assert!(mark_ai_generated(no_title).starts_with(AI_GENERATED_TAG));

        // Idempotent: Claude Desktop can re-save the same recap.
        let once = mark_ai_generated(with_title);
        assert_eq!(mark_ai_generated(&once), once);
        assert_eq!(once.matches("dimmy-ai-generated:").count(), 1);

        // Never drops content.
        for line in with_title.lines() {
            assert!(out.contains(line), "lost line {line:?}");
        }

        // Empty input is left alone.
        assert_eq!(mark_ai_generated(""), "");
    }
}
