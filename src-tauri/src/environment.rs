//! Directory and Git inspection for a thread's selected environment.
//!
//! Everything here describes a *user-selected* directory read at a stated
//! moment. It is never a claim about where any subprocess, agent or SSH session
//! is actually working: a shell can `cd` and we do not observe that.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::clock::MonotonicClock;
use crate::error::AppError;

pub const PROVENANCE_USER_SELECTED: &str = "user-selected";
const PROVENANCE_NOTE: &str = "Directory chosen by you in Faiden and read just now. This is not an observed process working directory: a running shell may have changed its cwd.";

const GIT_BIN: &str = "git";

/// How long a single Git subcommand may run before it is killed. A repository
/// whose status path blocks — a filesystem monitor, a credential helper, a
/// network mount — must not be able to hold the environment view hostage.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitContext {
    pub repo_root: String,
    pub branch: Option<String>,
    pub head_short: Option<String>,
    pub detached: bool,
    pub unborn: bool,
    pub bare: bool,
    pub is_linked_worktree: bool,
    pub dirty: bool,
    pub changed_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryContext {
    /// The path exactly as it was selected.
    pub path: String,
    pub exists: bool,
    pub is_directory: bool,
    pub provenance: String,
    pub note: String,
    pub observed_at: i64,
    pub git: Option<GitContext>,
    /// Why Git could not be consulted at all. This is deliberately distinct
    /// from `git: None`, which is the positive fact "not a repository".
    pub git_unavailable: Option<String>,
}

fn clock() -> &'static MonotonicClock {
    use std::sync::OnceLock;
    static CLOCK: OnceLock<MonotonicClock> = OnceLock::new();
    CLOCK.get_or_init(MonotonicClock::new)
}

pub fn inspect_directory(path: &str) -> Result<DirectoryContext, AppError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation("path", "must not be blank"));
    }
    let p = Path::new(trimmed);
    if !p.is_absolute() {
        return Err(AppError::validation("path", "must be an absolute path"));
    }

    let metadata = std::fs::metadata(p).ok();
    let exists = metadata.is_some();
    let is_directory = metadata.map(|m| m.is_dir()).unwrap_or(false);

    let (git, git_unavailable) = if is_directory {
        match read_git(p) {
            Ok(g) => (g, None),
            Err(reason) => (None, Some(reason)),
        }
    } else {
        (None, None)
    };

    Ok(DirectoryContext {
        path: trimmed.to_string(),
        exists,
        is_directory,
        provenance: PROVENANCE_USER_SELECTED.to_string(),
        note: PROVENANCE_NOTE.to_string(),
        observed_at: clock().now_ms(),
        git,
        git_unavailable,
    })
}

/// What a single Git probe produced.
#[derive(Debug)]
enum Probe {
    /// Git ran and exited zero.
    Ok(String),
    /// Git ran and exited non-zero. That is itself a fact — "not a repository",
    /// "no HEAD yet" — and not a failure to consult Git.
    Refused,
    /// Git could not be consulted at all: missing, unrunnable, or killed for
    /// outliving [`GIT_TIMEOUT`].
    Unavailable(String),
}

/// Runs a git subcommand in `dir` under a bounded wait.
fn git(dir: &Path, args: &[&str]) -> Probe {
    let mut cmd = Command::new(GIT_BIN);
    cmd.arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        // Keep the user's own config: their excludes and hooks are real. Only
        // interactivity is suppressed so a prompt can never block the UI.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    run_bounded(cmd, GIT_TIMEOUT)
}

/// Spawns `cmd` and waits for it for at most `timeout`, killing it if it
/// overruns.
///
/// Stdout is drained on a helper thread rather than after the wait, so a child
/// that fills the pipe cannot deadlock the bound. Only the child Faiden spawned
/// is killed: it shares this process's group, so signalling the group would
/// mean signalling Faiden.
fn run_bounded(mut cmd: Command, timeout: Duration) -> Probe {
    const POLL: Duration = Duration::from_millis(5);
    /// How long the drained output is waited for once the child is gone.
    const DRAIN: Duration = Duration::from_millis(500);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Probe::Unavailable(format!("git could not be started: {e}")),
    };

    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut out) = child.stdout.take() {
        // Detached deliberately: it ends at EOF, which killing the child forces.
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    } else {
        drop(tx);
    }

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Probe::Unavailable(format!("git could not be waited for: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Probe::Unavailable(format!(
                "git timed out after {}s in this directory and was stopped",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    };

    if !status.success() {
        return Probe::Refused;
    }
    let bytes = rx.recv_timeout(DRAIN).unwrap_or_default();
    Probe::Ok(
        String::from_utf8_lossy(&bytes)
            .trim_end_matches('\n')
            .trim()
            .to_string(),
    )
}

/// A probe whose non-zero exit is a meaningful answer rather than an error.
fn optional(probe: Probe) -> Result<Option<String>, String> {
    match probe {
        Probe::Ok(s) => Ok(Some(s)),
        Probe::Refused => Ok(None),
        Probe::Unavailable(why) => Err(why),
    }
}

/// `Ok(None)` means "not a repository". `Err` means Git could not be consulted
/// at all, which is never silently turned into "not a repository".
fn read_git(dir: &Path) -> Result<Option<GitContext>, String> {
    // `--absolute-git-dir` refusing is the authoritative "not a repository".
    let git_dir = match git(dir, &["rev-parse", "--absolute-git-dir"]) {
        Probe::Ok(s) => s,
        Probe::Refused => return Ok(None),
        Probe::Unavailable(why) => return Err(why),
    };
    let bare =
        optional(git(dir, &["rev-parse", "--is-bare-repository"]))?.as_deref() == Some("true");

    let common_dir = optional(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ))?
    .unwrap_or_else(|| git_dir.clone());
    let is_linked_worktree = !bare && normalise(&common_dir) != normalise(&git_dir);

    let repo_root = if bare {
        git_dir.clone()
    } else {
        optional(git(dir, &["rev-parse", "--show-toplevel"]))?.unwrap_or_else(|| git_dir.clone())
    };

    // `symbolic-ref` still answers on an unborn branch; refusing means detached.
    let branch = optional(git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]))?
        .filter(|b| !b.is_empty());
    let head_short =
        optional(git(dir, &["rev-parse", "--short", "HEAD"]))?.filter(|h| !h.is_empty());
    let unborn = head_short.is_none() && branch.is_some();
    let detached = branch.is_none() && head_short.is_some();

    let (dirty, changed_files) = if bare {
        (false, 0)
    } else {
        let status = optional(git(
            dir,
            &["status", "--porcelain", "--untracked-files=normal"],
        ))?
        .unwrap_or_default();
        let count = status.lines().filter(|l| !l.trim().is_empty()).count();
        (count > 0, count)
    };

    Ok(Some(GitContext {
        repo_root,
        branch,
        head_short,
        detached,
        unborn,
        bare,
        is_linked_worktree,
        dirty,
        changed_files,
    }))
}

/// Resolves symlinks where possible so `/var` and `/private/var` compare equal.
fn normalise(path: &str) -> PathBuf {
    let p = Path::new(path);
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        cmd
    }

    #[test]
    fn a_bounded_run_returns_a_fast_commands_output() {
        match run_bounded(sh("printf 'on-a-branch\\n'"), Duration::from_secs(5)) {
            Probe::Ok(s) => assert_eq!(s, "on-a-branch"),
            other => panic!("expected output, got {other:?}"),
        }
    }

    #[test]
    fn a_non_zero_exit_is_an_answer_not_an_outage() {
        assert!(matches!(
            run_bounded(sh("exit 128"), Duration::from_secs(5)),
            Probe::Refused
        ));
    }

    #[test]
    fn an_overrunning_child_is_killed_and_reported_as_unavailable() {
        let started = Instant::now();
        let probe = run_bounded(sh("exec sleep 30"), Duration::from_millis(200));
        let elapsed = started.elapsed();

        match probe {
            Probe::Unavailable(why) => assert!(
                why.contains("timed out"),
                "the reason says what happened, got: {why}"
            ),
            other => panic!("an overrunning child must not be waited out, got {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(5),
            "the wait is bounded by the timeout, not by the child; took {elapsed:?}"
        );
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_cannot_deadlock_the_bound() {
        // 1 MiB is far beyond the 64 KiB pipe buffer: draining has to happen
        // while the child is still running.
        match run_bounded(
            sh("dd if=/dev/zero bs=1024 count=1024 2>/dev/null | tr '\\000' 'x'"),
            Duration::from_secs(10),
        ) {
            Probe::Ok(s) => assert_eq!(s.len(), 1024 * 1024),
            other => panic!("a large writer must still complete, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_binary_is_unavailable_rather_than_not_a_repository() {
        let mut cmd = Command::new("/nonexistent/faiden-not-a-real-binary");
        cmd.stdout(Stdio::piped());
        assert!(matches!(
            run_bounded(cmd, Duration::from_secs(5)),
            Probe::Unavailable(_)
        ));
    }
}
