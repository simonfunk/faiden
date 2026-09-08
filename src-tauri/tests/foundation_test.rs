//! End-to-end composition of the foundation, exercised the way the Tauri
//! command layer composes it: real SQLite file, real Git worktree, real PTY.
//!
//! Unlike the per-module suites, this file was written after those units were
//! driven out test-first. It exists to catch integration regressions and to
//! exercise the "survives a restart" gate without a window.

mod support;

use std::time::Duration;

use faiden_lib::environment::inspect_directory;
use faiden_lib::store::{NewSession, Store};
use faiden_lib::terminal::{TerminalManager, TerminalSpec};
use support::{
    git, has_exit, init_repo_with_commit, process_gone, text_of, wait_until, CollectingSink,
};
use tempfile::TempDir;

const T: Duration = Duration::from_secs(20);

#[test]
fn a_thread_survives_a_restart_with_its_environment_lineage_and_review_state() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let db = root.join("data").join("faiden.sqlite3");

    // A disposable repository plus a linked worktree, created for this test only.
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo_with_commit(&repo);
    let worktree = root.join("wt-foundation");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "foundation",
            worktree.to_str().unwrap(),
        ],
    );

    let thread_id;
    let shell_session_id;
    let continuation_id;
    let review_id;

    {
        let store = Store::open(&db).expect("open store");

        // 1. A thread starts with nothing attached.
        let thread = store.create_thread("Foundation check").unwrap();
        thread_id = thread.id.clone();
        assert_eq!(thread.workdir, None);
        store
            .set_thread_notes(&thread.id, "Decided: keep the daemon out of this slice.")
            .unwrap();

        // 2. The directory is validated the way the command layer validates it,
        //    then bound.
        let ctx = inspect_directory(worktree.to_str().unwrap()).unwrap();
        assert!(ctx.is_directory);
        let git_ctx = ctx.git.expect("the worktree is a repository");
        assert!(git_ctx.is_linked_worktree);
        assert_eq!(git_ctx.branch.as_deref(), Some("foundation"));
        assert!(!git_ctx.dirty);
        store
            .set_thread_workdir(&thread.id, Some(worktree.to_str().unwrap()))
            .unwrap();

        // 3. A shell session, and a real PTY started in that directory.
        let shell = store
            .create_session(
                &thread.id,
                NewSession {
                    label: "Shell session".into(),
                    kind: "shell".into(),
                    predecessor_id: None,
                    briefing: String::new(),
                },
            )
            .unwrap();
        shell_session_id = shell.id.clone();

        let sink = CollectingSink::new();
        let terminals = TerminalManager::new(sink.clone());
        let info = terminals
            .start(TerminalSpec {
                session_id: shell.id.clone(),
                cwd: Some(worktree.to_string_lossy().to_string()),
                command: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "pwd -P; git rev-parse --abbrev-ref HEAD".into(),
                ]),
                cols: 100,
                rows: 30,
            })
            .expect("start a real shell");
        let terminal_id = info.terminal_id.clone();
        let pid = info.pid.expect("a real child process");

        sink.wait_for(T, |e| has_exit(e, &terminal_id));
        let output = text_of(&sink.events(), &terminal_id);
        assert!(
            output.contains(worktree.to_string_lossy().as_ref()),
            "the shell really ran in the bound worktree, got: {output}"
        );
        assert!(
            output.contains("foundation"),
            "git inside the shell agrees about the branch, got: {output}"
        );

        let exit = terminals
            .info(&terminal_id)
            .unwrap()
            .exit
            .expect("exit recorded");
        assert!(exit.success);
        store.end_session(&shell.id, &exit.description).unwrap();

        // 4. A handoff draft, saved as a successor session.
        let draft = store
            .build_handoff_draft(&thread.id, Some(&shell.id))
            .unwrap();
        assert!(draft.text.contains("Foundation check"));
        assert!(draft
            .text
            .contains("Decided: keep the daemon out of this slice."));
        assert!(draft.text.contains(worktree.to_string_lossy().as_ref()));
        assert!(draft.disclosure.to_lowercase().contains("does not resume"));

        let continuation = store
            .create_session(
                &thread.id,
                NewSession {
                    label: "Continuation".into(),
                    kind: "handoff-draft".into(),
                    predecessor_id: Some(shell.id.clone()),
                    briefing: draft.text.clone(),
                },
            )
            .unwrap();
        continuation_id = continuation.id.clone();

        // 5. Something lands in the review inbox; opening the thread marks it
        //    seen but never reviewed.
        let item = store
            .add_review_item(
                &thread.id,
                Some(&continuation.id),
                "handoff",
                "Handoff draft saved — read it before starting the next session.",
            )
            .unwrap();
        review_id = item.id.clone();
        store.mark_thread_seen(&thread.id).unwrap();

        // 6. Quitting the app ends every terminal: there is no background service.
        terminals.shutdown_all();
        assert!(
            wait_until(T, || process_gone(pid)),
            "quitting reaps the child"
        );
        assert!(terminals.list().is_empty());
    }

    // --- restart ------------------------------------------------------------

    let store = Store::open(&db).expect("reopen the same database file");

    let thread = store.get_thread(&thread_id).expect("the thread survived");
    assert_eq!(thread.title, "Foundation check");
    assert_eq!(thread.notes, "Decided: keep the daemon out of this slice.");
    assert_eq!(thread.workdir.as_deref(), worktree.to_str());
    assert!(thread.seen_at.is_some());
    assert_eq!(
        thread.reviewed_at, None,
        "seen is not reviewed, across restarts too"
    );

    let sessions = store.list_sessions(&thread_id).unwrap();
    assert_eq!(sessions.len(), 2);
    let shell = sessions.iter().find(|s| s.id == shell_session_id).unwrap();
    assert!(shell.ended_at.is_some());
    assert_eq!(shell.outcome.as_deref(), Some("Success"));
    let continuation = sessions.iter().find(|s| s.id == continuation_id).unwrap();
    assert_eq!(
        continuation.predecessor_id.as_deref(),
        Some(shell_session_id.as_str())
    );
    assert!(continuation
        .briefing
        .contains("does not resume an AI context"));

    let items = store.list_review_items(&thread_id).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, review_id);
    assert!(items[0].seen_at.is_some());
    assert_eq!(items[0].reviewed_at, None);

    // No terminal is restored: this build owns no process across a restart and
    // does not pretend otherwise.
    let sink = CollectingSink::new();
    let terminals = TerminalManager::new(sink);
    assert!(terminals.list().is_empty());
}

#[test]
fn removing_the_worktree_leaves_the_thread_and_its_history_intact() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let db = root.join("faiden.sqlite3");

    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo_with_commit(&repo);
    let worktree = root.join("wt-temporary");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "temporary",
            worktree.to_str().unwrap(),
        ],
    );

    let store = Store::open(&db).unwrap();
    let thread = store
        .create_thread("Discussion that outlives its branch")
        .unwrap();
    store
        .set_thread_notes(&thread.id, "The reasoning lives here.")
        .unwrap();
    store
        .set_thread_workdir(&thread.id, Some(worktree.to_str().unwrap()))
        .unwrap();
    store
        .create_session(
            &thread.id,
            NewSession {
                label: "Shell session".into(),
                kind: "shell".into(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .unwrap();

    git(
        &repo,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );

    // The thread, its notes and its history are untouched; only the reading of
    // the directory changes.
    let after = store.get_thread(&thread.id).unwrap();
    assert_eq!(after.notes, "The reasoning lives here.");
    assert_eq!(after.workdir.as_deref(), worktree.to_str());
    assert_eq!(store.list_sessions(&thread.id).unwrap().len(), 1);

    let ctx = inspect_directory(worktree.to_str().unwrap()).unwrap();
    assert!(!ctx.exists, "the missing directory is reported honestly");
    assert!(ctx.git.is_none());

    // And a second implementation branch can be derived from the same thread.
    let second = root.join("wt-second");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "second-attempt",
            second.to_str().unwrap(),
        ],
    );
    store
        .set_thread_workdir(&thread.id, Some(second.to_str().unwrap()))
        .unwrap();
    let rebound = inspect_directory(second.to_str().unwrap()).unwrap();
    assert_eq!(
        rebound.git.expect("git context").branch.as_deref(),
        Some("second-attempt")
    );
    assert_eq!(
        store.list_sessions(&thread.id).unwrap().len(),
        1,
        "history is not reset"
    );
}
