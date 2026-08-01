//! "Is there a newer version?" — asked only when the user asks it.
//!
//! This is the one place in the app that talks to the network, and it is worth
//! being explicit about what that means. The request carries no identifier, no
//! transcript, no telemetry: it is a plain GET for a public release listing,
//! sent when someone clicks the version number and at no other time. There is
//! no timer, no check on launch, and nothing to opt out of, because there is
//! nothing running in the background to opt out of.
//!
//! **Why curl.** macOS ships `/usr/bin/curl`, which brings the system trust
//! store with it. Linking an HTTP client and a TLS stack into a local
//! transcription app to service one button is a poor trade — the same reasoning
//! that made `media.rs` reach for `/usr/bin/afconvert` rather than bundle an
//! encoder. Absolute path, as there, so a `PATH` we don't control can't decide
//! what "curl" means.
//!
//! **Why the URL is built here rather than read from the response.** GitHub's
//! JSON has an `html_url` and it would be the obvious thing to hand to the
//! browser. But that is a string from the network deciding what the app opens,
//! and the whole value of a signed-off release page is that it is *the* release
//! page. So the tag is validated as a version and nothing else — digits and
//! dots — and the URL is assembled from a constant repository. A response that
//! has been tampered with can, at absolute worst, name a tag that does not
//! exist.
//!
//! **What this does not do.** It does not install anything. Downloading and
//! swapping a running application is a different problem with a different
//! answer — a signed update feed, a key that never touches this repository —
//! and doing it *without* that machinery, by fetching a binary over HTTPS and
//! trusting it because the connection was encrypted, is how you turn a
//! transcription app into an execution service for whoever holds the domain.

use std::process::Command;

use serde::Serialize;

/// Where releases live. A constant, so the response can never redirect us.
const REPO: &str = "heynaavi/voiceDump";
const CURL: &str = "/usr/bin/curl";

#[derive(Serialize)]
pub struct Update {
    /// The running version, from the bundle rather than anything on disk.
    pub current: String,
    /// The newest published tag, normalised without its leading "v".
    pub latest: String,
    /// Whether `latest` is actually ahead. Equal or older is not an update.
    pub newer: bool,
}

/// Parse "0.4.1" into comparable numbers. `None` for anything else.
///
/// Deliberately strict: this is the gate that decides a string is a version and
/// therefore safe to put in a URL. Pre-release suffixes are refused rather than
/// half-understood — a tag this can't read is reported as "no update", which is
/// the harmless direction to be wrong in.
fn parts(tag: &str) -> Option<Vec<u32>> {
    let trimmed = tag.strip_prefix('v').unwrap_or(tag);
    if trimmed.is_empty() || trimmed.len() > 32 {
        return None;
    }
    let nums: Vec<u32> = trimmed
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    if nums.is_empty() || nums.len() > 4 {
        return None;
    }
    Some(nums)
}

/// Is `latest` ahead of `current`? Missing components count as zero, so
/// "0.5" beats "0.4.9" and ties with "0.5.0".
fn is_newer(latest: &str, current: &str) -> bool {
    let (Some(a), Some(b)) = (parts(latest), parts(current)) else {
        return false;
    };
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Fetch the newest published tag from GitHub.
fn latest_tag(agent: &str) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = Command::new(CURL)
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            // Redirects are followed, but only ever to https.
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            // A hung request must not leave the button spinning forever.
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            &format!("User-Agent: {agent}"),
            &url,
        ])
        .output()
        .map_err(|e| format!("could not reach GitHub: {e}"))?;

    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim();
        return Err(if why.is_empty() {
            "could not reach GitHub".into()
        } else {
            format!("could not reach GitHub: {why}")
        });
    }

    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| "unreadable reply from GitHub".to_string())?;
    body.get("tag_name")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string())
        .ok_or_else(|| "no release published yet".to_string())
}

#[tauri::command]
pub async fn check_update(app: tauri::AppHandle) -> Result<Update, String> {
    let current = app.package_info().version.to_string();
    let agent = format!("VoiceDumps/{current}");

    // Blocking process call, kept off the runtime's threads.
    let tag = tauri::async_runtime::spawn_blocking(move || latest_tag(&agent))
        .await
        .map_err(|_| "the check was interrupted".to_string())??;

    let latest = parts(&tag)
        .map(|_| tag.strip_prefix('v').unwrap_or(&tag).to_string())
        .ok_or_else(|| "GitHub returned a version this can't read".to_string())?;

    Ok(Update {
        newer: is_newer(&latest, &current),
        latest,
        current,
    })
}

/// Open the release page for a version in the browser.
///
/// Takes a version, not a URL. The webview cannot name a destination — it can
/// only name a tag, which is re-validated here before it is put anywhere near
/// a URL.
#[tauri::command]
pub fn open_release(version: String) -> Result<(), String> {
    parts(&version).ok_or("not a version")?;
    let url = format!("https://github.com/{REPO}/releases/tag/v{}", version);
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_version_is_newer() {
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(is_newer("v0.4.1", "0.4.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        // Numeric, not lexical: "10" is not less than "9" here.
        assert!(is_newer("0.10.0", "0.9.0"));
    }

    #[test]
    fn the_same_or_older_is_not_an_update() {
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("0.4", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        // A dev build ahead of the published release must not be told to
        // "update" backwards.
        assert!(!is_newer("0.4.0", "0.5.0"));
    }

    /// The gate that keeps a network string out of a URL.
    #[test]
    fn only_digits_and_dots_are_a_version() {
        assert!(parts("0.4.1").is_some());
        assert!(parts("v0.4.1").is_some());
        assert!(parts("").is_none());
        assert!(parts("0.4.1-beta").is_none());
        assert!(parts("../../../etc").is_none());
        assert!(parts("0.4.1/../../evil").is_none());
        assert!(parts("latest").is_none());
        assert!(parts(&"1.".repeat(40)).is_none());
    }

    /// An unreadable tag reads as "nothing new", never as an update.
    #[test]
    fn an_unparseable_tag_is_not_an_update() {
        assert!(!is_newer("nightly", "0.4.0"));
        assert!(!is_newer("0.4.1", "not-a-version"));
    }
}
