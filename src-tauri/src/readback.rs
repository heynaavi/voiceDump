//! Seeing what happened to the words after they were pasted.
//!
//! Dictation hands its text to somebody else's app and walks away, which means
//! the one moment a person tells us we got a word wrong — when they fix it, in
//! Slack, in Mail, in whatever they were typing in — happens where we cannot
//! see. This looks, briefly, at the field the text went into.
//!
//! **What it reads, and what it keeps.** Through Accessibility, the permission
//! the dictation key already needs: the focused field's text, once before the
//! paste and a few times in the half-minute after. It finds the words it pasted
//! by the text on either side of them — which the person is not editing — and
//! compares only that stretch with what was pasted. The field's text is never
//! stored and never leaves this function; what survives is the pairs
//! [`crate::vocabulary::fixes_between`] accepts, and nothing else.
//!
//! **What it refuses.** Password fields, by role, before reading anything.
//! Fields too large to be something a person is composing — a terminal's
//! scrollback, a code editor's whole file. And any field it cannot find its own
//! words in again: a rewrite, a cleared box, a paste that landed somewhere else.
//! Each of those ends the watch without learning, which is always safe.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};

type AXUIElementRef = CFTypeRef;
type AXError = i32;

const AX_SUCCESS: AXError = 0;
/// `kAXValueTypeCFRange`, from `AXValue.h`.
const AX_VALUE_CFRANGE: u32 = 4;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> AXError;
    fn AXValueGetValue(value: CFTypeRef, the_type: u32, value_ptr: *mut c_void) -> bool;
}

#[repr(C)]
#[derive(Default)]
struct CFRange {
    location: isize,
    length: isize,
}

/// The largest field worth watching, in UTF-16 units.
///
/// A message, an email, a document someone is writing are all well under this.
/// A terminal's scrollback or an editor's entire file are not, and diffing them
/// every two seconds is work that could never find anything a person corrected.
const MAX_FIELD: usize = 50_000;

/// How long an unresponsive app may hold up a single read.
const MESSAGING_TIMEOUT: f32 = 0.25;

/// When the paste has certainly landed.
const SETTLE: Duration = Duration::from_millis(700);

/// How often the field is looked at, and for how long.
///
/// Short enough to see a fix before a chat message is sent and its box emptied;
/// long enough that a person rereading what they dictated has time to correct it.
const EVERY: Duration = Duration::from_millis(1500);
const FOR: Duration = Duration::from_secs(45);

/// How many unchanged readings mean the person has stopped typing.
///
/// What they fixed is learned then, rather than at the end of the watch, so the
/// next thing they dictate is already spelled right. Waiting for the full
/// three-quarters of a minute meant a correction made in the first five seconds
/// still missed the dictation made in the twentieth.
const SETTLED_FOR: u32 = 2;

/// How much longer than what was pasted the field may grow and still be a
/// correction of it.
///
/// A person fixing a word changes a few characters. A person dictating a second
/// time into the same note doubles the text — and when both dictations say
/// almost the same thing, which is exactly what someone testing the app does,
/// every word of the new one is a word the old one had, so overlap alone cannot
/// tell them apart.
const MOST_GROWTH: f64 = 1.5;

/// Which dictation is the current one.
///
/// A watch speaks only for the paste it was started for. The moment the next
/// dictation begins, whatever appears in the field is that one's words, not a
/// fix to these — so older watches keep what they had already read and stop.
static DICTATION: AtomicU64 = AtomicU64::new(0);

fn latest() -> u64 {
    DICTATION.load(Ordering::SeqCst)
}

/// One Accessibility attribute, retained.
fn attribute(element: &CFType, name: &str) -> Option<CFType> {
    let key = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    let status = unsafe {
        AXUIElementCopyAttributeValue(element.as_CFTypeRef(), key.as_concrete_TypeRef(), &mut value)
    };
    (status == AX_SUCCESS && !value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
}

fn text_of(element: &CFType) -> Option<Vec<u16>> {
    let value = attribute(element, "AXValue")?;
    let s = value.downcast::<CFString>()?;
    Some(s.to_string().encode_utf16().collect())
}

fn selection_of(element: &CFType) -> Option<(usize, usize)> {
    let value = attribute(element, "AXSelectedTextRange")?;
    let mut range = CFRange::default();
    let ok = unsafe {
        AXValueGetValue(value.as_CFTypeRef(), AX_VALUE_CFRANGE, &mut range as *mut CFRange as *mut c_void)
    };
    (ok && range.location >= 0 && range.length >= 0)
        .then_some((range.location as usize, range.length as usize))
}

fn is_secure(element: &CFType) -> bool {
    [attribute(element, "AXRole"), attribute(element, "AXSubrole")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.downcast::<CFString>())
        .any(|s| s.to_string() == "AXSecureTextField")
}

fn focused() -> Option<CFType> {
    let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
    let element = attribute(&system, "AXFocusedUIElement")?;
    unsafe { AXUIElementSetMessagingTimeout(element.as_CFTypeRef(), MESSAGING_TIMEOUT) };
    Some(element)
}

/// The field, just before the words go into it.
pub struct Before {
    element: CFType,
    /// The text on either side of where the paste goes.
    prefix: Vec<u16>,
    suffix: Vec<u16>,
    /// Which dictation this is — see [`DICTATION`].
    nth: u64,
}

// The element is only ever read, from one thread at a time, and Accessibility
// references are safe to use from a thread other than the one that made them.
unsafe impl Send for Before {}

/// Look at the focused field before pasting. `None` means: do not watch.
pub fn before_paste() -> Option<Before> {
    // Counted before anything can turn this into `None`: a dictation into a
    // password field still ends the watch on the last one, because its words are
    // about to land somewhere and none of them are a fix to what came before.
    let nth = DICTATION.fetch_add(1, Ordering::SeqCst) + 1;
    let element = focused()?;
    if is_secure(&element) {
        return None;
    }
    let text = text_of(&element)?;
    if text.len() > MAX_FIELD {
        return None;
    }
    let (at, replacing) = selection_of(&element)?;
    let end = at.checked_add(replacing)?;
    if end > text.len() {
        return None;
    }
    Some(Before {
        element,
        prefix: text[..at].to_vec(),
        suffix: text[end..].to_vec(),
        nth,
    })
}

/// Whether the field holds more than a corrected version of what went into it.
fn grew_past(known: &str, now: &str) -> bool {
    let was = known.split_whitespace().count() as f64;
    let is = now.split_whitespace().count() as f64;
    is > was * MOST_GROWTH + 2.0
}

/// The stretch between two anchors, if the field still has both.
///
/// The anchors are the text the person is not editing. When they are still at
/// either end of the field, whatever lies between them is what became of the
/// words that were pasted — typed over, fixed, or left alone.
fn between_anchors(current: &[u16], prefix: &[u16], suffix: &[u16]) -> Option<Vec<u16>> {
    if current.len() < prefix.len() + suffix.len() {
        return None;
    }
    if !current.starts_with(prefix) || !current.ends_with(suffix) {
        return None;
    }
    Some(current[prefix.len()..current.len() - suffix.len()].to_vec())
}

/// Whether what stands where the paste was is still recognisably the paste.
///
/// Below this, the person has replaced the words rather than corrected them,
/// and whatever changed is theirs, not a mishearing to learn from.
fn still_ours(pasted: &str, now: &str) -> bool {
    let a: std::collections::HashSet<String> = pasted.split_whitespace().map(str::to_lowercase).collect();
    let b: Vec<String> = now.split_whitespace().map(str::to_lowercase).collect();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let shared = b.iter().filter(|w| a.contains(*w)).count();
    shared as f64 / a.len().max(b.len()) as f64 >= 0.5
}

/// Keep what changed between one reading of the field and the next.
fn learn_from(
    app: &tauri::AppHandle,
    known: &str,
    now: &str,
    target: Option<&str>,
    sure: &[(String, f32)],
) {
    let fixes = crate::vocabulary::fixes_between(known, now);
    if fixes.is_empty() {
        return;
    }
    use tauri::Manager;
    let store = app.state::<crate::store::Store>();
    let Ok(conn) = store.0.lock() else { return };
    crate::vocabulary::learn_sure(
        &conn,
        &fixes,
        crate::vocabulary::Source::Corrected,
        target,
        crate::now_ms(),
        sure,
    );
    eprintln!("[readback] learned {} word(s) from {}", fixes.len(), target.unwrap_or("another app"));
}

/// Watch the field after pasting, and learn from what was fixed.
///
/// Returns at once; the watch runs on its own thread and ends by itself.
///
/// `sure` is the sentences just transcribed and how sure the model was of each,
/// so a correction can record what the model thought of the speech it got
/// wrong — see [`crate::vocabulary::learn_sure`].
pub fn watch(
    app: tauri::AppHandle,
    before: Before,
    pasted: String,
    target: Option<String>,
    sure: Vec<(String, f32)>,
) {
    std::thread::spawn(move || {
        // Moved in whole. A 2021-edition closure captures the fields it touches
        // one by one, and the element on its own is not `Send` — the snapshot
        // around it is the thing that promises to stay on this one thread.
        let before = before;
        std::thread::sleep(SETTLE);
        let began = Instant::now();
        // What the field is already known to say: what was pasted, and then
        // whatever has been learned since. A second fix a minute later is read
        // against the first rather than against the transcript.
        let mut known = pasted;
        let mut last: Option<String> = None;
        let mut steady = 0u32;

        while began.elapsed() < FOR {
            let Some(text) = text_of(&before.element) else { break };
            if text.len() > MAX_FIELD {
                break;
            }
            // The anchors are gone: sent, cleared, or edited around. What was
            // read last is the most anybody can say.
            let Some(region) = between_anchors(&text, &before.prefix, &before.suffix) else { break };
            let now = String::from_utf16_lossy(&region);
            // Asked after the reading rather than before it: the next dictation
            // pastes within a moment of being counted, and a reading taken
            // across that moment is already showing its words.
            if latest() != before.nth {
                break;
            }
            if !still_ours(&known, &now) || grew_past(&known, &now) {
                break;
            }

            if last.as_deref() == Some(now.as_str()) {
                steady += 1;
                if steady >= SETTLED_FOR && now != known {
                    learn_from(&app, &known, &now, target.as_deref(), &sure);
                    known = now.clone();
                }
            } else {
                steady = 0;
            }
            last = Some(now);
            std::thread::sleep(EVERY);
        }

        // Ended before the typing ever settled — the next dictation began, the
        // message was sent, the three-quarters of a minute ran out.
        if let Some(finally) = last {
            if finally != known {
                learn_from(&app, &known, &finally, target.as_deref(), &sure);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// The field still has what was around the paste: the middle is the paste.
    #[test]
    fn the_paste_is_found_between_what_was_around_it() {
        let got = between_anchors(&u("Hi team, send it to Shaun today. Thanks"), &u("Hi team, "), &u(" Thanks"));
        assert_eq!(got, Some(u("send it to Shaun today.")));
    }

    /// A sent message empties its box; nothing can be said about it after that.
    #[test]
    fn an_emptied_field_has_no_paste_in_it() {
        assert_eq!(between_anchors(&u(""), &u("Hi team, "), &u(" Thanks")), None);
    }

    /// Editing the text around the paste loses the anchor, and the watch ends.
    #[test]
    fn an_edit_outside_the_paste_ends_the_watch() {
        assert_eq!(between_anchors(&u("Hello team, send it."), &u("Hi team, "), &u("")), None);
    }

    /// Emoji and accented names are UTF-16 in Accessibility, not bytes.
    #[test]
    fn anchors_survive_text_that_is_not_ascii() {
        let got = between_anchors(&u("Siobhán 👋 send it to Shaun"), &u("Siobhán 👋 "), &u(""));
        assert_eq!(got, Some(u("send it to Shaun")));
    }

    #[test]
    fn a_corrected_paste_is_still_ours() {
        assert!(still_ours("send the contract to Sean today", "send the contract to Shaun today"));
    }

    #[test]
    fn a_replaced_paste_is_not() {
        assert!(!still_ours("send the contract to Sean today", "never mind, call me later"));
    }

    /// The incident this guard is named for. Three dictations of nearly the same
    /// sentence into one note, the first one's watch still reading — so what
    /// stood in the field was two more dictations, and diffing against them
    /// invented a correction of a name into a misspelling of itself.
    #[test]
    fn a_field_that_grew_by_another_dictation_is_not_a_correction() {
        let pasted = "Hi, my name is Navi also known as Navin Upadhyaya.";
        let grown = "Hi, my name is Navi also known as Naveen Upadhyay. \
                     My name is Navi also known as Naveen Upadhyaya \
                     My name is Navi also known as Navin Padhyay.";
        assert!(still_ours(pasted, grown), "every word of the new ones was a word of the old");
        assert_eq!(
            crate::vocabulary::fixes_between(pasted, grown)
                .into_iter()
                .map(|f| (f.heard, f.term))
                .collect::<Vec<_>>(),
            vec![("Upadhyaya".to_string(), "Padhyay".to_string())],
            "which is exactly what it learned, before this guard existed",
        );
        assert!(grew_past(pasted, grown));
    }

    /// Fixing a word is not growing.
    #[test]
    fn a_corrected_sentence_has_not_grown() {
        assert!(!grew_past("send the contract to Sean today", "send the contract to Shaun today"));
    }

    /// Neither is finishing the sentence you dictated the start of.
    #[test]
    fn a_few_words_added_are_still_watched() {
        assert!(!grew_past("send it to Shaun", "send it to Shaun before five, thanks"));
    }
}
