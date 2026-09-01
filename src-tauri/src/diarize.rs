//! Putting names to the voices in a single track.
//!
//! The engine answers "what was said". This answers "was that the same person",
//! which is a different question that no version of Whisper attempts — so it is
//! a second model rather than a better one. It also never reads the words: it
//! clusters voice timbre, so a Hindi sentence, an English one, and one that
//! switches between them mid-clause are all the same to it.
//!
//! **This module is the join, not the model.** Whatever produces the turns —
//! `sherpa-onnx` today — hands back time ranges with a cluster id on each. What
//! is here turns those into the labels the reading view already knows how to
//! draw, and it is deliberately pure: no audio, no ONNX, no I/O, so the part
//! most likely to be subtly wrong is the part that is cheapest to test.
//!
//! The reason this is a small file at all is [`crate::insert_transcript`]'s
//! word-level timings, shipped in 1.1.2. Diarization emits ranges; word timings
//! put every word on the same clock. Attribution is then an interval
//! intersection over data already in the database — nothing is re-transcribed.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// `tar`, `curl` and the helper, by absolute path.
///
/// A bundled app inherits launchd's `PATH`, not a login shell's — the mistake
/// `media.rs` documents at length, which made the shipped app quietly worse than
/// every dev build until it was found.
const CURL: &str = "/usr/bin/curl";
const TAR: &str = "/usr/bin/tar";

/// One file the diarizer needs before it can run.
///
/// Data, not code, which is the whole reason these can be fetched on demand
/// while the ONNX runtime beside them cannot: the hardened runtime refuses to
/// load a library it did not see at signing time, but it has no opinion about a
/// file the app reads.
pub struct Asset {
    /// What it is called once it is ready to use.
    pub file: &'static str,
    pub url: &'static str,
    /// Of the download itself — the archive, when it arrives as one.
    pub sha256: &'static str,
    pub bytes: u64,
    /// The member to lift out, when the download is a `tar.bz2` rather than the
    /// model. Upstream publishes the segmentation model only inside an archive.
    pub member: Option<&'static str>,
}

/// Both halves of the pipeline: what is speech, and whose voice it is.
///
/// Checksums and lengths were taken from the files these measurements were made
/// with, so a future release cannot silently swap the weights the numbers in
/// `docs/speaker-diarization.md` describe.
pub const ASSETS: [Asset; 2] = [
    Asset {
        file: "pyannote-segmentation-3.0.int8.onnx",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
        sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
        bytes: 6_958_444,
        member: Some("sherpa-onnx-pyannote-segmentation-3-0/model.int8.onnx"),
    },
    Asset {
        file: "titanet-small.onnx",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx",
        sha256: "ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e",
        bytes: 40_257_283,
        member: None,
    },
];

/// The clustering distance, validated rather than guessed.
///
/// sherpa ships 0.5, which reports seven speakers in its own four-speaker
/// reference file. 0.8 recovers exactly four there, and also on a 1.6-minute and
/// a 28.7-minute English fixture built from the same voices — so it is not a
/// property of length, which was the first theory and was wrong. See
/// `docs/speaker-diarization.md` for the tables.
pub const THRESHOLD: f64 = 0.8;

/// Where the diarizer's models live: beside Whisper's, under the data
/// directory, so replacing the .app never costs a re-download.
pub fn model_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = crate::models::store_dir(app)?.join("diarizer");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {dir:?}: {e}"))?;
    Ok(dir)
}

/// Whether both models are already on disk.
pub fn ready(app: &tauri::AppHandle) -> bool {
    model_dir(app).is_ok_and(|dir| ASSETS.iter().all(|a| dir.join(a.file).exists()))
}

/// Fetch whatever is missing. Does nothing when both are already there.
pub fn fetch(app: &tauri::AppHandle) -> Result<(), String> {
    let dir = model_dir(app)?;
    for asset in ASSETS.iter() {
        let done = dir.join(asset.file);
        if done.exists() {
            continue;
        }
        let part = dir.join(format!("{}.part", asset.file));

        // A leftover longer than the published length is not resumable, it is
        // wrong. Same reasoning as the Whisper download beside it.
        if std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) > asset.bytes {
            let _ = std::fs::remove_file(&part);
        }

        let status = Command::new(CURL)
            .args(["--fail", "--location", "--silent", "--show-error",
                   "--retry", "3", "--retry-delay", "2", "--continue-at", "-", "--output"])
            .arg(&part)
            .arg(asset.url)
            .status()
            .map_err(|e| format!("could not start the download: {e}"))?;
        if !status.success() {
            return Err(format!("could not download {}", asset.file));
        }

        // Verified before it is unpacked or used, and deleted when wrong: a
        // half-file left on disk is found by the next launch and handed to a
        // model loader, which is how a bad download becomes a crash instead of
        // a retry.
        match crate::models::digest(&part) {
            Some(got) if got == asset.sha256 => {}
            other => {
                let _ = std::fs::remove_file(&part);
                return Err(match other {
                    Some(got) => format!("{} downloaded corrupt ({got})", asset.file),
                    None => format!("could not check {}", asset.file),
                });
            }
        }

        match asset.member {
            None => std::fs::rename(&part, &done)
                .map_err(|e| format!("could not save {}: {e}", asset.file))?,
            Some(member) => {
                let out = Command::new(TAR)
                    .arg("-xjf").arg(&part)
                    .arg("-C").arg(&dir)
                    .arg(member)
                    .output()
                    .map_err(|e| format!("could not unpack {}: {e}", asset.file))?;
                if !out.status.success() {
                    return Err(format!("could not unpack {}", asset.file));
                }
                std::fs::rename(dir.join(member), &done)
                    .map_err(|e| format!("could not save {}: {e}", asset.file))?;
                // The archive and its now-empty folder are of no further use.
                let _ = std::fs::remove_file(&part);
                if let Some(top) = Path::new(member).components().next() {
                    let _ = std::fs::remove_dir_all(dir.join(top.as_os_str()));
                }
            }
        }
    }
    Ok(())
}

/// Read the helper's output into turns.
///
/// Its format is one line per turn: `12.345 -- 67.890 speaker_03`. Anything else
/// on the stream — the config echo it prints at startup, progress, warnings — is
/// skipped rather than parsed, because a strict reader would turn a new banner
/// line upstream into a feature that stops working.
pub fn parse_turns(out: &str) -> Vec<Turn> {
    let mut turns = Vec::new();
    for line in out.lines() {
        let Some((range, who)) = line.rsplit_once(' ') else { continue };
        let Some(cluster) = who.trim().strip_prefix("speaker_") else { continue };
        let Some((start, end)) = range.split_once("--") else { continue };
        let (Ok(start), Ok(end), Ok(cluster)) = (
            start.trim().parse::<f64>(),
            end.trim().parse::<f64>(),
            cluster.parse::<u32>(),
        ) else {
            continue;
        };
        if end > start {
            turns.push(Turn { start, end, cluster });
        }
    }
    turns
}


/// The helper binary, in the bundle or in a dev checkout.
///
/// Same two-step lookup the HUD and capture helpers use: `resource_dir` is where
/// a shipped app finds it, and the source tree is where `npm run tauri dev`
/// does.
fn helper(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    if let Ok(res) = app.path().resource_dir() {
        let p = res.join("voicedumps-diarize");
        if p.exists() {
            return Some(p);
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../diarize-helper/voicedumps-diarize");
    dev.exists().then_some(dev)
}

/// Find the voices in one recording.
///
/// Blocking, and expected to be: it runs at about 6× realtime, so a ten-minute
/// note takes well under two. On CoreML rather than the GPU, which is not a
/// detail — Whisper has Metal, and since 1.1.2 it may have two decodes running
/// on it at once. Putting this on the Neural Engine keeps a diarization from
/// slowing a transcription that somebody is waiting for.
pub fn run(app: &tauri::AppHandle, audio: &str) -> Result<Vec<Turn>, String> {
    let helper = helper(app).ok_or("the speaker helper is missing from this build")?;
    let dir = model_dir(app)?;
    let segmentation = dir.join(ASSETS[0].file);
    let embedding = dir.join(ASSETS[1].file);
    if !segmentation.exists() || !embedding.exists() {
        return Err("the speaker models have not been downloaded".into());
    }

    let out = Command::new(&helper)
        .arg(format!("--segmentation.pyannote-model={}", segmentation.display()))
        .arg(format!("--embedding.model={}", embedding.display()))
        .arg("--segmentation.provider=coreml")
        .arg("--embedding.provider=coreml")
        .arg("--segmentation.num-threads=4")
        .arg("--embedding.num-threads=4")
        .arg(format!("--clustering.cluster-threshold={THRESHOLD}"))
        .arg(audio)
        .output()
        .map_err(|e| format!("could not run the speaker helper: {e}"))?;

    if !out.status.success() {
        // Its diagnostics go to stderr and are long; the last line is the one
        // that says what went wrong.
        let why = String::from_utf8_lossy(&out.stderr);
        let last = why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output");
        return Err(format!("the speaker helper failed: {last}"));
    }
    Ok(parse_turns(&String::from_utf8_lossy(&out.stdout)))
}

/// One stretch of audio that one voice owns.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    /// The clusterer's own id. Arbitrary, and not what anybody is shown — see
    /// [`label_of`] for why it gets renumbered before it reaches a reader.
    pub cluster: u32,
}

/// How a cluster id is rendered once it has been put in speaking order.
///
/// Numbers rather than letters: they read correctly in prose and in the exported
/// Markdown (`Speaker 2: …`), and they do not run out at twenty-six.
fn label_of(ordinal: usize) -> String {
    format!("Speaker {}", ordinal + 1)
}

/// Cluster ids, in the order their voices first speak.
///
/// The clusterer's ids are arbitrary and shuffle between runs, so `speaker_03`
/// carries no meaning and would not survive re-running the same audio. Ordering
/// by first speech does carry meaning and is stable: Speaker 1 opened the call.
pub fn speaking_order(turns: &[Turn]) -> Vec<u32> {
    let mut order: Vec<u32> = Vec::new();
    let mut by_start = turns.to_vec();
    by_start.sort_by(|a, b| a.start.total_cmp(&b.start));
    for turn in by_start {
        if !order.contains(&turn.cluster) {
            order.push(turn.cluster);
        }
    }
    order
}

/// The voice that owns a moment, if any turn covers it.
fn cluster_at(turns: &[Turn], at: f64) -> Option<u32> {
    turns
        .iter()
        .find(|t| at >= t.start && at < t.end)
        .map(|t| t.cluster)
}

/// Which voice a segment belongs to, by how much of it each one holds.
///
/// A majority vote across the segment's words rather than a lookup at its start
/// time. Turn boundaries and segment boundaries are drawn by two models that
/// have never met, so they land a little apart — and a segment whose first
/// syllable falls inside the previous speaker's turn is common enough that
/// trusting the start alone mislabels whole sentences at every handover.
///
/// Falls back to the segment's midpoint when it has no word timings, which is
/// what a transcript saved before 1.1.2 looks like.
pub fn cluster_for_segment(segment: &Value, turns: &[Turn]) -> Option<u32> {
    let words = segment["words"].as_array();

    let mut tally: Vec<(u32, usize)> = Vec::new();
    if let Some(words) = words {
        for word in words {
            let (Some(start), Some(end)) = (word["start"].as_f64(), word["end"].as_f64()) else {
                continue;
            };
            if let Some(cluster) = cluster_at(turns, (start + end) / 2.0) {
                match tally.iter_mut().find(|(c, _)| *c == cluster) {
                    Some((_, n)) => *n += 1,
                    None => tally.push((cluster, 1)),
                }
            }
        }
    }

    if tally.is_empty() {
        let (Some(start), Some(end)) = (segment["start"].as_f64(), segment["end"].as_f64()) else {
            return None;
        };
        return cluster_at(turns, (start + end) / 2.0);
    }

    // Ties go to whichever was seen first, which is the earlier speaker — the
    // same bias as `speaking_order`, so a coin-flip segment at a handover stays
    // with the person who was already talking rather than jumping ahead.
    tally.iter().max_by_key(|(_, n)| *n).map(|(c, _)| *c)
}

/// Whether a set of turns is worth labelling at all.
///
/// One voice earns nothing. On a one-to-one call the other side is already
/// called `Others`, and replacing a word that means something with `Speaker 1`
/// is a loss — the number says no more than the word did and looks like the app
/// found something it did not.
pub fn worth_labelling(turns: &[Turn]) -> bool {
    speaking_order(turns).len() >= 2
}

/// Write speaker labels onto the segments a diarizer covered.
///
/// Returns how many segments were labelled. Segments no turn covers keep
/// whatever they had: silence, music and crosstalk all land there, and guessing
/// at them would be inventing an attribution rather than reporting one.
pub fn label_segments(segments: &mut Value, turns: &[Turn]) -> usize {
    if !worth_labelling(turns) {
        return 0;
    }
    let order = speaking_order(turns);
    let Some(items) = segments.as_array_mut() else {
        return 0;
    };

    let mut labelled = 0;
    for segment in items.iter_mut() {
        let Some(cluster) = cluster_for_segment(segment, turns) else { continue };
        let Some(ordinal) = order.iter().position(|c| *c == cluster) else { continue };
        segment["speaker"] = json!(label_of(ordinal));
        labelled += 1;
    }
    labelled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(start: f64, end: f64, cluster: u32) -> Turn {
        Turn { start, end, cluster }
    }

    fn segment(start: f64, end: f64, words: &[(f64, f64)]) -> Value {
        json!({
            "start": start,
            "end": end,
            "text": "words",
            "words": words.iter().map(|(s, e)| json!({"start": s, "end": e, "text": "w"}))
                .collect::<Vec<_>>(),
        })
    }

    #[test]
    fn the_helpers_output_is_read_and_its_noise_ignored() {
        // Real output: a config echo first, then one line per turn. Parsing
        // strictly would mean a new banner line upstream silently breaking the
        // feature, so anything unrecognised is skipped rather than fatal.
        let out = "\
OfflineSpeakerDiarizationConfig(segmentation=...)
Started
  0.031 -- 1.185 speaker_00
 12.500 -- 14.000 speaker_02
not a turn at all
 20.000 -- 19.000 speaker_01
";
        let turns = parse_turns(out);
        assert_eq!(turns.len(), 2, "{turns:?}");
        assert_eq!(turns[0], Turn { start: 0.031, end: 1.185, cluster: 0 });
        assert_eq!(turns[1], Turn { start: 12.5, end: 14.0, cluster: 2 });
        // A turn that ends before it starts is not a turn.
        assert!(turns.iter().all(|t| t.end > t.start));
    }

    #[test]
    fn every_asset_is_pinned_to_a_length_and_a_digest() {
        // These are the exact files the numbers in the design doc were measured
        // against; an unpinned download could quietly replace them.
        for a in ASSETS.iter() {
            assert_eq!(a.sha256.len(), 64, "{}", a.file);
            assert!(a.bytes > 1_000_000, "{}", a.file);
            assert!(a.url.starts_with("https://"), "{}", a.file);
        }
        assert_eq!(THRESHOLD, 0.8);
    }

    #[test]
    fn speakers_are_numbered_by_who_talks_first() {
        // Cluster 7 speaks first, so it is Speaker 1 — the clusterer's own
        // numbering is arbitrary and must not leak to a reader.
        let turns = vec![turn(10.0, 20.0, 3), turn(0.0, 5.0, 7), turn(30.0, 40.0, 3)];
        assert_eq!(speaking_order(&turns), vec![7, 3]);
        assert_eq!(label_of(0), "Speaker 1");
        assert_eq!(label_of(1), "Speaker 2");
    }

    #[test]
    fn one_voice_is_never_labelled() {
        // The whole point: "Others" already says this, and better.
        let one = vec![turn(0.0, 10.0, 0), turn(12.0, 20.0, 0)];
        assert!(!worth_labelling(&one));

        let mut segments = json!([segment(0.0, 5.0, &[(0.0, 5.0)])]);
        assert_eq!(label_segments(&mut segments, &one), 0);
        assert!(segments[0].get("speaker").is_none());

        assert!(worth_labelling(&[turn(0.0, 5.0, 0), turn(5.0, 9.0, 1)]));
    }

    #[test]
    fn a_segment_goes_to_whoever_holds_most_of_it() {
        // Straddles a handover: one word left of the boundary, three right of
        // it. Reading the start time alone would give this to the wrong person,
        // which is what happens at every single handover in a conversation.
        let turns = vec![turn(0.0, 10.0, 0), turn(10.0, 30.0, 1)];
        let straddling = segment(9.0, 20.0, &[(9.0, 9.5), (11.0, 12.0), (13.0, 14.0), (15.0, 16.0)]);
        assert_eq!(cluster_for_segment(&straddling, &turns), Some(1));
        assert_eq!(cluster_at(&turns, 9.25), Some(0), "the start really is speaker 0");
    }

    #[test]
    fn a_segment_with_no_word_times_falls_back_to_its_middle() {
        // What a transcript saved before 1.1.2 looks like.
        let turns = vec![turn(0.0, 10.0, 0), turn(10.0, 30.0, 1)];
        let old = json!({"start": 12.0, "end": 18.0, "text": "words"});
        assert_eq!(cluster_for_segment(&old, &turns), Some(1));
    }

    #[test]
    fn uncovered_segments_are_left_alone() {
        // Silence, music and crosstalk land here. Guessing would be inventing.
        let turns = vec![turn(0.0, 5.0, 0), turn(5.0, 9.0, 1)];
        let mut segments = json!([
            segment(0.0, 4.0, &[(0.0, 4.0)]),
            segment(50.0, 55.0, &[(50.0, 55.0)]),
        ]);
        assert_eq!(label_segments(&mut segments, &turns), 1);
        assert_eq!(segments[0]["speaker"], "Speaker 1");
        assert!(segments[1].get("speaker").is_none());
    }
}
