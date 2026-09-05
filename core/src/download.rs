//! Shared resumable + integrity-checked download for local model assets
//! (LLM GGUF, whisper ggml/GGUF, parakeet ONNX bundle). Keeping resume + the
//! corruption guards in ONE place means every model download — on Windows, macOS
//! and Linux — survives a mid-flight kill and never installs a corrupt file.
//!
//! Robustness package:
//! - RESUME: an existing `.part` continues via `Range: bytes=N-` (+ `If-Range`
//!   when we know the ETag) instead of restarting a multi-GB download.
//! - INTEGRITY: after the bytes land, the file must (optionally) start with a
//!   known magic AND — when the server gave us its SHA-256 (HuggingFace LFS
//!   serves it as the (X-Linked-)ETag) — hash to it. A corrupt resumed partial or
//!   a garbage/HTML body that still reached full size is caught here.
//! - SELF-HEAL: on any integrity failure the `.part` is DELETED so the retry
//!   restarts clean instead of resuming corruption forever.

use std::path::Path;

/// Normalize an HTTP ETag into a bare lowercase hex hash: strip the weak-validator
/// `W/` prefix, quotes, and any `sha256:` prefix. HuggingFace LFS ETags ARE the
/// file's SHA-256.
pub fn normalize_etag(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("W/")
        .trim_matches('"')
        .trim_start_matches("sha256:")
        .trim_matches('"')
        .to_ascii_lowercase()
}

/// `true` when `s` is a 64-char lowercase-hex SHA-256.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Ask the origin for `X-Linked-ETag` without following the redirect.
///
/// Only huggingface.co knows the file's real SHA-256; the CDN it redirects to
/// reports its own Xet content hash under the same `etag` name. A normal
/// request follows the redirect and loses the header, so this makes a second,
/// cheap HEAD with redirects disabled purely to read it.
///
/// Returns None on any failure — a missing hash means the download is verified
/// by magic bytes alone, which is what happened before HuggingFace served a
/// hash at all. Never fatal.
async fn probe_linked_etag(url: &str) -> Option<String> {
    let probe = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()?;
    let resp = probe.head(url).send().await.ok()?;
    let raw = resp.headers().get("x-linked-etag")?.to_str().ok()?;
    let norm = normalize_etag(raw);
    is_sha256(&norm).then(|| raw.to_string())
}

/// Validate a finished file: optional magic-byte prefixes (any one may match) +
/// (when available) a streaming SHA-256 against `expected_sha`. Streams in 1 MiB
/// chunks so a multi-GB model is never buffered whole. Sync — callable from
/// blocking code (parakeet) and async alike. `Err(reason)` on any mismatch.
pub fn verify_file(
    path: &Path,
    accept_magics: &[&[u8]],
    expected_sha: Option<&str>,
) -> Result<(), String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;

    if !accept_magics.is_empty() {
        let maxlen = accept_magics.iter().map(|m| m.len()).max().unwrap_or(0);
        let mut head = vec![0u8; maxlen];
        f.read_exact(&mut head)
            .map_err(|e| format!("read magic: {e}"))?;
        if !accept_magics.iter().any(|&m| head.starts_with(m)) {
            return Err(format!("bad magic {:02x?}", head));
        }
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(0))
            .map_err(|e| format!("seek: {e}"))?;
    }

    let Some(expected) = expected_sha else {
        return Ok(()); // magic-only when the server gave no usable hash
    };
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = hex_lower(&hasher.finalize());
    if got != expected {
        return Err(format!(
            "sha256 mismatch (got {}…, want {}…)",
            &got[..8.min(got.len())],
            &expected[..8.min(expected.len())]
        ));
    }
    Ok(())
}

/// Download `url` to `dest` with resume + integrity. Writes to `<dest>.part`,
/// verifies, then atomically renames. `accept_magics` may be empty (skip the
/// magic check and rely on size + SHA-256). `on_progress(done, total)` fires per
/// chunk. Returns `Err(reason)`; the caller maps it to its own error type.
pub async fn download_resumable(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    accept_magics: &[&[u8]],
    on_progress: impl Fn(u64, u64),
) -> Result<(), String> {
    let dir = dest.parent().ok_or("dest has no parent directory")?;
    let fname = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("dest has no filename")?;
    let part_path = dir.join(format!("{fname}.part"));
    // Sidecar holding the ETag captured when the download first started, so a
    // later resume can send If-Range and we know the expected hash up front.
    let meta_path = dir.join(format!("{fname}.part.etag"));
    let resume_from: u64 = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    let prior_etag = std::fs::read_to_string(&meta_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
        if let Some(etag) = &prior_etag {
            req = req.header(reqwest::header::IF_RANGE, etag.clone());
        }
        crate::log(&format!(
            "[download] resuming {fname} from {resume_from} bytes"
        ));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        if status.as_u16() == 416 && resume_from > 0 {
            let _ = std::fs::remove_file(&part_path);
            return Err("stale partial discarded — retry to start fresh".into());
        }
        let code = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {code} — {}",
            crate::truncate_utf8(&body, 200)
        ));
    }

    // HuggingFace serves the file's SHA-256 as `X-Linked-ETag` — but only on
    // the 302 from huggingface.co. Since the Xet storage rollout the CDN's own
    // response carries an `etag` that is the XET CONTENT HASH, a completely
    // different number, and reqwest follows the redirect for us so that is all
    // we are left holding. Taking it as the SHA-256 fails every integrity check
    // and deletes the download: measured 2026-09-05, when every unsloth model
    // in our catalogue had become unfetchable.
    //
    // So ask the origin separately, without following, and prefer what it says.
    let linked_etag = probe_linked_etag(url).await;
    let raw_etag = linked_etag.or_else(|| {
        resp.headers()
            .get("x-linked-etag")
            .or_else(|| resp.headers().get(reqwest::header::ETAG))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    if let Some(raw) = &raw_etag {
        let _ = std::fs::write(&meta_path, raw.trim());
    }
    let expected_sha = raw_etag
        .as_deref()
        .map(normalize_etag)
        .filter(|s| is_sha256(s));

    // 206 = server honored the Range → APPEND. 200 = ignored it (or none sent, or
    // If-Range said the file changed) → start over from byte 0.
    let resuming = status.as_u16() == 206 && resume_from > 0;
    let start_at = if resuming { resume_from } else { 0 };
    let total_bytes = resp.content_length().unwrap_or(0) + start_at;

    use std::io::Write;
    let mut file = if resuming {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&part_path)
            .map_err(|e| format!("open part for resume: {e}"))?
    } else {
        std::fs::File::create(&part_path).map_err(|e| format!("create part: {e}"))?
    };

    let mut downloaded: u64 = start_at;
    let mut resp = resp;
    // Report progress on a CLOCK, not per chunk. reqwest hands back 8-64 KB at
    // a time, so a 2.4 GB model produced tens of thousands of callbacks — each
    // one a JSON string across the FFI and a marshal onto the host's UI thread.
    // The download itself was fine (it runs off that thread); the UI drowned in
    // its own queue and froze until Dimmy was restarted, by which point the
    // file had quietly finished (reported 2026-09-05).
    //
    // Four or five updates a second is more than a progress bar can show.
    let mut last_report = std::time::Instant::now();
    let report_every = std::time::Duration::from_millis(200);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("stream error: {e}"))?
    {
        file.write_all(&chunk).map_err(|e| format!("write: {e}"))?;
        downloaded += chunk.len() as u64;
        if last_report.elapsed() >= report_every {
            last_report = std::time::Instant::now();
            on_progress(downloaded, total_bytes);
        }
    }
    drop(file);
    // Always land on the true final figure, whatever the clock did.
    on_progress(downloaded, total_bytes);

    // Size: never rename a short file — keep the .part so the next try resumes.
    if total_bytes > 0 && downloaded < total_bytes {
        return Err(format!(
            "incomplete: {downloaded} of {total_bytes} bytes — retry to resume"
        ));
    }
    // Content: magic + SHA-256. On failure delete the .part so the retry is clean.
    if let Err(e) = verify_file(&part_path, accept_magics, expected_sha.as_deref()) {
        let _ = std::fs::remove_file(&part_path);
        let _ = std::fs::remove_file(&meta_path);
        return Err(format!(
            "integrity check failed ({e}) — deleted, retry to re-download"
        ));
    }

    std::fs::rename(&part_path, dest).map_err(|e| format!("rename: {e}"))?;
    let _ = std::fs::remove_file(&meta_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_etag_strips_quotes_and_prefixes() {
        assert_eq!(normalize_etag("\"abc123\""), "abc123");
        assert_eq!(normalize_etag("W/\"ABC\""), "abc");
        assert_eq!(normalize_etag("sha256:DEADbeef"), "deadbeef");
    }

    #[test]
    fn is_sha256_basics() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(!is_sha256(&"a".repeat(63)));
        assert!(!is_sha256(&"g".repeat(64)));
    }

    #[test]
    fn verify_file_magic_and_sha() {
        let dir = std::env::temp_dir();
        let good = dir.join("dimmy_dl_verify_good.bin");
        std::fs::write(&good, b"GGUFpayload").unwrap();
        // magic accepted (one of several), no hash
        assert!(verify_file(&good, &[b"lmgg", b"GGUF"], None).is_ok());
        // wrong magic rejected
        assert!(verify_file(&good, &[b"ZZZZ"], None).is_err());
        // sha256 path
        use sha2::{Digest, Sha256};
        let want = {
            let mut h = Sha256::new();
            h.update(b"GGUFpayload");
            hex_lower(&h.finalize())
        };
        assert!(verify_file(&good, &[], Some(&want)).is_ok());
        assert!(verify_file(&good, &[], Some("deadbeef")).is_err());
        let _ = std::fs::remove_file(&good);
    }
}
