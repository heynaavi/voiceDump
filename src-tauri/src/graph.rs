//! What the library is *about*, as something you can query.
//!
//! Full-text search answers "which note contains this word". That is not the
//! question people actually have. "What did we decide about pricing", "what has
//! Priya been working on", "when did the roadmap last come up" are all questions
//! about *things* — a topic, a person, a project — that appear across many notes
//! under many different sentences. FTS cannot answer them because the thing
//! being asked about is rarely spelled the same way twice, and often is not
//! spelled out at all in the note that matters most.
//!
//! So the model reads each note once and says what it was about. Those answers
//! become nodes, deduped by a normalised name so one project mentioned in forty
//! notes is one node with forty edges rather than forty rows. That shape is what
//! makes "chat with your data" possible without an embedding index: a question
//! names a node, the node names its notes, and the notes are what the model gets
//! handed to answer from.
//!
//! Design notes:
//! - Same database, same connection, same WAL as the transcripts. A graph that
//!   can disagree with the library about which notes exist is worse than none.
//! - Extraction is keyed off the *brief*, not the transcript. The brief has
//!   already been reduced to what mattered, always fits one pass, and is what a
//!   question would be answered from anyway. Running the graph off raw
//!   transcript text would cost a full map-reduce per note to arrive at a worse
//!   list, because an hour of conversation mentions a hundred things and is
//!   about four.
//! - Re-graphing a note is idempotent: its edges are cleared and rewritten, so a
//!   re-run after a better brief corrects the graph instead of doubling it.
//! - Nodes are never deleted when a note is. An orphan node is invisible to
//!   every query here (they all join through `graph_mentions`) and keeps its id
//!   stable for anything that cached it.

use rusqlite::{params, Connection};

/// The kinds of thing worth being a node, most specific first.
///
/// Deliberately four and not fourteen. This list is what the on-device model is
/// asked to fill, and a small model given a rich ontology does not produce a
/// richer graph — it produces the same entities filed inconsistently, which is
/// the one failure a graph cannot survive. Everything that is not a person, a
/// piece of work, or an organisation is a topic.
///
/// A node's identity is its *name*, not its (kind, name). Kind was part of the
/// key at first, on the reasoning that Sam the person and Sam the project are
/// two things — true, and the wrong thing to optimise for. The model's kind
/// assignment is not stable across notes: on a real library "live
/// transcription" came back a project in three notes and a topic in two, and
/// "light build" split two and two. So the graph grew two nodes for one
/// subject, each holding half the evidence, and both ranked below where the
/// subject belonged — which corrupts the one number the whole feature leans on.
/// Splitting is common and silently wrong; colliding is rare and visible. Kind
/// is now a label the node carries rather than part of who it is.
pub const KINDS: &[&str] = &["person", "org", "project", "topic"];

/// Where a kind sits in [`KINDS`]. Lower is more specific.
fn specificity(kind: &str) -> usize {
    KINDS.iter().position(|k| *k == kind).unwrap_or(KINDS.len())
}

/// One thing the model said a note was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub kind: String,
    pub name: String,
}

/// A node, with how much of the library backs it up.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Node {
    pub id: i64,
    pub kind: String,
    pub name: String,
    /// How many notes mention it. The whole reason to rank: a node seen once is
    /// usually the model having a thought, and a node seen thirty times is the
    /// thing this library is actually about.
    pub mentions: i64,
    pub last_seen: i64,
}

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS graph_nodes (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            kind     TEXT NOT NULL,
            -- As first written, and shown as written. "Q3 roadmap" reads better
            -- than "q3 roadmap" and neither is what we match on.
            name     TEXT NOT NULL,
            -- What dedup actually compares, and the node's whole identity.
            -- See `key` for what it normalises, and `KINDS` for why `kind` is
            -- not part of this.
            name_key TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS graph_mentions (
            node_id       INTEGER NOT NULL,
            transcript_id TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            PRIMARY KEY (node_id, transcript_id)
        );

        -- Both directions are hot: "what is this note about" draws the chips on
        -- one note, "which notes are about this" is the retrieval step.
        CREATE INDEX IF NOT EXISTS graph_mentions_by_note
            ON graph_mentions(transcript_id);
        CREATE INDEX IF NOT EXISTS graph_mentions_by_node
            ON graph_mentions(node_id);
        "#,
    )?;
    merge_nodes_split_by_kind(conn)
}

/// Pull back together the subjects an earlier `UNIQUE(kind, name_key)` split.
///
/// Gated on the index that enforces the rule, not on a version counter: a
/// database whose `name_key` is already unique cannot be holding a split, so
/// this is a no-op on a graph built by this code and runs exactly once on one
/// built by the old code.
///
/// Mentions are repointed rather than recounted. The surviving node keeps the
/// most specific kind any of the duplicates carried, and its earliest id, so
/// anything holding a node id still resolves.
fn merge_nodes_split_by_kind(conn: &Connection) -> rusqlite::Result<()> {
    let already: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_index_list('graph_nodes') l
           JOIN pragma_index_info(l.name) i
          WHERE l.\"unique\" = 1 AND i.name = 'name_key'
          AND (SELECT COUNT(*) FROM pragma_index_info(l.name)) = 1",
        [],
        |row| row.get(0),
    )?;
    if already > 0 {
        return Ok(());
    }

    // Rebuilt rather than altered: SQLite cannot drop a UNIQUE constraint that
    // was declared inline on the table.
    conn.execute_batch(
        r#"
        BEGIN;

        CREATE TABLE graph_nodes_merged (
            id       INTEGER PRIMARY KEY,
            kind     TEXT NOT NULL,
            name     TEXT NOT NULL,
            name_key TEXT NOT NULL UNIQUE
        );

        -- One winner per name: the lowest id among those carrying the most
        -- specific kind, so the display spelling stays the one first written.
        INSERT INTO graph_nodes_merged (id, kind, name, name_key)
        SELECT n.id, n.kind, n.name, n.name_key
          FROM graph_nodes n
          JOIN (
            SELECT name_key,
                   MIN(CASE kind WHEN 'person' THEN 0 WHEN 'org' THEN 1
                                 WHEN 'project' THEN 2 ELSE 3 END) AS rank
              FROM graph_nodes GROUP BY name_key
          ) best
            ON best.name_key = n.name_key
           AND best.rank = (CASE n.kind WHEN 'person' THEN 0 WHEN 'org' THEN 1
                                        WHEN 'project' THEN 2 ELSE 3 END)
         WHERE n.id = (
            SELECT MIN(m.id) FROM graph_nodes m
             WHERE m.name_key = n.name_key AND m.kind = n.kind
         );

        -- Every mention follows its subject to the surviving node. OR IGNORE
        -- because a note that mentioned both halves of a split now mentions the
        -- one node once, which is the whole point.
        INSERT OR IGNORE INTO graph_mentions (node_id, transcript_id, created_at)
        SELECT w.id, m.transcript_id, m.created_at
          FROM graph_mentions m
          JOIN graph_nodes old ON old.id = m.node_id
          JOIN graph_nodes_merged w ON w.name_key = old.name_key;

        DELETE FROM graph_mentions
         WHERE node_id NOT IN (SELECT id FROM graph_nodes_merged);

        DROP TABLE graph_nodes;
        ALTER TABLE graph_nodes_merged RENAME TO graph_nodes;

        COMMIT;
        "#,
    )?;
    eprintln!("[graph] merged subjects that had split across kinds");
    Ok(())
}

/// What two spellings of the same thing have in common.
///
/// Case, surrounding punctuation, inner whitespace and a leading article all
/// vary run to run for what is plainly one thing — "The Q3 Roadmap", "q3
/// roadmap", "Q3  roadmap." Trailing plurals are left alone on purpose: "the
/// pricing model" and "pricing models" are close enough to be one node and
/// "spec" and "specs" are not reliably the same, and a graph that merges two
/// distinct things is harder to notice and harder to undo than one that splits
/// a thing in two.
pub fn key(name: &str) -> String {
    let lowered = name.to_lowercase();
    let trimmed = lowered
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .trim();
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .strip_prefix("the ")
        .unwrap_or(&collapsed)
        .to_string()
}

/// Replace everything a note is on record as being about.
///
/// Cleared first so this is a statement of fact rather than an addition: a note
/// re-graphed after a better brief ends up with the entities from the better
/// brief, and nothing left over from the worse one.
pub fn set_mentions(
    conn: &Connection,
    transcript_id: &str,
    created_at: i64,
    entities: &[Entity],
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM graph_mentions WHERE transcript_id = ?1",
        params![transcript_id],
    )?;

    let mut written = 0;
    for entity in entities {
        let name_key = key(&entity.name);
        if name_key.is_empty() || !KINDS.contains(&entity.kind.as_str()) {
            continue;
        }

        // Insert-then-select rather than an upsert returning: the node very
        // often already exists, and this keeps the existing display spelling —
        // the first note to mention something names it for good, instead of the
        // most recent one silently renaming it under everyone.
        conn.execute(
            "INSERT OR IGNORE INTO graph_nodes (kind, name, name_key) VALUES (?1, ?2, ?3)",
            params![entity.kind, entity.name.trim(), name_key],
        )?;
        // Looked up by name alone, never by (kind, name). See `KINDS`.
        let (node_id, had): (i64, String) = conn.query_row(
            "SELECT id, kind FROM graph_nodes WHERE name_key = ?1",
            params![name_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        // A later note may know better what this is. "Rupesh" first seen as a
        // topic and later as a person is a person; the reverse is the model
        // being vague, not new information.
        if specificity(&entity.kind) < specificity(&had) {
            conn.execute(
                "UPDATE graph_nodes SET kind = ?2 WHERE id = ?1",
                params![node_id, entity.kind],
            )?;
        }

        conn.execute(
            "INSERT OR IGNORE INTO graph_mentions (node_id, transcript_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![node_id, transcript_id, created_at],
        )?;
        written += 1;
    }
    Ok(written)
}

/// What one note is about.
pub fn mentions_of(conn: &Connection, transcript_id: &str) -> rusqlite::Result<Vec<Node>> {
    let mut stmt = conn.prepare(
        "SELECT n.id, n.kind, n.name,
                (SELECT COUNT(*) FROM graph_mentions WHERE node_id = n.id),
                m.created_at
           FROM graph_nodes n
           JOIN graph_mentions m ON m.node_id = n.id
          WHERE m.transcript_id = ?1
          ORDER BY 4 DESC, n.name",
    )?;
    let rows = stmt.query_map(params![transcript_id], |row| {
        Ok(Node {
            id: row.get(0)?,
            kind: row.get(1)?,
            name: row.get(2)?,
            mentions: row.get(3)?,
            last_seen: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// The library, ranked by what it keeps coming back to.
pub fn top_nodes(conn: &Connection, kind: Option<&str>, limit: usize) -> rusqlite::Result<Vec<Node>> {
    let mut stmt = conn.prepare(
        "SELECT n.id, n.kind, n.name, COUNT(m.transcript_id), MAX(m.created_at)
           FROM graph_nodes n
           JOIN graph_mentions m ON m.node_id = n.id
          WHERE (?1 IS NULL OR n.kind = ?1)
          GROUP BY n.id
          ORDER BY COUNT(m.transcript_id) DESC, MAX(m.created_at) DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![kind, limit as i64], |row| {
        Ok(Node {
            id: row.get(0)?,
            kind: row.get(1)?,
            name: row.get(2)?,
            mentions: row.get(3)?,
            last_seen: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// The notes behind a node, newest first.
///
/// This is the retrieval step "chat with your data" is built on: a question
/// names a node, and this says which notes have to be read to answer it.
pub fn notes_about(conn: &Connection, node_id: i64, limit: usize) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT transcript_id FROM graph_mentions
          WHERE node_id = ?1
          ORDER BY created_at DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![node_id, limit as i64], |row| row.get(0))?;
    rows.collect()
}

/// Find nodes by name, for a question that arrived as words rather than an id.
pub fn lookup(conn: &Connection, term: &str, limit: usize) -> rusqlite::Result<Vec<Node>> {
    let needle = key(term);
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    // Word-start rather than anywhere-inside. Matching anywhere made `action`
    // find 'data interaction', `graph` find 'paragraph highlighting' and `test`
    // find 'latest recordings' — a needle landing in the middle of a longer
    // word is a coincidence, not a subject.
    //
    // The hyphen clause is not decoration. Without it, a plain word-start rule
    // loses `based` from 'hover-based overlay', `powered` from 'AI-powered
    // chat' and `source` from 'open-source development' — 24 needles in all on
    // a real graph, because compound names are what a graph fills up with.
    //
    // And no minimum needle length: a four-character floor would discard live
    // nodes called Mac, PDF, app and HUD.
    let mut stmt = conn.prepare(
        "SELECT n.id, n.kind, n.name, COUNT(m.transcript_id), MAX(m.created_at)
           FROM graph_nodes n
           JOIN graph_mentions m ON m.node_id = n.id
          WHERE n.name_key = ?1
             OR n.name_key LIKE ?1 || '%'
             OR n.name_key LIKE '% ' || ?1 || '%'
             OR n.name_key LIKE '%-' || ?1 || '%'
          GROUP BY n.id
          -- Exact before partial, then by how much of the library backs it.
          ORDER BY (n.name_key = ?1) DESC, COUNT(m.transcript_id) DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![needle, limit as i64], |row| {
        Ok(Node {
            id: row.get(0)?,
            kind: row.get(1)?,
            name: row.get(2)?,
            mentions: row.get(3)?,
            last_seen: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// Notes with a brief worth reading and no entities on record.
///
/// The brief is the input, so a note without one is not behind — it is not
/// eligible yet, and will be picked up the launch after its overview lands.
pub fn list_ungraphed(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.id FROM transcripts t
          WHERE t.brief != ''
            AND NOT EXISTS (SELECT 1 FROM graph_mentions m WHERE m.transcript_id = t.id)
          ORDER BY t.created_at DESC
          LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| row.get(0))?;
    rows.collect()
}

// -- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE transcripts (id TEXT PRIMARY KEY, brief TEXT NOT NULL DEFAULT '',
                                       created_at INTEGER NOT NULL DEFAULT 0);",
        )
        .unwrap();
        init(&conn).unwrap();
        conn
    }

    fn e(kind: &str, name: &str) -> Entity {
        Entity { kind: kind.into(), name: name.into() }
    }

    /// The whole point of the table. If these split, a library of forty notes
    /// about one project looks like forty projects.
    #[test]
    fn one_thing_spelled_four_ways_is_one_node() {
        let conn = db();
        for (note, spelling) in [
            ("a", "Q3 roadmap"),
            ("b", "the Q3 Roadmap"),
            ("c", "  q3   roadmap  "),
            ("d", "Q3 roadmap."),
        ] {
            set_mentions(&conn, note, 0, &[e("project", spelling)]).unwrap();
        }

        let nodes = top_nodes(&conn, None, 10).unwrap();
        assert_eq!(nodes.len(), 1, "got {nodes:?}");
        assert_eq!(nodes[0].mentions, 4);
        assert_eq!(nodes[0].name, "Q3 roadmap", "the first spelling names it");
    }

    #[test]
    fn different_names_stay_apart() {
        let conn = db();
        set_mentions(&conn, "a", 0, &[e("project", "Sam")]).unwrap();
        set_mentions(&conn, "b", 0, &[e("project", "Samsung")]).unwrap();
        assert_eq!(top_nodes(&conn, None, 10).unwrap().len(), 2);
    }

    /// The failure this cost a real library. "live transcription" came back a
    /// project in three notes and a topic in two, and the graph held two nodes
    /// with three and two mentions instead of one with five — so the subject
    /// ranked below things mentioned less.
    #[test]
    fn one_subject_the_model_kept_reclassifying_is_one_node() {
        let conn = db();
        for note in ["a", "b", "c"] {
            set_mentions(&conn, note, 0, &[e("project", "live transcription")]).unwrap();
        }
        for note in ["d", "e"] {
            set_mentions(&conn, note, 0, &[e("topic", "Live Transcription")]).unwrap();
        }

        let nodes = top_nodes(&conn, None, 10).unwrap();
        assert_eq!(nodes.len(), 1, "got {nodes:?}");
        assert_eq!(nodes[0].mentions, 5, "all five notes back the same subject");
    }

    /// A later note may know better what something is; a vaguer later note does
    /// not un-know it.
    #[test]
    fn a_kind_is_upgraded_towards_the_specific_and_never_back() {
        let conn = db();
        set_mentions(&conn, "a", 0, &[e("topic", "Rupesh")]).unwrap();
        set_mentions(&conn, "b", 0, &[e("person", "Rupesh")]).unwrap();
        assert_eq!(top_nodes(&conn, None, 10).unwrap()[0].kind, "person");

        set_mentions(&conn, "c", 0, &[e("topic", "Rupesh")]).unwrap();
        assert_eq!(
            top_nodes(&conn, None, 10).unwrap()[0].kind,
            "person",
            "a vaguer mention is the model being unsure, not new information"
        );
    }

    /// Databases built before the name became the identity carry the split.
    #[test]
    fn a_graph_split_by_kind_is_merged_on_open() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE transcripts (id TEXT PRIMARY KEY, brief TEXT NOT NULL DEFAULT '',
                                       created_at INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE graph_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL,
                name TEXT NOT NULL, name_key TEXT NOT NULL, UNIQUE(kind, name_key));
             CREATE TABLE graph_mentions (
                node_id INTEGER NOT NULL, transcript_id TEXT NOT NULL,
                created_at INTEGER NOT NULL, PRIMARY KEY (node_id, transcript_id));
             INSERT INTO graph_nodes (id, kind, name, name_key) VALUES
                (1, 'project', 'live transcription', 'live transcription'),
                (2, 'topic',   'Live Transcription', 'live transcription'),
                (3, 'topic',   'waveform',           'waveform');
             INSERT INTO graph_mentions (node_id, transcript_id, created_at) VALUES
                (1,'a',0),(1,'b',0),(1,'c',0),(2,'d',0),(2,'e',0),(3,'a',0);",
        )
        .unwrap();

        init(&conn).unwrap();

        let nodes = top_nodes(&conn, None, 10).unwrap();
        assert_eq!(nodes.len(), 2, "got {nodes:?}");
        let merged = nodes.iter().find(|n| n.name_key_is("live transcription")).unwrap();
        assert_eq!(merged.mentions, 5, "both halves' notes, deduped");
        assert_eq!(merged.kind, "project", "the more specific of the two");
        assert_eq!(merged.name, "live transcription", "the first spelling");

        // Idempotent: a second open must not rebuild or double anything.
        init(&conn).unwrap();
        assert_eq!(top_nodes(&conn, None, 10).unwrap().len(), 2);
    }

    impl Node {
        fn name_key_is(&self, key_: &str) -> bool {
            key(&self.name) == key_
        }
    }

    /// A note re-graphed after a better brief must end up describing the better
    /// brief, with nothing left over from the worse one.
    #[test]
    fn regraphing_a_note_replaces_what_it_said_before() {
        let conn = db();
        set_mentions(&conn, "a", 0, &[e("topic", "pricing"), e("topic", "hiring")]).unwrap();
        set_mentions(&conn, "a", 0, &[e("topic", "pricing")]).unwrap();

        let about = mentions_of(&conn, "a").unwrap();
        assert_eq!(about.len(), 1);
        assert_eq!(about[0].name, "pricing");

        // The hiring node survives with nothing behind it, and every query here
        // joins through mentions, so it is invisible rather than wrong.
        assert!(top_nodes(&conn, None, 10).unwrap().iter().all(|n| n.name != "hiring"));
    }

    #[test]
    fn a_node_knows_which_notes_to_read() {
        let conn = db();
        set_mentions(&conn, "old", 100, &[e("topic", "pricing")]).unwrap();
        set_mentions(&conn, "new", 200, &[e("topic", "pricing")]).unwrap();

        let found = lookup(&conn, "Pricing", 5).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].mentions, 2);
        assert_eq!(
            notes_about(&conn, found[0].id, 5).unwrap(),
            vec!["new".to_string(), "old".to_string()],
            "newest first — a question about pricing wants this week's answer"
        );
    }

    /// Junk from the model must not become a node.
    #[test]
    fn a_kind_nobody_asked_for_is_dropped() {
        let conn = db();
        let written = set_mentions(
            &conn,
            "a",
            0,
            &[e("vibe", "good"), e("topic", "  "), e("topic", "!!!"), e("topic", "real")],
        )
        .unwrap();
        assert_eq!(written, 1);
        assert_eq!(mentions_of(&conn, "a").unwrap()[0].name, "real");
    }

    /// A note is eligible when it has a brief to read, and stops being eligible
    /// once it has been read.
    #[test]
    fn the_sweep_picks_up_briefed_notes_that_have_no_entities() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO transcripts (id, brief, created_at) VALUES
                ('briefed', '{\"summary\":\"x\"}', 2),
                ('bare', '', 1);",
        )
        .unwrap();

        assert_eq!(list_ungraphed(&conn, 10).unwrap(), vec!["briefed".to_string()]);
        set_mentions(&conn, "briefed", 2, &[e("topic", "pricing")]).unwrap();
        assert!(list_ungraphed(&conn, 10).unwrap().is_empty());
    }
}
