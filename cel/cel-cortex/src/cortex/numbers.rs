//! Apple Numbers document bootstrap glue (macOS).
//!
//! Ensures a Numbers document is open and ready before `read_cells` /
//! `write_cells` dispatch, dismissing the open / template dialogs via
//! accessibility and System Events.

#[cfg(target_os = "macos")]
use super::focus::activate_app_with_verification;
#[cfg(target_os = "macos")]
use super::*;

#[cfg(target_os = "macos")]
const NUMBERS_DOCUMENT_BOOTSTRAP_CANDIDATES: &[&str] = &[
    "/Applications/Numbers Creator Studio.app/Contents/Resources/SampleDocument.numbers",
    "/Applications/Numbers.app/Contents/Resources/SampleDocument.numbers",
];

#[cfg(target_os = "macos")]
const NUMBERS_BLANK_TEMPLATE_CANDIDATES: &[&str] = &[
    "/Applications/Numbers Creator Studio.app/Contents/SharedSupport/Templates/Blank/Traditional.nmbtemplate",
    "/Applications/Numbers.app/Contents/SharedSupport/Templates/Blank/Traditional.nmbtemplate",
];

#[cfg(target_os = "macos")]
pub(crate) fn should_attempt_numbers_document_bootstrap(error: &InputError) -> bool {
    matches!(
        error,
        InputError::ScriptingUnavailable { app, .. } if app.eq_ignore_ascii_case("Numbers")
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn bootstrap_numbers_document() -> Result<(), String> {
    let mut attempts = Vec::new();

    match activate_app_with_verification("Numbers") {
        Ok(_) => attempts.push("activated Numbers".to_string()),
        Err(err) => attempts.push(format!("activate_app failed: {err}")),
    }

    if numbers_document_ready() {
        attempts.push("existing Numbers document already scriptable".into());
        return Ok(());
    }

    if let Some(document_path) = NUMBERS_DOCUMENT_BOOTSTRAP_CANDIDATES
        .iter()
        .copied()
        .find(|path| std::path::Path::new(path).exists())
    {
        let mut open_command = std::process::Command::new("open");
        open_command.arg(document_path);
        match command_status_with_timeout(open_command, std::time::Duration::from_secs(5)) {
            Some(status) if status.success() => {
                attempts.push(format!("opened sample document {}", document_path));
                std::thread::sleep(std::time::Duration::from_millis(1400));
                record_numbers_reactivation(&mut attempts);
                if numbers_document_ready() {
                    return Ok(());
                }
            }
            Some(status) => attempts.push(format!(
                "open sample document exited with status {:?}",
                status.code()
            )),
            None => attempts.push("open sample document timed out".into()),
        }
    } else {
        attempts.push("no bundled Numbers sample document found".into());
    }

    if let Some(template_path) = NUMBERS_BLANK_TEMPLATE_CANDIDATES
        .iter()
        .copied()
        .find(|path| std::path::Path::new(path).exists())
    {
        let mut open_command = std::process::Command::new("open");
        open_command.arg(template_path);
        match command_status_with_timeout(open_command, std::time::Duration::from_secs(5)) {
            Some(status) if status.success() => {
                attempts.push(format!("opened template {}", template_path));
                std::thread::sleep(std::time::Duration::from_millis(1200));
                if numbers_document_ready() {
                    record_numbers_reactivation(&mut attempts);
                    return Ok(());
                }
            }
            Some(status) => attempts.push(format!(
                "open template exited with status {:?}",
                status.code()
            )),
            None => attempts.push("open template timed out".into()),
        }
    } else {
        attempts.push("no bundled Numbers blank template found".into());
    }

    if let Some(clicked) = click_numbers_dialog_button(&[
        "New Spreadsheet",
        "New Document",
        "Create Document",
        "Create",
        "Blank",
    ]) {
        attempts.push(format!("clicked {}", clicked));
        record_numbers_reactivation(&mut attempts);
        if numbers_document_ready() {
            return Ok(());
        }
    }

    if send_system_keystroke("n", true) {
        attempts.push("sent Cmd+N".into());
        std::thread::sleep(std::time::Duration::from_millis(800));
        if let Some(clicked) = click_numbers_dialog_button(&[
            "New Spreadsheet",
            "New Document",
            "Create Document",
            "Create",
            "Blank",
        ]) {
            attempts.push(format!("clicked {}", clicked));
            record_numbers_reactivation(&mut attempts);
            if numbers_document_ready() {
                return Ok(());
            }
        }
    } else {
        attempts.push("failed to send Cmd+N".into());
    }

    if send_system_key_code(36) {
        attempts.push("sent Return".into());
        std::thread::sleep(std::time::Duration::from_millis(600));
        record_numbers_reactivation(&mut attempts);
        if numbers_document_ready() {
            return Ok(());
        }
        return Err(attempts.join("; "));
    }

    attempts.push("failed to send Return".into());
    Err(attempts.join("; "))
}

#[cfg(target_os = "macos")]
fn record_numbers_reactivation(attempts: &mut Vec<String>) {
    match activate_app_with_verification("Numbers") {
        Ok(_) => attempts.push("re-activated Numbers".into()),
        Err(err) => attempts.push(format!("re-activate Numbers failed: {err}")),
    }
}

#[cfg(target_os = "macos")]
fn numbers_document_ready() -> bool {
    let probe_refs = vec![String::from("A1")];
    cel_input::read_numbers_cells(None, None, &probe_refs).is_ok()
}

#[cfg(target_os = "macos")]
pub(crate) fn dismiss_numbers_dialog_if_present() {
    if let Some(label) = click_numbers_dialog_button_via_ax(&["Cancel"]) {
        trace!(button = %label, "dismissed Numbers dialog via AX cancel");
        std::thread::sleep(std::time::Duration::from_millis(300));
        return;
    }
    if let Some(label) = click_numbers_dialog_button_via_system_events(&["Cancel"]) {
        trace!(button = %label, "dismissed Numbers dialog via System Events cancel");
        std::thread::sleep(std::time::Duration::from_millis(300));
        return;
    }
    if send_system_key_code(53) {
        trace!("dismissed Numbers dialog via Escape");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

#[cfg(target_os = "macos")]
fn click_numbers_dialog_button(candidates: &[&str]) -> Option<String> {
    for _ in 0..5 {
        if let Some(label) = click_numbers_dialog_button_via_ax(candidates) {
            return Some(label);
        }
        if let Some(label) = click_numbers_dialog_button_via_system_events(candidates) {
            return Some(label);
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    None
}

#[cfg(target_os = "macos")]
fn click_numbers_dialog_button_via_ax(candidates: &[&str]) -> Option<String> {
    let tree = cel_accessibility::create_tree();
    for candidate in candidates {
        let matches = tree
            .find_elements(Some(&ElementRole::Button), Some(candidate))
            .ok()?;
        for element in matches {
            if !element.state.enabled || !element.state.visible {
                continue;
            }
            if tree.perform_action(&element.id, "click").ok()? {
                let label = element.label.unwrap_or_else(|| (*candidate).to_string());
                return Some(label);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn click_numbers_dialog_button_via_system_events(candidates: &[&str]) -> Option<String> {
    let quoted_candidates = candidates
        .iter()
        .map(|candidate| {
            let escaped = candidate.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"set targetNames to {{{quoted_candidates}}}
tell application "System Events"
  repeat with p in (every application process whose name is "Numbers" or name contains "Numbers")
    repeat with w in windows of p
      repeat with uiElem in entire contents of w
        try
          set buttonName to (name of uiElem) as text
          repeat with targetName in targetNames
            if buttonName contains (targetName as text) then
              click uiElem
              return buttonName
            end if
          end repeat
        end try
      end repeat
    end repeat
  end repeat
end tell
return ""#
    );

    let mut command = std::process::Command::new("osascript");
    command.args(["-e", &script]);
    let output = command_output_with_timeout(command, std::time::Duration::from_secs(2))?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

#[cfg(target_os = "macos")]
fn send_system_keystroke(key: &str, command_down: bool) -> bool {
    let escaped = key.replace('\\', "\\\\").replace('"', "\\\"");
    let script = if command_down {
        format!(r#"tell application "System Events" to keystroke "{escaped}" using command down"#)
    } else {
        format!(r#"tell application "System Events" to keystroke "{escaped}""#)
    };
    let mut command = std::process::Command::new("osascript");
    command.args(["-e", &script]);
    command_status_with_timeout(command, std::time::Duration::from_secs(2))
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn send_system_key_code(key_code: u16) -> bool {
    let mut command = std::process::Command::new("osascript");
    command.args([
        "-e",
        &format!("tell application \"System Events\" to key code {key_code}"),
    ]);
    command_status_with_timeout(command, std::time::Duration::from_secs(2))
        .map(|status| status.success())
        .unwrap_or(false)
}
