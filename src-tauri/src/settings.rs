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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            live_preview: cfg!(feature = "assistant"),
            microphone: None,
            shortcut: crate::shortcut::DEFAULT.to_string(),
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
