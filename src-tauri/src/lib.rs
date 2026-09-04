mod analytics;
mod background;
// Overviews from the model macOS 26 ships. macOS-only because that model is.
#[cfg(target_os = "macos")]
mod brief;
#[cfg(target_os = "macos")]
mod chat;
mod clipboard;
#[cfg(target_os = "macos")]
mod dictation;
// Putting names to the voices in one track, behind the `find_speakers`
// command. See `docs/speaker-diarization.md`.
mod diarize;
mod engine;
mod export;
mod graph;
// What a message is, before anything is looked up. Not macOS-gated: it is a
// word list and a loop, and the chat that calls it is the only macOS part.
//
// No longer the thing that decides what somebody meant — see `route`, which
// asks the model. What is left here is the part a list is genuinely good at:
// the closed set of ways to ask what this app is, and which pleasantry to
// answer a pleasantry with.
mod intent;
mod media;
// Where a message goes, decided by the model under a schema rather than by
// matching words. macOS-only: it is a model call.
#[cfg(target_os = "macos")]
mod route;
// Both halves of a call. macOS-only for the same reason dictation is: it rests
// on a CoreAudio tap that exists nowhere else.
#[cfg(target_os = "macos")]
mod meeting;
mod microphone;
mod models;
mod settings;
mod shortcut;
mod sidecar;
#[cfg(target_os = "macos")]
mod sound;
mod store;
// "Is there a newer version?" — one of only two places in the app that speak to
// the network, and the only one that speaks to it more than once. See the
// module for why it checks rather than installs.
mod update;

// Everything the standalone build does without. Transcription, dictation,
// recording, history and the reading UI all live outside this boundary, so the
// lite app is the same app with the network integrations and the brain removed
// — not a fork that has to be maintained twice.
#[cfg(feature = "assistant")]
mod discord;
#[cfg(feature = "assistant")]
mod knowledge;
#[cfg(feature = "assistant")]
mod slack;

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::Manager;
// Both builds emit now: the public one announces an overview being saved,
// which a meeting starts on its own with nobody awaiting a promise.
use tauri::Emitter;

use sidecar::{SidecarState, SidecarStatus};
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

// -- sidecar ---------------------------------------------------------------

/// The running version, from the bundle rather than anything on disk.
///
/// Both builds. The version is what the update check compares against, and the
/// lite build is the one that ships to people who have to notice a release
/// themselves.
#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

#[cfg(feature = "assistant")]
#[tauri::command]
fn sidecar_status(state: tauri::State<SidecarState>) -> SidecarStatus {
    SidecarStatus {
        port: *state.port.lock().unwrap(),
        error: state.error.lock().unwrap().clone(),
    }
}

/// The lite build has no sidecar at all. The window still asks "is the engine
/// usable?" on boot, so answer the question it's actually asking.
#[cfg(not(feature = "assistant"))]
#[tauri::command]
fn sidecar_status(app: tauri::AppHandle) -> SidecarStatus {
    SidecarStatus {
        port: None,
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
        true,
    )
}

/// Show a note's recording in Finder.
///
/// The original is preferred when it is still there: revealing the file somebody
/// dropped in is more useful than revealing our copy of it. But `origin_path` is
/// a record of where a recording *came from*, not a promise that it is still
/// there — a dictation's temporary WAV is deleted the moment it has been
/// archived, which is every dictation ever made. Reaching for it and giving up
/// when it is missing is why this button did nothing at all.
///
/// So both are tried, in order of usefulness, and a note whose audio is genuinely
/// gone says so instead of failing silently.
#[tauri::command]
fn reveal_source(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let (origin, archived) = {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        let t = store::get(&conn, &id).map_err(|e| e.to_string())?;
        (t.meta.origin_path.clone(), t.meta.source_path.clone())
    };

    for candidate in [origin, archived] {
        if candidate.is_empty() || !std::path::Path::new(&candidate).exists() {
            continue;
        }
        // `-R` reveals the file in its folder rather than opening it, and the
        // absolute path is deliberate: a bundled app inherits launchd's PATH,
        // not a login shell's. See `media.rs` for the time that assumption cost
        // us a release.
        return std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&candidate)
            .status()
            .map_err(|e| e.to_string())
            .and_then(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err("Finder would not open that folder".into())
                }
            });
    }

    Err("that recording is no longer on disk".into())
}

/// Put names to the voices in one note, and save the result.
///
/// Deliberately on demand rather than at ingest. It costs a 42 MB download the
/// first time and a minute or two of work after that, and it is wrong for most
/// notes — a dictation has one voice in it by definition. Making it a thing
/// somebody asks for keeps it away from the notes it cannot help.
///
/// Meetings are refused outright. Their two sides were recorded separately, so
/// who spoke is a fact already in the database; replacing it with a 70.9% guess
/// would be a downgrade, and no threshold fixes that.
/// One sentence made out of the words on somebody's card.
///
/// Asked for by the Insights panel just before it records the reel, and by
/// nothing else. `words` is what is actually on the card — the user has already
/// struck out anything they did not want on it, and this must never see a word
/// they hid.
///
/// Blocking work on the blocking pool: this spawns the helper process and waits
/// on a generation, which is a second or two.
#[tauri::command]
async fn cloud_sentence(app: tauri::AppHandle, words: Vec<String>) -> Result<String, String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || brief::sentence(&handle, &words))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn find_speakers(app: tauri::AppHandle, id: String) -> Result<usize, String> {
    // Blocking work off the async runtime: a download, then a decode.
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || label_speakers(&handle, &id))
        .await
        .map_err(|e| e.to_string())?
}

/// One labelling job at a time, whoever asked for it.
///
/// Two reasons, and the first one shipped as a bug. The models are fetched
/// lazily by whoever needs them first, so two jobs starting together meant two
/// downloads of the same 40 MB file into the same `.part` — see
/// `diarize::FETCHING`. And past that, a second job is a second ONNX session
/// and a second decode of a whole recording held in memory, to do work that
/// gains nothing by overlapping. The queue is the optimisation, not a
/// limitation.
pub(crate) static LABELLING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Say where a note's labelling has got to.
fn speaker_stage(app: &tauri::AppHandle, id: &str, stage: &str) {
    let _ = app.emit(
        "speakers-progress",
        serde_json::json!({ "id": id, "stage": stage }),
    );
}

/// Put names to the voices in one recording, in place.
///
/// Blocking, and shared by the two ways this happens: the SPEAKERS button on a
/// note, and the automatic pass [`spawn_speaker_job`] makes on a new one. They
/// were one function that had grown a command wrapper, and splitting them here
/// rather than duplicating the save is what keeps a hand-run and an automatic
/// run from ever disagreeing about what a labelled note looks like.
fn label_speakers(app: &tauri::AppHandle, id: &str) -> Result<usize, String> {
    let (source, audio, origin, mut segments) = {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        let t = store::get(&conn, id).map_err(|e| e.to_string())?;
        (
            t.meta.source.clone(),
            t.meta.source_path.clone(),
            t.meta.origin_path.clone(),
            t.segments.clone(),
        )
    };

    if source == "meeting" {
        return Err("this meeting already recorded who was speaking".into());
    }
    if !std::path::Path::new(&audio).exists() {
        return Err("that recording is no longer on disk".into());
    }

    // A pass may already be done. `diarize::start_early` starts one the moment a
    // recording arrives, so for anything that came in through the app the
    // listening has been happening alongside the transcription and the answer is
    // waiting here — no queue, no second decode, and the note's first appearance
    // already carries its labels.
    //
    // Looked for before `LABELLING` is touched, which is what keeps this from
    // deadlocking: the early pass holds that lock while it runs, so a note that
    // took it first and then waited for its own pass would wait for ever.
    let turns = match diarize::take_early(&origin) {
        Some(turns) => {
            // Said out loud because it is otherwise invisible. The whole point
            // of the early pass is that nothing appears on screen — no queue,
            // no LISTENING… — so from the outside a note that was labelled
            // ahead of time and one that waited its turn look identical, and
            // the only way to know which happened is to be told.
            eprintln!("[speakers] {id}: collected a pass that ran during transcription");
            turns
        }
        None => {
            // Queued behind any job already running — including its download,
            // which is the whole point. `LABELLING` is held to the end of this
            // arm, which is the last thing in it that needs it.
            if LABELLING.try_lock().is_err() {
                speaker_stage(app, id, "queued");
            }
            let _turn = LABELLING.lock().unwrap_or_else(|e| e.into_inner());

            if !diarize::ready(app) {
                speaker_stage(app, id, "downloading");
                let handle = app.clone();
                let note = id.to_string();
                diarize::fetch_or_wait(app, &move |at| {
                    let _ = handle.emit(
                        "speakers-progress",
                        serde_json::json!({
                            "id": note,
                            "stage": if at.verifying { "verifying" } else { "downloading" },
                            "received": at.received,
                            "total": at.total,
                            "index": at.index,
                            "count": at.count,
                        }),
                    );
                })?;
            }

            speaker_stage(app, id, "listening");
            eprintln!("[speakers] {id}: listening now — no pass was waiting");
            diarize::run(app, &audio)?
        }
    };

    // Marked before the outcome is known, and on every path out from here: the
    // point of the flag is "we looked", not "we found somebody". A one-voice
    // recording writes no labels, so without this it stays a backfill candidate
    // for ever and the sweep never finishes.
    {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        let _ = store::mark_speakers_checked(&conn, id);
    }

    if !diarize::worth_labelling(&turns) {
        return Ok(0);
    }
    let labelled = diarize::label_segments(&mut segments, &turns);
    if labelled == 0 {
        return Ok(0);
    }

    // Re-grouped from the segments rather than patched in place, so the reading
    // view gets turns that break where the speaker changes — and the flat text
    // rewritten with them, because that string is what an overview reads. The
    // same three things a rename rewrites, for the same reason.
    let list: Vec<serde_json::Value> = segments.as_array().cloned().unwrap_or_default();
    let paragraphs = meeting::turns(&list);

    // The turns are what get saved, so they are what has to be judged — not the
    // diarizer's raw clusters, which is what `worth_labelling` already checked.
    // Those two can disagree: a 32-second dictation came back with two clusters,
    // survived that check, and after grouping was a single turn labelled
    // "Speaker 2" — one voice, named as though it were the second of several,
    // with no first anywhere. Labels that say nothing are worse than none,
    // because they claim a recording had people in it.
    let named: std::collections::HashSet<&str> = paragraphs
        .iter()
        .filter_map(|t| t["speaker"].as_str())
        .collect();
    if named.len() < 2 {
        return Ok(0);
    }

    let text = meeting::transcript_text(&paragraphs);

    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    store::set_conversation(&conn, id, &text, &serde_json::json!(paragraphs), &segments)
        .map_err(|e| e.to_string())?;
    Ok(diarize::speaking_order(&turns).len())
}

/// Go back over notes saved before this feature existed, one at a time.
///
/// The same sweep the AI titler does for names, and for the same reason: a
/// feature that only ever applies to what you record next is invisible to
/// somebody who already has a library. On the machine this was written for
/// that was 565 notes and nine hours of audio, which is the other half of the
/// design — it must be impossible to notice.
///
/// So it is deliberately slow. One note at a time behind [`LABELLING`], which
/// the live path also holds, so anything recorded now goes first and the sweep
/// simply waits its turn. [`BETWEEN_BACKFILL`] then idles between notes, which
/// costs nothing anybody is waiting for and keeps a fan quiet.
///
/// Resumable, because nine hours will not finish in one sitting: each note is
/// marked as looked-at the moment the diarizer has been over it, whatever it
/// found, so a restart picks up where it stopped rather than starting again.
///
/// Newest first — if it only ever gets halfway, this morning's recording is
/// likelier to be opened again than one from March.
fn spawn_speaker_backfill(app: &tauri::AppHandle) {
    if !settings::diarization(app) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // Behind the models. Starting the sweep first would mean 565 notes each
        // waiting on the same download, and the first one holding `LABELLING`
        // while it did — which is exactly the queue that made this feature look
        // broken in the first place.
        // Bounded. If the models never arrive — the setting is on but the
        // download keeps failing — this thread should end rather than wake up
        // every twenty seconds for the life of the process. The next launch
        // tries again, which is soon enough for a sweep over old notes.
        let mut patience = 90; // half an hour
        while !diarize::ready(&app) {
            patience -= 1;
            if patience == 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(20));
        }

        let waiting = {
            let store = app.state::<Store>();
            let conn = store.0.lock().unwrap();
            store::list_unlabelled(&conn, LONG_ENOUGH_FOR_TWO).unwrap_or_default()
        };
        if waiting.is_empty() {
            return;
        }
        let minutes: f64 = waiting.iter().map(|(_, secs)| secs / 60.0).sum();
        eprintln!(
            "[speakers] backfilling {} note(s), {minutes:.0} minutes of audio",
            waiting.len()
        );

        let mut labelled = 0usize;
        for (id, _) in &waiting {
            match label_speakers(&app, id) {
                Ok(n) if n > 0 => {
                    labelled += 1;
                    let _ = app.emit(
                        "speakers-found",
                        serde_json::json!({ "id": id, "speakers": n }),
                    );
                }
                Ok(_) => {}
                // One unreadable recording must not end the sweep — the audio
                // may simply be gone, which is ordinary for an old note. And
                // nothing ran, so there is nothing to rest after: a library
                // with a hundred missing files would otherwise spend five
                // minutes asleep to discover that.
                Err(why) => {
                    eprintln!("[speakers] {id}: {why}");
                    continue;
                }
            }
            std::thread::sleep(BETWEEN_BACKFILL);
        }
        eprintln!("[speakers] backfill done; {labelled} note(s) gained labels");
    });
}

/// How long the backfill rests between notes.
///
/// Long enough that the sweep is background noise rather than a machine that
/// has become busy for reasons its owner did not ask for. It makes the whole
/// pass take longer, which costs nothing: nobody is waiting on a note they
/// recorded in March.
const BETWEEN_BACKFILL: std::time::Duration = std::time::Duration::from_secs(3);

/// Fetch the speaker models at launch, so nobody waits for them mid-recording.
///
/// The Whisper models are fetched up front by [`crate::models`] because the app
/// cannot transcribe without them — there is a setup screen and you wait at it.
/// These are not like that: everything works without them, so blocking first
/// run on an optional 40 MB would be a worse trade than the one it prevents.
///
/// The trade it *does* prevent is the one that shipped: the first recording
/// somebody dropped in started a download nobody had been told about, took
/// eight minutes, and looked exactly like a feature hanging. Fetching quietly
/// at launch means that by the time a recording needs them they are already
/// there, which is what "downloaded already when you update" has to mean.
///
/// Only when the setting is on, and only when something is actually missing —
/// so this is a directory listing and nothing else on almost every launch.
fn spawn_model_prefetch(app: &tauri::AppHandle) {
    if !settings::diarization(app) || diarize::ready(app) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let report = |at: diarize::Fetching| {
            let _ = app.emit(
                "speakers-progress",
                serde_json::json!({
                    "stage": if at.verifying { "verifying" } else { "downloading" },
                    "received": at.received,
                    "total": at.total,
                    "index": at.index,
                    "count": at.count,
                }),
            );
        };
        match diarize::fetch_or_wait(&app, &report) {
            Ok(()) => eprintln!("[speakers] models ready"),
            // Never a dialog and never a retry loop. The next recording that
            // wants them tries again, and until then nothing is worse than it
            // was — the app has simply not got an optional model yet.
            Err(why) => eprintln!("[speakers] models not fetched: {why}"),
        }
    });
}

/// The shortest recording that could hold a conversation.
///
/// Not a cost dodge — with the models already on disk a pass is a second or so
/// — but an honest floor. Two people cannot take a turn each inside a few
/// seconds, so "testing the mic, testing the mic" at 2.5s is a run whose answer
/// is known before it starts.
pub(crate) const LONG_ENOUGH_FOR_TWO: f64 = 15.0;

/// Which recordings are worth looking for speakers in without being asked.
///
/// Everything transcribed that is long enough to hold a conversation. That is
/// the whole rule now, and it is deliberately broader than it was: the first
/// version ran only on files, on the grounds that a dictation is one person
/// holding a key and 94% of a real library, so a pass over them would cost
/// almost everything to learn almost nothing.
///
/// That reasoning was about the *download*, which used to be triggered by
/// whoever needed the models first. It is not any more — `spawn_model_prefetch`
/// gets them at launch — and once they are local a pass is cheap and its answer
/// is usually "one voice", which changes nothing and shows nothing. Being wrong
/// about a dictation that did have two people in it is the more expensive
/// mistake, because there is nothing on screen to suggest looking.
///
/// Meetings stay out, and firmly. Their two sides were recorded separately, so
/// who spoke is a fact there rather than a guess, and `label_speakers` refuses
/// them outright as well — this is the cheaper of the two refusals, not the
/// only one.
fn wants_speakers(source: &str, duration: f64) -> bool {
    source != "meeting" && duration >= LONG_ENOUGH_FOR_TWO
}

/// Look for speakers in a note that has just been saved, in the background.
///
/// Never blocks the save and never surfaces an error. The note is complete
/// without labels, the models may not be downloaded yet, and a recording with
/// one voice in it is the ordinary outcome rather than a failure — so this
/// reports what it found and otherwise says nothing.
fn spawn_speaker_job(app: &tauri::AppHandle, id: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = app.emit("speakers-looking", serde_json::json!({ "id": id }));
        let found = label_speakers(&app, &id);
        match found {
            // Zero is the answer for most recordings: one voice, nothing to
            // label, and the note is left exactly as it was.
            Ok(0) => {
                let _ = app.emit("speakers-found", serde_json::json!({ "id": id, "speakers": 0 }));
            }
            Ok(n) => {
                eprintln!("[speakers] {id}: labelled {n} voice(s)");
                let _ = app.emit("speakers-found", serde_json::json!({ "id": id, "speakers": n }));
            }
            Err(why) => {
                eprintln!("[speakers] {id}: {why}");
                let _ = app.emit("speakers-found", serde_json::json!({ "id": id, "speakers": 0 }));
            }
        }
    });
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
    let ours = current.contains("/media/");

    // Ours *and* playable is the finished state. Ours but not playable is a
    // library entry written by a build that fell back to a raw copy, and it can
    // be re-encoded from itself — the bytes are still here and symphonia reads
    // the containers the webview won't play.
    if ours && media::is_playable(&current) {
        return Ok(current);
    }
    if !src.exists() {
        return Err("the original file is no longer on disk".into());
    }

    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let stored = media::archive(&dir, &id, src, created)?;
    let stored = stored.to_string_lossy().into_owned();

    // Repairing in place leaves the unplayable original behind in our own
    // directory; nothing will ever read it again.
    if ours && stored != current {
        media::discard(&current);
    }

    // Only a path outside the library is an origin. Re-encoding one of our own
    // files must not record the file we just deleted as "where it came from" —
    // that is precisely the dangling path that made REVEAL SOURCE do nothing.
    let origin = if ours { "" } else { current.as_str() };

    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    store::set_media_path(&conn, &id, &stored, origin).map_err(|e| e.to_string())?;
    Ok(stored)
}

/// Read a note's audio again and replace what it says.
///
/// The way out when a transcript is wrong. Speech recognition is not a thing
/// that either works or doesn't — it degrades, and it degrades quietly, and the
/// person who can tell is the one who was in the room. They had no move to make
/// except delete the note and lose the recording with it.
///
/// The audio is the durable thing here. Every note keeps its own copy in the
/// media library, so this costs nothing but the minutes the model takes.
///
/// One thing it cannot give back, and the window says so before asking: a
/// meeting's two sides are mixed down to a single track when it is saved, so a
/// re-read of a meeting comes back as one voice. The words return; who said
/// them does not.
#[tauri::command]
async fn transcribe_again(app: tauri::AppHandle, id: String) -> Result<(), String> {
    // Minutes of decoding. Same reasoning as a meeting finishing: not on the
    // thread that paints.
    tauri::async_runtime::spawn_blocking(move || read_it_again(&app, &id))
        .await
        .map_err(|e| format!("the transcription could not be started: {e}"))?
}

fn read_it_again(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let audio = {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        store::get(&conn, id).map_err(|e| e.to_string())?.meta.source_path
    };
    if audio.is_empty() || !std::path::Path::new(&audio).exists() {
        return Err(
            "The recording this note was made from is no longer on disk, so there is \
nothing to read again."
                .into(),
        );
    }

    let announce = |stage: &str, fraction: f64| {
        let _ = app.emit(
            "retranscribe-progress",
            serde_json::json!({ "id": id, "stage": stage, "progress": fraction }),
        );
    };

    // Nothing is written until this returns. A failed re-read leaves the note
    // exactly as it was, which matters more here than anywhere else in the app:
    // the transcript being replaced is the only copy of it.
    let result = engine::transcribe(app, &audio, |stage, fraction| announce(stage, fraction))?;

    let text = result["text"].as_str().unwrap_or_default();
    if text.trim().is_empty() {
        return Err("Reading that recording again produced no words at all, so the \
existing transcript has been left alone."
            .into());
    }

    {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        store::replace_transcript(
            &conn,
            id,
            text,
            &result["paragraphs"],
            &result["segments"],
        )
        .map_err(|e| e.to_string())?;
        let run = engine::Run::from_result(&result);
        if run.measured() {
            let _ = store::set_engine_run(&conn, id, &run.model, run.millis);
        }
    }

    // Said before the overview is written, because the overview takes its own
    // tens of seconds and the transcript is already there to read.
    announce("Done", 1.0);
    let _ = app.emit("retranscribe-done", id);

    // The old overview went with the old transcript. Writing the new one is
    // best-effort — without Apple Intelligence there is nothing to write with,
    // and a note with a transcript and no overview is a working note.
    let _ = write_brief(app, id);
    Ok(())
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

/// Give one side of a meeting a name, everywhere it appears.
///
/// The tap hears the whole far side as one stream, so the app calls it "Others"
/// — honest, and useless the moment you want to know who agreed to do
/// something. A person watching the transcript knows exactly who was talking,
/// and this is the cheapest way to get that knowledge into the note.
///
/// Returns the whole transcript rather than an acknowledgement: every paragraph
/// and every segment changed, and the window would otherwise have to re-fetch
/// what this function is already holding.
/// Vet a name somebody typed above a paragraph.
///
/// Separate from the command so what is and is not allowed can be stated as a
/// test rather than inferred from the window's behaviour. What is allowed is
/// nearly everything: people have two, three and four-part names, and the only
/// characters refused are the ones that would change what the transcript *means*
/// when it is read back.
fn check_speaker_name(to: &str) -> Result<String, String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("A speaker needs a name.".into());
    }
    // The transcript is written as "Name: what they said", and that is what the
    // overview reads. A name carrying a colon or a newline would split a turn
    // into two speakers the next time anything read it back. Spaces are fine —
    // "Marcus Chen" is a name, not two of them.
    if to.contains([':', '\n', '\r']) {
        return Err("A name can't contain a colon or a line break.".into());
    }
    if to.chars().count() > 40 {
        return Err("That name is too long to sit above a paragraph.".into());
    }
    Ok(to.to_string())
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn rename_speaker(
    store: tauri::State<Store>,
    id: String,
    from: String,
    to: String,
) -> Result<store::Transcript, String> {
    let to = check_speaker_name(&to)?;
    let to = to.as_str();

    let conn = store.0.lock().unwrap();
    let existing = store::get(&conn, &id).map_err(|e| e.to_string())?;
    let (paragraphs, segments, text) =
        meeting::relabel(&existing.paragraphs, &existing.segments, &from, to)
            .ok_or_else(|| format!("Nobody in this recording is called \"{from}\" any more."))?;

    store::set_conversation(&conn, &id, &text, &paragraphs, &segments)
        .map_err(|e| e.to_string())?;

    // The overview named an owner because the transcript did. Carrying the
    // rename into it is what makes this worth doing at all — an action item
    // owned by "Others" is a reminder that somebody, somewhere, agreed to
    // something. Only the owner field, and only on an exact match: rewriting
    // the prose would mean editing sentences the model wrote, and a summary
    // that has been quietly reworded is worse than one that is a little stale.
    let brief = relabel_owners(&existing.brief, &from, to);
    if brief != existing.brief {
        store::set_brief(&conn, &id, &brief).map_err(|e| e.to_string())?;
    }

    store::get(&conn, &id).map_err(|e| e.to_string())
}

/// The names spoken in a recording, to offer when naming a speaker.
///
/// Not just meetings, despite where this started: since the automatic speaker
/// pass began labelling ordinary notes, a dictation or a dropped file can carry
/// "Speaker 1" and "Speaker 2" too, and those are the labels most worth
/// replacing — at least "Others" says something. The old name said `meeting` and
/// was wrong about half its callers.
///
/// Asked for only when somebody opens the rename control, never on save. Most
/// notes are never renamed, and a model call per note to answer a question
/// nobody asked is exactly the kind of cost the overview is deliberate about not
/// paying either.
///
/// An empty list is a normal answer — plenty of calls go by without anyone
/// saying a name — and so is an error, when Apple Intelligence is off. Both mean
/// the same thing to the window: type it yourself.
#[cfg(target_os = "macos")]
#[tauri::command]
async fn names_in_transcript(app: tauri::AppHandle, id: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (text, labels) = {
            let store = app.state::<Store>();
            let conn = store.0.lock().unwrap();
            let note = store::get(&conn, &id).map_err(|e| e.to_string())?;
            let labels = note
                .paragraphs
                .as_array()
                .map(|turns| {
                    let mut seen: Vec<String> = Vec::new();
                    for turn in turns {
                        if let Some(who) = turn["speaker"].as_str() {
                            if !seen.iter().any(|s| s == who) {
                                seen.push(who.to_string());
                            }
                        }
                    }
                    seen
                })
                .unwrap_or_default();
            (note.text, labels)
        };
        brief::people(&app, &text, &labels)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Ask the library a question, and answer it out of the library.
///
/// Slow — retrieval is instant, the model is not — so it runs off the UI thread
/// like every other call that ends at Apple Intelligence.
/// Slow — retrieval is instant, the model is not — so it runs off the UI thread
/// like every other call that ends at Apple Intelligence.
///
/// The turn is written down here rather than by the window, so that what is
/// remembered is what actually happened: a window that asked, got an answer and
/// was closed before it could report back would otherwise lose the turn, and a
/// failure would never be recorded at all.
#[cfg(target_os = "macos")]
#[tauri::command]
async fn ask_library(app: tauri::AppHandle, question: String) -> Result<chat::Answer, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let asked = chat::ask(&app, &question);

        let (answer, problem) = match &asked {
            Ok(answer) => (serde_json::to_value(answer).unwrap_or_default(), String::new()),
            Err(problem) => (serde_json::Value::Null, problem.clone()),
        };
        {
            let store = app.state::<Store>();
            let conn = store.0.lock().unwrap();
            // Best-effort: an answer somebody is looking at is worth more than
            // the record of it, so a write failure never fails the ask.
            let _ = chat::remember(&conn, &question, &answer, &problem, now_ms());
        }
        asked
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Everything asked so far, oldest first.
#[cfg(target_os = "macos")]
#[tauri::command]
fn chat_history(app: tauri::AppHandle) -> Result<Vec<chat::StoredTurn>, String> {
    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    chat::history(&conn).map_err(|e| e.to_string())
}

/// Throw the conversation away.
///
/// Necessary rather than a nicety: a log of everything somebody has asked about
/// their own recordings, kept forever with no way to clear it, is a liability
/// dressed as a feature.
#[cfg(target_os = "macos")]
#[tauri::command]
fn forget_chat(app: tauri::AppHandle) -> Result<(), String> {
    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    chat::forget_all(&conn).map_err(|e| e.to_string())
}

/// Turn a recording into words and hand them back, without keeping it.
///
/// The chat's microphone. Everything else that transcribes in this app is
/// making a note — it archives the audio, writes a row, names it, summarises it.
/// This one is a *text field*: what you said was a question, the answer is the
/// thing worth keeping, and a library full of one-line questions is not a
/// library. The scratch file goes with it.
#[tauri::command]
async fn transcribe_once(app: tauri::AppHandle, path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let heard = engine::transcribe(&app, &path, |_, _| {});
        // Removed whether or not it transcribed: a failed question is still not
        // something to leave lying in the application-support directory.
        let _ = std::fs::remove_file(&path);

        let heard = heard?;
        Ok(heard["text"].as_str().unwrap_or_default().trim().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What one note is about — the chips under its title.
#[tauri::command]
fn note_topics(app: tauri::AppHandle, id: String) -> Result<Vec<graph::Node>, String> {
    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    graph::mentions_of(&conn, &id).map_err(|e| e.to_string())
}

/// What the library keeps coming back to.
#[tauri::command]
fn library_topics(
    app: tauri::AppHandle,
    kind: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<graph::Node>, String> {
    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    graph::top_nodes(&conn, kind.as_deref(), limit.unwrap_or(50)).map_err(|e| e.to_string())
}

/// The notes to read to answer a question about something.
///
/// The retrieval half of "chat with your data", exposed on its own so it can be
/// used — and checked — before there is any chat to put it behind. Returns whole
/// notes rather than snippets: a summary is already short, and an answer built
/// from fragments of one is how a grounded answer stops being grounded.
#[tauri::command]
fn notes_about(
    app: tauri::AppHandle,
    term: String,
    limit: Option<usize>,
) -> Result<Vec<store::Transcript>, String> {
    let store_state = app.state::<Store>();
    let conn = store_state.0.lock().unwrap();

    let mut ids: Vec<String> = Vec::new();
    for node in graph::lookup(&conn, &term, 5).map_err(|e| e.to_string())? {
        for id in graph::notes_about(&conn, node.id, limit.unwrap_or(8)).map_err(|e| e.to_string())?
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }

    Ok(ids.iter().filter_map(|id| store::get(&conn, id).ok()).collect())
}

#[cfg(target_os = "macos")]
fn relabel_owners(brief: &serde_json::Value, from: &str, to: &str) -> serde_json::Value {
    let mut brief = brief.clone();
    if let Some(items) = brief
        .get_mut("action_items")
        .and_then(serde_json::Value::as_array_mut)
    {
        for item in items {
            if item["owner"].as_str() == Some(from) {
                item["owner"] = serde_json::json!(to);
            }
        }
    }
    brief
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
/// Every ingest path — dropped file, mic, Discord, dictation — comes through
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
    // When false the caller has already settled the title (e.g. Discord, which
    // names its reply synchronously), so we neither fire the async namer nor let
    // the backfill touch it.
    auto_title: bool,
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

    // Name the note from its own content, in the background. What was said is
    // almost always a better title than a filename or a dictation's first seven
    // words. Never blocks the save — on any failure the fallback title we just
    // stored simply stands.
    // Voices, in the background, when the setting is on and the recording is
    // long enough to have more than one in it. On by default — see
    // `settings::Settings::diarization`.
    if settings::diarization(app) && wants_speakers(source, duration) {
        spawn_speaker_job(app, id.clone());
    }

    if auto_title {
        // A meeting is the exception, and it names itself later. It briefs
        // itself as it saves, and that overview has read the whole call; naming
        // it here would instead spend a model call on the first few minutes of
        // a two-hour conversation. Until then it keeps the time it happened,
        // which is a real answer rather than a placeholder.
        if source != "meeting" && should_ai_title(source, text.split_whitespace().count()) {
            spawn_title_job(app, id.clone(), text.to_string());
        }
    } else {
        // Caller owns the title already; keep the backfill away from it.
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        let _ = store::mark_titled(&conn, &id);
    }

    // …and summarise it, if it is the kind of note worth summarising. Meetings
    // did this on their own and nothing else did, which meant a recording you
    // dropped in — the other way a long conversation gets into this app — sat
    // there with a button on it waiting to be pressed.
    if should_brief(source, text.split_whitespace().count()) {
        brief_in_background(app, &id);
    }

    Ok(id)
}

/// Whether a note earns an overview without being asked.
///
/// Length is the whole test, and it is a different question from whether a note
/// deserves a title. A title says what a note is; an overview saves you reading
/// it. Below the floor there is nothing to save — the summary comes out longer
/// than the note, and the note was already one glance.
///
/// Dictations used to be excluded at any length, on the theory that they are
/// pasted somewhere else within seconds of being spoken and nobody reopens
/// them. That was true of dictation as a clipboard and false of dictation as a
/// record: it left 84 notes of forty words or more sitting with no overview,
/// which is most of the library. Everything that gets transcribed now gets
/// named and summarised.
fn should_brief(source: &str, word_count: usize) -> bool {
    BRIEFED_SOURCES.contains(&source) && word_count >= BRIEF_MIN_WORDS
}

/// Forty words is about fifteen seconds of speech, and about where a spoken
/// note stops being a single thought and starts being several.
const BRIEF_MIN_WORDS: usize = 40;

/// Write a note's overview without waiting for it.
///
/// On its own thread because the callers are an ingest finishing and a meeting
/// stopping, and somebody who has just left a call should get their transcript
/// now rather than when the summary is written. The window is told through
/// events either way.
pub fn brief_in_background(app: &tauri::AppHandle, id: &str) {
    let app = app.clone();
    let id = id.to_string();
    std::thread::spawn(move || {
        if let Err(problem) = write_brief(&app, &id) {
            // Not fatal and not worth a dialog: the note is saved and readable,
            // the overview has a button of its own to try again, and the sweep
            // at the next launch will have another go unprompted.
            eprintln!("[brief] {id}: {problem}");
            let _ = app.emit("brief-failed", serde_json::json!({ "id": id, "problem": problem }));
        }
    });
}

/// Below this a dictation is its own title.
///
/// It was forty when every name cost a Bedrock call and the question was what a
/// title is worth paying for. On-device it is free, so the question is only
/// whether a name says more than the words already do — and by fifteen words a
/// dictation has a subject, while "Also, all meetings should be automatically…"
/// is a sentence cut off mid-flow rather than a name for anything.
const DICTATION_TITLE_MIN_WORDS: usize = 15;

fn should_ai_title(source: &str, word_count: usize) -> bool {
    if source == "hotkey" {
        word_count >= DICTATION_TITLE_MIN_WORDS
    } else {
        word_count > 0
    }
}

/// Ask whichever model this build has to title a note, then rename it and tell
/// the UI.
///
/// Emits `title-naming {id, naming}` around the request so the card can show a
/// "naming…" state while it's in flight, and `title-updated {id, title}` when it
/// lands. On any failure it emits `title-naming false` so the UI clears the
/// indicator and keeps the fallback title.
fn spawn_title_job(app: &tauri::AppHandle, id: String, text: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = app.emit("title-naming", serde_json::json!({ "id": id, "naming": true }));

        let clear = || {
            let _ = app.emit("title-naming", serde_json::json!({ "id": id, "naming": false }));
        };

        let Some(title) = make_title(&app, &text) else {
            clear();
            return;
        };
        {
            let store = app.state::<Store>();
            let conn = store.0.lock().unwrap();
            if store::rename(&conn, &id, &title).is_err() {
                clear();
                return;
            }
            let _ = store::mark_titled(&conn, &id);
        }
        let _ = app.emit("title-updated", serde_json::json!({ "id": id, "title": title }));
    });
}

/// Name a piece of text, or return `None` and leave the note as it is.
///
/// Bedrock first where it exists, on-device second — the same order and the same
/// reasoning as [`write_brief`]. Blocking; callers put it on a thread.
fn make_title(app: &tauri::AppHandle, text: &str) -> Option<String> {
    #[cfg(feature = "assistant")]
    {
        // Read the port out before the `if`, so the mutex guard isn't held
        // across a twenty-second HTTP call.
        let port = *app.state::<SidecarState>().port.lock().unwrap();
        if let Some(port) = port {
            if let Some(title) = fetch_title(port, text) {
                return Some(title);
            }
        }
    }
    title_on_device(app, text)
}

#[cfg(target_os = "macos")]
fn title_on_device(app: &tauri::AppHandle, text: &str) -> Option<String> {
    match brief::title(app, text) {
        Ok(title) => Some(title),
        Err(problem) => {
            // Logged, never shown. A note that keeps its filename is a normal
            // outcome — Apple Intelligence may simply be off — and it is not
            // worth a banner over a note that saved fine.
            eprintln!("[title] {problem}");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn title_on_device(_app: &tauri::AppHandle, _text: &str) -> Option<String> {
    None
}

/// Whether this binary carries the AI layer.
///
/// Registered only in the assistant build, so the lite app rejects it as an
/// unknown command and the frontend reads that rejection as `false`. A command
/// rather than a build-time flag because one webview bundle serves both builds —
/// there is nothing at bundle time that knows which binary it will be paired
/// with.
#[cfg(feature = "assistant")]
#[tauri::command]
fn assistant_build() -> bool {
    true
}

/// Whether an overview can be made on this Mac, and why not when it can't.
///
/// The full build can always make one, because Bedrock does not care what the
/// Mac is. The public build depends entirely on the on-device model, so the
/// answer is a reason as well as a yes or no — "turn on Apple Intelligence" is
/// something a person can act on, and a greyed-out button is not.
#[cfg(target_os = "macos")]
#[tauri::command]
async fn brief_capability(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let local = brief::usable(&app);
        let assistant = cfg!(feature = "assistant");
        Ok(serde_json::json!({
            "available": assistant || local.available,
            "reason": local.reason,
            "on_device": local.available,
            // The sentence lives here rather than in the window so there is one
            // wording per reason, shared by the pane that explains why the
            // button is dead and the error a failed attempt returns.
            "message": brief::explain(&local.reason),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
async fn brief_capability() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "available": cfg!(feature = "assistant"),
        "reason": "os-too-old",
        "on_device": false,
        "message": "Overviews need Apple's on-device model, which is macOS only.",
    }))
}

/// Ask Bedrock, when this build has it. `None` when it does not, or when the
/// sidecar is not up — either way the on-device model is the next thing to try.
#[cfg(feature = "assistant")]
fn brief_from_bedrock(app: &tauri::AppHandle, text: &str) -> Option<serde_json::Value> {
    let port = (*app.state::<SidecarState>().port.lock().unwrap())?;
    sidecar::fetch_brief(port, text)
}

#[cfg(not(feature = "assistant"))]
fn brief_from_bedrock(_app: &tauri::AppHandle, _text: &str) -> Option<serde_json::Value> {
    None
}

/// Generate the structured overview for a note and keep it.
///
/// On demand rather than at ingest. Most notes are dictated, pasted somewhere
/// and never opened again; briefing all of them would spend real money in the
/// full build and real minutes in the public one. Asking once and storing the
/// answer makes every later open free.
///
/// Bedrock first where it exists, on-device second. Not the other way round,
/// even though local is free and private: the full build's overviews are what
/// they are today, and quietly moving them to a 3B model would change every
/// note's overview for people who did not ask for that. The public build has no
/// first choice to lose, so it goes straight to the second.
///
/// `async` so the round trip — tens of seconds either way — runs on the async
/// runtime instead of blocking the webview, with the blocking work handed to a
/// pool thread.
#[tauri::command]
async fn generate_brief(app: tauri::AppHandle, id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || write_brief(&app, &id))
        .await
        .map_err(|e| e.to_string())?
}

/// Read a note, write its overview, keep it. Blocking; caller picks the thread.
///
/// Shared by the button and by a meeting finishing, so the two cannot drift on
/// which model is preferred or what gets stored.
pub fn write_brief(app: &tauri::AppHandle, id: &str) -> Result<serde_json::Value, String> {
    let text = {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        store::get(&conn, id).map_err(|e| e.to_string())?.text
    };
    if text.trim().is_empty() {
        return Err("This note has no text to summarise.".to_string());
    }

    // Deliberately not holding the store lock across either call: a brief takes
    // tens of seconds, and the rest of the app keeps writing.
    let brief = match brief_from_bedrock(app, &text) {
        Some(brief) => brief,
        None => brief_on_device(app, id, &text)?,
    };

    {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        store::set_brief(&conn, id, &brief).map_err(|e| e.to_string())?;
    }
    // Announced as well as returned: the button awaits this, but a meeting's
    // own overview has nobody waiting on a promise.
    let _ = app.emit("brief-saved", serde_json::json!({ "id": id, "brief": brief }));

    name_from_brief(app, id, &brief);
    graph_from_brief(app, id, &brief);
    Ok(brief)
}

/// Record what this note was about, from the overview just written for it.
///
/// Last in the chain and quietest: the transcript is saved, the overview is
/// saved, the note has a name, and this only decides whether the note can be
/// found later by subject as well as by word. A failure here is not shown and
/// not returned — the launch sweep picks the note up again, and until then the
/// only thing missing is a few chips.
fn graph_from_brief(app: &tauri::AppHandle, id: &str, brief: &serde_json::Value) {
    let entities = match brief::entities(app, brief) {
        Ok(entities) => entities,
        Err(problem) => {
            eprintln!("[graph] {id}: {problem}");
            return;
        }
    };
    if entities.is_empty() {
        return;
    }

    let store = app.state::<Store>();
    let conn = store.0.lock().unwrap();
    let created = store::get(&conn, id).map(|note| note.meta.created_at).unwrap_or(0);
    match graph::set_mentions(&conn, id, created, &entities) {
        Ok(written) => eprintln!("[graph] {id}: {written} entit(ies)"),
        Err(problem) => eprintln!("[graph] {id}: {problem}"),
    }
}

/// Name a note nobody has named yet, from the overview just written for it.
///
/// This is how meetings get called something. A meeting saves as `Meeting — 6
/// Aug, 2:14 PM`, which says exactly when it was and nothing about what it was,
/// and then briefs itself. By the time that finishes there is a summary in hand
/// that was distilled from the whole call — a far better thing to name it from
/// than the transcript's opening minutes, and one short model call rather than a
/// second pass over an hour of speech.
///
/// Only for a note nobody has named. A title the user typed, or one already
/// chosen at ingest, is not ours to overwrite — which is also what stops a
/// second press of Generate from renaming a note out from under someone.
fn name_from_brief(app: &tauri::AppHandle, id: &str, brief: &serde_json::Value) {
    {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        if store::ai_titled(&conn, id) {
            return;
        }
    }
    let summary = brief
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim();
    if summary.is_empty() {
        return;
    }
    spawn_title_job(app, id.to_string(), summary.to_string());
}

#[cfg(target_os = "macos")]
fn brief_on_device(
    app: &tauri::AppHandle,
    id: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    brief::generate(app, text, |fraction, stage| {
        brief::report(app, id, fraction, stage)
    })
}

#[cfg(not(target_os = "macos"))]
fn brief_on_device(
    _app: &tauri::AppHandle,
    _id: &str,
    _text: &str,
) -> Result<serde_json::Value, String> {
    Err("Overviews need Apple's on-device model, which is macOS only.".into())
}

/// Say that the app is working through a backlog, and how much is left.
///
/// Somebody who has been using this since before titles, overviews and the
/// graph existed opens a new version and finds it quietly rewriting their whole
/// library — hundreds of model calls, spread over an hour so as not to fight
/// with dictation. Without a word on screen, the honest reading of that is
/// "something is wrong with my Mac". With one, it is "it is catching up".
///
/// Deliberately one line and no bar. There is no honest total: the graph sweep
/// is capped per launch, a note can fail and be retried next time, and a
/// progress bar that jumps backwards is worse than no bar. A count going down
/// is true, and true is what this can offer.
///
/// `left` at zero means finished, and the strip goes away.
fn catching_up(app: &tauri::AppHandle, doing: &str, left: usize) {
    let _ = app.emit(
        "catching-up",
        serde_json::json!({ "doing": doing, "left": left }),
    );
}

/// One-time sweep that names notes saved before AI titling existed (older
/// Discord messages, files, long dictations, meetings whose overview failed).
/// Runs sequentially and gently so it never hammers Bedrock or hogs the
/// on-device model, and marks each row done so it won't run again. A note whose
/// title call fails is simply left for the next launch to retry.
fn spawn_backfill(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        // The full build's namer lives behind the sidecar, so there is nothing
        // to sweep with until it answers. Give it ~30s, then go anyway: the
        // on-device model is the fallback and needs no port.
        #[cfg(feature = "assistant")]
        for _ in 0..60 {
            if app.state::<SidecarState>().port.lock().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let rows = {
            let store = app.state::<Store>();
            let conn = store.0.lock().unwrap();
            store::list_untitled(&conn, DICTATION_TITLE_MIN_WORDS as i64).unwrap_or_default()
        };
        if rows.is_empty() || !worth_asking_the_model(&app, "title") {
            return;
        }
        eprintln!("[title] backfilling {} untitled note(s)", rows.len());
        let mut left = rows.len();
        catching_up(&app, "naming your notes", left);

        for (id, text) in rows {
            if let Some(title) = make_title(&app, &text) {
                {
                    let store = app.state::<Store>();
                    let conn = store.0.lock().unwrap();
                    if store::rename(&conn, &id, &title).is_ok() {
                        let _ = store::mark_titled(&conn, &id);
                    }
                }
                let _ = app.emit("title-updated", serde_json::json!({ "id": id, "title": title }));
            }
            left = left.saturating_sub(1);
            catching_up(&app, "naming your notes", left);
            // Space the calls out; the backfill is never urgent, and on the
            // on-device path it is competing with everything else Apple
            // Intelligence is being asked to do on this Mac.
            std::thread::sleep(BACKFILL_PAUSE);
        }
        catching_up(&app, "naming your notes", 0);
    });
}

/// The sources an overview is written for, unasked. Kept beside
/// [`should_brief`] because the sweep and the ingest have to agree: a note the
/// sweep would pick up but the ingest would not is one that only ever gets
/// summarised on the launch *after* it was recorded.
const BRIEFED_SOURCES: &[&str] = &["meeting", "file", "mic", "hotkey"];

/// Whether there is any point starting a sweep on this Mac.
///
/// Asked once for a whole sweep rather than once per note. Plenty of Macs
/// cannot run Apple's on-device model — wrong macOS, wrong silicon, or somebody
/// simply turned it off — and on those the sweeps would otherwise spawn a
/// helper process per note, a hundred times over, each one starting a model
/// session purely to be told there isn't one.
///
/// The full build has a second route and does not depend on this: Bedrock has
/// no opinion about Apple Intelligence, so the sweep goes ahead regardless and
/// the on-device path is only its fallback.
fn worth_asking_the_model(app: &tauri::AppHandle, what: &str) -> bool {
    #[cfg(feature = "assistant")]
    {
        let _ = (app, what);
        return true;
    }

    #[cfg(all(not(feature = "assistant"), target_os = "macos"))]
    {
        let state = brief::usable(app);
        if !state.available {
            // Once, at the level that says which sweep gave up, and never per
            // note: on a Mac without the model this is the normal state of the
            // world rather than an error, and a hundred lines saying so is how
            // a log stops being read.
            eprintln!("[{what}] skipped: {}", brief::explain(&state.reason));
        }
        state.available
    }

    #[cfg(all(not(feature = "assistant"), not(target_os = "macos")))]
    {
        let _ = (app, what);
        false
    }
}

/// Have another go at every overview that never landed.
///
/// A brief that failed used to stay failed for good: the meeting was saved, the
/// error went to a toast, and nothing ever tried again. That is the wrong shape
/// for a job whose commonest failure is "Apple Intelligence was still
/// downloading its model" — a condition that fixes itself and leaves no way to
/// notice it has.
///
/// Strictly one at a time and unhurried. This is the same on-device model the
/// user might be dictating through, and a summary is never the urgent thing on
/// the machine.
fn spawn_brief_sweep(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        // After the titles. Both want the same model and the titles are what
        // somebody sees first — a list of real names is more use in the ten
        // seconds after launch than one overview nobody has opened yet.
        std::thread::sleep(std::time::Duration::from_secs(90));

        let ids = {
            let store = app.state::<Store>();
            let conn = store.0.lock().unwrap();
            store::list_unbriefed(&conn, BRIEFED_SOURCES, BRIEF_MIN_WORDS as i64)
                .unwrap_or_default()
        };
        if ids.is_empty() || !worth_asking_the_model(&app, "brief") {
            return;
        }
        eprintln!("[brief] {} note(s) have no overview; writing them", ids.len());
        let mut left = ids.len();
        catching_up(&app, "writing overviews", left);

        for id in ids {
            // Re-checked rather than trusted: the list was taken minutes ago and
            // the note may have been briefed by hand, or deleted, since.
            let still_wants_one = {
                let store = app.state::<Store>();
                let conn = store.0.lock().unwrap();
                store::get(&conn, &id)
                    .map(|note| note.brief.is_null())
                    .unwrap_or(false)
            };
            if !still_wants_one {
                left = left.saturating_sub(1);
                catching_up(&app, "writing overviews", left);
                continue;
            }
            if let Err(problem) = write_brief(&app, &id) {
                eprintln!("[brief] {id}: {problem}");
                // Left unbriefed on purpose, for the next launch to retry. The
                // sweep does not thrash a note that is failing for a reason that
                // will not change in the next thirty seconds.
            }
            left = left.saturating_sub(1);
            catching_up(&app, "writing overviews", left);
            std::thread::sleep(std::time::Duration::from_secs(5));
        }

        // Chained rather than given its own thread: both want the same model,
        // one at a time, and the graph reads briefs — so it wants to run after
        // the pass that writes them, not beside it.
        sweep_the_graph(&app);
        // Only here, at the end of the chain, is the app actually caught up.
        catching_up(&app, "", 0);
    });
}

/// Record what every already-briefed note was about.
///
/// New notes are graphed as their overview lands, so this is only ever catching
/// up: notes briefed before the graph existed, and notes whose extraction failed
/// once. Bounded per launch because the on-device model is shared with dictation
/// and there is no version of this that is worth making the app feel slow —
/// whatever is left is picked up next time.
const GRAPH_SWEEP_LIMIT: usize = 40;

fn sweep_the_graph(app: &tauri::AppHandle) {
    let ids = {
        let store = app.state::<Store>();
        let conn = store.0.lock().unwrap();
        graph::list_ungraphed(&conn, GRAPH_SWEEP_LIMIT).unwrap_or_default()
    };
    if ids.is_empty() {
        return;
    }
    eprintln!("[graph] reading {} note(s) for what they are about", ids.len());
    let mut left = ids.len();
    catching_up(app, "reading what your notes are about", left);

    for id in ids {
        let brief = {
            let store = app.state::<Store>();
            let conn = store.0.lock().unwrap();
            store::get(&conn, &id).map(|note| note.brief).unwrap_or(serde_json::Value::Null)
        };
        if brief.is_null() {
            left = left.saturating_sub(1);
            catching_up(app, "reading what your notes are about", left);
            continue;
        }
        graph_from_brief(app, &id, &brief);
        left = left.saturating_sub(1);
        catching_up(app, "reading what your notes are about", left);
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

/// Longer without a sidecar, because the work is happening on this machine.
const BACKFILL_PAUSE: std::time::Duration = std::time::Duration::from_millis(if cfg!(feature =
    "assistant")
{
    250
} else {
    1500
});

#[cfg(feature = "assistant")]
/// POST the transcript to the sidecar's `/title` route. Returns None whenever
/// Bedrock is unreachable (no creds, no model access, offline) — the sidecar
/// answers `{"title": null}` in that case rather than erroring.
pub(crate) fn fetch_title(port: u16, text: &str) -> Option<String> {
    let resp = reqwest::blocking::Client::new()
        .post(format!("http://127.0.0.1:{port}/title"))
        .json(&serde_json::json!({ "text": text }))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .ok()?;
    let body: serde_json::Value = resp.json().ok()?;
    let title = body.get("title")?.as_str()?.trim();
    (!title.is_empty()).then(|| title.to_string())
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

#[cfg(feature = "assistant")]
#[tauri::command]
fn discord_status(state: tauri::State<discord::DiscordStatus>) -> discord::DiscordStatusView {
    discord::DiscordStatusView {
        enabled: *state.enabled.lock().unwrap(),
        error: state.error.lock().unwrap().clone(),
        processed: *state.processed.lock().unwrap(),
    }
}

#[cfg(feature = "assistant")]
#[tauri::command]
fn slack_status(state: tauri::State<slack::SlackStatus>) -> slack::SlackStatusView {
    slack::SlackStatusView {
        enabled: *state.enabled.lock().unwrap(),
        error: state.error.lock().unwrap().clone(),
        processed: *state.processed.lock().unwrap(),
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

/// Write bytes the frontend produced to a path the user picked.
///
/// The share card is drawn on a canvas and arrives here as PNG bytes. Separate
/// from `write_text_file` because a PNG through a `String` would be mangled by
/// UTF-8 validation long before it reached the disk.
#[tauri::command]
fn write_binary_file(path: String, bytes: Vec<u8>) -> Result<(), String> {
    std::fs::write(&path, bytes).map_err(|e| e.to_string())
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
        .manage(SidecarState::default())
        .manage(engine::EngineState::default())
        .manage(dictation::DictationState::default())
        .manage(meeting::MeetingState::default());

    #[cfg(feature = "assistant")]
    let builder = builder
        .manage(discord::DiscordStatus::default())
        .manage(slack::SlackStatus::default());

    builder
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let conn = store::open(&dir)?;
            // qwee's memory shares the transcript DB — one connection, one WAL.
            #[cfg(feature = "assistant")]
            knowledge::init(&conn)?;
            app.manage(Store(Mutex::new(conn)));

            // Watch for calls from launch. Nothing is recorded by this — it
            // only notices that some app has opened the microphone, so the
            // window can offer to take notes instead of relying on someone
            // remembering to press record before a meeting they are already in.
            // The floating card that sits over the call. Launched with the
            // app like the dictation overlay: it is hidden until something
            // happens, and starting it lazily would put a process launch in
            // the way of the first thing it has to show.
            meeting::spawn_hud(app.handle().clone());
            meeting::spawn_detector(app.handle().clone());

            // Read once at launch and kept in memory: the dictation path reads
            // these from a CGEventTap thread with no window open to ask.
            let loaded = settings::load(app.handle());
            // Point the tap at the stored chord before it is installed below,
            // so the first keypress after launch already uses it.
            shortcut::arm(&loaded.shortcut);
            // And tell it whether that chord is held or switched, for the same
            // reason: the very first press after launch should behave the way
            // the settings pane says it will.
            shortcut::arm_hold(loaded.hold_to_talk);
            app.manage(settings::SettingsState(Mutex::new(loaded)));

            // Speaker models, quietly, before anything needs them. Reads the
            // setting that was just managed, so it goes after that line.
            spawn_model_prefetch(app.handle());
            spawn_speaker_backfill(app.handle());

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

            // Starting the sidecar blocks on its ready line, so keep it off the
            // main thread — the window should paint immediately.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                #[cfg(feature = "assistant")]
                {
                    // Only the brain lives here now — transcription runs
                    // in-process via `engine`.
                    let state = handle.state::<SidecarState>();
                    sidecar::spawn(&handle, &state);
                    // Only after the sidecar reports a port, since the ingest has
                    // nowhere to send work until then.
                    discord::spawn(handle.clone());
                    // One Socket Mode connection covers every Slack channel the
                    // bot is invited to.
                    slack::spawn(handle.clone());
                    // Daily morning-sync nudge (no-op unless sync_channel is set,
                    // and it steps aside when the calendar watcher is configured).
                    slack::spawn_scheduler(handle.clone());
                    // Calendar-driven meeting reminders (no-op unless
                    // calendar_ics_url + team_emails are set).
                    slack::spawn_calendar(handle.clone());
                    // Start-of-day and end-of-day team digests, weekdays only.
                    slack::spawn_digests(handle.clone());
                }

                // Name any notes that predate AI titling (older Discord voice
                // notes, files, long dictations). Both builds: the public one
                // has Apple's on-device model to name them with, and its
                // library is the one most likely to be full of filenames.
                spawn_backfill(handle.clone());

                // And write the overviews that never landed — a failed one used
                // to stay failed for good.
                spawn_brief_sweep(handle.clone());

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
                #[cfg(feature = "assistant")]
                sidecar::shutdown(&_window.state::<SidecarState>());
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

/// The command surface. `generate_handler!` takes a fixed list, so the two
/// builds get one each rather than the lite app exposing Slack and Discord
/// commands that can never answer.
#[cfg(feature = "assistant")]
fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        sidecar_status,
        discord_status,
        slack_status,
        list_transcripts,
        get_transcript,
        save_transcript,
        reveal_source,
        find_speakers,
        cloud_sentence,
        update_transcript,
        set_transcript_peaks,
        archive_transcript_media,
        transcribe_again,
            export::export_pdf,
        open_accessibility_settings,
        settings::get_settings,
        settings::set_diarization,
        settings::set_live_preview,
        settings::set_microphone,
        settings::set_shortcut,
        settings::set_hold_to_talk,
        settings::set_restore_clipboard,
        microphone::list_microphones,
        engine::start_transcription,
        engine::transcribe_peaks,
        engine::engine_status,
        engine::engine_unload,
        meeting::meeting_status,
        meeting::meeting_start,
        meeting::meeting_stop,
        meeting::open_audio_capture_settings,
        models::models_status,
        models::models_fetch,
        rename_transcript,
        rename_speaker,
        names_in_transcript,
        delete_transcript,
        write_text_file,
        write_binary_file,
        save_recording,
        analytics::analytics_summary,
        analytics::analytics_assistant,
        analytics::analytics_themes,
        assistant_build,
        brief_capability,
        generate_brief,
        note_topics,
        library_topics,
        notes_about,
        transcribe_once,
        #[cfg(target_os = "macos")]
        ask_library,
        #[cfg(target_os = "macos")]
        chat_history,
        #[cfg(target_os = "macos")]
        forget_chat,
        app_version,
        update::check_update,
        update::open_release,
    ]
}

#[cfg(not(feature = "assistant"))]
fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        sidecar_status,
        list_transcripts,
        get_transcript,
        save_transcript,
        reveal_source,
        find_speakers,
        cloud_sentence,
        update_transcript,
        set_transcript_peaks,
        archive_transcript_media,
        transcribe_again,
            export::export_pdf,
        open_accessibility_settings,
        settings::get_settings,
        settings::set_diarization,
        settings::set_live_preview,
        settings::set_microphone,
        settings::set_shortcut,
        settings::set_hold_to_talk,
        settings::set_restore_clipboard,
        microphone::list_microphones,
        engine::start_transcription,
        engine::transcribe_peaks,
        engine::engine_status,
        engine::engine_unload,
        meeting::meeting_status,
        meeting::meeting_start,
        meeting::meeting_stop,
        meeting::open_audio_capture_settings,
        models::models_status,
        models::models_fetch,
        rename_transcript,
        rename_speaker,
        names_in_transcript,
        delete_transcript,
        write_text_file,
        write_binary_file,
        save_recording,
        analytics::analytics_summary,
        brief_capability,
        generate_brief,
        note_topics,
        library_topics,
        notes_about,
        transcribe_once,
        #[cfg(target_os = "macos")]
        ask_library,
        #[cfg(target_os = "macos")]
        chat_history,
        #[cfg(target_os = "macos")]
        forget_chat,
        app_version,
        update::check_update,
        update::open_release,
    ]
}

// -- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule that decides whether a note labels itself, which is the
    /// difference between the feature working and the feature being a button
    /// somebody has to know about.
    #[test]
    fn anything_long_enough_to_be_a_conversation_labels_itself() {
        // A dictation counts. It usually holds one voice and the pass will say
        // so cheaply — but a dictation with two people in it is the case with
        // nothing on screen to suggest looking, so it is the expensive one to
        // get wrong.
        assert!(wants_speakers("hotkey", 60.0));
        assert!(wants_speakers("file", 110.0));
        assert!(wants_speakers("discord", 30.0));
    }

    #[test]
    fn a_meeting_is_never_guessed_at() {
        // Both sides were recorded separately, so who spoke is a fact there.
        // `label_speakers` refuses it too; this is the cheaper of the two.
        assert!(!wants_speakers("meeting", 3_600.0));
    }

    #[test]
    fn a_few_seconds_cannot_hold_two_people() {
        // "testing the mic, testing the mic" at 2.5s has a known answer.
        assert!(!wants_speakers("hotkey", 2.5));
        assert!(!wants_speakers("file", LONG_ENOUGH_FOR_TWO - 0.1));
        assert!(wants_speakers("file", LONG_ENOUGH_FOR_TWO));
    }

    use super::*;

    /// The two floors answer different questions and must not be conflated. A
    /// title says what a note is; a summary saves you reading it. There is a
    /// band where a note has earned a name and has nothing a summary could add.
    #[test]
    fn a_note_earns_a_name_before_it_earns_a_summary() {
        assert!(should_ai_title("hotkey", 20), "a 20-word dictation has a subject");
        assert!(!should_brief("hotkey", 20), "and nothing a summary would shorten");
        assert!(!should_brief("file", 20));

        assert!(
            DICTATION_TITLE_MIN_WORDS < BRIEF_MIN_WORDS,
            "a note must never be summarised before it is even named"
        );
    }

    /// Whatever the source, once a note is long enough to be worth summarising
    /// it gets summarised. Dictations were the exception and stopped being one:
    /// the exception had left most of the library with no overview.
    #[test]
    fn every_kind_of_note_is_summarised_once_it_is_long_enough() {
        for source in ["hotkey", "meeting", "file", "mic"] {
            assert!(
                should_brief(source, 200),
                "a 200-word {source} note is worth an overview"
            );
        }
    }

    /// A note the ingest would brief but the sweep would not — or the reverse —
    /// is one that only gets summarised on the launch after it was recorded, or
    /// gets summarised twice.
    #[test]
    fn the_sweep_looks_for_exactly_what_the_ingest_writes() {
        for source in BRIEFED_SOURCES {
            assert!(
                should_brief(source, BRIEF_MIN_WORDS),
                "the sweep collects {source} but the ingest would not brief it"
            );
        }
        assert!(!should_brief("meeting", BRIEF_MIN_WORDS - 1));
    }

    /// A `#[tauri::command]` that is never named in `generate_handler!` is not a
    /// compile error — it is a command the frontend can call and always be
    /// refused by, which is how the "Find speakers" switch spent a release
    /// snapping back to off. There are two lists and it is easy to add to one,
    /// so this reads the source and insists every settings command is in both.
    #[test]
    fn every_settings_command_is_registered_in_both_builds() {
        let settings = include_str!("settings.rs");
        let me = include_str!("lib.rs");

        let commands: Vec<&str> = settings
            .split("#[tauri::command]")
            .skip(1)
            .filter_map(|after| after.split_once("pub fn "))
            .filter_map(|(_, name)| name.split(['(', '<', ' ', '\n']).next())
            .collect();

        assert!(
            commands.contains(&"set_diarization"),
            "the scan found {commands:?}, which is not the settings command surface"
        );
        for name in commands {
            assert_eq!(
                me.matches(&format!("settings::{name},")).count(),
                2,
                "settings::{name} is not in both handler lists, so one build \
                 rejects every call to it"
            );
        }
    }

    /// People have more than one word in their name, and the window offers a
    /// plain text field, so the first thing anyone types into it is a full one.
    #[test]
    fn a_full_name_with_spaces_is_a_name() {
        for name in [
            "Marcus Chen",
            "Priya Raghavan Nair",
            "Ana Sofía",
            "Mary-Jane O'Neill",
            "  Marcus Chen  ",
        ] {
            assert_eq!(
                check_speaker_name(name).as_deref(),
                Ok(name.trim()),
                "{name:?} was refused"
            );
        }
    }

    /// The two that would change what the transcript means when it is read back
    /// — a colon splits a turn into two speakers, a line break splits the
    /// paragraph — plus the two that are simply not a name.
    #[test]
    fn a_name_that_would_break_the_transcript_is_refused() {
        for name in ["Marcus: Chen", "Marcus\nChen", "Marcus\rChen", "", "   "] {
            assert!(check_speaker_name(name).is_err(), "{name:?} was allowed");
        }
        assert!(check_speaker_name(&"a".repeat(41)).is_err());
        assert!(check_speaker_name(&"a".repeat(40)).is_ok());
    }

    /// The same trap, one file closer to home. `settings.rs` was guarded above
    /// because that is where it was first sprung, but nothing about it is
    /// particular to settings: the commands declared in *this* file go into the
    /// same two lists by hand, and renaming one — `names_in_meeting` to
    /// `names_in_transcript`, say — means editing both or shipping a build where
    /// the frontend calls a command that is not there.
    ///
    /// A command the lite build can answer belongs in both lists. One gated on
    /// `assistant` belongs in the full list only — and `sidecar_status`, which
    /// is written twice with opposite gates, counts as reachable from both.
    #[test]
    fn every_command_in_this_file_is_registered_in_the_builds_it_belongs_to() {
        // Only the source above the tests: the marker being scanned for is
        // itself written out below, and would otherwise be scanned as a command.
        let me = include_str!("lib.rs");
        let me = me.split_once("#[cfg(test)]").expect("this module").0;

        // A command can be declared twice under opposite gates, so the builds it
        // is reachable from are collected across every declaration before any of
        // them is judged.
        let mut in_lite: Vec<&str> = Vec::new();
        let mut commands: Vec<&str> = Vec::new();

        let pieces: Vec<&str> = me.split("#[tauri::command]").collect();
        for (i, piece) in pieces.iter().enumerate().skip(1) {
            // Attributes stack above the marker, so the builds a declaration
            // belongs to are read from the lines immediately before it.
            let gates: Vec<&str> = pieces[i - 1]
                .lines()
                .rev()
                .take_while(|line| line.trim_start().starts_with("#["))
                .collect();
            let full_only = gates
                .iter()
                .any(|line| line.contains("feature = \"assistant\"") && !line.contains("not("));

            let Some(name) = piece
                .split_once("fn ")
                .and_then(|(_, rest)| rest.split(['(', '<', ' ', '\n']).next())
            else {
                continue;
            };
            if !commands.contains(&name) {
                commands.push(name);
            }
            if !full_only && !in_lite.contains(&name) {
                in_lite.push(name);
            }
        }

        assert!(
            commands.len() > 20 && commands.contains(&"rename_speaker"),
            "the scan found {commands:?}, which is not this file's command surface"
        );

        for name in &commands {
            // Matched with the list indentation, so a mention of the name in
            // prose or in a call cannot stand in for a registration.
            let listed = me.matches(&format!("\n        {name},")).count();
            let wanted = if in_lite.contains(name) { 2 } else { 1 };
            assert_eq!(
                listed, wanted,
                "{name} is registered in {listed} of the handler lists, not \
                 {wanted} — one build refuses every call to it"
            );
        }
    }
}
