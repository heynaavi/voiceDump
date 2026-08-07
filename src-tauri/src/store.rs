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
    /// Where it came from: "file", "mic", or "discord". Drives the origin mark
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
    /// The structured overview, once one has been generated. `Null` covers both
    /// "never asked" and "asked, and the model had nothing to say" — the UI
    /// offers to generate in either case, which is the right affordance for both.
    #[serde(default)]
    pub brief: serde_json::Value,
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
    // Which speech model produced this note, and how long it ran. Older rows keep
    // the empty default forever: nothing in the history implies which weights were
    // loaded at the time, and Insights would rather show a smaller honest sample
    // than back-fill a guess.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN model TEXT NOT NULL DEFAULT ''",
        [],
    )
    .ok();
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN transcribe_ms INTEGER NOT NULL DEFAULT 0",
        [],
    )
    .ok();
    // The structured overview, as the JSON the sidecar returned. Stored whole
    // rather than shredded into columns for the same reason `paragraphs` is: the
    // brief's shape is the model's to change, and a note without one is the
    // normal case, not a missing row.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN brief TEXT NOT NULL DEFAULT ''",
        [],
    )
    .ok();

    // The overview as prose, which is what search should read. The `brief`
    // column beside it is raw JSON, so indexing that put `action_items`,
    // `decisions`, `key_points` and `summary` into every briefed note — words
    // with no discriminating power, which is why "What are my action items?"
    // matched everything and therefore scored below the relevance floor and
    // returned nothing at all. See `index_the_prose`.
    conn.execute(
        "ALTER TABLE transcripts ADD COLUMN brief_text TEXT NOT NULL DEFAULT ''",
        [],
    )
    .ok();

    // Answers that cost money to produce. Keyed by a fingerprint of the history
    // they describe, so opening Insights twice is free and the model is only
    // asked again once there is something new to read.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS insight_cache (
            key        TEXT PRIMARY KEY,
            value      TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        "#,
    )
    .ok();

    // After the `ALTER TABLE`s, not before them. This is an external-content
    // index: its triggers read `new.brief`, and on a database being created for
    // the first time that column does not exist until the statements above have
    // run. Building the index first leaves every insert failing on a column the
    // table gains a moment later.
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS transcripts_fts
            USING fts5(title, text, brief_text, content='transcripts', content_rowid='rowid',
                       -- porter folds plural and tense together, so "decision"
                       -- finds "Decisions" and "agree" finds "agreed". It only
                       -- knows English suffixes; on any other language it is a
                       -- no-op and unicode61 does the tokenising, so it costs
                       -- nothing to anybody it cannot help.
                       tokenize="porter unicode61");

        CREATE TRIGGER IF NOT EXISTS transcripts_ai AFTER INSERT ON transcripts BEGIN
            INSERT INTO transcripts_fts(rowid, title, text, brief_text)
            VALUES (new.rowid, new.title, new.text, new.brief_text);
        END;

        CREATE TRIGGER IF NOT EXISTS transcripts_ad AFTER DELETE ON transcripts BEGIN
            INSERT INTO transcripts_fts(transcripts_fts, rowid, title, text, brief_text)
            VALUES('delete', old.rowid, old.title, old.text, old.brief_text);
        END;

        CREATE TRIGGER IF NOT EXISTS transcripts_au AFTER UPDATE ON transcripts BEGIN
            INSERT INTO transcripts_fts(transcripts_fts, rowid, title, text, brief_text)
            VALUES('delete', old.rowid, old.title, old.text, old.brief_text);
            INSERT INTO transcripts_fts(rowid, title, text, brief_text)
            VALUES (new.rowid, new.title, new.text, new.brief_text);
        END;
        "#,
    )?;

    crate::graph::init(&conn).ok();
    #[cfg(target_os = "macos")]
    crate::chat::init(&conn).ok();

    reopen_generated_titles(&conn).ok();
    widen_the_index(&conn).ok();
    index_the_prose(&conn).ok();

    Ok(conn)
}

/// The schema version this build expects. Bumped only for corrections that must
/// run once and never again — the `ALTER TABLE`s above are idempotent and don't
/// need it.
const SCHEMA_VERSION: i64 = 3;

/// Bring an index built before overviews existed up to including them.
///
/// Search covered the title and the words that were said. It did not cover the
/// summary, which is the one part of a note written to be read — so a meeting
/// whose overview says "pricing" in as many words was unfindable by searching
/// for pricing unless somebody happened to say it out loud.
///
/// Gated on the index's own shape rather than a schema version, because that is
/// the actual precondition and it cannot drift: an index that already has the
/// column needs nothing, whatever any counter says. `CREATE TABLE IF NOT
/// EXISTS` will not widen a table that is already there, so the old one is
/// dropped and rebuilt from the content table — nothing is lost, an
/// external-content index holds no data of its own.
fn widen_the_index(conn: &Connection) -> rusqlite::Result<()> {
    // Two reasons to rebuild, and this asks the index about both rather than
    // trusting a version counter: whether it covers the overview at all, and
    // whether it was built with the stemmer.
    let current: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
          WHERE type = 'table' AND name = 'transcripts_fts'
            AND sql LIKE '%brief_text%' AND sql LIKE '%porter%'",
        [],
        |row| row.get(0),
    )?;
    if current > 0 {
        return Ok(());
    }

    conn.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS transcripts_ai;
        DROP TRIGGER IF EXISTS transcripts_ad;
        DROP TRIGGER IF EXISTS transcripts_au;
        DROP TABLE IF EXISTS transcripts_fts;

        CREATE VIRTUAL TABLE transcripts_fts
            USING fts5(title, text, brief_text, content='transcripts', content_rowid='rowid',
                       -- porter folds plural and tense together, so "decision"
                       -- finds "Decisions" and "agree" finds "agreed". It only
                       -- knows English suffixes; on any other language it is a
                       -- no-op and unicode61 does the tokenising, so it costs
                       -- nothing to anybody it cannot help.
                       tokenize="porter unicode61");

        CREATE TRIGGER transcripts_ai AFTER INSERT ON transcripts BEGIN
            INSERT INTO transcripts_fts(rowid, title, text, brief_text)
            VALUES (new.rowid, new.title, new.text, new.brief_text);
        END;

        CREATE TRIGGER transcripts_ad AFTER DELETE ON transcripts BEGIN
            INSERT INTO transcripts_fts(transcripts_fts, rowid, title, text, brief_text)
            VALUES('delete', old.rowid, old.title, old.text, old.brief_text);
        END;

        CREATE TRIGGER transcripts_au AFTER UPDATE ON transcripts BEGIN
            INSERT INTO transcripts_fts(transcripts_fts, rowid, title, text, brief_text)
            VALUES('delete', old.rowid, old.title, old.text, old.brief_text);
            INSERT INTO transcripts_fts(rowid, title, text, brief_text)
            VALUES (new.rowid, new.title, new.text, new.brief_text);
        END;

        INSERT INTO transcripts_fts(transcripts_fts) VALUES('rebuild');
        "#,
    )?;
    eprintln!("[store] search index widened to cover overviews");
    Ok(())
}

/// Notes matching any of a question's words, best first.
///
/// [`list`] ANDs its terms, which is right for a search box — you narrow as you
/// type. It is wrong for a question: "what did we decide about pricing" ANDed
/// across five words matches nothing in any library. This ORs them and lets
/// bm25 do the ranking, so a note that hits "pricing" and "decide" outranks one
/// that only hits "about".
pub fn search_any(conn: &Connection, question: &str, limit: usize) -> rusqlite::Result<Vec<String>> {
    let terms: Vec<String> = question
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(str::trim)
        .filter(|t| t.len() >= 3 && !is_noise(t))
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    // The cut is *relative to the best match*, never an absolute score, and
    // that is the whole point of it.
    //
    // bm25 is negative, more negative is better, and its magnitude grows with
    // the size of the corpus — a term is scored by how rare it is, and rarity
    // is a fact about the library rather than about the note. An absolute
    // threshold is therefore correct at exactly one library size and wrong
    // everywhere else. Measured: the same relevant note scored -0.0 in a
    // 2-note library, -5.6 in a 4-note one, -12.2 at ten and -43.6 at two
    // hundred. A fixed -6.0 floor returns nothing at all below five notes,
    // and a meeting note that genuinely contains "action items" scored -2.06
    // in a 21-note library and was cut by a floor of -3.0.
    //
    // Keeping everything within a fraction of the best score has no such
    // problem: it means the same thing in a library of five notes and a
    // library of fifty thousand, in any language, whatever anybody records.
    // The cut happens in Rust rather than in SQL because `bm25` is an FTS5
    // auxiliary function: it is only usable in a query that matches the index
    // directly, so it cannot be referenced through a CTE or a sub-select. The
    // rows come back ranked and are trimmed here.
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id,
                bm25(transcripts_fts, 2.0, 1.0, 1.0) AS score
           FROM transcripts t
           JOIN transcripts_fts f ON f.rowid = t.rowid
          WHERE transcripts_fts MATCH ?1
            AND {}
          -- Ranked on the score damped by length, cut below on the raw one:
          -- cutting on the damped score would penalise a short note twice.
          -- The damping is what replaced the exclusion rules this used to
          -- have — a slight note sinks below a substantial one instead of
          -- being silently removed from somebody's library.
          ORDER BY score * min(1.0, t.word_count / 60.0)
          LIMIT ?2"
    , TOO_SLIGHT))?;

    // Fetched a little wider than asked for, so the relative cut has something
    // to cut from rather than trimming a list that was already truncated.
    let hits: Vec<(String, f64)> = stmt
        .query_map(rusqlite::params![terms.join(" OR "), (limit * 3) as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    // The best score, bm25 being negative, is the most negative one.
    let best = hits.iter().map(|(_, s)| *s).fold(0.0_f64, f64::min);
    Ok(hits
        .into_iter()
        .filter(|(_, score)| *score <= best * KEEP_WITHIN)
        .map(|(id, _)| id)
        .take(limit)
        .collect())
}

/// How far below the best match a note may score and still be worth reading.
///
/// A third. Loose enough that a note answering the question from a different
/// angle survives, tight enough that a note sharing one incidental word with a
/// six-word question does not.
///
/// The one case it deliberately does not handle is a question where *every*
/// match is weak — the best is kept regardless, because there is no absolute
/// scale to judge "weak" against. That is covered better elsewhere anyway:
/// social messages never reach retrieval at all, and the model is told to say
/// "none of these notes mention that" rather than answer from notes that do
/// not bear on the question.
const KEEP_WITHIN: f64 = 0.35;

/// Notes too slight to answer anything.
///
/// This deliberately excludes almost nothing, and the reason is that it runs on
/// other people's libraries. An earlier version matched English probe phrases —
/// "test", "can you hear me", "is this working" — and a short note ending in a
/// question mark. It suppressed 52 of 188 notes here, correctly, because this
/// library's short notes really are microphone checks.
///
/// Tried against a library of ordinary meeting notes, the same rule dropped six
/// realistic notes out of six:
///
/// - "We need to test the new payment API before Friday"
/// - "Reminder: the QA test plan for the mobile app is due Monday"
/// - "Blood test results came back normal, follow up in six months"
/// - "The A/B test on the pricing page finished, variant B won"
/// - "Should we move the launch given the Acme escalation?"
/// - "Ask Priya whether the budget freeze applies to existing contracts?"
///
/// Every one is a real note. "test" is an ordinary English word, and a note
/// phrased as a question is content in most people's libraries even though it
/// is chatter in this one. Worse, dropping a note is *invisible*: nobody can
/// tell that the answer was built without it. A weak note surviving into the
/// sources is visible, and the model has been told to ignore what does not bear
/// on the question.
///
/// So what is left is only what holds in any language and any library: a note
/// too short to contain an answer, and one that is literal repetition. Ranking
/// does the rest — see the length damping in [`search_any`], which sinks a
/// short note without silencing it.
/// Kept as a format hole so the SQL and [`TOO_SLIGHT`] cannot disagree.
const WORTH_READING_SQL: &str = "t.word_count >= {}";

/// Under this a note cannot contain an answer to anything, in any language.
const TOO_SLIGHT: i64 = 10;

/// Below this share of distinct words, a short note is somebody repeating
/// themselves into the microphone rather than saying something.
///
/// Only applied to short notes: real prose of any length sits far above this,
/// but a long note quoting a chant should not be judged on it.
const REPETITION_FLOOR: f64 = 0.6;
const REPETITION_CHECKED_UNDER: i64 = 25;

/// The same judgement as [`WORTH_READING_SQL`], plus the repetition test that
/// is not worth expressing in SQL.
///
/// The two are checked against each other by `the_two_chatter_filters_agree`,
/// so the SQL cannot quietly diverge from this.
pub fn worth_reading(text: &str, word_count: i64) -> bool {
    if word_count < TOO_SLIGHT {
        return false;
    }
    if word_count < REPETITION_CHECKED_UNDER {
        let words: Vec<String> = text
            .to_lowercase()
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|w| !w.is_empty())
            .collect();
        if !words.is_empty() {
            let mut distinct = words.clone();
            distinct.sort();
            distinct.dedup();
            if (distinct.len() as f64) / (words.len() as f64) < REPETITION_FLOOR {
                return false;
            }
        }
    }
    true
}

/// Words that are in every note and so distinguish none of them.
///
/// Short and deliberately not a full stopword list: bm25 already discounts a
/// term that appears everywhere. What this is for is the handful of words that
/// make up the *question* rather than its subject — a question is mostly "what
/// did we say about", and matching on those is what turns a search into a
/// random sample of the library.
pub fn is_noise_word(word: &str) -> bool {
    is_noise(word)
}

fn is_noise(word: &str) -> bool {
    const NOISE: &[&str] = &[
        "the", "and", "for", "was", "were", "are", "did", "does", "what", "when", "where", "who",
        "why", "how", "about", "with", "from", "have", "has", "had", "you", "your", "our", "any",
        "all", "can", "could", "would", "should", "there", "their", "this", "that", "these",
        "those", "say", "said", "says", "tell", "told", "get", "got", "some", "much", "many",
        "been", "being", "into", "over", "than", "then", "them", "they", "his", "her", "its",
    ];
    NOISE.contains(&word.to_lowercase().as_str())
}

/// Un-mark every note the app named itself, so the AI namer can have a go.
///
/// The public build had no AI at all, so every note it saved was marked titled
/// on the way in — not because anything had named it, but so that a backfill
/// which did not exist there would never come looking. Now that it does have a
/// namer, that flag is a lie on every row, and left alone it would keep a whole
/// library called "Meeting — 6 Aug, 1:41 PM" and "I think one thing that we
/// can…" for good.
///
/// Only rows still carrying a name the app generated are reopened. A title
/// somebody typed is theirs, and no amount of "the model would do better" makes
/// overwriting it acceptable — so the test is not "does this look bad", it is
/// "can we show this exact string was produced by us".
fn reopen_generated_titles(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    let rows: Vec<(String, String, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, title, text, source, origin_path FROM transcripts WHERE ai_titled = 1",
        )?;
        let found = stmt.query_map([], |row| {
            Ok((
                row.get("id")?,
                row.get("title")?,
                row.get("text")?,
                row.get("source")?,
                row.get("origin_path")?,
            ))
        })?;
        found.collect::<rusqlite::Result<_>>()?
    };

    let mut reopened = 0;
    for (id, title, text, source, origin_path) in rows {
        if generated_title(&title, &text, &source, &origin_path) {
            conn.execute("UPDATE transcripts SET ai_titled = 0 WHERE id = ?1", [&id])?;
            reopened += 1;
        }
    }
    if reopened > 0 {
        eprintln!("[title] {reopened} note(s) still carry a name the app made up");
    }

    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    Ok(())
}

/// Whether this exact title is one the app produced rather than one a person
/// chose. Each arm reproduces a fallback the app is known to generate.
fn generated_title(title: &str, text: &str, source: &str, origin_path: &str) -> bool {
    // `meeting_title` — a date and a time, and nothing about the call.
    if title == "Meeting" || title.starts_with("Meeting — ") {
        return true;
    }
    // `dictation_title` — the first seven words, with an ellipsis if there were
    // more. Comparing against the transcript rather than re-deriving it because
    // the note may have been edited since, and the opening words are what make
    // it recognisably a fallback either way.
    if title == "Dictation" {
        return true;
    }
    let opening = title.trim_end_matches('…');
    if !opening.is_empty() && text.starts_with(opening) {
        return true;
    }
    // `title_from_path` — the filename, tidied up.
    if !origin_path.is_empty() && title == crate::title_from_path(origin_path) && source != "hotkey"
    {
        return true;
    }

    // The mic recorder. Its files are named `recording-<epoch>`, and the window
    // renders that as a date rather than showing a raw timestamp in the
    // sidebar. Checked as a shape rather than by re-deriving the string,
    // because the date is formatted in the user's locale in TypeScript and this
    // is Rust: the file we generated plus the heading we generate for it is
    // proof enough without trying to reproduce `toLocaleDateString` here.
    if title.starts_with("Recording ")
        && origin_path
            .rsplit('/')
            .next()
            .is_some_and(|file| file.starts_with("recording-"))
    {
        return true;
    }

    // A model that answered with something other than a name, back when the
    // caller would salvage a title out of whatever came back. Real examples from
    // a real library: `tool_call: {name: "retrieve_latest_transcription"}`,
    // `SYSTEM_VOICE_MODEL_TOOL_RESPONSE_RECEIVED`, `NOTE: "Taking Notes`.
    //
    // The test is still "can we show a machine wrote this", not "is this a bad
    // title": braces and quotes do not survive a person typing a name, and a
    // one-word title with an underscore in it is an identifier. Notes with
    // genuinely thin names — "Test", "Hello" — are left alone, because somebody
    // may well have meant them.
    title.contains(['{', '}', '"'])
        || (!title.contains(' ') && title.contains('_'))
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
            // Empty string for every note that has never been briefed, which is
            // most of them; only a successful generation writes JSON here.
            brief: row
                .get::<_, String>("brief")
                .ok()
                .filter(|s| !s.is_empty())
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
/// Write back a conversation whose speakers were renamed.
///
/// Separate from [`update_text`], which edits prose and has no business
/// touching the raw segments — this is the opposite: not a word changes, only
/// who is recorded as having said it.
pub fn set_conversation(
    conn: &Connection,
    id: &str,
    text: &str,
    paragraphs: &serde_json::Value,
    segments: &serde_json::Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcripts SET text = ?2, paragraphs = ?3, segments = ?4 WHERE id = ?1",
        params![id, text, paragraphs.to_string(), segments.to_string()],
    )?;
    Ok(())
}

/// Replace everything a transcription produced, for a note read a second time.
///
/// Wider than [`update_text`], which is a person editing prose, and than
/// [`set_conversation`], which only relabels who spoke. This is the whole
/// result: new words, new turns, new segments — and the overview cleared,
/// because an overview is a reading of a transcript that no longer exists and
/// leaving it would have the note summarising something it does not say. The
/// caller writes a fresh one immediately; if that fails, no overview is the
/// honest state.
///
/// Untouched on purpose: the title (renaming a note out from under someone who
/// named it would be worse than a stale one), when it was recorded, the audio,
/// and the waveform — the recording did not change, only what we heard in it.
pub fn replace_transcript(
    conn: &Connection,
    id: &str,
    text: &str,
    paragraphs: &serde_json::Value,
    segments: &serde_json::Value,
) -> rusqlite::Result<()> {
    let word_count = text.split_whitespace().count() as i64;
    conn.execute(
        "UPDATE transcripts
            SET text = ?2, paragraphs = ?3, segments = ?4, word_count = ?5,
                brief = '', brief_text = ''
          WHERE id = ?1",
        params![id, text, paragraphs.to_string(), segments.to_string(), word_count],
    )?;
    Ok(())
}

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

/// Record which speech model produced this note and how long it took.
///
/// Separate from `insert` for the same reason as `set_app_name`: it is the
/// engine's answer, not the caller's, and threading it through would mean four
/// ingest paths passing values they never look at.
pub fn set_engine_run(
    conn: &Connection,
    id: &str,
    model: &str,
    millis: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcripts SET model = ?2, transcribe_ms = ?3 WHERE id = ?1",
        params![id, model, millis],
    )?;
    Ok(())
}

/// Attach a structured overview to a note.
///
/// The brief is generated long after the transcript is saved — tens of seconds
/// of model work the save must never wait on — so this is always an update.
///
/// Ungated, unlike the column's other assistant-only neighbours: the public
/// build makes its overviews on the Mac itself, and the column it writes them
/// into has always been there for both.
pub fn set_brief(
    conn: &Connection,
    id: &str,
    brief: &serde_json::Value,
) -> rusqlite::Result<()> {
    // Written together, always. The JSON is what the Overview pane renders and
    // the prose is what search reads, and a note whose summary says "pricing"
    // being unfindable by searching for pricing is exactly the bug this column
    // exists to fix — so they must never drift apart.
    conn.execute(
        "UPDATE transcripts SET brief = ?2, brief_text = ?3 WHERE id = ?1",
        params![id, brief.to_string(), brief_prose(brief)],
    )?;
    Ok(())
}

/// An overview as the words in it, with the schema thrown away.
///
/// Deliberately not `brief::readable`: this module compiles on every platform
/// and that one lives behind a macOS gate. Same shape, and the test
/// `the_prose_column_holds_words_not_schema` pins what it must produce.
fn brief_prose(brief: &serde_json::Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(summary) = brief["summary"].as_str() {
        lines.push(summary.to_string());
    }

    // The heading is written out, in the words somebody would type, and *only*
    // when the list has something in it.
    //
    // This is the whole trick. Indexing the raw JSON put `action_items` into
    // every briefed note including the ones whose list was empty, which made it
    // a word with no discriminating power — so "What are my action items?"
    // matched everything, scored near zero, and fell under the relevance floor.
    // Emitted only where they exist, the same words match exactly the notes
    // that have them.
    //
    // Plural, because that is how the question is asked.
    for (list, heading) in [
        ("key_points", "Key points"),
        ("decisions", "Decisions"),
        ("action_items", "Action items"),
    ] {
        let items: Vec<String> = brief[list]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| match item {
                serde_json::Value::String(line) => Some(line.clone()),
                // An action item is an object — "who" and "what" — and the name
                // in it is exactly what somebody searches for.
                serde_json::Value::Object(fields) => {
                    let joined = fields
                        .values()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ");
                    (!joined.is_empty()).then_some(joined)
                }
                _ => None,
            })
            .collect();

        if !items.is_empty() {
            lines.push(heading.to_string());
            lines.extend(items);
        }
    }
    lines.join("\n")
}

/// Fill `brief_text` for every overview written before the column existed.
///
/// Gated on there being work to do rather than on a version counter, so it is a
/// single cheap query on every launch after the first.
fn index_the_prose(conn: &Connection) -> rusqlite::Result<()> {
    let owed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM transcripts WHERE brief != '' AND brief_text = ''",
        [],
        |row| row.get(0),
    )?;
    if owed == 0 {
        return Ok(());
    }

    let rows: Vec<(String, String)> = conn
        .prepare("SELECT id, brief FROM transcripts WHERE brief != '' AND brief_text = ''")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    for (id, brief) in &rows {
        let parsed: serde_json::Value =
            serde_json::from_str(brief).unwrap_or(serde_json::Value::Null);
        conn.execute(
            "UPDATE transcripts SET brief_text = ?2 WHERE id = ?1",
            params![id, brief_prose(&parsed)],
        )?;
    }
    // The UPDATE trigger has already re-indexed each row.
    eprintln!("[store] indexed the prose of {} overview(s)", rows.len());
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

/// Whether a note already carries a name somebody chose — the AI, or the user
/// typing over it. Errs towards `true`: a row we cannot read is one we should
/// not be renaming.
pub fn ai_titled(conn: &Connection, id: &str) -> bool {
    conn.query_row(
        "SELECT ai_titled FROM transcripts WHERE id = ?1",
        [id],
        |row| row.get::<_, i64>(0),
    )
    .map(|flag| flag != 0)
    .unwrap_or(true)
}

/// Notes that still want an AI title: never titled, have text, and — for
/// dictations, which are mostly short throwaways — long enough to be worth it.
/// Returns `(id, text)` newest first.
pub fn list_untitled(
    conn: &Connection,
    hotkey_min_words: i64,
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, text FROM transcripts
          WHERE ai_titled = 0
            AND length(trim(text)) > 0
            AND (source != 'hotkey' OR word_count >= ?1)
          ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([hotkey_min_words], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// Notes that should carry an overview and don't.
///
/// An overview is written once and kept, so an empty column means one of two
/// things: it was never asked for, or it was asked for and did not arrive.
/// Neither is distinguishable from the other here, and neither needs to be —
/// the answer to both is to try. Newest first, because that is the one somebody
/// is most likely to open.
pub fn list_unbriefed(
    conn: &Connection,
    sources: &[&str],
    min_words: i64,
) -> rusqlite::Result<Vec<String>> {
    let list = sources
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM transcripts
          WHERE brief = ''
            AND word_count >= ?1
            AND source IN ({list})
          ORDER BY created_at DESC"
    ))?;
    let rows = stmt.query_map([min_words], |row| row.get(0))?;
    rows.collect()
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM transcripts WHERE id = ?1", [id])?;
    Ok(())
}

/// The newest note, as (title, text).
///
/// Ordered by `created_at` rather than by insertion: a dropped file is dated
/// when it was recorded, so importing an old recording writes the last row in
/// the table and must not be what "the last transcript" hands back.
pub fn latest(conn: &Connection) -> rusqlite::Result<Option<(String, String)>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT title, text FROM transcripts ORDER BY created_at DESC LIMIT 1",
        [],
        |row| Ok((row.get("title")?, row.get("text")?)),
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(name: &str) -> Connection {
        let dir = std::env::temp_dir().join(format!("vd-store-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        open(&dir).unwrap()
    }

    fn note(conn: &Connection, id: &str, created_at: i64, text: &str) {
        let nothing = serde_json::Value::Null;
        insert(
            conn, id, id, "", 0.0, None, created_at, text, &nothing, &nothing, &nothing,
            "hotkey", "",
        )
        .unwrap();
    }

    /// Every case worth arguing about, and the answer the filter must give.
    ///
    /// The `true` rows are the point of this test. Every one of them was
    /// *dropped* by an earlier version tuned on a single library, and every one
    /// is a real note somebody would be furious to lose silently.
    const CHATTER_CASES: &[(&str, bool)] = &[
        // Too short to contain an answer, in any language.
        ("just a couple of words", false),
        // Literal repetition — somebody talking into the microphone to see
        // whether it is on. Detected structurally, so it works in any language.
        ("test test test test test test test test test test test", false),
        ("what's up what's up what's up what's up what's up what's up", false),
        // --- and now everything an English probe list used to destroy ---
        ("We need to test the new payment API before Friday or the release slips", true),
        ("Reminder: the QA test plan for the mobile app is due Monday and Anil owns it", true),
        ("Blood test results came back normal, follow up with the clinic in six months", true),
        ("The A/B test on the pricing page finished, variant B won by four percent", true),
        ("Should we move the launch to the following week given the Acme escalation?", true),
        ("Ask Priya whether the budget freeze applies to existing vendor contracts?", true),
        ("a perfectly ordinary note about the waveform height and how it should look", true),
    ];

    /// One rule written twice — once in SQL for the search route, once in Rust
    /// for the graph route — is a real cost, and this is what stops the two
    /// copies drifting apart quietly.
    ///
    /// They agree on length, which is all the SQL expresses. The repetition
    /// test is Rust-only and deliberately so: it is not worth a SQL window
    /// function, and erring toward keeping a note is the safe direction.
    #[test]
    fn the_two_chatter_filters_agree() {
        let conn = library("chatter");
        for (i, (text, _)) in CHATTER_CASES.iter().enumerate() {
            note(&conn, &format!("n{i}"), 1000 + i as i64, text);
        }

        let kept: Vec<String> = conn
            .prepare(&format!(
                "SELECT t.id FROM transcripts t WHERE {} ORDER BY t.id",
                WORTH_READING_SQL.replace("{}", &TOO_SLIGHT.to_string())
            ))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        for (i, (text, want)) in CHATTER_CASES.iter().enumerate() {
            let words = text.split_whitespace().count() as i64;
            assert_eq!(
                worth_reading(text, words),
                *want,
                "the filter disagrees with the table on {text:?}"
            );
            // The SQL half only knows about length, so it may keep something
            // Rust drops for repetition — never the reverse.
            if worth_reading(text, words) {
                assert!(
                    kept.contains(&format!("n{i}")),
                    "SQL dropped something Rust keeps: {text:?}"
                );
            }
        }
    }

    /// A library of meetings rather than of one person thinking out loud, which
    /// is what most people will point this at.
    ///
    /// Both of these returned *nothing at all* before `brief_text` existed: the
    /// index held the overview as raw JSON, so `action_items` and `decisions`
    /// appeared in every briefed note, scored as the common words they had
    /// become, and fell under the relevance floor. "What are my action items?"
    /// is the single most likely question anybody asks a meeting library.
    #[test]
    fn a_meeting_library_can_be_asked_about_its_action_items() {
        let conn = library("meetings");
        for i in 0..20 {
            note(&conn, &format!("f{i}"), 100 + i, "ordinary talk about panel spacing and margins");
        }

        note(
            &conn,
            "budget",
            2000,
            "Priya said the marketing spend is over and we agreed to freeze new vendor \
             contracts until October.",
        );
        set_brief(
            &conn,
            "budget",
            &serde_json::json!({
                "summary": "Marketing overspend reviewed and vendor contracts frozen.",
                "key_points": ["Marketing is over budget"],
                "decisions": ["Freeze new vendor contracts until October"],
                "action_items": [{ "who": "Tom", "what": "circulate the revised figures by Friday" }],
            }),
        )
        .unwrap();

        let by_action = search_any(&conn, "what are my action items", 6).unwrap();
        assert!(by_action.contains(&"budget".to_string()), "got {by_action:?}");

        // And the person named only inside an action item is findable.
        let by_person = search_any(&conn, "what did Tom commit to", 6).unwrap();
        assert!(by_person.contains(&"budget".to_string()), "got {by_person:?}");
    }

    /// The prose column must hold the words and not the JSON around them.
    #[test]
    fn the_prose_column_holds_words_not_schema() {
        let prose = brief_prose(&serde_json::json!({
            "summary": "Two roles approved.",
            "key_points": ["Backend hiring"],
            "decisions": ["Open two roles"],
            "action_items": [{ "who": "Priya", "what": "draft the job descriptions" }],
        }));
        assert!(prose.contains("Two roles approved."));
        assert!(prose.contains("Priya"), "a name in an action item is searchable");
        assert!(prose.contains("draft the job descriptions"));
        assert!(!prose.contains("action_items"), "the JSON key is not content");
        assert!(!prose.contains('{'), "{prose:?}");
        // The heading is there, in the words somebody types...
        assert!(prose.contains("Action items"), "{prose:?}");
        assert!(prose.contains("Decisions"), "{prose:?}");

        // ...and absent when the list is empty, which is what makes it worth
        // indexing at all. A word in every note discriminates nothing.
        let none = brief_prose(&serde_json::json!({
            "summary": "A chat with no outcomes.",
            "key_points": ["something"], "decisions": [], "action_items": [],
        }));
        assert!(!none.contains("Action items"), "{none:?}");
        assert!(!none.contains("Decisions"), "{none:?}");
    }

    /// The whole reason the English probe list was deleted. These are ordinary
    /// notes in somebody else's library and every one of them used to vanish.
    #[test]
    fn a_real_note_that_merely_mentions_testing_survives() {
        for text in CHATTER_CASES.iter().filter(|(_, keep)| *keep).map(|(t, _)| t) {
            assert!(
                worth_reading(text, text.split_whitespace().count() as i64),
                "{text:?} is a real note"
            );
        }
    }

    /// The floor is meaningless on a small library and must not fire there.
    /// bm25 scales a term by how rare it is across the corpus, so on a handful
    /// of notes every score sits near zero and a fixed threshold rejects the
    /// lot — leaving a new user with an empty answer to every question.
    #[test]
    fn a_small_library_is_not_filtered_into_silence() {
        let conn = library("floor-small");
        note(
            &conn,
            "only",
            1000,
            "The knowledge graph work is the interesting part of the transcription \
             pipeline and how the nodes get built from each overview",
        );
        assert!(
            !search_any(&conn, "what did I say about the knowledge graph pipeline", 6)
                .unwrap()
                .is_empty(),
            "a one-note library must still answer"
        );
    }

    /// bm25 saturates and `-1.5 * n_terms` does not, so without the cap a long
    /// question demands a score no document can reach and returns nothing at
    /// all — which is the worst possible answer to the most detailed question
    /// somebody asks.
    #[test]
    fn a_long_question_still_finds_something() {
        let conn = library("floor");
        for i in 0..20 {
            note(
                &conn,
                &format!("filler{i}"),
                500 + i,
                "unrelated filler about panel spacing and window layout and margins",
            );
        }
        note(
            &conn,
            "real",
            1000,
            "The knowledge graph work is the interesting part of the transcription \
             pipeline and I want to remember what we decided about how the nodes get \
             built from each overview rather than from the raw transcript itself",
        );

        let long = "Remind me everything I said about the knowledge graph and the \
                    transcription pipeline and how nodes are built from an overview";
        assert!(
            !search_any(&conn, long, 6).unwrap().is_empty(),
            "a thirteen-term question must not return nothing"
        );
    }

    /// And the floor still does its job on a question that merely brushes past
    /// a note on one common word.
    #[test]
    fn a_coincidental_word_match_is_below_the_floor() {
        let conn = library("floor-cuts");
        for i in 0..20 {
            note(&conn, &format!("filler{i}"), 500 + i, "panel spacing window layout margins");
        }
        note(
            &conn,
            "unrelated",
            1000,
            "The waveform height needs adjusting because it sits too tall in the \
             panel and crowds the transcript underneath it on a small window",
        );
        assert!(
            search_any(&conn, "action items", 6).unwrap().is_empty(),
            "schema words alone are not a subject"
        );
    }

    /// The tray copies whatever this returns, so "newest" has to mean newest by
    /// clock and not by insertion order. They come apart in practice: a dropped
    /// file is dated when it was recorded, so importing an old one writes a row
    /// that is last in and must not be what the menu hands back.
    #[test]
    fn the_latest_note_is_the_most_recent_one() {
        let conn = library("latest");
        note(&conn, "old", 1_000, "spoken first");
        note(&conn, "new", 9_000, "spoken last");
        note(&conn, "imported", 500, "recorded years ago, added just now");

        let (title, text) = latest(&conn).unwrap().expect("a library with notes in it");
        assert_eq!(title, "new");
        assert_eq!(text, "spoken last");
    }

    /// A fresh install. The menu item is always present, so this path runs the
    /// first time anyone opens the tray.
    #[test]
    fn an_empty_library_has_no_latest_note() {
        assert!(latest(&library("empty")).unwrap().is_none());
    }

    // -- reopening titles the app made up -----------------------------------

    #[test]
    fn the_names_the_app_generates_are_recognised() {
        let cases = [
            ("Meeting — 6 Aug, 1:41 PM", "", "meeting", ""),
            ("Meeting", "", "meeting", ""),
            ("Dictation", "", "hotkey", ""),
            // `dictation_title`, with and without the truncation mark.
            (
                "I think one thing that we…",
                "I think one thing that we can do is open the newest note",
                "hotkey",
                "",
            ),
            ("Short one", "Short one", "hotkey", ""),
            // `title_from_path`.
            (
                "2026 03 04 client call",
                "anything at all",
                "file",
                "/Users/x/2026-03-04_client call.m4a",
            ),
            // A model answering with something that was not a name.
            (
                r#"tool_call: {name: "retrieve_latest_transcription", arguments: {}}"#,
                "You: right, so",
                "hotkey",
                "",
            ),
            ("SYSTEM_VOICE_MODEL_TOOL_RESPONSE_RECEIVED", "x", "meeting", ""),
            // The mic recorder's own heading over its own file.
            (
                "Recording 30 Jul 7:34 PM",
                "Testing, one two",
                "mic",
                "/Users/x/recordings/recording-1785412891350.m4a",
            ),
            ("NOTE: \"Taking Notes", "x", "meeting", ""),
            ("tool_call", "x", "hotkey", ""),
        ];
        for (title, text, source, origin) in cases {
            assert!(
                generated_title(title, text, source, origin),
                "{title:?} is a name the app makes up"
            );
        }
    }

    #[test]
    fn a_name_somebody_typed_is_left_alone() {
        let cases = [
            ("Pricing review", "We should talk about pricing", "meeting", ""),
            ("Q3 planning", "Right, so the roadmap", "hotkey", ""),
            // Same note as above, renamed by hand: the filename no longer
            // matches, so nothing reopens it.
            (
                "Client call — pricing",
                "anything",
                "file",
                "/Users/x/2026-03-04_client call.m4a",
            ),
            // Thin, but somebody may well have meant them. "Can we prove a
            // machine wrote this" is the test, not "is this any good".
            ("Test", "Right, one two three", "hotkey", ""),
            ("Hello", "Right, so the thing is", "hotkey", ""),
            ("Notes on the tool_call refactor", "x", "hotkey", ""),
            // Renamed since; the file is still one of ours, the heading is not.
            (
                "Recording studio setup",
                "x",
                "mic",
                "/Users/x/audio/studio.m4a",
            ),
        ];
        for (title, text, source, origin) in cases {
            assert!(
                !generated_title(title, text, source, origin),
                "{title:?} is somebody's own name for a note"
            );
        }
    }

    /// The whole point of the correction: a library saved by a build with no AI
    /// in it has every row marked titled, and the ones still carrying a made-up
    /// name have to become available again exactly once.
    #[test]
    fn a_library_named_by_the_old_build_is_reopened_once() {
        let conn = library("reopen");
        let nothing = serde_json::Value::Null;
        for (id, title, text) in [
            ("a", "Meeting — 6 Aug, 1:41 PM", "You: hello there"),
            ("b", "Pricing review", "You: hello there"),
        ] {
            insert(
                &conn, id, title, "", 0.0, None, 1_000, text, &nothing, &nothing, &nothing,
                "meeting", "",
            )
            .unwrap();
            mark_titled(&conn, id).unwrap();
        }
        // `open` already ran the correction on an empty library and stamped the
        // version, so put it back to before it ran.
        conn.execute_batch("PRAGMA user_version = 0").unwrap();

        reopen_generated_titles(&conn).unwrap();
        assert!(!ai_titled(&conn, "a"), "the generated name should reopen");
        assert!(ai_titled(&conn, "b"), "a chosen name should not");

        // Named since, by the backfill this correction exists to feed. A second
        // run must not undo that.
        rename(&conn, "a", "Roadmap and hiring").unwrap();
        mark_titled(&conn, "a").unwrap();
        reopen_generated_titles(&conn).unwrap();
        assert!(ai_titled(&conn, "a"), "the correction ran twice");
    }
}
