//! Which keys you hold to dictate.
//!
//! The globe key was hard-coded here for as long as dictation has existed, and
//! it is still the default — but it is not on every keyboard, some people have
//! already given it to something else, and a key you press by accident reaching
//! for the arrows is a poor push-to-talk button. So the chord is a setting.
//!
//! **Modifiers only.** Not a limitation we ran into so much as the shape of the
//! problem: [`crate::dictation`] watches the keyboard through a *listen-only*
//! event tap, which sees keys without consuming them. A chord of pure modifiers
//! is invisible to whatever app has focus — holding ⌃⌥ types nothing and means
//! nothing — so listening is enough. The moment a letter joins the chord that
//! stops being true: ⌃⌥D would start dictating *and* send ⌃⌥D to the editor
//! underneath. Swallowing it would mean routing every keystroke on the machine
//! through this process, which is a much larger promise than a shortcut picker
//! ought to make. Push-to-talk is a modifier's job anyway — macOS's own
//! dictation is a double-tap of Control.
//!
//! The chord is stored as a `+`-joined string ("control+option") because it
//! goes in the same JSON as everything else and has to survive being read by a
//! human. It is turned into a bitmask once, at the edge, and compared as an
//! integer on the tap thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// `CGEventFlags` bits. Named here rather than pulled from `core-graphics` so
// this module builds and tests on any host.
const FN: u64 = 0x0080_0000;
const SHIFT: u64 = 0x0002_0000;
const CONTROL: u64 = 0x0004_0000;
const OPTION: u64 = 0x0008_0000;
const COMMAND: u64 = 0x0010_0000;

/// Every bit this module has an opinion about.
///
/// Flags outside this mask — caps lock, the numeric-keypad bit, the
/// device-dependent left/right bits — are deliberately ignored, so a chord
/// still matches with caps lock on.
pub const MODIFIERS: u64 = FN | SHIFT | CONTROL | OPTION | COMMAND;

/// What dictation has always used, and what an unreadable setting falls back to.
pub const DEFAULT: &str = "globe";

/// Spelling → bit. The order is the order a canonical chord is written in,
/// which is the order the keys sit on the keyboard, left to right.
const KEYS: [(&str, u64); 5] = [
    ("globe", FN),
    ("control", CONTROL),
    ("option", OPTION),
    ("shift", SHIFT),
    ("command", COMMAND),
];

/// The mask a chord asks to see held down.
///
/// `None` for anything unrecognised, and for the empty chord — a chord of no
/// keys would match the moment every modifier was released, i.e. constantly.
pub fn mask(chord: &str) -> Option<u64> {
    let mut bits = 0u64;
    for part in chord.split('+') {
        let name = part.trim().to_ascii_lowercase();
        let (_, bit) = KEYS.iter().find(|(n, _)| *n == name)?;
        bits |= bit;
    }
    (bits != 0).then_some(bits)
}

/// The chord written the one way we agree to write it, or `None` if it isn't a
/// chord we will let anyone choose. Used to validate at the command boundary,
/// so what reaches the JSON is always something [`mask`] will accept on the
/// next launch.
///
/// Stricter than [`mask`] in one way: a lone ordinary modifier is refused. ⌘ is
/// held down at the start of every ⌘-key shortcut there is, so "dictate while ⌘
/// is down" would start recording on ⌘C and stop on release — dozens of times
/// an hour. Two keys is the smallest chord nobody presses by accident. The
/// globe key is the exception, because not being part of anyone else's
/// shortcuts is exactly what made it the default.
pub fn canonical(chord: &str) -> Option<String> {
    let bits = mask(chord)?;
    if bits != FN && bits.count_ones() < 2 {
        return None;
    }
    let names: Vec<&str> = KEYS
        .iter()
        .filter(|(_, b)| bits & b != 0)
        .map(|(n, _)| *n)
        .collect();
    Some(names.join("+"))
}

/// The mask the tap is currently watching for.
///
/// An atomic rather than the settings mutex: this is read inside the event-tap
/// callback, which runs on every modifier press machine-wide and must never
/// block on a lock another thread is holding.
static HELD: AtomicU64 = AtomicU64::new(FN);

/// Point the tap at a chord. Anything unparseable leaves it where it was, so a
/// hand-edited settings file cannot leave the app with no way to dictate.
pub fn arm(chord: &str) {
    if let Some(bits) = mask(chord) {
        HELD.store(bits, Ordering::Relaxed);
    }
}

/// Whether `flags` mean "this chord is down".
///
/// Exact equality across [`MODIFIERS`], not a subset test: ⌃⌥ must not fire
/// while the user is holding ⌃⌥⌘ for something else entirely.
fn matches(flags: u64, chord: u64) -> bool {
    flags & MODIFIERS == chord
}

/// Whether these event flags mean "the dictation chord is down".
pub fn is_held(flags: u64) -> bool {
    matches(flags, HELD.load(Ordering::Relaxed))
}

// -- hold, or switch --------------------------------------------------------

/// Whether the chord has to stay down for the whole dictation.
///
/// Holding is the default and stays it: the key is down for exactly as long as
/// you are talking, so there is no state to remember and no way to leave the
/// microphone running by walking away. It is also the harder thing to do on a
/// laptop with one hand busy, and a long dictation with the globe key held is
/// genuinely uncomfortable — which is the whole reason the other mode exists.
///
/// The alternative is a switch: one press starts, the next one stops, and the
/// releases in between mean nothing. See [`crate::dictation::spawn`] for how
/// the two edges are read differently.
///
/// An `AtomicBool` for the same reason [`HELD`] is an atomic — this is read
/// inside the event-tap callback, which runs on every modifier press on the
/// machine and must never wait on a lock.
static HOLD: AtomicBool = AtomicBool::new(true);

/// Tell the tap which of the two the chord is. Called at startup from the
/// stored settings, and again whenever the switch is flipped.
pub fn arm_hold(hold: bool) {
    HOLD.store(hold, Ordering::Relaxed);
}

/// Whether the chord is push-to-talk right now.
pub fn is_hold_to_talk() -> bool {
    HOLD.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holding_is_what_the_tap_assumes_until_told_otherwise() {
        // The stored setting arms this at startup; a build that never got that
        // far must still behave the way dictation always has.
        assert!(is_hold_to_talk());
        arm_hold(false);
        assert!(!is_hold_to_talk());
        arm_hold(true);
    }

    #[test]
    fn the_default_is_the_globe_key() {
        assert_eq!(mask(DEFAULT), Some(FN));
    }

    #[test]
    fn a_chord_is_the_keys_it_names() {
        assert_eq!(mask("control+option"), Some(CONTROL | OPTION));
    }

    #[test]
    fn order_and_case_are_not_part_of_the_chord() {
        assert_eq!(mask("Option+Control"), mask("control+option"));
        assert_eq!(canonical("option+control").as_deref(), Some("control+option"));
    }

    #[test]
    fn nonsense_is_not_a_chord() {
        assert_eq!(mask(""), None);
        assert_eq!(mask("banana"), None);
        assert_eq!(mask("control+"), None);
        // A letter is refused rather than silently dropped: see the module note
        // on why a listen-only tap cannot own one.
        assert_eq!(mask("control+d"), None);
    }

    #[test]
    fn a_lone_modifier_cannot_be_chosen() {
        // ⌘ is down for the first moment of every ⌘-key shortcut.
        assert_eq!(canonical("command"), None);
        assert_eq!(canonical("control"), None);
        // Two keys is fine, and so is the globe key on its own.
        assert_eq!(canonical("command+shift").as_deref(), Some("shift+command"));
        assert_eq!(canonical("globe").as_deref(), Some("globe"));
    }

    #[test]
    fn a_held_chord_is_matched_exactly() {
        let chord = CONTROL | OPTION;
        assert!(matches(CONTROL | OPTION, chord));
        // One of the two on its own is not the chord.
        assert!(!matches(CONTROL, chord));
        // Neither is the chord plus a key the user is holding for something else.
        assert!(!matches(CONTROL | OPTION | COMMAND, chord));
    }

    #[test]
    fn flags_we_do_not_care_about_do_not_break_a_match() {
        const CAPS_LOCK: u64 = 0x0001_0000;
        assert!(matches(FN | CAPS_LOCK, FN));
    }

    /// `arm` and `is_held` share one process-wide atomic, so they are exercised
    /// in a single test rather than three that would race each other.
    #[test]
    fn arming_repoints_the_tap_and_survives_nonsense() {
        arm("command");
        assert!(is_held(COMMAND));
        assert!(!is_held(FN));

        // An unreadable chord leaves the previous one in place, so a
        // hand-edited settings file cannot leave the app unable to dictate.
        arm("banana");
        assert!(is_held(COMMAND));

        arm(DEFAULT);
        assert!(is_held(FN));
    }
}
