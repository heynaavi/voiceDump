//! Push-to-talk dictation on a held chord of modifier keys.
//!
//! Hold the chord to record, release to stop; the transcript is pasted wherever
//! the cursor happens to be and also saved to history. Which keys make up the
//! chord is a setting — the globe (fn) key by default. [`crate::shortcut`] owns
//! the encoding and the matching.
//!
//! Why an event tap rather than a registered shortcut: the chord is made of
//! *modifiers*, not keycodes. They never produce a key-down event, only a
//! `flagsChanged` with the relevant mask set, so the usual global-shortcut APIs
//! can't see them at all. The same tap is used to synthesise ⌘V.
//!
//! Requires Accessibility permission. macOS also needs
//! System Settings → Keyboard → "Press 🌐 to:" set to "Do Nothing", or it will
//! open the emoji picker underneath us.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Emitter, Manager};


#[derive(Default)]
pub struct DictationState {
    recording: AtomicBool,
    /// The running ffmpeg capture, if any.
    capture: Mutex<Option<Capture>>,
}

/// The native overlay helper — a separate accessory process that renders the
/// dictation pill.
///
/// It exists because a Tauri/tao webview window cannot enter another app's
/// active full-screen Space, however it's configured (measured exhaustively). A
/// standalone accessory `NSPanel` can, so the pill lives in this tiny process
/// and we drive it over its stdin. Protocol: one command per line —
/// `show`, `transcribing`, `level <0..1>`, `text <words>`, `hide`, `quit`.
mod overlay {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::{Mutex, OnceLock};

    static PIPE: OnceLock<Mutex<Option<ChildStdin>>> = OnceLock::new();
    static CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

    fn pipe() -> &'static Mutex<Option<ChildStdin>> {
        PIPE.get_or_init(|| Mutex::new(None))
    }

    /// Locate the helper binary: bundled resource in production, the built
    /// binary in the source tree during development.
    pub fn locate(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
        use tauri::Manager;
        if let Ok(res) = app.path().resource_dir() {
            let p = res.join("voicedumps-overlay");
            if p.exists() {
                return Some(p);
            }
        }
        let dev =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../overlay-helper/voicedumps-overlay");
        dev.exists().then_some(dev)
    }

    pub fn spawn(path: &Path) {
        match Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                *pipe().lock().unwrap() = child.stdin.take();
                *CHILD.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(child);
                eprintln!("[dictation] overlay helper spawned: {}", path.display());
            }
            Err(e) => eprintln!("[dictation] overlay helper failed to spawn: {e}"),
        }
    }

    fn send(cmd: &str) {
        if let Ok(mut guard) = pipe().lock() {
            if let Some(stdin) = guard.as_mut() {
                let _ = writeln!(stdin, "{cmd}");
                let _ = stdin.flush();
            }
        }
    }

    pub fn show() {
        send("show");
    }
    pub fn transcribing() {
        send("transcribing");
    }
    pub fn hide() {
        send("hide");
    }
    pub fn level(v: f32) {
        send(&format!("level {v:.3}"));
    }

    /// Show what has been heard so far, each word carrying how sure the model
    /// was of it.
    ///
    /// The protocol is one command per line, so the words are encoded rather
    /// than sent as JSON: each is prefixed with a single digit for its
    /// confidence in ninths, `0` (a guess) to `9` (certain). Words were split on
    /// whitespace upstream and any that survives inside one is stripped here, so
    /// a space is still an unambiguous separator.
    pub fn words(list: &[crate::engine::Heard]) {
        let mut line = String::from("text");
        for h in list {
            let word: String = h.word.split_whitespace().collect();
            if word.is_empty() {
                continue;
            }
            line.push(' ');
            line.push(confidence_digit(h.confidence));
            line.push_str(&word);
        }
        send(&line);
    }

    /// Take the panel down to nothing.
    pub fn clear() {
        send("text");
    }

    fn confidence_digit(c: f32) -> char {
        let d = (c * 9.0).round().clamp(0.0, 9.0) as u32;
        char::from_digit(d, 10).unwrap_or('9')
    }

    #[cfg(test)]
    mod tests {
        use super::confidence_digit;

        /// The digit is the whole channel the overlay has for confidence, so
        /// both ends of the range have to survive the round trip: a certain word
        /// must not read as slightly doubtful, and a guess must reach `0` rather
        /// than being rounded up into "fine".
        #[test]
        fn confidence_covers_the_whole_range() {
            assert_eq!(confidence_digit(1.0), '9');
            assert_eq!(confidence_digit(0.0), '0');
            assert_eq!(confidence_digit(0.5), '5');
        }

        /// `exp()` of a sum of logs can land a hair outside 0…1, and a digit
        /// outside 0…9 would be parsed as part of the word itself.
        #[test]
        fn out_of_range_confidence_still_yields_one_digit() {
            for c in [-1.0, -0.001, 1.001, 42.0, f32::NAN] {
                let d = confidence_digit(c);
                assert!(d.is_ascii_digit(), "{c} produced {d:?}");
            }
        }
    }

    /// Ask the helper to quit, on app shutdown.
    pub fn shutdown() {
        send("quit");
        if let Some(m) = CHILD.get() {
            if let Some(mut child) = m.lock().unwrap().take() {
                let _ = child.kill();
            }
        }
    }
}

/// A running native capture. The cpal stream itself lives on the capture thread
/// (it isn't `Send`), so this handle just carries the stop flag and a channel
/// the thread reports the finalized WAV path back through.
struct Capture {
    stop: Arc<AtomicBool>,
    done: std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    /// Mono samples, at `rate`, streamed to the live preview as they arrive.
    ///
    /// A channel rather than a shared buffer. The first version had the audio
    /// callback `try_lock` a `Vec` and *skip the batch* when the preview held
    /// it — silently dropping audio, so the preview fell permanently behind
    /// real time no matter how fast it transcribed. A send never blocks the
    /// audio thread and never drops.
    ///
    /// Separate from the WAV writer rather than re-reading the file: the header
    /// is only finalized on stop, so a half-written WAV cannot be decoded
    /// mid-recording.
    audio: Mutex<Option<std::sync::mpsc::Receiver<Vec<f32>>>>,
    /// The microphone's own rate, which is whatever the device runs at — only
    /// known once the stream is open, hence the atomic.
    rate: Arc<AtomicU32>,
    /// Set on key-up. Checked around each preview pass, so the worst the final
    /// transcription ever waits is one short chunk.
    preview_cancel: Arc<AtomicBool>,
}

/// Start the native overlay helper at launch.
///
/// The pill is rendered by a separate accessory process (the `overlay` module)
/// because a Tauri webview window cannot float over another app's full-screen
/// Space and a native `NSPanel` can. Spawning it once here means it's ready and
/// already a member of every Space by the time the user first presses the key.
/// Where the bundled Swift helper lives.
///
/// One binary, two jobs: it draws the dictation overlay, and in `--pdf` mode it
/// typesets transcript exports. Both need the same resolution, so `export.rs`
/// borrows this rather than keeping a second copy that could drift.
pub fn helper_binary(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    overlay::locate(app)
}

pub fn prepare_overlay(app: &tauri::AppHandle) {
    match overlay::locate(app) {
        Some(path) => overlay::spawn(&path),
        None => eprintln!(
            "[dictation] overlay helper binary not found — build it with \
             `swiftc -O overlay-helper/main.swift -o overlay-helper/voicedumps-overlay`"
        ),
    }
}

/// Stop the native overlay helper on app shutdown.
pub fn stop_overlay() {
    overlay::shutdown();
}

// -- audio capture ---------------------------------------------------------

/// Begin recording the chosen input device — or the system default — to a WAV.
///
/// A native CoreAudio stream (cpal) rather than spawning ffmpeg per press:
/// ffmpeg's avfoundation input pays a ~2s AVCaptureSession warm-up on every
/// open, which clips (or entirely swallows) short globe-key dictations. cpal
/// opens in tens of milliseconds. We capture at the device's native rate and
/// let the engine resample to 16 kHz, which it does for every input anyway, so
/// there's no benefit to matching Whisper's rate here.
///
/// The stream is built and torn down on its own thread because a cpal `Stream`
/// isn't `Send`; it reports the finalized path back through a channel.
fn start_capture(app: &tauri::AppHandle, dir: &Path) -> Result<Capture, String> {
    std::fs::create_dir_all(dir).ok();
    let path = dir.join(format!("dictation-{}.wav", crate::now_ms()));

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, done) = std::sync::mpsc::channel();
    let (audio_tx, audio_rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let rate = Arc::new(AtomicU32::new(0));

    let app = app.clone();
    let thread_stop = stop.clone();
    let thread_path = path.clone();
    let thread_rate = rate.clone();
    std::thread::spawn(move || {
        let _ = tx.send(capture_loop(
            &app,
            &thread_path,
            &thread_stop,
            audio_tx,
            &thread_rate,
        ));
    });

    Ok(Capture {
        stop,
        done,
        audio: Mutex::new(Some(audio_rx)),
        rate,
        preview_cancel: Arc::new(AtomicBool::new(false)),
    })
}

/// Own the cpal stream for the life of one dictation: build it, meter every
/// buffer, and finalize the WAV when `stop` is set.
fn capture_loop(
    app: &tauri::AppHandle,
    path: &Path,
    stop: &AtomicBool,
    audio_tx: std::sync::mpsc::Sender<Vec<f32>>,
    live_rate: &Arc<AtomicU32>,
) -> Result<PathBuf, String> {
    use cpal::traits::{DeviceTrait, StreamTrait};

    let device = crate::microphone::open(crate::settings::microphone(app).as_deref())?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("could not read the microphone's format: {e}"))?;
    let sample_format = config.sample_format();
    let channels = config.channels() as usize;
    let sample_rate = config.sample_rate().0;
    let cfg: cpal::StreamConfig = config.into();
    live_rate.store(sample_rate, Ordering::SeqCst);

    let writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|e| format!("could not open the recording file: {e}"))?;
    let writer = Arc::new(Mutex::new(Some(writer)));

    // Loudest sample seen. A microphone always carries a noise floor, so a
    // recording that stays bit-exactly zero means the OS handed us a muted
    // stream — almost always a denied Microphone permission — which we want to
    // report as that, not as "no speech detected".
    let peak = Arc::new(AtomicU32::new(0));

    // 50 ms of mono samples per meter update, matching the old ffmpeg cadence.
    let win = (sample_rate as f32 * 0.05).max(1.0) as usize;

    // One arm per sample format the device might hand us. Each downmixes to
    // mono, writes 16-bit PCM, tracks the peak, and emits a 0..1 level.
    macro_rules! stream_for {
        ($ty:ty, $to_f32:expr) => {{
            let writer = writer.clone();
            let peak = peak.clone();
            let app = app.clone();
            let audio_tx = audio_tx.clone();
            let mut acc = 0f32;
            let mut n = 0usize;
            device
                .build_input_stream(
                    &cfg,
                    move |data: &[$ty], _: &cpal::InputCallbackInfo| {
                        let conv = $to_f32;
                        let mut guard = writer.lock().unwrap();
                        let Some(w) = guard.as_mut() else { return };
                        // Buffered locally and appended once per callback: the
                        // audio thread must not contend on a lock per sample.
                        let mut batch: Vec<f32> = Vec::with_capacity(data.len() / channels + 1);
                        for frame in data.chunks(channels) {
                            let mut s = 0f32;
                            for &samp in frame {
                                s += conv(samp);
                            }
                            s /= channels as f32;
                            let i = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                            let _ = w.write_sample(i);
                            batch.push(s);
                            peak.fetch_max(i.unsigned_abs() as u32, Ordering::Relaxed);
                            acc += s * s;
                            n += 1;
                            if n >= win {
                                let rms = (acc / n as f32).sqrt();
                                let db = if rms > 0.0 { 20.0 * rms.log10() } else { -120.0 };
                                // Same anchor the ffmpeg meter used: silence
                                // bottoms near -120 dB, speech sits near -50,
                                // peaks reach about -12; -72..-14 keeps quiet
                                // passages visible without loud ones pinning it.
                                let level = ((db + 72.0) / 58.0).clamp(0.0, 1.0);
                                overlay::level(level);
                                let _ = app.emit("dictation-level", level);
                                acc = 0.0;
                                n = 0;
                            }
                        }
                        // Never blocks, never drops. A closed channel just
                        // means the preview finished first.
                        let _ = audio_tx.send(batch);
                    },
                    move |e| eprintln!("[dictation] capture stream error: {e}"),
                    None,
                )
                .map_err(|e| format!("could not open the microphone: {e}"))?
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => stream_for!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => stream_for!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::U16 => stream_for!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        other => return Err(format!("unsupported microphone format: {other:?}")),
    };
    stream.play().map_err(|e| format!("could not start the microphone: {e}"))?;

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }

    // Dropping the stream stops capture; then flush the WAV header.
    drop(stream);
    overlay::level(0.0);
    if let Some(w) = writer.lock().unwrap().take() {
        w.finalize()
            .map_err(|e| format!("could not finalize the recording: {e}"))?;
    }

    if peak.load(Ordering::Relaxed) == 0 {
        return Err(
            "the microphone returned no audio — grant Microphone access in \
             System Settings › Privacy & Security, then restart the app"
                .into(),
        );
    }

    Ok(path.to_path_buf())
}

fn stop_capture(cap: Capture) -> Result<PathBuf, String> {
    cap.stop.store(true, Ordering::SeqCst);
    // Block until the capture thread has finalized the WAV (or failed). The
    // bound guards against a wedged audio thread hanging the dictation flow.
    match cap.done.recv_timeout(Duration::from_secs(5)) {
        Ok(res) => res,
        Err(_) => Err("the recording did not finish in time".into()),
    }
}

// -- paste -----------------------------------------------------------------

/// How long the dictated text is left on the clipboard before whatever was
/// there goes back.
///
/// The paste is a synthetic ⌘V, and the app it lands in reads the clipboard on
/// its own schedule afterwards — put the old contents back too eagerly and the
/// text that arrives is the old contents. Nothing reports when that read has
/// happened, so this is a wait rather than a handshake, and it is long enough
/// to cover an app that is busy when the keystroke arrives.
const RESTORE_AFTER: Duration = Duration::from_millis(600);

/// Put text on the clipboard and synthesise ⌘V into the focused app.
///
/// Pasting rather than typing the text out character by character: synthetic
/// keystrokes are slow and get mangled by autocorrect and input methods.
///
/// Dictating used to cost you your clipboard — whatever you had copied was
/// gone, replaced by the transcript. So what was there is read first and put
/// back once the paste has landed, which makes the clipboard a place you can
/// keep something across a dictation rather than a channel this happens to use.
///
/// The transcript itself is not lost by that: the tray's "Copy Last Transcript"
/// puts it back on the clipboard whenever you want it.
fn paste(text: &str) -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let previous = crate::clipboard::read();
    crate::clipboard::write(text)?;

    let src = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| "could not create event source")?;
    // 9 = kVK_ANSI_V
    let down = CGEvent::new_keyboard_event(src.clone(), 9, true)
        .map_err(|_| "could not create key event")?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    let up = CGEvent::new_keyboard_event(src, 9, false)
        .map_err(|_| "could not create key event")?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);

    if let Some(previous) = previous {
        // Off this thread: the note still has to be saved, and none of that
        // should wait on a timer.
        let ours = text.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(RESTORE_AFTER);
            // Only if the clipboard is still ours. Copying something during
            // those few hundred milliseconds is a clear instruction about what
            // the clipboard should hold, and it outranks what was there before.
            if crate::clipboard::read().as_deref() == Some(ours.as_str()) {
                let _ = crate::clipboard::write(&previous);
            }
        });
    }

    Ok(())
}

// -- the flow --------------------------------------------------------------


fn cues(app: &tauri::AppHandle) -> Option<crate::sound::Cues> {
    let dir = app.path().app_data_dir().ok()?;
    crate::sound::ensure(&dir).ok()
}

/// Stream the words out while the user is still speaking.
///
/// The first version re-transcribed the whole recording every pass, which is
/// how most Whisper "live" demos work and is why they all feel sluggish: with
/// `medium` a 30-second window costs several seconds, so the text lands further
/// behind the longer you talk. Speaking for seventy seconds meant watching the
/// opening sentence sit there.
///
/// So it transcribes forward instead. A cursor marks how much has been read;
/// each pass takes only what has arrived since, appends the words, and moves
/// the cursor on. Pass cost is bounded by the chunk, not by how long you have
/// been going, so the lag stays flat instead of compounding.
///
/// The trade is context: whisper sees each chunk alone, so it cannot revise an
/// earlier word once a later one clarifies it. For a preview that is the right
/// side of the trade — this text is disposable, and what actually gets pasted
/// still comes from one clean read of the complete recording in `engine::run`.
///
/// Chunks are cut at the quietest moment in their tail rather than on a fixed
/// stride, so boundaries tend to land between words instead of slicing one in
/// half.
fn spawn_preview(app: &tauri::AppHandle, cap: &Capture) {
    /// Wait for at least this much new speech before spending a pass on it.
    const MIN_CHUNK_SECS: f32 = 2.5;
    /// And never bite off more than this, so a pass stays quick.
    const MAX_CHUNK_SECS: f32 = 8.0;

    let Some(rx) = cap.audio.lock().unwrap().take() else {
        return;
    };
    let app = app.clone();
    let rate = cap.rate.clone();
    let stop = cap.stop.clone();
    let cancel = cap.preview_cancel.clone();

    std::thread::spawn(move || {
        // One state for the whole dictation. Building it costs ~530 MB of KV
        // caches and compute buffers, so a fresh one per pass does not merely
        // run slowly — the allocations fail and the transcript freezes on its
        // opening line.
        let mut session: Option<crate::engine::Preview> = None;
        let mut buf: Vec<f32> = Vec::new();
        let mut cursor = 0usize;
        let mut heard: Vec<crate::engine::Heard> = Vec::new();

        while !stop.load(Ordering::SeqCst) && !cancel.load(Ordering::Relaxed) {
            // Drain whatever the microphone has produced. Blocking briefly here
            // is what paces the loop; the channel is the clock.
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(batch) => buf.extend_from_slice(&batch),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            while let Ok(batch) = rx.try_recv() {
                buf.extend_from_slice(&batch);
            }

            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let src_rate = rate.load(Ordering::SeqCst);
            if src_rate == 0 {
                continue;
            }

            let min = (src_rate as f32 * MIN_CHUNK_SECS) as usize;
            let max = (src_rate as f32 * MAX_CHUNK_SECS) as usize;
            if buf.len() - cursor < min {
                continue;
            }

            // Built on the first turn the engine is free — the model is still
            // warming when recording starts, so this would otherwise be a race
            // the preview always lost.
            if session.is_none() {
                session = crate::engine::Preview::start(&app);
                if session.is_none() {
                    continue;
                }
            }

            let end = quietest_cut(&buf, cursor + min, (cursor + max).min(buf.len()), src_rate);
            let audio = crate::engine::to_engine_rate(&buf[cursor..end], src_rate);

            let Some(s) = session.as_mut() else { continue };
            if let Some(piece) = s.step(&audio, cancel.clone()) {
                heard.extend(piece);
                overlay::words(&heard);
                // The window gets the plain sentence: confidence is a reading
                // aid for the words floating over another app, and the app's own
                // UI is not where you are looking while you dictate.
                let words: Vec<&str> = heard.iter().map(|h| h.word.as_str()).collect();
                let _ = app.emit("dictation-partial", words.join(" "));
            }
            // Advance regardless: a chunk that transcribed to nothing was
            // silence, and re-reading it forever would stall the cursor.
            cursor = end;
        }

        // Explicit, and before the final pass allocates its own: two live
        // states plus the model is over 1.6 GB on `medium`, and there is no
        // reason for them to overlap.
        drop(session);
        overlay::clear();
    });
}

/// Pick a chunk boundary that is unlikely to fall inside a word.
///
/// Scans 20 ms windows between `from` and `to` for the quietest one — a pause
/// between words is the cheapest place to cut, and cutting mid-word makes the
/// preview stutter out fragments. Falls back to `to` when everything is loud,
/// which is the correct answer for someone talking without drawing breath.
fn quietest_cut(buf: &[f32], from: usize, to: usize, rate: u32) -> usize {
    if to <= from || to > buf.len() {
        return to.min(buf.len()).max(from.min(buf.len()));
    }
    let win = ((rate as f32 * 0.02) as usize).max(1);
    if to - from <= win {
        return to;
    }

    let mut best = to;
    let mut best_energy = f32::MAX;
    let mut at = from;
    while at + win <= to {
        let energy: f32 = buf[at..at + win].iter().map(|s| s * s).sum();
        if energy <= best_energy {
            best_energy = energy;
            best = at + win;
        }
        at += win;
    }
    best
}

/// How long an abandoned scratch capture is allowed to sit on disk.
const SCRATCH_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Delete scratch captures that never became transcripts.
///
/// The happy path already removes each WAV the moment the library has its
/// normalised copy, so nothing here races a live dictation — but every early
/// return between "recording stopped" and "transcript inserted" leaks the file,
/// and a crash mid-capture leaks a *growing* one. Left alone that is unbounded:
/// one of these directories reached 4.6 GB, of which 4.57 GB was five captures
/// orphaned by a killed recorder that had held the microphone open for ten
/// hours apiece.
///
/// Age is the whole test, deliberately. A successful capture is deleted
/// synchronously, so anything still here after a day is abandoned by
/// definition — consulting the database would mean taking its lock during
/// startup to learn nothing new.
fn sweep_scratch(dir: &Path, ttl: std::time::Duration) -> (u32, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let (mut files, mut bytes) = (0u32, 0u64);
    for entry in entries.flatten() {
        let path = entry.path();
        // Only our own captures. The directory is ours, but deleting by age
        // alone would take anything a user happened to drop in it.
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        // `modified` rather than `created`: a leaked capture is still being
        // written to, and its creation time says nothing about whether the
        // process that owns it is still alive.
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > ttl);
        if stale && std::fs::remove_file(&path).is_ok() {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

/// Run the sweep off the startup path. It is pure disk I/O on a directory that
/// is normally empty, but "normally" is exactly what failed here before.
pub fn spawn_sweep(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let Ok(dir) = app.path().app_data_dir() else {
            return;
        };
        let (files, bytes) = sweep_scratch(&dir.join("dictation"), SCRATCH_TTL);
        if files > 0 {
            eprintln!(
                "[dictation] swept {files} abandoned capture(s), {:.1} MB",
                bytes as f64 / 1_048_576.0
            );
        }
    });
}

fn start(app: &tauri::AppHandle) {
    let state = app.state::<DictationState>();
    if state.recording.load(Ordering::SeqCst) {
        return;
    }

    let dir = match app.path().app_data_dir() {
        Ok(d) => d.join("dictation"),
        Err(e) => {
            let _ = app.emit("dictation-error", e.to_string());
            return;
        }
    };

    match start_capture(app, &dir) {
        Ok(cap) => {
            *state.capture.lock().unwrap() = Some(cap);
            state.recording.store(true, Ordering::SeqCst);
            if let Some(c) = cues(app) {
                crate::sound::play(&c.start);
            }
            overlay::show();
            let _ = app.emit("dictation-state", "recording");

            // Load the model while the user is still talking, so releasing the
            // key feels instant instead of paying a cold load at the worst
            // possible moment. On its own thread: the load blocks for half a
            // second and this one is driving the event tap, so every keystroke
            // on the machine would stall behind it.
            let warming = app.clone();
            std::thread::spawn(move || crate::engine::warm(&warming));

            // Live preview reads the same audio the recording is capturing, and
            // is off unless asked for — see `settings::Settings::live_preview`.
            //
            // When it is off the receiver is dropped rather than left sitting in
            // the `Option`: the capture callback sends into that channel on
            // every buffer, and with nobody draining it a long dictation would
            // bank the entire recording a second time in memory. A closed
            // channel makes the send a no-op, which is what the callback already
            // expects.
            if let Some(c) = state.capture.lock().unwrap().as_ref() {
                if crate::settings::live_preview(app) {
                    spawn_preview(app, c);
                } else {
                    drop(c.audio.lock().unwrap().take());
                }
            }
        }
        Err(e) => {
            let _ = app.emit("dictation-error", e);
        }
    }
}

fn stop(app: &tauri::AppHandle) {
    let state = app.state::<DictationState>();
    if !state.recording.swap(false, Ordering::SeqCst) {
        return;
    }

    let cap = state.capture.lock().unwrap().take();

    // First thing, before anything slower: tell the preview to stop. It checks
    // between passes, and a pass is a few seconds of audio rather than the
    // whole recording, so the final transcription waits at most one chunk.
    if let Some(c) = cap.as_ref() {
        c.preview_cancel.store(true, Ordering::Relaxed);
    }

    if let Some(c) = cues(app) {
        crate::sound::play(&c.stop);
    }
    overlay::transcribing();
    let _ = app.emit("dictation-state", "transcribing");

    let Some(cap) = cap else {
        overlay::hide();
        return;
    };

    let app = app.clone();
    // Off the tap thread: the event tap's run loop must never block, or every
    // keystroke on the system stalls behind us.
    std::thread::spawn(move || {
        if let Err(e) = finish(&app, cap) {
            let _ = app.emit("dictation-error", e);
        }
        let _ = app.emit("dictation-state", "idle");
        overlay::hide();
    });
}

/// The app the dictated text is about to land in.
///
/// Read from the window list rather than `NSWorkspace`, because the window list
/// is already a core-graphics dependency and gives the answer in one call. Only
/// `kCGWindowOwnerName` is used: window *titles* would need Screen Recording
/// permission, and asking for that to draw a chart would be a poor trade — the
/// owner name has never needed it.
///
/// The list comes back front-to-back, so the first layer-0 window is the
/// frontmost ordinary one. Higher layers are menu-bar extras, the Dock and our
/// own dictation pill, none of which are where anyone is typing.
#[cfg(target_os = "macos")]
fn frontmost_app() -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::window::{
        copy_window_info, kCGNullWindowID, kCGWindowLayer, kCGWindowListExcludeDesktopElements,
        kCGWindowListOptionOnScreenOnly, kCGWindowOwnerName,
    };

    let windows = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )?;

    for i in 0..windows.len() {
        let raw = *windows.get(i)?;
        let dict: CFDictionary = unsafe { CFDictionary::wrap_under_get_rule(raw as _) };

        let layer = dict
            .find(unsafe { kCGWindowLayer } as *const _)
            .map(|v| unsafe { CFNumber::wrap_under_get_rule(*v as _) })
            .and_then(|n| n.to_i64())
            .unwrap_or(-1);
        if layer != 0 {
            continue;
        }

        let name = dict
            .find(unsafe { kCGWindowOwnerName } as *const _)
            .map(|v| unsafe { CFString::wrap_under_get_rule(*v as _) })
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Skip ourselves: if our window happens to be front the user is
        // dictating into the app itself, which is worth recording as such,
        // but an empty name tells us nothing.
        if name.is_empty() {
            continue;
        }
        return Some(name);
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn frontmost_app() -> Option<String> {
    None
}

#[cfg(test)]
mod sweep_tests {
    use std::time::Duration;

    /// A scratch directory with one of each thing that can be in it.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vd-sweep-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dictation-1.wav"), b"aaaa").unwrap();
        std::fs::write(dir.join("dictation-2.wav"), b"bb").unwrap();
        std::fs::write(dir.join("notes.txt"), b"not ours").unwrap();
        dir
    }

    /// Everything past its time goes, and the byte count is real — the numbers
    /// end up in a log line claiming how much was reclaimed.
    #[test]
    fn expired_captures_are_removed() {
        let dir = scratch("expired");
        let (files, bytes) = super::sweep_scratch(&dir, Duration::ZERO);
        assert_eq!((files, bytes), (2, 6));
        assert!(!dir.join("dictation-1.wav").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The guard that matters: a capture written moments ago may still belong to
    /// a dictation in flight, and deleting it would destroy live audio.
    #[test]
    fn fresh_captures_survive() {
        let dir = scratch("fresh");
        let (files, _) = super::sweep_scratch(&dir, Duration::from_secs(3600));
        assert_eq!(files, 0);
        assert!(dir.join("dictation-1.wav").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Age alone is not licence to delete. Anything that isn't one of our WAVs
    /// stays put however old it is.
    #[test]
    fn other_files_are_left_alone() {
        let dir = scratch("others");
        super::sweep_scratch(&dir, Duration::ZERO);
        assert!(dir.join("notes.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A missing directory is the normal state on a fresh install, not a fault.
    #[test]
    fn a_missing_directory_is_not_an_error() {
        let missing = std::env::temp_dir().join("vd-sweep-nope");
        std::fs::remove_dir_all(&missing).ok();
        assert_eq!(super::sweep_scratch(&missing, Duration::ZERO), (0, 0));
    }
}

#[cfg(test)]
mod chunk_tests {
    /// The cut lands in the gap, not mid-word.
    ///
    /// A chunk boundary inside a word makes the preview stutter out fragments,
    /// so the scan looks for the quietest moment in the tail of the range.
    #[test]
    fn cuts_at_the_quiet_part() {
        let rate = 16_000u32;
        // Half a second of loud, then a clear gap, then loud again.
        let mut buf = vec![0.5f32; rate as usize / 2];
        buf.extend(std::iter::repeat(0.0).take(rate as usize / 10));
        buf.extend(std::iter::repeat(0.5).take(rate as usize / 2));

        let cut = super::quietest_cut(&buf, 0, buf.len(), rate);
        let gap_start = rate as usize / 2;
        let gap_end = gap_start + rate as usize / 10;
        assert!(
            cut >= gap_start && cut <= gap_end + 1,
            "cut at {cut} should land in the silent gap {gap_start}..{gap_end}"
        );
    }

    /// Someone talking without drawing breath still has to be cut somewhere,
    /// and the end of the range is the right answer.
    #[test]
    fn constant_speech_cuts_at_the_end() {
        let rate = 16_000u32;
        let buf = vec![0.4f32; rate as usize];
        assert_eq!(super::quietest_cut(&buf, 0, buf.len(), rate), buf.len());
    }

    /// Degenerate ranges must not panic or walk off the buffer — this runs on
    /// every dictation, including one-second ones.
    #[test]
    fn odd_ranges_are_safe() {
        let rate = 16_000u32;
        let buf = vec![0.1f32; 100];
        assert!(super::quietest_cut(&buf, 0, 0, rate) <= buf.len());
        assert!(super::quietest_cut(&buf, 50, 40, rate) <= buf.len());
        assert!(super::quietest_cut(&buf, 0, 500, rate) <= buf.len());
        assert!(super::quietest_cut(&[], 0, 0, rate) == 0);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod app_name_tests {
    /// Exercises the raw CoreFoundation casts in `frontmost_app`.
    ///
    /// The point is the unsafe block, not the answer: a wrong `wrap_under_*`
    /// rule or a bad pointer cast shows up as a crash or an over-release, and
    /// this runs it for real against the live window server. The returned name
    /// depends on whatever is on screen, so it is printed rather than asserted.
    #[test]
    fn reading_the_frontmost_app_is_memory_safe() {
        for _ in 0..50 {
            let name = super::frontmost_app();
            assert!(
                name.as_deref() != Some(""),
                "empty names must be skipped, not returned"
            );
        }
        println!("frontmost app: {:?}", super::frontmost_app());
    }
}

fn finish(app: &tauri::AppHandle, cap: Capture) -> Result<(), String> {
    let path = stop_capture(cap)?;

    let result =
        crate::engine::transcribe_ingest(app, &path.to_string_lossy(), "Dictation", "hotkey")?;
    let text = result
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if text.is_empty() {
        // Nothing said. Don't paste an empty string over a selection.
        return Err("no speech detected".into());
    }

    // Read before pasting: the paste synthesises ⌘V, and if anything about that
    // shifts focus we'd be recording where the text went afterwards rather than
    // where it was aimed.
    let target = frontmost_app();

    paste(&text)?;

    let duration = result.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let id = crate::insert_transcript(
        app,
        &crate::dictation_title(&text),
        &path.to_string_lossy(),
        duration,
        result.get("language").and_then(|v| v.as_str()),
        &text,
        result.get("paragraphs").cloned().unwrap_or(serde_json::Value::Null),
        result.get("segments").cloned().unwrap_or(serde_json::Value::Null),
        result.get("peaks").cloned().unwrap_or(serde_json::Value::Null),
        "hotkey",
        crate::engine::Run::from_result(&result),
    )?;

    // Scratch capture; the library holds the normalised copy now.
    if let Some(name) = target {
        use tauri::Manager;
        let store = app.state::<crate::store::Store>();
        let conn = store.0.lock().unwrap();
        // Best-effort: the note is already saved, and losing the app label is
        // not worth failing a dictation over.
        let _ = crate::store::set_app_name(&conn, &id, &name);
    }

    let _ = std::fs::remove_file(&path);

    let _ = app.emit("ingest-done", id);
    Ok(())
}

// -- event tap -------------------------------------------------------------

pub fn spawn(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
        use core_graphics::event::{
            CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        };

        // Ask macOS to show its own dialog on first run. It only appears once
        // per binary path, so also log and surface it — in dev the binary path
        // changes on every rebuild, which quietly revokes the grant.
        if !has_accessibility(true) {
            let msg = "Accessibility permission is required for the dictation key. \
                       Grant it in System Settings → Privacy & Security → Accessibility, \
                       then restart the app.";
            eprintln!("[dictation] {msg}");
            let _ = app.emit("dictation-error", msg);

            // Poll rather than give up: the user can grant it without a restart
            // and the tap will come up on its own.
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3));
                if has_accessibility(false) {
                    eprintln!("[dictation] accessibility granted; installing tap");
                    let _ = app.emit("dictation-error", "");
                    break;
                }
            }
        }

        eprintln!("[dictation] tap active");

        // Debounce: flagsChanged fires on both press and release of the globe
        // key, and we only want to act on one edge.
        let was_down = Arc::new(AtomicBool::new(false));

        let handle = app.clone();
        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            // Listen-only: we observe the chord, we don't swallow it. If we
            // consumed events the whole keyboard would route through us.
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::FlagsChanged],
            move |_, _, event| {
                let down = crate::shortcut::is_held(event.get_flags().bits());
                // Hold to talk: press starts, release stops. flagsChanged fires
                // on both edges, so the previous state is tracked to avoid
                // acting twice on key repeat.
                if down && !was_down.swap(true, Ordering::SeqCst) {
                    start(&handle);
                } else if !down && was_down.swap(false, Ordering::SeqCst) {
                    stop(&handle);
                }
                None
            },
        );

        let tap = match tap {
            Ok(t) => t,
            Err(_) => {
                let _ = app.emit("dictation-error", "Could not install the keyboard tap.");
                return;
            }
        };

        let loop_source = match tap.mach_port.create_runloop_source(0) {
            Ok(s) => s,
            Err(_) => return,
        };
        let run_loop = CFRunLoop::get_current();
        unsafe { run_loop.add_source(&loop_source, kCFRunLoopCommonModes) };
        tap.enable();
        CFRunLoop::run_current();
    });
}

/// Whether Accessibility is granted, optionally showing the system prompt.
///
/// Without this permission `CGEventTap::new` succeeds but the tap never fires a
/// single event — no error, no callback, nothing. It is by far the most
/// confusing failure mode here, so we check up front and say so loudly.
pub fn has_accessibility(prompt: bool) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::string::{CFString, CFStringRef};

    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        static kAXTrustedCheckOptionPrompt: CFStringRef;
    }

    unsafe {
        if !prompt {
            return AXIsProcessTrustedWithOptions(std::ptr::null());
        }
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let opts = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
        AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef())
    }
}

/// Open the Accessibility pane, since the prompt only appears once per binary.
pub fn open_accessibility_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}
