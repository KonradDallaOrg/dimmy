use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single transcription record from the history database.
///
/// v2 schema (see `apply_v2_migration`): adds `enhanced_text`,
/// `audio_path`, `app_process_name`, `app_bundle_id`, `llm_style`,
/// `llm_translate_to`, `size_bytes`. Old rows have NULL for all of
/// them; the v1 callers (single-arg save) keep working unchanged.
pub struct Transcript {
    pub id: i64,
    pub text: String,
    pub language: String,
    pub timestamp: f64,
    pub duration: f64,
    pub word_count: i32,
    /// LLM-enhanced text (post-style-rewrite). NULL when LLM was off
    /// or the call failed — caller should display `text` then.
    pub enhanced_text: Option<String>,
    /// Absolute path to the saved WAV (if audio retention is on).
    /// NULL otherwise.
    pub audio_path: Option<String>,
    /// Foreground process at recording time (Win/Linux). NULL on Mac.
    pub app_process_name: Option<String>,
    /// Foreground bundle id at recording time (Mac). NULL elsewhere.
    pub app_bundle_id: Option<String>,
    /// LLM style applied at the time of save. NULL = LLM was off.
    pub llm_style: Option<String>,
    /// Translation target ISO code or NULL = no translation.
    pub llm_translate_to: Option<String>,
    /// On-disk size of the saved WAV. 0 when no audio stored.
    pub size_bytes: i64,
    /// JSON array of word-level timestamps:
    /// `[{"word":"hello","start_ms":120,"end_ms":480},...]`. NULL
    /// when the backend didn't emit them (current state — Whisper
    /// + Parakeet extraction is a follow-up Phase).
    pub word_timestamps: Option<String>,
}

/// Map a 14-column SELECT result into a `Transcript`. Used by both
/// `recent` and `search` so the column-index ↔ field mapping is in
/// one place.
fn row_to_transcript(row: &rusqlite::Row<'_>) -> rusqlite::Result<Transcript> {
    Ok(Transcript {
        id: row.get(0)?,
        text: row.get(1)?,
        language: row.get(2)?,
        timestamp: row.get(3)?,
        duration: row.get(4)?,
        word_count: row.get(5)?,
        enhanced_text: row.get(6)?,
        audio_path: row.get(7)?,
        app_process_name: row.get(8)?,
        app_bundle_id: row.get(9)?,
        llm_style: row.get(10)?,
        llm_translate_to: row.get(11)?,
        size_bytes: row.get(12)?,
        word_timestamps: row.get(13)?,
    })
}

/// Run the history-audio retention pass on `dir`: delete WAVs older
/// than `keep_days`, then if total size still exceeds `max_mb` MB,
/// delete oldest first until under the cap. Returns (files_removed,
/// bytes_reclaimed). `keep_days = 0` skips age-based pruning;
/// `max_mb = 0` skips size-based. Both 0 = no-op.
///
/// Called from a background thread spawned in `dimmy_init`. Best-
/// effort — any I/O error is logged and the next file is tried.
/// The history.db pointers to deleted files become stale (UI shows
/// "audio missing"); a rare cleanup pass racing a UI playback is
/// fine because Windows holds a file lock during read.
pub fn prune_audio_dir(
    dir: &std::path::Path,
    keep_days: u32,
    max_mb: u32,
) -> Result<(u32, u64), String> {
    if keep_days == 0 && max_mb == 0 {
        return Ok((0, 0));
    }
    if !dir.exists() {
        return Ok((0, 0));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {:?}: {}", dir, e))?;
    let mut files: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if !path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wav"))
                .unwrap_or(false)
            {
                return None;
            }
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            Some((path, meta.len(), modified))
        })
        .collect();

    let mut removed = 0u32;
    let mut bytes_reclaimed = 0u64;

    if keep_days > 0 {
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(keep_days as u64 * 86_400))
            .unwrap_or(std::time::UNIX_EPOCH);
        files.retain(|(path, size, modified)| {
            if *modified < cutoff {
                if let Err(e) = std::fs::remove_file(path) {
                    crate::log(&format!(
                        "[HistoryAudio] prune-age failed on {:?}: {}",
                        path, e
                    ));
                    true
                } else {
                    removed += 1;
                    bytes_reclaimed += *size;
                    false
                }
            } else {
                true
            }
        });
    }

    if max_mb > 0 {
        let max_bytes = max_mb as u64 * 1_048_576;
        let total: u64 = files.iter().map(|(_, s, _)| *s).sum();
        if total > max_bytes {
            files.sort_by_key(|(_, _, modified)| *modified);
            let mut current = total;
            for (path, size, _) in &files {
                if current <= max_bytes {
                    break;
                }
                if let Err(e) = std::fs::remove_file(path) {
                    crate::log(&format!(
                        "[HistoryAudio] prune-size failed on {:?}: {}",
                        path, e
                    ));
                    continue;
                }
                removed += 1;
                bytes_reclaimed += *size;
                current = current.saturating_sub(*size);
            }
        }
    }
    Ok((removed, bytes_reclaimed))
}

/// Bundle of optional metadata for `HistoryStore::save_v2`. All fields
/// are optional; the caller passes only what's meaningful at this
/// recording boundary. Empty strings collapse to None at insert time.
#[derive(Default, Clone)]
pub struct SaveMetadata<'a> {
    pub enhanced_text: Option<&'a str>,
    pub audio_path: Option<&'a str>,
    pub app_process_name: Option<&'a str>,
    pub app_bundle_id: Option<&'a str>,
    pub llm_style: Option<&'a str>,
    pub llm_translate_to: Option<&'a str>,
    pub size_bytes: i64,
}

/// Aggregated statistics across all transcription sessions.
pub struct HistoryStats {
    pub total_words: i64,
    pub total_sessions: i64,
    pub total_duration: f64,
}

/// Thread-safe transcription history backed by SQLite with FTS5 full-text search.
pub struct HistoryStore {
    conn: Mutex<Connection>,
}

impl HistoryStore {
    /// Open (or create) the history database at `db_path`.
    /// Pass `Path::new(":memory:")` for an ephemeral in-memory database (tests).
    pub fn new(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|e| format!("sqlite open: {e}"))?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("sqlite pragma: {e}"))?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS transcripts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                text TEXT NOT NULL,
                language TEXT NOT NULL DEFAULT 'en',
                timestamp REAL NOT NULL,
                duration REAL NOT NULL DEFAULT 0.0,
                word_count INTEGER NOT NULL DEFAULT 0
            );
            ",
        )
        .map_err(|e| format!("sqlite schema base: {e}"))?;

        // v2 migration: add metadata + enhanced_text + audio_path columns
        // and rebuild the FTS5 index to cover both raw + enhanced text.
        // Idempotent: ALTER TABLE ADD COLUMN guarded by pragma probe so
        // re-runs (existing user db) are no-ops.
        Self::apply_v2_migration(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Add the v2 columns + drop+recreate the FTS5 virtual table to
    /// cover `text` and `enhanced_text`. Existing rows keep their
    /// content; new columns default to NULL and the FTS reindex
    /// re-emits one row per existing transcript.
    fn apply_v2_migration(conn: &Connection) -> Result<(), String> {
        // Each ALTER is wrapped in a probe — sqlite < 3.35 lacks
        // `ADD COLUMN IF NOT EXISTS`, but we can check pragma_table_info
        // and skip when the column is already present.
        for (col, decl) in &[
            ("enhanced_text", "ADD COLUMN enhanced_text TEXT"),
            ("audio_path", "ADD COLUMN audio_path TEXT"),
            ("app_process_name", "ADD COLUMN app_process_name TEXT"),
            ("app_bundle_id", "ADD COLUMN app_bundle_id TEXT"),
            ("llm_style", "ADD COLUMN llm_style TEXT"),
            ("llm_translate_to", "ADD COLUMN llm_translate_to TEXT"),
            (
                "size_bytes",
                "ADD COLUMN size_bytes INTEGER NOT NULL DEFAULT 0",
            ),
            // Word-level timestamps as JSON array. Schema lands now;
            // extraction from whisper.cpp segments + Parakeet TDT
            // alignments is a follow-up Phase. NULL until wired.
            ("word_timestamps", "ADD COLUMN word_timestamps TEXT"),
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM pragma_table_info('transcripts') WHERE name = ?1",
                    params![col],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if !exists {
                let sql = format!("ALTER TABLE transcripts {decl}");
                conn.execute(&sql, [])
                    .map_err(|e| format!("v2 migration {col}: {e}"))?;
            }
        }

        // Index on timestamp DESC so `recent()` is a no-scan lookup
        // even with 10k+ rows.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_transcripts_timestamp
                 ON transcripts (timestamp DESC)",
            [],
        )
        .map_err(|e| format!("v2 index: {e}"))?;

        // Drop old FTS5 (if any) and rebuild covering text+enhanced.
        // Triggers reference the new column set so old triggers must
        // go too; CREATE OR REPLACE isn't a thing in sqlite, so we
        // drop and recreate.
        conn.execute_batch(
            "
            DROP TRIGGER IF EXISTS transcripts_ai;
            DROP TRIGGER IF EXISTS transcripts_ad;
            DROP TRIGGER IF EXISTS transcripts_au;
            DROP TABLE IF EXISTS transcripts_fts;

            CREATE VIRTUAL TABLE transcripts_fts USING fts5(
                text, enhanced_text, content=transcripts, content_rowid=id
            );

            INSERT INTO transcripts_fts(rowid, text, enhanced_text)
                SELECT id, text, COALESCE(enhanced_text, '') FROM transcripts;

            CREATE TRIGGER transcripts_ai AFTER INSERT ON transcripts BEGIN
                INSERT INTO transcripts_fts(rowid, text, enhanced_text)
                    VALUES (new.id, new.text, COALESCE(new.enhanced_text, ''));
            END;

            CREATE TRIGGER transcripts_ad AFTER DELETE ON transcripts BEGIN
                INSERT INTO transcripts_fts(transcripts_fts, rowid, text, enhanced_text)
                    VALUES('delete', old.id, old.text, COALESCE(old.enhanced_text, ''));
            END;

            CREATE TRIGGER transcripts_au AFTER UPDATE ON transcripts BEGIN
                INSERT INTO transcripts_fts(transcripts_fts, rowid, text, enhanced_text)
                    VALUES('delete', old.id, old.text, COALESCE(old.enhanced_text, ''));
                INSERT INTO transcripts_fts(rowid, text, enhanced_text)
                    VALUES (new.id, new.text, COALESCE(new.enhanced_text, ''));
            END;
            ",
        )
        .map_err(|e| format!("v2 fts rebuild: {e}"))?;

        Ok(())
    }

    /// Persist a new transcription.
    ///
    /// `text` must be non-empty (whitespace-only counts as empty).
    /// `duration` must be non-negative.
    /// Returns the row id of the inserted record.
    pub fn save(&self, text: &str, language: &str, duration: f64) -> Result<i64, String> {
        let trimmed = text.trim();
        assert!(!trimmed.is_empty(), "save() called with empty text");
        if trimmed.is_empty() {
            return Err("text must not be empty".into());
        }
        assert!(
            duration >= 0.0,
            "save() called with negative duration: {duration}"
        );
        assert!(!duration.is_nan(), "save() called with NaN duration");
        assert!(
            !language.trim().is_empty(),
            "save() called with empty language"
        );

        let word_count = trimmed.split_whitespace().count() as i32;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system time: {e}"))?
            .as_secs_f64();

        assert!(timestamp > 0.0, "timestamp must be positive");
        assert!(
            word_count > 0,
            "word_count must be positive for non-empty text"
        );

        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        conn.execute(
            "INSERT INTO transcripts (text, language, timestamp, duration, word_count)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![trimmed, language, timestamp, duration, word_count],
        )
        .map_err(|e| format!("sqlite insert: {e}"))?;

        let id = conn.last_insert_rowid();
        assert!(id > 0, "inserted row id must be positive");
        Ok(id)
    }

    /// v2 save: persist a transcription along with the v2 columns.
    /// `text` is the raw STT output (always required). `meta` carries
    /// every optional field — pass `SaveMetadata::default()` for the
    /// minimum-info case (matches the v1 `save` behaviour).
    pub fn save_v2(
        &self,
        text: &str,
        language: &str,
        duration: f64,
        meta: SaveMetadata<'_>,
    ) -> Result<i64, String> {
        let trimmed = text.trim();
        assert!(!trimmed.is_empty(), "save_v2() called with empty text");
        if trimmed.is_empty() {
            return Err("text must not be empty".into());
        }
        assert!(
            duration >= 0.0 && !duration.is_nan(),
            "save_v2() invalid duration: {duration}"
        );
        assert!(
            !language.trim().is_empty(),
            "save_v2() called with empty language"
        );
        let word_count = trimmed.split_whitespace().count() as i32;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system time: {e}"))?
            .as_secs_f64();

        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let opt = |s: Option<&str>| s.filter(|x| !x.trim().is_empty()).map(|x| x.to_string());
        conn.execute(
            "INSERT INTO transcripts
                 (text, language, timestamp, duration, word_count,
                  enhanced_text, audio_path, app_process_name,
                  app_bundle_id, llm_style, llm_translate_to, size_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                trimmed,
                language,
                timestamp,
                duration,
                word_count,
                opt(meta.enhanced_text),
                opt(meta.audio_path),
                opt(meta.app_process_name),
                opt(meta.app_bundle_id),
                opt(meta.llm_style),
                opt(meta.llm_translate_to),
                meta.size_bytes.max(0),
            ],
        )
        .map_err(|e| format!("sqlite insert v2: {e}"))?;
        let id = conn.last_insert_rowid();
        assert!(id > 0, "inserted row id must be positive");
        Ok(id)
    }

    /// Return the most recent transcriptions, newest first.
    pub fn recent(&self, limit: i32) -> Result<Vec<Transcript>, String> {
        assert!(limit > 0, "recent() limit must be positive, got {limit}");

        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, text, language, timestamp, duration, word_count,
                        enhanced_text, audio_path, app_process_name,
                        app_bundle_id, llm_style, llm_translate_to,
                        COALESCE(size_bytes, 0), word_timestamps
                 FROM transcripts ORDER BY timestamp DESC LIMIT ?1",
            )
            .map_err(|e| format!("sqlite prepare: {e}"))?;

        let rows = stmt
            .query_map(params![limit], row_to_transcript)
            .map_err(|e| format!("sqlite query: {e}"))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("sqlite row: {e}"))?);
        }
        Ok(results)
    }

    /// Full-text search across transcription text.
    ///
    /// The query is sanitized by wrapping each token in double quotes so that
    /// FTS5 special characters (`AND`, `OR`, `NOT`, `*`, `^`, etc.) are treated
    /// as literals. An empty / whitespace-only query falls back to `recent()`.
    pub fn search(&self, query: &str, limit: i32) -> Result<Vec<Transcript>, String> {
        assert!(limit > 0, "search() limit must be positive, got {limit}");

        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.recent(limit);
        }

        // Sanitize: quote each token so FTS5 operators are neutralised.
        let sanitized: String = trimmed
            .split_whitespace()
            .map(|token| {
                // Escape any internal double-quotes by doubling them.
                let escaped = token.replace('"', "\"\"");
                format!("\"{escaped}\"")
            })
            .collect::<Vec<_>>()
            .join(" ");

        assert!(
            !sanitized.is_empty(),
            "sanitized FTS query must not be empty"
        );

        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.text, t.language, t.timestamp, t.duration, t.word_count,
                        t.enhanced_text, t.audio_path, t.app_process_name,
                        t.app_bundle_id, t.llm_style, t.llm_translate_to,
                        COALESCE(t.size_bytes, 0), t.word_timestamps
                 FROM transcripts t
                 JOIN transcripts_fts f ON t.id = f.rowid
                 WHERE transcripts_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|e| format!("sqlite prepare: {e}"))?;

        let rows = stmt
            .query_map(params![sanitized, limit], row_to_transcript)
            .map_err(|e| format!("sqlite query: {e}"))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("sqlite row: {e}"))?);
        }
        Ok(results)
    }

    /// Update the LLM-enhanced text on an existing transcript row.
    /// Called by the C#/Swift host after the post-transcribe LLM
    /// rewrite finishes (the LLM call lives outside dimmy_stop_recording
    /// so the user gets the raw transcript pasted instantly; the
    /// enhanced version follows asynchronously). The FTS5 trigger
    /// reindexes automatically.
    pub fn update_enhanced(&self, id: i64, enhanced: &str) -> Result<(), String> {
        assert!(id > 0, "update_enhanced() id must be positive, got {id}");
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let opt: Option<&str> = if enhanced.trim().is_empty() {
            None
        } else {
            Some(enhanced)
        };
        let affected = conn
            .execute(
                "UPDATE transcripts SET enhanced_text = ?1 WHERE id = ?2",
                params![opt, id],
            )
            .map_err(|e| format!("sqlite update_enhanced: {e}"))?;
        if affected == 0 {
            return Err(format!("no transcript with id {id}"));
        }
        Ok(())
    }

    /// Set / clear the word_timestamps JSON for a row. Empty / NULL
    /// indicates the backend didn't emit them. Caller serialises the
    /// `[{"word":..., "start_ms":..., "end_ms":...}, ...]` shape.
    pub fn update_word_timestamps(&self, id: i64, json: &str) -> Result<(), String> {
        assert!(
            id > 0,
            "update_word_timestamps() id must be positive, got {id}"
        );
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let opt: Option<&str> = if json.trim().is_empty() {
            None
        } else {
            Some(json)
        };
        let affected = conn
            .execute(
                "UPDATE transcripts SET word_timestamps = ?1 WHERE id = ?2",
                params![opt, id],
            )
            .map_err(|e| format!("sqlite update_word_timestamps: {e}"))?;
        if affected == 0 {
            return Err(format!("no transcript with id {id}"));
        }
        Ok(())
    }

    /// Update the audio path + size for an existing transcript row.
    /// Called by the audio retention layer after it has streamed the
    /// PCM samples to disk. NULL/empty path clears the link.
    pub fn update_audio(&self, id: i64, audio_path: &str, size_bytes: i64) -> Result<(), String> {
        assert!(id > 0, "update_audio() id must be positive, got {id}");
        assert!(size_bytes >= 0, "size_bytes must be non-negative");
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let opt: Option<&str> = if audio_path.trim().is_empty() {
            None
        } else {
            Some(audio_path)
        };
        let affected = conn
            .execute(
                "UPDATE transcripts
                    SET audio_path = ?1, size_bytes = ?2 WHERE id = ?3",
                params![opt, size_bytes, id],
            )
            .map_err(|e| format!("sqlite update_audio: {e}"))?;
        if affected == 0 {
            return Err(format!("no transcript with id {id}"));
        }
        Ok(())
    }

    /// Delete a transcript by id.
    pub fn delete(&self, id: i64) -> Result<(), String> {
        assert!(id > 0, "delete() id must be positive, got {id}");

        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let affected = conn
            .execute("DELETE FROM transcripts WHERE id = ?1", params![id])
            .map_err(|e| format!("sqlite delete: {e}"))?;

        // Not asserting affected == 1 — deleting a non-existent row is idempotent.
        // But we do note it for diagnostics.
        if affected == 0 {
            return Err(format!("no transcript with id {id}"));
        }
        Ok(())
    }

    /// Aggregate statistics across all stored transcriptions.
    pub fn stats(&self) -> Result<HistoryStats, String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT COALESCE(SUM(word_count), 0),
                        COUNT(*),
                        COALESCE(SUM(duration), 0.0)
                 FROM transcripts",
            )
            .map_err(|e| format!("sqlite prepare: {e}"))?;

        let stats = stmt
            .query_row([], |row| {
                Ok(HistoryStats {
                    total_words: row.get(0)?,
                    total_sessions: row.get(1)?,
                    total_duration: row.get(2)?,
                })
            })
            .map_err(|e| format!("sqlite query: {e}"))?;

        assert!(stats.total_words >= 0, "total_words must be non-negative");
        assert!(
            stats.total_sessions >= 0,
            "total_sessions must be non-negative"
        );
        assert!(
            stats.total_duration >= 0.0,
            "total_duration must be non-negative"
        );

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> HistoryStore {
        HistoryStore::new(Path::new(":memory:")).expect("in-memory DB")
    }

    #[test]
    fn save_and_retrieve() {
        let store = test_store();
        let id = store.save("Hello world", "en", 2.5).unwrap();
        assert!(id > 0);
        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].text, "Hello world");
        assert_eq!(recent[0].language, "en");
        assert_eq!(recent[0].word_count, 2);
    }

    #[test]
    fn search_fts5() {
        let store = test_store();
        store.save("The quick brown fox jumps", "en", 3.0).unwrap();
        store.save("A lazy dog sleeps", "en", 2.0).unwrap();
        let results = store.search("fox", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].text.contains("fox"));
    }

    #[test]
    fn delete_removes_transcript() {
        let store = test_store();
        let id = store.save("To be deleted", "en", 1.0).unwrap();
        store.delete(id).unwrap();
        let recent = store.recent(10).unwrap();
        assert!(recent.is_empty());
    }

    #[test]
    fn stats_aggregation() {
        let store = test_store();
        store.save("Hello world", "en", 2.0).unwrap();
        store.save("Ciao mondo come stai", "it", 3.5).unwrap();
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_sessions, 2);
        assert_eq!(stats.total_words, 6);
        assert!((stats.total_duration - 5.5).abs() < 0.01);
    }

    #[test]
    fn recent_ordered_by_newest_first() {
        let store = test_store();
        store.save("First", "en", 1.0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.save("Second", "en", 1.0).unwrap();
        let recent = store.recent(10).unwrap();
        assert_eq!(recent[0].text, "Second");
        assert_eq!(recent[1].text, "First");
    }

    #[test]
    fn empty_text_not_saved() {
        let store = test_store();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.save("", "en", 1.0)));
        // Should panic due to assertion (negative space programming)
        assert!(result.is_err());
    }

    #[test]
    fn whitespace_only_text_not_saved() {
        let store = test_store();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.save("   \t\n  ", "en", 1.0)
        }));
        assert!(result.is_err());
    }

    #[test]
    fn special_chars_in_search() {
        let store = test_store();
        store
            .save("It's a test with \"quotes\"", "en", 1.0)
            .unwrap();
        let results = store.search("quotes", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn empty_search_falls_back_to_recent() {
        let store = test_store();
        store.save("Some text", "en", 1.0).unwrap();
        let results = store.search("", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "Some text");
    }

    #[test]
    fn stats_on_empty_db() {
        let store = test_store();
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_words, 0);
        assert_eq!(stats.total_sessions, 0);
        assert!((stats.total_duration - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn delete_nonexistent_returns_error() {
        let store = test_store();
        let result = store.delete(9999);
        assert!(result.is_err());
    }

    #[test]
    fn search_after_delete_excludes_removed() {
        let store = test_store();
        let id = store.save("unique findable phrase", "en", 1.0).unwrap();
        store.delete(id).unwrap();
        let results = store.search("findable", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn save_preserves_duration() {
        let store = test_store();
        store.save("test duration", "en", 7.25).unwrap();
        let recent = store.recent(1).unwrap();
        assert!((recent[0].duration - 7.25).abs() < 0.001);
    }

    #[test]
    fn save_trims_whitespace() {
        let store = test_store();
        store.save("  padded text  ", "en", 1.0).unwrap();
        let recent = store.recent(1).unwrap();
        assert_eq!(recent[0].text, "padded text");
        assert_eq!(recent[0].word_count, 2);
    }

    #[test]
    #[should_panic(expected = "negative duration")]
    fn negative_duration_panics() {
        let store = test_store();
        let _ = store.save("text", "en", -1.0);
    }

    #[test]
    #[should_panic(expected = "limit must be positive")]
    fn zero_limit_recent_panics() {
        let store = test_store();
        let _ = store.recent(0);
    }

    #[test]
    #[should_panic(expected = "limit must be positive")]
    fn zero_limit_search_panics() {
        let store = test_store();
        let _ = store.search("test", 0);
    }

    #[test]
    #[should_panic(expected = "id must be positive")]
    fn zero_id_delete_panics() {
        let store = test_store();
        let _ = store.delete(0);
    }
}
