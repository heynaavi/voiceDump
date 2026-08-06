//! The managed media library.
//!
//! Every transcript owns a copy of its audio here, whatever it arrived as. Three
//! reasons this exists rather than pointing at wherever the file came from:
//!
//! 1. **Stability.** A dropped file gets moved, renamed or deleted and playback
//!    silently breaks. The library copy is ours and outlives the original.
//! 2. **Playability.** WKWebView decodes a narrow set of formats — notably *not*
//!    Ogg Opus, which is exactly what Discord voice notes are, nor Matroska or
//!    WebM. Normalising to AAC means anything we ingest is playable by the same
//!    `<audio>` element.
//! 3. **Size.** Video is transcoded down to its audio track. A 200 MB screen
//!    recording becomes a ~2 MB m4a, and nothing downstream ever wanted the
//!    pixels.
//!
//! Layout is `media/YYYY/MM/<transcript-id>.m4a` — dated so the directory stays
//! browsable by hand, and keyed by transcript id so there's never a name clash.
//!
//! **This deliberately does not use `ffmpeg`.** It used to, and that made the
//! shipped app worse than the dev build in a way no amount of local testing
//! would show: a GUI process launched from Finder inherits launchd's `PATH`
//! (`/usr/bin:/bin:/usr/sbin:/sbin`), not a login shell's, so `Command::new
//! ("ffmpeg")` could not find a Homebrew install at `/opt/homebrew/bin` even on
//! the machines that had one. `npm run tauri dev` inherits the terminal's `PATH`
//! and works. Every user therefore fell through to the old raw-copy fallback,
//! which kept the source container intact — and an Ogg or Matroska file handed
//! to `<audio>` is exactly the "audio is unplayable" report.
//!
//! So the pipeline is symphonia to decode (already compiled in for
//! transcription, and the reason the app has no `ffmpeg` dependency at all) and
//! `/usr/bin/afconvert` to encode — CoreAudio's own tool, part of macOS, invoked
//! by absolute path so no `PATH` is consulted.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Mono AAC. Speech doesn't benefit from stereo or a higher rate, and this
/// keeps an hour-long recording around 30 MB.
const BITRATE: &str = "64000";

/// CoreAudio's converter. Absolute, because a bundled app's `PATH` is not the
/// user's — that assumption is what broke this in the first place.
const AFCONVERT: &str = "/usr/bin/afconvert";

fn month_dir(root: &Path, created_at_ms: i64) -> PathBuf {
    // Cheap civil-date maths: enough for a directory name, no chrono dependency.
    let days = created_at_ms / 86_400_000;
    let (mut y, mut d) = (1970, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if d < len {
            break;
        }
        d -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let months = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut m = 0;
    while m < 12 && d >= months[m] {
        d -= months[m];
        m += 1;
    }
    root.join(format!("{y:04}")).join(format!("{:02}", m + 1))
}

/// Write mono f32 samples as 16-bit PCM WAV.
///
/// The intermediate `afconvert` reads, and the end of the line if it can't run:
/// an uncompressed WAV is bulky but it is the one thing every `<audio>` element
/// on macOS will play, so a transcript is never left with silent playback.
fn write_wav(path: &Path, samples: &[f32], rate: u32) -> Result<(), String> {
    let mut w = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|e| format!("could not write audio: {e}"))?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .map_err(|e| format!("could not write audio: {e}"))?;
    }
    w.finalize().map_err(|e| format!("could not write audio: {e}"))
}

/// Transcode `src` into the library and return the stored path.
///
/// Three tiers, each a working fallback rather than a failure: AAC in an m4a is
/// what we want, a WAV is what we keep if CoreAudio's encoder is unavailable,
/// and a plain copy is the last resort for a file symphonia cannot decode — at
/// which point the transcript could not have been produced either, so it is
/// mostly there to keep the user's audio rather than to be played.
pub fn archive(
    data_dir: &Path,
    id: &str,
    src: &Path,
    created_at_ms: i64,
) -> Result<PathBuf, String> {
    let dir = month_dir(&data_dir.join("media"), created_at_ms);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let Ok((samples, rate)) = crate::engine::decode_mono(src) else {
        return copy_verbatim(&dir, id, src);
    };

    let wav = dir.join(format!("{id}.wav"));
    if write_wav(&wav, &samples, rate).is_err() {
        let _ = std::fs::remove_file(&wav);
        return copy_verbatim(&dir, id, src);
    }

    let dest = dir.join(format!("{id}.m4a"));
    let encoded = Command::new(AFCONVERT)
        .args(["-f", "m4af", "-d", "aac", "-b", BITRATE])
        .arg(&wav)
        .arg(&dest)
        .output()
        .map(|o| o.status.success() && dest.exists())
        .unwrap_or(false);

    if encoded {
        let _ = std::fs::remove_file(&wav);
        return Ok(dest);
    }

    // Keep the WAV. Bigger than we'd like, but it plays.
    let _ = std::fs::remove_file(&dest);
    Ok(wav)
}

/// Last resort: the original bytes under our own name, so at least the file is
/// ours and survives the user moving the source.
fn copy_verbatim(dir: &Path, id: &str, src: &Path) -> Result<PathBuf, String> {
    let fallback = dir.join(format!(
        "{id}.{}",
        src.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ));
    std::fs::copy(src, &fallback).map_err(|e| format!("could not archive media: {e}"))?;
    Ok(fallback)
}

/// Whether `<audio>` in WKWebView can actually play this.
///
/// Used to decide whether an *already archived* file needs re-encoding. Anyone
/// who ran a build from before the `ffmpeg` removal has a library full of raw
/// copies — the transcripts are fine, the audio is whatever container it arrived
/// in, and Ogg or Matroska among them simply will not play. They are ours and
/// decodable, so they can be repaired in place rather than written off.
pub fn is_playable(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("m4a" | "mp4" | "mp3" | "wav" | "aac" | "aiff" | "aif" | "caf" | "flac")
    )
}

/// Remove a transcript's archived audio. Best-effort: a missing file is fine.
pub fn discard(path: &str) {
    let p = Path::new(path);
    // Only ever delete inside our own library — never a user's original file.
    if p.components().any(|c| c.as_os_str() == "media") && p.exists() {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(dir: &Path) -> PathBuf {
        let path = dir.join("tone.wav");
        let samples: Vec<f32> = (0..48_000 * 2)
            .map(|i| 0.4 * (i as f32 * 2.0 * std::f32::consts::PI * 330.0 / 48_000.0).sin())
            .collect();
        write_wav(&path, &samples, 48_000).expect("write source");
        path
    }

    /// Archiving must not need anything on `PATH`.
    ///
    /// This is the regression that made shipped builds differ from dev ones: a
    /// GUI process gets launchd's `PATH`, so the old `Command::new("ffmpeg")`
    /// never resolved for a user and every note fell through to a raw copy of
    /// whatever container it arrived in. Clearing `PATH` here reproduces the
    /// bundled app's environment exactly — the encode has to survive it.
    #[test]
    fn archives_without_anything_on_path() {
        let tmp = std::env::temp_dir().join(format!("vd-media-{}", crate::now_ms()));
        std::fs::create_dir_all(&tmp).expect("tmp");
        let src = tone(&tmp);

        let restore = std::env::var_os("PATH");
        // SAFETY: single-threaded test, restored below.
        unsafe { std::env::set_var("PATH", "") };
        let out = archive(&tmp, "note1", &src, 1_760_000_000_000);
        if let Some(p) = restore {
            unsafe { std::env::set_var("PATH", p) };
        }

        let out = out.expect("archive");
        assert_eq!(
            out.extension().and_then(|e| e.to_str()),
            Some("m4a"),
            "expected AAC, got {out:?}"
        );
        assert!(out.exists());

        // Smaller than the PCM it came from, and non-trivial: an empty or
        // header-only file would satisfy `exists()` but play as silence.
        let encoded = std::fs::metadata(&out).expect("stat").len();
        let raw = std::fs::metadata(&src).expect("stat src").len();
        assert!(encoded > 1_000, "suspiciously small: {encoded} bytes");
        assert!(encoded < raw, "{encoded} should be under {raw}");

        // The intermediate must not be left behind next to it.
        assert!(!out.with_extension("wav").exists(), "temp WAV survived");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A file symphonia cannot decode still gets kept, under our own name.
    #[test]
    fn undecodable_input_is_still_archived() {
        let tmp = std::env::temp_dir().join(format!("vd-media-junk-{}", crate::now_ms()));
        std::fs::create_dir_all(&tmp).expect("tmp");
        let src = tmp.join("broken.opus");
        std::fs::write(&src, b"not actually audio").expect("write");

        let out = archive(&tmp, "note2", &src, 1_760_000_000_000).expect("archive");
        assert_eq!(out.extension().and_then(|e| e.to_str()), Some("opus"));
        assert!(out.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
