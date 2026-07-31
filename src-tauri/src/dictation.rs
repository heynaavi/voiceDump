//! Push-to-talk dictation on the globe (fn) key.
//!
//! Tap the globe key to start recording, tap again to stop; the transcript is
//! pasted wherever the cursor happens to be and also saved to history.
//!
//! Why an event tap rather than a registered shortcut: the globe key is a
//! *modifier*, not a keycode. It never produces a key-down event, only a
//! `flagsChanged` with the secondary-fn mask set, so the usual global-shortcut
//! APIs can't see it at all. The same tap is used to synthesise ⌘V.
//!
//! Requires Accessibility permission. macOS also needs
//! System Settings → Keyboard → "Press 🌐 to:" set to "Do Nothing", or it will
//! open the emoji picker underneath us.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Emitter, Manager};

/// `kCGEventFlagMaskSecondaryFn` — the globe/fn bit in a flagsChanged event.
const FN_MASK: u64 = 0x0080_0000;

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
/// `show`, `transcribing`, `level <0..1>`, `hide`, `quit`.
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

/// Begin recording the default input device to a WAV.
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

    let app = app.clone();
    let thread_stop = stop.clone();
    let thread_path = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(capture_loop(&app, &thread_path, &thread_stop));
    });

    Ok(Capture { stop, done })
}

/// Own the cpal stream for the life of one dictation: build it, meter every
/// buffer, and finalize the WAV when `stop` is set.
fn capture_loop(app: &tauri::AppHandle, path: &Path, stop: &AtomicBool) -> Result<PathBuf, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no microphone found")?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("could not read the microphone's format: {e}"))?;
    let sample_format = config.sample_format();
    let channels = config.channels() as usize;
    let sample_rate = config.sample_rate().0;
    let cfg: cpal::StreamConfig = config.into();

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
            let mut acc = 0f32;
            let mut n = 0usize;
            device
                .build_input_stream(
                    &cfg,
                    move |data: &[$ty], _: &cpal::InputCallbackInfo| {
                        let conv = $to_f32;
                        let mut guard = writer.lock().unwrap();
                        let Some(w) = guard.as_mut() else { return };
                        for frame in data.chunks(channels) {
                            let mut s = 0f32;
                            for &samp in frame {
                                s += conv(samp);
                            }
                            s /= channels as f32;
                            let i = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                            let _ = w.write_sample(i);
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

/// Put text on the clipboard and synthesise ⌘V into the focused app.
///
/// Pasting rather than typing the text out character by character: synthetic
/// keystrokes are slow and get mangled by autocorrect and input methods.
fn paste(text: &str) -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let mut pb = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("clipboard failed: {e}"))?;
    {
        use std::io::Write;
        pb.stdin
            .as_mut()
            .ok_or("clipboard pipe unavailable")?
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    pb.wait().map_err(|e| e.to_string())?;

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

    Ok(())
}

// -- the flow --------------------------------------------------------------


fn cues(app: &tauri::AppHandle) -> Option<crate::sound::Cues> {
    let dir = app.path().app_data_dir().ok()?;
    crate::sound::ensure(&dir).ok()
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

            // Load the model while the user is still talking, so releasing
            // the key feels instant instead of paying a cold load at the worst
            // possible moment.
            crate::engine::warm(app);
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
            let msg = "Accessibility permission is required for the globe key. \
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

        eprintln!("[dictation] globe-key tap active");

        // Debounce: flagsChanged fires on both press and release of the globe
        // key, and we only want to act on one edge.
        let was_down = Arc::new(AtomicBool::new(false));

        let handle = app.clone();
        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            // Listen-only: we observe the globe key, we don't swallow it. If we
            // consumed events the whole keyboard would route through us.
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::FlagsChanged],
            move |_, _, event| {
                let down = event.get_flags().bits() & FN_MASK != 0;
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
