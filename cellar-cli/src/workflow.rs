//! Workflow scripts (WS10).
//!
//! A workflow script is a user-authored, ordered list of CEL actions saved as
//! JSON. `cellar run <file>` loads + validates it and dispatches each step
//! through the **same governed gateway path** as `cellar act`
//! (`gateway.intercept`), so every step is rule-checked and produces an
//! execution receipt. `cellar run <file> --dry-run` validates + prints the plan
//! offline (no daemon, no actions dispatched).
//!
//! This module owns the script *model + loading + validation*; the execution
//! loop lives in `main.rs::run_workflow`. Flow control is supported:
//! stop-on-failure (with `--keep-going`), per-step `--retries` on transient
//! failures, and per-step `when` branching (run a step only on the prior step's
//! success/failure).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// A user-authored workflow: a name and an ordered list of steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowScript {
    /// Human-readable workflow name (surfaced in logs/receipts).
    pub name: String,
    /// Ordered steps to dispatch.
    pub steps: Vec<WorkflowStep>,
}

/// One step: a gateway action (`action_type` + `params`) plus optional label
/// and caller. Mirrors the inputs to `cellar act` / `gateway.intercept`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// Optional human label, used in per-step output. Falls back to `action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The gateway action type — e.g. `shell.run`, `fs.move`, `copy_file`.
    pub action: String,
    /// Action arguments as a JSON object. Defaults to `{}`.
    #[serde(default = "empty_object")]
    pub params: serde_json::Value,
    /// Caller label surfaced to rules via `data.caller`. Defaults to `cli.run`
    /// at dispatch time when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    /// When to run this step, relative to the previous *run* step's outcome.
    /// Defaults to `always`. Use `on_failure` (with `--keep-going`) for recovery
    /// steps; `on_success` for steps that should only follow a success. Skipped
    /// steps are not failures and don't change the branch state.
    #[serde(default)]
    pub when: When,
}

/// Branch condition for a [`WorkflowStep`], evaluated against the previous
/// step that actually *ran* (skipped steps don't change it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Always run (the default).
    #[default]
    Always,
    /// Run only if the previous run step was executed by the gateway.
    OnSuccess,
    /// Run only if the previous run step was NOT executed (vetoed / denied /
    /// timed-out).
    OnFailure,
}

impl When {
    /// Whether this step should run, given the previous run step's executed
    /// status (`None` = this is the first step / no prior run step).
    pub fn should_run(&self, prev_executed: Option<bool>) -> bool {
        match self {
            When::Always => true,
            When::OnSuccess => prev_executed == Some(true),
            When::OnFailure => prev_executed == Some(false),
        }
    }
}

impl WorkflowScript {
    /// Load + parse + validate a workflow script from a JSON file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read workflow script {}", path.display()))?;
        let script: WorkflowScript = serde_json::from_str(&raw)
            .with_context(|| format!("parse workflow script {}", path.display()))?;
        script.validate()?;
        Ok(script)
    }

    /// Structural validation: a name, at least one step, and every step names a
    /// non-empty action with object-shaped params.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("workflow has an empty name");
        }
        if self.steps.is_empty() {
            bail!("workflow '{}' has no steps", self.name);
        }
        for (i, step) in self.steps.iter().enumerate() {
            if step.action.trim().is_empty() {
                bail!(
                    "workflow '{}' step {} has an empty action",
                    self.name,
                    i + 1
                );
            }
            if !step.params.is_object() {
                bail!(
                    "workflow '{}' step {} ({}) has non-object params; expected a JSON object",
                    self.name,
                    i + 1,
                    step.action
                );
            }
        }
        Ok(())
    }

    /// Render the execution plan as a human-readable, numbered list.
    pub fn plan_summary(&self) -> String {
        let mut out = format!("workflow: {} ({} steps)\n", self.name, self.steps.len());
        for (i, step) in self.steps.iter().enumerate() {
            let label = step.label.as_deref().unwrap_or("");
            let sep = if label.is_empty() { "" } else { " — " };
            out.push_str(&format!(
                "  {:>2}. {}{}{}\n",
                i + 1,
                step.action,
                sep,
                label
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("cellar-ws10-{}-{}.json", name, std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn loads_and_validates_a_well_formed_script() {
        let path = write_temp(
            "ok",
            r#"{
                "name": "tidy downloads",
                "steps": [
                    { "label": "move the pdf", "action": "fs.move",
                      "params": { "source_path": "~/Downloads/x.pdf", "target_path": "~/Documents/" } },
                    { "action": "shell.run", "params": { "command": "echo done" } }
                ]
            }"#,
        );
        let script = WorkflowScript::load(&path).unwrap();
        assert_eq!(script.name, "tidy downloads");
        assert_eq!(script.steps.len(), 2);
        // params defaulting + caller optionality
        assert!(script.steps[1].caller.is_none());
        assert!(script.steps[0].params.is_object());
        let plan = script.plan_summary();
        assert!(plan.contains("1. fs.move — move the pdf"));
        assert!(plan.contains("2. shell.run"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn step_without_params_defaults_to_empty_object() {
        let path = write_temp(
            "noparams",
            r#"{ "name": "w", "steps": [ { "action": "daemon.ping" } ] }"#,
        );
        let script = WorkflowScript::load(&path).unwrap();
        assert_eq!(script.steps[0].params, serde_json::json!({}));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_steps_is_rejected() {
        let path = write_temp("empty", r#"{ "name": "w", "steps": [] }"#);
        assert!(WorkflowScript::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_action_is_rejected() {
        let path = write_temp(
            "emptyaction",
            r#"{ "name": "w", "steps": [ { "action": "  " } ] }"#,
        );
        assert!(WorkflowScript::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn non_object_params_is_rejected() {
        let path = write_temp(
            "badparams",
            r#"{ "name": "w", "steps": [ { "action": "x", "params": [1,2,3] } ] }"#,
        );
        assert!(WorkflowScript::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn when_branching_logic() {
        assert!(When::Always.should_run(None));
        assert!(When::Always.should_run(Some(false)));
        assert!(When::OnSuccess.should_run(Some(true)));
        assert!(!When::OnSuccess.should_run(Some(false)));
        assert!(!When::OnSuccess.should_run(None));
        assert!(When::OnFailure.should_run(Some(false)));
        assert!(!When::OnFailure.should_run(Some(true)));
    }

    #[test]
    fn when_defaults_to_always_and_parses() {
        let path = write_temp(
            "when",
            r#"{ "name": "w", "steps": [ { "action": "a" }, { "action": "b", "when": "on_failure" } ] }"#,
        );
        let script = WorkflowScript::load(&path).unwrap();
        assert_eq!(script.steps[0].when, When::Always);
        assert_eq!(script.steps[1].when, When::OnFailure);
        let _ = std::fs::remove_file(&path);
    }
}
