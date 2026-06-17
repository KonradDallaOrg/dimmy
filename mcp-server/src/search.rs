//! Pure-Rust, filesystem-native full-text search over meetings.
//!
//! Deliberately NO SQLite/rusqlite and NO core-crate dependency — this
//! keeps `dimmy-mcp` a ~3.5 MB JSON-file reader (see Cargo.toml). The
//! textbook answer for this would be SQLite FTS5, but meetings live on
//! the filesystem (one dir each: meta.json / transcripts.txt / recap.md
//! / notes.md), not in a DB the MCP can open, so we build our own small
//! inverted index in memory.
//!
//! Design (matches the "fast, scalable, permissive, token-cheap" goal):
//!   - Inverted index built ONCE per process and cached; rebuilt only
//!     when the meetings dir changes (signature = entry count + newest
//!     mtime). Personal scale (hundreds of meetings) builds in tens of ms.
//!   - BM25 ranking, with the meeting title weighted far above the body.
//!   - Permissive term expansion per query word: exact -> prefix
//!     (plurals / word forms) -> bounded Levenshtein over the VOCABULARY
//!     only (typo / "I don't remember the exact word"). Fuzzy never scans
//!     documents, only the vocab, so it stays cheap.
//!   - Relaxation: results where ALL query words matched rank above
//!     partial (any-word) matches, so a near-miss still returns something.
//!   - Token-cheap output: a short highlighted snippet per hit, never the
//!     full transcript. The caller fetches full content via dimmy_get_meeting.
//!
//! True semantic synonyms (car<->automobile) would need embeddings; that
//! is overkill at this scale and is intentionally left out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

const TITLE_BOOST: u32 = 6; // title tokens count this many times in tf
const K1: f64 = 1.2;
const B: f64 = 0.75;
const MIN_TOKEN_LEN: usize = 2;
const MAX_FUZZY_VOCAB_WEIGHT_EXACT: f64 = 1.0;
const WEIGHT_PREFIX: f64 = 0.7;
const WEIGHT_FUZZY: f64 = 0.5;

/// One indexed meeting.
struct Doc {
    id: String,
    title: String,
    started_at: String,
    has_recap: bool,
    len: u32,                 // total weighted token count (BM25 normalizer)
    tf: HashMap<String, u32>, // term -> weighted frequency
    transcript_path: PathBuf, // re-read lazily for the snippet
}

struct Index {
    sig: (usize, SystemTime), // (dir entry count, newest mtime)
    docs: Vec<Doc>,
    df: HashMap<String, u32>, // document frequency per term
    vocab: Vec<String>,       // distinct terms, for fuzzy expansion
    avg_len: f64,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);

/// Public result row (serialized to JSON by the caller).
#[derive(serde::Serialize)]
pub struct Hit {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub has_recap: bool,
    pub score: f64,
    pub snippet: String,
    /// "all" = every query word matched this meeting, "partial" = some.
    pub matched: &'static str,
}

/// Entry point. Synchronous on purpose — call it from a blocking task
/// (`tokio::task::spawn_blocking`) so the async runtime thread never stalls.
pub fn run(meetings_dir: &Path, query: &str, limit: usize) -> Vec<Hit> {
    let limit = limit.clamp(1, 25);
    let qterms = tokenize_unique(query);
    if qterms.is_empty() {
        return vec![];
    }

    let mut guard = INDEX.lock().unwrap_or_else(|e| e.into_inner());
    let sig = dir_signature(meetings_dir);
    let stale = guard.as_ref().map(|i| i.sig != sig).unwrap_or(true);
    if stale {
        *guard = Some(build_index(meetings_dir, sig));
    }
    let index = guard.as_ref().expect("index built above");

    search_index(index, &qterms, limit)
}

// ── Indexing ─────────────────────────────────────────────────────────

fn dir_signature(dir: &Path) -> (usize, SystemTime) {
    let mut count = 0usize;
    let mut newest = SystemTime::UNIX_EPOCH;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                count += 1;
                if let Ok(m) = e.metadata() {
                    if let Ok(t) = m.modified() {
                        if t > newest {
                            newest = t;
                        }
                    }
                }
            }
        }
    }
    (count, newest)
}

fn build_index(dir: &Path, sig: (usize, SystemTime)) -> Index {
    let mut docs = Vec::new();
    let mut df: HashMap<String, u32> = HashMap::new();

    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let id = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            let meta: serde_json::Value = std::fs::read_to_string(path.join("meta.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let recap = std::fs::read_to_string(path.join("recap.md")).ok();
            let notes = std::fs::read_to_string(path.join("notes.md")).unwrap_or_default();
            let transcript_path = path.join("transcripts.txt");
            let transcript = std::fs::read_to_string(&transcript_path).unwrap_or_default();

            // Title: meta -> first H1 of recap -> dir id.
            let title = meta
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| recap.as_deref().and_then(first_h1))
                .unwrap_or_else(|| id.clone());
            let started_at = meta
                .get("started_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let has_recap = recap
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);

            // Build the weighted term frequencies.
            let mut tf: HashMap<String, u32> = HashMap::new();
            let mut buf = Vec::new();
            // Title carries the most signal -> boost.
            tokenize_into(&title, &mut buf);
            for t in buf.drain(..) {
                *tf.entry(t).or_insert(0) += TITLE_BOOST;
            }
            // Body: transcript (prefixes stripped) + recap + notes, weight 1.
            tokenize_into(&strip_transcript_markers(&transcript), &mut buf);
            if let Some(r) = &recap {
                tokenize_into(r, &mut buf);
            }
            tokenize_into(&notes, &mut buf);
            for t in buf.drain(..) {
                *tf.entry(t).or_insert(0) += 1;
            }

            if tf.is_empty() {
                continue;
            }
            let len: u32 = tf.values().sum();
            for term in tf.keys() {
                *df.entry(term.clone()).or_insert(0) += 1;
            }

            docs.push(Doc {
                id,
                title,
                started_at,
                has_recap,
                len,
                tf,
                transcript_path,
            });
        }
    }

    let avg_len = if docs.is_empty() {
        1.0
    } else {
        docs.iter().map(|d| d.len as f64).sum::<f64>() / docs.len() as f64
    };
    let vocab: Vec<String> = df.keys().cloned().collect();

    Index {
        sig,
        docs,
        df,
        vocab,
        avg_len,
    }
}

// ── Query ────────────────────────────────────────────────────────────

fn search_index(index: &Index, qterms: &[String], limit: usize) -> Vec<Hit> {
    let n = index.docs.len() as f64;
    if n == 0.0 {
        return vec![];
    }

    // Expand each query word to (vocab_term, weight) candidates.
    let expansions: Vec<Vec<(String, f64)>> =
        qterms.iter().map(|q| expand_term(index, q)).collect();

    // Score every doc: each query group contributes its single best
    // matching expanded term (so a word counts once). Track how many
    // groups matched for AND/OR relaxation.
    struct Scored {
        idx: usize,
        score: f64,
        matched_groups: usize,
        // the expanded terms that hit this doc, for snippet highlighting
        hit_terms: Vec<String>,
    }
    let mut scored: Vec<Scored> = Vec::new();

    for (i, doc) in index.docs.iter().enumerate() {
        let mut score = 0.0;
        let mut matched_groups = 0usize;
        let mut hit_terms: Vec<String> = Vec::new();
        for group in &expansions {
            let mut best = 0.0f64;
            let mut best_term: Option<&str> = None;
            for (term, weight) in group {
                if let Some(&freq) = doc.tf.get(term) {
                    let s = bm25_term(freq, doc.len, index, term, n) * weight;
                    if s > best {
                        best = s;
                        best_term = Some(term);
                    }
                }
            }
            if best > 0.0 {
                score += best;
                matched_groups += 1;
                if let Some(t) = best_term {
                    hit_terms.push(t.to_string());
                }
            }
        }
        if matched_groups > 0 {
            scored.push(Scored {
                idx: i,
                score,
                matched_groups,
                hit_terms,
            });
        }
    }

    // All-terms matches first, then by score. This is the relaxation:
    // exact-AND on top, partial-OR below, so a near miss still appears.
    let total_groups = qterms.len();
    scored.sort_by(|a, b| {
        b.matched_groups.cmp(&a.matched_groups).then(
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    scored.truncate(limit);

    scored
        .into_iter()
        .map(|s| {
            let doc = &index.docs[s.idx];
            Hit {
                id: doc.id.clone(),
                title: doc.title.clone(),
                started_at: doc.started_at.clone(),
                has_recap: doc.has_recap,
                score: (s.score * 1000.0).round() / 1000.0,
                snippet: build_snippet(doc, &s.hit_terms),
                matched: if s.matched_groups == total_groups {
                    "all"
                } else {
                    "partial"
                },
            }
        })
        .collect()
}

/// BM25 contribution of a single term in a single doc.
fn bm25_term(freq: u32, doc_len: u32, index: &Index, term: &str, n: f64) -> f64 {
    let df = *index.df.get(term).unwrap_or(&1) as f64;
    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
    let tf = freq as f64;
    let denom = tf + K1 * (1.0 - B + B * (doc_len as f64 / index.avg_len));
    idf * (tf * (K1 + 1.0)) / denom
}

/// exact -> prefix -> bounded fuzzy over the vocabulary.
fn expand_term(index: &Index, q: &str) -> Vec<(String, f64)> {
    // 1. exact
    if index.df.contains_key(q) {
        let mut out = vec![(q.to_string(), MAX_FUZZY_VOCAB_WEIGHT_EXACT)];
        // Still add prefix variants so "meeting" also catches "meetings".
        add_prefix(index, q, &mut out);
        return out;
    }
    // 2. prefix (only for reasonably long stems to avoid blowups)
    let mut out = Vec::new();
    add_prefix(index, q, &mut out);
    if !out.is_empty() {
        return out;
    }
    // 3. fuzzy: bounded Levenshtein over vocab of similar length
    let max_dist = if q.chars().count() <= 4 { 1 } else { 2 };
    let ql = q.chars().count();
    let mut fuzzy: Vec<(String, f64)> = Vec::new();
    for v in &index.vocab {
        let vl = v.chars().count();
        if vl + 2 < ql || ql + 2 < vl {
            continue; // length too different
        }
        if let Some(d) = bounded_levenshtein(q, v, max_dist) {
            if d > 0 {
                // closer match -> slightly higher weight
                let w = WEIGHT_FUZZY * (1.0 - (d as f64 - 1.0) * 0.2);
                fuzzy.push((v.clone(), w));
            }
        }
    }
    // keep the best few fuzzy candidates by df desc (common words first)
    fuzzy.sort_by(|a, b| {
        let da = index.df.get(&a.0).copied().unwrap_or(0);
        let db = index.df.get(&b.0).copied().unwrap_or(0);
        db.cmp(&da)
    });
    fuzzy.truncate(8);
    fuzzy
}

fn add_prefix(index: &Index, q: &str, out: &mut Vec<(String, f64)>) {
    if q.chars().count() < 3 {
        return; // too short -> avoid matching half the vocab
    }
    let mut prefixed: Vec<(&String, u32)> = index
        .vocab
        .iter()
        .filter(|v| v.as_str() != q && v.starts_with(q))
        .map(|v| (v, index.df.get(v).copied().unwrap_or(0)))
        .collect();
    prefixed.sort_by(|a, b| b.1.cmp(&a.1));
    for (v, _) in prefixed.into_iter().take(12) {
        out.push((v.clone(), WEIGHT_PREFIX));
    }
}

// ── Snippet ──────────────────────────────────────────────────────────

/// Re-read the transcript and extract a short window around the first
/// occurrence of any hit term. Token-cheap (one file, ~30 words).
fn build_snippet(doc: &Doc, hit_terms: &[String]) -> String {
    let text = std::fs::read_to_string(&doc.transcript_path).unwrap_or_default();
    let text = strip_transcript_markers(&text);
    if let Some(s) = snippet_from(&text, hit_terms) {
        return s;
    }
    // fall back to the title if the transcript had no hit (title-only match)
    doc.title.clone()
}

fn snippet_from(text: &str, hit_terms: &[String]) -> Option<String> {
    // Word list with original-cased spans + folded form for matching.
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let folded: Vec<String> = words.iter().map(|w| fold_word(w)).collect();
    let hitset: std::collections::HashSet<&str> = hit_terms.iter().map(|s| s.as_str()).collect();

    // Find first word whose folded form starts with / equals a hit term.
    let pos = folded.iter().position(|w| {
        hitset
            .iter()
            .any(|h| w == h || (h.len() >= 3 && w.starts_with(*h)))
    })?;

    const WIN: usize = 14; // words on each side
    let start = pos.saturating_sub(WIN);
    let end = (pos + WIN + 1).min(words.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
        out.push(' ');
    }
    for (i, w) in words[start..end].iter().enumerate() {
        let gi = start + i;
        if gi == pos {
            out.push('«');
            out.push_str(w);
            out.push('»');
        } else {
            out.push_str(w);
        }
        out.push(' ');
    }
    if end < words.len() {
        out.push('…');
    }
    Some(out.trim().to_string())
}

// ── Tokenization / text utils ────────────────────────────────────────

fn tokenize_unique(text: &str) -> Vec<String> {
    let mut v = Vec::new();
    tokenize_into(text, &mut v);
    v.sort();
    v.dedup();
    v
}

fn tokenize_into(text: &str, out: &mut Vec<String>) {
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.push(fold_char(ch));
        } else if !cur.is_empty() {
            if cur.chars().count() >= MIN_TOKEN_LEN {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.chars().count() >= MIN_TOKEN_LEN {
        out.push(cur);
    }
}

fn fold_word(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .map(fold_char)
        .collect()
}

/// Lowercase + fold common Latin diacritics to ASCII so "città" matches
/// "citta" and "café" matches "cafe".
fn fold_char(c: char) -> char {
    let l = c.to_ascii_lowercase();
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'a',
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'i',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'u',
        'ç' | 'Ç' => 'c',
        'ñ' | 'Ñ' => 'n',
        _ => {
            if l.is_ascii() {
                l
            } else {
                // non-Latin alphanumerics pass through, lowercased best-effort
                c.to_lowercase().next().unwrap_or(c)
            }
        }
    }
}

/// Strip the live-worker line markers `[<ms> ms] [<band>]` so we index
/// the spoken words, not the timestamps / "mic" / "system" labels.
fn strip_transcript_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut rest = line.trim_start();
        // strip up to two leading [...] groups
        for _ in 0..2 {
            if rest.starts_with('[') {
                if let Some(close) = rest.find(']') {
                    rest = rest[close + 1..].trim_start();
                    continue;
                }
            }
            break;
        }
        out.push_str(rest);
        out.push('\n');
    }
    out
}

fn first_h1(md: &str) -> Option<String> {
    for line in md.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("# ") {
            let s = rest.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Levenshtein distance, early-exit if it exceeds `max`. Returns None if
/// the distance is > max. Operates on chars (unicode-safe).
fn bounded_levenshtein(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let d = prev[m];
    if d <= max {
        Some(d)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_folds_and_splits() {
        let v = tokenize_unique("Città di  Café, NFC-tags!");
        assert!(v.contains(&"citta".to_string()));
        assert!(v.contains(&"cafe".to_string()));
        assert!(v.contains(&"nfc".to_string()));
        assert!(v.contains(&"tags".to_string()));
    }

    #[test]
    fn strip_markers_removes_prefixes() {
        let s = strip_transcript_markers("[  123 ms] [mic] hello world\n[5 ms] [system] foo");
        assert!(s.contains("hello world"));
        assert!(s.contains("foo"));
        assert!(!s.contains("mic"));
        assert!(!s.contains("123"));
    }

    #[test]
    fn levenshtein_bounded() {
        assert_eq!(bounded_levenshtein("roadmap", "roadmap", 2), Some(0));
        assert_eq!(bounded_levenshtein("raodmap", "roadmap", 2), Some(2));
        assert_eq!(bounded_levenshtein("xyz", "roadmap", 1), None);
    }

    #[test]
    fn search_ranks_and_is_permissive() {
        let dir = std::env::temp_dir().join(format!("dimmy-search-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let m1 = dir.join("meeting-one");
        let m2 = dir.join("meeting-two");
        std::fs::create_dir_all(&m1).unwrap();
        std::fs::create_dir_all(&m2).unwrap();
        std::fs::write(
            m1.join("transcripts.txt"),
            "[0 ms] [mic] we discussed the pricing roadmap for NFC tags at length",
        )
        .unwrap();
        std::fs::write(m1.join("recap.md"), "# Pricing roadmap\nDecisions...").unwrap();
        std::fs::write(
            m2.join("transcripts.txt"),
            "[0 ms] [mic] lunch plans and the weather today",
        )
        .unwrap();

        // exact
        let hits = run(&dir, "pricing roadmap", 8);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].id, "meeting-one");
        assert_eq!(hits[0].matched, "all");
        assert!(hits[0].snippet.contains('«'));

        // typo tolerance (fuzzy): "raodmap" -> roadmap
        // force rebuild by clearing the cache (dir unchanged across calls)
        let hits = run(&dir, "raodmap", 8);
        assert!(
            hits.iter().any(|h| h.id == "meeting-one"),
            "fuzzy should find roadmap from raodmap"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
