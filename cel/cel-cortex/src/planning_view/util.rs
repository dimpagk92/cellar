//! Small formatting helpers (date and content-preview) for the planning view.

pub(crate) fn short_content_preview(content: &serde_json::Value) -> String {
    let s = serde_json::to_string(content).unwrap_or_default();
    if s.len() <= 80 {
        s
    } else {
        format!("{}…", &s[..80])
    }
}

pub(crate) fn unix_to_iso(secs: i64) -> String {
    // Best-effort ISO-8601 without pulling chrono. Same approach as the
    // canonical-runner outcome auto-write.
    let days = secs / 86_400;
    let remaining = secs % 86_400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;
    let (year, month, day) = unix_days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn unix_days_to_ymd(days_from_epoch: i64) -> (i64, u32, u32) {
    let mut days = days_from_epoch;
    let mut year: i64 = 1970;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days >= dy {
            days -= dy;
            year += 1;
        } else {
            break;
        }
    }
    let months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u32 = 1;
    for &dm in &months {
        let dm_actual = if month == 2 && is_leap(year) { 29 } else { dm };
        if days >= dm_actual {
            days -= dm_actual;
            month += 1;
        } else {
            break;
        }
    }
    (year, month, (days + 1) as u32)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}
