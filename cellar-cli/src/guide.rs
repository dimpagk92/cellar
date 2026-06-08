//! WS15 — `cellar learn` + `cellar tools`: agent ergonomics.
//!
//! `learn` prints an embeddable guide so an agent (or a human wiring one up)
//! knows how to drive CEL. `tools` lists the surfaces: the human CLI verbs, the
//! native action vocabulary dispatched through the governed gateway, and the
//! MCP tools the cellar server exposes. Both are static prints — no daemon.

use anyhow::Result;
use serde_json::json;

/// Embeddable CEL agent guide.
pub const AGENT_GUIDE: &str = r#"# Driving CEL (Cellar)

CEL is the trust + execution layer for AI-operated computers: reliable eyes,
hands, verification, and receipts over the macOS desktop.

## The loop: See → Act → Verify → Receipt
1. SEE first. Read the screen (MCP `cel_see`, or `cellar see` once wired) before
   acting — never act blind.
2. ACT through the governed gateway. Every mutating action is rule-checked and
   returns an execution *receipt* (dispatch path, verification, evidence).
3. VERIFY. Confirm the expected post-state with a fresh observation or an
   adapter read-back.
4. Cite the receipt + the verifying evidence in your answer.

For multi-step / volatile tasks, prefer the warm Cortex (`cel_perceive`) over
re-polling `cel_see` every step.

## Two surfaces, same trust model
- MCP (warm, stateful, governed): cel_see / cel_act / cel_think / cel_perceive
  + memory (cel_remember / cel_recall / cel_forget). Best for agent loops.
- CLI (composable, one-shot): `cellar <verb>`. Best for scripts and pipelines.
  Every action verb routes through the SAME governed gateway as the MCP path.

## CLI quickstart
  cellar click <id>                 # click an element
  cellar type "text" --target <id>  # type (optionally focus first)
  cellar app Safari                 # activate an app
  cellar window left_half           # tile / minimize / center
  cellar menu list                  # menu-bar extras
  cellar space list                 # virtual desktops (macOS)
  cellar run workflow.json          # run a scripted action sequence
  cellar act <type> -a <json>       # raw governed action
  cellar mcp inspect <server>       # call OTHER MCP servers

Run `cellar tools` for the full surface, `cellar capabilities` for what the
running daemon advertises.
"#;

/// Print the embeddable agent guide.
pub fn learn() {
    print!("{AGENT_GUIDE}");
}

/// CLI verbs (the human-automation surface).
const CLI_VERBS: &[(&str, &str)] = &[
    ("click", "click an element by target id"),
    ("type", "type text, optionally focusing a target first"),
    ("app", "activate an application"),
    ("window", "window tiling / minimize / center"),
    ("menu", "menu-bar extras (list / click)"),
    ("space", "virtual desktops (macOS / SkyLight)"),
    ("run", "run a workflow script of actions"),
    ("act", "submit a raw governed action"),
    ("mcp", "call other MCP servers (CEL as a client)"),
];

/// Native action verbs — the cel_act vocabulary, executed through the gateway.
const NATIVE_ACTIONS: &[&str] = &[
    "click",
    "type",
    "key",
    "key_combo",
    "set_value",
    "scroll",
    "drag",
    "wait",
    "ax_action",
    "activate_app",
    "select",
    "navigate",
    "cdp_eval",
    "extract",
    "window",
    "dialog",
    "dock",
    "menu_extra",
];

/// MCP tools exposed by the cellar MCP *server*.
const MCP_TOOLS: &[(&str, &str)] = &[
    ("cel_see", "read the current screen state"),
    ("cel_act", "execute actions + return receipts"),
    ("cel_think", "planning / autonomy / knowledge"),
    ("cel_perceive", "warm stateful perception (Cortex)"),
    ("cel_remember", "persist a memory chunk"),
    ("cel_recall", "hybrid retrieve over memory"),
    ("cel_forget", "delete memory chunks"),
];

/// Print the tool/verb inventory (native CLI + gateway actions + MCP tools).
pub fn tools(json_out: bool) -> Result<()> {
    if json_out {
        let payload = json!({
            "cli_verbs": CLI_VERBS.iter().map(|(n, d)| json!({"name": n, "desc": d})).collect::<Vec<_>>(),
            "native_actions": NATIVE_ACTIONS,
            "mcp_tools": MCP_TOOLS.iter().map(|(n, d)| json!({"name": n, "desc": d})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("CLI verbs (cellar <verb>):");
    for (name, desc) in CLI_VERBS {
        println!("  {name:<10} {desc}");
    }
    println!("\nNative action types (cel_act / cellar act, governed via the gateway):");
    println!("  {}", NATIVE_ACTIONS.join(", "));
    println!("\nMCP tools (cellar MCP server):");
    for (name, desc) in MCP_TOOLS {
        println!("  {name:<14} {desc}");
    }
    println!("\nFor daemon-advertised capabilities, run `cellar capabilities`.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_is_substantive() {
        assert!(AGENT_GUIDE.contains("See → Act → Verify → Receipt"));
        assert!(AGENT_GUIDE.contains("cellar run"));
    }

    #[test]
    fn tools_inventory_is_populated() {
        assert!(!CLI_VERBS.is_empty());
        assert!(NATIVE_ACTIONS.contains(&"menu_extra"));
        assert!(MCP_TOOLS.iter().any(|(n, _)| *n == "cel_see"));
    }
}
