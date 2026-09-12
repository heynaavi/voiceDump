//! Local transcription, in-process.
//!
//! This replaces the Python sidecar's transcribe path. That version worked, but
//! it could never be shipped: it needed a 1 GB virtualenv, the `mlx` stack, and
//! `ffmpeg`/`ffprobe` on the user's PATH. "Download the app and run it" is not
//! possible on those terms.
//!
//! Here, whisper.cpp does the transcription (Metal-accelerated on Apple
//! Silicon) and symphonia does the decoding — both compiled into the binary,
//! with the model weights bundled as a resource. Nothing is fetched at runtime,
//! so a fresh install works offline on first launch.
//!
//! The output shape deliberately matches what the sidecar returned, down to the
//! paragraph-splitting heuristics, so the store, the UI and the reading view
//! didn't have to change.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Whisper is trained on 16 kHz mono; anything else costs a resample inside the
/// model and loses accuracy.
const SAMPLE_RATE: u32 = 16_000;

// Ported verbatim from pipeline.py so paragraphs break in the same places and
// old transcripts stay visually consistent with new ones.
const PEAK_BUCKETS: usize = 900;
const PARAGRAPH_GAP_SEC: f64 = 0.75;
const PARAGRAPH_SOFT_CHARS: usize = 420;
const PARAGRAPH_HARD_CHARS: usize = 900;

// -- model selection --------------------------------------------------------

/// Which weights to run. Both ship inside the app; this only decides which one
/// to load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSize {
    Small,
    Medium,
}

impl ModelSize {
    fn file_name(self) -> &'static str {
        // Quantised multilingual weights. Multilingual rather than `.en`
        // because the app already translates non-English notes, and q5 because
        // the accuracy loss is inaudible next to halving the download.
        match self {
            ModelSize::Small => "ggml-small-q5_1.bin",
            ModelSize::Medium => "ggml-medium-q5_0.bin",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModelSize::Small => "small",
            ModelSize::Medium => "medium",
        }
    }
}

/// Physical RAM in bytes, or None if the sysctl fails.
fn total_memory() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Pick the best model this machine can comfortably run.
///
/// Medium is the better transcription and is preferred wherever it fits. The
/// gate is memory rather than chip generation: a base M1 with 8 GB is already
/// juggling a browser and an editor, and quietly making the whole machine swap
/// to win a little accuracy is a bad trade. Above that, medium every time.
pub fn auto_model() -> ModelSize {
    if let Ok(forced) = std::env::var("VOICEDUMPS_MODEL_SIZE") {
        match forced.trim().to_ascii_lowercase().as_str() {
            "small" => return ModelSize::Small,
            "medium" => return ModelSize::Medium,
            _ => {}
        }
    }
    const GB: u64 = 1024 * 1024 * 1024;
    match total_memory() {
        Some(bytes) if bytes >= 16 * GB => ModelSize::Medium,
        Some(_) => ModelSize::Small,
        // Unknown machine: medium is ~1.5 GB resident, which is a lot to assume.
        None => ModelSize::Small,
    }
}

/// Locate a model file.
///
/// Order is deliberate. The downloaded copy in the data directory comes first
/// because it is the one that survives an upgrade — see [`crate::models`]. The
/// resource directory stays as a candidate behind it so a bundle that *did*
/// ship the weights keeps working: anyone still running 0.8.0 or earlier has
/// them in there, and asking those users to re-download a file they already
/// have would be a strange way to introduce a change that exists to save them
/// a download.
pub(crate) fn model_path(app: &tauri::AppHandle, size: ModelSize) -> Option<PathBuf> {
    find_model_file(app, size.file_name())
}

/// The voice-activity model, which lives beside Whisper's weights.
///
/// Silero v5.1.2 in ggml's format, 885 KB — see [`crate::models::VAD`] for
/// where it comes from and how it is pinned.
pub(crate) const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";

/// Locate the voice-activity model, in the same places and the same order as
/// [`model_path`] looks for Whisper.
///
/// `None` is an ordinary answer rather than a broken install: the model is
/// fetched quietly after launch, and until it is here transcription runs the
/// way it always has — every sample to the decoder.
pub(crate) fn vad_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    find_model_file(app, VAD_FILE)
}

fn find_model_file(app: &tauri::AppHandle, name: &str) -> Option<PathBuf> {
    use tauri::Manager;

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("VOICEDUMPS_MODEL_DIR") {
        candidates.push(PathBuf::from(dir).join(name));
    }
    if let Ok(data) = app.path().app_data_dir() {
        candidates.push(data.join("models").join(name));
    }
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("models").join(name));
    }
    // Dev only: the repo's own model directory, which isn't committed.
    //
    // Debug builds exclusively, because `CARGO_MANIFEST_DIR` is baked in at
    // compile time. In a release binary it is an absolute path on whatever
    // machine did the build — which is both a path leak into a public artifact
    // and, on that machine, a copy of the models that silently satisfies the
    // first-run check, so the one build you would test the setup screen with
    // is the one build that never shows it.
    #[cfg(debug_assertions)]
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../models")
            .join(name),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// Why transcription can't run, or None if it can. Checked at startup so a
/// broken install says so immediately rather than on the user's first
/// recording. The assistant build reports the sidecar's health instead.
///
/// Reaching this now means something went wrong *after* first-run setup — the
/// window will not let you past the download screen without the weights — so
/// it points at the place they live rather than blaming the build.
#[cfg(not(feature = "assistant"))]
pub fn missing_model(app: &tauri::AppHandle) -> Option<String> {
    let want = auto_model();
    if model_path(app, want).is_some() {
        return None;
    }
    Some(format!(
        "The {} speech model is missing. Quit and reopen the app to download it again.",
        want.label()
    ))
}

// -- resident model ---------------------------------------------------------

/// The loaded model, kept between jobs.
///
/// Loading medium costs a couple of seconds, so holding it makes back-to-back
/// dictations feel instant. It's dropped by [`unload`] when the app goes idle —
/// a menu-bar app that sits on 1.5 GB all day is a bad neighbour.
#[derive(Default)]
pub struct EngineState {
    inner: Mutex<Option<Loaded>>,
    /// A second, smaller model kept for live preview only.
    ///
    /// The preview wants speed far more than accuracy — its text is thrown
    /// away the moment the real transcription lands — and `small` runs roughly
    /// four times quicker, which is the difference between words appearing as
    /// you speak and words appearing after you have stopped.
    ///
    /// Only populated when the main engine is running `medium`; on a machine
    /// small enough to be using `small` anyway there is nothing to gain and a
    /// second copy would be pure waste.
    preview: Mutex<Option<WhisperContext>>,
    /// The voice-activity detector, loaded once and kept with the model.
    ///
    /// whisper.cpp's own VAD switch would load it again for every state, which
    /// here means every dictation. It is under a megabyte, so the load is not
    /// slow — but it is a file opened and a graph built on the path somebody is
    /// waiting on, to arrive at exactly the object this already holds.
    vad: Mutex<Option<whisper_rs::WhisperVadContext>>,
    /// When the model was last wanted. `None` means never.
    ///
    /// Always locked *after* `inner`, everywhere, so the reaper and a running
    /// transcription can't deadlock against each other.
    last_use: Mutex<Option<std::time::Instant>>,
    /// How many transcriptions are decoding right now.
    ///
    /// The slot's lock used to be what said "busy", because it was held for the
    /// whole of a decode. It no longer is — see [`transcribe`] — so the reaper
    /// needs its own way to tell a model nobody has touched for five minutes
    /// from one that has been chewing through an hour of meeting for twenty.
    /// Dropping the latter is safe but pointless: the decode carries on, and the
    /// next dictation pays to load a second copy of weights already in memory.
    running: std::sync::atomic::AtomicUsize,
}

struct Loaded {
    size: ModelSize,
    ctx: WhisperContext,
}

/// Counts one decode in and out again, whatever way it leaves.
///
/// A plain increment and decrement around `full()` would leak the count on the
/// error path and pin the model in memory until quit.
struct InFlight<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> InFlight<'a> {
    fn enter(counter: &'a std::sync::atomic::AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        InFlight(counter)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl EngineState {
    pub fn unload(&self) {
        *self.inner.lock().unwrap() = None;
        *self.preview.lock().unwrap() = None;
        *self.vad.lock().unwrap() = None;
    }

    pub fn loaded_size(&self) -> Option<ModelSize> {
        self.inner.lock().unwrap().as_ref().map(|l| l.size)
    }

    /// Mark the model as wanted right now, resetting the idle clock.
    fn touch(&self) {
        *self.last_use.lock().unwrap() = Some(std::time::Instant::now());
    }
}

/// How long the model may sit unused before it is dropped.
///
/// Overridable mainly so the test below doesn't have to wait five minutes;
/// `0` disables the reaper entirely for anyone who would rather spend the
/// memory than ever wait.
fn idle_timeout() -> Option<std::time::Duration> {
    let secs = std::env::var("VOICEDUMPS_IDLE_UNLOAD_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Drop the model once nobody has used it for a while.
///
/// A loaded medium model is ~590 MB resident (measured — see
/// `benchmark_latency`), and this is a menu-bar app: closing the window keeps
/// the globe key alive, so without this the memory is held from the first
/// dictation until quit. One dictation at 9am used to cost 590 MB all day.
///
/// It is nearly free to give back. Reloading costs ~560 ms, and dictation warms
/// the model on key *down* — so the reload overlaps with the user still
/// speaking, exactly like a first-ever cold load does. The only case that pays
/// for it is a sub-second utterance that is also the first one after an idle
/// spell.
///
/// Safety no longer comes from the lock. A transcription used to hold `inner`
/// for its whole length, so the reaper simply blocked until the work finished;
/// now it hands the lock back as soon as it has a state, so that a second
/// transcription can start. Two things replace it:
///
/// * A live `WhisperState` owns an `Arc` on the model, so clearing the slot
///   cannot free weights something is still decoding with. The context outlives
///   the slot for exactly as long as it has to.
/// * `running` says whether anything is decoding, and the reaper leaves the
///   model alone while it is. Not for safety — for sense. Collecting a model
///   halfway through an hour of meeting only means the next dictation loads a
///   second copy of what is already resident.
pub fn start_idle_unload(app: tauri::AppHandle) {
    use tauri::Manager;

    let Some(timeout) = idle_timeout() else {
        return;
    };
    // Poll rather than schedule: an unused model costing a few extra seconds of
    // residency is not worth a timer that has to be cancelled and rearmed on
    // every dictation.
    let tick = (timeout / 4).clamp(
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(30),
    );

    std::thread::spawn(move || loop {
        std::thread::sleep(tick);

        let state = app.state::<EngineState>();
        // Asked before the lock, because a decode no longer holds it: a busy
        // model would otherwise look exactly like an abandoned one.
        if state.running.load(Ordering::SeqCst) > 0 {
            state.touch();
            continue;
        }
        let mut guard = state.inner.lock().unwrap();
        if guard.is_none() {
            continue;
        }
        let idle_for = state
            .last_use
            .lock()
            .unwrap()
            .map(|t| t.elapsed())
            .unwrap_or(timeout);
        if idle_for >= timeout {
            *guard = None;
            *state.preview.lock().unwrap() = None;
            eprintln!(
                "[engine] model released after {}s idle",
                idle_for.as_secs()
            );
        }
    });
}

/// Put `wanted` in the slot, loading it only if what's there isn't already it.
///
/// The caller holds the lock across the load on purpose. Two callers racing —
/// a warm-up and the transcription it was warming for — would otherwise each
/// build a context, and one would be dropped seconds later having achieved
/// nothing but a 1.5 GB spike.
fn ensure_loaded(
    app: &tauri::AppHandle,
    guard: &mut Option<Loaded>,
    wanted: ModelSize,
) -> Result<(), String> {
    if guard.as_ref().map(|l| l.size) == Some(wanted) {
        return Ok(());
    }
    let file = model_path(app, wanted).ok_or_else(|| {
        format!(
            "The {} speech model is missing from this build.",
            wanted.label()
        )
    })?;
    let ctx = WhisperContext::new_with_params(
        file.to_string_lossy().as_ref(),
        WhisperContextParameters::default(),
    )
    .map_err(|e| format!("could not load the speech model: {e}"))?;
    *guard = Some(Loaded { size: wanted, ctx });
    Ok(())
}

/// Preload the model so the next transcription can start decoding immediately.
///
/// Dictation calls this the moment the key goes down, so the couple of seconds
/// a cold load costs overlap with the user still speaking instead of landing
/// after they let go — which is the one moment the delay is unmissable.
///
/// Deliberately silent: this is an optimisation, not a step. If it fails, `run`
/// will try the same load again and report the failure properly, in the place
/// the user can actually act on it.
pub fn warm(app: &tauri::AppHandle) {
    use tauri::Manager;
    let state = app.state::<EngineState>();
    let wanted = auto_model();
    let mut guard = state.inner.lock().unwrap();
    let _ = ensure_loaded(app, &mut guard, wanted);
    // Held key, no transcription yet: the reaper must not collect the model out
    // from under the dictation this was warming for.
    state.touch();
}

// -- speech only ------------------------------------------------------------

/// How sure the detector must be that a moment is speech.
///
/// Chosen by replaying 46 real recordings, and ten-second fixtures of pure
/// silence and room noise, across 0.5, 0.35, 0.2 and 0.1. Judging sentences
/// removed nothing real at any of them. What moved was the silent skip: at 0.5
/// and 0.35 a 4.4-second dictation was called silent and never decoded; at 0.2
/// nothing real was, and silence and room noise still came back with no speech
/// in them at every threshold. 0.2 rather than 0.1 keeps a margin for rooms
/// noisier than a synthesised hiss.
///
/// The two ways to be wrong are not the same size. Too high, and a sentence
/// somebody really said is judged to have no speech under it and is removed —
/// the one failure here that nothing downstream can undo. Too low, and a little
/// room tone counts as speech, which only lets an invented sentence survive that
/// would have survived without the detector at all.
const VAD_THRESHOLD: f32 = 0.2;

/// How far from anything the detector heard a sentence may land and be kept.
///
/// Generous on purpose. Whisper's segment times are its own estimates and move
/// by a second at the edges, and a real sentence that starts a beat before the
/// detector's first frame of speech must not be judged as having none.
const NEAR_SPEECH: f64 = 1.0;

/// What the speech detector made of a recording.
///
/// **It judges what Whisper wrote; it never decides what Whisper hears.** The
/// first version did the textbook thing — cut out everything the detector did
/// not call speech and hand Whisper the rest, stitched end to end. Replayed over
/// 46 real recordings it was worse at everything this app is judged on: 7% of
/// words came back different, a short dictation vanished, a second voice under a
/// music bed lost 49 words, word timings moved by over a second at the 95th
/// percentile, and decoding took 30% longer on less audio, because every seam
/// sent the decoder back to try again.
///
/// So Whisper decodes the recording exactly as it always has, and the detector
/// is asked two things: whether anybody spoke at all, and afterwards, which
/// sentences have speech anywhere near them. The first saves decoding a
/// recording of nothing. The second is what removes "Thanks for watching!" from
/// the end of a dictation left running in a quiet room — a perfectly good
/// sentence, which no filter that reads only the text could ever catch.
enum Detected {
    /// No detector to ask — its model has not arrived — so nothing is judged.
    Unchecked,
    /// The detector heard nobody, so there is nothing to decode.
    Silent,
    /// Where the speech is, in seconds.
    Speech(Vec<(f64, f64)>),
}

/// Ask the resident detector where the speech in a recording is.
fn find_speech(app: &tauri::AppHandle, state: &EngineState, samples: &[f32]) -> Detected {
    let mut slot = state.vad.lock().unwrap();
    if slot.is_none() {
        let Some(path) = vad_path(app) else {
            return Detected::Unchecked;
        };
        match whisper_rs::WhisperVadContext::new(
            &path.to_string_lossy(),
            whisper_rs::WhisperVadContextParams::default(),
        ) {
            Ok(vad) => *slot = Some(vad),
            Err(e) => {
                eprintln!("[engine] speech detection unavailable: {e:?}");
                return Detected::Unchecked;
            }
        }
    }
    let vad = slot.as_mut().expect("just loaded");
    match speech_spans(vad, samples, VAD_THRESHOLD) {
        Ok(spans) if spans.is_empty() => Detected::Silent,
        Ok(spans) => Detected::Speech(spans),
        // A detector that fails is not evidence of silence.
        Err(why) => {
            eprintln!("[engine] speech detection failed, keeping everything: {why}");
            Detected::Unchecked
        }
    }
}

/// The detector's stretches of speech, in seconds.
fn speech_spans(
    vad: &mut whisper_rs::WhisperVadContext,
    samples: &[f32],
    threshold: f32,
) -> Result<Vec<(f64, f64)>, String> {
    let mut params = whisper_rs::WhisperVadParams::new();
    params.set_threshold(threshold);
    let found = vad
        .segments_from_samples(params, samples)
        .map_err(|e| format!("{e:?}"))?;
    Ok(found
        .map(|seg| (seg.start as f64 / 100.0, seg.end as f64 / 100.0))
        .collect())
}

/// Whether a sentence Whisper wrote has any speech near it.
fn near_speech(start: f64, end: f64, spans: &[(f64, f64)]) -> bool {
    spans
        .iter()
        .any(|&(s, e)| start < e + NEAR_SPEECH && end > s - NEAR_SPEECH)
}

/// How much quieter than the recording's own speech a sentence must be before it
/// can be judged invented.
///
/// The detector alone was not enough. Replayed over two real meetings it was right
/// once — "you", written over digital silence, Whisper's best-known invention —
/// and wrong once: "I can hear you now", said at a loudness above that meeting's
/// median, which the detector did not call speech. So a sentence is removed only
/// when everything agrees: nothing the detector heard is near it, the audio under it
/// is less than half as loud as the speech it did hear, *and* it is quiet outright.
/// Invented sentences live over silence and room tone; missed speech is at speaking
/// volume. Where they disagree, the words stay.
const QUIETER_THAN_SPEECH: f64 = 0.5;

/// Quiet outright: −40 dBFS, as root mean square on a ±1 scale.
///
/// The relative rule alone failed on a mixed meeting recording, where the far side
/// was loud enough to put "half as loud as the speech" above a sentence actually
/// spoken at 0.036 — so it was removed. This is the floor that stops that: "you"
/// was written over 0.0000 and goes; "I can hear you now" was at 0.036 and stays.
/// The noisiest room in the replay, a meeting with its microphone at the noise
/// floor, sat at 0.0107 in its quietest tenth — above this, which means an invented
/// sentence over a floor that loud would be kept. That is the direction to be
/// wrong in.
const QUIET_OUTRIGHT: f64 = 0.01;

/// The loudness of a stretch of a recording, as root mean square.
fn loudness(samples: &[f32], from: f64, to: f64) -> f64 {
    let rate = SAMPLE_RATE as f64;
    let a = ((from.max(0.0) * rate) as usize).min(samples.len());
    let b = ((to.max(0.0) * rate) as usize).min(samples.len());
    if b <= a {
        return 0.0;
    }
    (samples[a..b].iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / (b - a) as f64).sqrt()
}

/// Keep only the sentences that have speech near them or are spoken as loud as speech.
fn judged(segments: Vec<Value>, heard: &Detected, samples: &[f32]) -> Vec<Value> {
    let Detected::Speech(spans) = heard else {
        return segments;
    };
    // The recording's own voice: how loud the stretches the detector called
    // speech are. Relative, so a quiet microphone is judged against itself.
    let rate = SAMPLE_RATE as f64;
    let (energy, length) = spans.iter().fold((0.0_f64, 0_usize), |(energy, length), &(s, e)| {
        let a = ((s.max(0.0) * rate) as usize).min(samples.len());
        let b = ((e.max(0.0) * rate) as usize).min(samples.len());
        let here: f64 = samples[a..b.max(a)].iter().map(|x| (*x as f64).powi(2)).sum();
        (energy + here, length + b.saturating_sub(a))
    });
    let voice = if length == 0 { 0.0 } else { (energy / length as f64).sqrt() };
    segments
        .into_iter()
        .filter(|seg| {
            let (start, end) = (seg["start"].as_f64().unwrap_or(0.0), seg["end"].as_f64().unwrap_or(0.0));
            let level = loudness(samples, start, end);
            near_speech(start, end, spans) || level >= QUIET_OUTRIGHT || level >= QUIETER_THAN_SPEECH * voice
        })
        .collect()
}

/// Read a finished decode back out as segments with word times.
///
/// Shared by the app and by the replay that checks it, so that what is measured
/// is the code that ships rather than a copy of it.
fn collect(
    st: &whisper_rs::WhisperState,
    shift: f64,
    language: &str,
    progress: &mut dyn FnMut(usize, usize),
) -> Vec<Value> {
    let n = st.full_n_segments();
    let mut segments: Vec<Value> = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        let Some(seg) = st.get_segment(i) else { continue };
        let text = seg.to_str_lossy().unwrap_or_default().into_owned();
        let trimmed = text.trim();
        if trimmed.is_empty() || is_non_speech(trimmed) {
            continue;
        }
        // whisper.cpp reports timestamps in centiseconds. `shift` places a
        // decode of part of a recording back where that part was.
        let start = seg.start_timestamp() as f64 / 100.0 + shift;
        let end = seg.end_timestamp() as f64 / 100.0 + shift;
        let mut sure = 0.0_f32;

        // Per-token times are what make the reading view follow along word by
        // word during playback, so they're worth reassembling here.
        let mut words: Vec<Value> = Vec::new();
        for t in 0..seg.n_tokens() {
            let Some(tok) = seg.get_token(t) else { continue };
            let Ok(raw) = tok.to_str_lossy() else { continue };
            // Whisper emits control tokens inline ([_BEG_], <|notimestamps|>…).
            if raw.starts_with("[_") || raw.starts_with("<|") || raw.trim().is_empty() {
                continue;
            }
            let data = tok.token_data();
            sure += data.p;
            words.push(json!({
                "start": data.t0 as f64 / 100.0 + shift,
                "end": data.t1 as f64 / 100.0 + shift,
                "text": raw,
            }));
        }

        let confidence = if words.is_empty() { 0.0 } else { sure / words.len() as f32 };
        segments.push(json!({
            "start": start,
            "end": end,
            "text": trimmed,
            "words": words,
            "language": language,
            "confidence": confidence,
        }));
        progress((i + 1) as usize, n.max(1) as usize);
    }
    segments
}

// -- what has been learned ---------------------------------------------------

/// What a person has taught the app, read once per transcription.
struct Lessons {
    /// Languages they speak, the Mac's preferred first.
    languages: Vec<&'static str>,
    /// Taught spellings — see `vocabulary::spell`.
    terms: Vec<String>,
    /// Taught names that may be recognised by sound — see `vocabulary::resemble`.
    names: Vec<String>,
    /// Mishearings corrected often enough to rewrite.
    rules: Vec<(String, String)>,
}

impl Lessons {
    fn from_app(app: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        let mut languages: Vec<&'static str> = crate::settings::languages(app)
            .iter()
            .filter_map(|code| static_code(code))
            .collect();
        if languages.is_empty() {
            languages.push("en");
        }
        let (terms, names, rules) = app
            .try_state::<crate::store::Store>()
            .map(|store| {
                let conn = store.0.lock().unwrap();
                (
                    crate::vocabulary::spellings(&conn),
                    crate::vocabulary::sound_alikes(&conn),
                    crate::vocabulary::rules(&conn),
                )
            })
            .unwrap_or_default();
        Lessons { languages, terms, names, rules }
    }
}

/// A language code as the `'static` string whisper.cpp keeps for it.
fn static_code(code: &str) -> Option<&'static str> {
    whisper_rs::get_lang_id(code).and_then(whisper_rs::get_lang_str)
}

/// The languages the live preview should listen for.
pub fn preview_languages(app: &tauri::AppHandle) -> Vec<&'static str> {
    Lessons::from_app(app).languages
}

/// Point a decode at a language — and deliberately at nothing else.
///
/// The vocabulary is not given to the decoder. It was, the way Whisper is
/// usually taught names: carried in front of every thirty-second window, with the
/// context budget sized to hold exactly that and nothing decoded. Replayed over 46
/// real recordings with six real product names, it spelled one of them right that
/// had been wrong — and changed 49 content words to do it, deleting a whole
/// two-second dictation ("testing") and words from the middle of sentences
/// ("partnerships", "tools", "I created"). A prompt does not only suggest names;
/// it restyles everything it sits in front of. Taught spellings are applied after
/// decoding instead — see `taught` — where they can change nothing but the words
/// they match.
fn condition(params: &mut FullParams<'static, 'static>, language: &'static str) {
    params.set_language(Some(language));
}

// -- languages ---------------------------------------------------------------

/// The model's odds for every language, heard from this stretch alone.
///
/// The spectrogram is made from the stretch and nothing around it: detection
/// encodes a thirty-second window from its offset, so asked about three seconds
/// of Hindi inside a longer recording it would also be listening to whatever
/// English came after.
///
/// And it is heard at a context the size of the stretch. The encoder normally
/// reads thirty seconds whatever it is given, and asking about a two-second
/// stretch at that size is what made declaring a second language cost 54% more
/// time over 46 English recordings — a median of 678 ms on every short dictation.
/// A detect-only decode sets the encoder to the stretch's own length, which the
/// following detection inherits, and stops before writing anything.
fn language_odds(st: &mut whisper_rs::WhisperState, slice: &[f32]) -> Option<Vec<f32>> {
    if slice.len() < SAMPLE_RATE as usize {
        return None;
    }
    let seconds = slice.len() as f64 / SAMPLE_RATE as f64;
    // 50 encoder frames a second, a margin, and never past the model's 1500.
    let frames = ((seconds * 50.0).ceil() as i32 + 64).min(1500);
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("auto"));
    params.set_detect_language(true);
    params.set_audio_ctx(frames);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_n_threads(num_threads() as i32);
    st.full(params, slice).ok()?;
    st.lang_detect(0, num_threads()).ok().map(|(_, odds)| odds)
}

fn odds_of(odds: &[f32], code: &str) -> f32 {
    whisper_rs::get_lang_id(code)
        .and_then(|id| odds.get(id as usize).copied())
        .unwrap_or(0.0)
}

/// The likeliest of the allowed languages — never one outside them.
fn likeliest(odds: &[f32], allowed: &[&'static str]) -> Option<&'static str> {
    allowed
        .iter()
        .copied()
        .max_by(|a, b| odds_of(odds, a).total_cmp(&odds_of(odds, b)))
}

/// How much speech is enough to name the language it is in.
///
/// Four seconds. Twelve was tried and named the same languages with the same
/// confidence on every recording, so the extra audio bought nothing — the cost
/// of asking is the encoder pass itself, not the length of what it reads.
const ENOUGH_TO_NAME: f64 = 4.0;

/// How unlikely a language may be and still be chosen over the one listed first.
///
/// A ratio, not a level. The model is emphatic when it knows — 0.972 for Hindi
/// against 0.018 for English — and merely ahead when it does not: 0.512 against
/// 0.326 on a sentence that turned out to be English. A ratio separates those
/// two; a level does not, and an absolute floor of 0.5 tried first let a
/// six-to-one favourite lose to a long shot because neither reached it. This
/// floor only refuses to choose between two languages both thought improbable.
const NAMED_LANGUAGE_ODDS: f32 = 0.15;

/// The language a recording is actually in, when more than one is declared.
///
/// **Why this has to be asked.** Told the audio is English, Whisper does not
/// give up on speech that is not — it translates it, fluently and with every
/// appearance of confidence. A whole dictation in Hindi came back as "I think we
/// should stop till tomorrow": nothing garbled, nothing missing, nothing to
/// suggest the words had ever been in another language. The pass below that
/// re-examines speech which produced *no words* can never fire on that, because
/// the words are all there. They are simply not the ones that were said.
///
/// **It is not free, and cannot be made free.** `whisper_lang_auto_detect` runs
/// the encoder before it answers, so naming the language is a second encoder
/// pass however it is asked. Decoding on `auto` instead — letting whisper.cpp
/// name it during the decode — was built and measured as the cheaper route and
/// is not: it costs the same second pass, changed a word in 46 English
/// recordings, returned an entire short dictation empty, and picked languages
/// nobody had declared. This asks plainly and keeps the transcripts intact.
///
/// **Only people who declare a second language pay.** For them it buys the
/// feature working at all: today their Hindi comes back as English prose.
///
/// **Biased towards the language listed first.** Another has to be twice as
/// likely before it wins, because being wrong here changes the script of a whole
/// dictation, and most people who add a second language still mostly speak the
/// first.
fn spoken_language(
    st: &mut whisper_rs::WhisperState,
    samples: &[f32],
    spans: &[(f64, f64)],
    allowed: &[&'static str],
) -> &'static str {
    let first = allowed.first().copied().unwrap_or("en");
    let Some(&(start, _)) = spans.first() else {
        return first;
    };
    let from = (start * SAMPLE_RATE as f64) as usize;
    let to = (((start + ENOUGH_TO_NAME) * SAMPLE_RATE as f64) as usize).min(samples.len());
    if to <= from {
        return first;
    }
    let Some(odds) = language_odds(st, &samples[from..to]) else {
        return first;
    };
    // A threshold nobody can see is a threshold nobody can tune. Set
    // `VOICEDUMPS_LANG_ODDS` to watch what the model actually thought.
    if std::env::var("VOICEDUMPS_LANG_ODDS").is_ok() {
        let seen: Vec<String> = allowed.iter().map(|l| format!("{l}={:.3}", odds_of(&odds, l))).collect();
        eprintln!("[lang] {}", seen.join("  "));
    }
    let Some(best) = likeliest(&odds, allowed) else {
        return first;
    };
    if best == first {
        return first;
    }
    let (p_best, p_first) = (odds_of(&odds, best), odds_of(&odds, first));
    if p_best >= NAMED_LANGUAGE_ODDS && p_best >= 2.0 * p_first {
        best
    } else {
        first
    }
}

/// Pad either side of a decoded word when deciding what speech it accounts for.
const UNHEARD_PAD: f64 = 0.3;
/// The longest a single word can reasonably take. Whisper's word times stretch
/// the last word of a segment to the segment's end, which would otherwise claim
/// seconds of speech in another language as heard.
const WORD_SPAN: f64 = 0.8;
/// The shortest stretch worth asking about. Language detection on a second is a
/// guess, a cough is not a sentence in Hindi, and every question asked is an
/// encoder pass paid for. Two seconds still takes the Hindi sentence in this
/// project's fixture, which left a 2.1-second gap.
const UNHEARD_MIN: f64 = 2.0;
/// Unheard stretches closer than this are one question, not several.
const UNHEARD_JOIN: f64 = 2.0;
/// How sure detection must be that unheard speech is in another language.
const OTHER_LANGUAGE_ODDS: f32 = 0.5;

/// Speech the detector heard that no decoded word accounts for.
///
/// This is where a second language goes missing. Decoded in English, a Hindi
/// sentence does not come back as bad English — measured on this project's
/// fixture, it came back as nothing at all, and the English either side was
/// perfect. So the place to look is not at the words, but at the speech with no
/// words on it.
fn unheard(spans: &[(f64, f64)], segments: &[Value]) -> Vec<(f64, f64)> {
    let at = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
    let mut covered: Vec<(f64, f64)> = Vec::new();
    for seg in segments {
        match seg["words"].as_array() {
            Some(words) if !words.is_empty() => {
                for w in words {
                    let start = at(w, "start");
                    let end = at(w, "end").min(start + WORD_SPAN);
                    covered.push((start - UNHEARD_PAD, end + UNHEARD_PAD));
                }
            }
            _ => covered.push((at(seg, "start") - UNHEARD_PAD, at(seg, "end") + UNHEARD_PAD)),
        }
    }

    let mut raw: Vec<(f64, f64)> = Vec::new();
    for &(start, end) in spans {
        let mut pieces = vec![(start, end)];
        for &(cs, ce) in &covered {
            pieces = pieces
                .into_iter()
                .flat_map(|(ps, pe)| {
                    if ce <= ps || cs >= pe {
                        vec![(ps, pe)]
                    } else {
                        let mut left = Vec::new();
                        if cs > ps {
                            left.push((ps, cs));
                        }
                        if ce < pe {
                            left.push((ce, pe));
                        }
                        left
                    }
                })
                .collect();
        }
        raw.extend(pieces);
    }

    // One detection per region, not per fragment: a sentence in another language
    // is often broken by the odd word the main decode did catch.
    raw.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut joined: Vec<(f64, f64)> = Vec::new();
    for (ps, pe) in raw {
        match joined.last_mut() {
            Some(last) if ps - last.1 < UNHEARD_JOIN && pe - last.0 <= 30.0 => last.1 = last.1.max(pe),
            _ => joined.push((ps, pe)),
        }
    }

    let mut out = Vec::new();
    for (ps, pe) in joined {
        let mut t = ps;
        while pe - t >= UNHEARD_MIN {
            let u = (t + 30.0).min(pe);
            out.push((t, u));
            t = u;
        }
    }
    out
}

/// Whether a re-decode came back as words the model stands behind.
fn confident(segments: &[Value]) -> bool {
    let words: usize = segments
        .iter()
        .map(|s| s["text"].as_str().unwrap_or("").split_whitespace().count())
        .sum();
    let sure = segments
        .iter()
        .map(|s| s["confidence"].as_f64().unwrap_or(0.0))
        .sum::<f64>()
        / segments.len().max(1) as f64;
    words >= 2 && sure >= 0.5
}

/// Hear unheard speech again, in whichever other declared language it is in.
///
/// The cost is paid only where words went missing: speech in the main language
/// is accounted for by its own words and never re-examined.
fn with_other_languages(
    st: &mut whisper_rs::WhisperState,
    samples: &[f32],
    spans: &[(f64, f64)],
    mut segments: Vec<Value>,
    main: &'static str,
    allowed: &[&'static str],
) -> Vec<Value> {
    let others: Vec<&'static str> = allowed.iter().copied().filter(|c| *c != main).collect();
    let rate = SAMPLE_RATE as f64;
    for (start, end) in unheard(spans, &segments) {
        let a = ((start.max(0.0) * rate) as usize).min(samples.len());
        let b = ((end * rate) as usize).min(samples.len());
        if b <= a {
            continue;
        }
        let Some(odds) = language_odds(st, &samples[a..b]) else { continue };
        let Some(other) = likeliest(&odds, &others) else { continue };
        let (p_other, p_main) = (odds_of(&odds, other), odds_of(&odds, main));
        if p_other < OTHER_LANGUAGE_ODDS || p_other < 2.0 * p_main {
            continue;
        }
        let mut params = decoding();
        condition(&mut params, other);
        if st.full(params, &samples[a..b]).is_err() {
            continue;
        }
        let found = collect(st, a as f64 / rate, other, &mut |_, _| {});
        if confident(&found) {
            segments.extend(found);
        }
    }
    segments.sort_by(|x, y| {
        x["start"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&y["start"].as_f64().unwrap_or(0.0))
    });
    segments
}

/// Apply earned rewrites and taught spellings — see `vocabulary`.
///
/// To the text of every segment, and to single-word tokens too, so the word the
/// reading view highlights during playback is the word the text says. A rewrite
/// that joins several words ("hyper frames" → HyperFrames) changes the text only;
/// its tokens cannot be merged without inventing times for them.
fn taught(
    segments: Vec<Value>,
    terms: &[String],
    names: &[String],
    rules: &[(String, String)],
) -> Vec<Value> {
    if terms.is_empty() && names.is_empty() && rules.is_empty() {
        return segments;
    }
    let key = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let mut single: Vec<(String, &str)> = rules
        .iter()
        .filter(|(heard, _)| !heard.contains(' '))
        .map(|(heard, term)| (key(heard), term.as_str()))
        .collect();
    single.extend(
        terms
            .iter()
            .filter(|t| !t.contains(' ') && t.chars().any(|c| c.is_uppercase() || c.is_ascii_digit()))
            .map(|t| (key(t), t.as_str())),
    );
    segments
        .into_iter()
        .map(|mut seg| {
            if let Some(text) = seg["text"].as_str().map(str::to_string) {
                let written = crate::vocabulary::spell(&crate::vocabulary::apply(&text, rules), terms);
                seg["text"] = json!(crate::vocabulary::resemble(&written, names));
            }
            if let Some(words) = seg["words"].as_array_mut() {
                for w in words.iter_mut() {
                    let Some(raw) = w["text"].as_str().map(str::to_string) else { continue };
                    let Some((_, term)) = single.iter().find(|(h, _)| *h == key(&raw)) else {
                        // No spelling matched its letters. It may still be a
                        // taught name spelt a way nobody listed — `resemble`
                        // keeps the space and punctuation around it itself.
                        // Asked here as well as inside `resemble`, so the table
                        // of sounds is not built again for every ordinary word
                        // in the transcript — only for the few written as names.
                        if !raw.chars().find(|c| c.is_alphanumeric()).is_some_and(char::is_uppercase) {
                            continue;
                        }
                        let sounded = crate::vocabulary::resemble(&raw, names);
                        if sounded != raw {
                            w["text"] = json!(sounded);
                        }
                        continue;
                    };
                    let lead: String = raw.chars().take_while(|c| !c.is_alphanumeric()).collect();
                    let trail: String = raw
                        .chars()
                        .rev()
                        .take_while(|c| !c.is_alphanumeric())
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    w["text"] = json!(format!("{lead}{term}{trail}"));
                }
            }
            seg
        })
        .collect()
}

/// Everything between audio and finished segments, as the app runs it.
///
/// One function for the app and for every replay that measures it, so a number
/// in a commit message is a number about the code that ships.
fn decode_with(
    st: &mut whisper_rs::WhisperState,
    samples: &[f32],
    heard: &Detected,
    lessons: &Lessons,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(Vec<Value>, &'static str), String> {
    let spans: &[(f64, f64)] = match heard {
        Detected::Speech(spans) => spans,
        _ => &[],
    };
    // Which language this actually is. Nothing at all when one is declared; a
    // second encoder pass when more are, which is what naming it costs however
    // it is asked — see `spoken_language`, and why the unheard-speech pass
    // below cannot answer this question.
    let language = if lessons.languages.len() > 1 {
        spoken_language(st, samples, spans, &lessons.languages)
    } else {
        lessons.languages.first().copied().unwrap_or("en")
    };
    let mut params = decoding();
    condition(&mut params, language);
    st.full(params, samples)
        .map_err(|e| format!("transcription failed: {e}"))?;
    let mut segments = collect(st, 0.0, language, progress);
    if lessons.languages.len() > 1 && !spans.is_empty() {
        segments = with_other_languages(
            st,
            samples,
            spans,
            segments,
            language,
            &lessons.languages,
        );
    }
    let segments = collapse_repeats(judged(segments, heard, samples));
    Ok((taught(segments, &lessons.terms, &lessons.names, &lessons.rules), language))
}

/// Whether a segment is whisper narrating the audio rather than transcribing it.
///
/// whisper emits `[BLANK_AUDIO]` for silence, and `[MUSIC]`, `(upbeat music)`,
/// `*door creaks*` and friends for anything else it hears but cannot render as
/// speech. They are useful in subtitles and actively wrong here: this app
/// pastes its output into whatever the user was typing in, and nobody wants
/// `[BLANK_AUDIO]` in the middle of their message. Four had already reached the
/// database before this existed.
///
/// The test is structural rather than a list of known strings — any segment
/// wholly wrapped in brackets, parentheses or asterisks is the model describing
/// a sound, because real speech does not arrive fully parenthesised.
fn is_non_speech(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 2 {
        return false;
    }
    let b = s.as_bytes();
    let wrapped = matches!(
        (b[0], b[b.len() - 1]),
        (b'[', b']') | (b'(', b')') | (b'*', b'*')
    );
    // Only if the wrapper encloses the whole thing: "(as I said) we shipped"
    // is speech that happens to open with a bracket.
    wrapped
        && !s[1..s.len() - 1].contains('[')
        && !s[1..s.len() - 1].contains('(')
}

/// How many identical segments in a row are still allowed to be speech.
///
/// Two is comfortably real: "Yeah. Yeah." is how people talk. Three of exactly
/// the same string, with the same punctuation, whisper's own sentence splitter
/// having decided three times that a sentence ended — that is a decoder that has
/// stopped listening.
const A_RUN_TOO_LONG_TO_BE_SPEECH: usize = 3;

/// Throw away a decoder that got stuck on one phrase.
///
/// The cause of the stuck decoder is fixed — see [`decoding`] — but this is not
/// a second attempt at that fix. It is the check that would have caught it, and
/// caught it in the one place a person would ever see it. A fifty-one minute
/// meeting saved with "Morning." fifty-three times, "Okay." twelve times, and
/// one sentence about a centre frame eight times; a sixth of every word in that
/// note was a phrase the model had latched onto. Nobody needed a root cause to
/// know it was wrong, and nothing in the app said so.
///
/// So a run is cut to its first two and the rest goes. Two rather than one
/// because two is a thing people say, and because the phrase was almost
/// certainly said at least once — cutting to nothing would put a hole in the
/// timeline exactly where somebody was speaking.
fn collapse_repeats(segments: Vec<Value>) -> Vec<Value> {
    /// Same words, ignoring case and punctuation — "Okay." and "okay!" are the
    /// same latch, and a run that alternates between them is still a run.
    fn key(segment: &Value) -> String {
        segment["text"]
            .as_str()
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    let mut out: Vec<Value> = Vec::with_capacity(segments.len());
    let mut run = 0usize;
    let mut previous = String::new();

    for segment in segments {
        let this = key(&segment);
        // An empty key is punctuation or a stray mark, not a phrase being
        // repeated; counting those as a run would merge unrelated fragments.
        if !this.is_empty() && this == previous {
            run += 1;
        } else {
            run = 1;
            previous = this;
        }
        if run < A_RUN_TOO_LONG_TO_BE_SPEECH {
            out.push(segment);
        }
    }
    out
}

/// A live-preview session: one whisper state, reused for every pass.
///
/// The state is the expensive part, not the context. `whisper_init_state`
/// allocates the KV caches and compute buffers — about 530 MB for `medium` —
/// so creating one per pass, several times a second, is not a slow version of
/// this. It is a broken one: the first preview lands and every later pass dies
/// trying to allocate another half-gigabyte, which reads to the user as the
/// transcript freezing on its opening line.
///
/// Held for the length of one dictation and dropped on key-up, so the final
/// pass never allocates its own alongside a live one.
pub struct Preview {
    state: whisper_rs::WhisperState,
}

/// One word of the live preview, and how sure the model was of it.
///
/// The preview runs on `small`, which is fast and wrong more often than the
/// model that produces the pasted text — so the overlay says which words it is
/// standing behind rather than presenting every guess with equal confidence.
pub struct Heard {
    pub word: String,
    /// 0…1. The geometric mean of the probabilities whisper assigned to the
    /// tokens this word is made of — geometric because a word is only as
    /// certain as its least certain piece, and an arithmetic mean lets one
    /// confident token carry a doubtful one.
    pub confidence: f32,
}

/// Reassemble whisper's tokens into words, carrying their probabilities.
///
/// Whisper's BPE emits the space that belongs *in front of* a token as part of
/// it, so a leading space is exactly the signal that a new word has started —
/// which is also why "graphify" arrives as `" graph"` + `"ify"` and must be
/// joined back up rather than shown as two words with two confidences.
fn words_with_confidence(seg: &whisper_rs::WhisperSegment<'_>, out: &mut Vec<Heard>) {
    /// A word being built: its text so far, the sum of its tokens' ln(p), and
    /// how many tokens went into it.
    type Open = Option<(String, f32, u32)>;

    fn flush(open: &mut Open, out: &mut Vec<Heard>) {
        let Some((word, sum_ln, n)) = open.take() else {
            return;
        };
        let word = word.trim().to_string();
        if !word.is_empty() {
            out.push(Heard {
                confidence: (sum_ln / n.max(1) as f32).exp().clamp(0.0, 1.0),
                word,
            });
        }
    }

    let mut open: Open = None;
    for t in 0..seg.n_tokens() {
        let Some(tok) = seg.get_token(t) else { continue };
        let Ok(raw) = tok.to_str_lossy() else { continue };
        // Whisper emits control tokens inline ([_BEG_], <|notimestamps|>…).
        if raw.starts_with("[_") || raw.starts_with("<|") {
            continue;
        }
        // A token that is nothing but space carries no letters but still ends
        // whatever word was open.
        if raw.trim().is_empty() {
            flush(&mut open, out);
            continue;
        }
        // `p` rather than `plog`: it is the field the decoder always fills, and
        // clamping off zero keeps `ln` finite for a token the model gave no
        // weight at all.
        let ln = tok.token_data().p.clamp(1e-6, 1.0).ln();
        match open.as_mut() {
            Some(w) if !raw.starts_with(' ') => {
                w.0.push_str(&raw);
                w.1 += ln;
                w.2 += 1;
            }
            _ => {
                flush(&mut open, out);
                open = Some((raw.trim_start().to_string(), ln, 1));
            }
        }
    }
    flush(&mut open, out);
}

impl Preview {
    /// Borrow the loaded model long enough to build a state.
    ///
    /// `None` while the engine is busy or still warming — the caller simply
    /// tries again on its next turn rather than blocking the recording.
    pub fn start(app: &tauri::AppHandle) -> Option<Self> {
        use tauri::Manager;
        let state = app.state::<EngineState>();

        // If the main engine is already on `small` there is nothing faster to
        // switch to, so share its context rather than loading a second copy.
        {
            let guard = state.inner.try_lock().ok()?;
            if let Some(loaded) = guard.as_ref() {
                if loaded.size == ModelSize::Small {
                    return Some(Preview {
                        state: loaded.ctx.create_state().ok()?,
                    });
                }
            }
        }

        let mut slot = state.preview.try_lock().ok()?;
        if slot.is_none() {
            let file = model_path(app, ModelSize::Small)?;
            *slot = WhisperContext::new_with_params(
                file.to_string_lossy().as_ref(),
                WhisperContextParameters::default(),
            )
            .ok();
        }
        Some(Preview {
            state: slot.as_ref()?.create_state().ok()?,
        })
    }

    /// Transcribe what has been said so far, abandoning the attempt the moment
    /// `cancel` is set.
    ///
    /// This is the disposable half of dictation. Its output is shown while the
    /// user is still speaking and then thrown away — what gets pasted always
    /// comes from one clean pass over the complete audio in [`run`], because a
    /// stitched sequence of partials is measurably worse than a single read of
    /// the whole thing, and the pasted text is not the place to trade accuracy
    /// for feel.
    ///
    /// Cancellation is checked around the pass, not inside it.
    ///
    /// whisper-rs 0.16's `set_abort_callback_safe` cannot be used: it boxes the
    /// closure as `Box<dyn FnMut() -> bool>`, boxes that again, and hands out a
    /// `*mut Box<dyn FnMut>` — but installs `trampoline::<F>`, which casts the
    /// pointer back to `*mut F`, the original closure type. Calling it
    /// reinterprets a fat-pointer box as the closure struct and returns
    /// whatever happens to be in those bytes. When that is truthy ggml aborts
    /// the graph, which is exactly the `failed to encode` storm this used to
    /// produce.
    ///
    /// Checking between passes is enough now that a pass covers seconds rather
    /// than the whole recording: the worst key-up delay is one short chunk.
    pub fn step(
        &mut self,
        samples: &[f32],
        cancel: Arc<AtomicBool>,
        languages: &[&'static str],
    ) -> Option<Vec<Heard>> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // Below about a second whisper invents words rather than admitting it
        // heard nothing, and a panel that flashes a hallucinated sentence
        // before the real one is worse than one that stays quiet a moment
        // longer.
        if samples.len() < SAMPLE_RATE as usize {
            return None;
        }

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads((num_threads() as i32).max(1));
        // No token timestamps: the panel shows words, not a scrubber, and they
        // cost real time to compute.
        params.set_token_timestamps(false);
        // The first declared language, never the model's free guess — which on a
        // chunk of breath is how a preview flashes a sentence in Norwegian. Not
        // detected per chunk either: that is an encoder pass on every chunk of a
        // draft. Speech in another declared language comes back from this as
        // nothing rather than as wrong English (measured on the Hindi fixture),
        // so the preview stays quiet and the final pass finds it.
        params.set_language(Some(languages.first().copied().unwrap_or("en")));

        // A failed pass is not worth surfacing — the final transcription is
        // what the user actually receives.
        self.state.full(params, samples).ok()?;
        if cancel.load(Ordering::Relaxed) {
            return None;
        }

        let n = self.state.full_n_segments();
        let mut out: Vec<Heard> = Vec::new();
        for i in 0..n {
            let Some(seg) = self.state.get_segment(i) else {
                continue;
            };
            let piece = seg.to_str_lossy().unwrap_or_default();
            let piece = piece.trim();
            if piece.is_empty() || is_non_speech(piece) {
                continue;
            }
            words_with_confidence(&seg, &mut out);
        }
        (!out.is_empty()).then_some(out)
    }
}

/// The same live preview, read again by the model that will actually be pasted.
///
/// [`Preview`] buys latency with `small`, which is roughly four times quicker
/// and wrong more often. This reads the *same chunks* a second time on the
/// already-resident `medium` and hands back a better answer for the ones it
/// finishes, so the panel reads fast at the edge and accurate behind it.
///
/// It exists only where there is something to gain: `start` returns `None`
/// unless the engine is already holding `medium`. On a machine small enough to
/// be running `small` for real transcription, the preview is already using the
/// best model there is and a second pass would buy nothing for a great deal of
/// memory.
///
/// No model is loaded here — the resident one is borrowed. What this does cost
/// is a second `WhisperState` on `medium`, which is the ~530 MB of KV caches
/// and compute buffers described on [`Preview`], so it is dropped on key-up
/// before the final pass allocates its own.
pub struct Refine {
    state: whisper_rs::WhisperState,
}

/// The answer to "can this dictation be refined?", which is not always known
/// when it is first asked.
///
/// The distinction matters because the two negative answers want opposite
/// handling. `Never` is a property of the machine — the engine is on `small`,
/// there is nothing better to re-read with, and the caller should stop asking.
/// `NotYet` is a moment in time: the model is still warming, or another thread
/// is holding it. Treating that as `Never` is the bug this enum exists to
/// prevent — the model is *usually* still loading when a dictation starts, so a
/// single up-front ask fails on exactly the first dictation after launch and
/// then silently never refines again.
pub enum Refinable {
    Ready(Refine),
    NotYet,
    Never,
}

impl Refine {
    pub fn start(app: &tauri::AppHandle) -> Refinable {
        use tauri::Manager;
        let state = app.state::<EngineState>();
        // Not while a real transcription is decoding.
        //
        // This never used to be reachable: `transcribe` held the slot's lock for
        // the whole of a decode, so the `try_lock` below simply failed. Now that
        // it doesn't, a dictation started during an hour-long meeting would add
        // a third heavy state — 530 MB of caches and compute buffers on
        // `medium` — to buy a better *preview*. The pasted text is unaffected;
        // it comes from the final pass. `NotYet` is already the word for "ask
        // again in a moment", and a moment is all this is.
        if state.running.load(Ordering::SeqCst) > 0 {
            return Refinable::NotYet;
        }
        let Ok(guard) = state.inner.try_lock() else {
            return Refinable::NotYet;
        };
        let Some(loaded) = guard.as_ref() else {
            return Refinable::NotYet;
        };
        if loaded.size != ModelSize::Medium {
            return Refinable::Never;
        }
        match loaded.ctx.create_state() {
            Ok(state) => Refinable::Ready(Refine { state }),
            // Out of memory for the KV caches, most likely. Worth another try
            // on the next turn rather than giving up on the whole dictation.
            Err(_) => Refinable::NotYet,
        }
    }

    /// Re-read one chunk. Same contract as [`Preview::step`] — `None` for a
    /// cancelled or failed pass, and the caller simply keeps what it had.
    pub fn step(
        &mut self,
        samples: &[f32],
        cancel: Arc<AtomicBool>,
        languages: &[&'static str],
    ) -> Option<Vec<Heard>> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_token_timestamps(false);
        // Half the threads, floored at one. The fast pass is what the user is
        // reading at the edge of their sentence; if this took every core, the
        // words would arrive later in exchange for being righter sooner, which
        // is the wrong way round for a preview.
        params.set_n_threads(((num_threads() / 2).max(1)) as i32);
        params.set_language(Some(languages.first().copied().unwrap_or("en")));

        self.state.full(params, samples).ok()?;
        if cancel.load(Ordering::Relaxed) {
            return None;
        }

        let n = self.state.full_n_segments();
        let mut out: Vec<Heard> = Vec::new();
        for i in 0..n {
            let Some(seg) = self.state.get_segment(i) else {
                continue;
            };
            let piece = seg.to_str_lossy().unwrap_or_default();
            let piece = piece.trim();
            if piece.is_empty() || is_non_speech(piece) {
                continue;
            }
            words_with_confidence(&seg, &mut out);
        }
        (!out.is_empty()).then_some(out)
    }
}

/// Resample arbitrary-rate mono audio to what whisper expects.
///
/// Exposed for live preview, which reads the microphone at whatever rate the
/// device runs at rather than going through a decoded file.
pub fn to_engine_rate(input: &[f32], src_rate: u32) -> Vec<f32> {
    resample_to_16k(input, src_rate)
}

// -- decoding ---------------------------------------------------------------

/// Decode any supported media file to 16 kHz mono f32.
///
/// symphonia is pure Rust, so this is what lets the app drop its `ffmpeg`
/// dependency: mp3, m4a/aac, wav, flac, ogg and mp4/mov audio tracks all decode
/// in-process.
pub(crate) fn decode_mono_16k(path: &Path) -> Result<Vec<f32>, String> {
    let (samples, rate) = decode_mono(path)?;
    Ok(resample_to_16k(&samples, rate))
}

/// Decode to mono f32 at whatever rate the file is in.
///
/// The transcription path immediately resamples this to 16 kHz, but the media
/// library wants the audio at its own rate — that copy is for a human to listen
/// to, and downsampling a 48 kHz recording to model rate would make every
/// archived note sound like a phone call.
pub fn decode_mono(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("could not open the file: {e}"))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported or damaged media: {e}"))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| "no audio track in that file".to_string())?;
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("no decoder for that audio: {e}"))?;

    let mut src_rate = track.codec_params.sample_rate.unwrap_or(SAMPLE_RATE);

    let mut mono: Vec<f32> = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean end of stream, or a truncated file we've read all we can of.
            Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(format!("read error: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                // Trust the decoded spec over the container header, which
                // can disagree with the actual stream.
                src_rate = spec.rate;
                let channels = spec.channels.count().max(1);

                let sb = buf.get_or_insert_with(|| {
                    SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)
                });
                sb.copy_interleaved_ref(decoded);

                // Downmix to mono by averaging: taking one channel throws away
                // half of a stereo interview.
                for frame in sb.samples().chunks(channels) {
                    mono.push(frame.iter().sum::<f32>() / channels as f32);
                }
            }
            // A corrupt packet mid-file shouldn't lose the whole recording.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode error: {e}")),
        }
    }

    if mono.is_empty() {
        return Err("that file contains no audio".into());
    }

    Ok((mono, src_rate))
}

/// Linear resample to 16 kHz.
///
/// Whisper's own reference pipeline low-passes first; at these ratios (48k/44.1k
/// down to 16k) the difference is inaudible to the model, and linear keeps this
/// dependency-free.
fn resample_to_16k(input: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == SAMPLE_RATE || src_rate == 0 {
        return input.to_vec();
    }
    let ratio = SAMPLE_RATE as f64 / src_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// Downsample the waveform to fixed 0..1 buckets for the scrubber.
///
/// RMS rather than peak, because peak spikes on clicks and renders as noise;
/// normalised to the loudest bucket, because an absolute scale draws a quiet
/// recording as a flat line.
fn compute_peaks(samples: &[f32]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let buckets = PEAK_BUCKETS.min(samples.len()).max(1);
    let per = samples.len() / buckets;
    if per == 0 {
        return Vec::new();
    }
    let mut rms: Vec<f32> = (0..buckets)
        .map(|b| {
            let chunk = &samples[b * per..(b + 1) * per];
            (chunk.iter().map(|s| s * s).sum::<f32>() / per as f32).sqrt()
        })
        .collect();
    let ceiling = rms.iter().copied().fold(0.0f32, f32::max);
    if ceiling <= 0.0 {
        return vec![0.0; buckets];
    }
    for v in &mut rms {
        *v /= ceiling;
    }
    rms
}

// -- paragraphs -------------------------------------------------------------

fn ends_sentence(text: &str) -> bool {
    let t = text.trim_end();
    let mut chars = t.chars().rev();
    // Allow one closing quote or bracket after the terminator.
    let last = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    let candidate = if matches!(last, '\'' | '"' | '”' | '’' | ')') {
        chars.next()
    } else {
        Some(last)
    };
    matches!(candidate, Some('.') | Some('!') | Some('?') | Some('…'))
}

/// Group segments into readable paragraphs. Same three rules as the sidecar:
/// a real pause after a completed thought, a long-enough run ending on a
/// sentence, or a hard cut for a monologue that never breaks cleanly.
fn build_paragraphs(segments: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut buf: Vec<&Value> = Vec::new();
    let mut chars = 0usize;

    let flush = |buf: &Vec<&Value>| -> Value {
        let text = buf
            .iter()
            .map(|s| s["text"].as_str().unwrap_or("").trim())
            .collect::<Vec<_>>()
            .join(" ");
        let words: Vec<Value> = buf
            .iter()
            .filter_map(|s| s["words"].as_array())
            .flat_map(|w| w.iter().cloned())
            .collect();
        json!({
            "start": buf.first().map(|s| s["start"].clone()).unwrap_or(json!(0.0)),
            "end":   buf.last().map(|s| s["end"].clone()).unwrap_or(json!(0.0)),
            "text":  text,
            "words": words,
        })
    };

    for (i, seg) in segments.iter().enumerate() {
        buf.push(seg);
        chars += seg["text"].as_str().unwrap_or("").len() + 1;

        if i + 1 >= segments.len() {
            break;
        }
        let gap = segments[i + 1]["start"].as_f64().unwrap_or(0.0) - seg["end"].as_f64().unwrap_or(0.0);
        let sentence = ends_sentence(seg["text"].as_str().unwrap_or(""));

        let should_break = (gap >= PARAGRAPH_GAP_SEC && sentence && chars >= 180)
            || (chars >= PARAGRAPH_SOFT_CHARS && sentence)
            || chars >= PARAGRAPH_HARD_CHARS;

        if should_break {
            out.push(flush(&buf));
            buf.clear();
            chars = 0;
        }
    }
    if !buf.is_empty() {
        out.push(flush(&buf));
    }
    out
}

// -- transcription ----------------------------------------------------------

/// What the engine did to produce one transcript.
///
/// Carried as a struct rather than two more positional arguments on an
/// `insert_transcript` that already takes eleven, and kept beside the JSON it is
/// read from so the two can't drift apart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// `"small"` / `"medium"`, or empty when the result predates this record.
    pub model: String,
    /// Milliseconds spent decoding. Zero means "not measured", never "instant".
    pub millis: i64,
}

impl Run {
    /// Read the engine's own record back out of a transcription result.
    ///
    /// Tolerant by design: a result that carries neither field — an old job
    /// replayed, or a future path that doesn't transcribe — yields the default,
    /// and Insights treats that as an unmeasured note rather than a zero.
    pub fn from_result(v: &Value) -> Self {
        Run {
            model: v
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string(),
            millis: v.get("transcribe_ms").and_then(|m| m.as_i64()).unwrap_or(0),
        }
    }

    /// Whether there is anything worth writing to the row.
    pub fn measured(&self) -> bool {
        !self.model.is_empty() && self.millis > 0
    }
}

/// Transcribe a media file. `report(stage, progress)` mirrors the sidecar's
/// progress contract so the existing UI keeps working unchanged.
pub fn transcribe(
    app: &tauri::AppHandle,
    path: &str,
    mut report: impl FnMut(&str, f64),
) -> Result<Value, String> {
    use tauri::Manager;

    report("Reading audio", 0.05);
    let samples = decode_mono_16k(Path::new(path))?;
    let duration = samples.len() as f64 / SAMPLE_RATE as f64;
    let peaks = compute_peaks(&samples);

    let state = app.state::<EngineState>();
    let wanted = auto_model();
    let lessons = Lessons::from_app(app);

    // Only speech reaches the decoder — see [`Speech`]. Asked before the model is
    // even loaded: a recording with nothing said in it has nothing to decode, and
    // loading half a gigabyte of weights to find that out would be the slow way
    // to arrive at an empty string.
    let heard = find_speech(app, &state, &samples);
    if matches!(heard, Detected::Silent) {
        report("Done", 1.0);
        return Ok(json!({
            "duration": duration,
            "language": lessons.languages.first().copied().unwrap_or("en"),
            "peaks": peaks,
            "segments": [],
            "paragraphs": [],
            "text": "",
            "model": wanted.label(),
            "transcribe_ms": 0,
        }));
    }

    report("Loading model", 0.12);
    let mut guard = state.inner.lock().unwrap();
    ensure_loaded(app, &mut guard, wanted)?;
    state.touch();
    let loaded = guard.as_ref().expect("model just loaded");

    report("Transcribing", 0.2);
    // Which weights actually ran, which is not always `wanted`: `ensure_loaded`
    // keeps an already-resident model rather than paying a reload to honour a
    // preference that changed since.
    let ran = loaded.size;

    let mut st = loaded
        .ctx
        .create_state()
        .map_err(|e| format!("could not start the transcriber: {e}"))?;

    // Hand the model back before decoding, so this is not the only thing in the
    // app that can transcribe for the next twenty minutes.
    //
    // The lock used to be held all the way through `full`, which made every
    // transcription in the app strictly one at a time. That is invisible for
    // dictation — a sentence takes under a second — and unbearable for a
    // meeting: an hour-long call transcribes both of its sides for tens of
    // minutes, and for all of that time the dictation key did nothing, an
    // uploaded file sat in the queue, and nothing said why.
    //
    // The weights are not copied to make this work. `create_state` is
    // whisper.cpp's own answer to the same question: one set of weights, one
    // set of KV caches and compute buffers per decode. On `medium` that is
    // 539 MB held once against 530 MB per concurrent decode — so two at a time
    // costs about 1.1 GB rather than the 2.1 GB a second loaded model would,
    // and the second one is given back the moment it finishes. Proven, on this
    // machine and under Metal, by `two_transcriptions_can_share_one_model`:
    // both clips come back byte for byte what they decode to on their own.
    //
    // A live state holds an `Arc` on the context, so the model cannot be freed
    // under it. That matters in two places now that the lock is released early:
    // the reaper may clear the slot, and another thread may swap it for a
    // different size. Both leave this decode's weights alive until it is done.
    //
    // `running` is what tells the reaper this is work rather than idleness.
    drop(guard);
    let _busy = InFlight::enter(&state.running);

    // Time the decode alone. Reading the file, loading weights and building
    // paragraphs all vary with things that have nothing to do with the model —
    // a cold load would make the same audio look three times slower on the
    // first note of the day — and it is the model that Insights is comparing.
    let began = std::time::Instant::now();
    let (segments, language) = decode_with(&mut st, &samples, &heard, &lessons, &mut |i, n| {
        report("Transcribing", 0.2 + 0.75 * (i as f64 / n as f64));
    })?;
    let elapsed_ms = began.elapsed().as_millis().min(i64::MAX as u128) as i64;
    // Again on the way out: a long file can transcribe for minutes, and dating
    // the model's last use from when the job *started* would make an hour-long
    // recording look idle the moment it finished. The slot's lock is long gone
    // by here; this touches the clock, which has its own.
    state.touch();

    let paragraphs = build_paragraphs(&segments);
    let text = paragraphs
        .iter()
        .map(|p| p["text"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n\n");

    report("Done", 1.0);
    Ok(json!({
        "duration": duration,
        "language": language,
        "peaks": peaks,
        "segments": segments,
        "paragraphs": paragraphs,
        "text": text,
        "model": ran.label(),
        "transcribe_ms": elapsed_ms,
    }))
}

fn num_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(2).max(1))
        .unwrap_or(4)
}

/// How every finished transcript is decoded.
///
/// A named thing rather than a block inside [`transcribe`], because one of the
/// lines below is the difference between an hour-long meeting and a blank page,
/// and a setting that important should be somewhere it can be pointed at and
/// tested rather than buried in the middle of a long function.
fn decoding() -> FullParams<'static, 'static> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_translate(false);

    // Decode every window on its own, with no memory of the last one.
    //
    // Whisper works in thirty-second windows and, by default, prepends what it
    // decoded in one window to the next as a prompt. On a short dictation that
    // costs nothing — there is only ever one window — and on a long recording
    // it was the single worst thing in this file.
    //
    // The prompt is a suggestion the model takes seriously, so a bad window
    // does not stay one window wide. Once a stretch decodes as nothing, the
    // prompt says "nothing was said here", the next window agrees more easily,
    // and the agreement compounds. It cannot recover on its own, because the
    // evidence that would break the loop is the thing being suppressed.
    //
    // Measured on a fifty-one minute call whose audio is clean end to end
    // (RMS 0.069 at the model's input, no clipping, no dropouts), decoded by
    // the same weights with the same sampler on the same threads:
    //
    //     prompt carried:  0 words — "[Silence]" for all fifty-one minutes
    //     prompt dropped:  ~6,300 words, spread evenly across every minute
    //
    // The zero is not a rounding of a bad result. That call opened with five
    // minutes of nobody speaking and a mouse being clicked; the model correctly
    // called it non-speech, and then never stopped calling it that. The two
    // people said hello at 2:55 and were never heard from again. The same
    // recording split into its two sides — which is what a meeting saves —
    // degenerated the same way, into "Okay." and then "Morning." repeated to
    // the end of the hour.
    //
    // It has to be this setting, and this one is easy to get wrong: `no_context`
    // sounds like the answer and is not. That flag clears the carry-over
    // *between calls*, once, on the way in; the prompt is then rebuilt window by
    // window inside the same call, which is where the damage happens. Setting it
    // and nothing else changes the result by zero words — measured, not assumed.
    // What actually severs the chain is capping how many past tokens may be
    // taken at zero.
    //
    // What it costs: a name or a piece of punctuation can now be spelled two
    // ways either side of a thirty-second boundary, because nothing carries
    // across. That is a real loss, and it is nowhere near the trade — a
    // transcript with a seam in it is still a transcript.
    params.set_n_max_text_ctx(0);
    // Belt and braces, for the day this runs on a state that has decoded
    // something before: nothing from a previous recording either.
    params.set_no_context(true);

    // Per-token times, which drive the word-by-word follow-along in the reading
    // view.
    params.set_token_timestamps(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // Leave a couple of cores for the UI and the rest of the machine.
    params.set_n_threads((num_threads() as i32).max(1));
    params
}

/// Peaks for a file we're not transcribing — used to backfill the waveform on
/// transcripts saved before it existed.
pub fn peaks_for(path: &str) -> Result<Vec<f32>, String> {
    Ok(compute_peaks(&decode_mono_16k(Path::new(path))?))
}

/// Transcribe a file arriving from outside the window — a Discord voice note, a
/// Slack clip, a globe-key dictation — mirroring progress into the sidebar.
///
/// Same signature as the sidecar bridge it replaces, minus the port, so the
/// ingest paths swapped over without restructuring.
pub fn transcribe_ingest(
    app: &tauri::AppHandle,
    path: &str,
    title: &str,
    source: &'static str,
) -> Result<Value, String> {
    use tauri::Emitter;

    // Voices, in parallel with the words. Both questions are asked of the same
    // audio and neither needs the other's answer, so the only reason this used
    // to happen afterwards was the order it was written in. See
    // `diarize::start_early` — it is silent, it does not download, and it stops
    // on anything too short to hold a conversation.
    crate::diarize::start_early(app, path);

    transcribe(app, path, |stage, progress| {
        let _ = app.emit(
            "ingest-progress",
            crate::sidecar::IngestProgress {
                title: title.to_string(),
                stage: stage.to_string(),
                progress,
                source,
            },
        );
    })
}

// -- commands ---------------------------------------------------------------

/// Kick off a transcription and stream progress back as `transcribe-progress`
/// events.
///
/// The shape mirrors the sidecar's old `JobState` exactly — same fields, same
/// stages, same 0..1 progress — so the window's job UI carried over untouched.
/// Transcription is CPU/GPU-bound and takes real seconds, so it runs on its own
/// thread; blocking the command would freeze the webview.
#[tauri::command]
pub fn start_transcription(app: tauri::AppHandle, path: String) -> String {
    use tauri::Emitter;

    let id = format!("{:x}", crate::now_ms());
    let job = id.clone();
    let src = path.clone();

    // The dropped-file path, and the one this matters most on: a file brought in
    // is whole from the first moment, and is the kind of recording that has more
    // than one person in it. See `diarize::start_early`.
    crate::diarize::start_early(&app, &path);

    std::thread::spawn(move || {
        let emit = |status: &str, stage: &str, progress: f64, error: Option<String>, result: Option<Value>| {
            let _ = app.emit(
                "transcribe-progress",
                json!({
                    "id": job,
                    "path": src,
                    "status": status,
                    "stage": stage,
                    "progress": progress,
                    "error": error,
                    "result": result,
                }),
            );
        };

        emit("running", "Starting", 0.0, None, None);
        let handle = app.clone();
        let jid = job.clone();
        let spath = src.clone();
        let result = transcribe(&handle, &spath, |stage, progress| {
            let _ = handle.emit(
                "transcribe-progress",
                json!({
                    "id": jid,
                    "path": spath,
                    "status": "running",
                    "stage": stage,
                    "progress": progress,
                    "error": Value::Null,
                    "result": Value::Null,
                }),
            );
        });

        match result {
            Ok(value) => emit("done", "Done", 1.0, None, Some(value)),
            Err(e) => emit("error", "Failed", 0.0, Some(e), None),
        }
    });

    id
}

/// Waveform peaks for a file, for transcripts saved before the waveform existed.
#[tauri::command]
pub fn transcribe_peaks(path: String) -> Result<Vec<f32>, String> {
    peaks_for(&path)
}

/// Which model this machine will use, and whether it's resident right now.
#[tauri::command]
pub fn engine_status(state: tauri::State<EngineState>) -> Value {
    json!({
        "model": auto_model().label(),
        "loaded": state.loaded_size().map(|s| s.label()),
    })
}

/// Hand the model's memory back. Called when the app goes idle.
#[tauri::command]
pub fn engine_unload(state: tauri::State<EngineState>) {
    state.unload();
}

#[cfg(test)]
mod run_tests {
    use super::Run;
    use serde_json::json;

    /// The shape `transcribe` actually emits, read back the way `insert_transcript`
    /// reads it. These two live in different files; this is what pins them
    /// together.
    #[test]
    fn reads_what_the_engine_writes() {
        let result = json!({
            "duration": 12.5,
            "text": "hello",
            "model": "medium",
            "transcribe_ms": 4820,
        });
        let run = Run::from_result(&result);
        assert_eq!(run.model, "medium");
        assert_eq!(run.millis, 4820);
        assert!(run.measured());
    }

    /// A result from before the engine kept this record. The row must keep its
    /// empty columns rather than claim a zero-millisecond transcription, which
    /// would divide into an infinite speed in Insights.
    #[test]
    fn an_older_result_is_not_a_measurement() {
        let run = Run::from_result(&json!({ "duration": 12.5, "text": "hello" }));
        assert_eq!(run, Run::default());
        assert!(!run.measured());
    }

    /// Half a record is not a record: a model with no timing still can't be
    /// divided, so it must not reach the speed calculation.
    #[test]
    fn a_partial_record_is_rejected() {
        let no_time = Run::from_result(&json!({ "model": "small", "transcribe_ms": 0 }));
        assert!(!no_time.measured());
        let no_model = Run::from_result(&json!({ "transcribe_ms": 900 }));
        assert!(!no_model.measured());
    }

    /// Wrong types shouldn't panic an ingest — a note without a timing beats a
    /// note that failed to save.
    #[test]
    fn nonsense_falls_back_to_the_default() {
        let run = Run::from_result(&json!({ "model": 7, "transcribe_ms": "ages" }));
        assert_eq!(run, Run::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Value {
        json!({ "text": text, "start": start, "end": end })
    }

    /// The fixture that started this: English, then a Hindi sentence the
    /// English decode left with no words at all — even though Whisper stretched
    /// its last English word to the end of the recording.
    #[test]
    fn speech_after_the_last_word_is_unheard() {
        // A word every 0.3s up to "testing" at 2.1s, which Whisper then
        // stretched to the end of the recording — as it does for the last word.
        let mut words: Vec<Value> = (0..7).map(|i| word(" w", 0.1 + i as f64 * 0.3, 0.35 + i as f64 * 0.3)).collect();
        words.push(word(" testing", 2.1, 5.3));
        let segs = vec![json!({
            "start": 0.0, "end": 5.3,
            "text": "Okay, so the build is ready for testing.",
            "words": words,
        })];
        let gaps = unheard(&[(0.0, 5.34)], &segs);
        assert_eq!(gaps.len(), 1, "got {gaps:?}");
        // "testing" is capped at WORD_SPAN (2.1 + 0.8) and padded by UNHEARD_PAD,
        // so the Hindi that follows is unheard from 3.2s to the end of speech.
        assert!((gaps[0].0 - 3.2).abs() < 1e-9 && (gaps[0].1 - 5.34).abs() < 1e-9, "got {gaps:?}");
    }

    /// Speech every word accounts for is never re-examined — so an English-only
    /// recording costs nothing extra for having a second language declared.
    #[test]
    fn fully_worded_speech_has_nothing_unheard() {
        let words: Vec<Value> = (0..20).map(|i| word(" w", i as f64 * 0.3, i as f64 * 0.3 + 0.25)).collect();
        let segs = vec![json!({ "start": 0.0, "end": 6.0, "text": "twenty words", "words": words })];
        assert!(unheard(&[(0.0, 5.9)], &segs).is_empty());
    }

    /// Less than a second is not a sentence in another language.
    #[test]
    fn a_short_gap_is_not_asked_about() {
        let segs = vec![json!({
            "start": 0.0, "end": 1.1, "text": "hi there",
            "words": [word(" hi", 0.0, 0.5), word(" there", 0.6, 1.1)],
        })];
        assert!(unheard(&[(0.0, 1.9)], &segs).is_empty());
    }

    /// Two fragments of one sentence the main decode interrupted with a word it
    /// caught are one question, and together they are long enough to ask.
    #[test]
    fn nearby_unheard_fragments_are_asked_about_once() {
        let segs = vec![json!({ "start": 1.1, "end": 1.4, "text": "okay", "words": [word(" okay", 1.1, 1.4)] })];
        let gaps = unheard(&[(0.0, 2.4)], &segs);
        assert_eq!(gaps.len(), 1, "got {gaps:?}");
        assert!((gaps[0].0 - 0.0).abs() < 1e-9 && (gaps[0].1 - 2.4).abs() < 1e-9);
    }

    #[test]
    fn a_long_unheard_stretch_is_asked_about_in_windows() {
        let gaps = unheard(&[(0.0, 70.0)], &[]);
        assert_eq!(gaps.len(), 3);
        assert!(gaps.iter().all(|(a, b)| b - a <= 30.0));
    }

    /// A taught spelling reaches the text and the highlighted word.
    #[test]
    fn a_rule_rewrites_the_text_and_the_word() {
        let segs = vec![json!({
            "start": 0.0, "end": 2.0, "text": "Send it to Sean.",
            "words": [word(" Send", 0.0, 0.3), word(" Sean", 1.0, 1.4), word(".", 1.4, 1.5)],
        })];
        let out = taught(segs, &[], &[], &[("sean".into(), "Shaun".into())]);
        assert_eq!(out[0]["text"], "Send it to Shaun.");
        assert_eq!(out[0]["words"][1]["text"], " Shaun");
    }

    /// A taught name, heard in pieces, is written the way it was taught — and a
    /// lone token of it is recased where the reading view highlights it.
    #[test]
    fn a_taught_spelling_reaches_the_text_and_the_word() {
        let segs = vec![json!({
            "start": 0.0, "end": 3.0, "text": "I rendered it in hyper frames with claude.",
            "words": [word(" hyper", 1.0, 1.3), word(" frames", 1.3, 1.6), word(" claude", 2.2, 2.6)],
        })];
        let out = taught(segs, &["HyperFrames".into(), "Claude".into()], &[], &[]);
        assert_eq!(out[0]["text"], "I rendered it in HyperFrames with Claude.");
        assert_eq!(out[0]["words"][2]["text"], " Claude");
    }

    #[test]
    fn a_rewrite_back_is_only_as_confident_as_its_words() {
        let sure = vec![json!({ "text": "लेकिन मुझे लगता है", "confidence": 0.8 })];
        let unsure = vec![json!({ "text": "लेकिन मुझे", "confidence": 0.2 })];
        assert!(confident(&sure));
        assert!(!confident(&unsure));
        assert!(!confident(&[json!({ "text": "हाँ", "confidence": 0.9 })]), "one word is not a sentence");
    }

    #[test]
    fn language_codes_come_back_static_or_not_at_all() {
        assert_eq!(static_code("hi"), Some("hi"));
        assert_eq!(static_code("klingon"), None);
    }

    /// A sentence spoken over detected speech stays.
    #[test]
    fn a_sentence_over_speech_is_kept() {
        assert!(near_speech(4.0, 6.0, &[(3.5, 7.0)]));
    }

    /// A sentence sitting in a long stretch the detector heard nobody in goes.
    #[test]
    fn a_sentence_in_a_long_silence_is_dropped() {
        assert!(!near_speech(20.0, 22.0, &[(3.0, 6.0), (40.0, 44.0)]));
    }

    /// Whisper's edges move; a real sentence a beat past the detector's is kept.
    #[test]
    fn a_sentence_just_past_the_edge_of_speech_is_kept() {
        assert!(near_speech(6.0 + NEAR_SPEECH - 0.1, 9.0, &[(3.0, 6.0)]));
    }

    /// Without the detector's model nothing is judged, which is how it was.
    #[test]
    fn nothing_is_judged_without_a_detector() {
        let segs = vec![json!({ "start": 50.0, "end": 52.0, "text": "Thanks for watching!" })];
        assert_eq!(judged(segs.clone(), &Detected::Unchecked, &[]), segs);
    }

    /// The case the detector is here for: a real sentence, then room tone that
    /// Whisper narrated as the end of a video.
    #[test]
    fn an_invented_sentence_over_room_tone_is_removed() {
        let segs = vec![
            json!({ "start": 0.5, "end": 3.0, "text": "Book the room for Thursday." }),
            json!({ "start": 18.0, "end": 20.0, "text": "Thanks for watching!" }),
        ];
        // Speech at 0.2 for the first 3.2s, room tone at 0.002 for the rest.
        let mut samples = vec![0.2_f32; (3.2 * SAMPLE_RATE as f64) as usize];
        samples.extend(vec![0.002_f32; (18.0 * SAMPLE_RATE as f64) as usize]);
        let kept = judged(segs, &Detected::Speech(vec![(0.3, 3.2)]), &samples);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["text"], "Book the room for Thursday.");
    }

    /// The meeting that forced the loudness rule: a sentence the detector did not
    /// call speech, said as loud as the rest, is kept — while one over silence goes.
    #[test]
    fn a_loud_sentence_the_detector_missed_is_kept() {
        let rate = SAMPLE_RATE as f64;
        let mut samples = vec![0.2_f32; (3.0 * rate) as usize]; // detected speech
        samples.extend(vec![0.0_f32; (17.0 * rate) as usize]); // silence
        samples.extend(vec![0.18_f32; (3.0 * rate) as usize]); // missed speech
        let segs = vec![
            json!({ "start": 0.5, "end": 2.5, "text": "We should start." }),
            json!({ "start": 10.0, "end": 11.0, "text": "you" }),
            json!({ "start": 20.5, "end": 22.5, "text": "I can hear you now." }),
        ];
        let kept: Vec<String> = judged(segs, &Detected::Speech(vec![(0.0, 3.0)]), &samples)
            .iter()
            .map(|s| s["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kept, vec!["We should start.", "I can hear you now."]);
    }

    /// A mixed recording: the far side is loud, and a nearer sentence the detector
    /// missed is quieter than half of it but still plainly audible. It stays.
    #[test]
    fn an_audible_sentence_is_kept_beside_a_loud_far_side() {
        let rate = SAMPLE_RATE as f64;
        let mut samples = vec![0.3_f32; (5.0 * rate) as usize]; // loud far side, detected
        samples.extend(vec![0.0_f32; (10.0 * rate) as usize]);
        samples.extend(vec![0.036_f32; (4.0 * rate) as usize]); // quieter, missed, real
        let segs = vec![json!({ "start": 15.5, "end": 18.5, "text": "I can hear you now." })];
        assert_eq!(judged(segs, &Detected::Speech(vec![(0.0, 5.0)]), &samples).len(), 1);
    }

    #[test]
    fn sentence_endings() {
        assert!(ends_sentence("That's done."));
        assert!(ends_sentence("Really?"));
        assert!(ends_sentence("He said \"go.\""));
        assert!(!ends_sentence("and then we"));
        assert!(!ends_sentence(""));
    }

    #[test]
    fn peaks_are_normalised() {
        let quiet: Vec<f32> = (0..48_000).map(|i| 0.001 * (i as f32 / 100.0).sin()).collect();
        let peaks = compute_peaks(&quiet);
        assert_eq!(peaks.len(), PEAK_BUCKETS);
        // A quiet recording must still fill the scrubber, not draw a flat line.
        assert!(peaks.iter().cloned().fold(0.0f32, f32::max) > 0.9);
        assert!(peaks.iter().all(|p| (0.0..=1.0).contains(p)));
    }

    /// The real thing: decode a media file and transcribe it, with no Python
    /// and no ffmpeg anywhere in the path.
    #[test]
    fn transcribes_real_audio() {
        let (Ok(audio), Ok(models)) = (
            std::env::var("TEST_AUDIO"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            eprintln!("skipping: set TEST_AUDIO and VOICEDUMPS_MODEL_DIR");
            return;
        };

        let samples = decode_mono_16k(Path::new(&audio)).expect("decode");
        let seconds = samples.len() as f64 / SAMPLE_RATE as f64;
        assert!(seconds > 0.1, "decoded {seconds}s");
        eprintln!("decoded {seconds:.1}s, {} samples", samples.len());

        let peaks = compute_peaks(&samples);
        assert!(!peaks.is_empty());
        eprintln!("peaks: {}", peaks.len());

        let model = Path::new(&models).join(ModelSize::Small.file_name());
        let ctx = WhisperContext::new_with_params(
            model.to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .expect("load model");

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(num_threads() as i32);

        let mut st = ctx.create_state().expect("state");
        st.full(params, &samples).expect("transcribe");

        let n = st.full_n_segments();
        assert!(n > 0, "no segments produced");
        let text: String = (0..n)
            .filter_map(|i| st.get_segment(i))
            .filter_map(|s| s.to_str_lossy().ok().map(|c| c.into_owned()))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("TRANSCRIPT: {}", text.trim());
        assert!(!text.trim().is_empty(), "empty transcript");
    }

    /// A long recording has to keep producing words all the way to the end.
    ///
    /// The failure this exists for does not look like a crash and does not look
    /// like a bad transcript. It looks like a *short* one: a fifty-one minute
    /// call saved as a thousand words, ending in "Morning." forty times. Every
    /// unit test in this file passed while that was happening, because nothing
    /// here decoded anything longer than a sentence.
    ///
    /// So this runs the real settings, from [`decoding`], over a real recording,
    /// and asserts the thing the bug broke: that the last few minutes carry as
    /// much speech as the first few. Needs weights and an hour of somebody's
    /// audio, neither of which belongs in a repository, so it is opt-in:
    ///
    /// ```text
    /// TEST_LONG_AUDIO=/path/to/an-hour.wav VOICEDUMPS_MODEL_DIR=/path/to/models \
    ///     cargo test --release -- --ignored --nocapture keeps_transcribing
    /// ```
    #[test]
    #[ignore = "needs the weights and a recording several minutes long"]
    fn a_long_recording_keeps_transcribing_to_the_end() {
        let (Ok(audio), Ok(models)) = (
            std::env::var("TEST_LONG_AUDIO"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            panic!("set TEST_LONG_AUDIO and VOICEDUMPS_MODEL_DIR");
        };

        let samples = decode_mono_16k(Path::new(&audio)).expect("decode");
        let seconds = samples.len() as f64 / SAMPLE_RATE as f64;
        // Ten minutes is roughly twenty windows, which is where the prompt
        // chain starts to matter. Anything shorter still has to come back with
        // words — that is the regression check for ordinary dictations — but
        // the shape assertions below are not asked of it, because on four
        // windows they would pass whatever happened.
        let long_enough = seconds > 600.0;

        let model = Path::new(&models).join(ModelSize::Medium.file_name());
        let ctx = WhisperContext::new_with_params(
            model.to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .expect("load model");
        let mut st = ctx.create_state().expect("state");
        st.full(decoding(), &samples).expect("transcribe");

        // What actually reached the model, and what it made of the opening.
        // Printed always, because when this test fails the first question is
        // which of the two was wrong — the audio or the decode — and rerunning
        // it costs several minutes.
        let energy = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        eprintln!("fed the model {:.0}s at RMS {energy:.4}", seconds);
        for i in 0..st.full_n_segments().min(8) {
            let Some(seg) = st.get_segment(i) else { continue };
            eprintln!(
                "  {:>7.1}s {:?}",
                seg.start_timestamp() as f64 / 100.0,
                seg.to_str_lossy().unwrap_or_default()
            );
        }

        // Words per minute of audio, in fifths — the shape of the decay, not
        // just its total. A run that dies halfway still passes a word count.
        let mut fifths = [0usize; 5];
        for i in 0..st.full_n_segments() {
            let Some(seg) = st.get_segment(i) else { continue };
            let text = seg.to_str_lossy().unwrap_or_default().into_owned();
            let text = text.trim();
            if text.is_empty() || is_non_speech(text) {
                continue;
            }
            let at = seg.start_timestamp() as f64 / 100.0;
            let fifth = ((at / seconds) * 5.0).floor().clamp(0.0, 4.0) as usize;
            fifths[fifth] += text.split_whitespace().count();
        }
        eprintln!("{:.0}s of audio, words per fifth: {fifths:?}", seconds);

        let total: usize = fifths.iter().sum();
        assert!(
            total as f64 / (seconds / 60.0) > 40.0,
            "{total} words in {:.0} minutes is not a transcript of a conversation",
            seconds / 60.0
        );
        if !long_enough {
            eprintln!("{seconds:.0}s is too short to say anything about decay");
            return;
        }
        // The specific shape of the bug: a healthy start, then nothing. A tenth
        // of the opening pace is far below any real quiet ending.
        let opening = fifths[0].max(1);
        for (n, words) in fifths.iter().enumerate().skip(1) {
            assert!(
                *words * 10 > opening,
                "the {} fifth of the recording produced {words} words against \
                 {opening} in the first — the decoder stopped hearing it",
                ["", "second", "third", "fourth", "last"][n]
            );
        }
    }

    /// Two decodes at once, on one set of weights, and both come back right.
    ///
    /// This is the claim [`transcribe`] now rests on. It hands the model back
    /// before decoding so that a dictation is not stuck behind an hour-long
    /// meeting, which is only sound if whisper.cpp really does allow several
    /// states to run against one context at the same time — and the app has
    /// never actually done that. `Refine` comes closest and always runs while
    /// nothing else is decoding.
    ///
    /// It is not enough for both to finish: a shared buffer would corrupt the
    /// results rather than crash, and two threads decoding two different clips
    /// would be very hard to tell from one decoding both. So each thread gets
    /// its own audio and has to return its own words — the same words a solo
    /// run of that clip returns, taken first, on the same weights.
    ///
    /// ```text
    /// TEST_AUDIO=/path/a.wav TEST_AUDIO_TWO=/path/b.wav \
    ///   VOICEDUMPS_MODEL_DIR=/path/to/models \
    ///   cargo test --release -- --ignored --nocapture two_transcriptions
    /// ```
    #[test]
    #[ignore = "needs the weights and two different recordings"]
    fn two_transcriptions_can_share_one_model() {
        let (Ok(first), Ok(second), Ok(models)) = (
            std::env::var("TEST_AUDIO"),
            std::env::var("TEST_AUDIO_TWO"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            panic!("set TEST_AUDIO, TEST_AUDIO_TWO and VOICEDUMPS_MODEL_DIR");
        };

        let clips: Vec<Vec<f32>> = [&first, &second]
            .iter()
            .map(|p| decode_mono_16k(Path::new(p)).expect("decode"))
            .collect();

        let model = Path::new(&models).join(ModelSize::Medium.file_name());
        let ctx = Arc::new(
            WhisperContext::new_with_params(
                model.to_string_lossy().as_ref(),
                WhisperContextParameters::default(),
            )
            .expect("load model"),
        );

        let read = |ctx: &WhisperContext, samples: &[f32]| -> String {
            let mut st = ctx.create_state().expect("state");
            st.full(decoding(), samples).expect("transcribe");
            (0..st.full_n_segments())
                .filter_map(|i| st.get_segment(i))
                .filter_map(|s| s.to_str_lossy().ok().map(|c| c.trim().to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        };

        // Alone first, so there is something to compare against that cannot
        // itself be wrong for the reason under test.
        let alone: Vec<String> = clips.iter().map(|c| read(&ctx, c)).collect();
        for (i, text) in alone.iter().enumerate() {
            assert!(!text.is_empty(), "clip {i} was empty even on its own");
        }

        let began = std::time::Instant::now();
        let together: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = clips
                .iter()
                .map(|clip| {
                    let ctx = Arc::clone(&ctx);
                    scope.spawn(move || read(&ctx, clip))
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread")).collect()
        });
        eprintln!("both together in {:?}", began.elapsed());

        for (i, (solo, shared)) in alone.iter().zip(&together).enumerate() {
            eprintln!("clip {i} alone:    {solo}");
            eprintln!("clip {i} together: {shared}");
            assert_eq!(
                solo, shared,
                "clip {i} decoded differently when it shared the model — the \
                 states are not independent and `transcribe` must go back to \
                 holding the lock across the decode"
            );
        }
    }

    fn said(lines: &[&str]) -> Vec<Value> {
        lines
            .iter()
            .enumerate()
            .map(|(i, t)| json!({ "start": i as f64, "end": i as f64 + 1.0, "text": t }))
            .collect()
    }

    fn spoken(segments: &[Value]) -> Vec<String> {
        segments
            .iter()
            .map(|s| s["text"].as_str().unwrap_or("").to_string())
            .collect()
    }

    /// The run that ended the fifty-one minute meeting.
    #[test]
    fn a_decoder_stuck_on_one_word_is_cut_short() {
        let stuck: Vec<&str> = std::iter::repeat_n("Morning.", 53).collect();
        let kept = collapse_repeats(said(&stuck));
        assert_eq!(spoken(&kept), ["Morning.", "Morning."]);
    }

    /// And the longer one, which is harder to spot because it reads like prose.
    #[test]
    fn a_whole_sentence_on_repeat_is_cut_too() {
        let mut lines = vec!["I'll explain the script so that you can understand."];
        lines.extend(std::iter::repeat_n(
            "So I'm going to change it to the center frame.",
            8,
        ));
        lines.push("And then as multiple people talk about it.");

        let kept = spoken(&collapse_repeats(said(&lines)));
        assert_eq!(kept.len(), 4, "{kept:?}");
        assert_eq!(kept.first().map(String::as_str), Some(lines[0]));
        assert_eq!(kept.last().map(String::as_str), Some("And then as multiple people talk about it."));
    }

    /// Saying a word twice is a thing people do, and it survives untouched.
    #[test]
    fn people_are_allowed_to_repeat_themselves() {
        let real = ["Yeah.", "Yeah.", "No, no — the other one.", "Okay.", "Okay."];
        let kept = collapse_repeats(said(&real));
        assert_eq!(spoken(&kept), real);
    }

    /// A run is a run whatever the model does with capitals and full stops.
    #[test]
    fn punctuation_does_not_hide_a_run() {
        let kept = collapse_repeats(said(&["Okay.", "okay", "Okay!", "Okay."]));
        assert_eq!(spoken(&kept), ["Okay.", "okay"]);
    }

    /// The same phrase said again later, with a conversation in between, is not
    /// a run — only consecutive segments count.
    #[test]
    fn a_phrase_that_comes_back_later_is_left_alone() {
        let real = ["Right.", "Right.", "So where were we?", "Right.", "Right."];
        assert_eq!(spoken(&collapse_repeats(said(&real))), real);
    }

    /// The markers that were reaching real transcripts.
    ///
    /// `[BLANK_AUDIO]` was pasted into four saved notes before this existed —
    /// whisper narrating silence, in the middle of somebody's message.
    #[test]
    fn non_speech_markers_are_dropped() {
        for s in [
            "[BLANK_AUDIO]",
            "[ Silence ]",
            "(upbeat music)",
            "*door creaks*",
            "[MUSIC PLAYING]",
        ] {
            assert!(is_non_speech(s), "{s:?} should be dropped");
        }
    }

    /// Real speech is never dropped, including speech that merely contains or
    /// begins with a bracket — the wrapper has to enclose the whole segment.
    #[test]
    fn speech_survives_the_filter() {
        for s in [
            "Ship the build tonight.",
            "(as I said) we shipped it",
            "the array is a[0] and b[1]",
            "I said (roughly) forty",
            "a",
            "",
        ] {
            assert!(!is_non_speech(s), "{s:?} should be kept");
        }
    }

    /// The idle policy itself, without needing a Tauri app or the weights.
    ///
    /// `start_idle_unload` needs an AppHandle, so what is checked here is the
    /// decision it makes — the timeout parse and the "has it been idle long
    /// enough" comparison — plus the guarantee that matters most: a touch
    /// resets the clock, so a model in active use is never collected.
    #[test]
    fn idle_policy() {
        use std::time::{Duration, Instant};

        // Default is five minutes; 0 means never.
        std::env::remove_var("VOICEDUMPS_IDLE_UNLOAD_SECS");
        assert_eq!(idle_timeout(), Some(Duration::from_secs(300)));
        std::env::set_var("VOICEDUMPS_IDLE_UNLOAD_SECS", "0");
        assert_eq!(idle_timeout(), None, "0 must disable the reaper entirely");
        std::env::set_var("VOICEDUMPS_IDLE_UNLOAD_SECS", "45");
        assert_eq!(idle_timeout(), Some(Duration::from_secs(45)));
        std::env::remove_var("VOICEDUMPS_IDLE_UNLOAD_SECS");

        let state = EngineState::default();
        // Never used: `last_use` is None, which the reaper treats as "idle for
        // at least the timeout" so a model loaded and then abandoned still goes.
        assert!(state.last_use.lock().unwrap().is_none());

        state.touch();
        let after_touch = state.last_use.lock().unwrap().expect("touched");
        assert!(
            after_touch.elapsed() < Duration::from_secs(1),
            "touch must reset the idle clock"
        );

        // The comparison the reaper makes, against a deliberately stale clock.
        let timeout = Duration::from_millis(50);
        *state.last_use.lock().unwrap() = Some(Instant::now() - Duration::from_secs(60));
        let idle_for = state.last_use.lock().unwrap().map(|t| t.elapsed()).unwrap();
        assert!(idle_for >= timeout, "a stale model must be collectable");

        state.touch();
        let fresh = state.last_use.lock().unwrap().map(|t| t.elapsed()).unwrap();
        assert!(fresh < timeout, "a just-used model must survive");
    }

    /// A decode in flight is not idleness, however stale the clock looks.
    ///
    /// The reaper used to get this for free: a transcription held the slot's
    /// lock the whole way through, so the collection simply blocked. It doesn't
    /// any more, and nothing touches the clock during the twenty minutes a long
    /// meeting spends inside `full()` — so on the old rule the model would be
    /// collected after five, and the next dictation would load a second copy of
    /// weights that are still in memory and still in use.
    #[test]
    fn a_decode_in_flight_is_not_idle() {
        use std::time::{Duration, Instant};

        let state = EngineState::default();
        // As stale as it ever gets: nothing has touched this in an hour.
        *state.last_use.lock().unwrap() = Some(Instant::now() - Duration::from_secs(3600));
        assert_eq!(state.running.load(Ordering::SeqCst), 0);

        {
            let _busy = InFlight::enter(&state.running);
            assert_eq!(state.running.load(Ordering::SeqCst), 1);
            {
                // Two at once — a dictation started while a meeting transcribes,
                // which is the whole point of the change. Both have to be
                // counted, or the first to finish would declare the model idle
                // while the second is still decoding with it.
                let _also = InFlight::enter(&state.running);
                assert_eq!(state.running.load(Ordering::SeqCst), 2);
            }
            assert_eq!(state.running.load(Ordering::SeqCst), 1);
        }

        assert_eq!(
            state.running.load(Ordering::SeqCst),
            0,
            "the count must come back down however the decode left"
        );
    }

    /// The reaper drops a real model from a background thread.
    ///
    /// This is the part that could actually break. A `WhisperContext` owns
    /// Metal buffers and residency sets, and the existing `unload` is only ever
    /// called from the run-event thread on quit. Freeing one from a worker
    /// thread instead is what the idle reaper does on every collection, so it
    /// is worth proving rather than assuming — a ggml assertion here would
    /// abort the process and read to the user as a random crash while the app
    /// sat untouched in the menu bar.
    ///
    /// Ignored by default: it needs the weights.
    #[test]
    #[ignore = "needs VOICEDUMPS_MODEL_DIR"]
    fn reaper_frees_a_live_model_off_thread() {
        let Ok(models) = std::env::var("VOICEDUMPS_MODEL_DIR") else {
            eprintln!("skipping: set VOICEDUMPS_MODEL_DIR");
            return;
        };
        // Measure whatever this machine would actually run, so the numbers
        // describe the real saving rather than the cheapest case.
        let want = auto_model();
        let file = match want {
            ModelSize::Medium => format!("{models}/ggml-medium-q5_0.bin"),
            ModelSize::Small => format!("{models}/ggml-small-q5_1.bin"),
        };
        println!("model: {}", want.label());

        let before = rss_mb();
        let state = std::sync::Arc::new(EngineState::default());
        *state.inner.lock().unwrap() = Some(Loaded {
            size: want,
            ctx: WhisperContext::new_with_params(&file, WhisperContextParameters::default())
                .expect("load"),
        });
        assert!(state.inner.lock().unwrap().is_some());
        let loaded = rss_mb();

        // Exactly what the reaper does, on exactly the kind of thread it does
        // it on.
        let s = state.clone();
        std::thread::spawn(move || {
            let mut guard = s.inner.lock().unwrap();
            *guard = None;
        })
        .join()
        .expect("the reaper thread must not panic or abort");

        assert!(
            state.inner.lock().unwrap().is_none(),
            "the model should be gone"
        );

        // The point of the whole feature: the memory has to come back, not just
        // the Rust value.
        let freed = rss_mb();
        println!(
            "rss  before {before:.0} MB -> loaded {loaded:.0} MB -> after collection {freed:.0} MB \
             (returned {:.0} MB of {:.0} MB)",
            loaded - freed,
            loaded - before
        );
        assert!(
            freed < before + (loaded - before) * 0.5,
            "collection must return most of the model's memory, not just drop the handle"
        );

        // And the slot is reusable afterwards — an idle collection must not
        // leave the engine permanently broken.
        *state.inner.lock().unwrap() = Some(Loaded {
            size: want,
            ctx: WhisperContext::new_with_params(&file, WhisperContextParameters::default())
                .expect("reload after collection"),
        });
        assert!(state.inner.lock().unwrap().is_some());
    }

    /// Quitting with a model loaded must not abort.
    ///
    /// It used to. `-[NSApplication terminate:]` calls `exit()`, which runs C
    /// static destructors but drops nothing Rust owns, so the `WhisperContext`
    /// in Tauri's managed state was still alive when ggml-metal tore itself
    /// down. Its teardown asserts `[rsets->data count] == 0` — every Metal
    /// residency set released — and a live context still holds them, so ggml
    /// called abort() and macOS reported a crash on an ordinary quit.
    ///
    /// This has to run in a child process, because the behaviour under test is
    /// what happens *during* process exit; there is no way to observe it from
    /// inside the process that is exiting.
    #[test]
    fn exits_cleanly_with_a_model_loaded() {
        let Ok(models) = std::env::var("VOICEDUMPS_MODEL_DIR") else {
            eprintln!("skipping: set VOICEDUMPS_MODEL_DIR");
            return;
        };

        if std::env::var("VD_EXIT_PROBE").is_ok() {
            let model = Path::new(&models).join(ModelSize::Small.file_name());
            let ctx = WhisperContext::new_with_params(
                model.to_string_lossy().as_ref(),
                WhisperContextParameters::default(),
            )
            .expect("load model");

            let state = EngineState::default();
            *state.inner.lock().unwrap() = Some(Loaded {
                size: ModelSize::Small,
                ctx,
            });

            // The two halves of quitting, in order: what lib.rs's RunEvent
            // handler does, then the exit AppKit performs regardless.
            eprintln!("{PROBE_READY}");
            state.unload();
            std::process::exit(0);
        }

        let out = std::process::Command::new(std::env::current_exe().expect("current exe"))
            // A substring filter, deliberately not `--exact`: an exact filter
            // has to spell out the full module path, and when it does not match
            // the child runs zero tests and exits 0 — a green light for work
            // never done. The PROBE_READY check below is the real guard.
            .args(["exits_cleanly_with_a_model_loaded", "--nocapture"])
            .env("VD_EXIT_PROBE", "1")
            .output()
            .expect("spawn exit probe");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(PROBE_READY),
            "the probe never loaded a model, so this proves nothing:\n{stderr}"
        );
        assert!(
            out.status.success(),
            "exiting with a model loaded did not exit cleanly ({}):\n{stderr}",
            out.status,
        );
    }

    /// Printed by the child once a model is loaded, so the parent can tell a
    /// real pass from a probe that never ran.
    const PROBE_READY: &str = "VD_EXIT_PROBE: model loaded";

    /// Resident size of this process, in MB.
    fn rss_mb() -> f64 {
        let pid = std::process::id().to_string();
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
            .map(|kb| kb / 1024.0)
            .unwrap_or(0.0)
    }

    /// What the numbers in the README are made of.
    ///
    /// Ignored by default: it is a measurement, not an assertion, and it needs a
    /// real audio file and the weights on disk. Run it with
    ///
    /// ```text
    /// scripts/bench.sh
    /// ```
    ///
    /// The split matters more than the total. Dictation warms the model while you
    /// are still speaking (see `dictation::start`), so the load column is paid
    /// during speech, not after it — the latency a user actually feels on key
    /// release is decode + transcribe. Both are reported separately so neither
    /// can be quietly folded into a flattering single figure.
    /// Replay real recordings through a decoder and write what came out.
    ///
    /// The guard on every change to how audio becomes text: the same recordings,
    /// through the pipeline as shipped and through the change, compared word for
    /// word. A change that fixes an invented sentence and quietly drops the first
    /// word of every dictation is not a fix, and only a replay says which one
    /// happened.
    ///
    ///     REPLAY_LIST=corpus.tsv REPLAY_OUT=out.jsonl VOICEDUMPS_MODEL_DIR=models \
    ///       cargo test --release --no-default-features replay_corpus -- --ignored --nocapture
    ///
    /// `REPLAY_LIST` is tab-separated with the audio path in the last column.
    #[test]
    #[ignore = "readout: needs REPLAY_LIST, REPLAY_OUT and models"]
    fn replay_corpus() {
        use std::io::Write;
        let (Ok(list), Ok(out), Ok(models)) = (
            std::env::var("REPLAY_LIST"),
            std::env::var("REPLAY_OUT"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            eprintln!("skipping: set REPLAY_LIST, REPLAY_OUT and VOICEDUMPS_MODEL_DIR");
            return;
        };
        let ctx = WhisperContext::new_with_params(
            Path::new(&models).join(auto_model().file_name()).to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .expect("load model");
        let mut sink = std::fs::File::create(&out).expect("out");
        let gate = std::env::var("REPLAY_MODE").as_deref() == Ok("gate");
        let pipeline = std::env::var("REPLAY_MODE").as_deref() == Ok("pipeline");
        let lessons = Lessons {
            languages: std::env::var("REPLAY_LANGS")
                .unwrap_or_else(|_| "en".into())
                .split(',')
                .filter_map(|c| static_code(c.trim()))
                .collect(),
            terms: std::env::var("REPLAY_TERMS")
                .unwrap_or_default()
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
            names: std::env::var("REPLAY_NAMES")
                .unwrap_or_default()
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
            rules: Vec::new(),
        };
        let mut vad = (gate || pipeline).then(|| {
            whisper_rs::WhisperVadContext::new(
                &Path::new(&models).join(VAD_FILE).to_string_lossy(),
                whisper_rs::WhisperVadContextParams::default(),
            )
            .expect("load the speech detector")
        });

        for line in std::fs::read_to_string(&list).expect("list").lines() {
            let Some(path) = line.split('\t').next_back() else { continue };
            let id = line.split('\t').next().unwrap_or("");
            let Ok(samples) = decode_mono_16k(Path::new(path)) else { continue };

            if pipeline {
                let vad = vad.as_mut().expect("detector");
                let began = std::time::Instant::now();
                let spans = speech_spans(vad, &samples, VAD_THRESHOLD).expect("detect");
                let heard = if spans.is_empty() { Detected::Silent } else { Detected::Speech(spans) };
                let segs = if matches!(heard, Detected::Silent) {
                    Vec::new()
                } else {
                    let mut st = ctx.create_state().expect("state");
                    decode_with(&mut st, &samples, &heard, &lessons, &mut |_, _| {})
                        .expect("decode")
                        .0
                };
                let ms = began.elapsed().as_millis();
                let text = segs.iter().map(|s| s["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join(" ");
                let words: Vec<Value> = segs
                    .iter()
                    .flat_map(|s| s["words"].as_array().cloned().unwrap_or_default())
                    .map(|w| json!([w["text"], w["start"], w["end"]]))
                    .collect();
                let languages: Vec<&str> = segs.iter().filter_map(|s| s["language"].as_str()).collect();
                writeln!(sink, "{}", json!({ "id": id, "ms": ms, "text": text, "words": words, "languages": languages })).unwrap();
                println!("{id} {ms}ms {} words", text.split_whitespace().count());
                continue;
            }

            if let Some(vad) = vad.as_mut() {
                let mut st = ctx.create_state().expect("state");
                let began = std::time::Instant::now();
                st.full(decoding(), &samples).expect("transcribe");
                let ms = began.elapsed().as_millis();
                let segments = collapse_repeats(collect(&st, 0.0, "en", &mut |_, _| {}));

                // Scored once; every threshold is then a cheap pass over the
                // same probabilities, so the sweep compares like with like.
                let v0 = std::time::Instant::now();
                vad.detect_speech(&samples).expect("detect");
                let vad_ms = v0.elapsed().as_millis();
                let mut sweep = serde_json::Map::new();
                for threshold in [0.5_f32, 0.35, 0.2, 0.1] {
                    let mut params = whisper_rs::WhisperVadParams::new();
                    params.set_threshold(threshold);
                    let spans: Vec<(f64, f64)> = vad
                        .segments_from_probabilities(params)
                        .expect("spans")
                        .map(|seg| (seg.start as f64 / 100.0, seg.end as f64 / 100.0))
                        .collect();
                    let dropped: Vec<&str> = segments
                        .iter()
                        .filter(|seg| {
                            spans.is_empty()
                                || !near_speech(
                                    seg["start"].as_f64().unwrap_or(0.0),
                                    seg["end"].as_f64().unwrap_or(0.0),
                                    &spans,
                                )
                        })
                        .map(|seg| seg["text"].as_str().unwrap_or(""))
                        .collect();
                    if !dropped.is_empty() {
                        println!(
                            "{id} @{threshold}: {}{dropped:?}",
                            if spans.is_empty() { "SILENT, would drop " } else { "would drop " }
                        );
                    }
                    let speech_s: f64 = spans.iter().map(|(a, b)| b - a).sum();
                    sweep.insert(
                        threshold.to_string(),
                        json!({ "silent": spans.is_empty(), "speech_s": speech_s, "dropped": dropped }),
                    );
                }
                let text = segments.iter().map(|seg| seg["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join(" ");
                writeln!(sink, "{}", json!({ "id": id, "ms": ms, "vad_ms": vad_ms, "text": text, "sweep": sweep })).unwrap();
                continue;
            }

            let mut st = ctx.create_state().expect("state");
            let began = std::time::Instant::now();
            st.full(decoding(), &samples).expect("transcribe");
            let ms = began.elapsed().as_millis();

            let mut segments = Vec::new();
            for i in 0..st.full_n_segments() {
                let Some(seg) = st.get_segment(i) else { continue };
                let text = seg.to_str_lossy().unwrap_or_default().trim().to_string();
                if text.is_empty() || is_non_speech(&text) {
                    continue;
                }
                let mut words = Vec::new();
                for t in 0..seg.n_tokens() {
                    let Some(tok) = seg.get_token(t) else { continue };
                    let Ok(raw) = tok.to_str_lossy() else { continue };
                    if raw.starts_with("[_") || raw.starts_with("<|") || raw.trim().is_empty() {
                        continue;
                    }
                    let d = tok.token_data();
                    words.push(json!([raw, d.t0 as f64 / 100.0, d.t1 as f64 / 100.0]));
                }
                segments.push(json!({ "text": text, "words": words }));
            }
            let kept = collapse_repeats(segments);
            let text = kept.iter().map(|s| s["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join(" ");
            let words: Vec<Value> = kept.iter().flat_map(|s| s["words"].as_array().cloned().unwrap_or_default()).collect();
            writeln!(sink, "{}", json!({ "id": id, "ms": ms, "text": text, "words": words })).unwrap();
            println!("{id} {ms}ms {} words", text.split_whitespace().count());
        }
    }

    /// The "day two" receipts: what the shipped decoder does with the recordings
    /// a dictation app is judged on, beside what each candidate fix would do.
    ///
    /// Not an assertion — a readout. Every claim about accents, code-switching,
    /// invented words and names should be something anyone can re-run against a
    /// folder of their own audio, not a sentence somebody wrote.
    ///
    ///     DAY_TWO_DIR=... VOICEDUMPS_MODEL_DIR=models cargo test --release \
    ///       --no-default-features day_two_receipts -- --ignored --nocapture
    #[test]
    #[ignore = "readout: needs DAY_TWO_DIR and models"]
    fn day_two_receipts() {
        let (Ok(dir), Ok(models)) = (
            std::env::var("DAY_TWO_DIR"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            eprintln!("skipping: set DAY_TWO_DIR and VOICEDUMPS_MODEL_DIR");
            return;
        };
        let size = auto_model();
        let ctx = WhisperContext::new_with_params(
            Path::new(&models).join(size.file_name()).to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .expect("load model");

        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .collect();
        files.sort();

        let prompt = std::env::var("DAY_TWO_PROMPT").unwrap_or_default();
        println!("\n--- day two ({}) ---", size.label());
        for file in files {
            let samples = decode_mono_16k(&file).expect("decode");
            println!("\n{}", file.file_name().unwrap().to_string_lossy());

            let variants: [(&str, Box<dyn Fn(&mut FullParams)>); 3] = [
                ("shipped (en)", Box::new(|_: &mut FullParams| {})),
                ("auto language", Box::new(|p: &mut FullParams| p.set_language(Some("auto")))),
                ("+ vocabulary", Box::new(|p: &mut FullParams| p.set_initial_prompt(&prompt))),
            ];
            for (name, tweak) in variants.iter() {
                if *name == "+ vocabulary" && prompt.is_empty() {
                    continue;
                }
                let mut params = decoding();
                tweak(&mut params);
                let mut st = ctx.create_state().expect("state");
                st.full(params, &samples).expect("transcribe");
                let lang = whisper_rs::get_lang_str(st.full_lang_id_from_state()).unwrap_or("?");

                let mut segs = Vec::new();
                for i in 0..st.full_n_segments() {
                    let Some(seg) = st.get_segment(i) else { continue };
                    let t = seg.to_str_lossy().unwrap_or_default().trim().to_string();
                    if t.is_empty() || is_non_speech(&t) {
                        continue;
                    }
                    segs.push(json!({ "text": t }));
                }
                let kept: Vec<String> = collapse_repeats(segs)
                    .iter()
                    .map(|s| s["text"].as_str().unwrap_or("").to_string())
                    .collect();
                let text = kept.join(" ");
                println!(
                    "  {name:<14} [{lang}] {}",
                    if text.is_empty() { "(nothing — correct for silence)".to_string() } else { format!("\"{text}\"") }
                );
            }
        }
        println!("--- end ---\n");
    }

    /// Where the detector hears speech in the fixtures, at each threshold.
    ///
    /// The detector alone, no Whisper: the question is only whether silence and
    /// room noise stay silent as the bar comes down.
    #[test]
    #[ignore = "readout: needs DAY_TWO_DIR and models"]
    fn vad_fixture_spans() {
        let (Ok(dir), Ok(models)) = (
            std::env::var("DAY_TWO_DIR"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            eprintln!("skipping: set DAY_TWO_DIR and VOICEDUMPS_MODEL_DIR");
            return;
        };
        let mut vad = whisper_rs::WhisperVadContext::new(
            &Path::new(&models).join(VAD_FILE).to_string_lossy(),
            whisper_rs::WhisperVadContextParams::default(),
        )
        .expect("load the speech detector");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .collect();
        files.sort();
        println!("\n--- detector spans ---");
        for file in files {
            let samples = decode_mono_16k(&file).expect("decode");
            vad.detect_speech(&samples).expect("detect");
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            let mut line = format!("  {name:<28}");
            for threshold in [0.5_f32, 0.35, 0.2, 0.1] {
                let mut params = whisper_rs::WhisperVadParams::new();
                params.set_threshold(threshold);
                let speech: f64 = vad
                    .segments_from_probabilities(params)
                    .expect("spans")
                    .map(|seg| (seg.end - seg.start) as f64 / 100.0)
                    .sum();
                line.push_str(&format!(" @{threshold}: {speech:5.1}s"));
            }
            println!("{line}");
        }
        println!("--- end ---\n");
    }

    /// The day-two fixtures through the whole pipeline the app now runs.
    #[test]
    #[ignore = "readout: needs DAY_TWO_DIR and models"]
    fn day_two_pipeline() {
        let (Ok(dir), Ok(models)) = (
            std::env::var("DAY_TWO_DIR"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            eprintln!("skipping: set DAY_TWO_DIR and VOICEDUMPS_MODEL_DIR");
            return;
        };
        let ctx = WhisperContext::new_with_params(
            Path::new(&models).join(auto_model().file_name()).to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .expect("load model");
        let mut vad = whisper_rs::WhisperVadContext::new(
            &Path::new(&models).join(VAD_FILE).to_string_lossy(),
            whisper_rs::WhisperVadContextParams::default(),
        )
        .expect("load the speech detector");

        let variants: Vec<(&str, Vec<&'static str>, Vec<String>)> = vec![
            ("en", vec!["en"], vec![]),
            ("en+hi", vec!["en", "hi"], vec![]),
            ("en+vocab", vec!["en"], vec!["Shaun".into(), "Siobhan".into(), "Nguyen".into()]),
        ];
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .collect();
        files.sort();
        println!("\n--- day two, pipeline ---");
        for file in files {
            let samples = decode_mono_16k(&file).expect("decode");
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            let spans = speech_spans(&mut vad, &samples, VAD_THRESHOLD).expect("detect");
            if spans.is_empty() {
                println!("  {name:<26} no speech found — nothing decoded");
                continue;
            }
            let heard = Detected::Speech(spans);
            for (label, languages, terms) in &variants {
                let lessons = Lessons {
                    languages: languages.clone(),
                    terms: terms.clone(),
                    names: Vec::new(),
                    rules: Vec::new(),
                };
                let mut st = ctx.create_state().expect("state");
                let began = std::time::Instant::now();
                let (segs, main) = decode_with(&mut st, &samples, &heard, &lessons, &mut |_, _| {})
                    .expect("decode");
                let ms = began.elapsed().as_millis();
                let said = segs
                    .iter()
                    .map(|seg| format!("[{}] {}", seg["language"].as_str().unwrap_or("?"), seg["text"].as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("  {name:<26} {label:<9} {ms:>5}ms main={main}  {said}");
            }
        }
        println!("--- end ---\n");
    }

    #[test]
    #[ignore = "benchmark: run via scripts/bench.sh"]
    fn benchmark_latency() {
        use std::time::Instant;

        let (Ok(audio), Ok(models)) = (
            std::env::var("TEST_AUDIO"),
            std::env::var("VOICEDUMPS_MODEL_DIR"),
        ) else {
            eprintln!("skipping: set TEST_AUDIO and VOICEDUMPS_MODEL_DIR");
            return;
        };

        let size = auto_model();
        let threads = num_threads();

        let t0 = Instant::now();
        let samples = decode_mono_16k(Path::new(&audio)).expect("decode");
        let decode = t0.elapsed();
        let seconds = samples.len() as f64 / SAMPLE_RATE as f64;

        let before = rss_mb();
        let model = Path::new(&models).join(size.file_name());
        let t1 = Instant::now();
        let ctx = WhisperContext::new_with_params(
            model.to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .expect("load model");
        let load = t1.elapsed();
        let after_load = rss_mb();

        // Two identical runs on the same context. The second is what a
        // back-to-back dictation costs, with nothing left to load.
        let run = || -> (std::time::Duration, usize) {
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_translate(false);
            params.set_token_timestamps(true);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_n_threads(threads as i32);

            let mut st = ctx.create_state().expect("state");
            let t = Instant::now();
            st.full(params, &samples).expect("transcribe");
            let elapsed = t.elapsed();
            let chars: usize = (0..st.full_n_segments())
                .filter_map(|i| st.get_segment(i))
                .filter_map(|s| s.to_str_lossy().ok().map(|c| c.trim().len()))
                .sum();
            (elapsed, chars)
        };

        let (first, chars) = run();
        let (second, _) = run();
        let peak_rss = rss_mb();

        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        let felt = ms(decode) + ms(first);

        println!("\n--- voicedumps latency ---");
        println!("model              {} ({} threads)", size.label(), threads);
        println!("audio              {seconds:.2}s, {chars} chars transcribed");
        println!("decode             {:.0} ms", ms(decode));
        println!("model load (cold)  {:.0} ms", ms(load));
        println!("transcribe         {:.0} ms", ms(first));
        println!("transcribe (again) {:.0} ms", ms(second));
        println!("felt on release    {felt:.0} ms   (decode + transcribe, model already warm)");
        println!("realtime factor    {:.1}x   (audio seconds per second of compute)",
            seconds / first.as_secs_f64());
        println!("rss before load    {before:.0} MB");
        println!("rss with model     {after_load:.0} MB");
        println!("rss peak           {peak_rss:.0} MB");
        println!("--- end ---\n");

        assert!(chars > 0, "nothing was transcribed, so these numbers mean nothing");
    }
}
