//! Whether to offer to take notes, and what each answer to the offer teaches.
//!
//! The meeting card used to have no memory. Every app that opened the
//! microphone got the card, every time, and nothing the user did with it was
//! remembered — so the one app it was always wrong about (somebody's voice
//! assistant, a dictation tool, Claude's own voice input) was offered again on
//! every single use. Ignoring the card was free, and therefore taught nothing.
//!
//! This is the memory. Three things feed it:
//!
//! * **Ignoring, per app.** Closing the card or letting it run out counts
//!   against the app that triggered it. Three in a row and the card stops
//!   asking about that app — and says so, once, with an undo. Per app because
//!   the case this exists for is one app that is never a call; a single shared
//!   count would let three ignored voice-assistant prompts silence the next
//!   Zoom meeting.
//! * **Taking notes.** Proof that the app holds calls, so its count clears.
//!   That is also what makes the count "in a row": three ignores spread over a
//!   month do not silence an app somebody records in every week.
//! * **Snoozing.** "Not now", for every app at once, for a while. It says
//!   nothing about any one app, so it leaves the counts alone.
//!
//! And above all of it, a switch that turns the card off entirely.
//!
//! Deliberately free of Tauri, the clock and the disk: every rule here is a
//! function of the stored state and a timestamp, which is what lets the tests
//! below pin each of them down.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How many asks an app gets before the card gives up on it.
pub const ASKS: u32 = 3;

/// What a snooze from the card lasts, before anyone picks something else.
pub const SNOOZE_MINUTES: u32 = 30;

/// The lengths offered, on the card and in the menu bar.
pub const SNOOZE_CHOICES: [u32; 4] = [15, 30, 45, 60];

/// An app the card has stopped asking about.
///
/// The name is kept beside the identifier because the identifier is what the
/// detector matches on and the name is what a person recognises in Settings —
/// `com.anthropic.claudefordesktop` is not something anybody should have to
/// read to get their prompts back.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutedApp {
    pub bundle: String,
    pub name: String,
}

/// Everything the card remembers. Stored with the rest of the settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Prompts {
    /// The switch. Off means no card for any app; recording by hand still works.
    pub enabled: bool,
    /// Consecutive ignores, by bundle identifier. Absent means zero.
    pub ignored: BTreeMap<String, u32>,
    /// Apps that reached [`ASKS`] and are no longer asked about.
    pub muted: Vec<MutedApp>,
    /// When the current snooze began, in milliseconds since the epoch.
    ///
    /// Kept so that changing the length on the card re-times the snooze from
    /// when it was pressed rather than from when the length was changed —
    /// choosing "15" a few seconds after snoozing should mean fifteen minutes
    /// of quiet, not fifteen minutes and however long the choosing took.
    pub snoozed_from: i64,
    /// When the current snooze ends. Zero, or anything in the past, is "not
    /// snoozed" — there is no separate flag to fall out of step with it.
    pub snoozed_until: i64,
}

impl Default for Prompts {
    fn default() -> Self {
        Self {
            // On, because it is what the app has always done. A new switch
            // should explain the behaviour, not quietly withdraw it.
            enabled: true,
            ignored: BTreeMap::new(),
            muted: Vec::new(),
            snoozed_from: 0,
            snoozed_until: 0,
        }
    }
}

/// What to do when an app opens the microphone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Show the card. `ask` is 1-based: which of the [`ASKS`] this is.
    Offer { ask: u32 },
    /// The switch is off.
    Off,
    /// Snoozed, for every app.
    Snoozed,
    /// This app has been ignored [`ASKS`] times in a row.
    Muted,
}

/// What an ignore did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignored {
    /// Counted. `n` is how many in a row now.
    Counted(u32),
    /// That was the last one: the app is muted, and the card should say so.
    Stopped,
}

const MINUTE_MS: i64 = 60_000;

/// Keep a requested length inside the range the card and the menu offer.
///
/// A value from elsewhere — a hand-edited settings file, a future caller —
/// should not be able to snooze for a week, or for nothing.
fn clamp_minutes(minutes: u32) -> u32 {
    minutes.clamp(SNOOZE_CHOICES[0], SNOOZE_CHOICES[SNOOZE_CHOICES.len() - 1])
}

impl Prompts {
    pub fn is_snoozed(&self, now_ms: i64) -> bool {
        self.snoozed_until > now_ms
    }

    pub fn is_muted(&self, bundle: &str) -> bool {
        self.muted.iter().any(|m| m.bundle == bundle)
    }

    /// Should the card appear for this app, right now?
    ///
    /// The order is the order of how broad each answer is: the switch covers
    /// every app forever, a snooze every app for a while, a mute one app.
    pub fn decide(&self, bundle: &str, now_ms: i64) -> Decision {
        if !self.enabled {
            return Decision::Off;
        }
        if self.is_snoozed(now_ms) {
            return Decision::Snoozed;
        }
        if self.is_muted(bundle) {
            return Decision::Muted;
        }
        let before = self.ignored.get(bundle).copied().unwrap_or(0);
        Decision::Offer {
            // Capped so a count that somehow ran past the limit — a settings
            // file from a build that had a different limit — still draws a
            // card that makes sense.
            ask: (before + 1).min(ASKS),
        }
    }

    /// The card for this app was closed, or left to run out.
    pub fn ignored(&mut self, bundle: &str, name: &str) -> Ignored {
        let n = self.ignored.get(bundle).copied().unwrap_or(0) + 1;
        if n >= ASKS {
            self.ignored.remove(bundle);
            if !self.is_muted(bundle) {
                self.muted.push(MutedApp {
                    bundle: bundle.to_string(),
                    name: name.to_string(),
                });
            }
            return Ignored::Stopped;
        }
        self.ignored.insert(bundle.to_string(), n);
        Ignored::Counted(n)
    }

    /// Notes were taken on a call in this app. It holds calls; start over.
    pub fn taken(&mut self, bundle: &str) {
        self.ignored.remove(bundle);
    }

    /// Quiet for every app, starting now.
    pub fn snooze(&mut self, minutes: u32, now_ms: i64) {
        self.snoozed_from = now_ms;
        self.snoozed_until = now_ms + clamp_minutes(minutes) as i64 * MINUTE_MS;
    }

    /// A different length for the snooze already running, counted from when it
    /// started. A no-op when nothing is snoozed: choosing a length on a card
    /// that has already expired must not quietly start a new snooze.
    pub fn retime(&mut self, minutes: u32, now_ms: i64) {
        if !self.is_snoozed(now_ms) {
            return;
        }
        self.snoozed_until = self.snoozed_from + clamp_minutes(minutes) as i64 * MINUTE_MS;
    }

    pub fn resume(&mut self) {
        self.snoozed_from = 0;
        self.snoozed_until = 0;
    }

    /// The undo on the "stopped asking" card. Back to one ignore short of the
    /// limit — the user changed their mind about the last ignore, not about
    /// the two before it.
    pub fn undo_stop(&mut self, bundle: &str) {
        self.muted.retain(|m| m.bundle != bundle);
        self.ignored.insert(bundle.to_string(), ASKS - 1);
    }

    /// Removed from the list in Settings. A deliberate fresh start, so the
    /// count goes back to zero rather than to one short.
    pub fn unmute(&mut self, bundle: &str) {
        self.muted.retain(|m| m.bundle != bundle);
        self.ignored.remove(bundle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZOOM: &str = "us.zoom.xos";
    const CLAUDE: &str = "com.anthropic.claudefordesktop";
    const NOW: i64 = 1_790_000_000_000;

    fn ignore(p: &mut Prompts, bundle: &str) -> Ignored {
        p.ignored(bundle, "Some App")
    }

    #[test]
    fn a_fresh_install_offers_and_counts_from_one() {
        let p = Prompts::default();
        assert!(p.enabled, "the card is on unless somebody turns it off");
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Offer { ask: 1 });
    }

    #[test]
    fn each_ignore_moves_the_next_ask_along() {
        let mut p = Prompts::default();
        assert_eq!(ignore(&mut p, CLAUDE), Ignored::Counted(1));
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Offer { ask: 2 });
        assert_eq!(ignore(&mut p, CLAUDE), Ignored::Counted(2));
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Offer { ask: 3 });
    }

    #[test]
    fn the_third_ignore_stops_the_card_for_that_app() {
        let mut p = Prompts::default();
        ignore(&mut p, CLAUDE);
        ignore(&mut p, CLAUDE);
        assert_eq!(ignore(&mut p, CLAUDE), Ignored::Stopped);
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Muted);
        assert_eq!(p.muted.len(), 1);
        assert_eq!(p.muted[0].name, "Some App", "the list shows a name, not an identifier");
    }

    /// The reason the count is per app.
    #[test]
    fn muting_one_app_never_silences_another() {
        let mut p = Prompts::default();
        for _ in 0..ASKS {
            ignore(&mut p, CLAUDE);
        }
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Muted);
        assert_eq!(p.decide(ZOOM, NOW), Decision::Offer { ask: 1 });
    }

    /// "In a row": a call recorded in between starts the count again.
    #[test]
    fn taking_notes_clears_the_count() {
        let mut p = Prompts::default();
        ignore(&mut p, ZOOM);
        ignore(&mut p, ZOOM);
        p.taken(ZOOM);
        assert_eq!(p.decide(ZOOM, NOW), Decision::Offer { ask: 1 });
        assert_eq!(ignore(&mut p, ZOOM), Ignored::Counted(1), "no muting on what was a third ignore");
    }

    #[test]
    fn a_snooze_quiets_every_app_and_then_ends() {
        let mut p = Prompts::default();
        p.snooze(SNOOZE_MINUTES, NOW);
        assert_eq!(p.decide(ZOOM, NOW), Decision::Snoozed);
        assert_eq!(p.decide(CLAUDE, NOW + 29 * MINUTE_MS), Decision::Snoozed);
        assert_eq!(p.decide(ZOOM, NOW + 30 * MINUTE_MS), Decision::Offer { ask: 1 });
    }

    /// Snoozing means "not now". It is not an opinion about any one app.
    #[test]
    fn a_snooze_leaves_the_counts_alone() {
        let mut p = Prompts::default();
        ignore(&mut p, CLAUDE);
        p.snooze(15, NOW);
        p.resume();
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Offer { ask: 2 });
    }

    /// Choosing a length re-times the snooze from when it was pressed.
    #[test]
    fn changing_the_length_counts_from_the_original_press() {
        let mut p = Prompts::default();
        p.snooze(SNOOZE_MINUTES, NOW);
        p.retime(15, NOW + 4_000);
        assert_eq!(p.snoozed_until, NOW + 15 * MINUTE_MS);
        p.retime(60, NOW + 9_000);
        assert_eq!(p.snoozed_until, NOW + 60 * MINUTE_MS);
    }

    #[test]
    fn a_length_chosen_after_the_snooze_ended_starts_nothing() {
        let mut p = Prompts::default();
        p.snooze(15, NOW);
        p.retime(60, NOW + 20 * MINUTE_MS);
        assert!(!p.is_snoozed(NOW + 20 * MINUTE_MS));
    }

    #[test]
    fn a_snooze_cannot_be_longer_than_an_hour_or_shorter_than_fifteen_minutes() {
        let mut p = Prompts::default();
        p.snooze(60 * 24 * 7, NOW);
        assert_eq!(p.snoozed_until, NOW + 60 * MINUTE_MS);
        p.snooze(0, NOW);
        assert_eq!(p.snoozed_until, NOW + 15 * MINUTE_MS);
    }

    #[test]
    fn resuming_ends_the_snooze_at_once() {
        let mut p = Prompts::default();
        p.snooze(60, NOW);
        p.resume();
        assert_eq!(p.decide(ZOOM, NOW), Decision::Offer { ask: 1 });
    }

    /// The undo on the card takes back the last ignore, not all three.
    #[test]
    fn undoing_a_stop_leaves_one_ask_left() {
        let mut p = Prompts::default();
        for _ in 0..ASKS {
            ignore(&mut p, CLAUDE);
        }
        p.undo_stop(CLAUDE);
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Offer { ask: ASKS });
        assert_eq!(ignore(&mut p, CLAUDE), Ignored::Stopped, "one more ignore stops it again");
    }

    /// Removing it from the list in Settings is a fresh start.
    #[test]
    fn unmuting_from_settings_starts_the_count_again() {
        let mut p = Prompts::default();
        for _ in 0..ASKS {
            ignore(&mut p, CLAUDE);
        }
        p.unmute(CLAUDE);
        assert!(p.muted.is_empty());
        assert_eq!(p.decide(CLAUDE, NOW), Decision::Offer { ask: 1 });
    }

    #[test]
    fn the_switch_beats_everything() {
        let mut p = Prompts::default();
        p.enabled = false;
        assert_eq!(p.decide(ZOOM, NOW), Decision::Off);
    }

    /// Settings files written before this existed have no `meeting_prompts`
    /// key at all, and must load as "on, nothing learned".
    #[test]
    fn an_old_settings_file_loads_as_on() {
        let p: Prompts = serde_json::from_str("{}").expect("empty object");
        assert_eq!(p, Prompts::default());
        let partial: Prompts = serde_json::from_str(r#"{"enabled": false}"#).expect("partial");
        assert!(!partial.enabled);
        assert!(partial.muted.is_empty());
    }

    #[test]
    fn a_count_left_past_the_limit_still_draws_a_sensible_card() {
        let mut p = Prompts::default();
        p.ignored.insert(ZOOM.to_string(), 7);
        assert_eq!(p.decide(ZOOM, NOW), Decision::Offer { ask: ASKS });
    }
}
