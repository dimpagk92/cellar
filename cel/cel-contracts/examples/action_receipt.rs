//! Planner/runtime action and receipt contract.
//!
//! Run with: `cargo run -p cel-contracts --example action_receipt`

use cel_contracts::{
    DispatchRoute, EffectExpectation, ExecutionReceipt, ObservedEffect, PlannedAction,
    ReceiptStatus,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let action = PlannedAction::Click {
        target_id: "dom:button:deploy".into(),
        expect_after: Some(EffectExpectation::SelectorTextContains {
            selector: "#status".into(),
            substring: "Deploy started".into(),
            timeout_ms: 2_000,
        }),
    };

    let receipt = ExecutionReceipt {
        receipt_id: "receipt-001".into(),
        run_id: Some("run-42".into()),
        trace_id: Some("trace-abc".into()),
        action_kind: "click".into(),
        target: Some("dom:button:deploy".into()),
        route: DispatchRoute::Cdp,
        observed_effect: ObservedEffect::selector_observed(
            EffectExpectation::SelectorTextContains {
                selector: "#status".into(),
                substring: "Deploy started".into(),
                timeout_ms: 2_000,
            },
        ),
        evidence: Vec::new(),
        requested_at_ms: 1_700_000_000_000,
        completed_at_ms: 1_700_000_000_120,
        duration_ms: 120,
        status: ReceiptStatus::Ok,
        error: None,
    };

    println!(
        "planned action:\n{}",
        serde_json::to_string_pretty(&action)?
    );
    println!(
        "\nexecution receipt:\n{}",
        serde_json::to_string_pretty(&receipt)?
    );
    Ok(())
}
