mod analytics;
mod background;
#[cfg(target_os = "macos")]
mod dictation;
mod engine;
mod export;
mod media;
#[cfg(target_os = "macos")]
mod sound;
mod store;

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::Manager;

use engine::EngineHealth;
use store::{Store, Transcript, TranscriptMeta};

/// First few words of a dictation, so history rows are scannable rather than a
/// wall of "Dictation 3".
pub fn dictation_title(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().take(7).collect();
    if words.is_empty() {
        return "Dictation".into();
    }
    let mut s = words.join(" ");
    if text.split_whitespace().count() > 7 {
        s.push('…');
    }
    s
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// -- engine ----------------------------------------------------------------

/// Answered on boot, so the window can say something useful instead of failing
/// on the first dropped file.
#[tauri::command]
fn engine_health(app: tauri::AppHandle) -> EngineHealth {
    EngineHealth {
        error: engine::missing_model(&app),
    }
}

// -- history ---------------------------------------------------------------

#[tauri::command]
fn list_transcripts(
    store: tauri::State<Store>,
    query: Option<String>,
) -> Result<Vec<TranscriptMeta>, String> {
    let conn = store.0.lock().unwrap();
    store::list(&conn, query.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_transcript(store: tauri::State<Store>, id: String) -> Result<Transcript, String> {
    let conn = store.0.lock().unwrap();
    store::get(&conn, &id).map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn save_transcript(
    app: tauri::AppHandle,
    title: String,
    source_path: String,
    duration: f64,
    language: Option<String>,
    text: String,
    paragraphs: serde_json::Value,
    segments: serde_json::Value,
    peaks: serde_json::Value,
    source: Option<String>,
    // Echoed back from the transcription result the window is saving. Optional
    // so a caller replaying an older job simply records nothing.
    model: Option<String>,
    transcribe_ms: Option<i64>,
) -> Result<String, String> {
    insert_transcript(
        &app,
        &title,
        &source_path,
        duration,
        language.as_deref(),
        &text,
        paragraphs,
        segments,
        peaks,
        source.as_deref().unwrap_or("file"),
        engine::Run {
            model: model.unwrap_or_default(),
            millis: transcribe_ms.unwrap_or(0),
        },
    )
}

#[tauri::command]
fn update_transcript(
    store: tauri::State<Store>,
    id: String,
    text: String,
    paragraphs: serde_json::Value,
) -> Result<(), String> {
    let conn = store.0.lock().unwrap();
    store::update_text(&conn, &id, &text, &paragraphs).map_err(|e| e.to_string())
}

/// Pull an older transcript's audio into the media library.
///
/// Runs on demand from the UI rather than inside `get_transcript`, because a
/// long video takes real seconds to transcode and opening a transcript should
/// never block on that.
#[tauri::command]
fn archive_transcript_media(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let (current, created) = {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        let t = store::get(&conn, &id).map_err(|e| e.to_string())?;
        (t.meta.source_path.clone(), t.meta.created_at)
    };

    let src = std::path::Path::new(&current);
    if current.contains("/media/") {
        return Ok(current); // already ours
    }
    if !src.exists() {
        return Err("the original file is no longer on disk".into());
    }

    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let stored = media::archive(&dir, &id, src, created)?;
    let stored = stored.to_string_lossy().into_owned();

    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    store::set_media_path(&conn, &id, &stored, &current).map_err(|e| e.to_string())?;
    Ok(stored)
}

/// Open the Accessibility pane. macOS shows its permission prompt only once per
/// binary, so the UI needs a way back to Settings after that.
#[tauri::command]
fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    dictation::open_accessibility_settings();
}

#[tauri::command]
fn set_transcript_peaks(
    store: tauri::State<Store>,
    id: String,
    peaks: serde_json::Value,
) -> Result<(), String> {
    let conn = store.0.lock().unwrap();
    store::set_peaks(&conn, &id, &peaks).map_err(|e| e.to_string())
}

#[tauri::command]
fn rename_transcript(
    store: tauri::State<Store>,
    id: String,
    title: String,
) -> Result<(), String> {
    let conn = store.0.lock().unwrap();
    store::rename(&conn, &id, &title).map_err(|e| e.to_string())?;
    // A name the user picked is final — keep the AI backfill away from it.
    let _ = store::mark_titled(&conn, &id);
    Ok(())
}

#[tauri::command]
fn delete_transcript(store: tauri::State<Store>, id: String) -> Result<(), String> {
    let conn = store.0.lock().unwrap();
    // Take the archived audio with it. `media::discard` only ever touches paths
    // inside the library, so a transcript still pointing at a user's own file
    // can't cause that file to be deleted.
    if let Ok(t) = store::get(&conn, &id) {
        media::discard(&t.meta.source_path);
    }
    store::delete(&conn, &id).map_err(|e| e.to_string())
}

/// The single door into the store.
///
/// Every ingest path — dropped file, mic, globe-key dictation — comes through
/// here, which is what makes the media archiving universal rather than something
/// each caller has to remember.
#[allow(clippy::too_many_arguments)]
pub fn insert_transcript(
    app: &tauri::AppHandle,
    title: &str,
    source_path: &str,
    duration: f64,
    language: Option<&str>,
    text: &str,
    paragraphs: serde_json::Value,
    segments: serde_json::Value,
    peaks: serde_json::Value,
    source: &str,
    // Which speech model ran and for how long. Default for anything that did not
    // come from the engine; the row keeps its empty columns rather than claiming
    // a zero-millisecond transcription.
    run: engine::Run,
) -> Result<String, String> {
    let created = now_ms();
    let id = format!("{:x}", created as u128 * 1000 + rand_suffix());

    // Take ownership of the audio before recording where it lives. If this
    // fails we still save, pointing at the original — a transcript with awkward
    // playback beats losing the transcript.
    let stored = app
        .path()
        .app_data_dir()
        .ok()
        .and_then(|dir| {
            media::archive(&dir, &id, std::path::Path::new(source_path), created).ok()
        })
        .map(|p| p.to_string_lossy().into_owned());
    let playable = stored.as_deref().unwrap_or(source_path);

    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    store::insert(
        &conn,
        &id,
        title,
        playable,
        duration,
        language,
        created,
        text,
        &paragraphs,
        &segments,
        &peaks,
        source,
        source_path,
    )
    .map_err(|e| e.to_string())?;
    if run.measured() {
        // Best-effort, like the app label: the note is saved, and losing a
        // timing is not worth failing an ingest over.
        let _ = store::set_engine_run(&conn, &id, &run.model, run.millis);
    }
    drop(conn);

    // The title the caller chose — a cleaned-up filename, or a dictation's
    // opening words — is the title. Nothing renames a note behind the user's
    // back, so mark the row titled and leave it alone.
    {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        let _ = store::mark_titled(&conn, &id);
    }

    Ok(id)
}

/// Mirror of the frontend's `titleFromPath` so ingested files are named the
/// same way dropped ones are.
pub fn title_from_path(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    let cleaned = stem
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        base.to_string()
    } else {
        cleaned
    }
}

/// Cheap unique-ish suffix so two saves in the same millisecond don't collide.
fn rand_suffix() -> u128 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    (RandomState::new().build_hasher().finish() % 1000) as u128
}

// -- misc ------------------------------------------------------------------

#[tauri::command]
fn write_text_file(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents).map_err(|e| e.to_string())
}

/// Persist a microphone recording and hand back its path.
///
/// Recordings live alongside the database rather than in a temp dir: the
/// transcript keeps a `source_path` and the player reads from it, so the audio
/// has to outlive the session that produced it.
#[tauri::command]
fn save_recording(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
    extension: String,
) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("recordings");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let ext = if extension.chars().all(|c| c.is_ascii_alphanumeric()) && !extension.is_empty() {
        extension
    } else {
        "webm".to_string()
    };
    let path = dir.join(format!("recording-{}.{}", now_ms(), ext));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // Launch at login, toggled from the tray. macos_launcher::AppleScript
        // registers a login item rather than a LaunchAgent plist, so the user
        // can see and remove it in System Settings like any other app.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::AppleScript,
            None,
        ))
        .manage(engine::EngineState::default())
        .manage(dictation::DictationState::default());

    builder
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let conn = store::open(&dir)?;
            app.manage(Store(Mutex::new(conn)));

            // Launch the native dictation-overlay helper. The pill is drawn by a
            // separate accessory process because a Tauri webview window cannot
            // float over another app's full-screen Space; a native NSPanel can.
            #[cfg(target_os = "macos")]
            dictation::prepare_overlay(app.handle());

            // The menu-bar presence, so closing the window doesn't take the
            // globe key down with it.
            if let Err(e) = background::install_tray(app.handle()) {
                eprintln!("[tray] could not install: {e}");
            }

            // …which is precisely why the model has to be given back when it
            // isn't in use: living in the menu bar means "not quitting" is the
            // normal state, so a model held until quit is a model held all day.
            engine::start_idle_unload(app.handle().clone());

            // Off the main thread so the window paints immediately.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                #[cfg(target_os = "macos")]
                {
                    dictation::spawn(handle.clone());
                    // Clear scratch captures abandoned by a previous run.
                    dictation::spawn_sweep(handle.clone());
                }
            });

            Ok(())
        })
        .on_window_event(|_window, event| {
            // Closing the main window puts the app in the menu bar rather than
            // exiting: dictation is a background feature and must survive it.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if _window.label() == "main" && background::hide_instead_of_quit(_window) {
                    api.prevent_close();
                    return;
                }
            }
            if let tauri::WindowEvent::Destroyed = event {
                #[cfg(target_os = "macos")]
                dictation::stop_overlay();
            }
        })
        .invoke_handler(invoke_handler())
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Tear the transcription model down before the process exits.
            //
            // Quitting goes through `-[NSApplication terminate:]` -> `exit()`,
            // which runs C static destructors but drops nothing Rust owns — so
            // Tauri's managed `EngineState`, and the `WhisperContext` inside it,
            // are still alive when ggml-metal's teardown runs. Its last act is
            //
            //   GGML_ASSERT([rsets->data count] == 0)
            //
            // asserting that every Metal residency set has been released. A live
            // context still holds them, the assert fails, ggml calls abort(),
            // and macOS shows "VoiceDumps quit unexpectedly" on what the user
            // experienced as a normal quit. It only happens once a model has
            // been loaded, which is why closing the app before transcribing
            // anything looks fine.
            //
            // Dropping the context here releases the residency sets while we
            // still control the order, and the assert passes.
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                app.state::<engine::EngineState>().unload();
            }
        });
}

/// The command surface.
fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        engine_health,
        list_transcripts,
        get_transcript,
        save_transcript,
        update_transcript,
        set_transcript_peaks,
        archive_transcript_media,
        export::export_pdf,
        open_accessibility_settings,
        engine::start_transcription,
        engine::transcribe_peaks,
        engine::engine_status,
        engine::engine_unload,
        rename_transcript,
        delete_transcript,
        write_text_file,
        save_recording,
        analytics::analytics_summary,
    ]
}
