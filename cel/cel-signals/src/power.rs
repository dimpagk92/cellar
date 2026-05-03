//! Power/battery state signal source.
//!
//! macOS: reads battery state via pmset.
//! Linux: reads from /sys/class/power_supply/.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerState {
    /// Battery percentage (0.0-1.0), None if no battery (desktop Mac).
    pub battery_level: Option<f32>,
    /// Whether the device is currently charging.
    pub is_charging: bool,
    /// Whether external power is connected.
    pub is_plugged_in: bool,
}

pub fn read_power_state() -> Option<PowerState> {
    #[cfg(target_os = "macos")]
    {
        read_power_macos()
    }
    #[cfg(target_os = "linux")]
    {
        read_power_linux()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn read_power_macos() -> Option<PowerState> {
    let output = std::process::Command::new("pmset")
        .args(["-g", "batt"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);

    // Parse: "Now drawing from 'AC Power'" or "Now drawing from 'Battery Power'"
    let is_plugged_in = text.contains("AC Power");

    // Parse: "InternalBattery-0 (id=...)	85%; charging; 0:45 remaining"
    let battery_level = text
        .lines()
        .find(|l| l.contains("InternalBattery"))
        .and_then(|line| {
            line.split('%').next().and_then(|before_pct| {
                before_pct
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
                    .parse::<f32>()
                    .ok()
                    .map(|v| v / 100.0)
            })
        });

    let is_charging = text.contains("charging") && !text.contains("discharging");

    Some(PowerState {
        battery_level,
        is_charging,
        is_plugged_in,
    })
}

#[cfg(target_os = "linux")]
fn read_power_linux() -> Option<PowerState> {
    let capacity = std::fs::read_to_string("/sys/class/power_supply/BAT0/capacity")
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|v| v / 100.0);

    let status = std::fs::read_to_string("/sys/class/power_supply/BAT0/status")
        .ok()
        .unwrap_or_default();

    let is_charging = status.trim() == "Charging";
    let is_plugged_in = status.trim() != "Discharging";

    if capacity.is_some() {
        Some(PowerState {
            battery_level: capacity,
            is_charging,
            is_plugged_in,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_power_does_not_panic() {
        let _ = read_power_state();
    }

    #[test]
    fn test_power_state_serialization() {
        let state = PowerState {
            battery_level: Some(0.85),
            is_charging: true,
            is_plugged_in: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: PowerState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.battery_level, Some(0.85));
    }
}
