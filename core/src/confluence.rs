//! Confluence Cloud integration — send a meeting recap to a wiki page.
//!
//! The corporate sibling of [`crate::notion`]. Same shape deliberately: one
//! user-supplied credential in the keystore, a connection test, a destination
//! picker, and one send call. Read that module first; this one only documents
//! where Confluence forced a different choice.
//!
//! ## Auth: email + API token, HTTP Basic
//!
//! Atlassian Cloud has no personal bearer token. The equivalent of Notion's
//! internal-integration token is an **API token** created at
//! id.atlassian.com/manage-profile/security/api-tokens, sent as
//! `Basic base64(email:token)`. So we need two fields where Notion needed one,
//! plus the site host (`yourcompany.atlassian.net`) since every tenant has its
//! own. The token is stored under [`crate::provider::KeyringScope::ConfluenceToken`];
//! the email and site are ordinary config (not secrets).
//!
//! OAuth 3LO exists and is NOT used: it needs a registered app, a redirect
//! listener and refresh handling, for no gain over a token the user pastes once.
//!
//! ## The markdown problem, and why this converts
//!
//! `crate::notion` sends `recap.md` **verbatim** because Notion ingests
//! markdown server-side. Confluence does not: `PageBodyWrite.representation`
//! accepts exactly `storage` (XHTML), `atlas_doc_format` (ADF) and `wiki`
//! (Confluence wiki markup) — verified against the v2 OpenAPI spec, in which
//! the string "markdown" does not appear at all.
//!
//! Of those three, `wiki` is the one that keeps the Notion property that
//! mattered: **the server does the parsing**. Generating `storage` XHTML would
//! mean owning an HTML emitter and every escaping edge case in it. So the only
//! code here is a line-based markdown → wiki markup translation, and it is
//! small on purpose — see [`md_to_wiki`].

use crate::error::TranscribeError;
use base64::Engine;
use serde_json::json;

const HTTP_TIMEOUT_SECS: u64 = 30;

/// A Confluence space the recap can be sent to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Space {
    pub id: String,
    pub key: String,
    pub name: String,
    /// `personal`, `global`, `collaboration`, …
    pub kind: String,
    /// True only for the space we positively identified as THIS user's.
    /// Never inferred from `kind`: every colleague has a personal space too,
    /// and labelling all of them "your space" is exactly the bug this
    /// replaces.
    pub is_mine: bool,
}

/// Who the credentials belong to, for the "connected as…" line.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SiteInfo {
    pub site: String,
    pub account_name: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CreatedPage {
    pub id: String,
    pub url: String,
}

fn http_client() -> Result<reqwest::Client, TranscribeError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| TranscribeError::Network(format!("reqwest build: {e}")))
}

/// `Basic base64(email:token)`. Never logged — callers log only booleans.
fn basic_auth(email: &str, token: &str) -> String {
    let raw = format!("{email}:{token}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

/// Normalise whatever the user pasted into a bare host.
///
/// People paste `https://acme.atlassian.net/wiki/spaces/X/pages/123`, or type
/// `acme`, or include a trailing slash. Getting this wrong produces a 404 that
/// reads like bad credentials, so it is worth being liberal here and strict
/// nowhere else.
pub fn normalize_site(input: &str) -> String {
    let s = input.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let host = s
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.');
    if host.is_empty() {
        return String::new();
    }
    if host.contains('.') {
        host.to_ascii_lowercase()
    } else {
        // Bare tenant name — the overwhelmingly common shape of a typo.
        format!("{}.atlassian.net", host.to_ascii_lowercase())
    }
}

/// Which credential form worked last. Set by `ping`, read by the calls that
/// follow it, so a successful connect is not re-discovered on every send.
/// A process-wide cell is enough: one user, one Confluence, one token.
static AUTH_STYLE: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// Classify a rejection without repeating it.
///
/// The body of a 401 is the only place Atlassian says WHICH thing is wrong —
/// an unknown credential and a credential missing a scope both come back as
/// 401 — but a body is also where a tenant can put account details. So it is
/// matched against known signatures and only the LABEL is kept: never the
/// text, never a fragment of it.
fn classify_auth_failure(www_authenticate: Option<&str>, body: &str) -> &'static str {
    let b = body.to_ascii_lowercase();
    if b.contains("scope") {
        "missing-scope"
    } else if b.contains("must be authenticated") || b.contains("unauthorized") {
        "credential-not-accepted"
    } else if b.contains("basic realm") || www_authenticate.is_some_and(|h| h.contains("Basic")) {
        "wants-basic"
    } else if b.contains("oauth") || www_authenticate.is_some_and(|h| h.contains("Bearer")) {
        "wants-oauth"
    } else if b.is_empty() {
        "empty-body"
    } else {
        "unclassified"
    }
}

fn remember_auth_style(style: &str) {
    let s: &'static str = if style == "bearer" { "bearer" } else { "basic" };
    if let Ok(mut g) = AUTH_STYLE.lock() {
        *g = Some(s);
    }
}

/// Header for the form that worked, defaulting to Basic until `ping` says
/// otherwise — that is the documented form and the more common token.
fn auth_header(email: &str, token: &str) -> String {
    let style = AUTH_STYLE.lock().ok().and_then(|g| *g).unwrap_or("basic");
    if style == "bearer" {
        format!("Bearer {token}")
    } else {
        basic_auth(email, token)
    }
}

/// Resolve a site to its cloud id, and from that to the API base URL.
///
/// This exists because Atlassian issues two kinds of API token that need two
/// DIFFERENT endpoints, and the user cannot reasonably be asked to know
/// which they have:
///
/// - a **classic** token authenticates against the site itself,
///   `https://acme.atlassian.net/wiki/...`;
/// - a **scoped** token only works through the gateway,
///   `https://api.atlassian.com/ex/confluence/<cloudId>/wiki/...`, and
///   returns a bare 401 on the site URL — indistinguishable from a wrong
///   password, which is exactly how it was first reported.
///
/// The gateway form accepts both, so we prefer it whenever the cloud id can
/// be found. `/_edge/tenant_info` is unauthenticated, so this costs one
/// request and no credentials; when it fails we fall back to the site URL,
/// which is still correct for a classic token.
async fn api_base(client: &reqwest::Client, site: &str) -> String {
    let direct = format!("https://{site}/wiki");
    let resp = match client
        .get(format!("https://{site}/_edge/tenant_info"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return direct,
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return direct,
    };
    match v.get("cloudId").and_then(|c| c.as_str()) {
        Some(id) if !id.is_empty() => {
            format!("https://api.atlassian.com/ex/confluence/{id}/wiki")
        }
        _ => direct,
    }
}

/// Markdown → Confluence wiki markup.
///
/// Deliberately covers only what a recap contains. Counted over a real
/// 121-line recap: headings h1-h3, bullets, numbered items and `**bold**`
/// inline. No tables, no code fences, no links, no quotes.
///
/// The recap is model-written, so the vocabulary can drift. Anything this does
/// not recognise is passed through as a paragraph rather than mangled or
/// dropped — a stray construct should cost its own formatting and nothing
/// else. That is why there is no parser here and no state beyond the current
/// line.
///
/// Wiki markup, for reference:
///   `h1. `/`h2. `/`h3. ` headings · `* `/`** ` bullets (depth by repetition)
///   `# ` numbered · `*bold*` · `_italic_` · `{{monospace}}`
pub fn md_to_wiki(markdown: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;

    for raw in markdown.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        // Code fences are not in a recap today, but if one appears it must not
        // have its contents reinterpreted as markup.
        if trimmed.starts_with("```") {
            out.push("{code}".into());
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push(line.to_string());
            continue;
        }

        if trimmed.is_empty() {
            out.push(String::new());
            continue;
        }

        // Dimmy's own invisible markers never reach a shared page: the caller
        // runs recap_for_sharing first, which swaps them for a visible notice.
        // Belt and braces in case a caller forgets.
        if trimmed.starts_with("<!--") && trimmed.ends_with("-->") {
            continue;
        }

        if let Some(rest) = heading(trimmed) {
            out.push(rest);
            continue;
        }
        if let Some(rest) = bullet(line) {
            out.push(rest);
            continue;
        }
        if let Some(rest) = numbered(line) {
            out.push(rest);
            continue;
        }

        // A paragraph that was not a heading or a list can still OPEN one:
        // `#` starts an ordered list in wiki markup even without a space, so
        // an unrecognised line beginning with it would silently become a
        // list item. Asterisks are already escaped by `inline`.
        let text = inline(trimmed);
        out.push(if text.starts_with('#') {
            format!("\\{text}")
        } else {
            text
        });
    }

    let mut text = out.join("\n");
    while text.ends_with('\n') {
        text.pop();
    }
    text.push('\n');
    text
}

/// `### Foo` → `h3. Foo`. Levels past 6 are not headings in markdown either.
fn heading(trimmed: &str) -> Option<String> {
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed[hashes..].strip_prefix(' ')?;
    Some(format!("h{}. {}", hashes, inline(rest.trim())))
}

/// `- Foo` / `  * Foo` → `* Foo` / `** Foo`. Wiki markup expresses depth by
/// repeating the marker, so indentation has to become a count. Two spaces per
/// level is the convention every markdown writer we feed it uses.
fn bullet(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    let depth = indent / 2 + 1;
    Some(format!("{} {}", "*".repeat(depth), inline(rest.trim())))
}

/// `1. Foo` → `# Foo`. Confluence numbers them itself, so the original digit
/// is discarded — which also means a list that restarts at 1 mid-document
/// renders as one list. Acceptable: a recap's numbered lists are short.
fn numbered(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = trimmed[digits.len()..].strip_prefix(". ")?;
    let depth = indent / 2 + 1;
    Some(format!("{} {}", "#".repeat(depth), inline(rest.trim())))
}

/// Inline spans. `**bold**` → `*bold*` is the only one a recap uses, but the
/// single-asterisk collision is the reason this is a scanner and not a
/// `replace`: emitting `*` for bold means a literal `*` in the text would
/// start a span, so those are escaped.
fn inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"**") {
            // Find the closing pair; unclosed bold stays literal.
            if let Some(end) = s[i + 2..].find("**") {
                out.push('*');
                out.push_str(&s[i + 2..i + 2 + end]);
                out.push('*');
                i += 2 + end + 2;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        // A bare asterisk or underscore would be read as markup by Confluence.
        if ch == '*' || ch == '_' || ch == '{' || ch == '}' || ch == '[' || ch == ']' {
            out.push('\\');
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Validate credentials against the capability we actually need.
///
/// The check is `GET /wiki/api/v2/spaces?limit=1`, NOT the obvious
/// `/rest/api/user/current`, and the difference matters because Atlassian now
/// issues two kinds of API token:
///
/// - a **classic** token, which carries all of the user's permissions;
/// - a **scoped** token, where the user ticks individual scopes.
///
/// v2 has no current-user endpoint at all, so identity can only come from a v1
/// call needing `read:confluence-user` — a scope this integration otherwise
/// never uses. Gating on it would reject a scoped token that has exactly
/// `read:space:confluence` + `write:page:confluence` and would have worked
/// perfectly: the wizard would block the user on a credential that is fine.
///
/// So the gate is the space read (needed for the picker and implied by the
/// page write), and the display name is fetched best-effort afterwards. No
/// name is a cosmetic loss; a false rejection is not.
pub async fn ping(site: &str, email: &str, token: &str) -> Result<SiteInfo, TranscribeError> {
    let site = normalize_site(site);
    // Trim BOTH: a token copied from the Atlassian page often carries a
    // trailing newline, and an email dragged from a contact card a leading
    // space. Either produces a 401 that reads like a wrong credential.
    let email = email.trim();
    let token = token.trim();
    if site.is_empty() || email.is_empty() || token.is_empty() {
        return Err(TranscribeError::Network(
            "site, email and API token are all required".into(),
        ));
    }
    let client = http_client()?;
    let base = api_base(&client, &site).await;
    let url = format!("{base}/api/v2/spaces?limit=1");
    // Anonymous and pertinent: the host the user typed, and whether a token
    // was present. Never the token, never the email.
    // Categorical only, no secret and no address. `shape` says whether the
    // string even looks like an Atlassian token (they carry a fixed public
    // prefix); the length is variable BY DESIGN — Atlassian warns against
    // assuming a fixed one — so it is logged only to spot a truncated paste.
    let shape = if token.starts_with("ATATT") {
        "api-token"
    } else if token.starts_with("ATCTT") {
        "scoped-token"
    } else {
        "unrecognised-prefix"
    };
    crate::log(&format!(
        "[Confluence] ping {} via {} (email={}, token={}, {} chars)",
        url,
        if base.contains("api.atlassian.com") {
            "gateway"
        } else {
            "site"
        },
        if email.contains('@') {
            "looks-like-email"
        } else {
            "NOT-an-email"
        },
        shape,
        token.len()
    ));
    // Two credential FORMS, tried in order, because Atlassian accepts
    // different ones depending on how the token was minted:
    //
    //   Basic  base64(email:token)  — classic API tokens
    //   Bearer token                — tokens carrying scopes, which behave
    //                                 like OAuth credentials and reject Basic
    //                                 with a bare 401
    //
    // Nothing in the token itself reliably says which it is (a scoped token
    // can share the classic ATATT prefix), and a wrong guess is a 401 that
    // looks exactly like a bad password. Trying both costs one extra request
    // on the less common path and removes the guess entirely.
    let mut status = reqwest::StatusCode::UNAUTHORIZED;
    let mut used = "";
    for (label, header) in [
        ("basic", basic_auth(email, token)),
        ("bearer", format!("Bearer {token}")),
    ] {
        let resp = client
            .get(&url)
            .header("Authorization", header)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| TranscribeError::Network(format!("Confluence ping: {e}")))?;
        status = resp.status();
        used = label;
        let www = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let reason = if status.is_success() {
            "ok"
        } else {
            let body = resp.text().await.unwrap_or_default();
            classify_auth_failure(www.as_deref(), &body)
        };
        crate::log(&format!(
            "[Confluence] ping ({label}) -> HTTP {} [{}]",
            status.as_u16(),
            reason
        ));
        if status.is_success() {
            break;
        }
        // Only a 401 is worth retrying in the other form; a 403 or 404 means
        // the credential was understood and something else is wrong.
        if status.as_u16() != 401 {
            break;
        }
    }
    if status.is_success() {
        remember_auth_style(used);
    }
    if !status.is_success() {
        // Body deliberately not read: on a corporate tenant it can carry the
        // account's own details, and the status already says what to do.
        return Err(TranscribeError::Network(match status.as_u16() {
            401 => "Confluence rejected these credentials (401). Use your Atlassian ACCOUNT email and an API token from id.atlassian.com — not your password.".into(),
            403 => "The token is valid but cannot read spaces (403). A scoped token needs read:space:confluence and write:page:confluence.".into(),
            404 => format!("No Confluence at {site} (404). Check the site address."),
            other => format!("Confluence ping failed (HTTP {other})."),
        }));
    }

    // Best-effort identity. A scoped token without read:confluence-user fails
    // here and that is fine — the connection is already proven.
    let account_name = current_user(&client, &base, email, token)
        .await
        .map(|(_, name)| name)
        .unwrap_or_default();
    Ok(SiteInfo { site, account_name })
}

/// Account id and display name of the authenticated user, best effort.
///
/// The id is what makes the personal space findable: its key is
/// `~<accountId>`, and on a tenant with hundreds of spaces that is the only
/// way to reach it — see `spaces`. Needs `read:confluence-user`, which a
/// scoped token may not carry, hence Option and no hard failure.
async fn current_user(
    client: &reqwest::Client,
    base: &str,
    email: &str,
    token: &str,
) -> Option<(String, String)> {
    let resp = client
        .get(format!("{base}/rest/api/user/current"))
        .header("Authorization", auth_header(email, token))
        .header("Accept", "application/json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let id = v.get("accountId").and_then(|s| s.as_str()).unwrap_or("");
    let name = v.get("displayName").and_then(|s| s.as_str()).unwrap_or("");
    if id.is_empty() && name.is_empty() {
        return None;
    }
    Some((id.to_string(), name.to_string()))
}

/// Spaces to offer as a destination: the user's OWN personal space first,
/// then the shared ones.
///
/// A single `?limit=100` was wrong on a real tenant. temera.atlassian.net has
/// enough spaces that the first page never reached the user's own — the list
/// came back full of other people's spaces and missing the one that matters,
/// which is the default we preselect. Two changes fix it:
///
/// - the personal space is fetched BY KEY (`~<accountId>`), so it is present
///   regardless of how the tenant sorts or how many spaces exist;
/// - the rest is paginated rather than truncated.
///
/// Other people's personal spaces are dropped: hundreds of them, none
/// writable, and they bury the real choices.
pub async fn spaces(site: &str, email: &str, token: &str) -> Result<Vec<Space>, TranscribeError> {
    let site = normalize_site(site);
    let email = email.trim();
    let token = token.trim();
    let client = http_client()?;
    let base = api_base(&client, &site).await;

    let mut out: Vec<Space> = Vec::new();
    let mut mine: Option<String> = None;

    // 1. The user's own space, by key. Best effort: without
    //    read:confluence-user there is no account id, and then it can only be
    //    found by name in the paginated list below.
    if let Some((account_id, _)) = current_user(&client, &base, email, token).await {
        if !account_id.is_empty() {
            let key = format!("~{account_id}");
            if let Ok(list) = fetch_spaces(&client, &base, email, token, &[("keys", &key)]).await {
                if let Some(mut sp) = list.into_iter().next() {
                    mine = Some(sp.id.clone());
                    sp.is_mine = true;
                    out.push(sp);
                }
            }
        }
    }

    // 2. Everything else, paginated. The cap exists so a huge tenant cannot
    //    turn opening a dropdown into a minute of requests; 1000 spaces is far
    //    past what anyone scrolls.
    let mut cursor = String::new();
    for _ in 0..4 {
        let limit = "250";
        let mut params: Vec<(&str, &str)> = vec![("limit", limit)];
        if !cursor.is_empty() {
            params.push(("cursor", &cursor));
        }
        let (list, next) = fetch_spaces_page(&client, &base, email, token, &params).await?;
        for sp in list {
            if mine.as_deref() == Some(sp.id.as_str()) {
                continue; // already first
            }
            // Other people's personal spaces: hundreds of them, none
            // writable. Dropped once we know which one is OURS. When we do
            // not — a token with only the granular v2 scopes cannot reach any
            // user endpoint, so this is the normal case for those — they stay
            // in, because ours is among them and dropping blind would delete
            // the one space the user is looking for. They sort to the bottom.
            if sp.kind == "personal" && mine.is_some() {
                continue;
            }
            out.push(sp);
        }
        match next {
            Some(c) if !c.is_empty() => cursor = c,
            _ => break,
        }
    }

    // Order: my own space (when identified), then the shared spaces, then
    // everyone else's personal ones. Without that last rank a 500-space
    // tenant buries the handful of team spaces under hundreds of colleagues.
    let tail_from = usize::from(mine.is_some());
    out[tail_from..].sort_by(|a, b| {
        let rank = |k: &str| u8::from(k == "personal");
        rank(&a.kind)
            .cmp(&rank(&b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    crate::log(&format!(
        "[Confluence] spaces: {} offered (own space {})",
        out.len(),
        if mine.is_some() {
            "found by key"
        } else {
            "not identified — no read:confluence-user, personal spaces kept"
        }
    ));
    Ok(out)
}

async fn fetch_spaces(
    client: &reqwest::Client,
    base: &str,
    email: &str,
    token: &str,
    params: &[(&str, &str)],
) -> Result<Vec<Space>, TranscribeError> {
    fetch_spaces_page(client, base, email, token, params)
        .await
        .map(|(list, _)| list)
}

async fn fetch_spaces_page(
    client: &reqwest::Client,
    base: &str,
    email: &str,
    token: &str,
    params: &[(&str, &str)],
) -> Result<(Vec<Space>, Option<String>), TranscribeError> {
    let resp = client
        .get(format!("{base}/api/v2/spaces"))
        .query(params)
        .header("Authorization", auth_header(email, token))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| TranscribeError::Network(format!("Confluence spaces: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(TranscribeError::Network(format!(
            "Confluence spaces failed (HTTP {})",
            status.as_u16()
        )));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TranscribeError::Network(format!("Confluence spaces decode: {e}")))?;

    let list: Vec<Space> = v
        .get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|s| s.get("status").and_then(|x| x.as_str()) != Some("archived"))
                .map(|s| Space {
                    id: str_field(s, "id"),
                    key: str_field(s, "key"),
                    name: str_field(s, "name"),
                    kind: str_field(s, "type"),
                    is_mine: false,
                })
                .filter(|s| !s.id.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // The cursor lives inside the `next` link as a query parameter.
    let next = v
        .get("_links")
        .and_then(|l| l.get("next"))
        .and_then(|n| n.as_str())
        .and_then(|n| {
            n.split('?').nth(1).and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix("cursor=").map(|c| c.to_string()))
            })
        });
    Ok((list, next))
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Create the recap page. `space_id` accepts a numeric id or a space key
/// (including a `~account` personal key) — the FFI resolves keys before
/// calling, because v2 wants the numeric id.
pub async fn send_meeting_recap(
    site: &str,
    email: &str,
    token: &str,
    space_id: &str,
    parent_id: &str,
    title: &str,
    markdown: &str,
) -> Result<CreatedPage, TranscribeError> {
    if token.is_empty() {
        return Err(TranscribeError::Network(
            "Confluence API token not set".into(),
        ));
    }
    if space_id.is_empty() {
        return Err(TranscribeError::Network(
            "Confluence destination space not configured".into(),
        ));
    }
    if title.is_empty() {
        return Err(TranscribeError::Network("title is empty".into()));
    }
    if markdown.trim().is_empty() {
        return Err(TranscribeError::Network("markdown is empty".into()));
    }

    let site = normalize_site(site);
    let client = http_client()?;
    let base = api_base(&client, &site).await;
    let mut body = json!({
        "spaceId": space_id,
        "status": "current",
        "title": title,
        "body": { "representation": "wiki", "value": md_to_wiki(markdown) },
    });
    if !parent_id.is_empty() {
        body["parentId"] = json!(parent_id);
    }

    let resp = client
        .post(format!("{base}/api/v2/pages"))
        .header("Authorization", auth_header(email, token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| TranscribeError::Network(format!("Confluence create page: {e}")))?;

    let status = resp.status();
    crate::log(&format!(
        "[Confluence] create page -> HTTP {}",
        status.as_u16()
    ));
    if !status.is_success() {
        return Err(TranscribeError::Network(match status.as_u16() {
            400 => "Confluence refused the page (400). The space or parent may be wrong.".into(),
            401 | 403 => "Not allowed to create a page there (401/403).".into(),
            404 => "That space or parent page no longer exists (404).".into(),
            other => format!("Confluence create page failed (HTTP {other})."),
        }));
    }

    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TranscribeError::Network(format!("Confluence create decode: {e}")))?;
    let id = str_field(&v, "id");
    let webui = v
        .get("_links")
        .and_then(|l| l.get("webui"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    Ok(CreatedPage {
        id,
        url: if webui.is_empty() {
            format!("https://{site}/wiki")
        } else {
            format!("https://{site}/wiki{webui}")
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rules MY half out of a 401. The vector is the one from RFC 7617
    /// itself, so a pass means the header we send is the header the standard
    /// describes and any rejection is about the credential, not the encoding.
    #[test]
    fn basic_auth_matches_the_rfc_vector() {
        assert_eq!(
            basic_auth("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        // And the shape we actually send: email, colon, token.
        assert_eq!(basic_auth("a@b.c", "tok"), "Basic YUBiLmM6dG9r");
    }

    #[test]
    fn site_is_normalised_from_whatever_was_pasted() {
        for input in [
            "acme.atlassian.net",
            "https://acme.atlassian.net",
            "https://acme.atlassian.net/",
            "https://acme.atlassian.net/wiki/spaces/ENG/pages/123/Title",
            "  ACME.atlassian.net  ",
        ] {
            assert_eq!(
                normalize_site(input),
                "acme.atlassian.net",
                "input: {input}"
            );
        }
        // A bare tenant name is the common typo; assume the default domain.
        assert_eq!(normalize_site("acme"), "acme.atlassian.net");
        assert_eq!(normalize_site(""), "");
        // A self-hosted host must be left alone, not suffixed.
        assert_eq!(normalize_site("wiki.acme.co.uk"), "wiki.acme.co.uk");
    }

    #[test]
    fn headings_become_wiki_headings() {
        assert_eq!(md_to_wiki("# Title\n").trim(), "h1. Title");
        assert_eq!(md_to_wiki("## Context\n").trim(), "h2. Context");
        assert_eq!(md_to_wiki("### Detail\n").trim(), "h3. Detail");
        // Not a heading: no space after the hashes.
        assert_eq!(md_to_wiki("#hashtag\n").trim(), "\\#hashtag");
    }

    #[test]
    fn bullets_carry_their_depth() {
        let out = md_to_wiki("- one\n  - nested\n    - deeper\n");
        assert_eq!(out.trim(), "* one\n** nested\n*** deeper");
        // `*` and `+` are markdown bullets too.
        assert_eq!(md_to_wiki("* one\n").trim(), "* one");
        assert_eq!(md_to_wiki("+ one\n").trim(), "* one");
    }

    #[test]
    fn numbered_items_let_confluence_do_the_counting() {
        let out = md_to_wiki("1. first\n2. second\n  1. nested\n");
        assert_eq!(out.trim(), "# first\n# second\n## nested");
    }

    #[test]
    fn bold_becomes_single_asterisks() {
        assert_eq!(md_to_wiki("**Owner:** Anna\n").trim(), "*Owner:* Anna");
        assert_eq!(
            md_to_wiki("- **Decision:** ship it\n").trim(),
            "* *Decision:* ship it"
        );
    }

    /// The collision that makes this a scanner instead of a replace: with bold
    /// rendered as `*`, a literal asterisk would open a span.
    #[test]
    fn literal_markup_characters_are_escaped() {
        assert_eq!(md_to_wiki("2 * 3 = 6\n").trim(), r"2 \* 3 = 6");
        assert_eq!(md_to_wiki("snake_case_name\n").trim(), r"snake\_case\_name");
        assert_eq!(md_to_wiki("a {macro} here\n").trim(), r"a \{macro\} here");
        // Unclosed bold is text, not a span.
        assert_eq!(md_to_wiki("**oops\n").trim(), r"\*\*oops");
    }

    /// An unrecognised construct must cost its own formatting and nothing more.
    #[test]
    fn unknown_constructs_degrade_to_paragraphs() {
        let out = md_to_wiki("| a | b |\n|---|---|\n| 1 | 2 |\n");
        for line in out.lines().filter(|l| !l.is_empty()) {
            assert!(
                line.contains('a') || line.contains('1') || line.contains('-'),
                "a table row must survive as text, got: {line}"
            );
        }
    }

    #[test]
    fn dimmy_internal_markers_never_reach_a_page() {
        let out = md_to_wiki("# T\n<!-- dimmy-ai-generated: true -->\n\nBody.\n");
        assert!(!out.contains("dimmy-ai-generated"), "got: {out}");
        assert!(out.contains("h1. T"));
        assert!(out.contains("Body."));
    }

    /// Shape check against the real thing: the recap of 2026-09-11 was 121
    /// lines of exactly these five constructs.
    #[test]
    fn a_realistic_recap_converts_without_losing_structure() {
        let recap = "# Federazione Keycloak su Auth0\n\n\
             > **Riassunto generato con AI** Rileggilo prima di condividerlo.\n\n\
             ## Contesto\n\nDue tenant, un solo login.\n\n\
             ## Decisioni\n\n\
             - **Federazione** invece di migrazione\n\
             - Rollout in due fasi\n  - prima gli interni\n\n\
             ## Prossimi passi\n\n1. Aprire il ticket\n2. Provare in staging\n";
        let out = md_to_wiki(recap);
        assert!(out.contains("h1. Federazione Keycloak su Auth0"));
        assert!(out.contains("h2. Contesto"));
        assert!(out.contains("* *Federazione* invece di migrazione"));
        assert!(out.contains("** prima gli interni"));
        assert!(out.contains("# Aprire il ticket"));
        // The AI notice survives as text — it is the one line that must.
        assert!(out.contains("Riassunto generato con AI"));
    }

    #[test]
    fn send_rejects_incomplete_configuration() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cases: [(&str, &str, &str, &str); 4] = [
            ("", "S", "T", "md"),     // no token
            ("tok", "", "T", "md"),   // no space
            ("tok", "S", "", "md"),   // no title
            ("tok", "S", "T", "   "), // no body
        ];
        for (token, space, title, md) in cases {
            let err = rt.block_on(send_meeting_recap(
                "acme.atlassian.net",
                "a@b.c",
                token,
                space,
                "",
                title,
                md,
            ));
            assert!(
                err.is_err(),
                "expected refusal for ({token},{space},{title})"
            );
        }
    }
}
