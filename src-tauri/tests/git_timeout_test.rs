//! A directory whose Git path blocks must not block Faiden.
//!
//! This binary contains exactly one test on purpose: it replaces `PATH` for the
//! whole process so that `git` resolves to a stub that hangs, which is only
//! safe without another test running beside it.

use std::time::{Duration, Instant};

use faiden_lib::environment::{inspect_directory, GIT_TIMEOUT};
use tempfile::TempDir;

/// Long enough that a missing timeout is unambiguous, short enough that the
/// failing case still terminates on its own.
const STUB_SLEEP: Duration = Duration::from_secs(20);

#[test]
fn a_git_command_that_never_returns_is_bounded_and_reported_as_unavailable() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let stub = bin.join("git");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\nexec sleep {}\n", STUB_SLEEP.as_secs()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let workdir = root.join("project");
    std::fs::create_dir(&workdir).unwrap();

    let original = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", bin.display(), original));

    let started = Instant::now();
    let ctx = inspect_directory(workdir.to_str().unwrap()).expect("inspection still succeeds");
    let elapsed = started.elapsed();

    std::env::set_var("PATH", original);

    assert!(
        elapsed < STUB_SLEEP,
        "a blocking Git call must be bounded, not waited out; took {elapsed:?}"
    );
    assert!(
        elapsed < GIT_TIMEOUT * 3,
        "the bound is per command and must not accumulate across probes; took {elapsed:?}"
    );

    // The directory itself is still described truthfully.
    assert!(ctx.exists);
    assert!(ctx.is_directory);

    let reason = ctx.git_unavailable.expect(
        "a Git call that timed out must be reported as unavailable, not silently \
         turned into 'not a repository'",
    );
    let lower = reason.to_lowercase();
    assert!(
        lower.contains("timed out") || lower.contains("timeout"),
        "the reason states what actually happened, got: {reason}"
    );
    assert!(
        ctx.git.is_none(),
        "no Git facts are invented when Git could not be consulted"
    );

    // The stub must not be left running.
    let leftovers = std::process::Command::new("/bin/sh")
        .args(["-c", "pgrep -f 'sleep 20' | wc -l"])
        .output()
        .unwrap();
    let count: usize = String::from_utf8_lossy(&leftovers.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    assert_eq!(count, 0, "the timed-out Git child is killed, not abandoned");
}
