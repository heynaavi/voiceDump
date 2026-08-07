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
    use tauri::Manager;

    let name = size.file_name();
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
    /// When the model was last wanted. `None` means never.
    ///
    /// Always locked *after* `inner`, everywhere, so the reaper and a running
    /// transcription can't deadlock against each other.
    last_use: Mutex<Option<std::time::Instant>>,
}

struct Loaded {
    size: ModelSize,
    ctx: WhisperContext,
}

impl EngineState {
    pub fn unload(&self) {
        *self.inner.lock().unwrap() = None;
        *self.preview.lock().unwrap() = None;
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
/// Safety comes from taking the same lock `run` holds for the entire length of
/// a transcription: the reaper simply blocks until the work is finished, so it
/// can never drop a context out from under whisper. The idle check happens
/// *after* the lock is acquired, so it can't act on a stale reading either.
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
    pub fn step(&mut self, samples: &[f32], cancel: Arc<AtomicBool>) -> Option<Vec<Heard>> {
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
    pub fn step(&mut self, samples: &[f32], cancel: Arc<AtomicBool>) -> Option<Vec<Heard>> {
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
fn decode_mono_16k(path: &Path) -> Result<Vec<f32>, String> {
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

    report("Loading model", 0.12);
    let mut guard = state.inner.lock().unwrap();
    ensure_loaded(app, &mut guard, wanted)?;
    state.touch();
    let loaded = guard.as_ref().expect("model just loaded");

    report("Transcribing", 0.2);
    let params = decoding();
    // Which weights actually ran, which is not always `wanted`: `ensure_loaded`
    // keeps an already-resident model rather than paying a reload to honour a
    // preference that changed since.
    let ran = loaded.size;

    let mut st = loaded
        .ctx
        .create_state()
        .map_err(|e| format!("could not start the transcriber: {e}"))?;

    // Time the decode alone. Reading the file, loading weights and building
    // paragraphs all vary with things that have nothing to do with the model —
    // a cold load would make the same audio look three times slower on the
    // first note of the day — and it is the model that Insights is comparing.
    let began = std::time::Instant::now();
    st.full(params, &samples)
        .map_err(|e| format!("transcription failed: {e}"))?;
    let elapsed_ms = began.elapsed().as_millis().min(i64::MAX as u128) as i64;

    let n = st.full_n_segments();
    let mut segments: Vec<Value> = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        let Some(seg) = st.get_segment(i) else { continue };
        let text = seg.to_str_lossy().unwrap_or_default().into_owned();
        let trimmed = text.trim();
        if trimmed.is_empty() || is_non_speech(trimmed) {
            continue;
        }
        // whisper.cpp reports timestamps in centiseconds.
        let start = seg.start_timestamp() as f64 / 100.0;
        let end = seg.end_timestamp() as f64 / 100.0;

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
            words.push(json!({
                "start": data.t0 as f64 / 100.0,
                "end": data.t1 as f64 / 100.0,
                "text": raw,
            }));
        }

        segments.push(json!({
            "start": start,
            "end": end,
            "text": trimmed,
            "words": words,
        }));
        report(
            "Transcribing",
            0.2 + 0.75 * ((i + 1) as f64 / n.max(1) as f64),
        );
    }
    // Again on the way out: a long file can transcribe for minutes, and dating
    // the model's last use from when the job *started* would make an hour-long
    // recording look idle the moment it finished.
    state.touch();
    drop(guard);

    let segments = collapse_repeats(segments);
    let paragraphs = build_paragraphs(&segments);
    let text = paragraphs
        .iter()
        .map(|p| p["text"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n\n");

    report("Done", 1.0);
    Ok(json!({
        "duration": duration,
        "language": "en",
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
