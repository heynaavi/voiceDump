//! Which microphone gets listened to.
//!
//! macOS already has a system input device, and following it is the right
//! default — most people have one microphone, and the ones who switch expect
//! everything to switch with them. But "the system input" is also a setting a
//! video call can change out from under you, and a laptop lid microphone is a
//! poor substitute for the interface you actually speak into. So the choice is
//! offered here, and *not choosing* stays a real answer rather than a fallback.
//!
//! Devices are remembered by name. cpal exposes no stable identifier — its
//! `Device` handle is alive only as long as the enumeration that produced it,
//! and cannot be persisted — so the name is what there is. It survives
//! unplugging and replugging, which is the case that matters; the case it
//! cannot serve is two devices sharing a name, and there the list shows one
//! entry because a second would be indistinguishable the moment it was picked.

use serde::Serialize;

/// One input device, as offered to the user.
#[derive(Serialize)]
pub struct Mic {
    pub name: String,
    /// Whether macOS would pick this one right now. Shown next to the "system
    /// default" option so that option says what it currently means, rather than
    /// leaving the user to guess what they are agreeing to.
    pub is_default: bool,
}

/// Every microphone currently attached.
///
/// Enumerated on demand rather than cached: this is asked for when the picker
/// opens, which is exactly the moment someone has just plugged something in and
/// wants to see it.
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn list_microphones() -> Vec<Mic> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };

    let mut out: Vec<Mic> = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        if out.iter().any(|m| m.name == name) {
            continue;
        }
        let is_default = default.as_deref() == Some(name.as_str());
        out.push(Mic { name, is_default });
    }
    out
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn list_microphones() -> Vec<Mic> {
    Vec::new()
}

/// The device to record from, given what the user asked for.
///
/// A named device that isn't attached falls back to the system default and says
/// so in the log. The alternative — refusing to record because the good
/// microphone is in a bag — would turn a preference into a way to break the
/// globe key, and the user pressing it is mid-sentence, not mid-diagnosis.
#[cfg(target_os = "macos")]
pub fn open(preferred: Option<&str>) -> Result<cpal::Device, String> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();

    if let Some(name) = preferred.filter(|n| !n.is_empty()) {
        match host.input_devices() {
            Ok(mut devices) => {
                let found = devices.find(|d| d.name().is_ok_and(|n| n == name));
                if let Some(device) = found {
                    return Ok(device);
                }
                eprintln!("[dictation] \"{name}\" is not connected — recording from the system default");
            }
            Err(e) => {
                eprintln!("[dictation] could not list microphones ({e}) — recording from the system default");
            }
        }
    }

    host.default_input_device()
        .ok_or_else(|| "no microphone found".to_string())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Runs against whatever is plugged into the machine, so it asserts the
    /// shape of the list rather than its contents — the picker relies on names
    /// being usable as keys, and on there being one obvious default to point at.
    #[test]
    fn the_list_is_usable_as_a_picker() {
        let mics = list_microphones();
        for m in &mics {
            assert!(!m.name.trim().is_empty(), "a microphone has no name");
            println!("{} {}", if m.is_default { "*" } else { " " }, m.name);
        }

        let names: std::collections::HashSet<&str> =
            mics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names.len(), mics.len(), "two entries share a name");

        let defaults = mics.iter().filter(|m| m.is_default).count();
        assert!(defaults <= 1, "{defaults} devices claim to be the default");
    }

    /// The fallback is the whole reason a preference cannot break dictation.
    #[test]
    fn an_absent_device_falls_back_to_the_default() {
        use cpal::traits::{DeviceTrait, HostTrait};

        let Some(default) = cpal::default_host().default_input_device() else {
            return; // No microphone at all; there is nothing to fall back to.
        };
        let picked = open(Some("Nothing By This Name")).expect("fell back");
        assert_eq!(picked.name().ok(), default.name().ok());
    }
}
