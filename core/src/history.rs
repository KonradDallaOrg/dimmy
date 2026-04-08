use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single transcription record from the history database.
pub struct Transcript {
    pub id: i64,
    pub text: String,
    pub language: String,
    pub timestamp: f64,
    pub duration: f64,
    pub word_count: i32,
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

            CREATE VIRTUAL TABLE IF NOT EXISTS transcripts_fts USING fts5(
                text, content=transcripts, content_rowid=id
            );

            CREATE TRIGGER IF NOT EXISTS transcripts_ai AFTER INSERT ON transcripts BEGIN
                INSERT INTO transcripts_fts(rowid, text) VALUES (new.id, new.text);
            END;

            CREATE TRIGGER IF NOT EXISTS transcripts_ad AFTER DELETE ON transcripts BEGIN
                INSERT INTO transcripts_fts(transcripts_fts, rowid, text)
                    VALUES('delete', old.id, old.text);
            END;

            CREATE TRIGGER IF NOT EXISTS transcripts_au AFTER UPDATE ON transcripts BEGIN
                INSERT INTO transcripts_fts(transcripts_fts, rowid, text)
                    VALUES('delete', old.id, old.text);
                INSERT INTO transcripts_fts(rowid, text) VALUES (new.id, new.text);
            END;
            ",
        )
        .map_err(|e| format!("sqlite schema: {e}"))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
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

    /// Return the most recent transcriptions, newest first.
    pub fn recent(&self, limit: i32) -> Result<Vec<Transcript>, String> {
        assert!(limit > 0, "recent() limit must be positive, got {limit}");

        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, text, language, timestamp, duration, word_count
                 FROM transcripts ORDER BY timestamp DESC LIMIT ?1",
            )
            .map_err(|e| format!("sqlite prepare: {e}"))?;

        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(Transcript {
                    id: row.get(0)?,
                    text: row.get(1)?,
                    language: row.get(2)?,
                    timestamp: row.get(3)?,
                    duration: row.get(4)?,
                    word_count: row.get(5)?,
                })
            })
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
                "SELECT t.id, t.text, t.language, t.timestamp, t.duration, t.word_count
                 FROM transcripts t
                 JOIN transcripts_fts f ON t.id = f.rowid
                 WHERE transcripts_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|e| format!("sqlite prepare: {e}"))?;

        let rows = stmt
            .query_map(params![sanitized, limit], |row| {
                Ok(Transcript {
                    id: row.get(0)?,
                    text: row.get(1)?,
                    language: row.get(2)?,
                    timestamp: row.get(3)?,
                    duration: row.get(4)?,
                    word_count: row.get(5)?,
                })
            })
            .map_err(|e| format!("sqlite query: {e}"))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("sqlite row: {e}"))?);
        }
        Ok(results)
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
