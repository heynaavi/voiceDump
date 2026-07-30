//! Audio cues for dictation.
//!
//! Synthesised to WAV on first run rather than shipped as assets: the tones are
//! a few hundred bytes of arithmetic, and generating them keeps the repo free of
//! binary blobs and lets the shape be tuned in code.
//!
//! Played with `afplay` rather than through the webview, because Web Audio needs
//! a user gesture to start and the overlay window is hidden when the cue fires —
//! the exact case where autoplay policy bites.

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const RATE: u32 = 44_100;
/// Deliberately quiet. This fires next to whatever the user is actually doing,
/// so it should register as a tick, not an alert.
const AMPLITUDE: f32 = 0.16;

/// One sine blip with a short attack and exponential decay. A raw sine gated on
/// and off clicks audibly; the envelope is what makes it a tick.
fn blip(samples: &mut Vec<i16>, freq: f32, ms: u32, gain: f32) {
    let n = (RATE * ms / 1000) as usize;
    let attack = (RATE / 400) as usize; // ~2.5ms
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let env = if i < attack {
            i as f32 / attack as f32
        } else {
            let d = (i - attack) as f32 / (n - attack).max(1) as f32;
            (1.0 - d).powf(2.2)
        };
        let v = (TAU * freq * t).sin() * env * AMPLITUDE * gain;
        samples.push((v * i16::MAX as f32) as i16);
    }
}

fn silence(samples: &mut Vec<i16>, ms: u32) {
    samples.extend(std::iter::repeat(0).take((RATE * ms / 1000) as usize));
}

fn write_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend(b"RIFF");
    out.extend(&(36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(&16u32.to_le_bytes()); // PCM header size
    out.extend(&1u16.to_le_bytes()); // PCM
    out.extend(&1u16.to_le_bytes()); // mono
    out.extend(&RATE.to_le_bytes());
    out.extend(&(RATE * 2).to_le_bytes()); // byte rate
    out.extend(&2u16.to_le_bytes()); // block align
    out.extend(&16u16.to_le_bytes()); // bits
    out.extend(b"data");
    out.extend(&data_len.to_le_bytes());
    for s in samples {
        out.extend(&s.to_le_bytes());
    }
    std::fs::write(path, out)
}

pub struct Cues {
    pub start: PathBuf,
    pub stop: PathBuf,
}

/// Create the cue files if they aren't already there.
pub fn ensure(data_dir: &Path) -> std::io::Result<Cues> {
    let dir = data_dir.join("sounds");
    std::fs::create_dir_all(&dir)?;

    let start = dir.join("start.wav");
    if !start.exists() {
        // Two rising ticks: "I'm listening." Rising reads as opening.
        let mut s = Vec::new();
        blip(&mut s, 660.0, 34, 0.75);
        silence(&mut s, 40);
        blip(&mut s, 990.0, 42, 1.0);
        write_wav(&start, &s)?;
    }

    let stop = dir.join("stop.wav");
    if !stop.exists() {
        // One falling tick: closing the loop, lower and softer than the start.
        let mut s = Vec::new();
        blip(&mut s, 520.0, 50, 0.85);
        write_wav(&stop, &s)?;
    }

    Ok(Cues { start, stop })
}

/// Fire and forget — never block the caller on audio.
pub fn play(path: &Path) {
    let _ = Command::new("afplay")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
