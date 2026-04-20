//! Recent file changes signal source.
//!
//! Checks for recently created/modified files in key directories (Downloads, Desktop).
//! This is a polled approach — FSEvents-based streaming is a future enhancement.

use serde::{Deserialize, Serialize};

/// A recently changed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentFile {
    /// File name (not full path, for privacy).
    pub name: String,
    /// Directory the file is in.
    pub directory: String,
    /// Seconds since the file was last modified.
    pub age_secs: u64,
}

/// Check for files modified in the last `max_age_secs` in key directories.
pub fn recent_downloads(max_age_secs: u64) -> Vec<RecentFile> {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return vec![],
    };

    let dirs = [
        (format!("{}/Downloads", home), "Downloads"),
        (format!("{}/Desktop", home), "Desktop"),
    ];

    let now = std::time::SystemTime::now();
    let mut files = Vec::new();

    for (path, dir_name) in &dirs {
        let entries = match std::fs::read_dir(path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            if !metadata.is_file() {
                continue;
            }

            let modified = match metadata.modified() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let age = now.duration_since(modified).unwrap_or_default();
            if age.as_secs() <= max_age_secs {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip hidden files
                if name.starts_with('.') {
                    continue;
                }
                files.push(RecentFile {
                    name,
                    directory: dir_name.to_string(),
                    age_secs: age.as_secs(),
                });
            }
        }
    }

    // Sort by most recent first
    files.sort_by_key(|f| f.age_secs);
    // Cap at 10 files
    files.truncate(10);
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recent_downloads_does_not_panic() {
        let files = recent_downloads(60); // last 60 seconds
        for f in &files {
            let _ = serde_json::to_string(f).unwrap();
        }
    }

    #[test]
    fn test_recent_file_serialization() {
        let f = RecentFile {
            name: "report.pdf".into(),
            directory: "Downloads".into(),
            age_secs: 5,
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: RecentFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "report.pdf");
    }
}
