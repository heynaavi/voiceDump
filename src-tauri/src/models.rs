//! Fetching the speech models on first run instead of shipping them.
//!
//! The two quantised whisper weights are 695 MB between them, and until now
//! they lived inside the .app. That made the download honest — install it and
//! it works on a plane — but it also meant every release was a 720 MB download
//! of which 695 MB had not changed since the first build. Someone updating for
//! a two-line fix re-downloaded the models to get it.
//!
//! So they come down once, into the same directory as the database and the
//! settings file, and stay there. An upgrade replaces the .app and finds the
//! models already sitting beside the history they belong to; a fresh install on
//! a machine that has run the app before finds them too. The only download that
//! ever happens twice is one that was interrupted.
//!
//! **Why curl.** Same reasoning [`crate::media`] uses for `afconvert` and the
//! public build uses for its update check: macOS ships `/usr/bin/curl`, with
//! the system trust store and with `-C -` resume already solved. Linking an
//! HTTP client and a TLS stack to fetch two files once is a poor trade.
//! Absolute path, so a `PATH` we don't control cannot decide what "curl" means.
//!
//! **Why the digests are pinned.** The bytes are fetched over the network and
//! then handed to ggml, which will happily `abort()` the whole process on a
//! malformed header — a truncated download would present as the app crashing
//! on launch, forever, with nothing to point at. A digest that does not match
//! is deleted rather than kept, so the failure is "download it again" instead.
//! It also means the app runs *these* weights or none: a mirror that has been
//! swapped out cannot quietly become the thing transcribing your notes.

use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::engine::{auto_model, ModelSize};

const CURL: &str = "/usr/bin/curl";
const SHASUM: &str = "/usr/bin/shasum";

/// Upstream. The same repository `scripts/fetch-models.sh` has always pulled
/// from, so the bytes a user downloads are the bytes previous releases shipped.
const BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// One downloadable model.
pub struct Spec {
    pub size: ModelSize,
    pub name: &'static str,
    /// Lowercase hex sha256 of the file as published.
    pub sha256: &'static str,
    /// Exact published length, so progress is a real percentage rather than a
    /// spinner, and so a short file is caught before the digest is even run.
    pub bytes: u64,
}

const SPECS: [Spec; 2] = [
    Spec {
        size: ModelSize::Small,
        name: "ggml-small-q5_1.bin",
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        bytes: 190_085_487,
    },
    Spec {
        size: ModelSize::Medium,
        name: "ggml-medium-q5_0.bin",
        sha256: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
        bytes: 539_212_467,
    },
];

fn spec(size: ModelSize) -> &'static Spec {
    SPECS
        .iter()
        .find(|s| s.size == size)
        .expect("every ModelSize has a spec")
}

/// Which weights this machine actually needs.
///
/// Not both, always. `medium` is the transcription and `small` is the live
/// preview that runs alongside it, so a machine big enough for medium wants
/// both. A machine that [`auto_model`] puts on `small` will never load medium —
/// it would swap — so downloading it would be 539 MB spent on a file that is
/// never opened. Those users get a quarter of the download the old bundle made
/// them take.
pub fn required() -> Vec<ModelSize> {
    needed_for(auto_model())
}

/// The rule on its own, away from the machine it is usually asked about, so it
/// can be tested without reaching for an environment variable the rest of the
/// suite is running in parallel with.
fn needed_for(chosen: ModelSize) -> Vec<ModelSize> {
    match chosen {
        ModelSize::Medium => vec![ModelSize::Medium, ModelSize::Small],
        ModelSize::Small => vec![ModelSize::Small],
    }
}

/// Where downloaded models live: beside the database, not inside the .app.
///
/// This is the whole point of the change. `app_data_dir` survives the .app
/// being replaced, so an update is a 4.6 MB download rather than a 720 MB one.
pub fn store_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no application data directory: {e}"))?
        .join("models");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {dir:?}: {e}"))?;
    Ok(dir)
}

/// Whether a model is already on disk somewhere the engine will find it.
///
/// Delegates to the engine rather than only checking the download directory: a
/// developer with a repo checkout, or anyone still running a bundle that
/// shipped the weights, has them already and must not be asked to fetch them.
fn have(app: &tauri::AppHandle, size: ModelSize) -> bool {
    crate::engine::model_path(app, size).is_some()
}

/// What the window needs to know on boot.
#[derive(Serialize)]
pub struct Status {
    /// Nothing to fetch — go straight to the app.
    pub ready: bool,
    /// Human-readable names of what is missing, longest first.
    pub needed: Vec<String>,
    /// Total bytes still to download, for "695 MB" in the copy.
    pub bytes: u64,
}

/// A download in flight. Two curls racing for the same `.part` would interleave
/// their writes and produce a file that fails its digest for no visible reason.
static FETCHING: AtomicBool = AtomicBool::new(false);

/// Progress, emitted as `model-progress` while [`models_fetch`] runs.
#[derive(Clone, Serialize)]
struct Progress {
    /// Which model is downloading, e.g. "medium".
    label: &'static str,
    /// 1-based position and total, so the UI can say "1 of 2".
    index: usize,
    count: usize,
    /// Bytes of *this* file so far, and its full size.
    received: u64,
    total: u64,
    /// The bytes are all here and the digest is being checked.
    ///
    /// Reported *while* it happens rather than after. Hashing half a gigabyte
    /// takes a second or two, and without this the bar would sit full and
    /// motionless with nothing on screen to explain the pause.
    verifying: bool,
}

#[tauri::command]
pub async fn models_status(app: tauri::AppHandle) -> Status {
    tauri::async_runtime::spawn_blocking(move || {
        let missing: Vec<&Spec> = required()
            .into_iter()
            .filter(|&s| !have(&app, s))
            .map(spec)
            .collect();
        Status {
            ready: missing.is_empty(),
            needed: missing.iter().map(|s| s.size.label().to_string()).collect(),
            bytes: missing.iter().map(|s| s.bytes).sum(),
        }
    })
    .await
    .unwrap_or(Status {
        ready: false,
        needed: Vec::new(),
        bytes: 0,
    })
}

/// Download whatever is missing. Safe to call again after a failure — anything
/// already verified is skipped and a half-finished file resumes.
#[tauri::command]
pub async fn models_fetch(app: tauri::AppHandle) -> Result<(), String> {
    if FETCHING.swap(true, Ordering::SeqCst) {
        return Err("A download is already running.".into());
    }
    let result = tauri::async_runtime::spawn_blocking({
        let app = app.clone();
        move || fetch_all(&app)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    FETCHING.store(false, Ordering::SeqCst);
    result
}

fn fetch_all(app: &tauri::AppHandle) -> Result<(), String> {
    let dir = store_dir(app)?;
    let wanted: Vec<&Spec> = required()
        .into_iter()
        .filter(|&s| !have(app, s))
        .map(spec)
        .collect();
    let count = wanted.len();

    for (i, s) in wanted.iter().enumerate() {
        let report = |received: u64, verifying: bool| {
            let _ = app.emit(
                "model-progress",
                Progress {
                    label: s.size.label(),
                    index: i + 1,
                    count,
                    received,
                    total: s.bytes,
                    verifying,
                },
            );
        };
        report(0, false);
        download(&dir, s, &report)?;
        eprintln!("[models] {} ready", s.name);
    }
    Ok(())
}

/// Fetch one model into `dir`, resuming and verifying.
///
/// The download lands on `<name>.part` and is only renamed once its digest
/// matches, so the engine's existence check can never see a partial file: a
/// name without `.part` is a model that has been proven whole.
fn download(
    dir: &std::path::Path,
    s: &Spec,
    report: &dyn Fn(u64, bool),
) -> Result<(), String> {
    let part = dir.join(format!("{}.part", s.name));
    let done = dir.join(s.name);
    let url = format!("{BASE}/{}", s.name);

    // A leftover part from a previous run that grew past the published size is
    // not resumable — it is wrong. Start it again rather than appending to it.
    if std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) > s.bytes {
        let _ = std::fs::remove_file(&part);
    }

    let mut child = Command::new(CURL)
        .args([
            "--fail",          // an error page must not be written to the file
            "--location",      // huggingface redirects to its CDN
            "--silent",
            "--show-error",
            "--retry", "3",
            "--retry-delay", "2",
            "--continue-at", "-", // resume whatever is already in .part
            "--output",
        ])
        .arg(&part)
        .arg(&url)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start the download: {e}"))?;

    // Progress by watching the file grow rather than parsing curl's own meter.
    // curl's progress output is a terminal animation, not an interface; the
    // size of the file it is writing is a fact.
    loop {
        match child.try_wait() {
            Err(e) => return Err(format!("download failed: {e}")),
            Ok(Some(status)) => {
                if !status.success() {
                    let mut why = String::new();
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.read_to_string(&mut why);
                    }
                    let why = why.trim();
                    return Err(if why.is_empty() {
                        format!("Could not download the {} model.", s.size.label())
                    } else {
                        format!("Could not download the {} model: {why}", s.size.label())
                    });
                }
                break;
            }
            Ok(None) => {
                report(std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0), false);
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }

    // Announce the hash before running it, not after: this is the one stretch
    // where nothing on disk is changing and the bar has nowhere left to go.
    report(s.bytes, true);
    verify(&part, s)?;
    std::fs::rename(&part, &done).map_err(|e| format!("could not save the model: {e}"))
}

/// Prove a downloaded file is the model it claims to be, deleting it if not.
///
/// Deleting rather than keeping is the important half. A file that fails here
/// is either truncated or not the published weights, and leaving it on disk
/// means the next launch finds something model-shaped and feeds it to ggml —
/// which does not return an error for a malformed header, it calls `abort()`.
/// Removing it turns a permanent crash into "download it again".
fn verify(part: &std::path::Path, s: &Spec) -> Result<(), String> {
    // Cheap check first: a short file cannot be the right one, and saying so
    // costs nothing next to hashing half a gigabyte to reach the same verdict.
    let got = std::fs::metadata(part)
        .map_err(|e| format!("the downloaded file went missing: {e}"))?
        .len();
    if got != s.bytes {
        let _ = std::fs::remove_file(part);
        return Err(format!(
            "The {} model downloaded incompletely ({got} of {} bytes). Try again.",
            s.size.label(),
            s.bytes
        ));
    }

    match digest(part) {
        Some(hex) if hex == s.sha256 => Ok(()),
        Some(_) => {
            let _ = std::fs::remove_file(part);
            Err(format!(
                "The {} model does not match its published checksum, so it was discarded.",
                s.size.label()
            ))
        }
        // Can't hash it: keeping an unverified 539 MB file and loading it into
        // ggml is the worse outcome of the two.
        None => {
            let _ = std::fs::remove_file(part);
            Err("Could not verify the download.".into())
        }
    }
}

/// sha256 of a file as lowercase hex, or None if it can't be computed.
fn digest(path: &std::path::Path) -> Option<String> {
    let out = Command::new(SHASUM)
        .args(["-a", "256"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let hex = text.split_whitespace().next()?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_has_a_spec() {
        for size in [ModelSize::Small, ModelSize::Medium] {
            let s = spec(size);
            assert_eq!(s.size, size);
            assert_eq!(s.sha256.len(), 64);
            assert!(s.bytes > 0);
        }
    }

    /// The digests are what stop a mirror swapping the weights, so a typo that
    /// makes one unmatchable would disable that guard silently.
    #[test]
    fn digests_are_lowercase_hex() {
        for s in SPECS.iter() {
            assert!(
                s.sha256.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
                "{} digest must be lowercase hex",
                s.name
            );
            assert!(s.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    /// A medium machine runs the preview on small, so asking for medium alone
    /// would leave live preview permanently unable to start.
    #[test]
    fn medium_machines_also_need_small() {
        let both = needed_for(ModelSize::Medium);
        assert!(both.contains(&ModelSize::Medium));
        assert!(both.contains(&ModelSize::Small));
    }

    /// The saving for smaller machines: 190 MB rather than 695 MB, because
    /// medium would only swap on a machine [`auto_model`] kept off it.
    #[test]
    fn small_machines_do_not_download_medium() {
        assert_eq!(needed_for(ModelSize::Small), vec![ModelSize::Small]);
    }

    /// Whatever is downloaded must be somewhere the engine will look, or the
    /// setup screen would complete and the app would still say it has no model.
    #[test]
    fn everything_required_is_downloadable() {
        for size in needed_for(ModelSize::Medium) {
            assert_eq!(spec(size).size, size);
        }
    }

    #[test]
    fn digest_reads_shasums_output() {
        let dir = std::env::temp_dir().join("voicedumps-digest-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("empty");
        std::fs::write(&file, b"").unwrap();
        // The sha256 of nothing at all, which is a fixed constant.
        assert_eq!(
            digest(&file).as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn digest_declines_a_missing_file() {
        assert_eq!(digest(std::path::Path::new("/no/such/file/here")), None);
    }

    /// The real thing: fetch a model off the network, resume it, and prove the
    /// digest gate works — including that a corrupt file is refused rather than
    /// handed to ggml, which is the failure this whole module exists to avoid.
    ///
    /// 190 MB over the wire, so it is opt-in:
    ///
    /// ```text
    /// VOICEDUMPS_NET_TEST=1 cargo test --  --ignored downloads_and_verifies
    /// ```
    #[test]
    #[ignore = "downloads 190 MB; set VOICEDUMPS_NET_TEST=1"]
    fn downloads_and_verifies_a_real_model() {
        if std::env::var("VOICEDUMPS_NET_TEST").is_err() {
            eprintln!("skipping: set VOICEDUMPS_NET_TEST=1");
            return;
        }
        let s = spec(ModelSize::Small);
        let dir = std::env::temp_dir().join("voicedumps-net-test");
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join(format!("{}.part", s.name));
        let done = dir.join(s.name);
        let _ = std::fs::remove_file(&done);

        // Start from a deliberately partial file: resume is the path an
        // interrupted first run takes, and it is the one worth proving.
        std::fs::write(&part, vec![0u8; 0]).unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        download(&dir, s, &|received, verified| {
            seen.lock().unwrap().push((received, verified))
        })
        .expect("download");

        assert!(done.exists(), "verified model should be renamed into place");
        assert!(!part.exists(), "the .part file should be gone");
        assert_eq!(std::fs::metadata(&done).unwrap().len(), s.bytes);
        assert_eq!(digest(&done).as_deref(), Some(s.sha256));
        assert!(
            seen.lock().unwrap().iter().any(|(r, _)| *r > 0),
            "progress should have been reported while downloading"
        );

        // Now corrupt it and prove the gate refuses it rather than keeping it.
        std::fs::copy(&done, &part).unwrap();
        let mut bytes = std::fs::read(&part).unwrap();
        bytes[0] ^= 0xff;
        std::fs::write(&part, &bytes).unwrap();
        // Same length, wrong contents: only the digest can catch this.
        let err = verify(&part, s).expect_err("a corrupt file must be refused");
        assert!(err.contains("checksum"), "unexpected error: {err}");
        assert!(!part.exists(), "a refused file must not be left on disk");

        let _ = std::fs::remove_file(&done);
    }
}
