//! What a message *is*, before anything is looked up.
//!
//! Typing "thanks" used to search the transcripts for the word "thanks", match
//! two notes that happened to contain it, and hand them to the model as though
//! a question had been asked. Nothing in the pipeline had ever been asked to
//! decide whether the thing typed was a question at all.
//!
//! **Deliberately not a model call.** A greeting does not need a language
//! model: this runs in well under a microsecond, is exhaustively testable, and
//! — the part that decided it — keeps working when Apple Intelligence does not,
//! which on the machine this was written on was for a solid hour.
//!
//! The precision comes from one rule: a message is social only when *every*
//! token in it is a pleasantry. Not "starts with thanks", not "contains hi".
//! An earlier draft did have a gratitude-opener rule and scored better on
//! aggregate; it was cut because it answered "thanks — pull up the marketing
//! sync", "thank you, open the roadmap note" and "appreciate it, and the mute
//! button one too" with "Any time." and never searched. Each of those names a
//! subject that exists in the graph. A silent social reply to a real request is
//! strictly worse than a visibly wrong answer, because nothing about it tells
//! you it went wrong.

/// Which pleasantry, so the reply can be the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Social {
    Greeting,
    Thanks,
    Farewell,
    Sorry,
    Ack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A pleasantry. Answer it and look nothing up.
    Social(Social),
    /// A question about the app rather than about the notes.
    Meta,
    /// Everything else, which is the default and must stay the default.
    Question,
}

/// Where a message stops being a pleasantry and starts being a sentence.
const MOST_WORDS: usize = 8;

/// Split a message the way the rest of the pipeline splits text.
///
/// `is_alphanumeric`, never `is_ascii_alphanumeric` — this is the difference
/// between a question in Russian, Japanese or Hindi being read as a question
/// and being read as an empty token list, which lands it in `Social(Ack)` and
/// answers "Ready when you are." to somebody asking about their pricing notes.
/// It also matches [`crate::chat`] and [`crate::store`], which both already
/// split this way.
///
/// Apostrophes are deleted rather than split on, so "that's" is one token
/// `thats`. Every list below is spelled in this alphabet for that reason.
fn words_of(message: &str) -> Vec<String> {
    message
        .chars()
        // The curly apostrophe a Mac types by default.
        .map(|c| if c == '\u{2019}' { '\'' } else { c })
        .collect::<String>()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.replace('\'', ""))
        .filter(|w| !w.is_empty())
        .collect()
}

pub fn classify(message: &str) -> Intent {
    let words = words_of(message);

    // Emoji, punctuation, an empty box. Nothing was asked.
    if words.is_empty() {
        return Intent::Social(Social::Ack);
    }

    // All-or-nothing. This single condition is the whole of the precision.
    if words.len() <= MOST_WORDS && words.iter().all(|w| social_word(w)) {
        let kind = if words.iter().any(|w| THANKS.contains(&w.as_str())) {
            Social::Thanks
        } else if words.iter().any(|w| FAREWELL.contains(&w.as_str())) {
            Social::Farewell
        } else if words.iter().any(|w| GREETING.contains(&w.as_str())) {
            Social::Greeting
        } else if words.iter().any(|w| SORRY.contains(&w.as_str())) {
            Social::Sorry
        } else {
            Social::Ack
        };
        return Intent::Social(kind);
    }

    // Whole-message first, so a bare "help" still lands.
    let whole = words.join(" ");
    if meta_has(&whole) {
        return Intent::Meta;
    }

    // Then with a leading pleasantry peeled off — "hi, what can you do" — but
    // only when three or more words survive. Without that floor "that's no
    // help" peels down to "help" and becomes a question about the app instead
    // of a complaint about an answer.
    let start = words.iter().position(|w| !social_word(w)).unwrap_or(words.len());
    if words.len() - start >= 3 && meta_has(&words[start..].join(" ")) {
        return Intent::Meta;
    }

    Intent::Question
}

/// Which kind of pleasantry this was, once something else has decided it *is*
/// one.
///
/// The split from [`classify`] matters. Deciding whether a message is small talk
/// is a judgement about meaning, and a word list is measurably the wrong tool for
/// it — that is [`crate::route`]'s whole subject. Deciding whether small talk was
/// a thank-you or a goodbye is not a judgement about meaning at all, it is a
/// lookup, and getting it wrong costs somebody the word "Anytime" where they
/// expected "See you". A list is exactly right for that.
pub fn flavour(message: &str) -> Social {
    let words = words_of(message);
    if words.iter().any(|w| THANKS.contains(&w.as_str())) {
        Social::Thanks
    } else if words.iter().any(|w| FAREWELL.contains(&w.as_str())) {
        Social::Farewell
    } else if words.iter().any(|w| GREETING.contains(&w.as_str())) {
        Social::Greeting
    } else if words.iter().any(|w| SORRY.contains(&w.as_str())) {
        Social::Sorry
    } else {
        Social::Ack
    }
}

/// Compare in the tokenizer's own alphabet, never against a raw literal.
///
/// `words_of` deletes apostrophes, so "what's this" arrives as `whats this`
/// and would never match an entry spelled "what's this". Running both sides
/// through the same function is what keeps the list honest.
fn meta_has(phrase: &str) -> bool {
    META.iter().any(|m| words_of(m).join(" ") == phrase)
}

fn social_word(word: &str) -> bool {
    let w = word;
    GREETING.contains(&w)
        || THANKS.contains(&w)
        || FAREWELL.contains(&w)
        || SORRY.contains(&w)
        || ACK.contains(&w)
        || FILLER.contains(&w)
}

// -- the lists ---------------------------------------------------------------
//
// GROWTH RULE, and it is the entire safety argument: a word goes in only if it
// could not plausibly name something in a library of voice notes. `notes`,
// `note`, `meeting`, `call`, `work`, `help`, `plan`, `problem` are deliberately
// absent even though adding them would score better on a test set — every one
// of them is a thing somebody records notes about.

const GREETING: &[&str] = &[
    "hi", "hii", "hiii", "hiya", "hey", "heyy", "heya", "hello", "helo", "hellooo", "yo", "sup",
    "howdy", "greetings", "morning", "afternoon", "evening", "gm",
];

const THANKS: &[&str] = &[
    "thanks", "thank", "thankyou", "thanku", "thx", "thnx", "thanx", "ty", "tysm", "cheers",
    "appreciate", "appreciated", "grateful",
];

const FAREWELL: &[&str] = &[
    "bye", "byee", "goodbye", "cya", "ciao", "ttyl", "goodnight", "gn", "later", "night", "see",
    "ya",
];

const SORRY: &[&str] = &["sorry", "oops", "apologies", "whoops", "mybad"];

const ACK: &[&str] = &[
    "ok", "okay", "okey", "k", "kk", "alright", "aight", "right", "sure", "yeah", "yea", "yep",
    "yes", "yup", "nope", "no", "nah", "got", "gotcha", "understood", "fine", "done", "true", "np",
    "nvm", "cool", "nice", "great", "good", "awesome", "perfect", "amazing", "brilliant", "lovely",
    "excellent", "sweet", "neat", "wonderful", "fantastic", "beautiful", "solid", "wow", "lol",
    "lmao", "rofl", "haha", "hahaha", "hehe", "ha", "hmm", "hm", "ah", "oh", "yay", "woo", "best",
    "worries", "worry", "welcome", "legend", "star", "top", "word", "ta", "sounds", "stuff",
    "exactly", "helpful",
];

/// Pure function words, so the lists above can be strung into a sentence
/// — "ok cool thanks so much man" — without any of them naming a subject.
const FILLER: &[&str] = &[
    "a", "an", "the", "it", "its", "that", "thats", "this", "you", "youre", "your", "ur", "u",
    "so", "very", "much", "lot", "lots", "my", "me", "i", "im", "am", "all", "too", "again", "and",
    "for", "to", "of", "man", "mate", "buddy", "dude", "bro", "friend", "there", "then", "well",
    "just", "really", "please", "pls", "plz", "one",
];

/// Questions about the app rather than about the notes.
///
/// SPELLING RULE: written the way `words_of` emits it — lowercase, apostrophes
/// deleted. Contracted forms are separate entries because "what's" → "whats".
const META: &[&str] = &[
    "what can you do", "what can you do for me", "what can you help with",
    "what can you help me with", "what else can you do", "what else can you help with",
    // "all" as an intensifier — Indian and Irish English especially. The
    // original miss that started all of this: "what can you do" was in the list
    // and "what all can you do" was not, so one was answered and the other went
    // off to search a library of voice notes for the word "all".
    "what all can you do", "what all can you help with", "what all can i ask",
    "what all can i ask you", "what all do you do",
    "what do you do", "what do you actually do", "what do you do exactly",
    "what exactly can you do", "what are you", "what are you for", "what are you good at",
    "what are you able to do", "what are your limits", "who are you", "who are you exactly",
    "who made you", "who built you", "who created you",
    "what is this", "whats this", "what is this app", "whats this app",
    "what is this thing", "whats this thing", "what is this for", "whats this for",
    "what does this do", "what can this app do", "what can this do",
    "how does this work", "hows this work", "how does it work", "hows it work",
    "how does this app work", "hows this app work", "how does this thing work",
    "hows this thing work", "how do you work", "how do i use this", "how do i use it",
    "how do i use this thing", "how can you help", "how can you help me",
    "can you help", "can you help me",
    "what can i ask", "what can i ask you", "what should i ask", "what can i do here",
    "what kind of questions can i ask", "what sort of questions can i ask",
    "what kind of questions can i ask you", "what sort of questions can i ask you",
    "what kind of things can i ask", "what kind of things can i ask you",
    "what do you know", "what do you know about me", "what can you tell me",
    "what else can you tell me", "tell me what you can do",
    "are you an ai", "are you ai", "are you a bot", "are you an assistant", "are you chatgpt",
    "what model are you", "tell me about yourself", "what is your purpose",
    "whats your purpose", "where do your answers come from", "do you use my data",
    "who is this", "whos this",
    "help", "help me", "what is voicedumps", "whats voicedumps",
];

// -- what to say -------------------------------------------------------------

/// Written, not generated. A greeting does not need a language model, and one
/// that took two seconds and a process spawn to say "Hello" would be worse.
pub fn social_reply(kind: Social) -> String {
    match kind {
        Social::Thanks => "Any time.",
        Social::Greeting => {
            "Hello. Ask me anything that's in your notes — a name, a project, something \
             you decided."
        }
        Social::Ack => "Ready when you are.",
        Social::Farewell => "See you.",
        Social::Sorry => "Nothing to apologise for.",
    }
    .to_string()
}

/// What the library actually contains, for the meta reply to be specific with.
pub fn library_facts(conn: &rusqlite::Connection) -> (i64, i64, Vec<String>) {
    let counts: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM transcripts),
                    (SELECT COUNT(DISTINCT n.id) FROM graph_nodes n
                       JOIN graph_mentions m ON m.node_id = n.id)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0, 0));

    let top = conn
        .prepare(
            "SELECT n.name FROM graph_nodes n JOIN graph_mentions m ON m.node_id = n.id
              GROUP BY n.id
              ORDER BY COUNT(m.transcript_id) DESC, MAX(m.created_at) DESC
              LIMIT 3",
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();

    (counts.0, counts.1, top)
}

/// Answer "what can you do" with what this install can actually do.
///
/// Specific rather than generic on purpose: the counts and the three subjects
/// come from the user's own library, so the answer is different on every Mac
/// and every one of the suggestions is a question that will land.
pub fn meta_reply(notes: i64, subjects: i64, top: &[String], model: bool) -> String {
    let mut out = String::from("I answer out of your own recordings, and nothing else.\n\n");

    out.push_str(if model {
        "Ask about a person, a project, or something you decided, and I'll find the notes \
         that bear on it, read up to six, and cite the ones I used so you can open them and \
         check. When they don't cover it I'll say so rather than fill the gap."
    } else {
        "Ask about a person, a project, or something you decided, and I'll find the notes \
         that bear on it and show you which ones. Writing the answer needs Apple \
         Intelligence, which is off on this Mac."
    });

    if notes == 0 {
        out.push_str("\n\nNothing is recorded yet — make a recording and then ask me about it.");
        return out;
    }

    out.push_str(&format!(
        "\n\nThere {} {} {} and {} {} on record here.",
        if notes == 1 { "is" } else { "are" },
        notes,
        if notes == 1 { "note" } else { "notes" },
        subjects,
        if subjects == 1 { "subject" } else { "subjects" },
    ));

    if !top.is_empty() {
        out.push_str(&format!(
            " {} you could ask about right now: {}.",
            match top.len() {
                1 => "One",
                2 => "Two",
                _ => "Three",
            },
            top.join(", "),
        ));
    }
    out
}

// -- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn q(m: &str) -> bool {
        classify(m) == Intent::Question
    }
    fn meta(m: &str) -> bool {
        classify(m) == Intent::Meta
    }
    fn social(m: &str) -> bool {
        matches!(classify(m), Intent::Social(_))
    }

    /// The bug this module exists for.
    #[test]
    fn a_pleasantry_is_not_a_search() {
        for m in [
            "thanks", "Thanks!", "thank you", "thanks so much", "ty", "cheers",
            "hi", "hello", "hey there", "good morning",
            "ok", "ok cool", "cool cool cool", "got it", "understood", "nice one",
            "bye", "goodnight", "see ya", "sorry", "oops",
            "👍", "?!", "   ", "haha", "lol",
            "ok cool thanks so much man",
        ] {
            assert!(social(m), "{m:?} should be social, got {:?}", classify(m));
        }
    }

    #[test]
    fn the_right_pleasantry_gets_the_right_reply() {
        assert_eq!(classify("thanks"), Intent::Social(Social::Thanks));
        assert_eq!(classify("hi"), Intent::Social(Social::Greeting));
        assert_eq!(classify("bye"), Intent::Social(Social::Farewell));
        assert_eq!(classify("sorry"), Intent::Social(Social::Sorry));
        assert_eq!(classify("ok"), Intent::Social(Social::Ack));
    }

    /// The rule that was cut, kept as a test so it cannot come back. Each of
    /// these names a subject that exists in a real graph; answering "Any time."
    /// and searching nothing is the worst outcome available, because nothing
    /// about it tells you it went wrong.
    #[test]
    fn a_pleasantry_in_front_of_a_real_request_is_still_a_request() {
        for m in [
            "thanks — pull up the marketing sync",
            "thank you, open the roadmap note",
            "thanks, more on the design system",
            "appreciate it, and the mute button one too",
            "good morning meeting notes",
            "hey what did Rupesh say",
        ] {
            assert!(q(m), "{m:?} should be a question, got {:?}", classify(m));
        }
    }

    /// The pair that decides whether the classifier is worth having: the same
    /// opening words, one asking about the app and one asking about the notes.
    #[test]
    fn asking_about_the_app_and_asking_about_the_notes_are_told_apart() {
        assert!(meta("what can you do"));
        assert!(q("what can I do about pricing"));
        assert!(q("what can you do about the menu overcrowding"));

        assert!(meta("who are you"));
        assert!(q("who is Rupesh"));

        assert!(meta("how does this work"));
        assert!(q("how does the waveform work"));
    }

    /// `words_of` deletes apostrophes, so every list entry is spelled without
    /// one. Comparing against a raw literal would silently never match.
    #[test]
    fn a_contraction_matches_the_list_it_is_spelled_out_of() {
        for m in ["what's this", "what's this app", "how's it work", "who's this"] {
            assert!(meta(m), "{m:?} should be meta, got {:?}", classify(m));
        }
        assert_eq!(
            words_of("that's great, thank you"),
            vec!["thats", "great", "thank", "you"]
        );
        // The curly apostrophe a Mac types by default.
        assert_eq!(words_of("what\u{2019}s this"), vec!["whats", "this"]);
    }

    /// Peeling a pleasantry off the front must not manufacture a meta phrase
    /// out of a complaint.
    #[test]
    fn a_complaint_containing_help_is_not_a_question_about_the_app() {
        for m in ["that's no help", "no help", "great help", "ok fine no help", "hmm help"] {
            assert!(q(m), "{m:?} should be a question, got {:?}", classify(m));
        }
        // But the bare word still is.
        assert!(meta("help"));
        assert!(meta("help me"));
        assert!(meta("hi what can you do"));
    }

    /// `is_ascii_alphanumeric` would tokenise all of these to nothing, land
    /// them in `Social(Ack)`, and answer "Ready when you are." to somebody
    /// asking about their notes.
    #[test]
    fn a_question_in_another_script_is_a_question() {
        for m in [
            "что я говорил о ценах",
            "私のメモに何が書いてありましたか",
            "मैंने प्राइसिंग के बारे में क्या कहा",
        ] {
            assert!(q(m), "{m:?} should be a question, got {:?}", classify(m));
        }
    }

    /// Anything long is a question whatever it is made of, because a sentence
    /// that long is not a pleasantry.
    #[test]
    fn a_long_message_is_never_social() {
        assert!(q("ok cool thanks so much man that is really very nice of you indeed"));
    }

    #[test]
    fn the_meta_reply_is_specific_to_this_library() {
        let said = meta_reply(183, 410, &["light build".into(), "AI".into()], true);
        assert!(said.contains("183 notes"));
        assert!(said.contains("410 subjects"));
        assert!(said.contains("light build, AI"));
        assert!(said.contains("cite"), "it should say answers are checkable");

        let off = meta_reply(183, 410, &[], false);
        assert!(off.contains("Apple Intelligence"));
        assert!(!off.contains("read up to six"), "it cannot promise what it cannot do");
    }

    #[test]
    fn an_empty_library_says_so_rather_than_counting_to_zero() {
        let said = meta_reply(0, 0, &[], true);
        assert!(said.contains("Nothing is recorded yet"));
        assert!(!said.contains("0 notes"));
    }

    #[test]
    fn one_note_is_not_one_notes() {
        let said = meta_reply(1, 1, &["pricing".into()], true);
        assert!(said.contains("is 1 note"), "{said}");
        assert!(said.contains("1 subject "), "{said}");
        assert!(!said.contains("notes and"), "{said}");
    }

    /// The guard that caught a refactor silently dropping `see`, `ya` and
    /// `sounds` from the lists: no title in a real library may be unreachable.
    /// A note called "Good morning" is a note somebody wants to find.
    #[test]
    fn a_real_note_title_is_never_swallowed_as_a_pleasantry() {
        for title in [
            "Loading Animation Issues",
            "UI and Notification Design",
            "Creative marketing concepts",
            "Mother's Daily Life Reflections",
            "Live Transcription Concerns",
            "Settings Animation Issue",
        ] {
            assert!(q(title), "{title:?} should be a question, got {:?}", classify(title));
        }
    }
}
