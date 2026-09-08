//! Behavioural tests for thread persistence, session lineage and review state.
//!
//! Every test writes into its own `tempfile::TempDir`; nothing touches the real
//! application data directory.

use faiden_lib::error::AppError;
use faiden_lib::store::{NewSession, Store, LIMIT_NOTES_CHARS, LIMIT_TITLE_CHARS};
use tempfile::TempDir;

fn temp_store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(&dir.path().join("faiden.sqlite3")).expect("open store");
    (dir, store)
}

// --- 1. Threads without any directory or repository -------------------------

#[test]
fn creates_and_reloads_a_thread_without_any_directory() {
    let dir = TempDir::new().expect("temp dir");
    let db = dir.path().join("faiden.sqlite3");

    let created = {
        let store = Store::open(&db).expect("open store");
        store
            .create_thread("Rate limiter design")
            .expect("create thread")
    };

    assert_eq!(created.title, "Rate limiter design");
    assert_eq!(created.notes, "");
    assert_eq!(
        created.workdir, None,
        "a new thread must not require a directory"
    );
    assert_eq!(created.seen_at, None);
    assert_eq!(created.reviewed_at, None);

    // Reopen: a fresh Store instance over the same file must see the thread.
    let store = Store::open(&db).expect("reopen store");
    let loaded = store.get_thread(&created.id).expect("load thread");
    assert_eq!(loaded.id, created.id);
    assert_eq!(loaded.title, "Rate limiter design");
    assert_eq!(loaded.workdir, None);

    let all = store.list_threads().expect("list threads");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, created.id);
}

#[test]
fn lists_threads_most_recently_updated_first() {
    let (_dir, store) = temp_store();
    let a = store.create_thread("First").unwrap();
    let b = store.create_thread("Second").unwrap();

    let listed: Vec<String> = store
        .list_threads()
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(listed, vec![b.id.clone(), a.id.clone()]);

    store.set_thread_notes(&a.id, "touched").unwrap();
    let listed: Vec<String> = store
        .list_threads()
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(listed, vec![a.id, b.id]);
}

#[test]
fn renames_and_notes_survive_a_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let db = dir.path().join("faiden.sqlite3");
    let id = {
        let store = Store::open(&db).unwrap();
        let t = store.create_thread("Untitled thread").unwrap();
        store
            .rename_thread(&t.id, "  Token bucket vs leaky bucket  ")
            .unwrap();
        store
            .set_thread_notes(&t.id, "Decided: token bucket.\nRejected: fixed window.")
            .unwrap();
        t.id
    };

    let store = Store::open(&db).unwrap();
    let t = store.get_thread(&id).unwrap();
    assert_eq!(
        t.title, "Token bucket vs leaky bucket",
        "titles are trimmed"
    );
    assert_eq!(t.notes, "Decided: token bucket.\nRejected: fixed window.");
}

#[test]
fn rejects_blank_and_overlong_titles() {
    let (_dir, store) = temp_store();

    match store.create_thread("   ") {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "title"),
        other => panic!("blank title must be rejected, got {other:?}"),
    }

    let overlong = "x".repeat(LIMIT_TITLE_CHARS + 1);
    match store.create_thread(&overlong) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "title"),
        other => panic!("overlong title must be rejected, got {other:?}"),
    }

    // Exactly at the limit is accepted.
    let at_limit = "y".repeat(LIMIT_TITLE_CHARS);
    let t = store
        .create_thread(&at_limit)
        .expect("title at the limit is valid");
    assert_eq!(t.title.chars().count(), LIMIT_TITLE_CHARS);

    // The rejected threads were never written.
    assert_eq!(store.list_threads().unwrap().len(), 1);
}

#[test]
fn title_length_is_counted_in_characters_not_bytes() {
    let (_dir, store) = temp_store();
    // 200 multi-byte characters is 800 bytes but still 200 characters.
    let emoji_title = "🧵".repeat(LIMIT_TITLE_CHARS);
    let t = store
        .create_thread(&emoji_title)
        .expect("multi-byte title at limit is valid");
    assert_eq!(t.title.chars().count(), LIMIT_TITLE_CHARS);
}

#[test]
fn rejects_notes_beyond_the_bound() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("Bounded notes").unwrap();

    let overlong = "n".repeat(LIMIT_NOTES_CHARS + 1);
    match store.set_thread_notes(&t.id, &overlong) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "notes"),
        other => panic!("overlong notes must be rejected, got {other:?}"),
    }
    assert_eq!(
        store.get_thread(&t.id).unwrap().notes,
        "",
        "rejected notes are not persisted"
    );

    let at_limit = "n".repeat(LIMIT_NOTES_CHARS);
    store
        .set_thread_notes(&t.id, &at_limit)
        .expect("notes at the limit are valid");
    assert_eq!(
        store.get_thread(&t.id).unwrap().notes.chars().count(),
        LIMIT_NOTES_CHARS
    );
}

#[test]
fn unknown_thread_ids_fail_closed() {
    let (_dir, store) = temp_store();

    for result in [
        store.get_thread("nope").map(|_| ()),
        store.rename_thread("nope", "x").map(|_| ()),
        store.set_thread_notes("nope", "x").map(|_| ()),
        store.mark_thread_seen("nope").map(|_| ()),
        store.mark_thread_reviewed("nope").map(|_| ()),
    ] {
        match result {
            Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "thread"),
            other => panic!("stale thread id must fail closed, got {other:?}"),
        }
    }
}

// --- Seen vs reviewed -------------------------------------------------------

#[test]
fn marking_a_thread_seen_does_not_mark_it_reviewed() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("Review states").unwrap();

    let seen = store.mark_thread_seen(&t.id).unwrap();
    assert!(
        seen.seen_at.is_some(),
        "opening a thread records that it was seen"
    );
    assert_eq!(
        seen.reviewed_at, None,
        "opening a thread must not approve its results"
    );

    let reviewed = store.mark_thread_reviewed(&t.id).unwrap();
    assert!(reviewed.seen_at.is_some());
    assert!(
        reviewed.reviewed_at.is_some(),
        "review is an explicit separate act"
    );
}

#[test]
fn review_items_track_seen_and_reviewed_independently() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("Inbox").unwrap();
    let item = store
        .add_review_item(&t.id, None, "note", "Handoff draft ready for review")
        .unwrap();

    assert_eq!(item.seen_at, None);
    assert_eq!(item.reviewed_at, None);

    let unseen = store.list_review_items(&t.id).unwrap();
    assert_eq!(unseen.len(), 1);
    assert_eq!(unseen[0].summary, "Handoff draft ready for review");

    // Opening the thread marks its inbox items seen but never reviewed.
    store.mark_thread_seen(&t.id).unwrap();
    let after_open = store.list_review_items(&t.id).unwrap();
    assert!(
        after_open[0].seen_at.is_some(),
        "opening the thread marks items seen"
    );
    assert_eq!(
        after_open[0].reviewed_at, None,
        "opening the thread must not mark items reviewed"
    );

    store.mark_review_item_reviewed(&item.id).unwrap();
    let after_review = store.list_review_items(&t.id).unwrap();
    assert!(after_review[0].reviewed_at.is_some());

    match store.mark_review_item_reviewed("stale-item-id") {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "review_item"),
        other => panic!("stale review item id must fail closed, got {other:?}"),
    }
}

// --- Environment binding ----------------------------------------------------

#[test]
fn thread_directory_binding_is_explicit_and_clearable() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("faiden.sqlite3");
    let workdir = dir.path().join("project");
    std::fs::create_dir_all(&workdir).unwrap();

    let store = Store::open(&db).unwrap();
    let t = store.create_thread("Bind later").unwrap();
    assert_eq!(t.workdir, None);

    let bound = store
        .set_thread_workdir(&t.id, Some(workdir.to_str().unwrap()))
        .unwrap();
    assert_eq!(bound.workdir.as_deref(), Some(workdir.to_str().unwrap()));

    let cleared = store.set_thread_workdir(&t.id, None).unwrap();
    assert_eq!(
        cleared.workdir, None,
        "unbinding a directory keeps the thread alive"
    );
    assert_eq!(store.get_thread(&t.id).unwrap().title, "Bind later");
}

// --- 2. Session lineage and continuation drafts -----------------------------

#[test]
fn successor_session_links_predecessor_and_preserves_source_briefing() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("faiden.sqlite3");

    let (thread_id, first_id) = {
        let store = Store::open(&db).unwrap();
        let t = store.create_thread("Long running topic").unwrap();
        let first = store
            .create_session(
                &t.id,
                NewSession {
                    label: "Shell session".into(),
                    kind: "shell".into(),
                    predecessor_id: None,
                    briefing: "Goal: reproduce the flaky test.".into(),
                },
            )
            .unwrap();
        (t.id, first.id)
    };

    let store = Store::open(&db).unwrap();
    let second = store
        .create_session(
            &thread_id,
            NewSession {
                label: "Continuation".into(),
                kind: "handoff-draft".into(),
                predecessor_id: Some(first_id.clone()),
                briefing: "Goal: reproduce the flaky test.\nNew: seed is time-dependent.".into(),
            },
        )
        .unwrap();

    assert_eq!(second.predecessor_id.as_deref(), Some(first_id.as_str()));

    // The predecessor's own briefing is untouched by the successor.
    let sessions = store.list_sessions(&thread_id).unwrap();
    assert_eq!(sessions.len(), 2);
    let first = sessions.iter().find(|s| s.id == first_id).unwrap();
    assert_eq!(first.briefing, "Goal: reproduce the flaky test.");
    assert_eq!(first.predecessor_id, None);
}

#[test]
fn continuation_draft_carries_notes_and_predecessor_and_never_claims_a_resumed_context() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("Flaky test hunt").unwrap();
    store
        .set_thread_notes(&t.id, "Decided: pin the clock.\nRejected: retry loop.")
        .unwrap();
    let first = store
        .create_session(
            &t.id,
            NewSession {
                label: "Shell session".into(),
                kind: "shell".into(),
                predecessor_id: None,
                briefing: "Goal: reproduce the flaky test.".into(),
            },
        )
        .unwrap();

    let draft = store.build_handoff_draft(&t.id, Some(&first.id)).unwrap();

    assert_eq!(draft.thread_id, t.id);
    assert_eq!(
        draft.predecessor_session_id.as_deref(),
        Some(first.id.as_str())
    );
    assert!(
        draft.text.contains("Flaky test hunt"),
        "draft carries the thread title"
    );
    assert!(
        draft.text.contains("Decided: pin the clock."),
        "draft carries the thread notes"
    );
    assert!(
        draft.text.contains("Goal: reproduce the flaky test."),
        "draft carries the predecessor briefing"
    );
    assert!(
        draft.disclosure.to_lowercase().contains("does not resume"),
        "the draft must state that it does not resume any AI context, got: {}",
        draft.disclosure
    );
    assert!(
        draft.text.contains(&draft.disclosure),
        "the disclosure travels with the text"
    );

    // Building a draft is not the same as creating a session: nothing is stored yet.
    assert_eq!(store.list_sessions(&t.id).unwrap().len(), 1);
}

#[test]
fn a_continuation_draft_needs_no_running_terminal() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("No terminal here").unwrap();
    store
        .set_thread_notes(&t.id, "Just thinking out loud.")
        .unwrap();

    let draft = store
        .build_handoff_draft(&t.id, None)
        .expect("draft without predecessor");
    assert_eq!(draft.predecessor_session_id, None);
    assert!(draft.text.contains("Just thinking out loud."));
    assert!(draft.disclosure.to_lowercase().contains("does not resume"));
}

#[test]
fn stale_or_foreign_predecessor_ids_fail_closed() {
    let (_dir, store) = temp_store();
    let a = store.create_thread("Thread A").unwrap();
    let b = store.create_thread("Thread B").unwrap();
    let in_b = store
        .create_session(
            &b.id,
            NewSession {
                label: "B session".into(),
                kind: "shell".into(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .unwrap();

    // Unknown predecessor.
    match store.build_handoff_draft(&a.id, Some("does-not-exist")) {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "session"),
        other => panic!("unknown predecessor must fail closed, got {other:?}"),
    }

    // Predecessor belonging to a different thread.
    match store.build_handoff_draft(&a.id, Some(&in_b.id)) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "predecessor_id"),
        other => panic!("cross-thread predecessor must fail closed, got {other:?}"),
    }
    match store.create_session(
        &a.id,
        NewSession {
            label: "Bad lineage".into(),
            kind: "handoff-draft".into(),
            predecessor_id: Some(in_b.id.clone()),
            briefing: String::new(),
        },
    ) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "predecessor_id"),
        other => panic!("cross-thread lineage must fail closed, got {other:?}"),
    }

    // Unknown thread.
    match store.create_session(
        "no-such-thread",
        NewSession {
            label: "Orphan".into(),
            kind: "shell".into(),
            predecessor_id: None,
            briefing: String::new(),
        },
    ) {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "thread"),
        other => panic!("orphan session must fail closed, got {other:?}"),
    }

    assert_eq!(store.list_sessions(&a.id).unwrap().len(), 0);
}

#[test]
fn sessions_record_their_own_end_without_touching_the_thread() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("Lifecycle").unwrap();
    let s = store
        .create_session(
            &t.id,
            NewSession {
                label: "Shell session".into(),
                kind: "shell".into(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .unwrap();
    assert_eq!(s.ended_at, None);

    store.end_session(&s.id, "exited with status 0").unwrap();
    let listed = store.list_sessions(&t.id).unwrap();
    assert!(listed[0].ended_at.is_some());
    assert_eq!(listed[0].outcome.as_deref(), Some("exited with status 0"));

    // Ending a session says nothing about human review.
    assert_eq!(store.get_thread(&t.id).unwrap().reviewed_at, None);

    match store.end_session("stale-session", "x") {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "session"),
        other => panic!("stale session id must fail closed, got {other:?}"),
    }
}

// --- Error serialization ----------------------------------------------------

#[test]
fn errors_serialize_to_a_stable_discriminated_shape() {
    let v = serde_json::to_value(AppError::Validation {
        field: "title".into(),
        message: "must not be blank".into(),
    })
    .unwrap();
    assert_eq!(v["code"], "VALIDATION");
    assert_eq!(v["field"], "title");
    assert_eq!(v["message"], "must not be blank");

    let v = serde_json::to_value(AppError::NotFound {
        entity: "thread".into(),
        id: "abc".into(),
    })
    .unwrap();
    assert_eq!(v["code"], "NOT_FOUND");
    assert_eq!(v["entity"], "thread");
    assert_eq!(v["id"], "abc");
    assert!(
        v["message"].as_str().unwrap().contains("abc"),
        "every error carries a human readable message"
    );

    // Errors must never serialize to a bare string that the UI cannot branch on.
    assert!(serde_json::to_value(AppError::internal("boom"))
        .unwrap()
        .is_object());
}

// --- Durable lifecycle invariants -------------------------------------------

fn shell_session(store: &Store, thread_id: &str) -> faiden_lib::store::Session {
    store
        .create_session(
            thread_id,
            NewSession {
                label: "Shell session".into(),
                kind: "shell".into(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .expect("create session")
}

#[test]
fn a_second_end_can_never_rewrite_the_first_outcome_or_its_timestamp() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("Lifecycle").unwrap();
    let s = shell_session(&store, &t.id);

    let first = store
        .end_session(&s.id, "terminal process exited with code 0")
        .unwrap();
    let first_end = first.ended_at.expect("the first end is recorded");

    let second = store
        .end_session(&s.id, "terminated because Faiden quit")
        .unwrap();
    assert_eq!(
        second.outcome.as_deref(),
        Some("terminal process exited with code 0"),
        "a later, contradictory outcome must not overwrite the recorded fact"
    );
    assert_eq!(
        second.ended_at,
        Some(first_end),
        "the original end timestamp is immutable"
    );

    // And the durable row agrees, not just the returned value.
    let reloaded = store.get_session(&s.id).unwrap();
    assert_eq!(
        reloaded.outcome.as_deref(),
        Some("terminal process exited with code 0")
    );
    assert_eq!(reloaded.ended_at, Some(first_end));
}

#[test]
fn get_session_fails_closed_on_a_forged_id() {
    let (_dir, store) = temp_store();
    match store.get_session("ses_not_a_real_id") {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "session"),
        other => panic!("a forged session id must fail closed, got {other:?}"),
    }
}

#[test]
fn the_store_itself_rejects_a_workdir_that_is_not_an_existing_directory() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("faiden.sqlite3");
    let store = Store::open(&db).unwrap();
    let t = store.create_thread("Bind checking").unwrap();

    // Absolute but nonexistent.
    let missing = dir.path().join("never-created");
    match store.set_thread_workdir(&t.id, Some(missing.to_str().unwrap())) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "workdir"),
        other => panic!("a nonexistent directory must be rejected by the store, got {other:?}"),
    }

    // Absolute and existing, but a regular file.
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "x").unwrap();
    match store.set_thread_workdir(&t.id, Some(file.to_str().unwrap())) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "workdir"),
        other => panic!("a regular file must be rejected by the store, got {other:?}"),
    }

    assert_eq!(
        store.get_thread(&t.id).unwrap().workdir,
        None,
        "a rejected binding is never persisted"
    );

    // A real directory is still accepted, and clearing still works.
    let good = dir.path().join("project");
    std::fs::create_dir(&good).unwrap();
    let bound = store
        .set_thread_workdir(&t.id, Some(good.to_str().unwrap()))
        .unwrap();
    assert_eq!(bound.workdir.as_deref(), Some(good.to_str().unwrap()));
    assert_eq!(store.set_thread_workdir(&t.id, None).unwrap().workdir, None);
}

#[test]
fn a_terminal_run_is_bound_to_its_session_and_ends_it_exactly_once() {
    let (_dir, store) = temp_store();
    let t = store.create_thread("Runs").unwrap();
    let s = shell_session(&store, &t.id);

    store.start_terminal_run("term_a", &s.id).unwrap();
    assert_eq!(
        store.get_session(&s.id).unwrap().ended_at,
        None,
        "starting a run does not end anything"
    );

    store
        .end_terminal_run("term_a", "terminal process exited with code 0")
        .unwrap();
    let ended = store.get_session(&s.id).unwrap();
    assert!(ended.ended_at.is_some());
    assert_eq!(
        ended.outcome.as_deref(),
        Some("terminal process exited with code 0")
    );

    // Ending the same run again is a no-op, not a rewrite.
    store
        .end_terminal_run("term_a", "terminated because Faiden quit")
        .unwrap();
    assert_eq!(
        store.get_session(&s.id).unwrap().outcome.as_deref(),
        Some("terminal process exited with code 0")
    );

    // A run can only be recorded against a session that exists.
    match store.start_terminal_run("term_b", "ses_forged") {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "session"),
        other => panic!("a run must not attach to a forged session, got {other:?}"),
    }
}

#[test]
fn runs_left_open_by_a_previous_process_are_reconciled_as_unknown_not_live() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("faiden.sqlite3");

    let thread_id;
    let live_id;
    let clean_id;
    {
        let store = Store::open(&db).unwrap();
        let t = store.create_thread("Abnormal exit").unwrap();
        thread_id = t.id.clone();

        // One session whose terminal was still running when the process died.
        let live = shell_session(&store, &t.id);
        live_id = live.id.clone();
        store.start_terminal_run("term_live", &live.id).unwrap();

        // One that ended properly beforehand.
        let clean = shell_session(&store, &t.id);
        clean_id = clean.id.clone();
        store.start_terminal_run("term_clean", &clean.id).unwrap();
        store
            .end_terminal_run("term_clean", "terminal process exited with code 0")
            .unwrap();

        // A session with no terminal at all must not be touched by reconciliation.
        store
            .create_session(
                &t.id,
                NewSession {
                    label: "Continuation".into(),
                    kind: "handoff-draft".into(),
                    predecessor_id: None,
                    briefing: "notes".into(),
                },
            )
            .unwrap();
    }

    // Restart.
    let store = Store::open(&db).unwrap();
    assert_eq!(
        store.reconcile_interrupted_runs().unwrap(),
        1,
        "exactly the interrupted run is reconciled"
    );

    let live = store.get_session(&live_id).unwrap();
    assert!(
        live.ended_at.is_some(),
        "an interrupted session must not be presented as still open"
    );
    let outcome = live.outcome.expect("an outcome is recorded");
    let lower = outcome.to_lowercase();
    assert!(
        lower.contains("unknown"),
        "the outcome states that the end is unknown, got: {outcome}"
    );
    assert!(
        !lower.contains("success"),
        "reconciliation never claims success, got: {outcome}"
    );

    assert_eq!(
        store.get_session(&clean_id).unwrap().outcome.as_deref(),
        Some("terminal process exited with code 0"),
        "an already recorded outcome survives reconciliation"
    );

    let untouched = store
        .list_sessions(&thread_id)
        .unwrap()
        .into_iter()
        .find(|s| s.kind == "handoff-draft")
        .unwrap();
    assert_eq!(
        untouched.ended_at, None,
        "a session that never had a terminal is not ended by reconciliation"
    );

    // Reconciliation is idempotent across a second restart.
    assert_eq!(store.reconcile_interrupted_runs().unwrap(), 0);
}
