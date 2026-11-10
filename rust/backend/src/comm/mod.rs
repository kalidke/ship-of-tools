//! Messaging: mail delivery, the address book, the identity, the wake, and `sotd comm` (the contract is CLAUDE.md here).

pub(crate) mod cli;
pub(crate) mod home;
pub(crate) mod identity;
pub(crate) mod mail;
pub(crate) mod registry;
#[cfg(test)]
pub(crate) mod test_home;
pub(crate) mod wake;

use std::path::PathBuf;

pub(crate) use sot_platform::host::env::Env;

/// The sot-comm home directory (`~/.sot-comm` by default) — ONE resolver
/// (Codex round finding 8), shared by the daemon's own registry reads
/// (`comm::registry::registry::comm_registry_path`, used on every platform for
/// `workspace.list`) and the env it hands a spawned capsule
/// (`agents::env::capsule_supervisor_env`, which ALSO injects
/// `SOT_COMM_HOME` explicitly into the child so the scripts and the
/// daemon can never resolve two different homes for the same box).
/// `SOT_COMM_HOME` wins outright when set (an explicit override, used
/// verbatim); else `$HOME/.sot-comm` (`comm-lib.sh`'s own default,
/// matches on every platform where a shell set `$HOME`); else — Windows
/// in practice, since `USERPROFILE` is normally unset elsewhere —
/// `%USERPROFILE%\.sot-comm`. Genuinely cross-platform (unlike
/// `windows_state_root` above): a Linux/macOS daemon reads the SAME
/// comm registry a Unix comm session writes, so this is never
/// `#[cfg(windows)]`-gated. `None` only when none of the three are set. `comm-lib.sh`'s `COMM_HOME` is the
/// same rule in shell (`$HOME` is the user profile under Git Bash).
pub(crate) fn sot_comm_home() -> Option<PathBuf> {
    sot_comm_home_in(&Env::process())
}

/// [`sot_comm_home`] reading the three variables from `env`.
pub(crate) fn sot_comm_home_in(env: &Env) -> Option<PathBuf> {
    let home = comm_home_rule(env)?;
    #[cfg(test)]
    assert_scratch(&home);
    Some(home)
}

/// The resolution rule alone, with no test guard: what the rule's own tests read.
fn comm_home_rule(env: &Env) -> Option<PathBuf> {
    if let Some(v) = env.get_nonempty("SOT_COMM_HOME") {
        return Some(PathBuf::from(v));
    }
    if let Some(h) = env.get_nonempty("HOME") {
        return Some(PathBuf::from(h).join(".sot-comm"));
    }
    env.get_nonempty("USERPROFILE").map(|u| PathBuf::from(u).join(".sot-comm"))
}

/// Under test, the one resolver refuses a comm folder that is not beneath the temporary folder, so a test that
/// forgot to isolate its comm home fails by name instead of pruning or locking a live folder.
#[cfg(test)]
pub(crate) fn assert_scratch(home: &std::path::Path) {
    let abs = |p: &std::path::Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let tmp = std::env::temp_dir();
    let beneath = |t: &std::path::Path| abs(home).starts_with(abs(t));
    let real = |p: &std::path::Path| std::fs::canonicalize(p).ok();
    assert!(
        beneath(&tmp) || real(&tmp).is_some_and(|t| beneath(&t)),
        "a test resolved the comm folder {} outside the temporary folder: it must set SOT_COMM_HOME to a folder it made",
        home.display()
    );
}

/// A blocking task's join result, with a panic in the task surfaced under test: the callers that prune the registry
/// treat a failed task as a prune that found nothing, which would swallow the guard above.
pub(crate) fn joined<T>(r: Result<T, tokio::task::JoinError>) -> Result<T, tokio::task::JoinError> {
    #[cfg(test)]
    if let Err(e) = r {
        return if e.is_panic() { std::panic::resume_unwind(e.into_panic()) } else { Err(e) };
    }
    r
}

#[cfg(test)]
#[path = "sot_comm_home_tests.rs"]
mod sot_comm_home_tests;
