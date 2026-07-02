//! Notion REST API client — sends meeting recap markdown to a
//! user-picked Notion page or database.
//!
//! Why this exists: the user wants every meeting recap to land in
//! their Notion workspace as a structured page, alongside whatever
//! they already organise there. The integration is opt-in (settings
//! toggle), works with the user's own Notion integration token (no
//! Dimmy server in the middle), and uses the Notion REST API
//! directly via the existing `reqwest` dep.
//!
//! ## Auth model — internal integration tokens
//!
//! Notion supports two auth flows: OAuth (for public integrations
//! distributed via the Notion gallery) and internal integration
//! tokens (for personal / company-internal use). We use the latter:
//! the user goes to https://www.notion.so/my-integrations, clicks
//! "+ New integration", names it "Dimmy", picks their workspace,
//! type "Internal" → gets a static token (`ntn_...` for tokens minted
//! after 2024-09-25, `secret_...` for older tokens — both still
//! valid in 2026, treat as opaque string). The token is then pasted
//! into Dimmy and stored under [`crate::provider::KeyringScope::NotionToken`]
//! in the existing AES-256 keystore. Internal tokens are workspace-
//! scoped: one token per workspace.
//!
//! Per-page / per-database access is OPT-IN by the user inside Notion:
//! the user opens a page, clicks "..." → "Connections" → searches for
//! "Dimmy" → adds. Until that's done, our integration sees zero pages.
//! The [`search`] endpoint reflects this: it only returns objects the
//! integration has been granted access to.
//!
//! ## API version pin
//!
//! `Notion-Version: 2026-03-11` (current latest as of 2026-05-10).
//! Notion does NOT retire old versions; older ones remain functional
//! indefinitely. We pin a specific version because the API has
//! breaking changes (e.g. 2025-09-03 split databases into databases +
//! data_sources, `archived` field removed in 2026-03-11). Bumping the
//! pin requires reading the changelog and updating callers. See
//! https://developers.notion.com/reference/versioning.
//!
//! ## Markdown shortcut path (key simplification)
//!
//! Notion shipped a server-side markdown ingestion endpoint on
//! 2026-02-26: `POST /v1/pages` accepts a top-level `markdown` field
//! that Notion parses into block trees server-side. We use this
//! exclusively. We do NOT hand-roll a markdown → block-tree converter
//! — that's hundreds of lines of fragile parser code, and Notion's
//! server-side parser handles edge cases (tables, code fences, nested
//! quotes, etc.) better than anything we'd write.
//!
//! ## Rate limits
//!
//! Notion enforces ~3 requests/second per integration (no plan
//! distinction — Free / Plus / Business / Enterprise all share the
//! same API limit). 429 responses include a `Retry-After` header in
//! seconds. We don't currently implement automatic retry — the
//! caller (UI button click or auto-send-on-stop) is human-paced and
//! shouldn't hit it; if a future scenario needs bulk send we'll add
//! a backoff loop here.
//!
//! ## Rust core invariants honoured
//!
//! - HTTPS only (validated via `Provider::validate_url`-style check).
//! - 30-second timeout (configurable up to 600 s for large recaps).
//! - Error bodies truncated to 200 chars before logging or returning
//!   to the FFI caller — prevents leaking the user's token from any
//!   provider response that echoes the auth header.
//! - Token never logged in any code path. Only `has_token: bool` is
//!   ever surfaced to telemetry / status JSON.

use crate::error::TranscribeError;

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2026-03-11";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// One result from the `search` endpoint — a page or database the
/// integration has access to. Used by the Settings UI to populate the
/// "where do recaps land?" picker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotionSearchResult {
    /// Object UUID (without dashes-vs-no-dashes mattering — Notion
    /// accepts both forms in subsequent API calls).
    pub id: String,
    /// `"page"` or `"database"` (or `"data_source"` for v2025-09-03+
    /// data-sources, but we filter to `"page"` since the user picks a
    /// destination by parent page; rows-in-databases are scoped per-
    /// database via `parent.database_id`, see [`SendOptions`]).
    pub object: String,
    /// User-visible title — best-effort extraction from the response's
    /// `properties.title.title[0].plain_text` (page) or
    /// `title[0].plain_text` (database). Empty string if Notion
    /// returns no title (un-named blank page).
    pub title: String,
    /// Parent description ("Workspace", "Page: <name>", "Database: <name>")
    /// rendered for the picker so the user can disambiguate two
    /// pages with the same title.
    pub parent_label: String,
    /// Direct URL — the Settings UI uses this to render an "open in
    /// Notion" link next to each result.
    pub url: String,
}

/// Where the recap should land. Set in AppState by the picker; read by
/// [`send_meeting_recap`] every time we send.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NotionTarget {
    /// UUID of the parent (page or database). Empty string means
    /// "no target configured yet — show onboarding".
    pub id: String,
    /// `"page"` or `"database"`. Determines the request shape:
    /// page parent → `parent.page_id`, database parent → `parent.database_id`
    /// (and the title property name has to come from the database
    /// schema — for now we hardcode `"Name"` which is the Notion default
    /// for new databases; if the user picks a database with a
    /// different title-property name, the create call fails and we
    /// surface a clear error).
    pub kind: String,
    /// Display name shown in Settings ("Meeting Notes", "Engineering
    /// log", etc.). Pure UI sugar, not used by the API.
    pub title: String,
}

/// Result from a successful page-create call. UI uses `url` to offer
/// the user a "Open in Notion" button after a successful send.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreatedPage {
    pub id: String,
    pub url: String,
}

/// Workspace metadata returned by the `users/me` ping. UI shows
/// "Connected as <bot_name> in <workspace_name>" so the user can
/// confirm they pasted the right token before they pick a destination.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceInfo {
    /// The bot user's display name (typically the integration name —
    /// "Dimmy" if the user followed our onboarding instructions).
    pub bot_name: String,
    /// Workspace display name (e.g. "Konrad's Workspace").
    pub workspace_name: String,
}

fn http_client() -> Result<reqwest::Client, TranscribeError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| TranscribeError::Network(format!("reqwest build: {e}")))
}

fn truncate_for_log(s: &str) -> String {
    let max = 200usize;
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s.chars().take(max).collect::<String>();
        out.push('…');
        out
    }
}

/// Map a non-2xx HTTP response into a `TranscribeError::Network` with
/// a redacted, length-bounded message. Notion error bodies look like
/// `{"object":"error","status":401,"code":"unauthorized","message":"..."}`;
/// we extract `message` if present, else the raw body, else the
/// status code alone.
async fn map_http_error(resp: reqwest::Response) -> TranscribeError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    // Best-effort: parse `{"message":"..."}` and use that as the
    // user-facing reason. If parsing fails, fall back to the raw body.
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or(body);
    let truncated = truncate_for_log(&msg);
    TranscribeError::Network(format!("Notion API {}: {}", status, truncated))
}

/// Verify the token works by calling `GET /v1/users/me`. Cheap (<200 ms
/// typical) and confirms the token is valid + the integration's bot
/// can authenticate. Returns workspace + bot name for display.
pub async fn ping(token: &str) -> Result<WorkspaceInfo, TranscribeError> {
    let client = http_client()?;
    let resp = client
        .get(format!("{NOTION_API_BASE}/users/me"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Notion-Version", NOTION_VERSION)
        .send()
        .await
        .map_err(|e| TranscribeError::Network(format!("Notion ping: {e}")))?;
    if !resp.status().is_success() {
        return Err(map_http_error(resp).await);
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TranscribeError::Network(format!("Notion ping decode: {e}")))?;
    // The bot user has shape:
    //   { "object": "user", "id": "...", "name": "Dimmy",
    //     "type": "bot",
    //     "bot": { "owner": {...}, "workspace_name": "Konrad's Workspace" } }
    let bot_name = v
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let workspace_name = v
        .get("bot")
        .and_then(|b| b.get("workspace_name"))
        .and_then(|w| w.as_str())
        .unwrap_or("")
        .to_string();
    // Auth worked but the response shape wasn't what we expect — say so
    // in the log instead of silently rendering "Connected as (blank)"
    // in Settings (audit 2026-07-02: no silent failures).
    if bot_name.is_empty() || workspace_name.is_empty() {
        crate::log(&format!(
            "[Notion] WARN ping ok but response missing fields (name empty: {}, workspace empty: {}) — API shape changed?",
            bot_name.is_empty(),
            workspace_name.is_empty()
        ));
    }
    Ok(WorkspaceInfo {
        bot_name,
        workspace_name,
    })
}

/// Search for pages + databases the integration has access to. Used
/// by the Settings UI to render the "pick destination" picker.
///
/// `query` — optional case-insensitive substring; empty string lists
/// everything the integration can see.
///
/// Limit: Notion returns up to 100 results per page; we don't paginate
/// (typical user has 5-50 accessible objects). If the user has more,
/// they can refine the query string.
pub async fn search(token: &str, query: &str) -> Result<Vec<NotionSearchResult>, TranscribeError> {
    let client = http_client()?;
    // Filter is optional; without it we get pages + databases mixed.
    // For our picker the user can pick either, so don't filter — let
    // the UI render with a small badge per result kind.
    let body = serde_json::json!({
        "query": query,
        "page_size": 100,
    });
    let resp = client
        .post(format!("{NOTION_API_BASE}/search"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Notion-Version", NOTION_VERSION)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| TranscribeError::Network(format!("Notion search: {e}")))?;
    if !resp.status().is_success() {
        return Err(map_http_error(resp).await);
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TranscribeError::Network(format!("Notion search decode: {e}")))?;
    let mut out = Vec::new();
    let arr = v.get("results").and_then(|r| r.as_array());
    if arr.is_none() {
        // 2xx but no `results` array — API shape drift. Log it so an
        // empty destination picker in Settings is explainable instead
        // of looking broken (audit 2026-07-02: no silent failures).
        crate::log("[Notion] WARN search response missing 'results' array — API shape changed?");
    }
    if let Some(arr) = arr {
        for item in arr {
            let id = item
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            let object = item
                .get("object")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string();
            // Different title extraction per object type:
            //   page:     properties.<title-prop>.title[0].plain_text
            //             OR (root pages without title prop) take from
            //             the response's "title" field directly.
            //   database: title[0].plain_text
            let title = extract_title(item);
            let url = item
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            let parent_label = extract_parent_label(item);
            if id.is_empty() {
                continue;
            }
            out.push(NotionSearchResult {
                id,
                object,
                title,
                parent_label,
                url,
            });
        }
    }
    Ok(out)
}

/// Extract a user-readable title from a search result. Pages can have
/// the title in different properties depending on the parent type;
/// databases have it at the root. Best-effort.
fn extract_title(item: &serde_json::Value) -> String {
    // Database root title.
    if let Some(arr) = item.get("title").and_then(|t| t.as_array()) {
        if let Some(s) = arr
            .first()
            .and_then(|t| t.get("plain_text"))
            .and_then(|p| p.as_str())
        {
            return s.to_string();
        }
    }
    // Page properties — find the property whose type is "title".
    if let Some(props) = item.get("properties").and_then(|p| p.as_object()) {
        for (_name, prop) in props {
            if prop.get("type").and_then(|t| t.as_str()) == Some("title") {
                if let Some(arr) = prop.get("title").and_then(|a| a.as_array()) {
                    if let Some(s) = arr
                        .first()
                        .and_then(|t| t.get("plain_text"))
                        .and_then(|p| p.as_str())
                    {
                        return s.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

/// "Workspace", "Page: ...", "Database: ..." — for the picker so the
/// user can disambiguate same-titled objects.
fn extract_parent_label(item: &serde_json::Value) -> String {
    let parent = match item.get("parent") {
        Some(p) => p,
        None => return String::new(),
    };
    let kind = parent.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match kind {
        "workspace" => "Workspace".to_string(),
        "page_id" => "Page".to_string(),
        "database_id" => "Database".to_string(),
        "block_id" => "Sub-block".to_string(),
        _ => String::new(),
    }
}

/// Send a meeting recap to the user's chosen Notion target.
///
/// `title` — the page title (e.g. "Meeting 2026-05-10 14:30 — 17 chunks").
/// `markdown` — the structured-recap markdown produced by the existing
///              recap pipeline (`Helpers/MeetingRecapHelpers.cs::BuildMarkdownFromSections`
///              on Win, the Mac mirror, or the on-disk `recap.md`).
/// `target` — page-id or database-id parent (chosen by the user in
///            Settings; empty = caller error).
///
/// Uses the markdown content API (Notion 2026-02-26+) so we don't have
/// to hand-roll a markdown → block-tree converter.
pub async fn send_meeting_recap(
    token: &str,
    title: &str,
    markdown: &str,
    target: &NotionTarget,
) -> Result<CreatedPage, TranscribeError> {
    if token.is_empty() {
        return Err(TranscribeError::Network("Notion token not set".into()));
    }
    if target.id.is_empty() {
        return Err(TranscribeError::Network(
            "Notion target page not configured".into(),
        ));
    }
    if title.is_empty() {
        return Err(TranscribeError::Network("title is empty".into()));
    }
    if markdown.trim().is_empty() {
        return Err(TranscribeError::Network("markdown is empty".into()));
    }

    let client = http_client()?;

    // Build the parent + properties shape based on target kind.
    // Page parent: `parent.page_id` + properties.title only.
    // Database parent: `parent.database_id` + properties with the
    //   database's title-property name. Notion's default for new
    //   databases is "Name" (capitalized N). If the user has renamed
    //   their title property, the create call returns a 400 with a
    //   clear "Could not find property with name..." which we surface.
    let body = if target.kind == "database" {
        serde_json::json!({
            "parent": { "type": "database_id", "database_id": target.id },
            "properties": {
                "Name": {
                    "title": [{
                        "type": "text",
                        "text": { "content": title }
                    }]
                }
            },
            "markdown": markdown,
        })
    } else {
        // Default to page parent (kind="page" or anything else).
        serde_json::json!({
            "parent": { "type": "page_id", "page_id": target.id },
            "properties": {
                "title": {
                    "title": [{
                        "type": "text",
                        "text": { "content": title }
                    }]
                }
            },
            "markdown": markdown,
        })
    };

    let resp = client
        .post(format!("{NOTION_API_BASE}/pages"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Notion-Version", NOTION_VERSION)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| TranscribeError::Network(format!("Notion create page: {e}")))?;
    if !resp.status().is_success() {
        return Err(map_http_error(resp).await);
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TranscribeError::Network(format!("Notion create page decode: {e}")))?;
    let id = v
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("")
        .to_string();
    let url = v
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return Err(TranscribeError::Network(
            "Notion create page: response missing id".into(),
        ));
    }
    Ok(CreatedPage { id, url })
}

/// Best-effort title from a meeting directory: the directory name is
/// already a UUID, but `meta.json` should have `start_ts`. Fallback to
/// the directory leaf if parsing fails. The recap pipeline is
/// independent of this — title is purely the Notion page label.
pub fn meeting_dir_to_title(meeting_dir: &std::path::Path, transcript: &str) -> String {
    use chrono::TimeZone;
    // Try to read meta.json for the start timestamp; if available
    // format as "Meeting YYYY-MM-DD HH:MM".
    let meta_path = meeting_dir.join("meta.json");
    if let Ok(content) = std::fs::read_to_string(&meta_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(ts_secs) = v.get("start_ts").and_then(|t| t.as_i64()) {
                if let chrono::LocalResult::Single(dt) = chrono::Local.timestamp_opt(ts_secs, 0) {
                    let stamp = dt.format("%Y-%m-%d %H:%M").to_string();
                    // If we have a transcript, append the first ~6
                    // words of speech so the page title carries a hint
                    // of what the meeting was about. Keep total under
                    // 80 chars (Notion accepts up to 2000 but UX gets
                    // ugly past 80).
                    let hint = transcript_first_words(transcript, 6);
                    if hint.is_empty() {
                        return format!("Meeting {stamp}");
                    } else {
                        return truncate_title(&format!("Meeting {stamp} — {hint}"), 80);
                    }
                }
            }
        }
    }
    // Last resort: directory leaf name.
    meeting_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| format!("Meeting {s}"))
        .unwrap_or_else(|| "Meeting".to_string())
}

fn transcript_first_words(transcript: &str, n: usize) -> String {
    // The transcript file lines look like `[12345 ms] [mic] hello world`.
    // Strip the metadata prefix, then take first N tokens of actual content.
    let mut words = Vec::new();
    for line in transcript.lines() {
        let content = line
            .splitn(3, "] ")
            .last()
            .unwrap_or(line)
            .trim()
            .to_string();
        for w in content.split_whitespace() {
            if words.len() >= n {
                break;
            }
            words.push(w.to_string());
        }
        if words.len() >= n {
            break;
        }
    }
    words.join(" ")
}

fn truncate_title(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_for_log_under_max() {
        assert_eq!(truncate_for_log("short"), "short");
    }

    #[test]
    fn truncate_for_log_over_max() {
        let s = "a".repeat(300);
        let out = truncate_for_log(&s);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 201);
    }

    #[test]
    fn extract_title_from_page_properties() {
        let v = serde_json::json!({
            "properties": {
                "Name": {
                    "type": "title",
                    "title": [{"plain_text": "My Meeting Notes"}]
                }
            }
        });
        assert_eq!(extract_title(&v), "My Meeting Notes");
    }

    #[test]
    fn extract_title_from_database_root() {
        let v = serde_json::json!({
            "title": [{"plain_text": "Engineering Log"}]
        });
        assert_eq!(extract_title(&v), "Engineering Log");
    }

    #[test]
    fn extract_title_returns_empty_when_no_title() {
        let v = serde_json::json!({"properties": {}});
        assert_eq!(extract_title(&v), "");
    }

    #[test]
    fn extract_parent_label_workspace() {
        let v = serde_json::json!({"parent": {"type": "workspace"}});
        assert_eq!(extract_parent_label(&v), "Workspace");
    }

    #[test]
    fn extract_parent_label_page() {
        let v = serde_json::json!({"parent": {"type": "page_id", "page_id": "abc"}});
        assert_eq!(extract_parent_label(&v), "Page");
    }

    #[test]
    fn extract_parent_label_database() {
        let v = serde_json::json!({"parent": {"type": "database_id", "database_id": "xyz"}});
        assert_eq!(extract_parent_label(&v), "Database");
    }

    #[test]
    fn transcript_first_words_handles_metadata_prefix() {
        let t = "[0 ms] [mic] hello world this is the test\n[1000 ms] [system] more text";
        assert_eq!(transcript_first_words(t, 4), "hello world this is");
    }

    #[test]
    fn transcript_first_words_empty_input() {
        assert_eq!(transcript_first_words("", 5), "");
    }

    #[test]
    fn truncate_title_under_max() {
        assert_eq!(truncate_title("short", 20), "short");
    }

    #[test]
    fn truncate_title_at_max() {
        let s = "a".repeat(80);
        assert_eq!(truncate_title(&s, 80), s);
    }

    #[test]
    fn truncate_title_over_max_appends_ellipsis() {
        let s = "abcdefghij";
        let out = truncate_title(s, 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn meeting_dir_to_title_falls_back_to_leaf_when_no_meta() {
        let tmp = std::env::temp_dir().join("dimmy-test-no-meta");
        let _ = std::fs::create_dir_all(&tmp);
        let title = meeting_dir_to_title(&tmp, "");
        assert!(title.starts_with("Meeting "));
        assert!(title.contains("dimmy-test-no-meta"));
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn send_meeting_recap_rejects_empty_token() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = NotionTarget {
            id: "abc".into(),
            kind: "page".into(),
            title: "Notes".into(),
        };
        let r = rt.block_on(send_meeting_recap("", "T", "M", &target));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("token"));
    }

    #[test]
    fn send_meeting_recap_rejects_empty_target() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = NotionTarget::default();
        let r = rt.block_on(send_meeting_recap("ntn_x", "T", "M", &target));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("target"));
    }

    #[test]
    fn send_meeting_recap_rejects_empty_markdown() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = NotionTarget {
            id: "abc".into(),
            kind: "page".into(),
            title: "Notes".into(),
        };
        let r = rt.block_on(send_meeting_recap("ntn_x", "T", "   ", &target));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("markdown"));
    }
}
