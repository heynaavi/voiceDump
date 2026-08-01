//! Insights: what the history actually says about how you speak.
//!
//! Everything here is derived from rows already in `transcripts` — nothing is
//! logged for the sake of analytics, and nothing leaves the machine. The one
//! column added for this feature is `app_name`, filled only by the hotkey path.
//!
//! Two rules shape the numbers below, both learned from dashboards that lie:
//!
//! 1. **Never average across things that aren't the same.** Speaking rate is
//!    computed from dictation and microphone notes only. A dropped-in podcast
//!    is somebody else's mouth, and folding it in would quietly make "your"
//!    words-per-minute a number about strangers.
//! 2. **Say when a number is thin.** A rate computed from nine seconds of audio
//!    is noise, so the payload carries the sample it was built from and the UI
//!    is expected to show it rather than print a confident figure.
//!
//! No AI is involved anywhere in here: every figure is counted or measured
//! from text already on disk, which is why the whole feature works offline
//! and with nothing configured.

use crate::store::Store;
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use tauri::Manager;

// -- shared shapes ----------------------------------------------------------

#[derive(Serialize)]
pub struct Count {
    pub label: String,
    pub notes: i64,
    pub words: i64,
    pub seconds: f64,
}

#[derive(Serialize)]
pub struct Day {
    /// ISO `YYYY-MM-DD`, in the user's local timezone.
    pub date: String,
    pub notes: i64,
    pub words: i64,
}

#[derive(Serialize)]
pub struct Word {
    pub word: String,
    pub count: i64,
}

#[derive(Serialize)]
pub struct Vocabulary {
    pub unique_words: i64,
    pub total_words: i64,
    /// Unique ÷ total. Higher means less repetition; only comparable against
    /// yourself, and only over similar amounts of text.
    pub variety: f64,
    pub top_words: Vec<Word>,
    pub fillers: Vec<Word>,
    /// Filler words per 100 spoken words.
    pub filler_rate: f64,
    pub avg_sentence_words: f64,
    pub longest_sentence_words: i64,
}

#[derive(Serialize)]
pub struct Speaking {
    pub words_per_minute: f64,
    /// Seconds of audio the rate was computed from — the honesty field. Under a
    /// few minutes, the rate is a rumour.
    pub sample_seconds: f64,
    pub sample_notes: i64,
}

#[derive(Serialize)]
pub struct Summary {
    pub total_notes: i64,
    pub total_words: i64,
    pub total_seconds: f64,
    pub first_day: Option<String>,
    pub last_day: Option<String>,

    pub speaking: Speaking,
    pub by_day: Vec<Day>,
    pub current_streak: i64,
    pub longest_streak: i64,
    /// 24 buckets, local time, counting notes started in each hour.
    pub by_hour: Vec<i64>,

    pub by_source: Vec<Count>,
    pub by_app: Vec<Count>,
    /// Dictations recorded before app capture existed, or where the frontmost
    /// window couldn't be read. Reported rather than hidden so the app chart
    /// can say what it doesn't cover.
    pub app_unknown: i64,
    pub by_language: Vec<Count>,

    pub vocabulary: Vocabulary,
}

// -- text analysis ----------------------------------------------------------

/// Words too common to be interesting in a "what do you talk about" list.
/// Deliberately plain English function words; anything domain-specific stays in
/// so the top list still reflects the actual subject matter.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "if", "then", "than", "that", "this", "these", "those",
    "is", "are", "was", "were", "be", "been", "being", "am", "do", "does", "did", "doing", "have",
    "has", "had", "having", "i", "me", "my", "mine", "myself", "we", "us", "our", "ours", "you",
    "your", "yours", "he", "him", "his", "she", "her", "hers", "it", "its", "they", "them",
    "their", "theirs", "what", "which", "who", "whom", "when", "where", "why", "how", "all",
    "any", "both", "each", "few", "more", "most", "other", "some", "such", "no", "nor", "not",
    "only", "own", "same", "too", "very", "can", "will", "would", "should", "could", "may",
    "might", "must", "shall", "to", "of", "in", "on", "at", "by", "for", "with", "about",
    "against", "between", "into", "through", "during", "before", "after", "above", "below",
    "from", "up", "down", "out", "off", "over", "under", "again", "further", "once", "here",
    "there", "as", "so", "because", "while", "get", "got", "go", "going", "one", "two", "now",
    "also", "yeah", "okay", "ok", "well", "want", "need", "make", "made", "let", "lets", "see",
    "think", "know", "like", "just", "really", "thing", "things", "way", "s", "t", "re", "ve",
    "ll", "d", "m",
    // Contractions, spelled out rather than stemmed. `normalise` keeps the
    // apostrophe, so "let's" never matched the "lets" above and every one of
    // these was placing in "what you talk about" — a top-words list led by
    // "let's, what's, don't, it's" describes nobody. Stripping the suffix
    // instead would leave stubs ("don", "isn", "won") that are worse than the
    // contraction, so the forms are simply listed.
    "i'm", "i've", "i'll", "i'd", "it's", "that's", "there's", "here's", "what's", "who's",
    "how's", "let's", "he's", "she's", "we're", "we've", "we'll", "we'd", "you're", "you've",
    "you'll", "you'd", "they're", "they've", "they'll", "they'd", "don't", "doesn't", "didn't",
    "isn't", "aren't", "wasn't", "weren't", "won't", "can't", "couldn't", "wouldn't",
    "shouldn't", "haven't", "hasn't", "hadn't",
];

/// Only unambiguous fillers.
///
/// "like", "just", "so" and "really" are the loudest fillers in real speech and
/// the most tempting to count — and they are also ordinary words doing ordinary
/// work ("I like it", "just one"). Counting them by string match would inflate
/// the rate against anyone who speaks plainly, so they're left out. A number
/// that undercounts honestly beats one that overcounts confidently.
const FILLERS: &[&str] = &[
    "um", "uh", "erm", "uhm", "hmm", "mmm", "ah", "er",
];

/// Multi-word fillers, matched on the normalised text.
const FILLER_PHRASES: &[&str] = &[
    "you know",
    "i mean",
    "sort of",
    "kind of",
    "you see",
    "or something",
    "and stuff",
    "i guess",
];

/// Lowercase, strip anything that isn't a letter, apostrophe or space.
fn normalise(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphabetic() || c == '\'' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect()
}

fn analyse_text(all: &[String]) -> Vocabulary {
    let mut freq: HashMap<String, i64> = HashMap::new();
    let mut filler_counts: HashMap<String, i64> = HashMap::new();
    let mut total_words: i64 = 0;
    let mut sentence_lengths: Vec<i64> = Vec::new();

    for text in all {
        // Sentence split before normalising, while the punctuation still
        // exists. Whisper punctuates, so this is meaningful on transcripts;
        // on a wall of unpunctuated text it degrades to one long sentence,
        // which is a fair description of what was said.
        for sentence in text.split(|c| c == '.' || c == '?' || c == '!' || c == '\n') {
            let n = sentence.split_whitespace().count() as i64;
            if n > 0 {
                sentence_lengths.push(n);
            }
        }

        let flat = normalise(text);

        // Padded so a phrase at either end still matches on word boundaries,
        // and so " sort of " can't fire inside "resort often".
        let padded = format!(" {} ", flat.split_whitespace().collect::<Vec<_>>().join(" "));
        let mut subject = padded.clone();

        for phrase in FILLER_PHRASES {
            let needle = format!(" {phrase} ");
            let n = padded.matches(&needle).count() as i64;
            if n > 0 {
                *filler_counts.entry((*phrase).to_string()).or_insert(0) += n;
            }
            // Blank the phrase out of the copy the subject-matter tally reads.
            // Counting a phrase as a filler and then also counting the words it
            // is made of is how "sort" became the most-talked-about topic of
            // someone who had merely said "sort of" a lot. Only the matched
            // occurrences go — a standalone "sort the list" still counts.
            while let Some(at) = subject.find(&needle) {
                subject.replace_range(at..at + needle.len(), "  ");
            }
        }

        // Totals and single-word fillers read the untouched text: the filler
        // rate is a proportion of everything said, so removing words from the
        // denominator would inflate it.
        for word in padded.split_whitespace() {
            let w = word.trim_matches('\'');
            if w.is_empty() {
                continue;
            }
            total_words += 1;
            if FILLERS.contains(&w) {
                *filler_counts.entry(w.to_string()).or_insert(0) += 1;
            }
        }

        for word in subject.split_whitespace() {
            let w = word.trim_matches('\'');
            if w.len() < 3 || FILLERS.contains(&w) || STOP_WORDS.contains(&w) {
                continue;
            }
            *freq.entry(w.to_string()).or_insert(0) += 1;
        }
    }

    let unique_words = freq.len() as i64;

    let mut top: Vec<Word> = freq
        .into_iter()
        .map(|(word, count)| Word { word, count })
        .collect();
    // Count descending, then alphabetically, so equal counts don't reshuffle
    // between calls and make the panel look alive when nothing changed.
    top.sort_by(|a, b| b.count.cmp(&a.count).then(a.word.cmp(&b.word)));
    top.truncate(25);

    let mut fillers: Vec<Word> = filler_counts
        .into_iter()
        .map(|(word, count)| Word { word, count })
        .collect();
    fillers.sort_by(|a, b| b.count.cmp(&a.count).then(a.word.cmp(&b.word)));
    let filler_total: i64 = fillers.iter().map(|f| f.count).sum();
    fillers.truncate(10);

    let avg_sentence_words = if sentence_lengths.is_empty() {
        0.0
    } else {
        sentence_lengths.iter().sum::<i64>() as f64 / sentence_lengths.len() as f64
    };

    Vocabulary {
        unique_words,
        total_words,
        variety: if total_words > 0 {
            unique_words as f64 / total_words as f64
        } else {
            0.0
        },
        top_words: top,
        fillers,
        filler_rate: if total_words > 0 {
            filler_total as f64 * 100.0 / total_words as f64
        } else {
            0.0
        },
        avg_sentence_words,
        longest_sentence_words: sentence_lengths.into_iter().max().unwrap_or(0),
    }
}

// -- streaks ----------------------------------------------------------------

/// Consecutive-day runs over the set of days that have at least one note.
///
/// The current streak counts back from today *or* yesterday: at 9am you have
/// not necessarily dictated yet, and zeroing a twelve-day streak because the
/// morning is young would be both wrong and demoralising.
fn streaks(days: &HashSet<NaiveDate>, today: NaiveDate) -> (i64, i64) {
    if days.is_empty() {
        return (0, 0);
    }

    let mut sorted: Vec<NaiveDate> = days.iter().copied().collect();
    sorted.sort();

    let mut longest = 1i64;
    let mut run = 1i64;
    for pair in sorted.windows(2) {
        if pair[1] == pair[0].succ_opt().unwrap_or(pair[1]) {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 1;
        }
    }

    let mut current = 0i64;
    let mut cursor = if days.contains(&today) {
        today
    } else {
        match today.pred_opt() {
            Some(y) if days.contains(&y) => y,
            _ => return (0, longest),
        }
    };
    while days.contains(&cursor) {
        current += 1;
        match cursor.pred_opt() {
            Some(p) => cursor = p,
            None => break,
        }
    }

    (current, longest)
}

fn local_day(ms: i64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.date_naive())
}

// -- the command ------------------------------------------------------------

#[tauri::command]
pub fn analytics_summary(app: tauri::AppHandle) -> Result<Summary, String> {
    let store = app.state::<Store>();
    let conn = store.0.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT created_at, word_count, duration, source, language, app_name, text
             FROM transcripts ORDER BY created_at",
        )
        .map_err(|e| e.to_string())?;

    struct Row {
        created_at: i64,
        words: i64,
        duration: f64,
        source: String,
        language: String,
        app_name: String,
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut texts: Vec<String> = Vec::new();

    let iter = stmt
        .query_map([], |r| {
            Ok((
                Row {
                    created_at: r.get(0)?,
                    words: r.get(1)?,
                    duration: r.get(2)?,
                    source: r.get(3)?,
                    language: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    app_name: r.get(5)?,
                },
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for item in iter {
        let (row, text) = item.map_err(|e| e.to_string())?;
        rows.push(row);
        texts.push(text);
    }

    let total_notes = rows.len() as i64;
    let total_words: i64 = rows.iter().map(|r| r.words).sum();
    let total_seconds: f64 = rows.iter().map(|r| r.duration).sum();

    // Rule 1: your voice only. See the module header.
    let spoken: Vec<&Row> = rows
        .iter()
        .filter(|r| r.source == "hotkey" || r.source == "mic")
        .collect();
    let spoken_seconds: f64 = spoken.iter().map(|r| r.duration).sum();
    let spoken_words: i64 = spoken.iter().map(|r| r.words).sum();

    let speaking = Speaking {
        words_per_minute: if spoken_seconds > 0.0 {
            spoken_words as f64 / (spoken_seconds / 60.0)
        } else {
            0.0
        },
        sample_seconds: spoken_seconds,
        sample_notes: spoken.len() as i64,
    };

    // Days, hours, streaks.
    let mut per_day: HashMap<NaiveDate, (i64, i64)> = HashMap::new();
    let mut by_hour = vec![0i64; 24];
    for r in &rows {
        if let Some(dt) = Local.timestamp_millis_opt(r.created_at).single() {
            let e = per_day.entry(dt.date_naive()).or_insert((0, 0));
            e.0 += 1;
            e.1 += r.words;
            by_hour[dt.hour() as usize] += 1;
        }
    }

    let mut by_day: Vec<Day> = per_day
        .iter()
        .map(|(d, (notes, words))| Day {
            date: format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()),
            notes: *notes,
            words: *words,
        })
        .collect();
    by_day.sort_by(|a, b| a.date.cmp(&b.date));

    let day_set: HashSet<NaiveDate> = per_day.keys().copied().collect();
    let (current_streak, longest_streak) = streaks(&day_set, Local::now().date_naive());

    // Groupings.
    let group = |key: &dyn Fn(&Row) -> Option<String>| -> Vec<Count> {
        let mut m: HashMap<String, (i64, i64, f64)> = HashMap::new();
        for r in &rows {
            if let Some(k) = key(r) {
                let e = m.entry(k).or_insert((0, 0, 0.0));
                e.0 += 1;
                e.1 += r.words;
                e.2 += r.duration;
            }
        }
        let mut v: Vec<Count> = m
            .into_iter()
            .map(|(label, (notes, words, seconds))| Count {
                label,
                notes,
                words,
                seconds,
            })
            .collect();
        v.sort_by(|a, b| b.notes.cmp(&a.notes).then(a.label.cmp(&b.label)));
        v
    };

    let by_source = group(&|r| Some(r.source.clone()));
    let by_app = group(&|r| {
        if r.app_name.is_empty() {
            None
        } else {
            Some(r.app_name.clone())
        }
    });
    let by_language = group(&|r| {
        if r.language.is_empty() {
            None
        } else {
            Some(r.language.clone())
        }
    });

    let app_unknown = rows
        .iter()
        .filter(|r| r.source == "hotkey" && r.app_name.is_empty())
        .count() as i64;

    Ok(Summary {
        total_notes,
        total_words,
        total_seconds,
        first_day: rows
            .first()
            .and_then(|r| local_day(r.created_at))
            .map(|d| d.to_string()),
        last_day: rows
            .last()
            .and_then(|r| local_day(r.created_at))
            .map(|d| d.to_string()),
        speaking,
        by_day,
        current_streak,
        longest_streak,
        by_hour,
        by_source,
        by_app,
        app_unknown,
        by_language,
        vocabulary: analyse_text(&texts),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn streak_counts_back_from_today() {
        let days: HashSet<_> = [day(2026, 7, 29), day(2026, 7, 30), day(2026, 7, 31)]
            .into_iter()
            .collect();
        assert_eq!(streaks(&days, day(2026, 7, 31)), (3, 3));
    }

    /// The 9am case: nothing dictated yet today, but yesterday's streak stands.
    #[test]
    fn streak_survives_a_quiet_morning() {
        let days: HashSet<_> = [day(2026, 7, 29), day(2026, 7, 30)].into_iter().collect();
        assert_eq!(streaks(&days, day(2026, 7, 31)).0, 2);
    }

    #[test]
    fn streak_is_zero_after_a_missed_day() {
        let days: HashSet<_> = [day(2026, 7, 20), day(2026, 7, 21)].into_iter().collect();
        let (current, longest) = streaks(&days, day(2026, 7, 31));
        assert_eq!(current, 0);
        assert_eq!(longest, 2);
    }

    #[test]
    fn longest_streak_spans_a_month_boundary() {
        let days: HashSet<_> = [day(2026, 6, 29), day(2026, 6, 30), day(2026, 7, 1)]
            .into_iter()
            .collect();
        assert_eq!(streaks(&days, day(2026, 7, 31)).1, 3);
    }

    #[test]
    fn ordinary_words_are_not_counted_as_fillers() {
        // The whole point of the conservative filler list: this sentence is
        // clean speech and must score zero, not three.
        let v = analyse_text(&["I like it, so just ship it.".to_string()]);
        assert_eq!(v.filler_rate, 0.0);
    }

    #[test]
    fn real_fillers_are_counted() {
        let v = analyse_text(&["um so you know it was uh fine".to_string()]);
        let total: i64 = v.fillers.iter().map(|f| f.count).sum();
        assert_eq!(total, 3); // um, uh, "you know"
    }

    /// A filler phrase must not also become subject matter.
    ///
    /// Saying "sort of" eleven times made "sort" the top word in "what you talk
    /// about" — the phrase was counted as a filler *and* its parts were counted
    /// as topics. The share card built on that list would have described
    /// somebody's verbal tic as their week's work.
    #[test]
    fn filler_phrases_do_not_leak_into_top_words() {
        let v = analyse_text(&["sort of the design sort of the design".to_string()]);
        let top: Vec<&str> = v.top_words.iter().map(|w| w.word.as_str()).collect();
        assert!(!top.contains(&"sort"), "filler part in top words: {top:?}");
        assert!(top.contains(&"design"));
        // Still counted as the filler it is.
        assert_eq!(v.fillers.iter().find(|f| f.word == "sort of").unwrap().count, 2);
    }

    /// The same word used properly is still subject matter.
    #[test]
    fn a_standalone_word_survives_its_phrase_being_a_filler() {
        let v = analyse_text(&["sort the records and sort the files".to_string()]);
        let top: Vec<&str> = v.top_words.iter().map(|w| w.word.as_str()).collect();
        assert!(top.contains(&"sort"), "lost a real use of the word: {top:?}");
        assert!(v.fillers.is_empty());
    }

    /// Contractions are function words, not topics.
    #[test]
    fn contractions_are_not_topics() {
        let v = analyse_text(
            &["let's ship it it's what's next don't wait let's ship".to_string()],
        );
        let top: Vec<&str> = v.top_words.iter().map(|w| w.word.as_str()).collect();
        for c in ["let's", "it's", "what's", "don't"] {
            assert!(!top.contains(&c), "contraction in top words: {top:?}");
        }
        assert!(top.contains(&"ship"));
    }

    #[test]
    fn stop_words_stay_out_of_the_top_list() {
        let v = analyse_text(&["the the the roadmap roadmap".to_string()]);
        assert_eq!(v.top_words.len(), 1);
        assert_eq!(v.top_words[0].word, "roadmap");
    }

    #[test]
    fn empty_history_does_not_divide_by_zero() {
        let v = analyse_text(&[]);
        assert_eq!(v.total_words, 0);
        assert_eq!(v.filler_rate, 0.0);
        assert_eq!(v.variety, 0.0);
        assert_eq!(streaks(&HashSet::new(), day(2026, 7, 31)), (0, 0));
    }
}
