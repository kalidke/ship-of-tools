// argv.rs — the agent launch recipe: argv per agent kind, resolved to absolute paths on Unix.

use std::path::{Path, PathBuf};
use super::memory::auto_memory_settings;

/// The process environment this module reads: the one place `PATH`, `HOME` and `SHELL` are read. The `_in` functions
/// take one, so a test passes its own and never changes the process's.
#[cfg_attr(windows, allow(dead_code))]
struct AgentEnv {
    path: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
    shell: Option<String>,
}

impl AgentEnv {
    fn of_process() -> Self {
        AgentEnv {
            path: std::env::var_os("PATH"),
            home: std::env::var_os("HOME").map(PathBuf::from),
            shell: std::env::var("SHELL").ok(),
        }
    }
}

/// The agent argv `sot-capsule supervise` spawns as its producer.
/// `"claude"` and `"codex"` (Unix only) each get their own launcher
/// recipe. Neither carries a resume token: every leg starts a fresh
/// conversation, which reads its handoff through `/sot-session-start`, and a
/// conversation is resumed only when asked (a reauth's `--resume <id>`,
/// [`claude_resume_argv`], or a person). `"none"` is the bare platform
/// shell; every other kind is refused, never substituted.
pub fn agent_argv(agent_kind: &str, memory_cwd: Option<&Path>) -> Result<Vec<String>, String> {
    agent_argv_in(&AgentEnv::of_process(), agent_kind, memory_cwd)
}

fn agent_argv_in(env: &AgentEnv, agent_kind: &str, memory_cwd: Option<&Path>) -> Result<Vec<String>, String> {
    match agent_kind {
        "none" => Ok(vec![none_argv(env)]),
        "claude" => claude_argv(env, memory_cwd),
        "codex" => codex_argv(env),
        other => Err(format!(
            "agent {other:?} has no capsule launcher yet (only \"claude\", \"codex\", and \"none\" are supported on this host)"
        )),
    }
}

/// The bare platform shell — Windows' own `cmd.exe`, or the user's login
/// shell (`$SHELL`, falling back to `/bin/sh`) everywhere else. One
/// shared "not Windows" arm rather than a Linux-only one: a bare shell
/// is equally the honest "no agent" placeholder on any other Unix this
/// crate happens to compile on (only [`claude_argv`]'s resolution is
/// scoped narrower, to Linux specifically).
#[cfg(windows)]
fn none_argv(_env: &AgentEnv) -> String {
    "cmd.exe".to_string()
}
#[cfg(not(windows))]
fn none_argv(env: &AgentEnv) -> String {
    login_shell(env.shell.as_deref())
}

/// The login shell: the value of `$SHELL`, or `/bin/sh` when it is unset.
#[cfg(not(windows))]
fn login_shell(shell: Option<&str>) -> String {
    shell.unwrap_or("/bin/sh").to_string()
}

/// Whether a caller already passed `--settings` (either spelling), in
/// which case [`claude_recipe`] adds none of its own: the two would
/// contend for one key and the caller's intent is the specific one.
fn caller_brought_settings(extra: &[String]) -> bool {
    extra
        .iter()
        .any(|a| a == "--settings" || a.starts_with("--settings="))
}

/// The one shared builder for claude's launch flags (ADR 0046 decision
/// 4): a capsule spawn ([`claude_argv`], no extra flags: every leg starts
/// a fresh conversation), a reauth leg ([`claude_resume_argv`],
/// `--resume <id>`) and `sotd agent-exec` (`main.rs`, the caller's own
/// flags — `--continue` is `agent-exec`'s caller's choice, never added
/// for it) build the SAME shape from here, so they can never drift
/// the way two independent copies of this argv already had (`ccb`
/// carried its own literal copy before this decision). Returns argv with
/// a LITERAL `"claude"` in position 0 — never resolved here — because
/// resolution is platform-specific ([`resolve_claude`] on Linux, the
/// daemon's own `PATH` on Windows) and each of this function's two real
/// callers already has its own resolved binary to substitute in; this
/// builder only owns the FLAG shape, not the binary path. Windows
/// absolute resolution stays out of scope (`claude_argv`'s Windows arm,
/// below, keeps the literal name); `"codex"` has its own, much smaller,
/// recipe ([`codex_argv`]) rather than sharing this one — its flag shape
/// (no skill argv) is unrelated to claude's.
fn claude_recipe(extra: &[String], memory_cwd: Option<&Path>) -> Vec<String> {
    let mut argv = vec![
        "claude".to_string(),
        "--permission-mode".to_string(),
        "auto".to_string(),
    ];
    // See [`caller_brought_settings`] for why a caller's own wins.
    if !caller_brought_settings(extra) {
        // `account_home`, never `HOME`: this module's own resolver is what
        // `spawn_detached_supervisor` uses, and it reads `USERPROFILE` on
        // Windows. A daemon started from the Windows shortcut has no `HOME`
        // at all, so reading it made the flag appear or vanish with the
        // daemon's ancestry -- absent on exactly the platform where every
        // new workspace is a capsule row.
        // The ONE `std::env` read for this flag, here rather than inside
        // `auto_memory_settings`, which stays pure over its arguments and
        // so is exercised against temp dirs instead of the real config.
        let child_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        if let Some(json) = memory_cwd
            .zip(crate::agents::accounts::account_home())
            .and_then(|(cwd, home)| {
                auto_memory_settings(&home, cwd, child_config_dir.as_deref())
            })
        {
            argv.push("--settings".to_string());
            argv.push(json);
        }
    }
    argv.extend(extra.iter().cloned());
    argv.push("/sot-session-start".to_string());
    argv
}

/// ADR 0046 decision 4, stated once for both arms below: resolving
/// `claude` to an ABSOLUTE path is a Unix thing ([`resolve_claude`]) —
/// Windows keeps the literal name from [`claude_recipe`] and relies on
/// the daemon's own `PATH` (a detached child inherits it); that stays
/// out of scope here, not a gap this decision closes.
#[cfg(windows)]
fn claude_argv(_env: &AgentEnv, memory_cwd: Option<&Path>) -> Result<Vec<String>, String> {
    Ok(claude_recipe(&[], memory_cwd))
}
/// macOS lane: widened from `target_os = "linux"` to `unix`, a DELETION
/// of the third arm that used to refuse here ("claude has no capsule
/// launcher on this host"). That refusal outlived its reason.
/// [`resolve_claude`] — the whole resolution rule — is already
/// `cfg(unix)` and already exercised on macOS by [`agent_exec_argv`],
/// which is how `ccb` itself launches there; a launcher recipe with
/// `agent-exec`'s own flag shape cannot need a narrower platform gate
/// than the resolver it calls.
#[cfg(unix)]
fn claude_argv(env: &AgentEnv, memory_cwd: Option<&Path>) -> Result<Vec<String>, String> {
    let claude = resolve_claude(env.path.as_deref(), env.home.as_deref())?;
    let mut argv = claude_recipe(&[], memory_cwd);
    argv[0] = claude;
    Ok(argv)
}

/// The reauth leg's producer argv (ADR 0046 decision 6): the SAME
/// [`claude_recipe`] every other claude leg is built from, with an
/// explicit `--resume <id>`: the one resume a row's launch ever makes.
/// `--continue` could not serve here: it resolves "the most recent
/// conversation" from `.claude.json`, which is per-account and never
/// shared (`accounts.rs`'s `SHARED_ENTRIES`), so the account being
/// switched TO either has no such selector or has one naming a different
/// conversation. The transcript itself is one file both accounts read
/// (`projects` IS shared), which is why an id is enough and nothing is
/// copied. The id is never persisted on the row: a later ordinary
/// [`claude_argv`] leg starts a fresh conversation, as every restart does.
pub fn claude_resume_argv(
    session_id: &str,
    memory_cwd: Option<&Path>,
) -> Result<Vec<String>, String> {
    claude_resume_argv_in(&AgentEnv::of_process(), session_id, memory_cwd)
}

#[cfg(unix)]
fn claude_resume_argv_in(
    env: &AgentEnv,
    session_id: &str,
    memory_cwd: Option<&Path>,
) -> Result<Vec<String>, String> {
    let claude = resolve_claude(env.path.as_deref(), env.home.as_deref())?;
    let mut argv = claude_recipe(
        &["--resume".to_string(), session_id.to_string()],
        memory_cwd,
    );
    argv[0] = claude;
    Ok(argv)
}
/// Windows twin: the literal name from [`claude_recipe`], resolved by the
/// daemon's own `PATH` exactly as [`claude_argv`]'s Windows arm is.
#[cfg(windows)]
fn claude_resume_argv_in(
    _env: &AgentEnv,
    session_id: &str,
    memory_cwd: Option<&Path>,
) -> Result<Vec<String>, String> {
    Ok(claude_recipe(
        &["--resume".to_string(), session_id.to_string()],
        memory_cwd,
    ))
}

/// `"codex"`'s capsule recipe: `ccx --capsule`, which starts a fresh
/// conversation with every `ccx` copy a host may hold: the installed
/// launcher is refreshed by `update_comm`, which a launch runs when comm's
/// stamp names another commit than the checkout's, and a released `ccx` run bare resumes by recency.
/// macOS lane: `unix`, not `target_os = "linux"` — `ccx` is the same
/// shell script installed to the same `~/.local/bin` on every Unix, so
/// the Linux gate here was naming the install layout of one host, not a
/// mechanism. The separate `not(any(windows, linux))` arm that refused
/// "codex has no capsule launcher on this host" is DELETED with it.
#[cfg(unix)]
fn codex_argv(env: &AgentEnv) -> Result<Vec<String>, String> {
    let ccx = resolve_ccx(env.path.as_deref(), env.home.as_deref())?;
    Ok(vec![ccx, "--capsule".to_string()])
}

/// Windows has no `ccx`, so a `codex` row's spawn fails with this reason
/// rather than launching something else.
#[cfg(windows)]
fn codex_argv(_env: &AgentEnv) -> Result<Vec<String>, String> {
    Err("codex has no capsule launcher on Windows (ccx is a bash script with no .ps1 counterpart)".to_string())
}

/// Unix-only argv for `sotd agent-exec <kind> [flags…]` (ADR 0046
/// decision 4) — the argv `main.rs` execs THIS process into directly,
/// never spawned as a child. Shares [`claude_recipe`] with the capsule
/// spawn path above (`agent-exec` never adds `--continue` itself; a
/// person's `ccb --continue` passes it in `extra`) but calls [`resolve_claude`]
/// DIRECTLY for `"claude"`, never through [`agent_argv`]/[`claude_argv`]:
/// those exist to gate the CAPSULE launcher, which is a narrower,
/// separate question (ADR 0043 decision 22: no validated capsule
/// resolution rule off Windows/Linux) from "can this Unix host resolve
/// and exec a real `claude` binary" — routing through them made capsule
/// availability into agent-exec availability, breaking `agent-exec`
/// (and so `ccb` itself, which execs through it) on every Unix
/// [`claude_argv`] refuses (macOS today), found by CI going red there.
/// `extra` lands between the fixed flags and the bootstrap skill, in the
/// order given — `ccb`'s own `"$@"`. Only `"claude"` has a recipe here:
/// `"none"` is a legitimate [`agent_argv`] kind for a capsule spawn (the
/// bare platform shell, no flags, no skill) but shares no recipe shape
/// with `"claude"`'s `--permission-mode`/skill argv, so `agent-exec`
/// refuses it — same as any kind [`agent_argv`] itself does not know —
/// by routing THOSE two cases (and only those) through `agent_argv`,
/// whose own error text surfaces verbatim for a truly unknown kind.
#[cfg(unix)]
pub fn agent_exec_argv(kind: &str, extra: &[String]) -> Result<Vec<String>, String> {
    agent_exec_argv_in(&AgentEnv::of_process(), kind, extra)
}

#[cfg(unix)]
fn agent_exec_argv_in(env: &AgentEnv, kind: &str, extra: &[String]) -> Result<Vec<String>, String> {
    match kind {
        "claude" => {
            let claude = resolve_claude(env.path.as_deref(), env.home.as_deref())?;
            // `agent-exec` EXECS this process in place (never spawns a
            // child), so this process's own cwd is already the directory
            // the session will run in — `ccb`'s caller chose it.
            let cwd = std::env::current_dir().ok();
            let mut recipe = claude_recipe(extra, cwd.as_deref());
            recipe[0] = claude;
            Ok(recipe)
        }
        other => {
            // Every kind besides "claude"/"none" is already Err from
            // `agent_argv` itself (propagated verbatim, `?`); "none" is
            // the one kind that succeeds there but still has no recipe
            // HERE (see doc above), so it falls through to its own
            // refusal below.
            agent_argv_in(env, other, None)?;
            Err(format!(
                "agent-exec has no recipe for {other:?} yet (only \"claude\" is supported)"
            ))
        }
    }
}

/// Any Unix (widened from Linux-only, ADR 0046 decision 4 CI fix): search
/// `path_var` (a `PATH`-shaped env value), then `<home>/.local/bin` and
/// `<home>/.claude/local`, for an executable file named `claude` — the
/// tmux launchers' own full-path rule (a daemon-spawned process inherits
/// the SERVICE's PATH, which lacks `~/.local/bin`; CLAUDE.md's own
/// documented gotcha). Returns the ABSOLUTE path so the eventual capsule
/// producer never repeats a PATH search of its own (`sot_log::capsule::producer::pty`'s
/// own `executable_is_resolvable` treats an absolute path as a direct
/// existence+executable check, never a second PATH walk). Takes its
/// inputs explicitly (never reads `std::env` itself) so it is testable
/// without mutating global process state — [`claude_argv`]'s Linux arm
/// and [`agent_exec_argv`]'s `"claude"` arm are the two real callers,
/// each supplying the process's own `PATH`/`HOME`; `claude_argv` itself
/// is `cfg(unix)` ABOVE as well (macOS lane: the narrower gate it used to
/// carry named no mechanism this resolver does not already provide, and
/// is deleted) — widening this shared helper is what let `agent_exec_argv`
/// resolve a real `claude` on any Unix in the first place.
#[cfg(unix)]
fn resolve_claude(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Result<String, String> {
    // Review round, reproduced: a RELATIVE `PATH` entry resolves against
    // the daemon's own current directory (`execve`'s own rule for a
    // relative argv[0]/PATH member) — which, for a daemon started as a
    // service, is essentially arbitrary and almost never the workspace
    // the eventual capsule producer will actually run in. Skipped
    // outright, never joined against anything: a relative entry here
    // would launch relative to whatever directory this DAEMON happens to
    // be running from, not the workspace the resolved `claude` will
    // actually be spawned into.
    let mut dirs: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|dir| dir.is_absolute())
        .collect();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
    }
    let mut searched: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let candidate = dir.join("claude");
        if is_executable_file(&candidate) {
            return Ok(candidate.to_string_lossy().into_owned());
        }
        searched.push(candidate);
    }
    Err(format!(
        "claude not found (searched: {})",
        searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    ))
}

/// [`codex_argv`]'s resolver: same PATH-then-`~/.local/bin` search as
/// [`resolve_claude`], minus its claude-only `.claude/local` fallback.
/// `cfg(unix)` alongside its one caller — nothing in this search is
/// Linux-specific.
#[cfg(unix)]
fn resolve_ccx(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Result<String, String> {
    let mut dirs: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|dir| dir.is_absolute())
        .collect();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
    }
    let mut searched: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let candidate = dir.join("ccx");
        if is_executable_file(&candidate) {
            return Ok(candidate.to_string_lossy().into_owned());
        }
        searched.push(candidate);
    }
    Err(format!(
        "ccx not found (searched: {})",
        searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    ))
}

/// `true` iff `path` is a REGULAR file (`metadata` follows symlinks —
/// review round, reproduced: a DIRECTORY named `claude` also passes
/// `access(2)`'s own `X_OK` check, since the execute bit on a directory
/// means "searchable," not "runnable as a program," so checking access
/// alone let a same-named directory earlier in `PATH` win over a real
/// executable later in it) that `access(2)` ALSO reports as executable
/// by THIS process — mirrors `sot_log::capsule::producer::pty`'s own
/// `is_executable_file`'s `access` check (the same test the eventual pty
/// producer performs before ever forking), so a path this returns is
/// never rejected there for a reason this check could have caught
/// first. A duplicated ~6 lines rather than a cross-crate refactor — not
/// worth it for this one call site. Widened alongside [`resolve_claude`],
/// its only caller, from Linux-only to any Unix.
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        _ => return false,
    }
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
#[path = "argv_tests.rs"]
mod tests;
