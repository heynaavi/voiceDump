//! Transcript → PDF.
//!
//! The typesetting happens in the bundled Swift helper (`overlay-helper/
//! pdf.swift`), because CoreText already knows how to break lines, hyphenate,
//! kern and paginate the app's own variable fonts, and a Rust PDF library would
//! mean reimplementing all of it worse.
//!
//! The split is deliberate: the window sends strings that are already formatted
//! the way it displays them — the duration, the word count, the timestamps —
//! so the page reads exactly like what the user was looking at, including any
//! edits they have made but not yet reloaded. This layer only resolves paths
//! and moves bytes.

#[cfg(target_os = "macos")]
use std::io::Write;

/// Render a transcript to a PDF at `dest`.
///
/// `doc` is the shape `pdf.swift` parses: `title`, `meta`, and `paragraphs` of
/// `{ stamp, text }`. The font paths are filled in here rather than by the
/// window, which has no idea where the bundle put them.
#[tauri::command]
pub fn export_pdf(
    app: tauri::AppHandle,
    dest: String,
    doc: serde_json::Value,
) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, dest, doc);
        Err("PDF export is only available on macOS.".into())
    }

    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;

        let helper = crate::dictation::helper_binary(&app)
            .ok_or("The export helper is missing from this build.")?;

        let mut doc = doc;
        let obj = doc
            .as_object_mut()
            .ok_or("The transcript payload was not an object.")?;

        // Fall back silently to the system faces if the fonts aren't there —
        // a PDF set in the wrong sans is far better than no PDF.
        let res = app.path().resource_dir().ok();
        for (key, file) in [("sans", "SpaceGrotesk.ttf"), ("mono", "JetBrainsMono.ttf")] {
            let bundled = res.as_ref().map(|r| r.join("fonts").join(file));
            let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../src/assets/fonts")
                .join(file);
            if let Some(p) = bundled.filter(|p| p.exists()).or(Some(dev).filter(|p| p.exists())) {
                obj.insert(key.into(), serde_json::json!(p.to_string_lossy()));
            }
        }

        let payload = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;

        let mut child = std::process::Command::new(&helper)
            .arg("--pdf")
            .arg(&dest)
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start the export helper: {e}"))?;

        child
            .stdin
            .take()
            .ok_or("the export helper refused input")?
            .write_all(&payload)
            .map_err(|e| format!("could not send the transcript to the exporter: {e}"))?;

        let out = child
            .wait_with_output()
            .map_err(|e| format!("the export helper did not finish: {e}"))?;

        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why.trim();
            return Err(if why.is_empty() {
                "the export helper failed".into()
            } else {
                why.to_string()
            });
        }
        Ok(())
    }
}
