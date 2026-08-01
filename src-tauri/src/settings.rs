//! User settings.
//!
//! Kept in Rust rather than in the webview's `localStorage`, where the theme
//! lives, because the one setting here is read by the globe-key path — which
//! runs on a CGEventTap thread while another app has focus, with no window
//! necessarily open and no JavaScript running to ask.
//!
//! A whole file for one flag is deliberate: settings that only the frontend
//! needs should keep using `localStorage`, and this exists for the ones the
//! engine has to see.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Show the transcript in the overlay while the user is still speaking.
    ///
    /// **Off by default.** The preview runs on `small` — a model chosen for
    /// latency, not accuracy — and its output is thrown away the moment the real
    /// pass lands. Watching a rough draft of your own sentence appear and then
    /// silently change is worse than watching nothing: it invites you to correct
    /// something that was never going to be pasted. Anyone who wants the
    /// feedback can turn it on; nobody should be handed it unasked.
    pub live_preview: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { live_preview: false }
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

#[tauri::command]
pub fn get_settings(state: tauri::State<SettingsState>) -> Settings {
    *state.0.lock().unwrap()
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
        *guard
    };
    // The in-memory value is what dictation reads, so a failed write costs the
    // user the setting on next launch, not this one. Report it either way.
    persist(&app, &updated)?;
    Ok(updated)
}
