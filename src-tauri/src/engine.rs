//! Local transcription, in-process.
//!
//! whisper.cpp does the transcription (Metal-accelerated on Apple Silicon) and
//! symphonia does the decoding. Both are compiled into the binary and the model
//! weights ship as a bundle resource, which is the whole point: there is no
//! Python, no virtualenv, no `ffmpeg` on the user's PATH, and nothing fetched at
//! runtime. Download the app, open it, drop in a file — offline, first launch.
//!
//! That constraint drives most of the decisions below. Anything that would need
//! a runtime download or a system binary is out, however convenient.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
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

/// Locate a bundled model file.
fn model_path(app: &tauri::AppHandle, size: ModelSize) -> Option<PathBuf> {
    use tauri::Manager;

    let name = size.file_name();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("VOICEDUMPS_MODEL_DIR") {
        candidates.push(PathBuf::from(dir).join(name));
    }
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("models").join(name));
    }
    // Dev: the repo's own model directory, which isn't committed.
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../models")
            .join(name),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// Why transcription can't run, or None if it can. Checked at startup so a
/// broken build says so immediately rather than on the user's first recording.
pub fn missing_model(app: &tauri::AppHandle) -> Option<String> {
    let want = auto_model();
    if model_path(app, want).is_some() {
        return None;
    }
    Some(format!(
        "The {} speech model is missing from this build.",
        want.label()
    ))
}

// -- resident model ---------------------------------------------------------

/// The loaded model, kept between jobs.
///
/// Loading medium costs a couple of seconds, so holding it makes back-to-back
/// dictations feel instant. It's dropped by [`unload`] when the app goes idle —
/// a menu-bar app that sits on 1.5 GB all day is a bad neighbour.
/// Answered on boot: is the app able to transcribe anything at all?
#[derive(Serialize, Clone)]
pub struct EngineHealth {
    pub error: Option<String>,
}

/// What the window shows while something is being transcribed in the background.
#[derive(Serialize, Clone)]
pub struct IngestProgress {
    pub title: String,
    pub stage: String,
    pub progress: f64,
    pub source: &'static str,
}

#[derive(Default)]
pub struct EngineState {
    inner: Mutex<Option<Loaded>>,
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
            eprintln!(
                "[engine] model released after {}s idle",
                idle_for.as_secs()
            );
        }
    });
}

/// Put `wanted` in the slot, loading it only if what's there isn't already it.
///
/// Shared by `run` and [`warm`] so the two can't drift — a warm-up that loaded
/// the model even slightly differently from the transcription it was warming
/// for would be worse than no warm-up at all.
///
/// The caller holds the lock across the load on purpose. Two callers racing
/// would otherwise each build a context, and one would be dropped seconds later
/// having achieved nothing but a 1.5 GB spike.
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

// -- decoding ---------------------------------------------------------------

/// Decode any supported media file to 16 kHz mono f32.
///
/// symphonia is pure Rust, so this is what lets the app drop its `ffmpeg`
/// dependency: mp3, m4a/aac, wav, flac, ogg and mp4/mov audio tracks all decode
/// in-process.
fn decode_mono_16k(path: &Path) -> Result<Vec<f32>, String> {
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

    Ok(resample_to_16k(&mono, src_rate))
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

/// Group segments into readable paragraphs.
///
/// Three rules, in order: a real pause after a completed thought, a long-enough
/// run that ends on a sentence, or a hard cut for a monologue that never breaks
/// cleanly. Whisper emits segments on its own rhythm, which is far too choppy to
/// read; this is what turns them into prose.
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

/// Transcribe a media file, reporting progress through `report(stage, fraction)`.
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
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_translate(false);
    params.set_token_timestamps(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // Leave a couple of cores for the UI and the rest of the machine.
    params.set_n_threads((num_threads() as i32).max(1));

    let mut st = loaded
        .ctx
        .create_state()
        .map_err(|e| format!("could not start the transcriber: {e}"))?;
    st.full(params, &samples)
        .map_err(|e| format!("transcription failed: {e}"))?;

    let n = st.full_n_segments();
    let mut segments: Vec<Value> = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        let Some(seg) = st.get_segment(i) else { continue };
        let text = seg.to_str_lossy().unwrap_or_default().into_owned();
        let trimmed = text.trim();
        if trimmed.is_empty() {
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
    }))
}

fn num_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(2).max(1))
        .unwrap_or(4)
}

/// Peaks for a file we're not transcribing — used to backfill the waveform on
/// transcripts saved before it existed.
pub fn peaks_for(path: &str) -> Result<Vec<f32>, String> {
    Ok(compute_peaks(&decode_mono_16k(Path::new(path))?))
}

/// Transcribe a file that arrived from outside the window — today a globe-key
/// dictation — mirroring its progress into the sidebar.
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
            IngestProgress {
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

/// Load the model in the background, before anyone asks for it.
///
/// Called the moment globe-key dictation starts, so the model is resident by the
/// time the key comes back up rather than making the user wait through a cold
/// load at exactly the wrong moment. Anything that needs the model meanwhile
/// simply blocks on the same lock, so this can never cause a double load.
pub fn warm(app: &tauri::AppHandle) {
    use tauri::Manager;
    let app = app.clone();
    // Off the calling thread: the load blocks for a couple of seconds, and the
    // caller is the one driving the event tap.
    std::thread::spawn(move || {
        let wanted = auto_model();
        let state = app.state::<EngineState>();
        let mut guard = state.inner.lock().unwrap();
        // Deliberately silent. This is an optimisation, not a step: if it
        // fails, `run` attempts the same load and reports the failure in the
        // place the user can act on it.
        let _ = ensure_loaded(&app, &mut guard, wanted);
    });
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
