//! The system clipboard.
//!
//! Two callers want it for opposite reasons — dictation puts a transcript on it
//! and then puts back whatever it displaced, and the tray puts the last
//! transcript back on it when you wish it hadn't. Shared so those two can't
//! drift into disagreeing about what "the clipboard" means.
//!
//! `pbcopy`/`pbpaste` rather than `NSPasteboard`: both ship with macOS, neither
//! can be missing, and the alternative is a page of `objc2` for a feature whose
//! whole surface is "a string in, a string out".

/// Read the clipboard's text, if it holds any.
///
/// `None` covers both an empty clipboard and one holding something that isn't
/// text — an image, a file — and the two are treated the same on purpose: what
/// this is for is putting back something worth putting back.
#[cfg(target_os = "macos")]
pub fn read() -> Option<String> {
    let out = std::process::Command::new("pbpaste").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    (!text.is_empty()).then_some(text)
}

#[cfg(target_os = "macos")]
pub fn write(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut pb = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("clipboard failed: {e}"))?;
    pb.stdin
        .as_mut()
        .ok_or("clipboard pipe unavailable")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    pb.wait().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn read() -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn write(_text: &str) -> Result<(), String> {
    Err("the clipboard is only wired up on macOS".into())
}
