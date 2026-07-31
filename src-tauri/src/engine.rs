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
}
