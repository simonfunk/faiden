//! The authority boundary for starting an agent.
//!
//! The webview names a session and nothing else. What runs, and where it runs,
//! is decided natively — exactly as it is for terminals.

use faiden_lib::store::{NewSession, Store};
use faiden_lib::{resolve_agent_cwd, AgentAvailability};
use tempfile::TempDir;

fn session_in(store: &Store, workdir: Option<&str>) -> String {
    let thread = store.create_thread("ACP thread").unwrap();
    if let Some(dir) = workdir {
        store.set_thread_workdir(&thread.id, Some(dir)).unwrap();
    }
    store
        .create_session(
            &thread.id,
            NewSession {
                label: "Hermes session".to_string(),
                kind: "hermes-acp".to_string(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .unwrap()
        .id
}

#[test]
fn an_agent_starts_in_the_directory_the_user_bound_to_the_thread() {
    let dir = TempDir::new().unwrap();
    let workdir = dir.path().join("project");
    std::fs::create_dir_all(&workdir).unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let session = session_in(&store, Some(&workdir.to_string_lossy()));

    let resolved = resolve_agent_cwd(&store, &session, Some(dir.path())).unwrap();
    assert_eq!(resolved.path, workdir);
    assert!(
        resolved.label.contains("bound to this thread"),
        "provenance must be explicit: {}",
        resolved.label
    );
}

#[test]
fn a_thread_with_no_directory_falls_back_to_home_and_labels_it() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let session = session_in(&store, None);

    let resolved = resolve_agent_cwd(&store, &session, Some(&home)).unwrap();
    assert_eq!(resolved.path, home);
    assert!(
        resolved.label.to_lowercase().contains("home"),
        "the fallback is named, never presented as a project directory: {}",
        resolved.label
    );
}

#[test]
fn a_directory_that_has_since_disappeared_fails_closed() {
    let dir = TempDir::new().unwrap();
    let workdir = dir.path().join("gone");
    std::fs::create_dir_all(&workdir).unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let session = session_in(&store, Some(&workdir.to_string_lossy()));
    std::fs::remove_dir(&workdir).unwrap();

    let err = resolve_agent_cwd(&store, &session, Some(dir.path())).expect_err("directory is gone");
    assert_eq!(err.code(), "VALIDATION");
}

#[test]
fn an_ended_session_cannot_be_given_a_new_agent() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let session = session_in(&store, None);
    store.end_session(&session, "closed by the user").unwrap();

    let err = resolve_agent_cwd(&store, &session, Some(dir.path())).expect_err("session ended");
    assert_eq!(err.code(), "CONFLICT");
}

#[test]
fn an_unknown_session_cannot_start_anything() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let err = resolve_agent_cwd(&store, "ses_nope", Some(dir.path())).expect_err("unknown session");
    assert_eq!(err.code(), "NOT_FOUND");
}

#[test]
fn with_no_home_at_all_the_caller_is_told_rather_than_given_a_guess() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let session = session_in(&store, None);
    let err = resolve_agent_cwd(&store, &session, None).expect_err("no directory to use");
    assert_eq!(err.code(), "VALIDATION");
}

#[test]
fn availability_describes_a_missing_install_without_offering_to_fix_it() {
    let dir = TempDir::new().unwrap();
    let report = AgentAvailability::probe(&faiden_lib::agent::locate::Lookup {
        path_var: Some(dir.path().join("empty").to_string_lossy().to_string()),
        home: Some(dir.path().to_path_buf()),
    });

    assert!(!report.installed);
    assert_eq!(report.program, None);
    assert!(report.message.contains(".local/bin"));
    assert!(
        !report.message.to_lowercase().contains("install it for you"),
        "{}",
        report.message
    );
    // The authority disclosure is always present, installed or not.
    assert!(report.disclosure.contains("not a sandbox"));
}

#[test]
fn availability_reports_a_real_install_and_where_it_came_from() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("hermes"), "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("hermes"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    let report = AgentAvailability::probe(&faiden_lib::agent::locate::Lookup {
        path_var: Some(String::new()),
        home: Some(dir.path().to_path_buf()),
    });
    assert!(report.installed);
    assert_eq!(
        report.program,
        Some(bin.join("hermes").to_string_lossy().to_string())
    );
    assert!(report.source.unwrap().contains(".local/bin"));
}

#[test]
fn the_overview_ties_every_run_to_its_thread_and_flags_a_real_permission_wait() {
    use faiden_lib::agent::runtime::{AgentRunInfo, AgentStatus};
    use faiden_lib::agent_overviews;

    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let session = session_in(&store, None);
    let thread_id = store.get_session(&session).unwrap().thread_id;

    let run = |status: AgentStatus| AgentRunInfo {
        run_id: "agr_1".to_string(),
        session_id: session.clone(),
        epoch: 1,
        pid: Some(1),
        program: "hermes".to_string(),
        source: "PATH".to_string(),
        cwd: "/tmp".to_string(),
        acp_session_id: Some("s".to_string()),
        status,
        detail: None,
        started_at: 1,
        ended_at: None,
        prompt_in_flight: false,
        disclosure: String::new(),
    };

    let waiting = agent_overviews(&store, vec![run(AgentStatus::AwaitingPermission)]);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].thread_id, thread_id);
    assert!(
        waiting[0].awaiting_permission,
        "a pending permission is genuine, actionable status"
    );

    let working = agent_overviews(&store, vec![run(AgentStatus::Responding)]);
    assert!(!working[0].awaiting_permission);
}

#[test]
fn a_run_whose_session_vanished_is_dropped_rather_than_shown_against_the_wrong_thread() {
    use faiden_lib::agent::runtime::{AgentRunInfo, AgentStatus};
    use faiden_lib::agent_overviews;

    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("faiden.sqlite3")).unwrap();
    let overviews = agent_overviews(
        &store,
        vec![AgentRunInfo {
            run_id: "agr_1".to_string(),
            session_id: "ses_gone".to_string(),
            epoch: 1,
            pid: None,
            program: "hermes".to_string(),
            source: "PATH".to_string(),
            cwd: "/tmp".to_string(),
            acp_session_id: None,
            status: AgentStatus::Ready,
            detail: None,
            started_at: 1,
            ended_at: None,
            prompt_in_flight: false,
            disclosure: String::new(),
        }],
    );
    assert!(overviews.is_empty());
}
