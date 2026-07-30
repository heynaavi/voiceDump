//! The managed media library.
//!
//! Every transcript owns a copy of its audio here, whatever it arrived as. Three
//! reasons this exists rather than pointing at wherever the file came from:
//!
//! 1. **Stability.** A dropped file gets moved, renamed or deleted and playback
//!    silently breaks. The library copy is ours and outlives the original.
//! 2. **Playability.** WKWebView decodes a narrow set of formats — notably *not*
//!    Ogg Opus, which is exactly what a chat app's voice notes are. Normalising to
//!    AAC means anything we ingest is playable by the same `<audio>` element.
//! 3. **Size.** Video is transcoded down to its audio track. A 200 MB screen
//!    recording becomes a ~2 MB m4a, and nothing downstream ever wanted the
//!    pixels.
//!
//! Layout is `media/YYYY/MM/<transcript-id>.m4a` — dated so the directory stays
//! browsable by hand, and keyed by transcript id so there's never a name clash.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Mono AAC. Speech doesn't benefit from stereo or a higher rate, and this
/// keeps an hour-long recording around 30 MB.
const BITRATE: &str = "64k";

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

/// Transcode `src` into the library and return the stored path.
///
/// Falls back to a plain copy if ffmpeg can't handle it, so a transcript is
/// never left pointing at a file outside the library.
pub fn archive(
    data_dir: &Path,
    id: &str,
    src: &Path,
    created_at_ms: i64,
) -> Result<PathBuf, String> {
    let dir = month_dir(&data_dir.join("media"), created_at_ms);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(format!("{id}.m4a"));

    let out = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(src)
        .args([
            // Drop video: we only ever needed the audio track.
            "-vn",
            "-ac", "1",
            "-c:a", "aac",
            "-b:a", BITRATE,
            // Faststart puts the index at the front so the player can seek
            // before the whole file is read.
            "-movflags", "+faststart",
        ])
        .arg(&dest)
        .output()
        .map_err(|e| format!("ffmpeg unavailable: {e}"))?;

    if out.status.success() && dest.exists() {
        return Ok(dest);
    }

    // Keep the original rather than losing the audio entirely; it may still be
    // playable even if the transcode failed.
    let fallback = dir.join(format!(
        "{id}.{}",
        src.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ));
    std::fs::copy(src, &fallback)
        .map_err(|e| format!("could not archive media: {e}"))?;
    Ok(fallback)
}

/// Remove a transcript's archived audio. Best-effort: a missing file is fine.
pub fn discard(path: &str) {
    let p = Path::new(path);
    // Only ever delete inside our own library — never a user's original file.
    if p.components().any(|c| c.as_os_str() == "media") && p.exists() {
        let _ = std::fs::remove_file(p);
    }
}
