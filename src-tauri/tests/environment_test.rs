//! Git/directory inspection, exercised against real disposable repositories
//! created under `tempfile::TempDir`. Nothing here touches a real project.

mod support;

use faiden_lib::environment::{inspect_directory, PROVENANCE_USER_SELECTED};
use faiden_lib::error::AppError;
use support::{git, init_repo_with_commit};
use tempfile::TempDir;

/// macOS puts temp dirs behind a `/var -> /private/var` symlink; git reports the
/// resolved path, so fixtures compare against the resolved one.
fn fixture() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = std::fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

fn inspect(path: &std::path::Path) -> faiden_lib::environment::DirectoryContext {
    inspect_directory(&path.to_string_lossy()).expect("inspect")
}

#[test]
fn a_plain_directory_has_no_git_context_and_that_is_not_an_error() {
    let (_d, path) = fixture();
    let ctx = inspect(&path);

    assert!(ctx.exists);
    assert!(ctx.is_directory);
    assert!(
        ctx.git.is_none(),
        "a directory without a repository is perfectly usable"
    );
}

#[test]
fn provenance_is_labelled_as_a_user_selection_never_as_an_observed_cwd() {
    let (_d, path) = fixture();
    let ctx = inspect(&path);

    assert_eq!(ctx.provenance, PROVENANCE_USER_SELECTED);
    assert!(
        ctx.observed_at > 0,
        "the reading is timestamped so staleness is visible"
    );
    let note = ctx.note.to_lowercase();
    assert!(
        note.contains("not") && (note.contains("cwd") || note.contains("working directory")),
        "the note must deny that this is an observed process cwd, got: {}",
        ctx.note
    );
}

#[test]
fn a_removed_or_missing_path_is_reported_rather_than_guessed() {
    let (_d, path) = fixture();
    let gone = path.join("never-created");

    let ctx = inspect(&gone);
    assert!(!ctx.exists);
    assert!(!ctx.is_directory);
    assert!(ctx.git.is_none());

    // A directory that disappears after being bound behaves the same way.
    let removed = path.join("temporary");
    std::fs::create_dir(&removed).unwrap();
    assert!(inspect(&removed).exists);
    std::fs::remove_dir(&removed).unwrap();
    assert!(
        !inspect(&removed).exists,
        "the stale binding is reported as missing"
    );
}

#[test]
fn a_file_is_not_accepted_as_a_working_directory() {
    let (_d, path) = fixture();
    let file = path.join("notes.txt");
    std::fs::write(&file, "x").unwrap();

    let ctx = inspect(&file);
    assert!(ctx.exists);
    assert!(!ctx.is_directory);
    assert!(ctx.git.is_none());
}

#[test]
fn relative_paths_are_rejected() {
    match inspect_directory("./somewhere") {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "path"),
        other => panic!("relative paths must be rejected, got {other:?}"),
    }
    match inspect_directory("   ") {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "path"),
        other => panic!("blank paths must be rejected, got {other:?}"),
    }
}

#[test]
fn a_clean_repository_reports_its_branch_head_and_root() {
    let (_d, path) = fixture();
    init_repo_with_commit(&path);

    let g = inspect(&path).git.expect("git context");
    assert_eq!(g.repo_root, path.to_string_lossy());
    assert_eq!(g.branch.as_deref(), Some("main"));
    assert!(!g.detached);
    assert!(!g.unborn);
    assert!(!g.bare);
    assert!(!g.is_linked_worktree);
    assert!(!g.dirty, "a freshly committed repository is clean");
    assert_eq!(g.changed_files, 0);
    let head = g.head_short.expect("a committed repository has a HEAD");
    assert!(head.len() >= 7, "short hash looks like a hash: {head}");
}

#[test]
fn a_subdirectory_resolves_to_the_repository_root() {
    let (_d, path) = fixture();
    init_repo_with_commit(&path);
    let nested = path.join("src/deep");
    std::fs::create_dir_all(&nested).unwrap();

    let ctx = inspect(&nested);
    let g = ctx.git.expect("git context");
    assert_eq!(
        g.repo_root,
        path.to_string_lossy(),
        "root is resolved, not assumed"
    );
    assert_eq!(
        ctx.path,
        nested.to_string_lossy(),
        "the selected path is reported verbatim"
    );
}

#[test]
fn uncommitted_changes_are_counted() {
    let (_d, path) = fixture();
    init_repo_with_commit(&path);

    std::fs::write(path.join("scratch-a.txt"), "one").unwrap();
    std::fs::write(path.join("README.md"), "changed\n").unwrap();

    let g = inspect(&path).git.expect("git context");
    assert!(g.dirty);
    assert_eq!(
        g.changed_files, 2,
        "one untracked file plus one modified file"
    );
}

#[test]
fn a_detached_head_is_labelled_as_detached() {
    let (_d, path) = fixture();
    init_repo_with_commit(&path);
    git(&path, &["checkout", "--quiet", "--detach", "HEAD"]);

    let g = inspect(&path).git.expect("git context");
    assert!(
        g.detached,
        "a detached HEAD is reported, not shown as a branch"
    );
    assert_eq!(g.branch, None);
    assert!(g.head_short.is_some());
}

#[test]
fn an_unborn_repository_has_a_branch_but_no_head() {
    let (_d, path) = fixture();
    git(&path, &["init", "--initial-branch=main", "--quiet"]);

    let g = inspect(&path).git.expect("git context");
    assert!(
        g.unborn,
        "a repository with no commits is reported as unborn"
    );
    assert_eq!(g.branch.as_deref(), Some("main"));
    assert_eq!(
        g.head_short, None,
        "no commit means no HEAD hash is invented"
    );
    assert!(!g.detached);
}

#[test]
fn a_linked_worktree_is_identified_as_such() {
    let (_d, path) = fixture();
    let main = path.join("main");
    std::fs::create_dir(&main).unwrap();
    init_repo_with_commit(&main);

    let wt = path.join("wt-feature");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );

    let g = inspect(&wt).git.expect("git context");
    assert!(
        g.is_linked_worktree,
        "a linked worktree is distinguished from the main checkout"
    );
    assert_eq!(g.repo_root, wt.to_string_lossy());
    assert_eq!(g.branch.as_deref(), Some("feature"));
    assert!(!g.dirty);

    // The main checkout is still reported as the main one.
    let main_ctx = inspect(&main).git.expect("git context");
    assert!(!main_ctx.is_linked_worktree);
    assert_eq!(main_ctx.branch.as_deref(), Some("main"));

    // Removing the worktree directory must not corrupt the reading.
    git(
        &main,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    let after = inspect(&wt);
    assert!(
        !after.exists,
        "the removed worktree is reported as missing, not as clean"
    );
    assert!(after.git.is_none());
}

#[test]
fn a_bare_repository_is_labelled_bare_and_never_reported_dirty() {
    let (_d, path) = fixture();
    let bare = path.join("origin.git");
    std::fs::create_dir(&bare).unwrap();
    git(
        &bare,
        &["init", "--bare", "--initial-branch=main", "--quiet"],
    );

    let g = inspect(&bare).git.expect("git context");
    assert!(g.bare);
    assert!(
        !g.dirty,
        "a bare repository has no working tree to be dirty"
    );
    assert_eq!(g.changed_files, 0);
}
