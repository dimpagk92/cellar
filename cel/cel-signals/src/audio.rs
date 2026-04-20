//! Audio state signal source.
//!
//! macOS: queries system audio state via osascript.
//! Linux: queries PulseAudio via pactl for volume and mute state.
//! Detects whether audio is playing, which app, volume level, and mute state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioState {
    /// System volume (0.0-1.0).
    pub volume: f32,
    /// Whether system output is muted.
    pub is_muted: bool,
}

pub fn read_audio_state() -> Option<AudioState> {
    #[cfg(target_os = "macos")]
    {
        read_audio_macos()
    }
    #[cfg(target_os = "linux")]
    {
        read_audio_linux()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn read_audio_macos() -> Option<AudioState> {
    // Get volume and mute state via osascript
    let output = std::process::Command::new("osascript")
        .args(["-e", "output volume of (get volume settings) & \",\" & output muted of (get volume settings)"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let parts: Vec<&str> = result.split(',').collect();
    if parts.len() < 2 {
        return None;
    }

    let volume = parts[0].trim().parse::<f32>().unwrap_or(0.0) / 100.0;
    let is_muted = parts[1].trim() == "true";

    Some(AudioState { volume, is_muted })
}

#[cfg(target_os = "linux")]
fn read_audio_linux() -> Option<AudioState> {
    // Get volume via pactl; returns None gracefully if pactl is not found
    let volume_output = std::process::Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        .ok()?;

    let volume = if volume_output.status.success() {
        let text = String::from_utf8_lossy(&volume_output.stdout);
        // Output looks like: "Volume: front-left: 42345 /  65% / -11.33 dB , front-right: ..."
        // Parse the first percentage value
        text.split('/')
            .find_map(|part| {
                let trimmed = part.trim();
                if trimmed.ends_with('%') {
                    trimmed.trim_end_matches('%').trim().parse::<f32>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0.0)
            / 100.0
    } else {
        return None;
    };

    // Get mute state
    let mute_output = std::process::Command::new("pactl")
        .args(["get-sink-mute", "@DEFAULT_SINK@"])
        .output()
        .ok()?;

    let is_muted = if mute_output.status.success() {
        let text = String::from_utf8_lossy(&mute_output.stdout);
        // Output looks like: "Mute: yes" or "Mute: no"
        text.trim().ends_with("yes")
    } else {
        false
    };

    Some(AudioState { volume, is_muted })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_audio_does_not_panic() {
        let _ = read_audio_state();
    }

    #[test]
    fn test_audio_state_serialization() {
        let state = AudioState { volume: 0.75, is_muted: false };
        let json = serde_json::to_string(&state).unwrap();
        let back: AudioState = serde_json::from_str(&json).unwrap();
        assert!((back.volume - 0.75).abs() < 0.01);
    }
}
