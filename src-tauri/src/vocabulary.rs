//! Words VoiceDumps has been taught to spell.
//!
//! A speech model cannot know that the person being talked about is spelled
//! Shaun and not Sean, or that "hyper frames" is a product called HyperFrames.
//! This module is where those spellings come from, and where they are applied —
//! after decoding, to the words that match, and nowhere else.
//!
//! **Not as a prompt.** Whisper is usually taught names by decoding with them in
//! front of the audio, and that was built and measured first: over 46 real
//! recordings with six real product names, it fixed one and changed 49 content
//! words to do it, deleting whole words from sentences nobody had asked it to
//! touch. A spelling applied after the fact can only change what it matches.
//!
//! **Three ways in.** Typed into Settings; fixed in a note inside VoiceDumps;
//! fixed in the app the dictation was pasted into, which is the one that matters,
//! because that is where people actually correct things — see
//! [`crate::readback`]. Across 1,355 notes in the two libraries this was built
//! against, twelve words had ever been corrected inside the app.
//!
//! **Two strengths of lesson.** Every accepted correction adds its spelling to
//! the vocabulary, which is written only where the words already spell it —
//! "voice dumps" as VoiceDumps, "claude" as Claude. A correction that looks like a
//! mishearing — the same word spelled differently, not a different word — and
//! has been made more than once also becomes a rule: after transcription, what
//! was heard is rewritten as what was meant. Suggesting a name costs nothing when
//! it is wrong. Rewriting a word does, so it has to be earned.
//!
//! Only the two words are kept. Nothing else from the text they came out of is
//! stored anywhere, and none of it leaves the Mac.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// How many times a mishearing must be corrected before it is rewritten
/// automatically.
///
/// Two, not one. A single correction is as likely to be a typo fixed, or a
/// change of mind that happens to look similar, as a word the model always gets
/// wrong. A second identical fix is the pattern the complaint is about — "you fix
/// a misspelled word three times and it makes the same mistake a fourth" — and
/// this makes the second time the last.
pub const REPLACE_AFTER: i64 = 2;

/// The longest thing that is still a word rather than a phrase or a sentence.
const MAX_TERM_CHARS: usize = 40;
const MAX_TERM_WORDS: usize = 4;

/// The widest edit that is still a correction rather than a rewrite.
const MAX_HUNK_WORDS: usize = 3;

/// The most alignment work one edit is allowed, after shared text is set aside:
/// a two-thousand-word stretch rewritten against another. Past that it is not a
/// correction anyone made one word at a time.
const MAX_ALIGN_CELLS: usize = 4_000_000;

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS vocabulary (
            term       TEXT PRIMARY KEY COLLATE NOCASE,
            source     TEXT NOT NULL,
            app        TEXT,
            count      INTEGER NOT NULL DEFAULT 1,
            last_seen  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS corrections (
            heard      TEXT NOT NULL COLLATE NOCASE,
            term       TEXT NOT NULL COLLATE NOCASE,
            similar    INTEGER NOT NULL DEFAULT 0,
            count      INTEGER NOT NULL DEFAULT 1,
            last_seen  INTEGER NOT NULL,
            PRIMARY KEY (heard, term)
        );
        "#,
    )?;
    // How sure the model was about the speech it misheard. Databases written
    // before this was asked have no such number, and -1 means exactly that —
    // shown as nothing rather than as no confidence at all, which is a very
    // different claim. The duplicate-column error on a re-run is the success
    // case, as everywhere else this pattern is used.
    conn.execute(
        "ALTER TABLE corrections ADD COLUMN confidence REAL NOT NULL DEFAULT -1",
        [],
    )
    .ok();
    Ok(())
}

/// Where a word was learned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Typed into Settings.
    Added,
    /// Fixed in a note inside VoiceDumps.
    Edited,
    /// Fixed in the app the words were pasted into.
    Corrected,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Added => "added",
            Source::Edited => "edited",
            Source::Corrected => "corrected",
        }
    }
}

/// One row of Settings' vocabulary list.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Term {
    pub term: String,
    /// What was written instead, most often, when it was learned from a fix.
    pub heard: Option<String>,
    pub source: String,
    pub app: Option<String>,
    pub count: i64,
    /// Whether `heard` is now rewritten as `term` after every transcription.
    pub replaces: bool,
    /// How alike the two spellings are, 0…1. One is the same letters written
    /// differently; near zero is a word swapped for an unrelated one.
    pub likeness: f64,
    /// Whether the two are the same sounds — how a name is recognised when it
    /// is spelt a way nobody could have listed. See [`resemble`].
    pub sounds_alike: bool,
    /// How sure the model was about the speech it misheard, 0…1, when that was
    /// recorded. `None` for a word typed in here, for a fix made to a note
    /// rather than to a fresh dictation, and for everything learned before this
    /// was asked.
    pub confidence: Option<f32>,
    /// Whether this word is written into transcriptions as they stand today —
    /// as a spelling, as a rewrite, or by sound. Asked rather than guessed,
    /// because the three have different bars and a row that says it is working
    /// when it is not is worse than no row.
    pub applied: bool,
    /// Whether it is also recognised when the model spells it a new way.
    pub by_sound: bool,
}

/// One fix, read out of an edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    pub heard: String,
    pub term: String,
    /// Spelled like what was heard — the kind of fix that may become a rule.
    pub similar: bool,
}

// -- reading fixes out of an edit -------------------------------------------

/// A word's letters and digits, lowercased: what two spellings are compared on.
fn letters(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A word without the punctuation a sentence puts around it.
fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let here = row[j];
            row[j] = if a[i - 1] == b[j - 1] {
                prev
            } else {
                1 + prev.min(row[j]).min(row[j - 1])
            };
            prev = here;
        }
    }
    row[b.len()]
}

/// How alike two spellings are, 0…1.
fn likeness(a: &str, b: &str) -> f64 {
    let (a, b) = (letters(a), letters(b));
    let longest = a.chars().count().max(b.chars().count());
    if longest == 0 {
        return 0.0;
    }
    1.0 - levenshtein(&a, &b) as f64 / longest as f64
}

/// Whether a word is written the way names and products are, not common words.
///
/// A capital after the first letter (iPhone, HyperFrames), a digit, or a mark
/// inside the word (Next.js, C++) — or, away from the start of a sentence, a
/// leading capital at all.
fn looks_like_a_name(term: &str, starts_sentence: bool) -> bool {
    let mut chars = term.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let rest: String = chars.collect();
    rest.chars().any(char::is_uppercase)
        || term.chars().any(|c| c.is_ascii_digit())
        || rest.chars().any(|c| !c.is_alphanumeric() && !c.is_whitespace() && c != '\'' && c != '-')
        || (first.is_uppercase() && !starts_sentence)
}

/// Decide whether one changed stretch of words is a correction worth learning.
fn judge(heard: &[&str], term: &[&str], starts_sentence: bool) -> Option<Fix> {
    if heard.is_empty() || term.is_empty() {
        return None;
    }
    if heard.len() > MAX_HUNK_WORDS || term.len() > MAX_HUNK_WORDS {
        return None;
    }
    let heard_text = heard.iter().map(|w| bare(w)).collect::<Vec<_>>().join(" ");
    let term_text = term.iter().map(|w| bare(w)).collect::<Vec<_>>().join(" ");
    if heard_text.is_empty() || term_text.is_empty() || heard_text == term_text {
        return None;
    }

    let same_letters = letters(&heard_text) == letters(&term_text);
    let named = looks_like_a_name(&term_text, starts_sentence);

    // Only the case or the spacing changed. That is a correction when it made a
    // name or a product ("graph if i" → graphify, iphone → iPhone), and noise
    // when it is a sentence being tidied ("so" → "So").
    if same_letters {
        let joined = heard.len() != term.len();
        return (named || joined).then(|| Fix {
            heard: heard_text,
            term: term_text,
            similar: true,
        });
    }

    // Spelled alike: a mishearing. "Thursday" → "Friday" sits at exactly one
    // half and is a change of mind, which is why the bar is strictly above it.
    if likeness(&heard_text, &term_text) > 0.5 {
        return Some(Fix {
            heard: heard_text,
            term: term_text,
            similar: true,
        });
    }

    // Not spelled alike, but the fix is a name the model did not hear as one —
    // "win" → Nguyen, written in lowercase. Worth suggesting to the model; never
    // worth rewriting, because the same sound being a real "win" in the next
    // sentence is entirely possible. When what it wrote was already capitalised
    // it *did* hear a name, and swapping it for an unlike one ("Thursday" →
    // "Friday") is a change of mind about which, not a mishearing.
    let heard_named = heard_text.chars().next().is_some_and(char::is_uppercase);
    (named && !heard_named).then(|| Fix {
        heard: heard_text,
        term: term_text,
        similar: false,
    })
}

/// The fixes between what was written and what it was changed to.
///
/// Word-level, aligned on what did not change, so a sentence edited in two
/// places yields two fixes and a word merely moved yields none.
pub fn fixes_between(before: &str, after: &str) -> Vec<Fix> {
    let a_all: Vec<&str> = before.split_whitespace().collect();
    let b_all: Vec<&str> = after.split_whitespace().collect();

    // Edits are local; notes are not. Everything the two texts share at the
    // start and the end is set aside before aligning, so correcting one name in
    // a six-thousand-word meeting aligns a few words rather than filling a
    // six-thousand-squared table — about 144 MB for that meeting.
    let head = a_all.iter().zip(&b_all).take_while(|(x, y)| x == y).count();
    let tail = a_all[head..]
        .iter()
        .rev()
        .zip(b_all[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a = &a_all[head..a_all.len() - tail];
    let b = &b_all[head..b_all.len() - tail];
    if a.len().saturating_mul(b.len()) > MAX_ALIGN_CELLS {
        return Vec::new();
    }
    // Whether the first changed word opens a sentence is a question about the
    // words before it, which may have been set aside with the shared start.
    let before_b = |k: usize| if k == 0 { head.checked_sub(1).map(|h| b_all[h]) } else { Some(b[k - 1]) };

    // Longest common subsequence on letters, so punctuation and case changes
    // alone are not treated as matches — they are what the hunks are made of.
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut hunk_a, mut hunk_b) = (i, j);
    let flush = |ha: usize, i: usize, hb: usize, j: usize, out: &mut Vec<Fix>| {
        if ha == i && hb == j {
            return;
        }
        let starts_sentence = before_b(hb)
            .map_or(true, |w| w.chars().last().is_some_and(|c| matches!(c, '.' | '!' | '?')));
        if let Some(fix) = judge(&a[ha..i], &b[hb..j], starts_sentence) {
            out.push(fix);
        }
    };
    while i < n && j < m {
        if a[i] == b[j] {
            flush(hunk_a, i, hunk_b, j, &mut out);
            i += 1;
            j += 1;
            hunk_a = i;
            hunk_b = j;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    flush(hunk_a, n, hunk_b, m, &mut out);
    out
}

// -- applying what was learned ----------------------------------------------

/// Rewrite every whole-word occurrence of each rule's `heard` as its `term`.
///
/// Case-insensitive on the way in, and the term is written exactly as it was
/// taught on the way out, because the spelling is the whole point. Punctuation
/// around a word is left where it was.
pub fn apply(text: &str, rules: &[(String, String)]) -> String {
    if rules.is_empty() {
        return text.to_string();
    }
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    'next: while i < words.len() {
        for (heard, term) in rules {
            let want: Vec<String> = heard.split_whitespace().map(letters).collect();
            if want.is_empty() || i + want.len() > words.len() {
                continue;
            }
            let got: Vec<String> = words[i..i + want.len()].iter().map(|w| letters(w)).collect();
            if got == want {
                let first = words[i];
                let last = words[i + want.len() - 1];
                let lead = &first[..first.len() - first.trim_start_matches(|c: char| !c.is_alphanumeric()).len()];
                let trail_start = last.trim_end_matches(|c: char| !c.is_alphanumeric()).len();
                out.push(format!("{lead}{term}{}", &last[trail_start..]));
                i += want.len();
                continue 'next;
            }
        }
        out.push(words[i].to_string());
        i += 1;
    }
    out.join(" ")
}

// -- the store ----------------------------------------------------------------

fn check(term: &str) -> Result<String, String> {
    let term = term.trim();
    if term.is_empty() {
        return Err("A word needs some letters in it.".into());
    }
    if term.contains(['\n', '\r']) {
        return Err("One word or name at a time.".into());
    }
    if term.chars().count() > MAX_TERM_CHARS || term.split_whitespace().count() > MAX_TERM_WORDS {
        return Err("That is longer than a name or a word — try the part that gets misspelled.".into());
    }
    Ok(term.to_string())
}

/// Record a word someone typed into Settings.
pub fn add(conn: &Connection, term: &str, now: i64) -> Result<(), String> {
    let term = check(term)?;
    conn.execute(
        "INSERT INTO vocabulary (term, source, app, count, last_seen) VALUES (?1, ?2, NULL, 1, ?3)
         ON CONFLICT(term) DO UPDATE SET last_seen = excluded.last_seen, source = excluded.source",
        params![term, Source::Added.as_str(), now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn remove(conn: &Connection, term: &str) -> Result<(), String> {
    conn.execute("DELETE FROM vocabulary WHERE term = ?1", params![term.trim()])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM corrections WHERE term = ?1", params![term.trim()])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// How sure the model was about the stretch of speech a fix corrects.
///
/// Confidence is kept per sentence, not per word — see `engine::collect` — so
/// this is the confidence of the sentence the misheard words sat in, which is
/// the most that can honestly be said about them. `None` when no sentence
/// contains them, which is every correction made to a note rather than to a
/// dictation that has just been pasted.
fn confidence_for(heard: &str, sure: &[(String, f32)]) -> Option<f32> {
    let want = letters(heard);
    if want.is_empty() {
        return None;
    }
    sure.iter()
        .find(|(text, _)| letters(text).contains(&want))
        .map(|(_, c)| *c)
}

/// Record fixes someone made.
pub fn learn(conn: &Connection, fixes: &[Fix], source: Source, app: Option<&str>, now: i64) {
    learn_sure(conn, fixes, source, app, now, &[]);
}

/// Record fixes someone made, alongside what the model made of the speech.
///
/// `sure` is the sentences that were just transcribed and how sure the model
/// was of each. It is empty wherever that is unknown, and an unknown confidence
/// is stored as one, never as zero.
pub fn learn_sure(
    conn: &Connection,
    fixes: &[Fix],
    source: Source,
    app: Option<&str>,
    now: i64,
    sure: &[(String, f32)],
) {
    for fix in fixes {
        if check(&fix.term).is_err() {
            continue;
        }
        let _ = conn.execute(
            "INSERT INTO vocabulary (term, source, app, count, last_seen) VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(term) DO UPDATE SET count = count + 1, last_seen = excluded.last_seen,
                 app = COALESCE(excluded.app, vocabulary.app)",
            params![fix.term, source.as_str(), app, now],
        );
        // A later correction that knows the confidence fills in for an earlier
        // one that did not; a later one that does not know it leaves the number
        // already there alone.
        let heard_sure = confidence_for(&fix.heard, sure).unwrap_or(-1.0);
        let _ = conn.execute(
            "INSERT INTO corrections (heard, term, similar, count, last_seen, confidence)
             VALUES (?1, ?2, ?3, 1, ?4, ?5)
             ON CONFLICT(heard, term) DO UPDATE SET count = count + 1, last_seen = excluded.last_seen,
                 confidence = CASE WHEN excluded.confidence >= 0
                                   THEN excluded.confidence ELSE corrections.confidence END",
            params![fix.heard, fix.term, fix.similar as i64, now, heard_sure],
        );
    }
}

/// The rewrites that have been earned.
///
/// A mishearing corrected [`REPLACE_AFTER`] times, spelled alike, and not
/// contradicted: a heard word corrected to two different terms is ambiguous and
/// rewrites nothing, and a heard word that is itself in the vocabulary is a word
/// the person really uses and is never rewritten away.
pub fn rules(conn: &Connection) -> Vec<(String, String)> {
    let mut stmt = match conn.prepare(
        "SELECT c.heard, c.term FROM corrections c
         WHERE c.similar = 1 AND c.count >= ?1
           AND NOT EXISTS (SELECT 1 FROM vocabulary v WHERE v.term = c.heard)
           AND (SELECT COUNT(*) FROM corrections d WHERE d.heard = c.heard) = 1
         ORDER BY length(c.heard) DESC",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![REPLACE_AFTER], |row| Ok((row.get(0)?, row.get(1)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The taught words that can be applied as spellings — see [`spell`].
///
/// A word typed into Settings is meant, and applies at once. A word read out of
/// a correction applies at once only when it is the same letters written
/// differently — "hyper frames" into HyperFrames — because that is a fix to how
/// the words were written, and the only thing it can ever rewrite is those same
/// letters again. A correction that changed the letters is a claim about what
/// was said, and waits for [`REPLACE_AFTER`] like any other.
pub fn spellings(conn: &Connection) -> Vec<String> {
    let taught: Vec<(String, String)> = conn
        .prepare("SELECT term, source FROM vocabulary ORDER BY length(term) DESC")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();
    let fixes: Vec<(String, String, i64)> = conn
        .prepare("SELECT heard, term, count FROM corrections")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();

    taught
        .into_iter()
        .filter(|(term, source)| {
            is_a_spelling(term)
                && (source == Source::Added.as_str()
                    || fixes.iter().any(|(heard, of, count)| {
                        of.eq_ignore_ascii_case(term)
                            && (letters(heard) == letters(term) || *count >= REPLACE_AFTER)
                    }))
        })
        .map(|(term, _)| term)
        .collect()
}

/// Whether a taught word says something about how it is written.
///
/// A capital or a digit. A lowercase word taught as vocabulary is still learned
/// and still listed, but applying it as a spelling could only ever *lowercase*
/// something, or join two ordinary words that happen to share its letters —
/// "not able" into "notable" — neither of which anyone taught it to do.
fn is_a_spelling(term: &str) -> bool {
    term.chars().any(|c| c.is_uppercase() || c.is_ascii_digit())
}

/// Write taught names the way they were taught, wherever the words already say them.
///
/// Letter for letter, over one to three words: "hyper frames" becomes HyperFrames,
/// "voice dumps" becomes VoiceDumps, "claude" becomes Claude. Never a different
/// word — that takes a correction, made twice, see [`rules`]. Never across
/// punctuation, because "hyper, frames" is two things somebody said.
pub fn spell(text: &str, terms: &[String]) -> String {
    let named: Vec<(String, &str)> = terms
        .iter()
        .filter(|t| is_a_spelling(t))
        .map(|t| (letters(t), t.as_str()))
        .filter(|(key, _)| key.chars().count() >= 3)
        .collect();
    if named.is_empty() {
        return text.to_string();
    }
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    'next: while i < words.len() {
        for n in (1..=3).rev() {
            if i + n > words.len() {
                continue;
            }
            let inner_clean = words[i..i + n - 1]
                .iter()
                .all(|w| w.chars().last().is_some_and(char::is_alphanumeric));
            if !inner_clean {
                continue;
            }
            let key: String = words[i..i + n].iter().map(|w| letters(w)).collect();
            let Some((_, term)) = named.iter().find(|(k, _)| *k == key) else { continue };
            let first = words[i];
            let last = words[i + n - 1];
            let lead = &first[..first.len() - first.trim_start_matches(|c: char| !c.is_alphanumeric()).len()];
            let trail = &last[last.trim_end_matches(|c: char| !c.is_alphanumeric()).len()..];
            out.push(format!("{lead}{term}{trail}"));
            i += n;
            continue 'next;
        }
        out.push(words[i].to_string());
        i += 1;
    }
    out.join(" ")
}

// -- names the model spells a different way every time -------------------------

/// The shortest sound a taught name may be recognised by.
///
/// Consonant classes throw a great deal away — every vowel, and the difference
/// between b, f, p, v and w. Four symbols is about two syllables; below that a
/// taught name starts answering to ordinary speech.
const MIN_SOUND: usize = 4;

/// The consonant a letter stands for, as far as spelling can say.
///
/// The classes English confuses, and that transliteration confuses more: v with
/// f and w, d with t, c with k and s. Vowels are not here because they are what
/// varies most. `None` is a letter that says nothing about the sound.
fn sound_of(c: char) -> Option<char> {
    match c {
        'b' | 'f' | 'p' | 'v' | 'w' => Some('P'),
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some('K'),
        'd' | 't' => Some('T'),
        'l' => Some('L'),
        'm' | 'n' => Some('N'),
        'r' => Some('R'),
        _ if c.is_ascii_digit() => Some(c),
        _ => None,
    }
}

/// How something sounds, as far as its spelling can say.
///
/// Whisper spells a name nobody has told it a different way nearly every time.
/// One name, said seven times into this app, came back as Navin Upadhyaya,
/// Navinupadhyay, Navinapathyaya, Navinupatya, Naveen Upadhyaya and Navin
/// Padhyay. No table keyed on letters can learn that, because the next spelling
/// is always one it has not seen. All six of those reduce to the same five
/// sounds, and so does the name as it is actually written.
///
/// Vowels drop, and separate: two consonants of one class are two sounds when a
/// vowel stood between them, and one sound when nothing did — so a doubled
/// letter, an h, or a word boundary changes nothing, while "papa" stays longer
/// than "pa".
fn sounds_like(text: &str) -> String {
    let mut out = String::new();
    let mut last: Option<char> = None;
    for c in text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase) {
        match sound_of(c) {
            Some(sound) => {
                if last != Some(sound) {
                    out.push(sound);
                    last = Some(sound);
                }
            }
            // A vowel separates: what follows is its own sound even in the same
            // class. An h or a y is not even that, and leaves the run alone.
            None if matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') => last = None,
            None => {}
        }
    }
    out
}

/// Whether the model wrote this word the way it writes a name.
fn written_as_a_name(word: &str) -> bool {
    word.chars().find(|c| c.is_alphanumeric()).is_some_and(char::is_uppercase)
}

/// Write a taught name over words that only sound like it.
///
/// One to three words at a time, longest first, never across punctuation — the
/// same reach as [`spell`], which this runs after and which has already taken
/// everything that matched letter for letter. What is left are the spellings
/// nobody could have listed in advance.
///
/// A name has to be earned to get here: typed into Settings, or corrected
/// [`REPLACE_AFTER`] times — see [`sound_alikes`]. Matching by sound is a much
/// wider net than matching by letters, and it is cast only where somebody has
/// said twice that this is a word they use.
///
/// **And only over words the model already wrote as a name.** Sounds alone are
/// far too wide. Put past every sentence ever dictated on this Mac — a hundred
/// and thirty-two thousand words — sound matching on its own rewrote "of voice
/// dumps" as VoiceDumps, "for reference" as HyperFrames, "in some ways" as
/// Magnific and "now move to" as somebody's name: eighteen ruined sentences.
/// Every one of the eighteen was ordinary lowercase speech, and every one of the
/// seven real mis-spellings was capitalised, because a capital is the one thing
/// Whisper gets right about a name it cannot spell.
pub fn resemble(text: &str, names: &[String]) -> String {
    let heard: Vec<(String, String, &str)> = names
        .iter()
        .filter(|t| is_a_spelling(t))
        .map(|t| (sounds_like(t), letters(t), t.as_str()))
        .filter(|(sound, _, _)| sound.chars().count() >= MIN_SOUND)
        .collect();
    if heard.is_empty() {
        return text.to_string();
    }
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    'next: while i < words.len() {
        // Nothing that does not start as a name is even sounded out. This is
        // what keeps ordinary speech whole, and it is also why the pass costs
        // almost nothing: in running text, almost every word stops here.
        if !written_as_a_name(words[i]) {
            out.push(words[i].to_string());
            i += 1;
            continue;
        }
        for n in (1..=3).rev() {
            if i + n > words.len() {
                continue;
            }
            let inner_clean = words[i..i + n - 1]
                .iter()
                .all(|w| w.chars().last().is_some_and(char::is_alphanumeric));
            if !inner_clean {
                continue;
            }
            // Every word of it, not just the first: "Navin Padhyay" is a name
            // written twice over, while "Now move to" is a sentence starting.
            if !words[i..i + n].iter().all(|w| written_as_a_name(w)) {
                continue;
            }
            let window: String = words[i..i + n].iter().map(|w| letters(w)).collect();
            if window.chars().count() < MIN_SOUND {
                continue;
            }
            let sound = sounds_like(&window);
            // Already written as taught — `spell` has had its turn.
            let Some((_, _, term)) = heard
                .iter()
                .find(|(s, spelt, _)| *s == sound && *spelt != window)
            else {
                continue;
            };
            let first = words[i];
            let last = words[i + n - 1];
            let lead = &first[..first.len() - first.trim_start_matches(|c: char| !c.is_alphanumeric()).len()];
            let trail = &last[last.trim_end_matches(|c: char| !c.is_alphanumeric()).len()..];
            out.push(format!("{lead}{term}{trail}"));
            i += n;
            continue 'next;
        }
        out.push(words[i].to_string());
        i += 1;
    }
    out.join(" ")
}

/// The taught names that may be recognised by sound — see [`resemble`].
///
/// Typed into Settings, or corrected [`REPLACE_AFTER`] times. The letters-alone
/// shortcut that [`spellings`] allows is deliberately not here: respelling
/// "hyper frames" as HyperFrames can only ever touch those letters again, while
/// a sound reaches words nobody has seen yet.
pub fn sound_alikes(conn: &Connection) -> Vec<String> {
    let taught: Vec<(String, String)> = conn
        .prepare("SELECT term, source FROM vocabulary ORDER BY length(term) DESC")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();
    let earned: Vec<(String, i64)> = conn
        .prepare("SELECT term, MAX(count) FROM corrections GROUP BY term")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();

    taught
        .into_iter()
        .filter(|(term, source)| {
            is_a_spelling(term)
                && (source == Source::Added.as_str()
                    || earned
                        .iter()
                        .any(|(of, count)| of.eq_ignore_ascii_case(term) && *count >= REPLACE_AFTER))
        })
        .map(|(term, _)| term)
        .collect()
}

pub fn list(conn: &Connection) -> Vec<Term> {
    let earned = rules(conn);
    // Asked once for the whole list rather than reasoned about per row: these
    // are the same three answers the transcriber itself uses.
    let spelt = spellings(conn);
    let sounds = sound_alikes(conn);
    let mut stmt = match conn.prepare(
        "SELECT term, source, app, count FROM vocabulary ORDER BY last_seen DESC",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    let rows: Vec<(String, String, Option<String>, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    rows.into_iter()
        .map(|(term, source, app, count)| {
            let fix: Option<(String, f64)> = conn
                .query_row(
                    "SELECT heard, confidence FROM corrections WHERE term = ?1
                     ORDER BY count DESC, last_seen DESC LIMIT 1",
                    params![term],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .ok()
                .flatten();
            let (heard, confidence) = match fix {
                // Stored as -1 where it was never asked — see `ensure`.
                Some((h, c)) => (Some(h), (c >= 0.0).then_some(c as f32)),
                None => (None, None),
            };
            let (likeness, sounds_alike) = match heard.as_deref() {
                Some(h) => (likeness(h, &term), sounds_like(h) == sounds_like(&term)),
                None => (1.0, true),
            };
            let replaces = earned.iter().any(|(_, t)| t.eq_ignore_ascii_case(&term));
            let by_sound = sounds.iter().any(|s| s.eq_ignore_ascii_case(&term));
            let applied =
                replaces || by_sound || spelt.iter().any(|s| s.eq_ignore_ascii_case(&term));
            Term {
                term,
                heard,
                source,
                app,
                count,
                replaces,
                likeness,
                sounds_alike,
                confidence,
                applied,
                by_sound,
            }
        })
        .collect()
}

// -- commands -----------------------------------------------------------------

#[tauri::command]
pub fn list_vocabulary(store: tauri::State<crate::store::Store>) -> Vec<Term> {
    let conn = store.0.lock().unwrap();
    list(&conn)
}

#[tauri::command]
pub fn add_vocabulary(store: tauri::State<crate::store::Store>, term: String) -> Result<Vec<Term>, String> {
    let conn = store.0.lock().unwrap();
    add(&conn, &term, crate::now_ms())?;
    Ok(list(&conn))
}

#[tauri::command]
pub fn remove_vocabulary(store: tauri::State<crate::store::Store>, term: String) -> Result<Vec<Term>, String> {
    let conn = store.0.lock().unwrap();
    remove(&conn, &term)?;
    Ok(list(&conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure(&conn).unwrap();
        conn
    }

    /// The complaint, word for word: a name the model spells the common way.
    #[test]
    fn a_name_spelled_the_common_way_is_a_mishearing() {
        let fixes = fixes_between("Send the contract to Sean today.", "Send the contract to Shaun today.");
        assert_eq!(fixes, vec![Fix { heard: "Sean".into(), term: "Shaun".into(), similar: true }]);
    }

    /// Changing what you meant is not the model mishearing you.
    #[test]
    fn a_change_of_mind_is_not_learned() {
        assert!(fixes_between("Move it to Thursday.", "Move it to Friday.").is_empty());
    }

    /// A name that sounds nothing like its spelling is suggested, never rewritten.
    #[test]
    fn an_unlike_name_is_suggested_but_not_a_rule() {
        let fixes = fixes_between("copy win on it", "copy Nguyen on it");
        assert_eq!(fixes, vec![Fix { heard: "win".into(), term: "Nguyen".into(), similar: false }]);
    }

    /// A product the model hears as three words.
    #[test]
    fn words_joined_into_a_product_name_are_learned() {
        let fixes = fixes_between("I ran graph if i on it", "I ran graphify on it");
        assert_eq!(fixes, vec![Fix { heard: "graph if i".into(), term: "graphify".into(), similar: true }]);
    }

    /// Tidying the start of a sentence teaches nothing.
    #[test]
    fn capitalising_a_sentence_is_not_a_correction() {
        assert!(fixes_between("so we shipped it", "So we shipped it").is_empty());
    }

    /// Rewriting a sentence is not a list of corrections.
    #[test]
    fn a_rewrite_is_not_learned() {
        assert!(fixes_between(
            "we should probably talk about the launch plan",
            "let us meet tomorrow morning to finalise every detail of it",
        )
        .is_empty());
    }

    #[test]
    fn a_rule_rewrites_whole_words_and_keeps_punctuation() {
        let rules = vec![("sean".to_string(), "Shaun".to_string())];
        assert_eq!(apply("Ask Sean, then Seanna.", &rules), "Ask Shaun, then Seanna.");
    }

    #[test]
    fn a_rule_can_rewrite_several_words_into_one() {
        let rules = vec![("graph if i".to_string(), "graphify".to_string())];
        assert_eq!(apply("run graph if i now", &rules), "run graphify now");
    }

    /// The second identical fix is the one that makes it a rule.
    #[test]
    fn a_mishearing_becomes_a_rule_on_its_second_correction() {
        let conn = db();
        let fix = Fix { heard: "Sean".into(), term: "Shaun".into(), similar: true };
        learn(&conn, &[fix.clone()], Source::Corrected, Some("Slack"), 1);
        assert!(rules(&conn).is_empty(), "one fix suggests, it does not rewrite");
        learn(&conn, &[fix], Source::Corrected, Some("Slack"), 2);
        assert_eq!(rules(&conn), vec![("Sean".to_string(), "Shaun".to_string())]);
        assert!(list(&conn)[0].replaces);
    }

    /// A word the person really uses is never rewritten away.
    #[test]
    fn a_heard_word_that_is_itself_vocabulary_is_never_rewritten() {
        let conn = db();
        add(&conn, "Sean", 0).unwrap();
        let fix = Fix { heard: "Sean".into(), term: "Shaun".into(), similar: true };
        learn(&conn, &[fix.clone(), fix], Source::Edited, None, 1);
        assert!(rules(&conn).is_empty());
    }

    /// Unlike names are suggested however many times they are fixed.
    #[test]
    fn an_unlike_name_never_becomes_a_rule() {
        let conn = db();
        let fix = Fix { heard: "win".into(), term: "Nguyen".into(), similar: false };
        for t in 0..5 {
            learn(&conn, &[fix.clone()], Source::Corrected, None, t);
        }
        assert!(rules(&conn).is_empty());
        assert_eq!(spellings(&conn), vec!["Nguyen".to_string()]);
    }

    #[test]
    fn a_name_heard_in_pieces_is_written_as_taught() {
        let terms = vec!["HyperFrames".to_string(), "VoiceDumps".to_string()];
        assert_eq!(spell("made in hyper frames.", &terms), "made in HyperFrames.");
        assert_eq!(spell("open voice dumps now", &terms), "open VoiceDumps now");
    }

    #[test]
    fn a_name_is_recased_as_taught() {
        assert_eq!(spell("ask claude about it", &["Claude".to_string()]), "ask Claude about it");
    }

    /// Teaching a lowercase word never rewrites ordinary speech.
    #[test]
    fn a_lowercase_word_is_not_applied_as_a_spelling() {
        assert_eq!(spell("we are not able to", &["notable".to_string()]), "we are not able to");
    }

    #[test]
    fn a_spelling_never_joins_across_punctuation() {
        assert_eq!(spell("hyper, frames", &["HyperFrames".to_string()]), "hyper, frames");
    }

    /// A name the model heard in pieces applies the moment it is fixed once: the
    /// only thing it can ever rewrite is the letters it was fixed from.
    #[test]
    fn a_respelt_name_is_a_spelling_on_its_first_correction() {
        let conn = db();
        let fix = Fix { heard: "hyper frames".into(), term: "hyperFrames".into(), similar: true };
        learn(&conn, &[fix], Source::Corrected, Some("Notes"), 1);
        assert_eq!(spellings(&conn), vec!["hyperFrames".to_string()]);
    }

    /// A correction that changed the letters is a claim about what was said, and
    /// waits like any other. One reading of somebody else's text box, taken
    /// while they were still typing, is not proof of anything.
    #[test]
    fn a_name_read_back_differently_waits_for_a_second_correction() {
        let conn = db();
        let fix = Fix { heard: "Upadhyaya".into(), term: "Padhyay".into(), similar: true };
        learn(&conn, &[fix.clone()], Source::Corrected, Some("Notes"), 1);
        assert!(spellings(&conn).is_empty());
        learn(&conn, &[fix], Source::Corrected, Some("Notes"), 2);
        assert_eq!(spellings(&conn), vec!["Padhyay".to_string()]);
    }

    /// Typing a word into Settings is meant, however it was first learned.
    #[test]
    fn a_word_typed_into_settings_applies_at_once() {
        let conn = db();
        learn(
            &conn,
            &[Fix { heard: "Navinupadhyay".into(), term: "Naveen Upadhyay".into(), similar: true }],
            Source::Corrected,
            Some("Notes"),
            1,
        );
        assert!(spellings(&conn).is_empty());
        add(&conn, "Naveen Upadhyay", 2).unwrap();
        assert_eq!(spellings(&conn), vec!["Naveen Upadhyay".to_string()]);
    }

    #[test]
    fn only_words_with_a_shape_are_spellings() {
        let conn = db();
        add(&conn, "graphify", 1).unwrap();
        add(&conn, "HyperFrames", 2).unwrap();
        assert_eq!(spellings(&conn), vec!["HyperFrames".to_string()]);
    }

    /// Every sentence already dictated on this Mac, put past a set of taught
    /// names, printing each one the sound match would rewrite.
    ///
    /// The question a unit test cannot answer: matching by sound is a wide net,
    /// and the only honest measure of what else it catches is everything the
    /// person has actually said.
    ///
    ///     SOUND_SWEEP=corpus.txt SOUND_NAMES="Naveen Upadhyay,HyperFrames" \
    ///       cargo test --no-default-features sound_sweep -- --ignored --nocapture
    #[test]
    #[ignore = "readout: needs SOUND_SWEEP"]
    fn sound_sweep() {
        let (Ok(path), Ok(list)) = (std::env::var("SOUND_SWEEP"), std::env::var("SOUND_NAMES")) else {
            eprintln!("skipping: set SOUND_SWEEP and SOUND_NAMES");
            return;
        };
        let names: Vec<String> = list.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        for name in &names {
            println!("  {name:<24} {}", sounds_like(name));
        }
        let text = std::fs::read_to_string(&path).expect("corpus");
        let (mut lines, mut changed, mut words) = (0usize, 0usize, 0usize);
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            lines += 1;
            words += line.split_whitespace().count();
            let after = resemble(line, &names);
            if after != line {
                changed += 1;
                // Whole lines, not paired words: a name may be written over two
                // words and leave one, and every word after it would then read
                // as changed when nothing of the sort happened.
                println!("  -- {line}\n  ++ {after}");
            }
        }
        println!("\n{changed} of {lines} lines touched, {words} words swept");
    }

    /// The six spellings one name came back as, in one sitting, in one voice.
    /// Every one of them is the same five sounds as the name itself.
    #[test]
    fn every_spelling_of_a_name_sounds_the_same() {
        for heard in [
            "Navin Upadhyaya",
            "Navinupadhyay",
            "Navinapathyaya",
            "Navinupatya",
            "Naveen Upadhyaya",
            "Navin Padhyay",
        ] {
            assert_eq!(sounds_like(heard), sounds_like("Naveen Upadhyay"), "{heard}");
        }
    }

    #[test]
    fn a_name_is_written_as_taught_however_it_was_spelt() {
        let names = vec!["Naveen Upadhyay".to_string()];
        assert_eq!(
            resemble("my name is Navi also known as Navinapathyaya", &names),
            "my name is Navi also known as Naveen Upadhyay",
        );
        assert_eq!(
            resemble("also known as Navin Padhyay.", &names),
            "also known as Naveen Upadhyay.",
        );
    }

    /// The eighteen sentences that matching by sound alone ruined, taken from
    /// the corpus that caught them. Each is ordinary speech whose consonants
    /// happen to fall the same way as a taught name, and not one is written as
    /// a name — which is the whole of why they survive now.
    #[test]
    fn ordinary_words_that_merely_sound_alike_are_left_alone() {
        let names = vec![
            "Naveen Upadhyay".to_string(),
            "HyperFrames".to_string(),
            "VoiceDumps".to_string(),
            "Magnific".to_string(),
        ];
        for line in [
            "I have to see the competitors of voice dumps, I believe",
            "my systems which are quite engine which we have been building",
            "I think that that's for reference for you to learn",
            "so that means that you can now move to beta phase",
            "it sort of adds up in some ways rather than",
            "you can see the voice dump QE database",
            "My app voice dumps use it",
            "I would like to have a reference for it",
            "we can state a preference here",
        ] {
            assert_eq!(resemble(line, &names), line, "{line}");
        }
    }

    /// The number is the sentence's, and only when the sentence is there.
    #[test]
    fn confidence_comes_from_the_sentence_the_words_were_in() {
        let sure = vec![
            ("My name is Navi also known as Navin Upadhyaya".to_string(), 0.42_f32),
            ("Send it to Sean today.".to_string(), 0.91_f32),
        ];
        assert_eq!(confidence_for("Navin Upadhyaya", &sure), Some(0.42));
        assert_eq!(confidence_for("Sean", &sure), Some(0.91));
        assert_eq!(confidence_for("a thing nobody said", &sure), None);
        assert_eq!(confidence_for("", &sure), None);
        assert_eq!(confidence_for("Sean", &[]), None);
    }

    /// What was pasted and what was fixed differ in punctuation and case, so
    /// the sentence is found on its letters like everything else here.
    #[test]
    fn a_sentence_is_found_through_its_punctuation() {
        let sure = vec![("Okay, so the build is ready.".to_string(), 0.7_f32)];
        assert_eq!(confidence_for("the build", &sure), Some(0.7));
    }

    /// A correction nobody measured reads back as no number, never as zero.
    #[test]
    fn an_unmeasured_correction_has_no_confidence() {
        let conn = db();
        let fix = Fix { heard: "Sean".into(), term: "Shaun".into(), similar: true };
        learn(&conn, &[fix], Source::Edited, None, 1);
        assert_eq!(list(&conn)[0].confidence, None);
    }

    #[test]
    fn a_measured_correction_keeps_what_the_model_thought() {
        let conn = db();
        let fix = Fix { heard: "Sean".into(), term: "Shaun".into(), similar: true };
        let sure = vec![("Send it to Sean today.".to_string(), 0.4_f32)];
        learn_sure(&conn, &[fix], Source::Corrected, Some("Notes"), 1, &sure);
        let got = list(&conn)[0].confidence.expect("recorded");
        assert!((got - 0.4).abs() < 1e-6, "got {got}");
    }

    /// A vowel between two consonants of one class keeps them two sounds.
    #[test]
    fn a_repeated_sound_is_not_one_sound() {
        assert_ne!(sounds_like("papa"), sounds_like("pa"));
        assert_eq!(sounds_like("pappa"), sounds_like("papa"));
    }

    /// Two sounds would answer to half the language, so a short name is left
    /// to the rules that match letters.
    #[test]
    fn a_short_name_is_not_matched_by_sound() {
        assert_eq!(resemble("ask Sean about it", &["Shaun".to_string()]), "ask Sean about it");
    }

    #[test]
    fn a_name_never_crosses_punctuation() {
        let names = vec!["Naveen Upadhyay".to_string()];
        assert_eq!(resemble("Navin, Padhyay", &names), "Navin, Padhyay");
    }

    /// Being recognised by sound is earned. One correction is not enough.
    #[test]
    fn a_name_corrected_once_is_not_yet_matched_by_sound() {
        let conn = db();
        let fix = Fix { heard: "Navinupadhyay".into(), term: "Naveen Upadhyay".into(), similar: true };
        learn(&conn, &[fix.clone()], Source::Corrected, Some("Notes"), 1);
        assert!(sound_alikes(&conn).is_empty());
        learn(&conn, &[fix], Source::Corrected, Some("Notes"), 2);
        assert_eq!(sound_alikes(&conn), vec!["Naveen Upadhyay".to_string()]);
    }

    /// Typing it into Settings says it once and for all.
    #[test]
    fn a_name_typed_into_settings_is_matched_by_sound() {
        let conn = db();
        add(&conn, "Naveen Upadhyay", 1).unwrap();
        assert_eq!(sound_alikes(&conn), vec!["Naveen Upadhyay".to_string()]);
    }

    #[test]
    fn a_sentence_is_not_a_word() {
        assert!(check("please remember that the launch moves").is_err());
        assert!(check("   ").is_err());
        assert_eq!(check("  HyperFrames ").unwrap(), "HyperFrames");
    }
}
