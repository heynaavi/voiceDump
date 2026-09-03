//! User settings.
//!
//! Kept in Rust rather than in the webview's `localStorage`, where the theme
//! lives, because everything here is read by the globe-key path — which runs on
//! a CGEventTap thread while another app has focus, with no window necessarily
//! open and no JavaScript running to ask.
//!
//! That is the whole membership rule: settings only the frontend needs should
//! keep using `localStorage`, and this exists for the ones the engine has to
//! see.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Show the transcript in the overlay while the user is still speaking.
    ///
    /// **On by default in the full build, off in the lite one** — the one
    /// setting whose default is deliberately not the same in both, which is why
    /// it hangs off the feature flag rather than off a constant.
    ///
    /// The preview runs on `small`, a model chosen for latency and not for
    /// accuracy, and its output is thrown away the moment the real pass lands.
    /// Handing that to someone unasked invites them to correct a sentence that
    /// was never going to be pasted, so the lite build ships it off.
    ///
    /// The full build had it running unconditionally before this setting
    /// existed. A new switch should explain what the app does, not quietly
    /// withdraw it, so there the default is what was happening yesterday and
    /// the row is the way to disagree.
    pub live_preview: bool,

    /// The microphone to record from, by name.
    ///
    /// `None` means "whatever macOS is set to", which is the default and stays
    /// a legitimate answer rather than an unset value: someone who switches
    /// their system input expects dictation to follow, and pinning a name at
    /// first launch would quietly break that. See [`crate::microphone`] for why
    /// this is a name and not an identifier.
    pub microphone: Option<String>,

    /// The keys held down to dictate, as `+`-joined modifier names.
    ///
    /// A string rather than a mask so the file stays legible, and so the one
    /// place that understands the encoding is [`crate::shortcut`]. Validated on
    /// the way in; anything unparseable that reaches here anyway is ignored at
    /// arming time rather than disabling dictation.
    pub shortcut: String,

    /// Put names to the voices in recordings that have only one track.
    ///
    /// **On by default**, which is a change and worth stating the cost of. It
    /// used to be off on the grounds that the case it exists for — one
    /// microphone, several people in a room — had never been measured, and
    /// that switching it on costs a 42 MB download nobody should pay for
    /// silently. Both of those are still true. What changed is the judgement
    /// about who should carry them: somebody who drops a recording of three
    /// people into this app wants the names, and finding a switch first is a
    /// worse trade than a download they only pay for once.
    ///
    /// What *has* been measured stays worth knowing: a clean four-voice fixture
    /// at 93-99%, and a two-track call at 70.9%. The second of those is a case
    /// this must never run on anyway, since the two sides were recorded
    /// separately and already know who spoke.
    ///
    /// The download is still not silent and still not paid by everybody. It
    /// happens on the first recording that could actually have several voices
    /// in it — see `wants_speakers`, which excludes dictations and meetings —
    /// so a person who only ever dictates never fetches a model at all.
    ///
    /// This only reaches somebody whose settings file has no opinion yet, which
    /// is a fresh install. Anybody who has ever saved a setting has a stored
    /// `false` and keeps it, deliberately: a default is what to do absent an
    /// answer, not licence to overrule one.
    pub diarization: bool,

    /// Whether the dictation chord has to be held down for the whole recording.
    ///
    /// **On by default, because it is what the app has always done** and a
    /// setting should explain the behaviour rather than quietly change it.
    ///
    /// Holding is the safer of the two — the microphone is live for exactly as
    /// long as your finger is, and there is no state to lose track of. It is
    /// also the one that hurts on a long dictation, and the one you cannot do
    /// while your other hand is on the mouse. Off, the chord is a switch:
    /// press and release to start, press and release again to stop.
    ///
    /// A switch can be left on, which holding cannot — see
    /// [`crate::dictation::LATCHED_LIMIT`] for what stops that becoming an
    /// overnight recording.
    pub hold_to_talk: bool,

    /// Whether whatever was on the clipboard goes back after a dictation.
    ///
    /// **On by default**, again because it is the existing behaviour: the text
    /// is pasted through the clipboard, and putting back what it displaced is
    /// what makes the clipboard a place you can keep something across a
    /// dictation rather than a channel this happens to use.
    ///
    /// Off, the transcript stays there — which is what you want if the paste is
    /// the start of what you are doing with the words rather than the end of
    /// it, or if it landed somewhere that mangled it and you want to try again
    /// with a plain paste. The tray's "Copy Last Transcript" covers that case
    /// either way; this makes it the default for people who reach for it every
    /// time.
    pub restore_clipboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            live_preview: cfg!(feature = "assistant"),
            microphone: None,
            shortcut: crate::shortcut::DEFAULT.to_string(),
            diarization: true,
            hold_to_talk: true,
            restore_clipboard: true,
        }
    }
}

/// The in-memory copy, so the dictation path never touches the disk.
pub struct SettingsState(pub Mutex<Settings>);

fn file(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("settings.json"))
}

/// Read from disk at startup. Anything unreadable or half-written falls back to
/// defaults rather than refusing to launch.
pub fn load(app: &tauri::AppHandle) -> Settings {
    file(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn persist(app: &tauri::AppHandle, s: &Settings) -> Result<(), String> {
    let path = file(app).ok_or("no settings directory")?;
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Whether the live preview is on right now.
///
/// Reads the cached copy and takes the default if the lock is poisoned — this is
/// called from the key-down path, where blocking or panicking would take the
/// dictation down with it.
pub fn live_preview(app: &tauri::AppHandle) -> bool {
    app.try_state::<SettingsState>()
        .and_then(|s| s.0.lock().ok().map(|g| g.live_preview))
        .unwrap_or_else(|| Settings::default().live_preview)
}

/// The microphone the user picked, if they picked one.
///
/// Same contract as [`live_preview`]: called from the capture thread, so a
/// poisoned lock gives back the default instead of taking the recording down.
pub fn microphone(app: &tauri::AppHandle) -> Option<String> {
    app.try_state::<SettingsState>()
        .and_then(|s| s.0.lock().ok().map(|g| g.microphone.clone()))
        .unwrap_or_default()
}

/// Whether to look for speakers in single-track recordings.
pub fn diarization(app: &tauri::AppHandle) -> bool {
    app.try_state::<SettingsState>()
        .and_then(|s| s.0.lock().ok().map(|g| g.diarization))
        .unwrap_or_default()
}

/// Whether to put the previous clipboard contents back after pasting.
///
/// Same contract as [`live_preview`]: read from the dictation path, so a
/// poisoned lock gives back the default rather than taking the paste down.
pub fn restore_clipboard(app: &tauri::AppHandle) -> bool {
    app.try_state::<SettingsState>()
        .and_then(|s| s.0.lock().ok().map(|g| g.restore_clipboard))
        .unwrap_or_else(|| Settings::default().restore_clipboard)
}

/// Turn speaker labelling on or off.
///
/// On, a recording brought in as a file is labelled by itself once its
/// transcript is saved — see `wants_speakers`, which is where the judgement
/// about *which* recordings lives. Dictations are excluded there rather than
/// here: holding a key to talk is one voice, and it is 94% of a real library,
/// so an automatic pass over them would cost nearly everything and learn
/// nearly nothing.
///
/// The switch does two things, and it is worth being clear that it is both.
/// It reveals the SPEAKERS button on every note, which is how a dictation or
/// an old recording gets labelled despite being excluded above — "usually
/// pointless" is not "never wanted". And it turns on the automatic pass for
/// the recordings where it usually is wanted. Off, neither happens and nothing
/// is downloaded.
///
/// Fetching the models is that action's job, not this one's: a settings write
/// that blocks on a 42 MB download would look like the switch was broken.
#[tauri::command]
pub fn set_diarization(
    app: tauri::AppHandle,
    state: tauri::State<SettingsState>,
    enabled: bool,
) -> Result<Settings, String> {
    let updated = {
        let mut guard = state.0.lock().unwrap();
        guard.diarization = enabled;
        guard.clone()
    };
    persist(&app, &updated)?;
    Ok(updated)
}

/// Choose between holding the chord and switching it.
///
/// The tap is re-pointed before the write, for the same reason
/// [`set_shortcut`] does it: the keyboard should agree with the switch on the
/// very next press, whether or not the file lands.
#[tauri::command]
pub fn set_hold_to_talk(
    app: tauri::AppHandle,
    state: tauri::State<SettingsState>,
    enabled: bool,
) -> Result<Settings, String> {
    crate::shortcut::arm_hold(enabled);
    let updated = {
        let mut guard = state.0.lock().unwrap();
        guard.hold_to_talk = enabled;
        guard.clone()
    };
    persist(&app, &updated)?;
    Ok(updated)
}

/// Choose whether a dictation costs you your clipboard.
#[tauri::command]
pub fn set_restore_clipboard(
    app: tauri::AppHandle,
    state: tauri::State<SettingsState>,
    enabled: bool,
) -> Result<Settings, String> {
    let updated = {
        let mut guard = state.0.lock().unwrap();
        guard.restore_clipboard = enabled;
        guard.clone()
    };
    persist(&app, &updated)?;
    Ok(updated)
}

#[tauri::command]
pub fn get_settings(state: tauri::State<SettingsState>) -> Settings {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_live_preview(
    app: tauri::AppHandle,
    state: tauri::State<SettingsState>,
    enabled: bool,
) -> Result<Settings, String> {
    let updated = {
        let mut guard = state.0.lock().unwrap();
        guard.live_preview = enabled;
        guard.clone()
    };
    // The in-memory value is what dictation reads, so a failed write costs the
    // user the setting on next launch, not this one. Report it either way.
    persist(&app, &updated)?;
    Ok(updated)
}

/// Pick a microphone by name, or `None` to follow the system input.
///
/// Nothing is checked against the attached devices here. The list the user
/// chose from was accurate when it was drawn, and a name that stops resolving
/// later is handled where it matters — at the moment of recording, by falling
/// back to the default. Rejecting it here would only mean a device that is
/// briefly asleep cannot be chosen.
#[tauri::command]
pub fn set_microphone(
    app: tauri::AppHandle,
    state: tauri::State<SettingsState>,
    name: Option<String>,
) -> Result<Settings, String> {
    let updated = {
        let mut guard = state.0.lock().unwrap();
        guard.microphone = name.filter(|n| !n.is_empty());
        guard.clone()
    };
    persist(&app, &updated)?;
    Ok(updated)
}

/// Choose the keys that start a dictation.
///
/// Rejected rather than coerced if it isn't a chord we can watch for: the
/// alternative is storing something the tap will silently ignore, leaving a
/// picker that says one thing and a keyboard that does another. The tap is
/// re-pointed before the write, so the new chord works on the next keypress
/// whether or not the file lands.
#[tauri::command]
pub fn set_shortcut(
    app: tauri::AppHandle,
    state: tauri::State<SettingsState>,
    chord: String,
) -> Result<Settings, String> {
    let chord = crate::shortcut::canonical(&chord)
        .ok_or_else(|| format!("{chord:?} is not a chord this build can watch for"))?;
    crate::shortcut::arm(&chord);
    let updated = {
        let mut guard = state.0.lock().unwrap();
        guard.shortcut = chord;
        guard.clone()
    };
    persist(&app, &updated)?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every other default is shared, and that is the point — one tree, two
    /// products. This one is not, so each build asserts its own concrete value
    /// rather than restating the `cfg!` and proving nothing.
    #[cfg(feature = "assistant")]
    #[test]
    fn the_full_build_leaves_the_preview_on() {
        assert!(Settings::default().live_preview);
    }

    #[cfg(not(feature = "assistant"))]
    #[test]
    fn the_lite_build_ships_the_preview_off() {
        assert!(!Settings::default().live_preview);
    }

    #[test]
    fn both_builds_start_on_the_globe_key_and_the_system_input() {
        let d = Settings::default();
        assert_eq!(d.shortcut, crate::shortcut::DEFAULT);
        assert_eq!(d.microphone, None);
    }
}
