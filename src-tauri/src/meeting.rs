//! Meeting capture — both halves of a call, recorded apart on purpose.
//!
//! A meeting has two sources: this Mac's microphone (you) and this Mac's audio
//! output (everyone else). They are captured as two independent tracks and
//! transcribed separately, which buys speaker attribution for nothing.
//!
//! That is worth spelling out, because the obvious design is the wrong one.
//! Mixing both sides into a single recording and asking a model who said what
//! means diarisation: whisper.cpp does not do it, and the models that do would
//! drag a Python runtime into a build whose whole point is that it has none.
//! Keeping the streams apart answers the same question by construction — a
//! sample that arrived on the microphone is you, and a sample that arrived on
//! the tap is not. No model, no guess, no failure mode.
//!
//! What that does *not* buy is telling two other participants apart. On a group
//! call everyone who is not you collapses into one voice. That is a real limit
//! and the UI says so rather than implying more precision than exists; for the
//! one-to-one conversations this feature is mostly for, it is exactly right.
//!
//! The far side comes from `capture-helper/voicedumps-capture`, a small Swift
//! process holding a CoreAudio tap. See its header for why it is a separate
//! binary. Everything here treats it as a pipe that emits a JSON header and then
//! mono f32 until it is closed.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

/// Everything downstream of capture speaks 16 kHz mono, which is what the
/// speech model wants. Matches `engine`'s own rate; kept as a literal here
/// rather than reaching into its private constant.
const ENGINE_RATE: u32 = 16_000;

/// How the two tracks are labelled once they are one transcript.
///
/// "Others" rather than "Them" because the tap cannot tell one remote voice
/// from another — on a group call this is genuinely plural, and a label that
/// quietly implies a single other person would be the UI lying about what it
/// knows.
const LABEL_YOU: &str = "You";
const LABEL_THEM: &str = "Others";

/// Which track a turn came off, kept alongside the label rather than derived
/// from it. A label is a name somebody chose and can change; a side is a fact
/// about which microphone heard it, and it is what the reading view colours by.
const SIDE_YOU: &str = "you";
const SIDE_THEM: &str = "others";

/// The oldest macOS with `AudioHardwareCreateProcessTap`. Below this there is no
/// way to hear the far side without installing a virtual audio driver, which is
/// not something an app should do behind someone's back.
const MIN_MAJOR: u32 = 14;
const MIN_MINOR: u32 = 4;

// -- state ------------------------------------------------------------------

/// One finished side of a conversation.
struct Track {
    path: PathBuf,
    /// Wall-clock milliseconds at the first sample. The two captures do not
    /// start on the same instant — spawning a process takes longer than opening
    /// a `cpal` stream — so the gap between these is what realigns the
    /// transcripts before they are interleaved.
    first_sample_ms: i64,
    /// True if the track carries something other than digital silence. A muted
    /// microphone and an unpermitted tap both produce a valid, empty WAV, and
    /// transcribing one wastes a minute to discover nothing was said.
    heard_anything: bool,
    /// True if a single sample ever arrived, loud or not.
    ///
    /// Distinct from `heard_anything` on purpose, because the two failures need
    /// different words. A quiet call is a call; a stream that delivered *no
    /// bytes at all* is a broken capture, and until this existed the two were
    /// indistinguishable — a tap that produced nothing for seventy-six minutes
    /// looked exactly like a meeting where nobody spoke, and the app said
    /// nothing either way.
    carried_audio: bool,
}

struct Session {
    stop: Arc<AtomicBool>,
    started_ms: i64,
    /// The apps that were on the microphone when this recording began.
    ///
    /// When the last of them lets go, the call is over and so is the recording.
    /// Empty when someone pressed record with nothing else listening — there is
    /// nothing to follow then, and a meeting that ends itself for no visible
    /// reason is worse than one you have to stop by hand.
    following: std::collections::HashSet<String>,
    /// Dropped to stop the helper: closing its stdin is the shutdown it was
    /// built to watch for, and it works whether we exit cleanly or crash.
    child: Option<Child>,
    mic: std::thread::JoinHandle<Result<Track, String>>,
    sys: std::thread::JoinHandle<Result<Track, String>>,
}

/// The in-flight meeting, if there is one. Private field: a session owns live
/// capture threads and a child process, and nothing outside this module has any
/// business reaching past `meeting_start` / `meeting_stop` to touch them.
#[derive(Default)]
pub struct MeetingState(Mutex<Option<Session>>);

#[derive(Serialize, Clone)]
struct Level {
    /// `"you"` or `"others"` — which meter this moves.
    side: &'static str,
    level: f32,
}

#[derive(Serialize)]
pub struct Capability {
    /// Whether this Mac can capture system audio at all.
    pub available: bool,
    /// Why not, in a sentence fit to show someone. Empty when available.
    pub reason: String,
    /// Whether a meeting is being recorded right now.
    pub recording: bool,
}

// -- capability -------------------------------------------------------------

/// This Mac's macOS version, as (major, minor).
///
/// `sw_vers` rather than a crate: it is the same answer from the same place the
/// user would look, and the app already prefers asking macOS over linking
/// something to ask on its behalf. Absolute path so `PATH` cannot decide what
/// `sw_vers` means.
fn macos_version() -> Option<(u32, u32)> {
    let out = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor))
}

/// Locate the capture helper: bundled resource in production, the built binary
/// in the source tree during development. Mirrors how the dictation overlay is
/// found, for the same reasons.
fn helper_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Ok(res) = app.path().resource_dir() {
        let p = res.join("voicedumps-capture");
        if p.exists() {
            return Some(p);
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../capture-helper/voicedumps-capture");
    dev.exists().then_some(dev)
}

/// Why meeting capture cannot run here, or `None` if it can.
fn unavailable_because(app: &tauri::AppHandle) -> Option<String> {
    match macos_version() {
        Some((major, minor)) if major < MIN_MAJOR || (major == MIN_MAJOR && minor < MIN_MINOR) => {
            return Some(format!(
                "Meeting capture needs macOS {MIN_MAJOR}.{MIN_MINOR} or later. \
                 This Mac is running {major}.{minor}."
            ));
        }
        // An unreadable version is not a reason to refuse: the helper checks the
        // same thing at runtime and reports it properly. Failing open here means
        // one odd `sw_vers` cannot disable a working feature.
        _ => {}
    }
    if helper_path(app).is_none() {
        return Some(
            "The audio capture helper is missing from this build. \
             Reinstalling VoiceDumps will restore it."
                .into(),
        );
    }
    None
}

// -- capture: your microphone -----------------------------------------------

/// Record the default input device until `stop` is set.
///
/// Deliberately not reusing `dictation`'s capture: that one drives the floating
/// overlay, runs the live preview and emits `dictation-level`, none of which a
/// meeting wants. What is left after removing all three is this, and sharing it
/// would mean a pile of flags on a hot path for no gain.
/// `ready` carries the verdict on opening the device — `Ok` once audio is
/// actually flowing, or the reason it never will. The far side reports the same
/// thing by printing its header, and for the same reason: a meeting that is
/// missing a side should say so at the click, not an hour later at the save.
fn record_microphone(
    app: &tauri::AppHandle,
    path: &Path,
    stop: &Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<Track, String> {
    use cpal::traits::{DeviceTrait, StreamTrait};

    // Every early return below has to reach `ready`, or `meeting_start` waits
    // out its timeout for an answer that already exists.
    macro_rules! announce {
        ($result:expr) => {{
            match $result {
                Ok(value) => value,
                Err(problem) => {
                    let _ = ready.send(Err(problem.clone()));
                    return Err(problem);
                }
            }
        }};
    }

    // The microphone the user picked, not whichever one macOS is pointing at.
    // Dictation has always asked this way; meetings went straight to the system
    // default, so somebody who chose their headset in Settings was recorded off
    // the laptop lid for an hour without being told. The same call also decides
    // which device a video call is using, and it is the one most likely to have
    // been changed out from under us.
    let device = announce!(crate::microphone::open(
        crate::settings::microphone(app).as_deref()
    ));
    let config = announce!(device
        .default_input_config()
        .map_err(|e| format!("could not read the microphone's format: {e}")));
    let channels = config.channels() as usize;
    let rate = config.sample_rate().0;
    let format = config.sample_format();
    let cfg: cpal::StreamConfig = config.into();

    let writer = announce!(hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|e| format!("could not open the microphone recording: {e}")));
    let writer = Arc::new(Mutex::new(Some(writer)));

    let first_ms = Arc::new(Mutex::new(None::<i64>));
    let loud = Arc::new(AtomicBool::new(false));
    let window = (rate as f32 * 0.05).max(1.0) as usize;

    macro_rules! stream_for {
        ($ty:ty, $convert:expr) => {{
            let writer = writer.clone();
            let first_ms = first_ms.clone();
            let loud = loud.clone();
            let app = app.clone();
            let mut sum = 0f32;
            let mut count = 0usize;
            device
                .build_input_stream(
                    &cfg,
                    move |data: &[$ty], _: &cpal::InputCallbackInfo| {
                        let convert = $convert;
                        let mut guard = writer.lock().unwrap();
                        let Some(w) = guard.as_mut() else { return };
                        {
                            let mut slot = first_ms.lock().unwrap();
                            if slot.is_none() {
                                *slot = Some(crate::now_ms());
                            }
                        }
                        // Collected and handed over once per callback: the
                        // audio thread must not take a second lock per sample.
                        let mut batch: Vec<f32> = Vec::with_capacity(data.len() / channels + 1);
                        for frame in data.chunks(channels) {
                            let mut s = 0f32;
                            for &sample in frame {
                                s += convert(sample);
                            }
                            s /= channels as f32;
                            let quantised = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                            let _ = w.write_sample(quantised);
                            batch.push(s);
                            if quantised.unsigned_abs() > 96 {
                                loud.store(true, Ordering::Relaxed);
                            }
                            sum += s * s;
                            count += 1;
                            if count >= window {
                                emit_level(&app, "you", (sum / count as f32).sqrt());
                                sum = 0.0;
                                count = 0;
                            }
                        }
                        feed(feed_you(), rate, &batch);
                    },
                    move |e| eprintln!("[meeting] microphone stream error: {e}"),
                    None,
                )
                .map_err(|e| format!("could not open the microphone: {e}"))
        }};
    }

    let stream = announce!(match format {
        cpal::SampleFormat::F32 => stream_for!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => stream_for!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::U16 => stream_for!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        other => Err(format!("unsupported microphone format: {other:?}")),
    });

    announce!(stream
        .play()
        .map_err(|e| format!("could not start the microphone: {e}")));

    // Open, playing, and writing. Whoever is waiting can stop waiting.
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    drop(stream);
    if let Some(w) = writer.lock().unwrap().take() {
        w.finalize()
            .map_err(|e| format!("could not finish the microphone recording: {e}"))?;
    }

    let began = *first_ms.lock().unwrap();
    Ok(Track {
        path: path.to_path_buf(),
        first_sample_ms: began.unwrap_or_else(crate::now_ms),
        heard_anything: loud.load(Ordering::Relaxed),
        carried_audio: began.is_some(),
    })
}

// -- capture: everyone else -------------------------------------------------

/// The helper's opening line: `{"rate":48000.0,"channels":1}`.
#[derive(serde::Deserialize)]
struct Header {
    rate: f64,
}

/// Start the helper and read its header, so a refusal surfaces now rather than
/// at the end of an hour-long call.
///
/// This is the whole reason starting a meeting can fail with a useful sentence:
/// the helper only prints its header once the tap is live, so receiving one is
/// proof that capture is actually running. If it exits first, its stderr and
/// exit code say why.
fn start_helper(app: &tauri::AppHandle) -> Result<(Child, BufReader<std::process::ChildStdout>, u32), String> {
    let path = helper_path(app).ok_or("the audio capture helper is missing from this build")?;

    let mut child = Command::new(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start the audio capture helper: {e}"))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    // A closed pipe with nothing in it means the helper exited during startup.
    let got_header = reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false);
    if !got_header {
        return Err(explain_helper_exit(&mut child));
    }

    let header: Header = serde_json::from_str(line.trim())
        .map_err(|e| format!("the capture helper said something unexpected: {e}"))?;
    let rate = header.rate.round() as u32;
    if rate == 0 {
        return Err("the capture helper reported an impossible sample rate".into());
    }

    // Everything the helper has to say from here on, forwarded to our own
    // stderr. It was piped and then never read, which cost two things: every
    // diagnostic it wrote during a live call went into a pipe with no reader
    // and was never seen by anyone, and a long enough meeting would fill the
    // 64 KiB buffer and block the helper mid-write — on the thread doing the
    // audio, from a `note()` call meant to be harmless.
    //
    // `explain_helper_exit` still gets stderr when the header never arrives,
    // because this only takes it once startup has already succeeded.
    if let Some(err) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                eprintln!("{line}");
            }
        });
    }

    Ok((child, reader, rate))
}

/// Turn a dead helper into a sentence. Exit codes are defined in its header and
/// the two files have to agree; the stderr line is appended because it names the
/// exact CoreAudio call when the cause is not permission.
fn explain_helper_exit(child: &mut Child) -> String {
    let mut detail = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut detail);
    }
    let status = child.wait().ok().and_then(|s| s.code());
    let sentence = match status {
        Some(2) => "Meeting capture needs macOS 14.4 or later.".to_string(),
        Some(3) => "macOS did not allow VoiceDumps to record system audio. \
                    Grant it in System Settings › Privacy & Security › Audio Recording, \
                    then start the meeting again."
            .to_string(),
        _ => "The audio capture helper could not start.".to_string(),
    };
    let detail = detail.trim();
    if detail.is_empty() {
        sentence
    } else {
        format!("{sentence} ({detail})")
    }
}

/// Drain the helper into a WAV until the pipe closes.
fn record_system(
    app: &tauri::AppHandle,
    path: &Path,
    mut reader: BufReader<std::process::ChildStdout>,
    rate: u32,
    alive: &AtomicBool,
) -> Result<Track, String> {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|e| format!("could not open the meeting recording: {e}"))?;

    let mut first_ms: Option<i64> = None;
    let mut loud = false;
    let mut sum = 0f32;
    let mut count = 0usize;
    let window = (rate as f32 * 0.05).max(1.0) as usize;

    // 8 KiB is 2048 samples, about 43 ms at 48 kHz — small enough that the meter
    // stays live, large enough that we are not making a syscall per sample.
    let mut buffer = [0u8; 8192];
    // Whatever did not divide evenly into four bytes last time. A pipe read can
    // split a sample down the middle, and treating the remainder as a fresh
    // sample would turn every short read into a click.
    let mut remainder: Vec<u8> = Vec::with_capacity(4);

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("lost the meeting audio: {e}")),
        };
        if first_ms.is_none() {
            first_ms = Some(crate::now_ms());
            alive.store(true, Ordering::SeqCst);
        }

        remainder.extend_from_slice(&buffer[..read]);
        let usable = remainder.len() - (remainder.len() % 4);
        let mut batch: Vec<f32> = Vec::with_capacity(usable / 4);
        for chunk in remainder[..usable].chunks_exact(4) {
            let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            batch.push(sample);
            let quantised = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
            writer
                .write_sample(quantised)
                .map_err(|e| format!("could not write the meeting recording: {e}"))?;
            if quantised.unsigned_abs() > 96 {
                loud = true;
            }
            sum += sample * sample;
            count += 1;
            if count >= window {
                emit_level(app, "others", (sum / count as f32).sqrt());
                sum = 0.0;
                count = 0;
            }
        }
        feed(feed_others(), rate, &batch);
        remainder.drain(..usable);
    }

    writer
        .finalize()
        .map_err(|e| format!("could not finish the meeting recording: {e}"))?;

    Ok(Track {
        path: path.to_path_buf(),
        first_sample_ms: first_ms.unwrap_or_else(crate::now_ms),
        heard_anything: loud,
        carried_audio: first_ms.is_some(),
    })
}

/// How long to wait before deciding the tap is never going to produce anything.
///
/// A working tap streams continuously from the moment it starts — silence is
/// still samples — so this only has to outlast the aggregate device coming up.
/// Ten seconds is many times that and still early enough to be worth acting on.
const TAP_GRACE_SECS: u64 = 10;

/// Say something if the far side never arrives.
///
/// The failure this exists for: with a Bluetooth output device selected, the
/// aggregate device built around it can start without error and then never run
/// its IO cycle. Zero bytes, no error code, `AudioDeviceStart` returning
/// `noErr`. The recording then looks exactly like a call where nobody else
/// spoke, and the first anyone knew was a seventy-six minute meeting that
/// transcribed to two words.
///
/// The tap can be fixed while a call is still running — switching output back
/// to the built-in speakers is enough — so the only thing that made that
/// unrecoverable was not being told. Ten seconds in, it now says so.
fn watch_the_far_side(app: &tauri::AppHandle, alive: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    let app = app.clone();
    std::thread::spawn(move || {
        // Polled rather than slept through, so ending a short meeting does not
        // leave this sitting around to warn about a call that is already over.
        for _ in 0..TAP_GRACE_SECS * 4 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if stop.load(Ordering::SeqCst) || alive.load(Ordering::SeqCst) {
                return;
            }
        }

        let _ = app.emit(
            "meeting-side-missing",
            "VoiceDumps isn't hearing the other side of this call. Switching your Mac's \
sound output away from a Bluetooth device usually fixes it — your own microphone is \
still recording.",
        );
        // The window is usually behind the call, so the card says it too.
        hud::send("warn NOT HEARING THE CALL");
    });
}

/// Audio for the live preview, at whatever rate each side captured it.
///
/// A copy rather than a tee off the WAV writers: the recording on disk is the
/// thing that must not be disturbed, and the preview is allowed to miss audio
/// if it falls behind. Kept at the native rate and resampled once, in the
/// preview thread, so no audio callback ever pays for it.
struct Feed {
    rate: u32,
    samples: Vec<f32>,
}

static FEED_YOU: std::sync::OnceLock<Mutex<Feed>> = std::sync::OnceLock::new();
static FEED_OTHERS: std::sync::OnceLock<Mutex<Feed>> = std::sync::OnceLock::new();

fn feed_you() -> &'static Mutex<Feed> {
    FEED_YOU.get_or_init(|| Mutex::new(Feed { rate: 0, samples: Vec::new() }))
}

fn feed_others() -> &'static Mutex<Feed> {
    FEED_OTHERS.get_or_init(|| Mutex::new(Feed { rate: 0, samples: Vec::new() }))
}

fn feed(slot: &'static Mutex<Feed>, rate: u32, batch: &[f32]) {
    let Ok(mut guard) = slot.lock() else { return };
    guard.rate = rate;
    // A bound, because the preview may be paused behind a slow pass while the
    // microphone keeps producing. Thirty seconds of arrears is already useless
    // as a *live* preview; holding more of it only costs memory.
    if guard.samples.len() > rate as usize * 30 {
        guard.samples.clear();
    }
    guard.samples.extend_from_slice(batch);
}

/// The most recent level from each side, as `f32` bits.
///
/// The window is sent every update because its meters are the fine-grained
/// readout; the floating card is sent a snapshot on a timer instead. Two
/// capture threads emitting into one pipe forty times a second would interleave
/// their lines, and the card only needs to look alive.
static LEVEL_YOU: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static LEVEL_OTHERS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Map an RMS to the same 0..1 the dictation meter uses, so the two features
/// look like they were built by the same people.
fn emit_level(app: &tauri::AppHandle, side: &'static str, rms: f32) {
    let db = if rms > 0.0 {
        20.0 * rms.log10()
    } else {
        -120.0
    };
    let level = ((db + 72.0) / 58.0).clamp(0.0, 1.0);
    let slot = if side == "you" { &LEVEL_YOU } else { &LEVEL_OTHERS };
    slot.store(level.to_bits(), Ordering::Relaxed);
    let _ = app.emit("meeting-level", Level { side, level });
}

fn level_of(slot: &std::sync::atomic::AtomicU32) -> f32 {
    f32::from_bits(slot.load(Ordering::Relaxed))
}

/// Words as they are heard, so hovering the pill shows the meeting happening.
///
/// The same machinery as the dictation overlay's live preview — `engine::Preview`
/// on the fast model — and the same wire format, so there is one encoding to
/// reason about rather than two that drift. What differs is the source: both
/// sides of the call, mixed, because the preview answers "is this working and
/// is it hearing everyone", and one half of a conversation cannot answer that.
///
/// Best-effort throughout. A preview that cannot start, cannot keep up, or
/// stumbles on a pass simply shows less; the recording being written to disk is
/// untouched by any of it.
fn spawn_meeting_preview(app: tauri::AppHandle, stop: Arc<AtomicBool>) {
    /// Wait for at least this much new speech before spending a pass on it.
    /// Below a second or so whisper invents words rather than admitting it
    /// heard nothing.
    const MIN_CHUNK_SECS: usize = 3;

    std::thread::spawn(move || {
        // One state for the whole meeting: building it costs hundreds of
        // megabytes of KV cache, so a fresh one per pass does not merely run
        // slowly, it fails to allocate.
        let mut session: Option<crate::engine::Preview> = None;
        let mut chunk: Vec<f32> = Vec::new();
        // Kept per chunk, because a re-read replaces a chunk's words wholesale.
        let mut said: Vec<Vec<crate::engine::Heard>> = Vec::new();

        // The accurate model needs to be resident before it can re-read
        // anything, and nothing else loads it during a recording — the real
        // transcription does not start until the meeting ends. Dictation warms
        // it on key-down for exactly this reason.
        let warming = app.clone();
        std::thread::spawn(move || crate::engine::warm(&warming));

        let backlog: Backlog = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let (refined_tx, refined_rx) = std::sync::mpsc::channel();
        let refining = spawn_refine(&app, backlog.clone(), refined_tx, stop.clone());

        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(400));

            // Take everything both sides have produced since last time.
            let mine = drain(feed_you());
            let theirs = drain(feed_others());
            if mine.is_empty() && theirs.is_empty() {
                continue;
            }

            // Mixed, not concatenated: they are the same stretch of time from
            // two microphones, and playing them one after the other would make
            // every exchange sound like two monologues.
            let frames = mine.len().max(theirs.len());
            for frame in 0..frames {
                let a = mine.get(frame).copied().unwrap_or(0.0);
                let b = theirs.get(frame).copied().unwrap_or(0.0);
                chunk.push((a + b).clamp(-1.0, 1.0));
            }

            if chunk.len() < ENGINE_RATE as usize * MIN_CHUNK_SECS {
                continue;
            }

            if session.is_none() {
                session = crate::engine::Preview::start(&app);
            }
            let Some(preview) = session.as_mut() else {
                // The engine is busy or still warming. Drop what we have rather
                // than letting it grow into a minute-long chunk that would take
                // a minute to transcribe.
                chunk.clear();
                continue;
            };

            let heard = preview.step(&chunk, stop.clone());
            let audio = std::mem::take(&mut chunk);

            let Some(words) = heard else { continue };
            if words.is_empty() {
                continue;
            }

            said.push(words);
            // Hand the same audio to the accurate model. It replaces this
            // chunk's words in place when it is done, which is why the words
            // are kept per chunk rather than as one flat list: a re-read may
            // hear a different number of them.
            if refining {
                let index = said.len() - 1;
                let mut queue = backlog.lock().unwrap();
                queue.push_back((index, audio));
                // Two chunks is roughly what is still on screen ahead of the
                // reader; older than that and the correction lands after the
                // words have scrolled away.
                while queue.len() > 2 {
                    queue.pop_front();
                }
            }

            // Whatever the second pass finished while we were listening.
            while let Ok((index, better)) = refined_rx.try_recv() {
                if let Some(slot) = said.get_mut(index) {
                    *slot = better;
                }
            }

            send_words(&said);
        }
    });
}

/// The whole visible tail, re-sent every time.
///
/// A correction replaces a chunk wholesale, so there is no offset to patch —
/// and at a few hundred bytes a redraw, sending everything is cheaper to reason
/// about than anything cleverer.
fn send_words(said: &[Vec<crate::engine::Heard>]) {
    const VISIBLE: usize = 120;
    let flat: Vec<&crate::engine::Heard> = said.iter().flatten().collect();
    let tail = &flat[flat.len().saturating_sub(VISIBLE)..];

    // `<digit0-9><word>` per field, exactly what the dictation overlay parses.
    let payload = tail
        .iter()
        .map(|h| {
            let digit = ((h.confidence * 9.0).round() as i32).clamp(0, 9);
            format!("{digit}{}", h.word.trim())
        })
        .filter(|field| field.len() > 1)
        .collect::<Vec<_>>()
        .join(" ");
    if !payload.is_empty() {
        hud::send(&format!("partial {payload}"));
    }
}

/// Re-read chunks on the accurate model, in the background.
///
/// Returns false when there is nothing to gain — on a machine using `small` for
/// real transcription there is no better model to promote the words to, and the
/// preview is already showing the final answer.
type Backlog = Arc<Mutex<std::collections::VecDeque<(usize, Vec<f32>)>>>;

fn spawn_refine(
    app: &tauri::AppHandle,
    backlog: Backlog,
    done: std::sync::mpsc::Sender<(usize, Vec<crate::engine::Heard>)>,
    stop: Arc<AtomicBool>,
) -> bool {
    if crate::engine::auto_model() != crate::engine::ModelSize::Medium {
        return false;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let mut refiner: Option<crate::engine::Refine> = None;
        while !stop.load(Ordering::SeqCst) {
            let next = backlog.lock().unwrap().pop_front();
            let Some((index, audio)) = next else {
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue;
            };
            if refiner.is_none() {
                match crate::engine::Refine::start(&app) {
                    crate::engine::Refinable::Ready(r) => refiner = Some(r),
                    // The engine is still warming. Put the chunk back and wait.
                    crate::engine::Refinable::NotYet => {
                        backlog.lock().unwrap().push_front((index, audio));
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        continue;
                    }
                    crate::engine::Refinable::Never => return,
                }
            }
            let Some(r) = refiner.as_mut() else { continue };
            if let Some(words) = r.step(&audio, stop.clone()) {
                if !words.is_empty() {
                    let _ = done.send((index, words));
                }
            }
        }
    });
    true
}

/// Take everything buffered for one side, resampled to the engine's rate.
fn drain(slot: &'static Mutex<Feed>) -> Vec<f32> {
    let (rate, samples) = {
        let Ok(mut guard) = slot.lock() else {
            return Vec::new();
        };
        (guard.rate, std::mem::take(&mut guard.samples))
    };
    if samples.is_empty() || rate == 0 {
        return Vec::new();
    }
    if rate == ENGINE_RATE {
        samples
    } else {
        crate::engine::to_engine_rate(&samples, rate)
    }
}

/// Keep the floating card fed for as long as the meeting runs.
fn spawn_hud_ticker(stop: Arc<AtomicBool>, started_ms: i64) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            hud::send(&format!(
                "levels {:.3} {:.3}",
                level_of(&LEVEL_YOU),
                level_of(&LEVEL_OTHERS)
            ));
            hud::send(&format!(
                "elapsed {}",
                ((crate::now_ms() - started_ms) / 1000).max(0)
            ));
            // Twenty times a second. Ten was enough to prove the recording was
            // alive but not enough to look like a voice: with seven bars it
            // took most of a second for a syllable to cross the pill.
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
}

// -- assembling the transcript ----------------------------------------------

/// Interleave two transcribed sides into one conversation.
///
/// `offset_secs` shifts the far side onto the microphone's clock: whichever
/// capture started first is time zero, and the other is pushed forward by the
/// gap between them. Without this the two sides drift apart by however long the
/// helper took to spawn — a few hundred milliseconds, which is enough to put a
/// reply before the question it answers.
fn interleave(mine: &[Value], theirs: &[Value], offset_secs: f64) -> Vec<Value> {
    let mut all: Vec<Value> = Vec::with_capacity(mine.len() + theirs.len());

    let mut push = |segments: &[Value], speaker: &str, side: &str, shift: f64| {
        for segment in segments {
            let mut segment = segment.clone();
            if let Some(object) = segment.as_object_mut() {
                for key in ["start", "end"] {
                    if let Some(value) = object.get(key).and_then(Value::as_f64) {
                        object.insert(key.into(), json!((value + shift).max(0.0)));
                    }
                }
                // Word timings drive the follow-along highlight during playback,
                // so they need the same shift as the segment around them.
                if let Some(words) = object.get_mut("words").and_then(Value::as_array_mut) {
                    for word in words.iter_mut() {
                        if let Some(word) = word.as_object_mut() {
                            for key in ["start", "end"] {
                                if let Some(value) = word.get(key).and_then(Value::as_f64) {
                                    word.insert(key.into(), json!((value + shift).max(0.0)));
                                }
                            }
                        }
                    }
                }
                object.insert("speaker".into(), json!(speaker));
                // Which track this came off, kept separately from what it is
                // called. The label is the user's to change — naming yourself
                // is the first thing anyone does — and the reading view colours
                // the two sides differently, which would quietly stop working
                // the moment "You" became "Naveen" if the colour were decided
                // by reading the name.
                object.insert("side".into(), json!(side));
            }
            all.push(segment);
        }
    };

    // A positive offset means the far side started late, so it moves forward;
    // a negative one means the microphone did, and it moves instead. Only ever
    // one of the two is shifted, so the earlier track keeps an honest zero.
    push(
        mine,
        LABEL_YOU,
        SIDE_YOU,
        if offset_secs < 0.0 { -offset_secs } else { 0.0 },
    );
    push(
        theirs,
        LABEL_THEM,
        SIDE_THEM,
        if offset_secs > 0.0 { offset_secs } else { 0.0 },
    );

    all.sort_by(|a, b| {
        let (x, y) = (
            a["start"].as_f64().unwrap_or(0.0),
            b["start"].as_f64().unwrap_or(0.0),
        );
        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
    });
    all
}

/// Group the conversation into turns: consecutive segments from one speaker
/// become one paragraph.
///
/// This is what makes a meeting readable. Whisper's segments are breath-length,
/// so rendering them one per line turns a two-minute answer into forty ragged
/// fragments; a turn is the unit a person actually remembers.
pub fn turns(segments: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for segment in segments {
        let speaker = segment["speaker"].as_str().unwrap_or(LABEL_YOU);
        let text = segment["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let start = segment["start"].as_f64().unwrap_or(0.0);
        let end = segment["end"].as_f64().unwrap_or(start);

        let same_speaker = out
            .last()
            .and_then(|t| t["speaker"].as_str())
            .is_some_and(|s| s == speaker);

        // Carried, not dropped. These are what light each word as the audio
        // reaches it, and without them a meeting falls back to highlighting a
        // whole turn at a time — which for a two-minute answer means a
        // paragraph lit for two minutes. Every other kind of note in the app
        // follows along word by word; a recorded meeting is the one people are
        // most likely to scrub around in, so it needs it most.
        //
        // `interleave` has already shifted these onto the shared clock, so they
        // can be taken as they are.
        let words = segment
            .get("words")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if same_speaker {
            let last = out.last_mut().expect("checked above");
            let joined = format!("{} {}", last["text"].as_str().unwrap_or(""), text);
            last["text"] = json!(joined.trim());
            last["end"] = json!(end);
            if let Some(existing) = last.get_mut("words").and_then(Value::as_array_mut) {
                existing.extend(words);
            }
        } else {
            out.push(json!({
                "speaker": speaker,
                // Carried so the reading view can colour the two sides apart
                // without reading the label, which the user may have renamed.
                "side": segment["side"].as_str().unwrap_or(SIDE_THEM),
                "start": start,
                "end": end,
                "text": text,
                "words": words,
            }));
        }
    }
    out
}

/// The flat text of a conversation: one turn per paragraph, each attributed.
///
/// Shared by the save path and by renaming a speaker, and it has to be, because
/// this string is what the overview reads. A rename that relabelled the turns on
/// screen but not this would leave the next overview attributing decisions to a
/// name nobody can see any more.
pub fn transcript_text(paragraphs: &[Value]) -> String {
    paragraphs
        .iter()
        .map(|turn| {
            let speaker = turn["speaker"].as_str().unwrap_or("").trim();
            let said = turn["text"].as_str().unwrap_or("");
            if speaker.is_empty() {
                said.to_string()
            } else {
                format!("{speaker}: {said}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Give one side of a conversation a name, everywhere it appears.
///
/// `None` when the meeting has nobody by that name — which is what a stale
/// window asking to rename a speaker who was renamed a moment ago looks like,
/// and is a refusal rather than a silent no-op.
///
/// Both the turns and the raw segments are rewritten. The segments are what a
/// future re-grouping would read, and leaving them saying "Others" would undo
/// the rename the first time anything re-derived the paragraphs.
pub fn relabel(
    paragraphs: &Value,
    segments: &Value,
    from: &str,
    to: &str,
) -> Option<(Value, Value, String)> {
    fn rewrite(list: &Value, from: &str, to: &str) -> (Value, usize) {
        let mut hits = 0;
        let Some(items) = list.as_array() else {
            return (list.clone(), 0);
        };
        let out = items
            .iter()
            .map(|item| {
                let mut item = item.clone();
                if item["speaker"].as_str() == Some(from) {
                    item["speaker"] = json!(to);
                    hits += 1;
                }
                item
            })
            .collect::<Vec<_>>();
        (json!(out), hits)
    }

    let (paragraphs, turns_hit) = rewrite(paragraphs, from, to);
    let (segments, _) = rewrite(segments, from, to);
    if turns_hit == 0 {
        return None;
    }

    let text = transcript_text(paragraphs.as_array().unwrap_or(&Vec::new()));
    Some((paragraphs, segments, text))
}

/// Mix the two tracks into one file for playback.
///
/// The transcript keeps the sides apart; the audio should not. Someone replaying
/// a meeting wants to hear the conversation, not one half of it, and the media
/// library stores exactly one file per transcript.
fn mix(you: &Track, them: &Track, offset_secs: f64, dest: &Path) -> Result<f64, String> {
    let mut mine = read_wav_16k(&you.path)?;
    let mut theirs = read_wav_16k(&them.path)?;

    // Pad whichever started later, so both sit on the same timeline as the
    // transcript that was aligned the same way.
    let lead = (offset_secs.abs() * ENGINE_RATE as f64) as usize;
    if offset_secs > 0.0 {
        theirs.splice(0..0, std::iter::repeat_n(0.0, lead));
    } else if offset_secs < 0.0 {
        mine.splice(0..0, std::iter::repeat_n(0.0, lead));
    }

    let frames = mine.len().max(theirs.len());
    let mut writer = hound::WavWriter::create(
        dest,
        hound::WavSpec {
            channels: 1,
            sample_rate: ENGINE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|e| format!("could not open the mixed recording: {e}"))?;

    for frame in 0..frames {
        let a = mine.get(frame).copied().unwrap_or(0.0);
        let b = theirs.get(frame).copied().unwrap_or(0.0);
        // Straight sum, clipped. Halving each side would make a one-sided
        // meeting — which most of a call is — quietly half as loud as the
        // dictation recordings sitting next to it in the same list.
        let sample = (a + b).clamp(-1.0, 1.0);
        writer
            .write_sample((sample * 32767.0) as i16)
            .map_err(|e| format!("could not write the mixed recording: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("could not finish the mixed recording: {e}"))?;

    Ok(frames as f64 / ENGINE_RATE as f64)
}

/// Read one of our own 16-bit mono WAVs back as 16 kHz f32.
fn read_wav_16k(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("could not reopen a recording: {e}"))?;
    let rate = reader.spec().sample_rate;
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .filter_map(Result::ok)
        .map(|s| s as f32 / 32768.0)
        .collect();
    Ok(if rate == ENGINE_RATE {
        samples
    } else {
        crate::engine::to_engine_rate(&samples, rate)
    })
}

// -- the floating card ------------------------------------------------------

/// The HUD that sits over whatever app the call is in.
///
/// A meeting happens in someone else's window, so every control for it has to
/// live above that window. Driven exactly like the dictation overlay — one
/// command per line on its stdin — with one addition the pill does not need: it
/// answers back, because its buttons are the point.
mod hud {
    use std::io::{BufRead, BufReader, Write};
    use std::path::Path;
    use std::process::{ChildStdin, Command, Stdio};
    use std::sync::{Mutex, OnceLock};

    use tauri::Manager;

    static PIPE: OnceLock<Mutex<Option<ChildStdin>>> = OnceLock::new();

    fn pipe() -> &'static Mutex<Option<ChildStdin>> {
        PIPE.get_or_init(|| Mutex::new(None))
    }

    fn locate(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
        if let Ok(res) = app.path().resource_dir() {
            let p = res.join("voicedumps-hud");
            if p.exists() {
                return Some(p);
            }
        }
        let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hud-helper/voicedumps-hud");
        dev.exists().then_some(dev)
    }

    /// One line per press, coming back the other way.
    pub fn spawn(app: tauri::AppHandle) {
        let Some(path) = locate(&app) else {
            eprintln!("[meeting] HUD helper missing; the floating card is unavailable");
            return;
        };
        let mut child = match Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                eprintln!("[meeting] HUD helper failed to spawn: {e}");
                return;
            }
        };

        *pipe().lock().unwrap() = child.stdin.take();
        let Some(stdout) = child.stdout.take() else { return };

        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(word) = line else { break };
                match word.trim() {
                    // Starting from the card is the whole reason it exists: the
                    // answer to "shall I take notes" should not cost a Cmd-Tab
                    // away from the meeting it is about.
                    "take-notes" => {
                        if let Err(e) = super::meeting_start(app.clone()) {
                            let _ = tauri::Emitter::emit(&app, "meeting-failed", e);
                            send("hide");
                        }
                    }
                    "stop" => {
                        let handle = app.clone();
                        // Transcription takes minutes; doing it on the thread
                        // that reads the card would wedge every later press.
                        std::thread::spawn(move || super::finish_and_report(handle));
                    }
                    // Refused, or simply left to time out. Either way the
                    // offer is over, and the window's copy of it has to go too
                    // or it sits there after the floating one has left.
                    "dismiss" => {
                        send("hide");
                        let _ = tauri::Emitter::emit(&app, "meeting-offer-closed", ());
                    }
                    _ => {}
                }
            }
            let _ = child.wait();
        });
    }

    pub fn send(command: &str) {
        if let Ok(mut guard) = pipe().lock() {
            if let Some(stdin) = guard.as_mut() {
                let _ = writeln!(stdin, "{command}");
                let _ = stdin.flush();
            }
        }
    }
}

/// Launch the floating card. Best-effort: without it every control still
/// exists in the window, which is where they were until now.
pub fn spawn_hud(app: tauri::AppHandle) {
    hud::spawn(app);
}

// -- noticing that a call started -------------------------------------------
//
// Recording a meeting you have to remember to start is a feature you will
// forget to use. The helper's `--watch-input` mode reports which apps are using
// the microphone; this turns that into one offer, at the moment a call begins.
//
// Nothing here starts recording on its own. The offer is an offer.

/// One line from the watcher.
#[derive(serde::Deserialize)]
struct InputEvent {
    event: String,
    bundle: String,
    name: String,
}

#[derive(Serialize, Clone)]
pub struct Detected {
    pub bundle: String,
    pub name: String,
}

/// Every app currently holding the microphone that could be a call.
///
/// Written by the detector, read when a recording starts so it knows what it is
/// following, and again when one stops so it knows whether the call is over.
static ON_THE_MIC: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn on_the_mic() -> &'static Mutex<std::collections::HashSet<String>> {
    ON_THE_MIC.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Apps whose use of the microphone is never a meeting.
///
/// Dictation and voice assistants hold the input device exactly like a call
/// does, and offering to take notes on someone talking to Siri is the kind of
/// thing that gets a feature switched off for good.
const NEVER_A_MEETING: [&str; 6] = [
    // Ourselves. The globe key opens the microphone on every dictation, and an
    // app that offers to take notes on its own dictation is a joke at its own
    // expense. Both product identifiers, because this file is built into both.
    "dev.heynaavi.voicedump",
    "ai.qwee.voicedumps",
    "com.apple.Siri",
    "com.apple.siri",
    "com.apple.assistantd",
    "com.apple.CoreSpeech",
];

/// Is this microphone user worth offering to take notes on?
///
/// Deliberately a pure function of the bundle identifier: this is the whole
/// policy, it is the part most likely to need adjusting as people report what
/// their Mac does, and it should be changeable without a call in front of it.
fn worth_offering(bundle: &str) -> bool {
    // No bundle identifier means a command-line process — ffmpeg, a recording
    // script, an audio utility. Whatever it is, nobody is in a meeting with it.
    if bundle.is_empty() {
        return false;
    }
    if NEVER_A_MEETING.contains(&bundle) {
        return false;
    }
    // Helpers carry their parent's identifier plus a suffix, so matching the
    // prefix covers `dev.heynaavi.voicedump.helper` without listing it.
    NEVER_A_MEETING
        .iter()
        .all(|ignored| !bundle.starts_with(&format!("{ignored}.")))
}

/// End the recording when the call it was following ends.
///
/// Leaving the meeting is how people end a meeting; going back to the app to
/// press stop is an extra step nobody remembers, and forgetting it means an
/// hour of silence on the end of the transcript.
///
/// The pause before acting is not politeness — a conferencing app drops the
/// microphone for a moment when someone mutes, changes device or shares a
/// screen, and acting on the first quiet poll would cut the recording in half.
fn stop_if_the_call_is_over(app: &tauri::AppHandle) {
    let following = {
        let state = app.state::<MeetingState>();
        let guard = state.0.lock().unwrap();
        match guard.as_ref() {
            Some(session) if !session.following.is_empty() => session.following.clone(),
            // Not recording, or recording something nobody asked us to follow.
            _ => return,
        }
    };

    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(4));
        {
            let live = on_the_mic().lock().unwrap();
            if following.iter().any(|bundle| live.contains(bundle)) {
                return; // came back — a mute, not an ending
            }
        }
        // Still recording the same meeting? Then it is over.
        let still_running = {
            let state = app.state::<MeetingState>();
            let guard = state.0.lock().unwrap();
            guard.as_ref().is_some_and(|s| s.following == following)
        };
        if still_running {
            eprintln!("[meeting] the call ended; wrapping up");
            finish_and_report(app);
        }
    });
}

/// Watch for calls for as long as the app runs.
///
/// Best-effort by design: if the helper is missing or the Mac is too old, this
/// thread reports it once and stops. Meeting capture still works by hand, and a
/// missing convenience must not become a missing app.
pub fn spawn_detector(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        if let Some(reason) = unavailable_because(&app) {
            eprintln!("[meeting] not watching for calls: {reason}");
            return;
        }
        let Some(path) = helper_path(&app) else { return };

        let mut child = match Command::new(&path)
            .arg("--watch-input")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                eprintln!("[meeting] could not watch for calls: {e}");
                return;
            }
        };

        let Some(stdout) = child.stdout.take() else { return };
        let reader = BufReader::new(stdout);

        // Which apps we have already spoken up about. An app that holds the
        // microphone for an hour should produce one offer, not one every poll.
        let mut offered: std::collections::HashSet<String> = std::collections::HashSet::new();

        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Ok(event) = serde_json::from_str::<InputEvent>(&line) else {
                continue;
            };
            if !worth_offering(&event.bundle) {
                continue;
            }

            match event.event.as_str() {
                "started" => {
                    on_the_mic().lock().unwrap().insert(event.bundle.clone());
                    // Already recording? Then the answer to "shall I take
                    // notes" is visibly yes, and asking again is noise.
                    let recording = app.state::<MeetingState>().0.lock().unwrap().is_some();
                    if recording || !offered.insert(event.bundle.clone()) {
                        continue;
                    }
                    hud::send(&format!("detected {}", event.name));
                    let _ = app.emit(
                        "meeting-detected",
                        Detected {
                            bundle: event.bundle,
                            name: event.name,
                        },
                    );
                }
                "stopped" => {
                    on_the_mic().lock().unwrap().remove(&event.bundle);
                    offered.remove(&event.bundle);
                    let _ = app.emit("meeting-ended", &event.bundle);
                    stop_if_the_call_is_over(&app);
                }
                _ => {}
            }
        }

        let _ = child.wait();
    });
}

// -- commands ---------------------------------------------------------------

/// Whether this Mac can record meetings, and whether it is doing so now.
#[tauri::command]
pub fn meeting_status(app: tauri::AppHandle, state: tauri::State<MeetingState>) -> Capability {
    let recording = state.0.lock().unwrap().is_some();
    match unavailable_because(&app) {
        Some(reason) => Capability {
            available: false,
            reason,
            recording,
        },
        None => Capability {
            available: true,
            reason: String::new(),
            recording,
        },
    }
}

/// Open the privacy pane where system-audio recording is granted.
///
/// macOS prompts once per binary, so after a refusal there is no way back to
/// the decision from inside the app — this is it.
///
/// The anchor follows Apple's convention of naming the pane after the TCC
/// service (`kTCCServiceAudioCapture`), but anchors are undocumented and have
/// been renamed between releases. A wrong one is harmless — System Settings
/// opens on Privacy & Security itself rather than failing — and the UI names
/// the destination in words next to this button for exactly that reason.
#[tauri::command]
pub fn open_audio_capture_settings() {
    let _ = Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture")
        .spawn();
}

/// Begin recording both sides.
///
/// Fails fast and loudly: the helper's tap is created before this returns, so a
/// missing permission is a sentence on screen at the moment someone clicks
/// Start, not a silent half-recording discovered an hour later.
#[tauri::command]
pub fn meeting_start(app: tauri::AppHandle) -> Result<(), String> {
    {
        let state = app.state::<MeetingState>();
        if state.0.lock().unwrap().is_some() {
            return Err("a meeting is already being recorded".into());
        }
    }
    if let Some(reason) = unavailable_because(&app) {
        return Err(reason);
    }

    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("meetings");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let started_ms = crate::now_ms();
    let mic_path = dir.join(format!("meeting-{started_ms}-you.wav"));
    let sys_path = dir.join(format!("meeting-{started_ms}-others.wav"));

    // The far side first: it is the half that can be refused, and there is no
    // point opening a microphone we are about to abandon.
    //
    // The child keeps its stdin for the whole meeting — dropping that pipe is
    // how `finish` tells the helper to shut the tap down.
    let (child, reader, rate) = start_helper(&app)?;

    let stop = Arc::new(AtomicBool::new(false));
    let hud_stop = stop.clone();

    // Set by the far-side recorder the instant a single byte arrives.
    let tap_alive = Arc::new(AtomicBool::new(false));
    let sys = {
        let app = app.clone();
        let path = sys_path.clone();
        let alive = tap_alive.clone();
        std::thread::spawn(move || record_system(&app, &path, reader, rate, &alive))
    };
    watch_the_far_side(&app, tap_alive, stop.clone());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mic = {
        let app = app.clone();
        let path = mic_path.clone();
        let stop = stop.clone();
        std::thread::spawn(move || record_microphone(&app, &path, &stop, ready_tx))
    };

    // Wait for your own side to actually open before calling this a meeting.
    // Three seconds is far longer than opening a CoreAudio input takes and far
    // shorter than someone would sit staring at a button; a timeout is treated
    // as working, because refusing to record over a slow device would be a
    // worse answer than recording and finding out.
    // `Ok(Ok)` is open and running; `Err` is the timeout, treated as working.
    if let Ok(Err(problem)) = ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        // Take the far side down with it rather than leaving a tap open on a
        // meeting that was never started.
        stop.store(true, Ordering::SeqCst);
        drop(child);
        let _ = mic.join();
        let _ = sys.join();
        cleanup(&[&mic_path, &sys_path]);
        return Err(problem);
    }

    // Whatever is on the microphone right now is what this recording is about.
    let following = on_the_mic().lock().unwrap().clone();

    let state = app.state::<MeetingState>();
    *state.0.lock().unwrap() = Some(Session {
        stop,
        started_ms,
        following,
        child: Some(child),
        mic,
        sys,
    });
    // Levels start at whatever the last meeting left behind; zero them so the
    // card does not open mid-waveform.
    LEVEL_YOU.store(0, Ordering::Relaxed);
    LEVEL_OTHERS.store(0, Ordering::Relaxed);
    // Stale audio from the last meeting would otherwise be the first thing the
    // new one transcribes.
    let _ = drain(feed_you());
    let _ = drain(feed_others());

    hud::send("recording");
    spawn_hud_ticker(hud_stop.clone(), started_ms);
    spawn_meeting_preview(app.clone(), hud_stop);

    let _ = app.emit("meeting-started", started_ms);
    Ok(())
}

/// Stop, save, and tell the window how it went.
///
/// The command path returns its result to the caller, which is the window. The
/// floating card has no caller — so it goes through here, where the outcome is
/// announced as an event instead and both entry points end the same way.
fn finish_and_report(app: tauri::AppHandle) {
    match finish(app.clone()) {
        Ok(id) => {
            let _ = app.emit("meeting-saved", &id);
        }
        Err(problem) => {
            let _ = app.emit("meeting-failed", problem);
        }
    }
}

/// Stop, transcribe both sides, and save one conversation.
#[tauri::command]
pub async fn meeting_stop(app: tauri::AppHandle) -> Result<(), String> {
    // Every step past here is blocking — two transcriptions, a mixdown and a
    // media transcode — and none of it belongs on the thread that paints.
    //
    // The result is announced rather than returned: the card can stop a meeting
    // too, and it has no promise to resolve. One outcome, one path.
    tauri::async_runtime::spawn_blocking(move || finish_and_report(app))
        .await
        .map_err(|e| format!("the meeting could not be finished: {e}"))
}

fn finish(app: tauri::AppHandle) -> Result<String, String> {
    let session = {
        let state = app.state::<MeetingState>();
        let mut guard = state.0.lock().unwrap();
        guard.take().ok_or("no meeting is being recorded")?
    };

    let Session {
        stop,
        started_ms,
        following: _,
        mut child,
        mic,
        sys,
    } = session;

    let progress = |stage: &str, fraction: f64| {
        let _ = app.emit("meeting-progress", json!({ "stage": stage, "progress": fraction }));
        // The floating card is the only one of the two surfaces a person is
        // actually looking at when a call ends — the window is usually behind
        // the browser they just left.
        hud::send(&format!("progress {fraction:.3} {stage}"));
    };

    hud::send("finishing");
    progress("Stopping", 0.02);
    // Closing the helper's stdin is its cue to shut the tap down and flush.
    if let Some(child) = child.as_mut() {
        drop(child.stdin.take());
    }
    stop.store(true, Ordering::SeqCst);

    let mine = mic
        .join()
        .map_err(|_| "the microphone recorder stopped unexpectedly".to_string())??;
    let theirs = sys
        .join()
        .map_err(|_| "the meeting recorder stopped unexpectedly".to_string())??;
    if let Some(mut child) = child {
        let _ = child.wait();
    }

    if !mine.heard_anything && !theirs.heard_anything {
        cleanup(&[&mine.path, &theirs.path]);
        hud::send("hide");
        return Err(
            "That meeting was silent on both sides, so there was nothing to transcribe.".into(),
        );
    }

    // A tap that delivered no bytes at all is a broken capture, not a quiet
    // call, and the difference is worth saying out loud even though the meeting
    // is about to save successfully. Announced rather than returned as an error:
    // the user's own side is real and worth keeping, and refusing to save it
    // would turn half a meeting into none.
    if !theirs.carried_audio {
        let _ = app.emit(
            "meeting-side-missing",
            "Only your side of that meeting was recorded — VoiceDumps never received any \
audio from the call. Switching your Mac's sound output away from a Bluetooth device \
usually fixes it.",
        );
    }

    // Positive: the far side started after the microphone did.
    let offset_secs = (theirs.first_sample_ms - mine.first_sample_ms) as f64 / 1000.0;

    // Transcribing a track that never carried sound costs a minute to learn
    // nothing, so skip it and let the other side stand alone.
    progress("Transcribing you", 0.1);
    let my_run = if mine.heard_anything {
        Some(crate::engine::transcribe(
            &app,
            &mine.path.to_string_lossy(),
            |_, fraction| progress("Transcribing you", 0.1 + 0.4 * fraction),
        )?)
    } else {
        None
    };

    progress("Transcribing the others", 0.5);
    let their_run = if theirs.heard_anything {
        Some(crate::engine::transcribe(
            &app,
            &theirs.path.to_string_lossy(),
            |_, fraction| progress("Transcribing the others", 0.5 + 0.4 * fraction),
        )?)
    } else {
        None
    };

    let empty: Vec<Value> = Vec::new();
    let my_segments = my_run
        .as_ref()
        .and_then(|r| r["segments"].as_array())
        .cloned()
        .unwrap_or_else(|| empty.clone());
    let their_segments = their_run
        .as_ref()
        .and_then(|r| r["segments"].as_array())
        .cloned()
        .unwrap_or(empty);

    // A side that recorded sound and came back with no words at all.
    //
    // This is how a broken meeting looks from the inside, and until now it
    // looked like nothing: every segment attributed to one speaker, no error,
    // no warning, an hour of conversation saved as a monologue. The fifty-one
    // minute call that prompted this came back with all one hundred and
    // seventy-eight of its segments marked "Others" and not one word of the
    // user's own side, and the first anyone knew was reading it.
    //
    // Whatever the cause — a muted microphone, an input device the call took
    // for itself, a decode that fell over — a transcript that is missing half a
    // conversation should say so at the moment it is saved, not leave someone
    // to notice a month later that they are quoted nowhere in their own meeting.
    for (segments, track, complaint) in [
        (
            &my_segments,
            &mine,
            "Nothing you said in that meeting could be made out, so the transcript is all \
other people. If you were speaking, check which microphone VoiceDumps is listening to \
in Settings — it may not be the one the call was using.",
        ),
        (
            &their_segments,
            &theirs,
            "Nothing the other side said in that meeting could be made out, so the \
transcript is all you. The call's audio reached VoiceDumps but arrived as something it \
could not read as speech.",
        ),
    ] {
        if segments.is_empty() && track.heard_anything {
            // The recording is saved regardless, and saying so matters: this
            // reads like a failure and the audio is still all there.
            let _ = app.emit(
                "meeting-side-missing",
                format!("{complaint} The recording itself was kept."),
            );
        }
    }

    let segments = interleave(&my_segments, &their_segments, offset_secs);
    let paragraphs = turns(&segments);
    let text = transcript_text(&paragraphs);

    if text.trim().is_empty() {
        cleanup(&[&mine.path, &theirs.path]);
        hud::send("hide");
        return Err("Nothing was said in that meeting that could be transcribed.".into());
    }

    progress("Mixing the recording", 0.92);
    let mixed = mine.path.with_file_name(format!("meeting-{started_ms}.wav"));
    let duration = mix(&mine, &theirs, offset_secs, &mixed)?;

    // Peaks come from the mix, not from either side: the waveform under the
    // player should be the conversation the player is playing.
    let peaks = crate::engine::peaks_for(&mixed.to_string_lossy()).unwrap_or_default();

    // The run to record is whichever side actually ran; when both did they used
    // the same resident model, so either is the truthful answer for "which
    // model" and the times are worth adding rather than picking one.
    let run = crate::engine::Run {
        model: my_run
            .as_ref()
            .or(their_run.as_ref())
            .and_then(|r| r["model"].as_str())
            .unwrap_or_default()
            .to_string(),
        millis: [my_run.as_ref(), their_run.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|r| r["transcribe_ms"].as_i64())
            .sum(),
    };

    progress("Saving", 0.97);
    let id = crate::insert_transcript(
        &app,
        &meeting_title(started_ms),
        &mixed.to_string_lossy(),
        duration,
        Some("en"),
        &text,
        json!(paragraphs),
        json!(segments),
        json!(peaks),
        "meeting",
        run,
        // Yes, but not here. `insert_transcript` leaves meetings alone and the
        // overview below names it instead, once it has read the whole call —
        // until then the time it happened stands, which is a real answer rather
        // than a placeholder.
        true,
    )?;

    // The per-side WAVs and the mix have all been folded into the media library
    // by now; leaving them would double what a meeting costs on disk.
    cleanup(&[&mine.path, &theirs.path, &mixed]);

    hud::send("hide");
    progress("Done", 1.0);

    Ok(id)
}

/// A meeting is most findable by when it happened.
fn meeting_title(started_ms: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(started_ms).single() {
        Some(when) => format!("Meeting — {}", when.format("%-d %b, %-I:%M %p")),
        None => "Meeting".to_string(),
    }
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

// -- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(start: f64, end: f64, text: &str) -> Value {
        json!({ "start": start, "end": end, "text": text, "words": [
            { "start": start, "end": end, "text": text }
        ] })
    }

    #[test]
    fn interleaving_orders_a_conversation_by_time() {
        let mine = vec![segment(0.0, 1.0, "hello"), segment(4.0, 5.0, "sounds good")];
        let theirs = vec![segment(2.0, 3.0, "hi there")];

        let merged = interleave(&mine, &theirs, 0.0);

        let spoken: Vec<&str> = merged.iter().map(|s| s["text"].as_str().unwrap()).collect();
        assert_eq!(spoken, ["hello", "hi there", "sounds good"]);
    }

    #[test]
    fn every_segment_is_attributed() {
        let merged = interleave(&[segment(0.0, 1.0, "mine")], &[segment(0.5, 1.5, "theirs")], 0.0);
        assert_eq!(merged[0]["speaker"], LABEL_YOU);
        assert_eq!(merged[1]["speaker"], LABEL_THEM);
    }

    /// The whole point of tracking when each capture started. A late helper must
    /// not put the answer before the question.
    #[test]
    fn a_late_far_side_is_pushed_onto_the_microphones_clock() {
        // Both tracks think they start at zero, but the tap opened 2s later, so
        // its "0.0" is really the microphone's 2.0.
        let mine = vec![segment(0.0, 1.0, "how are you")];
        let theirs = vec![segment(0.0, 1.0, "good thanks")];

        let merged = interleave(&mine, &theirs, 2.0);

        assert_eq!(merged[0]["text"], "how are you");
        assert_eq!(merged[1]["text"], "good thanks");
        assert_eq!(merged[1]["start"], 2.0);
        // Word timings move with their segment or the follow-along highlight
        // drifts off the audio.
        assert_eq!(merged[1]["words"][0]["start"], 2.0);
    }

    #[test]
    fn a_late_microphone_is_shifted_instead() {
        let merged = interleave(&[segment(0.0, 1.0, "mine")], &[segment(0.0, 1.0, "theirs")], -2.0);
        assert_eq!(merged[0]["text"], "theirs");
        assert_eq!(merged[0]["start"], 0.0);
        assert_eq!(merged[1]["start"], 2.0);
    }

    /// Shifting must never produce a negative timestamp: the player treats the
    /// timeline as starting at zero and would never reach the segment.
    #[test]
    fn shifted_times_never_go_negative() {
        let merged = interleave(&[segment(0.0, 1.0, "mine")], &[], -5.0);
        assert!(merged[0]["start"].as_f64().unwrap() >= 0.0);
    }

    /// A `Track` shaped like a capture that never delivered a byte.
    fn dead_track() -> Track {
        Track {
            path: PathBuf::from("/dev/null"),
            first_sample_ms: 0,
            heard_anything: false,
            carried_audio: false,
        }
    }

    #[test]
    fn turns_keep_the_word_timings_that_drive_follow_along() {
        let segments = vec![
            json!({
                "speaker": LABEL_THEM, "start": 1.0, "end": 2.0, "text": "Hello there",
                "words": [
                    { "start": 1.0, "end": 1.4, "text": "Hello" },
                    { "start": 1.5, "end": 2.0, "text": "there" },
                ],
            }),
            json!({
                "speaker": LABEL_THEM, "start": 2.0, "end": 3.0, "text": "again",
                "words": [{ "start": 2.1, "end": 3.0, "text": "again" }],
            }),
        ];

        let grouped = turns(&segments);
        assert_eq!(grouped.len(), 1, "one speaker, one turn");
        let words = grouped[0]["words"].as_array().expect("words survive grouping");
        assert_eq!(words.len(), 3, "merging turns must concatenate their words");
        // In order and on the shared clock, or the highlight jumps backwards.
        let starts: Vec<f64> = words.iter().map(|w| w["start"].as_f64().unwrap()).collect();
        assert!(starts.windows(2).all(|w| w[1] >= w[0]), "got {starts:?}");
        assert_eq!(starts.last(), Some(&2.1));
    }

    #[test]
    fn a_segment_without_words_still_makes_a_turn() {
        // Some engines return none. A turn with no words follows along by
        // paragraph, which is worse but is not a crash.
        let segments = vec![json!({
            "speaker": LABEL_YOU, "start": 0.0, "end": 1.0, "text": "Morning",
        })];
        let grouped = turns(&segments);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0]["words"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_tap_that_delivered_nothing_is_not_the_same_as_a_quiet_call() {
        // The distinction this asserts is the whole reason `carried_audio`
        // exists. A Bluetooth output device can leave the tap running with no
        // error and no samples, and while these two were one flag, seventy-six
        // minutes of meeting came back as two words with nothing said about it.
        let dead = dead_track();
        let quiet = Track { heard_anything: false, carried_audio: true, ..dead_track() };

        assert!(!dead.carried_audio, "a dead tap carried nothing");
        assert!(quiet.carried_audio, "a quiet call still carried samples");
        assert_eq!(
            dead.heard_anything, quiet.heard_anything,
            "loudness cannot tell these apart — which is exactly why it must not \
             be the thing that decides what the user is told"
        );
    }

    #[test]
    fn consecutive_segments_from_one_speaker_become_one_turn() {
        let segments = interleave(
            &[segment(0.0, 1.0, "so I was thinking"), segment(1.0, 2.0, "we should ship it")],
            &[segment(3.0, 4.0, "agreed")],
            0.0,
        );

        let grouped = turns(&segments);

        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0]["text"], "so I was thinking we should ship it");
        // A turn spans from its first word to its last.
        assert_eq!(grouped[0]["start"], 0.0);
        assert_eq!(grouped[0]["end"], 2.0);
        assert_eq!(grouped[1]["speaker"], LABEL_THEM);
    }

    #[test]
    fn speakers_alternating_stay_separate_turns() {
        let segments = interleave(
            &[segment(0.0, 1.0, "one"), segment(2.0, 3.0, "three")],
            &[segment(1.0, 2.0, "two")],
            0.0,
        );
        let grouped = turns(&segments);
        assert_eq!(grouped.len(), 3);
    }

    #[test]
    fn empty_segments_are_dropped_rather_than_becoming_blank_turns() {
        let segments = interleave(&[segment(0.0, 1.0, "   "), segment(1.0, 2.0, "real")], &[], 0.0);
        let grouped = turns(&segments);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0]["text"], "real");
    }

    /// A one-sided meeting is the common case for a talk or a webinar, and it
    /// has to produce a transcript rather than an empty one.
    #[test]
    fn one_silent_side_still_produces_a_conversation() {
        let segments = interleave(&[], &[segment(0.0, 1.0, "presenting")], 0.0);
        let grouped = turns(&segments);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0]["speaker"], LABEL_THEM);
    }

    #[test]
    fn a_conferencing_app_is_worth_offering() {
        assert!(worth_offering("us.zoom.xos"));
        assert!(worth_offering("com.microsoft.teams2"));
        assert!(worth_offering("company.thebrowser.dia"));
        assert!(worth_offering("com.tinyspeck.slackmacgap.helper"));
    }

    /// The bug this prevents is the funny one: the globe key opens the
    /// microphone, so without this every dictation offers to take notes on
    /// itself.
    #[test]
    fn we_never_offer_to_take_notes_on_our_own_dictation() {
        assert!(!worth_offering("dev.heynaavi.voicedump"));
        assert!(!worth_offering("ai.qwee.voicedumps"));
        assert!(!worth_offering("dev.heynaavi.voicedump.helper"));
    }

    #[test]
    fn talking_to_siri_is_not_a_meeting() {
        assert!(!worth_offering("com.apple.assistantd"));
        assert!(!worth_offering("com.apple.CoreSpeech"));
    }

    /// A command-line process has no bundle identifier. Whatever it is doing
    /// with the microphone, nobody is in a call with it.
    #[test]
    fn a_process_with_no_bundle_is_not_a_meeting() {
        assert!(!worth_offering(""));
    }

    /// Prefix matching must not swallow an unrelated app that merely starts
    /// with the same letters.
    #[test]
    fn a_similar_name_is_not_treated_as_ours() {
        assert!(worth_offering("dev.heynaavi.voicedumpster"));
        assert!(worth_offering("com.apple.SiriousBusiness"));
    }

    #[test]
    fn the_title_says_when_the_meeting_was() {
        // 2026-08-05T14:30:00Z — rendered in local time, so assert the shape
        // rather than a wall-clock that depends on where the test runs.
        let title = meeting_title(1_754_404_200_000);
        assert!(title.starts_with("Meeting — "), "unexpected title: {title}");
        assert!(title.contains("Aug"), "unexpected title: {title}");
    }

    // -- naming a speaker ---------------------------------------------------

    fn conversation() -> (Value, Value) {
        let segments = json!([
            { "speaker": LABEL_YOU, "start": 0.0, "end": 1.0, "text": "hello" },
            { "speaker": LABEL_THEM, "start": 2.0, "end": 3.0, "text": "hi there" },
            { "speaker": LABEL_THEM, "start": 3.0, "end": 4.0, "text": "shall we start" },
            { "speaker": LABEL_YOU, "start": 5.0, "end": 6.0, "text": "yes" },
        ]);
        let paragraphs = json!(turns(segments.as_array().unwrap()));
        (paragraphs, segments)
    }

    #[test]
    fn naming_a_speaker_rewrites_every_turn_and_the_text() {
        let (paragraphs, segments) = conversation();
        let (paragraphs, segments, text) =
            relabel(&paragraphs, &segments, LABEL_THEM, "Rupesh").expect("Others is in there");

        assert!(!text.contains(LABEL_THEM), "the flat text still says Others");
        assert!(text.contains("Rupesh: hi there shall we start"));
        assert!(text.contains("You: hello"));

        // The segments matter as much as the turns: they are what a future
        // re-grouping would read back.
        for list in [&paragraphs, &segments] {
            for item in list.as_array().unwrap() {
                assert_ne!(item["speaker"].as_str(), Some(LABEL_THEM));
            }
        }
        assert_eq!(
            segments.as_array().unwrap()[1]["speaker"].as_str(),
            Some("Rupesh")
        );
    }

    /// The whole reason a side is stored at all: naming yourself must not cost
    /// the transcript the colour that tells the two of you apart.
    #[test]
    fn a_renamed_speaker_keeps_the_side_it_was_recorded_on() {
        let mine = vec![segment(0.0, 1.0, "hello")];
        let theirs = vec![segment(2.0, 3.0, "hi there")];
        let segments = json!(interleave(&mine, &theirs, 0.0));
        let paragraphs = json!(turns(segments.as_array().unwrap()));

        let (paragraphs, segments, _) = relabel(&paragraphs, &segments, LABEL_YOU, "Naveen")
            .expect("You is in there");

        let turns = paragraphs.as_array().unwrap();
        assert_eq!(turns[0]["speaker"].as_str(), Some("Naveen"));
        assert_eq!(turns[0]["side"].as_str(), Some(SIDE_YOU));
        assert_eq!(turns[1]["side"].as_str(), Some(SIDE_THEM));
        assert_eq!(segments.as_array().unwrap()[0]["side"].as_str(), Some(SIDE_YOU));
    }

    #[test]
    fn the_other_side_is_left_exactly_as_it_was() {
        let (paragraphs, segments) = conversation();
        let (paragraphs, _, _) =
            relabel(&paragraphs, &segments, LABEL_THEM, "Rupesh").expect("Others is in there");
        let turns = paragraphs.as_array().unwrap();
        assert_eq!(turns[0]["speaker"].as_str(), Some(LABEL_YOU));
        assert_eq!(turns[0]["text"].as_str(), Some("hello"));
    }

    /// Two windows open on the same meeting, one renaming after the other. The
    /// second ask is about somebody who no longer exists, and saying so beats
    /// reporting success over a transcript that did not change.
    #[test]
    fn renaming_somebody_who_is_not_there_is_refused() {
        let (paragraphs, segments) = conversation();
        assert!(relabel(&paragraphs, &segments, "Priya", "Rupesh").is_none());
    }

    #[test]
    fn a_note_with_no_speakers_keeps_its_prose_unattributed() {
        // Every dictation, and every dropped file. Nothing here should ever be
        // renamed, but the text builder is shared and must not start writing a
        // bare colon in front of a paragraph.
        let paragraphs = json!([{ "text": "just some prose", "start": 0.0, "end": 1.0 }]);
        assert_eq!(
            transcript_text(paragraphs.as_array().unwrap()),
            "just some prose"
        );
    }
}
