//! Deciding what somebody meant, by asking the model rather than a word list.
//!
//! The version of this that shipped first was `const GREETING: &[&str]` and
//! friends — about seventy phrases, matched against the message. It failed the
//! way word lists always fail: `"what can you do"` was in the list and
//! `"what all can you do"` was not, so one of them was answered and the other
//! went off to full-text-search a library of voice notes for the word "all".
//!
//! Replacing it needed one thing to be true, and it is worth being precise about
//! which, because the obvious version is false. **Free-text classification does
//! not work here.** Asked ten yes/no questions with "Reply YES or NO." in the
//! instructions, this model scores 10/10 on *format* and 5/10 on *truth*: it
//! answers "YES." to all ten, including "Do penguins fly?". Reverse the wording
//! to "Reply NO or YES." and it answers "YES." ten times again. It is latching
//! onto a literal token in the instruction, not deciding anything.
//!
//! Under a schema with a closed set of choices, the same model scores 9/10. That
//! is the whole reason this module exists in the shape it does: the routing is
//! not a prompt asking for a label, it is a constrained decode over five of
//! them.
//!
//! What the measurements said about how to write it, on a 52-message set:
//!
//! | change                                              | accuracy |
//! |-----------------------------------------------------|----------|
//! | five labels, no gloss, no history                   | 0.596    |
//! | + the previous turn in the prompt                   | 0.596    |
//! | + a one-line gloss of each label in `instructions`  | 0.750    |
//! | the same gloss moved into the schema's descriptions  | 0.654    |
//! | + "quote the words that decided it" field first     | 0.635    |
//!
//! Two of those are worth keeping in mind when editing the strings below. The
//! gloss belongs in the instructions and nowhere else — the identical words in
//! the field descriptions are worth nine points less. And a reasoning field
//! makes it *worse*, not better; there is no free chain of thought at this size.
//!
//! The remaining weakness is [`Intent::App`], which recalls 5 of 9: `"who are
//! you"` and `"what can you do"` still land in [`Intent::Social`]. Those are
//! caught by [`asked_about_us`] before the model is troubled, which is the one
//! place a fixed list is honestly the right tool — they are not a sample of a
//! category, they are the entire category.

use serde_json::{json, Value};

/// What the user wants done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A greeting, thanks, or filler with nothing in it to answer.
    Social,
    /// A question about this app or about the assistant itself.
    App,
    /// Something that needs the library, including a follow-up that needs more
    /// of it than the last answer had.
    Notes,
    /// Reword, shorten, lengthen or reformat what was just said.
    Rewrite,
    /// A question about the world, which the notes have nothing to do with.
    World,
}

/// What the router decided, and what to look up if it asked for a lookup.
#[derive(Debug, Clone)]
pub struct Route {
    pub intent: Intent,
    /// The words to search the library for. Often better than the question's
    /// own words: "what did I say about pricing last month" yields
    /// `["pricing", "last month"]` rather than every word in the sentence.
    pub terms: Vec<String>,
}

/// The five labels, glossed.
///
/// One line each, in the instructions field. The model has a budget for being
/// told things and a sixth *label* spends it — an earlier version had a separate
/// `follow_up_to_previous_answer` and scored 0.750, while folding follow-ups
/// into `question_about_notes` scored 0.808. A follow-up needs the notes again,
/// so it *is* a notes question; splitting it only gave the model a way to be
/// wrong. A later attempt at an `about_the_library` label cost more still,
/// 0.808 down to 0.739 — see [`crate::chat`], which answers those from the
/// database instead.
///
/// The `rewrite_previous_answer` line earns its extra length. Measured on 60
/// messages including eight that name a form:
///
/// | gloss for the rewrite label                       | overall | rewrite recall |
/// |---------------------------------------------------|---------|----------------|
/// | "reword, shorten, lengthen or reformat"           | 0.800   | 0.64           |
/// | + names the forms (bullets, poem, email, table)   | 0.833   | **0.93**       |
/// | + "nothing new is looked up", forms unnamed       | 0.850   | 0.64           |
/// | **both**                                          | **0.900** | **0.93**     |
///
/// Worth reading that table twice, because the middle two rows are a trap: the
/// version scoring 0.850 is the one that still gets "give me that as bullets"
/// wrong. It buys its total elsewhere. Naming the forms is what fixes the
/// rewrite class; "nothing new is looked up" is what stops the forms bleeding
/// into questions that merely *mention* an email or a table. Each alone trades
/// one failure for another; together they are strictly better than either.
const ROUTER_INSTRUCTIONS: &str = "\
You route the user's MESSAGE to one handler.
social: a greeting, thanks, or filler with nothing to answer.
about_the_app: a question about you or this app.
question_about_notes: needs the user's saved voice notes, including a follow-up \
question about the PREVIOUS ANSWER.
rewrite_previous_answer: wants the PREVIOUS ANSWER itself given again in a \
different form or length — as bullets, a paragraph, a poem, an email, a table, \
shorter, longer, or more formal. Nothing new is looked up.
general_knowledge: a question about the world.";

fn router_shape() -> Value {
    json!({
        "name": "Route",
        "fields": [
            {
                "name": "intent",
                "desc": "What the user wants",
                "anyOf": [
                    "social",
                    "about_the_app",
                    "question_about_notes",
                    "rewrite_previous_answer",
                    "general_knowledge",
                ],
            },
            {
                "name": "search_terms",
                "desc": "Keywords to look up in the notes, empty if none needed",
                "type": "[string]",
            },
        ],
    })
}

/// How much of the answer before this one the router is shown.
///
/// Enough to tell "make it shorter" from a fresh question, and no more. The
/// router is not answering anything, so a whole previous answer is tokens spent
/// on a decision that a first line already settles.
const PREVIOUS_ANSWER_SHOWN: usize = 240;

/// Questions about us, which the model reliably mistakes for small talk.
///
/// This is the one place a fixed list is the right tool, and it is worth saying
/// why, given the module docs above are an argument against exactly that.
///
/// A word list fails at *intent* because intent is open — there is no finite set
/// of ways to ask about a library of voice notes. "Is this software an
/// assistant" is closed: the phrasings are few, short, and none of them could
/// plausibly be the subject of a recording. That is the same growth rule
/// [`crate::intent`]'s lists already follow, so this reuses them rather than
/// starting a second list to drift away from the first.
///
/// It is load-bearing rather than belt-and-braces: the router's own recall on
/// this class is 5 of 9. `"who are you"` and `"what can you do"` both come back
/// as `social`, and a user who asks what the app does deserves better than
/// "You're welcome!".
fn asked_about_us(message: &str) -> bool {
    matches!(crate::intent::classify(message), crate::intent::Intent::Meta)
}

/// Read the router's reply.
fn read(reply: &Value) -> Option<Route> {
    let intent = match reply["intent"].as_str()? {
        "social" => Intent::Social,
        "about_the_app" => Intent::App,
        "question_about_notes" => Intent::Notes,
        "rewrite_previous_answer" => Intent::Rewrite,
        "general_knowledge" => Intent::World,
        // A label outside the schema's own list should be impossible under
        // constrained decoding. Treated as a question rather than trusted,
        // because the alternative is dropping a real one on the floor.
        _ => Intent::Notes,
    };

    let terms: Vec<String> = reply["search_terms"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|term| term.trim().to_string())
        .filter(|term| !term.is_empty())
        .collect();

    Some(Route { intent, terms })
}

/// Decide what this message is, using the model.
///
/// `previously` is the answer immediately before this message, if there was one.
/// Without it "make it shorter" is not a decidable sentence.
///
/// Falls back to [`crate::intent::classify`]'s word list when there is no model
/// to ask — that path is worse, and it is the difference between a degraded
/// feature and no feature at all on a Mac without Apple Intelligence.
#[cfg(target_os = "macos")]
pub fn decide(app: &tauri::AppHandle, message: &str, previously: Option<&str>) -> Route {
    if asked_about_us(message) {
        return Route { intent: Intent::App, terms: Vec::new() };
    }

    let prompt = match previously {
        Some(answer) if !answer.trim().is_empty() => format!(
            "PREVIOUS ANSWER\n{}\n\nMESSAGE\n{}",
            crate::chat::first_of(answer, PREVIOUS_ANSWER_SHOWN),
            message
        ),
        _ => format!("MESSAGE\n{message}"),
    };

    // 90 tokens is generous for `{"intent":"...","search_terms":[...]}` and
    // cheap: the router's whole cost is dominated by the ~200-token chat
    // template, not by what it writes.
    let asked = crate::brief::classify(app, ROUTER_INSTRUCTIONS, &prompt, &router_shape(), 90);

    match asked.ok().as_ref().and_then(read) {
        Some(route) => route,
        None => fallback(message),
    }
}

/// The old word list, kept for the case it was never bad at.
///
/// When the model cannot be reached at all, something still has to stop "thanks"
/// searching the library. This is measurably the worse classifier — that is why
/// it was replaced — but it costs nothing and it is right about the easy half.
fn fallback(message: &str) -> Route {
    use crate::intent::Intent as Old;
    let intent = match crate::intent::classify(message) {
        Old::Social(_) => Intent::Social,
        Old::Meta => Intent::App,
        Old::Question => Intent::Notes,
    };
    Route { intent, terms: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two the router is measurably unstable on.
    ///
    /// Across three identical runs of the 52-message set, exactly two messages
    /// changed their answer, and both were these: `"what can you do"` went
    /// SOCIAL, APP, APP and `"how does this work"` went APP, SOCIAL, APP. They
    /// have to be caught here, because the router will get them wrong about a
    /// third of the time.
    ///
    /// Everything else the list happens to know is a bonus. A phrasing it has
    /// never seen falls through to the router, which is the correct outcome and
    /// the reason this is a shortcut rather than a gate — the list is allowed to
    /// be incomplete, and completing it is not the plan.
    #[test]
    fn the_two_the_router_flips_on_never_reach_it() {
        for message in ["what can you do", "What can you do?", "how does this work"] {
            assert!(asked_about_us(message), "{message:?} must not reach the router");
        }
    }

    #[test]
    fn the_phrasings_that_started_this_are_known() {
        // Each of these was a real miss, and each is one word away from a
        // phrase that already worked.
        for message in [
            "what all can you do",
            "what kind of questions can i ask you",
            "who are you",
        ] {
            assert!(asked_about_us(message), "{message:?} should be about us");
        }
    }

    #[test]
    fn a_question_that_merely_starts_like_one_is_not_one() {
        // The failure the old word list made routine: a real question is thrown
        // away because its opening words look like small talk.
        for message in [
            "what can you do about the pricing page before Friday",
            "who are you meeting on Thursday",
            "what did Priya say in the retro",
        ] {
            assert!(!asked_about_us(message), "{message:?} is a real question");
        }
    }

    #[test]
    fn the_pre_check_only_ever_shortcuts_to_the_app() {
        // A pre-check that swallowed anything else would be the word list back
        // on the hot path, which is the thing this module exists to remove.
        // Every message that is not about the app must reach the model.
        for message in ["thanks!", "write that as a paragraph", "what did I say about pricing"] {
            assert!(!asked_about_us(message), "{message:?} must reach the router");
        }
    }

    #[test]
    fn the_router_reads_its_own_shape() {
        let reply = json!({
            "intent": "question_about_notes",
            "search_terms": ["pricing", "  Priya  ", ""],
        });
        let route = read(&reply).expect("a well-formed reply");
        assert_eq!(route.intent, Intent::Notes);
        assert_eq!(route.terms, vec!["pricing", "Priya"]);
    }

    #[test]
    fn every_label_in_the_schema_has_somewhere_to_go() {
        // The schema and the match arm are two lists that must agree, and
        // nothing else would notice if they stopped.
        let shape = router_shape();
        let labels = shape["fields"][0]["anyOf"].as_array().unwrap().clone();
        assert_eq!(labels.len(), 5);
        for label in labels {
            let reply = json!({ "intent": label, "search_terms": [] });
            let route = read(&reply).expect("a label from the schema's own list");
            // Only an unknown label is allowed to land on Notes by default.
            if label.as_str() != Some("question_about_notes") {
                assert_ne!(
                    route.intent,
                    Intent::Notes,
                    "{label} fell through to the default arm"
                );
            }
        }
    }

    #[test]
    fn a_reply_with_no_intent_is_not_a_route() {
        assert!(read(&json!({ "search_terms": ["pricing"] })).is_none());
    }

    /// The forms are named in the gloss because naming them is what took the
    /// rewrite class from 0.64 recall to 0.93, and "give me that as bullets"
    /// from re-searching the library to reformatting the answer.
    #[test]
    fn the_rewrite_gloss_still_names_the_forms_and_the_rule() {
        for form in ["bullets", "paragraph", "poem", "email", "table"] {
            assert!(ROUTER_INSTRUCTIONS.contains(form), "the gloss stopped naming {form}");
        }
        assert!(
            ROUTER_INSTRUCTIONS.contains("Nothing new is looked up"),
            "without this, a question that merely mentions an email becomes a rewrite"
        );
    }

    #[test]
    fn the_gloss_names_every_label_the_schema_offers() {
        // The gloss is worth 0.154 of accuracy and lives in a different string
        // from the labels it explains. A label added to one and not the other
        // is silent.
        let shape = router_shape();
        for label in shape["fields"][0]["anyOf"].as_array().unwrap() {
            let label = label.as_str().unwrap();
            assert!(
                ROUTER_INSTRUCTIONS.contains(label),
                "{label} is offered by the schema but never explained"
            );
        }
    }
}
