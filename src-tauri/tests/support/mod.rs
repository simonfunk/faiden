//! Test-side event sink. The production sink forwards to Tauri; this one
//! records so tests can assert on real ordering and payload bounds.

#![allow(dead_code)]

pub mod agent_fixture;

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use faiden_lib::terminal::{TerminalEvent, TerminalEvents};

#[derive(Default)]
struct Inner {
    events: Vec<TerminalEvent>,
}

pub struct CollectingSink {
    inner: Mutex<Inner>,
    signal: Condvar,
}

impl CollectingSink {
    pub fn new() -> Arc<CollectingSink> {
        Arc::new(CollectingSink {
            inner: Mutex::new(Inner::default()),
            signal: Condvar::new(),
        })
    }

    pub fn events(&self) -> Vec<TerminalEvent> {
        self.inner.lock().unwrap().events.clone()
    }

    /// Blocks until `pred` holds over the recorded events, or the deadline passes.
    pub fn wait_for<F>(&self, timeout: Duration, pred: F) -> Vec<TerminalEvent>
    where
        F: Fn(&[TerminalEvent]) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut guard = self.inner.lock().unwrap();
        loop {
            if pred(&guard.events) {
                return guard.events.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!(
                    "timed out after {timeout:?} waiting for terminal events; saw {} event(s): {:?}",
                    guard.events.len(),
                    summarise(&guard.events)
                );
            }
            let (g, _) = self.signal.wait_timeout(guard, remaining).unwrap();
            guard = g;
        }
    }

    pub fn output_text(&self, terminal_id: &str) -> String {
        self.events()
            .iter()
            .filter_map(|e| match e {
                TerminalEvent::Output {
                    terminal_id: t,
                    chunk,
                    ..
                } if t == terminal_id => Some(chunk.clone()),
                _ => None,
            })
            .collect()
    }
}

impl TerminalEvents for CollectingSink {
    fn emit(&self, event: TerminalEvent) {
        let mut guard = self.inner.lock().unwrap();
        guard.events.push(event);
        self.signal.notify_all();
    }
}

fn summarise(events: &[TerminalEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            TerminalEvent::Output { seq, chunk, .. } => {
                format!("output#{seq}({} chars)", chunk.chars().count())
            }
            TerminalEvent::Exit { seq, exit_code, .. } => format!("exit#{seq}(code {exit_code})"),
        })
        .collect()
}

/// Concatenated output for one terminal. Takes the already-borrowed slice so it
/// is safe to call from inside a `wait_for` predicate.
pub fn text_of(events: &[TerminalEvent], terminal_id: &str) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            TerminalEvent::Output {
                terminal_id: t,
                chunk,
                ..
            } if t == terminal_id => Some(chunk.as_str()),
            _ => None,
        })
        .collect()
}

pub fn has_exit(events: &[TerminalEvent], terminal_id: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(e, TerminalEvent::Exit { terminal_id: t, .. } if t == terminal_id))
}

pub fn exit_of(events: &[TerminalEvent], terminal_id: &str) -> Option<(u32, bool)> {
    events.iter().find_map(|e| match e {
        TerminalEvent::Exit {
            terminal_id: t,
            exit_code,
            success,
            ..
        } if t == terminal_id => Some((*exit_code, *success)),
        _ => None,
    })
}

/// True once the process with `pid` is fully gone (reaped, not a zombie).
pub fn process_gone(pid: u32) -> bool {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
    stat.is_empty()
}

/// Runs git inside a temporary fixture with the developer's global and system
/// config neutralised, so fixtures are reproducible.
pub fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Faiden Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@localhost")
        .env("GIT_COMMITTER_NAME", "Faiden Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@localhost")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {dir:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with exactly one commit on branch `main`.
pub fn init_repo_with_commit(dir: &std::path::Path) {
    git(dir, &["init", "--initial-branch=main", "--quiet"]);
    std::fs::write(dir.join("README.md"), "fixture\n").unwrap();
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "--quiet", "-m", "initial"]);
}

pub fn wait_until<F: Fn() -> bool>(timeout: Duration, pred: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pred()
}

pub fn exit_seq_of(events: &[TerminalEvent], terminal_id: &str) -> Option<u64> {
    events.iter().find_map(|e| match e {
        TerminalEvent::Exit {
            terminal_id: t,
            seq,
            ..
        } if t == terminal_id => Some(*seq),
        _ => None,
    })
}

/// True while the process exists at all, including as an unreaped zombie.
pub fn process_alive(pid: u32) -> bool {
    !process_gone(pid)
}

/// The process group of a live process, or `None` once it is gone.
pub fn pgid_of(pid: u32) -> Option<u32> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Waits until at least `count` review items exist for `thread_id`.
pub fn wait_for_items(
    store: &faiden_lib::store::Store,
    thread_id: &str,
    count: usize,
) -> Vec<faiden_lib::store::ReviewItem> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let items = store
            .list_review_items(thread_id)
            .expect("list review items");
        if items.len() >= count {
            return items;
        }
        if Instant::now() >= deadline {
            panic!(
                "only {} review item(s) appeared, expected {count}",
                items.len()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
