use std::collections::{HashMap, HashSet};

use cel_context::{ContextElement, ScreenContext};
use napi_derive::napi;

const PLANNING_CONTEXT_EXTRA_BUDGET: usize = 12;
const PLANNING_ANCESTOR_LIMIT: usize = 3;
const PLANNING_SIBLING_LIMIT: usize = 2;

struct PromptTierConfig {
    context_detail: cel_planner::ContextDetail,
    max_steps: u32,
    max_elements: usize,
    max_history_steps: usize,
    max_network_events: usize,
}

fn resolve_prompt_tier(
    provider: Option<&str>,
    model: Option<&str>,
    max_steps: Option<u32>,
) -> PromptTierConfig {
    let model_id = model
        .map(|m| m.to_string())
        .or_else(|| provider.map(|p| cel_llm::ProviderKind::from(p).default_model().to_string()))
        .unwrap_or_default();
    let profile = cel_llm::ModelProfile::from_model_id(&model_id);
    match profile.tier {
        cel_llm::ModelTier::Flash => PromptTierConfig {
            context_detail: cel_planner::ContextDetail::ActionableOnly,
            max_steps: max_steps.unwrap_or(20),
            max_elements: 20,
            max_history_steps: 15,
            max_network_events: 0,
        },
        cel_llm::ModelTier::Standard => PromptTierConfig {
            context_detail: cel_planner::ContextDetail::Tree,
            max_steps: max_steps.unwrap_or(30),
            max_elements: 60,
            max_history_steps: 25,
            max_network_events: 5,
        },
        cel_llm::ModelTier::Premium => PromptTierConfig {
            context_detail: cel_planner::ContextDetail::Tree,
            max_steps: max_steps.unwrap_or(50),
            max_elements: 80,
            max_history_steps: 25,
            max_network_events: 10,
        },
    }
}

fn is_stateful_anchor(element: &ContextElement) -> bool {
    element.state.focused
        || element.state.selected
        || element.state.checked.is_some()
        || element.state.expanded.is_some()
}

fn is_neighbor_anchor(element: &ContextElement) -> bool {
    element.state.visible
        && (!element.actions.is_empty()
            || is_stateful_anchor(element)
            || element
                .label
                .as_deref()
                .map(|label| !label.is_empty())
                .unwrap_or(false))
}

fn is_page_text(element: &ContextElement) -> bool {
    element.id.contains("page-text") || element.id.contains("page_text")
}

fn push_selected_id(
    id: &str,
    seen: &mut HashSet<String>,
    selected_ids: &mut Vec<String>,
    cap: usize,
) {
    if selected_ids.len() >= cap {
        return;
    }
    if seen.insert(id.to_string()) {
        selected_ids.push(id.to_string());
    }
}

fn build_planning_context(
    context: &ScreenContext,
    goal: &str,
    max_elements: usize,
) -> ScreenContext {
    if context.elements.len() <= max_elements {
        return context.clone();
    }

    let target = max_elements.max(10);
    let cap = target.saturating_add(PLANNING_CONTEXT_EXTRA_BUDGET);
    let all_by_id: HashMap<&str, &ContextElement> = context
        .elements
        .iter()
        .map(|element| (element.id.as_str(), element))
        .collect();
    let distilled = cel_planner::distiller::distill_for_goal(context, goal, target);

    let mut seen = HashSet::new();
    let mut selected_ids = Vec::new();

    for element in &distilled {
        push_selected_id(&element.id, &mut seen, &mut selected_ids, cap);

        let mut current_parent = element.parent_id.as_deref();
        let mut depth = 0;
        while let Some(parent_id) = current_parent {
            if depth >= PLANNING_ANCESTOR_LIMIT {
                break;
            }
            push_selected_id(parent_id, &mut seen, &mut selected_ids, cap);
            current_parent = all_by_id
                .get(parent_id)
                .and_then(|parent| parent.parent_id.as_deref());
            depth += 1;
        }

        if let Some(parent_id) = element.parent_id.as_deref() {
            for sibling in context
                .elements
                .iter()
                .filter(|candidate| {
                    candidate.id != element.id
                        && candidate.parent_id.as_deref() == Some(parent_id)
                        && is_neighbor_anchor(candidate)
                })
                .take(PLANNING_SIBLING_LIMIT)
            {
                push_selected_id(&sibling.id, &mut seen, &mut selected_ids, cap);
            }
        }
    }

    for element in &context.elements {
        if is_stateful_anchor(element) || is_page_text(element) {
            push_selected_id(&element.id, &mut seen, &mut selected_ids, cap);
        }
    }

    if selected_ids.len() < target {
        for element in &context.elements {
            if element.state.visible && (!element.actions.is_empty() || is_page_text(element)) {
                push_selected_id(&element.id, &mut seen, &mut selected_ids, cap);
            }
            if selected_ids.len() >= target {
                break;
            }
        }
    }

    let elements = context
        .elements
        .iter()
        .filter(|element| seen.contains(element.id.as_str()))
        .cloned()
        .collect();

    ScreenContext {
        elements,
        ..context.clone()
    }
}

/// Plan a single step given a goal, current context, and step history.
/// Returns a JSON PlannedStep: { reasoning, action, expected_outcome, confidence }.
///
/// The caller runs the loop in TypeScript, calling this function per iteration
/// with fresh context and accumulated history.
#[napi]
pub async fn plan_step(
    goal: String,
    context_json: String,
    history_json: String,
    provider: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    max_tokens: Option<u32>,
    max_steps: Option<u32>,
    loop_warning: Option<String>,
    device_baseline_json: Option<String>,
) -> napi::Result<String> {
    let llm = crate::llm::build_llm_client(
        provider.clone(),
        api_key,
        model.clone(),
        endpoint,
        Some(cel_llm::LlmRole::Planner),
    )?;

    let context: cel_context::ScreenContext = serde_json::from_str(&context_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid context JSON: {}", e)))?;

    let history_records: Vec<cel_planner::StepRecord> =
        serde_json::from_str(&history_json).unwrap_or_default();
    let step_count = history_records.len() as u32;
    let history = cel_planner::history::StepHistory::from_records(history_records);

    let tier = resolve_prompt_tier(provider.as_deref(), model.as_deref(), max_steps);

    let planning_context = build_planning_context(&context, &goal, tier.max_elements);

    // Detect task type and page state for composable prompt
    let task_type = cel_planner::prompt::detect_task_type(&goal);
    let page_state = cel_planner::prompt::analyze_page_state(&context);
    let system = cel_planner::prompt::build_composable_system_prompt(
        device_baseline_json.as_deref(),
        task_type,
        Some(&page_state),
    );
    let opts = cel_planner::prompt::PromptOptions {
        step_index: step_count,
        max_steps: tier.max_steps,
        loop_warning: loop_warning.as_deref(),
        context_detail: tier.context_detail,
        max_elements: planning_context.elements.len(),
        max_history_steps: tier.max_history_steps,
        max_network_events: tier.max_network_events,
        ..Default::default()
    };
    let prompt_result =
        cel_planner::prompt::build_user_prompt(&goal, &planning_context, &history, &opts);

    let raw = llm
        .complete(&system, &prompt_result.text, max_tokens.unwrap_or(8192))
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let cleaned = cel_llm::strip_code_fences(&raw);
    let mut step: cel_planner::PlannedStep = serde_json::from_str(cleaned).map_err(|e| {
        napi::Error::from_reason(format!(
            "LLM output parse error: {}. Raw[0..2000]: {}",
            e,
            &raw[..raw.len().min(2000)]
        ))
    })?;

    // Resolve numbered indices back to real element IDs
    cel_planner::prompt::resolve_action_indices(&mut step.action, &prompt_result.index_map);

    serde_json::to_string(&step).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Build the system + user prompts for planning WITHOUT calling the LLM.
/// Returns JSON: { "system": "...", "user": "..." }
///
/// Use this to get the exact prompts that `plan_step` would use, then call
/// `llm_complete_with_image` separately with a screenshot attached.
#[napi]
pub fn build_plan_prompt(
    goal: String,
    context_json: String,
    history_json: String,
    max_steps: Option<u32>,
    loop_warning: Option<String>,
    provider: Option<String>,
    model: Option<String>,
) -> napi::Result<String> {
    let context: cel_context::ScreenContext = serde_json::from_str(&context_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid context JSON: {}", e)))?;

    let history_records: Vec<cel_planner::StepRecord> =
        serde_json::from_str(&history_json).unwrap_or_default();
    let step_count = history_records.len() as u32;
    let history = cel_planner::history::StepHistory::from_records(history_records);

    let tier = resolve_prompt_tier(provider.as_deref(), model.as_deref(), max_steps);

    let planning_context = build_planning_context(&context, &goal, tier.max_elements);

    let system = cel_planner::prompt::system_prompt();
    let opts = cel_planner::prompt::PromptOptions {
        step_index: step_count,
        max_steps: tier.max_steps,
        loop_warning: loop_warning.as_deref(),
        context_detail: tier.context_detail,
        max_elements: planning_context.elements.len(),
        max_history_steps: tier.max_history_steps,
        max_network_events: tier.max_network_events,
        ..Default::default()
    };
    let prompt_result =
        cel_planner::prompt::build_user_prompt(&goal, &planning_context, &history, &opts);

    // Include index_map in output so the TypeScript caller can resolve indices
    let index_map_json: Vec<&str> = prompt_result.index_map.iter().map(|s| s.as_str()).collect();
    serde_json::to_string(&serde_json::json!({
        "system": system,
        "user": prompt_result.text,
        "index_map": index_map_json,
    }))
    .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Plan a step WITHOUT screen context (blind mode).
/// Uses device baseline (OS, shortcuts, installed apps) instead of element table.
/// Much faster — no accessibility tree walk needed.
#[napi]
pub async fn plan_step_blind(
    goal: String,
    history_json: String,
    device_baseline_json: String,
    max_steps: Option<u32>,
    loop_warning: Option<String>,
) -> napi::Result<String> {
    let llm =
        crate::llm::build_llm_client(None, None, None, None, Some(cel_llm::LlmRole::Planner))?;

    let history_records: Vec<cel_planner::StepRecord> =
        serde_json::from_str(&history_json).unwrap_or_default();
    let step_count = history_records.len() as u32;
    let history = cel_planner::history::StepHistory::from_records(history_records);

    let system = cel_planner::prompt::system_prompt_blind(&device_baseline_json);
    let opts = cel_planner::prompt::PromptOptions {
        step_index: step_count,
        max_steps: max_steps.unwrap_or(30),
        loop_warning: loop_warning.as_deref(),
        ..Default::default()
    };
    let user_prompt = cel_planner::prompt::build_user_prompt_blind(&goal, &history, &opts);

    let raw = llm
        .complete(&system, &user_prompt, 1024)
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let cleaned = cel_llm::strip_code_fences(&raw);
    let step: cel_planner::PlannedStep = serde_json::from_str(cleaned).map_err(|e| {
        napi::Error::from_reason(format!(
            "Blind planner parse error: {}. Raw: {}",
            e,
            &raw[..raw.len().min(500)]
        ))
    })?;

    serde_json::to_string(&step).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Decompose a complex goal into advisory milestones.
/// Returns a JSON string with feasibility assessment and milestone list.
#[napi]
pub async fn decompose_goal(
    goal: String,
    context_json: String,
    total_step_budget: u32,
    history_advice: Option<String>,
) -> napi::Result<String> {
    let context: cel_context::ScreenContext = serde_json::from_str(&context_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid context JSON: {}", e)))?;

    let system = cel_planner::decompose::build_decomposition_prompt();
    let element_summary = context
        .elements
        .iter()
        .take(20)
        .map(|e| {
            format!(
                "{}: {} \"{}\"",
                e.id,
                e.element_type,
                e.label.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    let user = cel_planner::decompose::build_decomposition_user_prompt(
        &goal,
        &context.app,
        &context.window,
        &element_summary,
        total_step_budget,
        history_advice.as_deref(),
    );

    let client =
        crate::llm::build_llm_client(None, None, None, None, Some(cel_llm::LlmRole::Planner))?;

    let raw = client
        .complete(&system, &user, 2048)
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let result = cel_planner::decompose::parse_decomposition(&raw)
        .map_err(|e| napi::Error::from_reason(format!("Decomposition parse error: {}", e)))?;

    serde_json::to_string(&result).map_err(|e| napi::Error::from_reason(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use cel_accessibility::ElementState;
    use cel_context::{ContentRole, ContextSource};

    fn make_element(id: &str, element_type: &str, label: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.to_string(),
            label: label.map(|value| value.to_string()),
            description: None,
            element_type: element_type.to_string(),
            value: None,
            bounds: None,
            state: ElementState {
                visible: true,
                enabled: true,
                ..ElementState::default()
            },
            parent_id: None,
            actions: Vec::new(),
            confidence: 0.9,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::Interactive,
            properties: HashMap::new(),
        }
    }

    fn make_context(elements: Vec<ContextElement>) -> ScreenContext {
        ScreenContext {
            app: "Browser".into(),
            window: "Results".into(),
            elements,
            network_events: Vec::new(),
            http_events: Vec::new(),
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: Vec::new(),
            audio: None,
            power: None,
            running_apps: Vec::new(),
            recent_files: Vec::new(),
            transcripts: Vec::new(),
        }
    }

    #[test]
    fn planning_context_keeps_goal_hits_and_stateful_anchors() {
        let mut page_text = make_element("page-text", "text", Some("Price $42 and shipping info"));
        page_text.content_role = ContentRole::Content;
        page_text.actions = Vec::new();

        let mut focused = make_element("search", "input", Some("Search"));
        focused.actions = vec!["set_value".into()];
        focused.state.focused = true;

        let mut target = make_element("target", "button", Some("Checkout"));
        target.actions = vec!["click".into()];
        target.parent_id = Some("toolbar".into());

        let toolbar = make_element("toolbar", "group", Some("Actions"));
        let sibling = {
            let mut element = make_element("sibling", "button", Some("Cancel"));
            element.actions = vec!["click".into()];
            element.parent_id = Some("toolbar".into());
            element
        };

        let noise = (0..80).map(|index| {
            let mut element = make_element(
                &format!("noise-{index}"),
                "text",
                Some("irrelevant content"),
            );
            element.content_role = ContentRole::Content;
            element
        });

        let mut elements = vec![page_text, focused, toolbar, target, sibling];
        elements.extend(noise);

        let distilled = build_planning_context(&make_context(elements), "click checkout", 20);
        let ids: HashSet<&str> = distilled
            .elements
            .iter()
            .map(|element| element.id.as_str())
            .collect();

        assert!(ids.contains("target"));
        assert!(ids.contains("toolbar"));
        assert!(ids.contains("sibling"));
        assert!(ids.contains("search"));
        assert!(ids.contains("page-text"));
        assert!(distilled.elements.len() <= 20 + PLANNING_CONTEXT_EXTRA_BUDGET);
    }
}
