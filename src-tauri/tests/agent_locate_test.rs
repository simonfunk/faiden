//! Finding an installed Hermes, and building the child's environment.
//!
//! Fixtures are executable stub files in a `TempDir`. Nothing here runs Hermes
//! or reads the real `~/.hermes`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use faiden_lib::agent::locate::{self, Lookup, BYPASS_ENV_VARS};
use tempfile::TempDir;

fn executable_stub(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn hermes_is_found_on_the_path() {
    let temp = TempDir::new().unwrap();
    let bin = temp.path().join("bin");
    let expected = executable_stub(&bin, "hermes");

    let found = locate::locate_hermes(&Lookup {
        path_var: Some(bin.to_string_lossy().to_string()),
        home: None,
    })
    .expect("hermes on PATH");
    assert_eq!(found.program, expected);
    assert!(found.source.contains("PATH"), "got {}", found.source);
}

#[test]
fn a_gui_launch_still_finds_the_local_bin_a_login_shell_would_have_added() {
    // A Tauri app started from Finder inherits a minimal PATH that does not
    // include ~/.local/bin, which is exactly where the Hermes launcher lives.
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let expected = executable_stub(&home.join(".local/bin"), "hermes");

    let found = locate::locate_hermes(&Lookup {
        path_var: Some("/usr/bin:/bin".to_string()),
        home: Some(home.clone()),
    })
    .expect("hermes under ~/.local/bin");
    assert_eq!(found.program, expected);
    assert!(found.source.contains(".local/bin"), "got {}", found.source);
}

#[test]
fn the_path_wins_over_the_fallback_directories() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let on_path = executable_stub(&bin, "hermes");
    executable_stub(&home.join(".local/bin"), "hermes");

    let found = locate::locate_hermes(&Lookup {
        path_var: Some(bin.to_string_lossy().to_string()),
        home: Some(home),
    })
    .unwrap();
    assert_eq!(found.program, on_path);
}

#[test]
fn a_non_executable_file_named_hermes_is_not_an_installation() {
    let temp = TempDir::new().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("hermes"), "not executable").unwrap();
    fs::set_permissions(bin.join("hermes"), fs::Permissions::from_mode(0o644)).unwrap();

    let err = locate::locate_hermes(&Lookup {
        path_var: Some(bin.to_string_lossy().to_string()),
        home: None,
    })
    .expect_err("a non-executable file is not an install");
    assert_eq!(err.code(), "NOT_FOUND");
}

#[test]
fn a_missing_install_says_what_was_searched_and_never_offers_to_install_it() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let err = locate::locate_hermes(&Lookup {
        path_var: Some(temp.path().join("empty").to_string_lossy().to_string()),
        home: Some(home),
    })
    .expect_err("nothing to find");
    assert_eq!(err.code(), "NOT_FOUND");

    let message = err.message();
    assert!(
        message.contains(".local/bin"),
        "must name what it searched: {message}"
    );
    let lowered = message.to_lowercase();
    assert!(
        !lowered.contains("installing") && !lowered.contains("we will install"),
        "Faiden never installs or authenticates on the user's behalf: {message}"
    );
}

#[test]
fn no_personal_absolute_path_is_compiled_into_the_search() {
    for dir in locate::fallback_directories(Some(Path::new("/home/example"))) {
        let text = dir.to_string_lossy().to_string();
        assert!(
            !text.contains("simonfunk"),
            "a developer's own path must never ship: {text}"
        );
    }
}

#[test]
fn inherited_approval_bypass_variables_are_removed_from_the_child() {
    // Every name below is a real switch in the installed Hermes, not an
    // invented flag: HERMES_YOLO_MODE (tools/approval.py),
    // HERMES_ACP_AUTO_APPROVE (hermes_cli/config.py, env_loader.py),
    // HERMES_NONINTERACTIVE (hermes_cli/setup.py), HERMES_EXEC_ASK and
    // HERMES_CRON_SESSION (acp_adapter/server.py).
    let inherited = vec![
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("HOME".to_string(), "/Users/example".to_string()),
        ("OPENAI_API_KEY".to_string(), "sk-example".to_string()),
        ("HERMES_YOLO_MODE".to_string(), "1".to_string()),
        ("HERMES_ACP_AUTO_APPROVE".to_string(), "1".to_string()),
        ("HERMES_NONINTERACTIVE".to_string(), "1".to_string()),
        ("HERMES_EXEC_ASK".to_string(), "1".to_string()),
        ("HERMES_CRON_SESSION".to_string(), "abc".to_string()),
    ];

    let scrubbed = locate::scrubbed_env(inherited);
    let names: Vec<&str> = scrubbed.iter().map(|(k, _)| k.as_str()).collect();

    for bypass in BYPASS_ENV_VARS {
        assert!(
            !names.contains(bypass),
            "{bypass} must not reach the agent: it would turn Faiden's approval UI into a rubber stamp"
        );
    }
    // Credentials and ordinary configuration are inherited untouched: auth
    // stays with the installed harness.
    assert!(names.contains(&"OPENAI_API_KEY"));
    assert!(names.contains(&"HOME"));
}

#[test]
fn the_child_path_keeps_the_users_path_and_adds_the_fallbacks() {
    let path = locate::augmented_path(Some("/usr/bin:/bin"), Some(Path::new("/home/example")));
    let entries: Vec<&str> = path.split(':').collect();
    assert_eq!(entries[0], "/usr/bin", "the user's own PATH keeps priority");
    assert!(entries.contains(&"/home/example/.local/bin"));
    assert_eq!(
        entries.len(),
        entries
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        "no duplicate entries: {path}"
    );
}
