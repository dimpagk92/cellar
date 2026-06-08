//! Unit tests for the planning-view builder and its selectors.

use super::elements::compress;
use super::*;
use cel_context::ElementState;

fn make_el(id: &str, ty: &str, label: Option<&str>) -> ContextElement {
    ContextElement {
        id: id.into(),
        label: label.map(String::from),
        description: None,
        element_type: ty.into(),
        value: None,
        bounds: None,
        state: ElementState {
            visible: true,
            enabled: true,
            ..Default::default()
        },
        parent_id: None,
        actions: vec![],
        confidence: 1.0,
        source: cel_context::ContextSource::AccessibilityTree,
        content_role: cel_context::ContentRole::Interactive,
        properties: Default::default(),
    }
}

fn make_context(elements: Vec<ContextElement>) -> ScreenContext {
    ScreenContext {
        app: "Browser".into(),
        window: "Test".into(),
        elements,
        network_events: vec![],
        http_events: vec![],
        timestamp_ms: 0,
        screen_width: None,
        screen_height: None,
        clipboard: None,
        window_list: vec![],
        audio: None,
        power: None,
        running_apps: vec![],
        recent_files: vec![],
        transcripts: vec![],
    }
}

#[test]
fn budget_caps_element_count_and_records_omitted() {
    let elements: Vec<ContextElement> = (0..200)
        .map(|i| make_el(&format!("e{i}"), "button", Some("Submit form")))
        .collect();
    let perception = make_context(elements);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget {
        max_elements: 30,
        ..Default::default()
    };
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit the form",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.elements.len(), 30);
    assert_eq!(view.omitted_counts.elements, 170);
}

#[test]
fn goal_relevant_elements_outrank_irrelevant_ones() {
    let mut elements = Vec::new();
    for i in 0..20 {
        elements.push(make_el(&format!("noise{i}"), "button", Some("Open menu")));
    }
    elements.push(make_el("submit-1", "button", Some("Submit Invoice")));
    elements.push(make_el("submit-2", "button", Some("Save Draft")));

    let perception = make_context(elements);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget {
        max_elements: 5,
        ..Default::default()
    };
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit the invoice",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    let ids: Vec<&str> = view.elements.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains(&"submit-1"),
        "submit-related element should rank in top-5; got {ids:?}"
    );
}

#[test]
fn caps_fold_into_capabilities_and_run_progress() {
    let perception = make_context(vec![]);
    let caps = RuntimeCaps {
        cdp_bound: true,
        cdp_browser: Some("Google Chrome".into()),
        cdp_url: Some("https://example.com".into()),
        native_input: true,
        steps_used: 13,
        max_steps: 80,
    };
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "anything",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    assert_eq!(view.run_progress.steps_used, 13);
    assert_eq!(view.run_progress.max_steps, 80);
    assert_eq!(view.run_progress.steps_remaining(), 67);

    assert!(view.capabilities.iter().any(|c| c.id == "cdp_bound"));
    assert!(view.capabilities.iter().any(|c| c.id == "native_input"));
    assert_eq!(view.screen.url.as_deref(), Some("https://example.com"));
}

#[test]
fn empty_perception_yields_empty_view() {
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "do nothing",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.elements.len(), 0);
    assert_eq!(view.omitted_counts.elements, 0);
    assert!(view.capabilities.is_empty());
}

#[test]
fn invisible_elements_are_filtered_before_scoring() {
    let mut visible = make_el("visible", "button", Some("Submit"));
    let mut hidden = make_el("hidden", "button", Some("Submit"));
    hidden.state.visible = false;
    visible.state.visible = true;

    let perception = make_context(vec![hidden, visible]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget {
        max_elements: 10,
        ..Default::default()
    };
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.elements.iter().all(|e| e.id != "hidden"));
}

// ─── PR3 + WK4: memory-aware hydration via CortexMemoryStore trait ──────

/// Build an in-memory `CelStore` wrapped in `Mutex` so it satisfies the
/// `CortexMemoryStore` trait's `Send + Sync` bound. Replaces the
/// PR3-era temp-file pattern: faster (no disk IO), no cleanup needed,
/// and exercises the same code path the canonical runner uses in
/// production (open once, share `&Mutex<CelStore>`).
fn fresh_store() -> std::sync::Mutex<cel_store::CelStore> {
    std::sync::Mutex::new(cel_store::CelStore::open_memory().expect("open in-memory CelStore"))
}

fn seed_memory(
    store: &std::sync::Mutex<cel_store::CelStore>,
    workflow: &str,
    kind: cel_store::cortex_memory::MemoryKind,
    summary: &str,
    content: serde_json::Value,
) -> i64 {
    store
        .lock()
        .expect("seed: store mutex poisoned")
        .insert_cortex_memory(&cel_store::cortex_memory::NewCortexMemory {
            workflow_id: workflow.into(),
            kind,
            content,
            summary: Some(summary.into()),
            tags: vec![],
            source_ref: None,
            embedding: None,
        })
        .expect("insert")
}

#[test]
fn pr3_view_stays_empty_when_memory_inputs_missing() {
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.memories.is_empty());
    assert_eq!(view.omitted_counts.memories, 0);
    assert!(view.selection_rationale.is_none());
}

#[test]
fn pr3_relevant_memory_outranks_irrelevant_one() {
    let store = fresh_store();
    seed_memory(
        &store,
        "test-pr3",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur successfully",
        serde_json::json!({"goal": "submit invoice"}),
    );
    seed_memory(
        &store,
        "test-pr3",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Read morning headlines from Hacker News",
        serde_json::json!({"goal": "read news"}),
    );

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("test-pr3"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    assert!(
        !view.memories.is_empty(),
        "expected at least one hydrated memory; got 0"
    );
    // The submit-invoice memory must rank first.
    assert!(
        view.memories[0]
            .summary
            .to_lowercase()
            .contains("submitted invoice"),
        "expected submit-invoice memory first; got {:?}",
        view.memories[0].summary
    );
}

#[test]
fn pr3_budget_caps_memory_count_and_records_omitted() {
    let store = fresh_store();
    // Seed 5 memories all referencing the goal keyword "form" so each
    // scores > 0.
    for i in 0..5 {
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            &format!("Submitted form attempt {i}"),
            serde_json::json!({"i": i}),
        );
    }

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget {
        max_memories: 2,
        ..PlanningBudget::default()
    };
    let view = build_planning_view(&PlanningViewInputs {
        goal: "fill out form",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    assert_eq!(view.memories.len(), 2);
    assert_eq!(view.omitted_counts.memories, 3);
}

#[test]
fn pr3_workflow_with_no_memories_returns_empty_with_rationale() {
    let store = fresh_store();
    // Seed only OTHER workflow's memories — should not surface here.
    seed_memory(
        &store,
        "other-wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "did something",
        serde_json::json!({}),
    );

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("the-empty-workflow"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    assert!(view.memories.is_empty());
    assert_eq!(view.omitted_counts.memories, 0);
    let rationale = view.selection_rationale.expect("expected rationale");
    assert!(
        rationale.contains("No prior memories"),
        "expected empty-workflow rationale; got {rationale}"
    );
}

#[test]
fn pr3_irrelevant_memories_score_zero_and_are_dropped() {
    let store = fresh_store();
    // Fully off-topic memories with no goal-keyword overlap.
    seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Watered the plants in the kitchen",
        serde_json::json!({"plants": "many"}),
    );
    seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Rebooted the router after midnight",
        serde_json::json!({"router": "fixed"}),
    );

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    // Both candidates score 0 → both omitted, none kept.
    assert_eq!(view.memories.len(), 0);
    assert_eq!(view.omitted_counts.memories, 2);
}

/// WK4: the PR3 "store_open_failure" test no longer exists at this
/// layer — the builder no longer opens the store, so open failure
/// can't happen here. The equivalent failure surface is now in the
/// canonical runner: when `RunLimits.memory_db_path` points at a bad
/// file, the runner logs and proceeds with `memory_store: None` (see
/// `pr2_outcome_*` tests + the new `wk4_open_failure_at_runner_*`
/// tests in `cel-goal-runner::canonical_runner::tests`).
///
/// We keep a thin trait-level analogue here: a store impl that
/// always errors must produce an empty view, not a panic.
#[test]
fn wk4_store_read_failure_returns_empty_view_no_panic() {
    struct AlwaysErrStore;
    impl cel_store::CortexMemoryStore for AlwaysErrStore {
        fn list_for_workflow(
            &self,
            _: &str,
            _: Option<&[cel_store::cortex_memory::MemoryKind]>,
            _: usize,
        ) -> Result<Vec<cel_store::cortex_memory::CortexMemory>, cel_store::StoreError> {
            Err(cel_store::StoreError::NotFound("simulated".into()))
        }
        fn insert_memory(
            &self,
            _: &cel_store::cortex_memory::NewCortexMemory,
        ) -> Result<i64, cel_store::StoreError> {
            Err(cel_store::StoreError::NotFound("simulated".into()))
        }
        fn search_for_workflow_ranked(
            &self,
            _: &str,
            _: &str,
            _: usize,
        ) -> Result<Vec<cel_store::cortex_memory::CortexMemory>, cel_store::StoreError> {
            Err(cel_store::StoreError::NotFound("simulated".into()))
        }
    }
    let store = AlwaysErrStore;
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    // Goal with usable keywords so FTS5 path is exercised — and
    // the fallback `list_for_workflow` also returns Err. Both reads
    // fail; the view must still build with empty memories.
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice via Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.memories.is_empty());
}

#[test]
fn pr3_quoted_phrase_in_goal_boosts_matching_memory() {
    let store = fresh_store();
    // Two memories — one matches the quoted phrase exactly.
    seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Prior,
        "Concur uses two-step submit",
        serde_json::json!({"app": "Concur"}),
    );
    seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Prior,
        "Some unrelated submit notes",
        serde_json::json!({}),
    );

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit \"two-step submit\" form",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    assert!(view.memories[0].summary.contains("two-step"));
}

// ─── Tier A1: knowledge hydration ────────────────────────────────────────

fn seed_knowledge(
    store: &std::sync::Mutex<cel_store::CelStore>,
    content: &str,
    source: &str,
) -> i64 {
    store
        .lock()
        .expect("seed: store mutex poisoned")
        .add_knowledge(content, source)
        .expect("add_knowledge")
}

#[test]
fn a1_knowledge_silent_when_store_not_provided() {
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    // PR1 perception-only behaviour preserved.
    assert!(view.knowledge.is_empty());
    assert_eq!(view.omitted_counts.knowledge, 0);
}

#[test]
fn a1_relevant_knowledge_hydrated_via_fts5_bm25() {
    let store = fresh_store();
    seed_knowledge(
        &store,
        "Concur uses a two-step submit: first 'Save', then 'Submit'.",
        "manual",
    );
    seed_knowledge(
        &store,
        "Espresso machine descaling guide step by step.",
        "wiki",
    );
    seed_knowledge(
        &store,
        "Submit button on payroll forms requires a separate confirmation.",
        "ops_doc",
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: Some(&store),
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    // Two facts mention "submit" / "Concur" tokens; the espresso
    // one doesn't. FTS5 returns only the matching pair.
    assert_eq!(view.knowledge.len(), 2);
    assert!(view
        .knowledge
        .iter()
        .all(|k| !k.content.contains("Espresso")));
}

#[test]
fn a1_knowledge_capped_by_budget_and_records_omitted() {
    let store = fresh_store();
    for i in 0..6 {
        seed_knowledge(
            &store,
            &format!("Submit step number {i} in the workflow."),
            "manual",
        );
    }
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget {
        max_knowledge: 3,
        ..PlanningBudget::default()
    };
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit step",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: Some(&store),
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.knowledge.len(), 3);
    assert_eq!(view.omitted_counts.knowledge, 3);
}

#[test]
fn a1_no_match_yields_empty_knowledge() {
    let store = fresh_store();
    seed_knowledge(
        &store,
        "Espresso machine descaling guide step by step.",
        "wiki",
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: Some(&store),
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.knowledge.len(), 0);
    // 0 candidates returned by FTS5 → 0 omitted (we never had them).
    assert_eq!(view.omitted_counts.knowledge, 0);
}

#[test]
fn a1_no_keywords_in_goal_yields_empty_knowledge() {
    // Goal has only stop words → safe_fts5_query_from_keywords
    // returns None → selector exits early, never queries the store.
    let store = fresh_store();
    seed_knowledge(&store, "important fact about anything", "doc");
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "do it",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: Some(&store),
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.knowledge.is_empty());
    assert_eq!(view.omitted_counts.knowledge, 0);
}

#[test]
fn a1_rationale_mentions_knowledge_when_hydrated() {
    let store = fresh_store();
    seed_knowledge(
        &store,
        "Concur submit button is on the upper right",
        "manual",
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: Some(&store),
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    let rationale = view.selection_rationale.expect("expected rationale");
    assert!(
        rationale.contains("knowledge"),
        "expected rationale to mention knowledge; got {rationale}"
    );
}

#[test]
fn a1_store_read_failure_returns_empty_knowledge_no_panic() {
    struct AlwaysErrKnowledge;
    impl cel_store::KnowledgeStore for AlwaysErrKnowledge {
        fn search_knowledge_for_workflow(
            &self,
            _: &str,
            _: Option<&str>,
            _: usize,
        ) -> Result<Vec<cel_store::ScoredKnowledge>, cel_store::StoreError> {
            Err(cel_store::StoreError::NotFound("simulated".into()))
        }
    }
    let store = AlwaysErrKnowledge;
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: Some(&store),
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.knowledge.is_empty());
}

#[test]
fn a1_memory_and_knowledge_can_coexist() {
    // Same Mutex<CelStore> handle satisfies BOTH traits — this is
    // the production canonical-runner shape. Verify both selectors
    // run, both surface, both rationale lines appear.
    let store = fresh_store();
    seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur successfully",
        serde_json::json!({}),
    );
    seed_knowledge(&store, "Concur submit requires manager approval", "ops_doc");

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: Some(&store),
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.memories.len(), 1);
    assert_eq!(view.knowledge.len(), 1);
    let rationale = view.selection_rationale.expect("expected rationale");
    assert!(rationale.contains("workflow memor"));
    assert!(rationale.contains("knowledge fact"));
}

// ─── Tier A2: recent_events from observations ────────────────────────────

fn seed_observation(
    store: &std::sync::Mutex<cel_store::CelStore>,
    workflow_name: &str,
    content: &str,
    priority: cel_store::ObservationPriority,
) -> i64 {
    store
        .lock()
        .expect("seed: store mutex poisoned")
        .add_observation(workflow_name, content, &priority, &[], None, None)
        .expect("add_observation")
}

#[test]
fn a2_recent_events_silent_when_store_not_provided() {
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    // PR1 perception-only behaviour preserved.
    assert!(view.recent_events.is_empty());
    assert_eq!(view.omitted_counts.recent_events, 0);
}

#[test]
fn a2_recent_events_silent_when_workflow_id_missing() {
    // Observations are workflow-scoped via workflow_name; without
    // a workflow_id we can't pick them. Silent rather than wrong.
    let store = fresh_store();
    seed_observation(
        &store,
        "wf",
        "noise",
        cel_store::ObservationPriority::Medium,
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None, // <-- key
        goal_embedding: None,
        recent_events_store: Some(&store),
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.recent_events.is_empty());
}

#[test]
fn a2_recent_events_hydrate_in_priority_then_recency_order() {
    let store = fresh_store();
    // Insert in mixed order; underlying ORDER BY in get_observations
    // surfaces high → medium → low, then created_at DESC within
    // priority. Observations are independent of goal keywords —
    // they're curated summaries, not keyword-search candidates.
    seed_observation(
        &store,
        "wf",
        "low priority older",
        cel_store::ObservationPriority::Low,
    );
    seed_observation(
        &store,
        "wf",
        "medium priority middle",
        cel_store::ObservationPriority::Medium,
    );
    seed_observation(
        &store,
        "wf",
        "high priority newest",
        cel_store::ObservationPriority::High,
    );
    seed_observation(
        &store,
        "wf",
        "high priority second-newest",
        cel_store::ObservationPriority::High,
    );

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any goal",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: Some(&store),
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.recent_events.len(), 4);
    // High-priority pair surface first (most-recent within priority
    // first), then medium, then low.
    assert!(view.recent_events[0].kind.contains("high"));
    assert!(view.recent_events[1].kind.contains("high"));
    assert!(view.recent_events[2].kind.contains("medium"));
    assert!(view.recent_events[3].kind.contains("low"));
}

#[test]
fn a2_recent_events_capped_by_budget_and_records_omitted() {
    let store = fresh_store();
    for i in 0..7 {
        seed_observation(
            &store,
            "wf",
            &format!("note {i}"),
            cel_store::ObservationPriority::Medium,
        );
    }
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget {
        max_recent_events: 3,
        ..PlanningBudget::default()
    };
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: Some(&store),
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.recent_events.len(), 3);
    assert_eq!(view.omitted_counts.recent_events, 4);
}

#[test]
fn a2_event_ref_id_and_kind_format_is_stable() {
    let store = fresh_store();
    let id = seed_observation(
        &store,
        "wf",
        "important",
        cel_store::ObservationPriority::High,
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: Some(&store),
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.recent_events.len(), 1);
    let ev = &view.recent_events[0];
    // ID format pinned so future tooling can parse it back.
    assert_eq!(ev.id, format!("obs:{id}"));
    // Kind includes priority for planner weighting.
    assert_eq!(ev.kind, "observation:high");
    assert_eq!(ev.summary, "important");
    // `at` populated from observed_at or created_at — both nullable
    // at insert; defaults to created_at fallback.
    assert!(ev.at.is_some(), "expected non-empty at timestamp");
}

#[test]
fn a2_rationale_mentions_recent_events_when_hydrated() {
    let store = fresh_store();
    seed_observation(
        &store,
        "wf",
        "anything",
        cel_store::ObservationPriority::Medium,
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: Some(&store),
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    let rationale = view.selection_rationale.expect("expected rationale");
    assert!(
        rationale.contains("recent event"),
        "expected rationale to mention recent_events; got {rationale}"
    );
}

#[test]
fn a2_store_read_failure_returns_empty_no_panic() {
    struct AlwaysErrEvents;
    impl cel_store::RecentEventStore for AlwaysErrEvents {
        fn recent_events_for_workflow(
            &self,
            _: &str,
            _: usize,
        ) -> Result<Vec<cel_store::Observation>, cel_store::StoreError> {
            Err(cel_store::StoreError::NotFound("simulated".into()))
        }
    }
    let store = AlwaysErrEvents;
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: Some(&store),
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.recent_events.is_empty());
}

// ─── Closing-gap fill: evidence + adapter_facts ─────────────────────────

#[test]
fn closing_evidence_synthesized_from_populated_memories() {
    let store = fresh_store();
    let id = seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur",
        serde_json::json!({}),
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.memories.len(), 1);
    assert_eq!(view.evidence.len(), 1);
    let ev = &view.evidence[0];
    assert_eq!(ev.source, "memory");
    assert_eq!(ev.id, id.to_string());
    assert!(ev.summary.contains("Submitted invoice"));
}

#[test]
fn closing_evidence_unions_memory_knowledge_event_adapter_sources() {
    // One of each surfaceable item; expect 4 EvidenceRefs total
    // with the right source mix.
    let store = fresh_store();
    let _ = seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur",
        serde_json::json!({}),
    );
    let _ = seed_knowledge(&store, "Concur uses two-step submit", "manual");
    let _ = seed_observation(
        &store,
        "wf",
        "saw login dialog",
        cel_store::ObservationPriority::High,
    );
    let adapter_facts = vec![cel_contracts::AdapterFactRef {
        id: None,
        adapter: "numbers".into(),
        kind: "selected_range".into(),
        payload: serde_json::json!({"sheet": "Sheet1", "range": "B2:B7"}),
    }];

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: Some(&store),
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: Some(&store),
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: Some(&adapter_facts),
    });
    // 1 memory + 1 knowledge + 1 event + 1 adapter fact → 4 evidence
    assert_eq!(view.evidence.len(), 4);
    let sources: Vec<&str> = view.evidence.iter().map(|e| e.source.as_str()).collect();
    assert!(sources.contains(&"memory"));
    assert!(sources.contains(&"knowledge"));
    assert!(sources.contains(&"observation"));
    assert!(sources.contains(&"adapter_fact"));
}

#[test]
fn closing_evidence_empty_when_nothing_selected() {
    // Pure perception-only build: no memory/knowledge/events/adapter
    // facts → empty evidence, just like before the closing-gap fix.
    // No observable behaviour change for callers that don't opt in.
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.evidence.is_empty());
    assert!(view.adapter_facts.is_empty());
}

#[test]
fn closing_adapter_facts_passthrough_into_view() {
    // Adapter facts provided to PlanningViewInputs land verbatim
    // in view.adapter_facts when they fit the budget (no reranking
    // — runner is the orchestrator, builder just hydrates).
    let facts = vec![
        cel_contracts::AdapterFactRef {
            id: None,
            adapter: "numbers".into(),
            kind: "selected_range".into(),
            payload: serde_json::json!({"range": "A1"}),
        },
        cel_contracts::AdapterFactRef {
            id: None,
            adapter: "browser".into(),
            kind: "url".into(),
            payload: serde_json::json!({"url": "https://example.com"}),
        },
    ];

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: Some(&facts),
    });
    assert_eq!(view.adapter_facts.len(), 2);
    assert_eq!(view.adapter_facts[0].adapter, "numbers");
    assert_eq!(view.adapter_facts[1].adapter, "browser");
    // Each adapter fact also contributes one evidence ref.
    let adapter_evidence: Vec<&cel_contracts::EvidenceRef> = view
        .evidence
        .iter()
        .filter(|e| e.source == "adapter_fact")
        .collect();
    assert_eq!(adapter_evidence.len(), 2);
}

#[test]
fn closing_adapter_facts_respect_budget_and_omitted_count() {
    let facts: Vec<cel_contracts::AdapterFactRef> = (0..4)
        .map(|i| cel_contracts::AdapterFactRef {
            id: None,
            adapter: "numbers".into(),
            kind: "cell".into(),
            payload: serde_json::json!({"cell": format!("A{}", i + 1)}),
        })
        .collect();

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget {
        max_adapter_facts: 2,
        ..PlanningBudget::default()
    };
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: Some(&facts),
    });

    assert_eq!(view.adapter_facts.len(), 2);
    assert_eq!(view.evidence.len(), 2);
    assert_eq!(view.omitted_counts.adapter_facts, 2);
    assert!(view
        .selection_rationale
        .as_deref()
        .unwrap_or_default()
        .contains("Hydrated 2 adapter facts"));
}

#[test]
fn closing_adapter_fact_evidence_uses_supplied_id_or_payload_hash() {
    let facts = vec![
        cel_contracts::AdapterFactRef {
            id: Some("numbers:selected:A1".into()),
            adapter: "numbers".into(),
            kind: "selected_cell".into(),
            payload: serde_json::json!({"cell": "A1"}),
        },
        cel_contracts::AdapterFactRef {
            id: None,
            adapter: "numbers".into(),
            kind: "selected_cell".into(),
            payload: serde_json::json!({"cell": "B2"}),
        },
        cel_contracts::AdapterFactRef {
            id: None,
            adapter: "numbers".into(),
            kind: "selected_cell".into(),
            payload: serde_json::json!({"cell": "C3"}),
        },
    ];

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: Some(&facts),
    });

    let ids: Vec<&str> = view.evidence.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"numbers:selected:A1"));
    assert_ne!(ids[1], "numbers:selected_cell");
    assert_ne!(ids[1], ids[2]);
}

// ─── Tier A3: anomalies + blockers ──────────────────────────────────────

fn make_anomaly(kind: AnomalyType, description: &str, element_id: Option<&str>) -> Anomaly {
    Anomaly {
        anomaly_type: kind,
        title: None,
        description: description.into(),
        timestamp: 0,
        element_ids: element_id.into_iter().map(String::from).collect(),
    }
}

fn make_freshness(state: FreshnessState, age_ms: u64) -> FreshnessAssessment {
    FreshnessAssessment {
        state,
        causes: vec![],
        age_ms,
        confidence: 1.0,
        last_update_ms: 0,
        last_event_ms: None,
        last_significant_event_ms: None,
    }
}

#[test]
fn a3_silent_when_no_anomalies_and_no_freshness() {
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert!(view.anomalies.is_empty());
    assert!(view.blockers.is_empty());
}

#[test]
fn a3_dialog_anomaly_surfaces_as_anomaly_and_blocker() {
    // Dialog is in the blocking subset → produces BOTH an
    // AnomalyRef and a Blocker. Element id from the anomaly's
    // first element_id should attach to the blocker.
    let anomalies = vec![make_anomaly(
        AnomalyType::Dialog,
        "Save changes before quitting?",
        Some("ax:save-dialog"),
    )];
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: Some(&anomalies),
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.anomalies.len(), 1);
    assert_eq!(view.anomalies[0].kind, "dialog");
    assert_eq!(view.blockers.len(), 1);
    assert_eq!(view.blockers[0].kind, "modal_dialog");
    assert_eq!(
        view.blockers[0].element_id.as_deref(),
        Some("ax:save-dialog")
    );
}

#[test]
fn a3_auth_prompt_surfaces_as_anomaly_and_blocker() {
    let anomalies = vec![make_anomaly(
        AnomalyType::AuthPrompt,
        "Sign in to continue",
        Some("ax:login-button"),
    )];
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: Some(&anomalies),
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.anomalies[0].kind, "auth_prompt");
    assert_eq!(view.blockers.len(), 1);
    assert_eq!(view.blockers[0].kind, "auth_required");
}

#[test]
fn a3_error_anomaly_surfaces_as_anomaly_only_no_blocker() {
    // Errors are informational, not blocking. The planner can
    // adapt without the heavier blocker treatment.
    let anomalies = vec![
        make_anomaly(AnomalyType::Error, "Network timeout", None),
        make_anomaly(AnomalyType::AppSwitch, "Frontmost changed to Mail", None),
    ];
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: Some(&anomalies),
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.anomalies.len(), 2);
    assert!(view.blockers.is_empty(), "Error/AppSwitch must not block");
}

#[test]
fn a3_hard_stale_freshness_produces_blocker() {
    let freshness = make_freshness(FreshnessState::HardStale, 10_000);
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: Some(&freshness),
        adapter_facts: None,
    });
    assert!(view.anomalies.is_empty());
    assert_eq!(view.blockers.len(), 1);
    assert_eq!(view.blockers[0].kind, "stale_perception");
    assert!(view.blockers[0].description.contains("hard-stale"));
}

#[test]
fn a3_soft_stale_freshness_produces_anomaly_only() {
    // Soft-stale: visible to the planner but not blocking.
    // Ranking signal still trustworthy.
    let freshness = make_freshness(FreshnessState::SoftStale, 2_000);
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: Some(&freshness),
        adapter_facts: None,
    });
    assert!(view.blockers.is_empty(), "soft-stale must NOT block");
    assert_eq!(view.anomalies.len(), 1);
    assert_eq!(view.anomalies[0].kind, "perception_soft_stale");
}

#[test]
fn a3_fresh_freshness_contributes_nothing() {
    let freshness = make_freshness(FreshnessState::Fresh, 50);
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: Some(&freshness),
        adapter_facts: None,
    });
    assert!(view.anomalies.is_empty());
    assert!(view.blockers.is_empty());
}

#[test]
fn a3_anomalies_and_freshness_combine() {
    // Both signals present: dialog → anomaly + blocker; hard-stale
    // → blocker. Total: 1 anomaly + 2 blockers.
    let anomalies = vec![make_anomaly(
        AnomalyType::Dialog,
        "Confirm delete?",
        Some("ax:confirm"),
    )];
    let freshness = make_freshness(FreshnessState::HardStale, 9_999);
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: Some(&anomalies),
        cortex_freshness: Some(&freshness),
        adapter_facts: None,
    });
    assert_eq!(view.anomalies.len(), 1, "1 anomaly from dialog");
    assert_eq!(
        view.blockers.len(),
        2,
        "1 blocker from dialog + 1 from hard-stale"
    );
    let blocker_kinds: Vec<&str> = view.blockers.iter().map(|b| b.kind.as_str()).collect();
    assert!(blocker_kinds.contains(&"modal_dialog"));
    assert!(blocker_kinds.contains(&"stale_perception"));
}

#[test]
fn a3_rationale_mentions_anomalies_and_blockers_when_present() {
    let anomalies = vec![make_anomaly(AnomalyType::Dialog, "Confirm?", None)];
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "any",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: None,
        knowledge_store: None,
        workflow_id: None,
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: Some(&anomalies),
        cortex_freshness: None,
        adapter_facts: None,
    });
    let rationale = view.selection_rationale.expect("expected rationale");
    assert!(
        rationale.contains("anomaly") || rationale.contains("anomalies"),
        "expected rationale to mention anomalies; got {rationale}"
    );
    assert!(
        rationale.contains("blocker"),
        "expected rationale to mention blockers; got {rationale}"
    );
}

// ─── WK2: vector embedding cosine boost ──────────────────────────────────

/// Deterministic "embedder" for tests — same byte-level output as
/// `cel_llm::Embedder` but synchronous and inline. Mirrors the
/// stub in `cel-llm::embedder::tests` (same hash bucketing) so a
/// memory embedded via the runner's stub embedder and a goal
/// embedded inline produce comparable vectors.
fn embed_inline(text: &str, dim: usize) -> Vec<u8> {
    let mut out = vec![0f32; dim];
    for (i, b) in text.bytes().enumerate() {
        out[i % dim] += (b as f32) / 255.0;
    }
    let mag: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 0.0 {
        for v in &mut out {
            *v /= mag;
        }
    }
    cel_llm::EmbeddingVector::new(out).to_bytes()
}

fn seed_memory_with_embedding(
    store: &std::sync::Mutex<cel_store::CelStore>,
    workflow: &str,
    kind: cel_store::cortex_memory::MemoryKind,
    summary: &str,
    embedded_text: &str,
    dim: usize,
) -> i64 {
    let bytes = embed_inline(embedded_text, dim);
    store
        .lock()
        .expect("seed: store mutex poisoned")
        .insert_cortex_memory(&cel_store::cortex_memory::NewCortexMemory {
            workflow_id: workflow.into(),
            kind,
            content: serde_json::json!({}),
            summary: Some(summary.into()),
            tags: vec![],
            source_ref: None,
            embedding: Some(bytes),
        })
        .expect("insert")
}

#[test]
fn wk2_cosine_boost_reranks_when_goal_embedding_provided() {
    // **Test design contract**: this assertion must FAIL if the
    // cosine boost is removed from `score_memory`. Both memories
    // have IDENTICAL keyword overlap with the goal (same three
    // keywords in their summaries), so without WK2 their base ×
    // decay scores tie and ordering is FTS5-bm25-dependent (not
    // deterministically aligned). With WK2, the embedding alignment
    // decisively breaks the tie via the 0.5x cosine boost.
    let dim = 16;
    let store = fresh_store();

    // Memory UNRELATED: 3 goal keywords in summary (base = 9),
    // embedded with text that doesn't share any goal keywords
    // → cos ≈ low.
    let id_unrelated = seed_memory_with_embedding(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur — alpha",
        "watered the kitchen plants this morning",
        dim,
    );
    // Memory ALIGNED: 3 goal keywords in summary (base = 9 —
    // identical to UNRELATED), embedded with the exact goal
    // text → cos ≈ 1, max boost (0.5x amplification).
    let id_aligned = seed_memory_with_embedding(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur — beta",
        "submit invoice in Concur",
        dim,
    );

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let goal_bytes = embed_inline("submit invoice in Concur", dim);

    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: Some(&goal_bytes),
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    assert_eq!(
        view.memories.len(),
        2,
        "expected both keyword-matched memories"
    );
    // Aligned memory MUST win — the only differentiator vs
    // UNRELATED is the cosine boost (bases tie at 9). Removing the
    // cosine path from `score_memory` would let UNRELATED tie or
    // win on FTS5 bm25 ordering.
    assert_eq!(
        view.memories[0].id, id_aligned,
        "expected cosine-aligned memory ranked first; \
             got id={} (unrelated id={}, aligned id={})",
        view.memories[0].id, id_unrelated, id_aligned
    );
    assert_eq!(view.memories[1].id, id_unrelated);
}

#[test]
fn wk2_no_goal_embedding_falls_back_to_pure_wk1() {
    // Same setup as the boost test, but with `goal_embedding: None`.
    // Selector must NOT consult stored embeddings — pure WK1
    // behaviour. We just check it doesn't panic and returns both.
    let dim = 16;
    let store = fresh_store();
    seed_memory_with_embedding(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur",
        "submit invoice",
        dim,
    );
    seed_memory_with_embedding(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice yet again",
        "submit invoice",
        dim,
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None, // <-- key
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    assert_eq!(view.memories.len(), 2);
}

#[test]
fn wk2_dimension_mismatch_safely_skips_cosine_boost() {
    // Memory was embedded with dim=16, goal embedding is dim=32 —
    // dimension mismatch. Cosine path must NOT panic; selector
    // falls back to pure WK1 base * decay. Ranking is preserved
    // (memory still hydrated via FTS5 keyword match).
    let store = fresh_store();
    seed_memory_with_embedding(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur successfully",
        "submit invoice in Concur",
        16, // memory dim
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let goal_bytes = embed_inline("submit invoice in Concur", 32); // DIFFERENT dim

    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: Some(&goal_bytes),
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    // Memory still surfaces via FTS5+decay; no panic, no NaN scoring.
    assert_eq!(view.memories.len(), 1);
}

#[test]
fn wk2_corrupted_embedding_bytes_safely_skip_cosine_boost() {
    // Misaligned bytes (not multiple of 4) → from_bytes returns
    // None → cosine path skipped → memory still scored via base ×
    // decay alone. Defensive against schema-corruption scenarios.
    let store = fresh_store();
    store
        .lock()
        .unwrap()
        .insert_cortex_memory(&cel_store::cortex_memory::NewCortexMemory {
            workflow_id: "wf".into(),
            kind: cel_store::cortex_memory::MemoryKind::Outcome,
            content: serde_json::json!({}),
            summary: Some("submit invoice with bad embedding".into()),
            tags: vec![],
            source_ref: None,
            embedding: Some(vec![1, 2, 3]), // 3 bytes — not f32-aligned
        })
        .expect("insert");

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let goal_bytes = embed_inline("submit invoice", 16);
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: Some(&goal_bytes),
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    // No panic. Memory hydrates via FTS5+decay despite invalid
    // stored embedding bytes.
    assert_eq!(view.memories.len(), 1);
}

#[test]
fn wk2_embedding_never_outranks_keyword_zero_score() {
    // Critical contract: cosine boost is multiplicative on `base`;
    // a memory with no keyword overlap (base = 0) MUST stay at 0
    // even if its embedding is a perfect cosine match. Embeddings
    // enrich keyword-matched ranking; they don't expand the matched
    // set.
    let dim = 16;
    let store = fresh_store();
    seed_memory_with_embedding(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        // Summary has NO goal keyword overlap.
        "Watered the kitchen plants this morning",
        "submit invoice in Concur", // perfect cosine match to goal
        dim,
    );
    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let goal_bytes = embed_inline("submit invoice in Concur", dim);
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: Some(&goal_bytes),
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });
    // Cosine alone cannot lift a base=0 memory. Empty hydration.
    assert_eq!(view.memories.len(), 0);
}

/// WK1: end-to-end check that the FTS5 pre-filter narrows the
/// candidate window before the Rust scorer runs. We seed a workflow
/// with one keyword-matching memory + many unrelated ones; the
/// selector must return exactly the matching one without the
/// unrelated rows polluting its candidate window.
#[test]
fn wk1_fts5_prefilter_narrows_candidates_to_keyword_matches() {
    let store = fresh_store();
    // 50 unrelated memories — pre-WK1, the selector would pull these
    // as part of "200 most recent" and the Rust scorer would reject
    // them. Post-WK1, FTS5 never returns them; the scorer never sees
    // them; `omitted_counts.memories` reflects only the FTS5-matched
    // candidates that scored too low.
    for i in 0..50 {
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            &format!("Watered the plants on day {i}"),
            serde_json::json!({"day": i}),
        );
    }
    // 1 keyword-matching memory.
    seed_memory(
        &store,
        "wf",
        cel_store::cortex_memory::MemoryKind::Outcome,
        "Submitted invoice via Concur successfully",
        serde_json::json!({"goal": "submit invoice"}),
    );

    let perception = make_context(vec![]);
    let caps = RuntimeCaps::default();
    let budget = PlanningBudget::default();
    let view = build_planning_view(&PlanningViewInputs {
        goal: "submit invoice in Concur",
        budget: &budget,
        perception: &perception,
        caps: &caps,
        memory_store: Some(&store),
        knowledge_store: None,
        workflow_id: Some("wf"),
        goal_embedding: None,
        recent_events_store: None,
        cortex_anomalies: None,
        cortex_freshness: None,
        adapter_facts: None,
    });

    // Exactly one memory hydrated; the unrelated 50 never made it
    // into the candidate window.
    assert_eq!(view.memories.len(), 1);
    assert!(view.memories[0]
        .summary
        .to_lowercase()
        .contains("submitted invoice"));
    // omitted_counts.memories reflects FTS5-returned candidates
    // (1 here) minus kept (1) = 0. Pre-WK1 this would have been
    // 50 (the recency-sourced unrelated rows scored to 0 and
    // were dropped).
    assert_eq!(view.omitted_counts.memories, 0);
}

#[test]
fn compress_propagates_select_options_property_to_planning_element() {
    // The browser CDP adapter encodes select option pairs into
    // properties["select_options"] (see element-mapper.ts:
    // "value|Label, value2|Label 2"). The PlanningElement
    // compression must carry this through verbatim so the planner
    // prompt can show real option values instead of leaving the
    // model to guess slugs. Without this, run-6's contact form
    // scenarios stayed 0/3 with `no-option:select:subject:...`
    // errors.
    let mut sel = make_el("dom:select:subject", "select", Some("Subject"));
    sel.properties.insert(
        "select_options".into(),
        "general_inquiry|General Inquiry, bug_report|Bug Report".into(),
    );

    let compressed = compress(&sel);
    assert_eq!(
        compressed.select_options.as_deref(),
        Some("general_inquiry|General Inquiry, bug_report|Bug Report")
    );
    assert!(
        compressed.settable,
        "select must remain marked settable so set_value is a valid action"
    );
}

#[test]
fn compress_leaves_select_options_none_when_property_absent() {
    // AX-sourced or non-select elements have no
    // properties["select_options"] — the compressor must leave
    // PlanningElement.select_options as None, not empty string.
    // (An empty string would render as an empty `options:` line
    // and confuse the planner.)
    let button = make_el("dom:button:submit", "button", Some("Submit"));
    let compressed = compress(&button);
    assert!(
        compressed.select_options.is_none(),
        "no select_options property → None"
    );
}
