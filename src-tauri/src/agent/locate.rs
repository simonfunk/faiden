//! Finding the Hermes the user installed, and deciding what environment its
//! child process gets.
//!
//! Faiden never installs Hermes, never authenticates on the user's behalf and
//! never writes to `~/.hermes`. If nothing is found, the error names every
//! directory that was searched and stops there.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::AppError;

/// The executable name the launcher installs.
pub const PROGRAM_NAME: &str = "hermes";

/// Approval-bypass variables that must not be inherited by the agent.
///
/// Each one is a real switch in the installed Hermes, verified in its source:
///
/// * `HERMES_YOLO_MODE` — frozen at import in `tools/approval.py` and bypasses
///   dangerous-command approval wholesale.
/// * `HERMES_ACP_AUTO_APPROVE` — an ACP-specific profile key
///   (`hermes_cli/config.py`, `hermes_cli/env_loader.py`).
/// * `HERMES_NONINTERACTIVE` — routes setup away from asking a human
///   (`hermes_cli/setup.py`).
/// * `HERMES_EXEC_ASK` — selects the gateway approval queue rather than the
///   interactive callback ACP installs (`acp_adapter/server.py`).
/// * `HERMES_CRON_SESSION` — marks the run as unattended cron
///   (`acp_adapter/server.py` masks it explicitly for the same reason).
///
/// Nothing here is invented: Faiden removes inherited values, and never *sets*
/// a flag of its own. A bypass the user configured inside their own
/// `~/.hermes/.env` or `config.yaml` is loaded by the adapter itself and is
/// outside Faiden's control; the UI says so rather than implying a sandbox.
pub const BYPASS_ENV_VARS: &[&str] = &[
    "HERMES_YOLO_MODE",
    "HERMES_ACP_AUTO_APPROVE",
    "HERMES_NONINTERACTIVE",
    "HERMES_EXEC_ASK",
    "HERMES_CRON_SESSION",
];

/// Where to look. Injected rather than read from the process so the search is
/// testable without touching the developer's own machine state.
#[derive(Debug, Clone, Default)]
pub struct Lookup {
    pub path_var: Option<String>,
    pub home: Option<PathBuf>,
}

impl Lookup {
    pub fn from_env() -> Lookup {
        Lookup {
            path_var: std::env::var("PATH").ok(),
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HermesInstall {
    pub program: PathBuf,
    /// Where it was found, for display. Provenance, not a capability claim.
    pub source: String,
}

/// Directories a login shell would have on `PATH` but a Finder-launched app
/// does not inherit. `~/.local/bin` is where the Hermes launcher installs.
pub fn fallback_directories(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".hermes/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

pub fn locate_hermes(lookup: &Lookup) -> Result<HermesInstall, AppError> {
    let mut searched: Vec<String> = Vec::new();

    for dir in path_entries(lookup.path_var.as_deref()) {
        searched.push(dir.to_string_lossy().to_string());
        if let Some(program) = executable_in(&dir) {
            return Ok(HermesInstall {
                program,
                source: format!("PATH entry {}", dir.display()),
            });
        }
    }

    for dir in fallback_directories(lookup.home.as_deref()) {
        searched.push(dir.to_string_lossy().to_string());
        if let Some(program) = executable_in(&dir) {
            return Ok(HermesInstall {
                program,
                source: format!("{} (not on this app's PATH)", dir.display()),
            });
        }
    }

    Err(AppError::not_found(
        "hermes executable",
        format!(
            "no executable named `{PROGRAM_NAME}` in any of: {}. Install Hermes and make it \
             runnable from your shell first — Faiden does not install or configure it for you",
            searched.join(", ")
        ),
    ))
}

fn path_entries(path_var: Option<&str>) -> Vec<PathBuf> {
    path_var
        .unwrap_or("")
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn executable_in(dir: &Path) -> Option<PathBuf> {
    let candidate = dir.join(PROGRAM_NAME);
    is_executable_file(&candidate).then_some(candidate)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The environment the child gets: everything inherited, minus the approval
/// bypasses. Credentials and ordinary configuration are left alone, because
/// authentication deliberately stays with the installed harness.
pub fn scrubbed_env<I>(inherited: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    inherited
        .into_iter()
        .filter(|(name, _)| !BYPASS_ENV_VARS.contains(&name.as_str()))
        .collect()
}

/// The child's `PATH`: the user's own entries first, then the fallbacks, with
/// duplicates removed. Hermes shells out to its own helpers, so a Finder-launch
/// PATH that lacks `~/.local/bin` would break it even once we found it.
pub fn augmented_path(path_var: Option<&str>, home: Option<&Path>) -> String {
    let mut entries: Vec<PathBuf> = path_entries(path_var);
    for dir in fallback_directories(home) {
        if !entries.contains(&dir) {
            entries.push(dir);
        }
    }
    entries
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

/// Convenience for the runtime: the full child environment, with `PATH`
/// replaced by [`augmented_path`].
pub fn child_env(lookup: &Lookup) -> Vec<(String, String)> {
    let mut env = scrubbed_env(std::env::vars());
    let path = augmented_path(lookup.path_var.as_deref(), lookup.home.as_deref());
    env.retain(|(name, _)| !OsStr::new(name).eq_ignore_ascii_case(OsStr::new("PATH")));
    env.push(("PATH".to_string(), path));
    env
}
