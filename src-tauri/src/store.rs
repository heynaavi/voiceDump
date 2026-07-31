//! Transcript history, backed by SQLite with FTS5 for search.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

pub struct Store(pub Mutex<Connection>);

#[derive(Serialize, Deserialize, Clone)]
pub struct TranscriptMeta {
    pub id: String,
    pub title: String,
    pub source_path: String,
    pub duration: f64,
    pub language: Option<String>,
    pub created_at: i64,
    pub word_count: i64,
    /// Where it came from: "file", "mic", or "hotkey". Drives the origin mark
    /// in the sidebar.
    #[serde(default)]
    pub source: String,
    /// Where the media came from before it was archived. Empty for transcripts
    /// that predate the library.
    #[serde(default)]
    pub origin_path: String,
}

#[derive(Serialize, Deserialize)]
pub struct Transcript {
    #[serde(flatten)]
    pub meta: TranscriptMeta,
    pub text: String,
    /// Paragraphs and raw segments, kept as JSON so the schema can evolve
    /// without a migration.
    pub paragraphs: serde_json::Value,
    pub segments: serde_json::Value,
    /// Normalised amplitude buckets driving the player's waveform.
    pub peaks: serde_json::Value,
}

pub fn open(dir: &PathBuf) -> rusqlite::Result<Connection> {
    std::fs::create_dir_all(dir).ok();
    let conn = Connection::open(dir.join("voicedumps.db"))?;

    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;

        CREATE TABLE IF NOT EXISTS transcripts (
            id          TEXT PRIMARY KEY,
            title       TEXT NOT NULL,
            source_path TEXT NOT NULL,
            duration    REAL NOT NULL DEFAULT 0,
            language    TEXT,
            created_at  INTEGER NOT NULL,
            word_count  INTEGER NOT NULL DEFAULT 0,
            text        TEXT NOT NULL,
            paragraphs  TEXT NOT NULL,
            segments    TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS transcripts_fts
            USING fts5(title, text, content='transcripts', content_rowid='rowid');

        CREATE TRIGGER IF NOT EXISTS transcripts_ai AFTER INSERT ON transcripts BEGIN
            INSERT INTO transcripts_fts(rowid, title, text)
            VALUES (new.rowid, new.title, new.text);
        END;

        CREATE TRIGGER IF NOT EXISTS transcripts_ad AFTER DELETE ON transcripts BEGIN
            INSERT INTO transcripts_fts(transcripts_fts, rowid, title, text)
            VALUES('delete', old.rowid, old.title, old.text);
        END;

        CREATE TRIGGER IF NOT EXISTS transcripts_au AFTER UPDATE ON transcripts BEGIN
            INSERT INTO transcripts_fts(transcripts_fts, rowid, title, text)
            VALUES('delete', old.rowid, old.title, old.text);
            INSERT INTO transcripts_fts(rowid, title, text)
            VALUES (new.rowid, new.title, new.text);
        END;
        "#,
    )?;

    // Added after the first release. `CREATE TABLE IF NOT EXISTS` won't touch an
    // existing table, so bring older databases forward explicitly; the error on
    // a re-run is just "duplicate column", which is the success case here.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN peaks TEXT NOT NULL DEFAULT '[]'",
        [],
    )
    .ok();
    // Everything that predates origin tracking was dropped or picked in the UI.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN source TEXT NOT NULL DEFAULT 'file'",
        [],
    )
    .ok();
    // `source_path` now points into the managed media library; this remembers
    // where the file originally came from so "Reveal in Finder" still lands
    // somewhere meaningful.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN origin_path TEXT NOT NULL DEFAULT ''",
        [],
    )
    .ok();
    // Whether the AI has named this note. Drives the one-time backfill of older
    // transcripts and stops it from ever re-touching a manually renamed one.
    // Existing rows default to 0 so the backfill sweeps them once.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN ai_titled INTEGER NOT NULL DEFAULT 0",
        [],
    )
    .ok();

    // Which app had focus when a dictation was spoken — the one number that
    // turns "you dictated 4,000 words" into something worth knowing. Only the
    // hotkey path fills it; everything else stays empty rather than guessing,
    // and Insights reports the blanks instead of quietly dropping them.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN app_name TEXT NOT NULL DEFAULT ''",
        [],
    )
    .ok();


    Ok(conn)
}

fn row_to_meta(row: &rusqlite::Row) -> rusqlite::Result<TranscriptMeta> {
    Ok(TranscriptMeta {
        id: row.get("id")?,
        title: row.get("title")?,
        source_path: row.get("source_path")?,
        duration: row.get("duration")?,
        language: row.get("language")?,
        created_at: row.get("created_at")?,
        word_count: row.get("word_count")?,
        source: row.get("source").unwrap_or_else(|_| "file".to_string()),
        origin_path: row.get("origin_path").unwrap_or_default(),
    })
}

pub fn list(conn: &Connection, query: Option<&str>) -> rusqlite::Result<Vec<TranscriptMeta>> {
    let trimmed = query.map(str::trim).filter(|q| !q.is_empty());

    match trimmed {
        Some(q) => {
            // Prefix-match every term so search feels live as you type.
            let fts: String = q
                .split_whitespace()
                .map(|t| format!("\"{}\"*", t.replace('"', "")))
                .collect::<Vec<_>>()
                .join(" ");

            let mut stmt = conn.prepare(
                "SELECT t.* FROM transcripts t
                 JOIN transcripts_fts f ON f.rowid = t.rowid
                 WHERE transcripts_fts MATCH ?1
                 ORDER BY rank",
            )?;
            let rows = stmt.query_map([fts], row_to_meta)?;
            rows.collect()
        }
        None => {
            let mut stmt =
                conn.prepare("SELECT * FROM transcripts ORDER BY created_at DESC")?;
            let rows = stmt.query_map([], row_to_meta)?;
            rows.collect()
        }
    }
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Transcript> {
    conn.query_row("SELECT * FROM transcripts WHERE id = ?1", [id], |row| {
        Ok(Transcript {
            meta: row_to_meta(row)?,
            text: row.get("text")?,
            paragraphs: serde_json::from_str(&row.get::<_, String>("paragraphs")?)
                .unwrap_or(serde_json::Value::Null),
            segments: serde_json::from_str(&row.get::<_, String>("segments")?)
                .unwrap_or(serde_json::Value::Null),
            // Absent on transcripts saved before the waveform existed.
            peaks: row
                .get::<_, String>("peaks")
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null),
        })
    })
}

#[allow(clippy::too_many_arguments)]
pub fn insert(
    conn: &Connection,
    id: &str,
    title: &str,
    source_path: &str,
    duration: f64,
    language: Option<&str>,
    created_at: i64,
    text: &str,
    paragraphs: &serde_json::Value,
    segments: &serde_json::Value,
    peaks: &serde_json::Value,
    source: &str,
    origin_path: &str,
) -> rusqlite::Result<()> {
    let word_count = text.split_whitespace().count() as i64;
    conn.execute(
        "INSERT INTO transcripts
            (id, title, source_path, duration, language, created_at, word_count, text, paragraphs, segments, peaks, source, origin_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            title,
            source_path,
            duration,
            language,
            created_at,
            word_count,
            text,
            paragraphs.to_string(),
            segments.to_string(),
            peaks.to_string(),
            source,
            origin_path,
        ],
    )?;
    Ok(())
}

/// Persist an inline edit. Only the prose and its paragraph structure change —
/// timings, peaks and the source file are left alone.
pub fn update_text(
    conn: &Connection,
    id: &str,
    text: &str,
    paragraphs: &serde_json::Value,
) -> rusqlite::Result<()> {
    let word_count = text.split_whitespace().count() as i64;
    conn.execute(
        "UPDATE transcripts SET text = ?2, paragraphs = ?3, word_count = ?4 WHERE id = ?1",
        params![id, text, paragraphs.to_string(), word_count],
    )?;
    Ok(())
}

/// Repoint a transcript at its archived audio, remembering where it came from.
pub fn set_media_path(
    conn: &Connection,
    id: &str,
    stored: &str,
    origin: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcripts
            SET source_path = ?2,
                origin_path = CASE WHEN origin_path = '' THEN ?3 ELSE origin_path END
          WHERE id = ?1",
        params![id, stored, origin],
    )?;
    Ok(())
}

/// Store a waveform computed after the fact for an older transcript.
pub fn set_peaks(
    conn: &Connection,
    id: &str,
    peaks: &serde_json::Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcripts SET peaks = ?2 WHERE id = ?1",
        params![id, peaks.to_string()],
    )?;
    Ok(())
}

/// Record which app was focused when this note was dictated.
///
/// Set separately from `insert` rather than threaded through it: only the
/// hotkey path has an answer, and `insert` already carries eleven arguments
/// shared by four call sites that would all have to pass `None`.
pub fn set_app_name(conn: &Connection, id: &str, app: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcripts SET app_name = ?2 WHERE id = ?1",
        params![id, app],
    )?;
    Ok(())
}

pub fn rename(conn: &Connection, id: &str, title: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcripts SET title = ?2 WHERE id = ?1",
        params![id, title],
    )?;
    Ok(())
}

/// Mark a note as titled, so the AI backfill leaves it alone from now on.
pub fn mark_titled(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE transcripts SET ai_titled = 1 WHERE id = ?1", [id])?;
    Ok(())
}

/// Notes that still want an AI title: never titled, have text, and — for
/// dictations, which are mostly short throwaways — long enough to be worth it.
/// Returns `(id, text)` newest first.
pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM transcripts WHERE id = ?1", [id])?;
    Ok(())
}
