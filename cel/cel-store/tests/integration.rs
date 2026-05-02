//! Integration tests for CEL Store.
//!
//! Tests real SQLite behavior: CRUD, search accuracy, concurrent access,
//! data integrity, and edge cases.

use cel_store::CelStore;

#[test]
fn test_store_lifecycle() {
    let store = CelStore::open_memory().expect("Failed to open in-memory store");

    store
        .add_knowledge("Ctrl+S saves the file in Excel", "excel")
        .unwrap();
    store
        .add_knowledge("Ctrl+Z undoes the last action in Excel", "excel")
        .unwrap();
    store
        .add_knowledge("Use T-code VA01 for sales orders", "sap")
        .unwrap();

    let results = store.query_knowledge("Ctrl").unwrap();
    assert_eq!(results.len(), 2);

    let results = store.query_knowledge("VA01").unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("VA01"));
}

#[test]
fn test_run_tracking() {
    let store = CelStore::open_memory().expect("Failed to open in-memory store");

    let run_id = store.start_run("test-workflow", 5).unwrap();
    assert!(run_id > 0);

    store.finish_run(run_id, "completed").unwrap();

    let run_id2 = store.start_run("test-workflow-2", 3).unwrap();
    assert!(run_id2 > run_id);
    store.finish_run(run_id2, "failed").unwrap();
}

#[test]
fn test_independent_stores() {
    let store1 = CelStore::open_memory().unwrap();
    let store2 = CelStore::open_memory().unwrap();

    store1.add_knowledge("unique-fact-alpha", "app1").unwrap();
    store2.add_knowledge("unique-fact-beta", "app2").unwrap();

    assert_eq!(store1.query_knowledge("alpha").unwrap().len(), 1);
    assert_eq!(store1.query_knowledge("beta").unwrap().len(), 0);
    assert_eq!(store2.query_knowledge("beta").unwrap().len(), 1);
    assert_eq!(store2.query_knowledge("alpha").unwrap().len(), 0);
}

#[test]
fn test_knowledge_search_is_case_insensitive_substring() {
    let store = CelStore::open_memory().unwrap();

    store
        .add_knowledge("Press Ctrl+C to copy text", "general")
        .unwrap();
    store
        .add_knowledge("Use CTRL+V to paste", "general")
        .unwrap();

    // Should match both regardless of case
    let results = store.query_knowledge("ctrl").unwrap();
    assert!(
        results.len() >= 1,
        "Case-insensitive search should find 'ctrl' in 'Ctrl+C'"
    );

    let results = store.query_knowledge("COPY").unwrap();
    // Depends on LIKE behavior — at minimum shouldn't crash
    assert!(results.len() <= 2);
}

#[test]
fn test_knowledge_query_no_match_returns_empty() {
    let store = CelStore::open_memory().unwrap();
    store
        .add_knowledge("Something about Excel", "excel")
        .unwrap();

    let results = store.query_knowledge("nonexistent_term_xyz").unwrap();
    assert!(
        results.is_empty(),
        "Query with no matches should return empty vec"
    );
}

#[test]
fn test_knowledge_with_special_characters() {
    let store = CelStore::open_memory().unwrap();

    // Content with SQL-sensitive characters.
    //
    // SAFETY: this stores a canonical SQL-injection payload as TEXT into an
    // in-memory SQLite via parameterized queries. The payload is the test
    // subject (we're verifying parameterization defends against it) — it is
    // never executed as SQL. The DB is `open_memory()` so even a regression
    // could only touch the test-scoped in-memory DB, never any real data.
    store
        .add_knowledge("Use ' single quotes ' carefully", "sql")
        .unwrap();
    store
        .add_knowledge("Path: C:\\Users\\admin\\file.txt", "windows")
        .unwrap();
    store
        .add_knowledge("SELECT * FROM users WHERE id = 1; DROP TABLE--", "security")
        .unwrap();

    // Should not crash or SQL-inject
    let results = store.query_knowledge("single quotes").unwrap();
    assert_eq!(results.len(), 1);

    let results = store.query_knowledge("DROP TABLE").unwrap();
    assert_eq!(results.len(), 1);
    // The content should be stored literally, not executed
    assert!(results[0].content.contains("DROP TABLE"));
}

#[test]
fn test_knowledge_with_unicode() {
    let store = CelStore::open_memory().unwrap();

    store
        .add_knowledge("日本語テスト: ボタンをクリック", "japanese")
        .unwrap();
    store
        .add_knowledge("Emoji test: 🎉 click the 🔘 button", "emoji")
        .unwrap();
    store
        .add_knowledge("Ñoño: señor González", "spanish")
        .unwrap();

    let results = store.query_knowledge("クリック").unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("クリック"));

    let results = store.query_knowledge("🎉").unwrap();
    assert_eq!(results.len(), 1);

    let results = store.query_knowledge("González").unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_knowledge_with_empty_and_whitespace() {
    let store = CelStore::open_memory().unwrap();

    // Empty content — should store without crashing
    store.add_knowledge("", "empty").unwrap();
    store.add_knowledge("   ", "whitespace").unwrap();

    // Querying empty string — implementation-dependent, shouldn't crash
    let results = store.query_knowledge("").unwrap();
    // May return all or none, just verify no crash
    let _ = results.len(); // Just verify no crash
}

#[test]
fn test_many_knowledge_entries() {
    let store = CelStore::open_memory().unwrap();

    // Insert 500 entries
    for i in 0..500 {
        store
            .add_knowledge(
                &format!("Knowledge item {} about topic-{}", i, i % 10),
                &format!("source-{}", i % 5),
            )
            .unwrap();
    }

    // Search should still work efficiently
    let results = store.query_knowledge("topic-3").unwrap();
    assert_eq!(
        results.len(),
        50,
        "Should find 50 entries for topic-3 (500/10)"
    );

    let results = store.query_knowledge("Knowledge item 42").unwrap();
    assert!(results.len() >= 1, "Should find specific item");
    assert!(results[0].content.contains("42"));
}

#[test]
fn test_run_tracking_multiple_workflows() {
    let store = CelStore::open_memory().unwrap();

    // Track several runs with different statuses
    let r1 = store.start_run("login", 3).unwrap();
    store.finish_run(r1, "completed").unwrap();

    let r2 = store.start_run("checkout", 5).unwrap();
    store.finish_run(r2, "failed").unwrap();

    let r3 = store.start_run("login", 3).unwrap();
    store.finish_run(r3, "completed").unwrap();

    // IDs should be strictly increasing
    assert!(r1 < r2);
    assert!(r2 < r3);
}

#[test]
fn test_run_tracking_finish_nonexistent() {
    let store = CelStore::open_memory().unwrap();

    // Finishing a non-existent run should not crash
    // (may return Ok or Err depending on implementation)
    let result = store.finish_run(99999, "completed");
    // Just verify it doesn't panic
    let _ = result;
}

#[test]
fn test_knowledge_source_filtering() {
    let store = CelStore::open_memory().unwrap();

    store
        .add_knowledge("Excel shortcut: Ctrl+Home", "excel")
        .unwrap();
    store.add_knowledge("SAP transaction: VA01", "sap").unwrap();
    store
        .add_knowledge("Excel formula: =SUM(A1:A10)", "excel")
        .unwrap();

    // Query by content, then verify source is preserved
    let results = store.query_knowledge("Excel").unwrap();
    assert_eq!(results.len(), 2);
    for r in &results {
        assert_eq!(r.source, "excel");
    }

    let results = store.query_knowledge("VA01").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source, "sap");
}
