// capsule_workspace.rs — ADR 0042 slice L1a / ADR 0043 decision 22: the
// daemon's capsule workspace runtime, on Windows AND Linux. One
// `sot-capsule supervise <state-dir>` authority per capsule workspace,
// spawned DETACHED so it survives the daemon's own exit — the daemon is
// never its kill domain. Which platform is chosen is exactly THREE
// forks inside `mod runtime` (the capsule executable's name, the detach
// mechanism, and the adopted leg's exit-status read) — everything else
// in that module is byte-identical on both platforms. `runtime: "tmux"`
// rows stay exactly what they are today; this module never touches
// them, and the Linux default row is STILL "tmux" (ADR 0043 decision
// 22: attach for a capsule row is same-machine-only until the bridge) —
// `workspace.create`'s own explicit `runtime` field is the only way to
// ask for a capsule row there today.
//
// Split deliberately into PURE helpers (no OS call: the state-dir path
// arithmetic, the phase-to-wire-string mapping, the agent argv choice)
// and the platform runtime (spawning, watching, querying, ending a
// supervisor over `sot_log::supervisor_client`). The pure half is
// compiled and unit-tested on every platform — ADR 0042 L1a's own gate
// runs `cargo test --workspace` on Linux, and gating path/string
// arithmetic behind `#[cfg(windows)]` would only prevent that gate from
// ever exercising it. On a host that is neither Windows nor Linux
// nothing in this module is called at all: `workspace.create` keeps
// today's tmux path unchanged (see `workspaces.rs`/`handlers.rs`).

use std::path::{Path, PathBuf};

/// The env-var hint named in every "could not resolve this machine's
/// state root" error text (`server.rs`'s boot resume-scan and `pty.open`
/// start-on-attach; `handlers.rs`'s create/destroy/list capsule gates) —
/// ONE shared constant so the two platforms' wording can never drift out
/// of step with `sot_log::state_dir::sot_state_dir`'s own actual
/// resolution order (`%LOCALAPPDATA%` on Windows; `$XDG_STATE_HOME` or
/// `$HOME` elsewhere). LU5a: the non-Windows arm is `not(windows)`, not
/// `target_os = "linux"` — [`qualified_state_root`]'s own BODY is
/// portable (it must at least TYPECHECK on every host `mod
/// capsule_workspace` compiles for, macOS included) and references this
/// constant unconditionally, so it must exist wherever that function's
/// body does; the text is identical to what the Linux-only arm already
/// said, since `sot_state_dir`'s own resolution order is the same on
/// every non-Windows host. Every ACTUAL caller stays gated to Windows and
/// Linux (`#[cfg(any(windows, target_os = "linux"))]`, matching the
/// capsule runtime's own availability) — macOS never reaches this text at
/// all, hence the `allow(dead_code)` below on the arm that serves it.
#[cfg(windows)]
pub(crate) const STATE_ROOT_HINT: &str = "%LOCALAPPDATA%";
#[cfg(not(windows))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const STATE_ROOT_HINT: &str = "$XDG_STATE_HOME or $HOME";

/// `<state-root>/workspaces/<workspace_id>/` — the capsule's own state
/// directory (ADR 0041/0042: `supervisor.lock`, `drawer.voyage`, the
/// journal, the voyages — all owned and created by `sot-capsule
/// supervise` itself, as its own first act after it actually runs; rule
/// C, shrink round: this daemon never creates it — a synchronous spawn
/// failure then leaves nothing on disk at all). `state_root` is
/// `sot_log::state_dir::sot_state_dir()`, injected rather than resolved
/// here so this stays a pure function of its inputs (real callers
/// resolve it once; a test supplies a tempdir root).
pub fn state_dir_for(state_root: &Path, workspace_id: &str) -> PathBuf {
    state_root.join("workspaces").join(workspace_id)
}

/// ADR 0043 decision 23: the ONE seam a capsule launch passes through
/// before it is ever allowed to touch disk on the row's behalf — called by
/// `handlers.rs`'s `workspace.create` so it can refuse an unqualified root
/// BEFORE any row persists, and again by [`runtime::spawn_detached_supervisor`]
/// — the one mechanism every later launch (attach start-on-attach, boot
/// resume, the watchdog's own restart) shares. This FUNCTION is portable
/// (no platform gate on the item
/// itself — every host `mod capsule_workspace` compiles for must be able
/// to typecheck it), but every real CALLER stays gated to Windows and
/// Linux exactly like the capsule runtime's own availability elsewhere in
/// this module — macOS never actually calls this (`allow(dead_code)`
/// below). Three steps:
///
/// 1. [`sot_log::state_dir::sot_state_dir`] resolves the root at all —
///    else the same [`STATE_ROOT_HINT`] wording every other "could not
///    resolve this machine's state root" caller already uses.
/// 2. Created if missing (this daemon's OWN private-dir helper — rule C
///    is about per-CAPSULE directories, never the daemon's own root) and
///    canonicalized: everything downstream judges the RESOLVED
///    destination, a symlink or a nested mount included, never `$HOME` by
///    inference.
/// 3. On Linux, `statfs` the resolved root and refuse VOLATILE types
///    (tmpfs, ramfs) — a SECOND, daemon-side deny list answering a
///    different question than [`sot_log::state_dir::preflight_volume`]'s own
///    remote-fs one: that one asks whether the store's primitives work at
///    all (tmpfs passes — see its own doc); this one asks whether the
///    root is durable enough to keep RESUMING capsule rows from across a
///    daemon restart, which tmpfs/ramfs answer no to regardless of how
///    well they support rename/fsync. Then `preflight_volume` itself,
///    mapped to its `Display` text. Windows: unchanged — the existing
///    NTFS-only arm already refuses everything this would and more.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn qualified_state_root() -> Result<PathBuf, String> {
    let root = sot_log::state_dir::sot_state_dir()
        .ok_or_else(|| format!("could not resolve this machine's state root ({STATE_ROOT_HINT} unset)"))?;
    crate::paths::ensure_private_dir(&root)
        .map_err(|e| format!("could not create the state root {root:?}: {e}"))?;
    let root = std::fs::canonicalize(&root)
        .map_err(|e| format!("could not resolve the state root {root:?}: {e}"))?;
    #[cfg(target_os = "linux")]
    {
        let f_type = linux_only::statfs_type(&root)
            .map_err(|e| format!("could not statfs the state root {root:?}: {e}"))?;
        if let Some(name) = linux_only::volatile_fs_name(f_type) {
            return Err(format!(
                "state root {root:?} is on {name}: capsule records need durable, non-volatile \
                 storage (set XDG_STATE_HOME to a local disk; ADR 0043 decision 23)"
            ));
        }
    }
    sot_log::state_dir::preflight_volume(&root).map_err(|e| e.to_string())?;
    Ok(root)
}

/// LU5a: the daemon's OWN volatile-filesystem deny list plus the raw
/// `statfs` call it is judged against — split into its own tiny module so
/// [`qualified_state_root`] above (portable) can gate just this one
/// `#[cfg(target_os = "linux")]` block rather than the whole function.
#[cfg(target_os = "linux")]
mod linux_only {
    use std::path::Path;

    /// Volatile filesystem magic numbers `statfs(2)` can report — refused
    /// on the state root itself regardless of how well they support the
    /// store's own primitives (tmpfs/ramfs support `RENAME_NOREPLACE`
    /// and directory fsync fine — [`sot_log::state_dir::preflight_volume`]'s
    /// own remote-fs deny list does NOT refuse them, deliberately, since
    /// developer `/tmp` is often tmpfs). A durable capsule RECORD ROOT is
    /// a different, stricter question this second list answers.
    const VOLATILE_FS_TYPES: &[(i64, &str)] = &[
        (0x0102_1994, "tmpfs"),
        (0x8584_58F6u32 as i64, "ramfs"),
    ];

    /// Pure lookup, mirroring `sot_log::fsutil`'s own `remote_fs_name` —
    /// unit-tested directly against the constants above.
    pub(super) fn volatile_fs_name(f_type: i64) -> Option<&'static str> {
        VOLATILE_FS_TYPES.iter().find(|(magic, _)| *magic == f_type).map(|(_, name)| *name)
    }

    pub(super) fn statfs_type(dir: &Path) -> std::io::Result<i64> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "nul byte in state root path"))?;
        let mut buf: std::mem::MaybeUninit<libc::statfs> = std::mem::MaybeUninit::uninit();
        // SAFETY: `buf` is a valid out-param for `statfs(2)`, read back
        // only once the call itself has reported success.
        let rc = unsafe { libc::statfs(c_dir.as_ptr(), buf.as_mut_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let buf = unsafe { buf.assume_init() };
        Ok(buf.f_type as i64)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn volatile_fs_name_matches_the_constants_only() {
            assert_eq!(volatile_fs_name(0x0102_1994), Some("tmpfs"));
            assert_eq!(volatile_fs_name(0x8584_58F6u32 as i64), Some("ramfs"));
            assert_eq!(volatile_fs_name(0x6969), None, "NFS is fsutil's own remote-fs list, not this one");
        }
    }
}

/// The agent argv `sot-capsule supervise` spawns as its producer (ADR
/// 0042 L1a: "the same launcher the drawer's autostart uses when
/// `autostart_claude` is set; otherwise the platform shell"). No Windows
/// equivalent of the Unix `ccb`/`ccx` launchers exists anywhere in this
/// repo (both are bash scripts — `comm/adapters/claude/bin/ccb`,
/// `comm/adapters/codex/bin/ccx` — with no `.ps1`/`.exe` counterpart, and
/// ADR 0041's own drawer capsule is deliberately a RAW TERMINAL voyage,
/// not yet wired to any launcher either — U4, the drawer cutover, is
/// still unbuilt). `"claude"` gets the closest honest equivalent: the
/// same flags `ccb` itself execs with (`claude --permission-mode auto
/// /sot-session-start`); on Windows this relies on `claude` being on the
/// daemon's own PATH (a detached child inherits it, same as any spawned
/// process), on Linux it is resolved to an ABSOLUTE path first
/// ([`resolve_claude`] — the tmux launchers' own full-path rule: a
/// daemon-spawned process inherits the SERVICE's PATH, which lacks
/// `~/.local/bin`). `"none"` is the explicit bare platform shell. Every
/// other kind (`"codex"` included — no known launcher exists on either
/// platform) is REFUSED (ADR 0042 L1a, Codex review finding 9): silently
/// substituting a bare shell for a kind the caller explicitly asked for
/// would launch something the caller never requested and never learn
/// about it.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn agent_argv(agent_kind: &str) -> Result<Vec<String>, String> {
    match agent_kind {
        "none" => Ok(vec![none_argv()]),
        "claude" => claude_argv(),
        other => Err(format!(
            "agent {other:?} has no capsule launcher yet (only \"claude\" and \"none\" are supported on this host)"
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
fn none_argv() -> String {
    "cmd.exe".to_string()
}
#[cfg(not(windows))]
fn none_argv() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[cfg(windows)]
fn claude_argv() -> Result<Vec<String>, String> {
    Ok(vec![
        "claude".to_string(),
        "--permission-mode".to_string(),
        "auto".to_string(),
        "/sot-session-start".to_string(),
    ])
}
#[cfg(target_os = "linux")]
fn claude_argv() -> Result<Vec<String>, String> {
    let claude = resolve_claude(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )?;
    Ok(vec![
        claude,
        "--permission-mode".to_string(),
        "auto".to_string(),
        "/sot-session-start".to_string(),
    ])
}
/// ADR 0043 decision 22: no known Windows-style launcher exists for
/// `claude` on any Unix other than Linux either — macOS stays
/// experimental (ADR 0043 §"Open for the maintainer"), so this refuses
/// rather than guessing a resolution rule nothing has validated there.
#[cfg(not(any(windows, target_os = "linux")))]
fn claude_argv() -> Result<Vec<String>, String> {
    Err("claude has no capsule launcher on this host".to_string())
}

/// Linux only: search `path_var` (a `PATH`-shaped env value), then
/// `<home>/.local/bin` and `<home>/.claude/local`, for an executable
/// file named `claude` — the tmux launchers' own full-path rule (a
/// daemon-spawned process inherits the SERVICE's PATH, which lacks
/// `~/.local/bin`; CLAUDE.md's own documented gotcha). Returns the
/// ABSOLUTE path so the eventual capsule producer never repeats a PATH
/// search of its own (`sot_log::producer_pty`'s own
/// `executable_is_resolvable` treats an absolute path as a direct
/// existence+executable check, never a second PATH walk). Takes its
/// inputs explicitly (never reads `std::env` itself) so it is testable
/// without mutating global process state — [`claude_argv`] is the one
/// real caller, which supplies the process's own `PATH`/`HOME`.
#[cfg(target_os = "linux")]
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

/// `true` iff `path` is a REGULAR file (`metadata` follows symlinks —
/// review round, reproduced: a DIRECTORY named `claude` also passes
/// `access(2)`'s own `X_OK` check, since the execute bit on a directory
/// means "searchable," not "runnable as a program," so checking access
/// alone let a same-named directory earlier in `PATH` win over a real
/// executable later in it) that `access(2)` ALSO reports as executable
/// by THIS process — mirrors `sot_log::producer_pty`'s own
/// `is_executable_file`'s `access` check (the same test the eventual pty
/// producer performs before ever forking), so a path this returns is
/// never rejected there for a reason this check could have caught
/// first. A duplicated ~6 lines rather than a cross-crate refactor — not
/// worth it for this one call site.
#[cfg(target_os = "linux")]
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

/// `sot-capsule supervise`'s own start-mode flag.
pub fn mode_flag(mode: StartMode) -> &'static str {
    match mode {
        StartMode::Start => "--start",
        StartMode::Resume => "--resume",
    }
}

/// Mirrors `sot_log::supervisor::StartMode` (portable re-statement: that
/// type lives in a platform-gated module, and this crate's own pure
/// tests need to name a mode without pulling in a platform-specific
/// type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Start,
    Resume,
}

/// The wire phase string `workspace.list` reports (`WorkspaceListEntry.phase`)
/// for a capsule workspace whose supervisor lane could not be reached at
/// all — connect refused, an undetermined challenge, or a timeout (ADR
/// 0042 L1a: "failure -> unreachable"). Distinct from every
/// [`sot_log::wire::SupervisorPhase`] variant, which are all states of a
/// lane that DID answer, and from [`FOREIGN_PHASE`] (ADR 0030 §8 decision
/// 31c) below — a challenge that specifically proves foreign now gets its
/// own phase rather than folding in here.
pub const UNREACHABLE_PHASE: &str = "unreachable";

/// The wire phase string for a capsule workspace whose supervisor lane
/// DID answer — but with `version_skew`: it is held by a supervisor
/// speaking ANOTHER lane protocol (ADR 0030 §8 decision 31c, ADR 0043
/// decision 31; ADR 0045 decision 7 — the gate is `proto` alone now, not
/// build; decision 9: adopting a supervisor of another BUILD is
/// ordinary, only a proto mismatch is foreign). This daemon can never
/// attach, adopt, end, or destroy such a row: end the row from a client
/// of the proto it speaks, or kill only its `sot-capsule supervise`
/// process and attach again — the leg and the agent in it survive and
/// the next attach's `ensure_started` resumes and adopts. Deliberately
/// its own phase rather than folding into [`UNREACHABLE_PHASE`]: the
/// lane DID answer, which is exactly the fact
/// `note_if_foreign` already detected and used to be discarded one line
/// before the wire (2026-09-08 field incident) — this is that fact,
/// finally on the wire. `phase_of` sets it on the SAME branch
/// `note_if_foreign` already recognizes by text-matching "foreign" in
/// `query_status`'s error — no new detection, only a new destination for
/// a fact this daemon already had.
pub const FOREIGN_PHASE: &str = "foreign";

/// The wire phase string for a capsule workspace with no published
/// voyage pointer (`<state_dir>/drawer.voyage`, `sot_log::pointer` —
/// ADR 0041 Lifecycle's write-once durable fact that a voyage exists).
/// Rule B (shrink round): the POINTER, not directory presence, is the
/// discriminator — a state directory can exist with no pointer ever
/// published to it (the exact pre-pointer crash window ADR 0041 names,
/// or simply a row `resume_all` correctly never touched because it had
/// none), and that reads identically to a workspace whose directory was
/// never created at all: neither has ever had a real run. Distinct from
/// [`UNREACHABLE_PHASE`]: a workspace WITH a published pointer means a
/// supervisor did reach a real run at least once, so a lane that fails to
/// answer against it stays "unreachable" — `query_status`'s own doc
/// deliberately folds every such failure (connect refused, a foreign/
/// undetermined challenge, a timeout) into one `Err` without saying
/// which, so a query against a workspace WITH a pointer can never be
/// reclassified as "never started" either. One narrow race this accepts
/// (Codex round, PR #172): a `workspace.list` landing in the brief window
/// where a resumed supervisor's pointer is still being (re-)published
/// reads "stopped" too — bounded by the spawn call itself and
/// self-correcting on the very next list once the pointer (and the
/// supervisor behind it) exists.
#[cfg_attr(not(windows), allow(dead_code))]
pub const NEVER_STARTED_PHASE: &str = "stopped";

/// Whether a capsule workspace's supervisor lane is even worth querying,
/// given whether its voyage pointer exists — pure, no I/O itself (the
/// caller supplies `pointer_exists`, e.g. `phase_of`'s own
/// `sot_log::pointer::pointer_path(state_dir).is_file()`). `None` means
/// "query it, we can't tell from this alone"; `Some(..)` short-circuits a
/// connect attempt that cannot possibly succeed — no pipe was ever bound
/// for a workspace whose pointer was never published, so `phase_of` skips
/// straight to [`NEVER_STARTED_PHASE`] rather than waiting out a connect
/// budget destined to fail. The pointer lives INSIDE the state dir, so
/// its absence subsumes "no state dir at all" (the check this replaces)
/// as well as "a state dir exists but nothing was ever durably published
/// to it" — both read as never started.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn phase_for_missing_pointer(pointer_exists: bool) -> Option<&'static str> {
    (!pointer_exists).then_some(NEVER_STARTED_PHASE)
}

/// Map the supervisor lane's own phase to the wire string
/// `workspace.list` reports — snake_case, matching every other
/// wire-enum-as-string in this protocol (`repl_state`, `agent_state`).
/// Portable: [`sot_log::wire`] has no OS dependency (see that crate's own
/// module doc), so this needs no `#[cfg(windows)]` either, and the pure
/// unit tests below exercise it directly on Linux.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn phase_str(phase: sot_log::wire::SupervisorPhase) -> &'static str {
    use sot_log::wire::SupervisorPhase;
    match phase {
        SupervisorPhase::Starting => "starting",
        SupervisorPhase::Ready => "ready",
        SupervisorPhase::Ending => "ending",
        SupervisorPhase::EndedNoRespawn => "ended_no_respawn",
        SupervisorPhase::Terminal => "terminal",
    }
}

/// ADR 0042 L1a (Codex review finding 6): the daemon's own watchdog
/// restart budget for a capsule supervisor — ADR 0041's own launcher
/// restart sequence ("restart with `--resume` on the launcher's shipped
/// 1/3/7/15/30 s sequence, at most 5 restarts in 60 s, then stop and
/// report"). The daemon has become that launcher for every capsule
/// workspace it creates or resumes, so this is the ADR's own row, not
/// new policy.
#[cfg_attr(not(windows), allow(dead_code))]
pub const RESTART_BACKOFFS: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(3),
    std::time::Duration::from_secs(7),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(30),
];
#[cfg_attr(not(windows), allow(dead_code))]
pub const MAX_RESTARTS_PER_WINDOW: usize = 5;
#[cfg_attr(not(windows), allow(dead_code))]
pub const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// ADR 0042 L1a (Codex review findings 10/11): "a small semaphore over
/// spawns" — the SAME fixed-width bound reused for both the startup
/// resume-scan's concurrent spawns and `workspace.list`'s concurrent
/// lane queries, rather than two independently-invented numbers.
#[cfg_attr(not(windows), allow(dead_code))]
pub const LANE_CONCURRENCY: usize = 4;

/// ADR 0042 L1a, Codex review finding 11: ONE absolute deadline over
/// `workspace.list`'s WHOLE lane-query gather — never a fresh budget per
/// row, which let total call time grow with row count. Generous over a
/// single `query_status` call's own worst case (connect 2s + hello 2s +
/// status 5s ~= 9s) to give `LANE_CONCURRENCY`-wide batches room to
/// drain; a row not yet resolved when this expires simply reports
/// "unreachable" — never blocks the ones that did answer.
#[cfg_attr(not(windows), allow(dead_code))]
pub const LIST_LANE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Environment variables scrubbed from the spawned supervisor's (and
/// hence its capsule leg's) environment before launch — the exact list
/// `comm/adapters/claude/bin/ccb` unsets, for the identical reason: a
/// spawning parent's own Claude Code nesting markers make a fresh
/// `claude` mis-detect itself as nested/forked and exit silently.
/// `CLAUDECODE`/`AI_AGENT`/`CLAUDE_CODE_SESSION_ID` make it think it is
/// running INSIDE another claude; `CLAUDE_CODE_FORK_SUBAGENT`/
/// `CLAUDE_CODE_CHILD_SESSION`/`CLAUDE_CODE_TEAMMATE_MODE`/
/// `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` make it think it is a forked/
/// teammate session. A daemon started from within a claude session (or
/// restarted by one) would otherwise propagate every one of these into
/// every capsule it spawns (ADR 0042 L1a, Codex review finding 9).
#[cfg_attr(not(windows), allow(dead_code))]
pub const NESTING_ENV_VARS_TO_SCRUB: &[&str] = &[
    "CLAUDE_CODE_FORK_SUBAGENT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_TEAMMATE_MODE",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "CLAUDECODE",
    "AI_AGENT",
    "CLAUDE_CODE_SESSION_ID",
];

/// The `SOT_COMM_HOME` value (forward-slash string form — a git-bash/MSYS
/// shell, not this native Windows process, is what reads it; MSYS accepts
/// either slash spelling for a drive-letter-absolute path) to hand a
/// capsule's producer env, so its own comm scripts resolve the EXACT SAME
/// home this builds `SOT_COMM_SELF_FILE` from below — Codex round finding
/// 8: ONE resolver ([`crate::paths::sot_comm_home`], also what the
/// daemon's own registry reads use, `handlers::comm_registry_path`),
/// injected into the child explicitly rather than left to each side's own
/// HOME/USERPROFILE guess landing on two different answers. `None` when
/// the resolver itself found nothing (matches comm-lib.sh: nothing to
/// pin).
#[cfg_attr(not(windows), allow(dead_code))]
fn capsule_comm_home_str() -> Option<String> {
    Some(crate::paths::sot_comm_home()?.to_string_lossy().replace('\\', "/"))
}

/// The `SOT_*` awareness env a capsule supervisor spawn stamps on its
/// producer — capsule-comm-identity fix: a capsule has no tmux pane, so
/// `comm-context.sh`'s pane-keyed self-file slot never applies to it, and
/// without `SOT_COMM_HOME`/`SOT_COMM_SELF_FILE` it fell back to the
/// shared per-host `__nopane` slot, colliding with any other no-pane
/// session on the same host (e.g. the frontend itself — the field bug
/// this fixes). `SOT_WORKSPACE` (and `SOT_WORKSPACE_ROOT`/`SOT_SESSION`/
/// `SOT_MANUAL`) reuse [`crate::pty::awareness_env`] verbatim — ONE
/// builder, not a second copy that could drift — keyed on `slug` (Codex
/// round finding 1: the frontend keys results and the active workspace by
/// SLUG, not the internal `ws-<slug>-<hex>` id; `workspace_id` is used
/// ONLY below, for the state dir and self-file paths, where stability
/// and uniqueness matter more than the display shape). `SOT_COMM_NAME` is
/// set ONLY for an explicitly requested `agent_name` (Codex round finding
/// 2: a synthesized default here would become an explicit pin that
/// OVERWRITES any existing registry row of that name — exactly what
/// PROTOCOL.md's "never reuse a handle" forbids, and a hand-started
/// session in the same repo on the same host derives precisely
/// `<slug>-<host>` on its own). `SOT_COMM_SELF_FILE`
/// (`<comm_home>/self/<host>__<workspace_id>.txt`, comm-lib.sh's EXISTING
/// pin-the-self-file-path seam — already used by its own test suite, and
/// already honoured unchanged by both `comm-context.sh`, the reader, and
/// `comm-join.sh`, the writer) is what actually gives the capsule its own
/// slot: `comm-join.sh`'s #148 auto-disambiguating derivation decides the
/// handle and writes it there; the daemon later reads that same file's
/// first line back to learn it (`handlers::capsule_comm_handle`). Pure
/// (no I/O beyond env reads): exercised by the cross-platform test suite
/// even though [`runtime::spawn_detached_supervisor`], its only caller,
/// is gated to Windows and Linux only.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn capsule_supervisor_env(workspace_id: &str, slug: &str, cwd: &Path, agent_name: &str) -> Vec<(String, String)> {
    let mut env = crate::pty::awareness_env(Some(slug), Some(cwd));
    if !agent_name.is_empty() {
        env.push(("SOT_COMM_NAME".to_string(), agent_name.to_string()));
    }
    if let Some(comm_home) = capsule_comm_home_str() {
        let host = crate::workspaces::state_host();
        let self_file = format!("{}/self/{}__{}.txt", comm_home.trim_end_matches('/'), host, workspace_id);
        env.push(("SOT_COMM_HOME".to_string(), comm_home));
        env.push(("SOT_COMM_SELF_FILE".to_string(), self_file));
    }
    env
}

/// Outcome of [`runtime::end_run`] — the daemon's own portable
/// vocabulary over `sot_log::supervisor_client::EndRunOutcome` (never
/// that raw, platform-specific type crossing into `handlers.rs`). Defined
/// here, outside `runtime`, so `handlers.rs`'s outcome→response
/// mapping stays plain and unit-testable on every platform; `end_run`'s
/// own real lane call is the only step gated to Windows and Linux only.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone)]
pub enum EndRunOutcome {
    /// The run ended and its record verified green.
    RecordVerified,
    /// The marker committed but the O(retained history) verify walk
    /// hadn't finished within the ADR's 90s cutoff — still safe to treat
    /// as "ended" (the marker itself is the irrevocable acceptance).
    RecordClosed,
    /// The lane was ALREADY resting in `EndedNoRespawn` — recovered via
    /// the leg's own end-marker alone, NEVER via `verify_voyage`, so
    /// this must not be reported as `RecordVerified`/`RecordClosed`.
    AlreadyEnded,
    /// The authority had already reached `Terminal` before this call
    /// ever reached it — its own internal flap/retry budget exhausted
    /// (`FLAP_THRESHOLD`, `rust/log/src/supervisor.rs`), most often an
    /// agent argv that can never launch (e.g. `claude` missing from
    /// PATH). There is no leg left to end, and a `Terminal` authority
    /// admits no fresh `EndRun` anyway (`supervisor.rs`'s
    /// `handle_command` gates `EndRun` on `Lifecycle::Ready`) — so this
    /// sends `stop` instead (admitted unconditionally, regardless of
    /// lifecycle: `SupervisorOp::Stop`'s own admission has no lifecycle
    /// gate) and waits for its confirmed exit. Without this arm the row
    /// was UNENDABLE: `workspace.destroy` kept reporting `NotEnded`
    /// forever, because nothing ever told the stuck authority to stop.
    /// Safe to treat as `Removable` — a `Terminal` authority has no live
    /// leg left to orphan.
    Terminal,
    /// The lane answered `phase: Starting` (voyage may still be `None`
    /// — only set once Recovering completes) — NEVER "not running"; a
    /// run may be about to (or already did) start. Retry.
    Starting,
    /// `end_run` reported the operation failed, was refused, or its
    /// outcome is unknown — no confirmed end in any case.
    NotEnded(String),
    /// The lane itself was unreachable (the `query_status` round trip
    /// failed — no listener answered at all, not merely a phase this
    /// call disagreed with) AND a bounded, non-blocking attempt to take
    /// `supervisor.lock` on the same state dir succeeded: nobody holds
    /// this row (the kernel released the fence the instant its last
    /// holder died — `sot_log::fence`). A run with no holder is not
    /// running, so this is safe to treat as `Removable`, same as
    /// `Terminal`. Distinct from `NotEnded`: that variant means the lane
    /// DID answer and refused/failed the request — a live, responsive
    /// holder, never fabricated as ended. Field defect closed
    /// (v0.6.0-rc.12): a supervisor that died out from under a row (a
    /// daemon-pair converge that ended the old build) left the row
    /// permanently `Kept`/unendable, because `query_status` failing was
    /// the ONLY signal this function ever consulted.
    Unheld,
}

/// The daemon's capsule runtime — spawning, watching, querying, and
/// ending a supervisor over `sot_log::supervisor_client`. Platform
/// chosen by exactly THREE forks inside (ADR 0043 decision 22): the
/// capsule executable's name ([`CAPSULE_EXE`]), the detach mechanism
/// ([`spawn_detached`]'s two twins), and the adopted leg's exit-status
/// read ([`wait_and_classify`]'s `WatchedLeg::Adopted` arm) — everything
/// else below is byte-identical on both platforms.
#[cfg(any(windows, target_os = "linux"))]
mod runtime {
    use super::{
        agent_argv, capsule_supervisor_env, mode_flag, phase_str, StartMode, LANE_CONCURRENCY,
        LIST_LANE_DEADLINE, MAX_RESTARTS_PER_WINDOW, NESTING_ENV_VARS_TO_SCRUB, NEVER_STARTED_PHASE,
        RESTART_BACKOFFS, RESTART_WINDOW, UNREACHABLE_PHASE,
    };
    use crate::workspaces::Workspaces;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use std::process::Stdio;
    use tokio::process::{Child, Command};

    /// `DETACHED_PROCESS` (Win32): the child gets no console of its own —
    /// right for a background authority that is never an interactive
    /// console session.
    #[cfg(windows)]
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    /// `CREATE_NEW_PROCESS_GROUP`: the supervisor becomes its own process
    /// group, so a Ctrl+C delivered to the daemon's own console (if any)
    /// never propagates to a process the daemon just detached from itself.
    #[cfg(windows)]
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    /// `CREATE_BREAKAWAY_FROM_JOB`: per MSDN, ignored when the calling
    /// process is not itself in a job — so there is nothing to probe
    /// first for the common case. When the daemon IS in a job whose limit
    /// flags lack `JOB_OBJECT_LIMIT_BREAKAWAY_OK`, `CreateProcess` fails
    /// `ERROR_ACCESS_DENIED` rather than silently dropping the flag —
    /// the signal [`spawn_detached`] retries on, without this flag,
    /// rather than refusing the launch: a contained daemon (CI; a
    /// terminal that is itself inside a job) is a context the daemon
    /// cannot change, only report (ADR 0043 decision 32, revised —
    /// survival is the launcher's to grant, never the daemon's to refuse
    /// over). No Linux analogue: there are no job objects to break away
    /// from.
    #[cfg(windows)]
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    /// Win32 `ERROR_ACCESS_DENIED` — what a denied breakaway attempt
    /// reports on `CreateProcess`; [`spawn_detached`]'s own retry signal.
    #[cfg(windows)]
    const ERROR_ACCESS_DENIED: i32 = 5;

    /// `sot-capsule supervise`'s own clean-exit code (`EXIT_CLEAN`).
    const EXIT_CLEAN: i32 = 0;
    /// `sot-capsule supervise`'s own terminal-failure exit code
    /// (`EXIT_TERMINAL`) — unconditionally terminal to
    /// [`wait_and_classify`], never restarted (rule F, shrink round).
    const EXIT_TERMINAL: i32 = 69;
    /// `sot-capsule supervise`'s own fence-contention exit code
    /// (`sot_log::supervisor::EXIT_CONTENDED` — see that const's own doc
    /// for the full reasoning): the authority fence was already held by
    /// a LIVE supervisor. Distinct from [`EXIT_TERMINAL`] in
    /// [`wait_and_classify`] — NEVER a failure of this workspace's own
    /// run, only proof some other leg (almost always the previous
    /// authority for this SAME state dir, still finishing its own
    /// teardown) currently holds the fence.
    const EXIT_CONTENDED: i32 = 70;

    /// The capsule executable's own file name — the FIRST of the three
    /// forks decision 22 names. Resolved next to the daemon's own
    /// executable ("the `sot-capsule` binary path: next to the daemon's
    /// own executable (`current_exe().parent()`), which is where the
    /// install layout puts it", ADR 0042 L1a) on both platforms.
    #[cfg(windows)]
    const CAPSULE_EXE: &str = "sot-capsule.exe";
    #[cfg(target_os = "linux")]
    const CAPSULE_EXE: &str = "sot-capsule";

    pub fn sot_capsule_exe() -> std::io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().ok_or_else(|| {
            std::io::Error::new(ErrorKind::NotFound, "daemon executable has no parent directory")
        })?;
        Ok(dir.join(CAPSULE_EXE))
    }

    /// ADR 0043 decision 25: the supervisor's stderr is the daemon's OWN
    /// log — a FRESH `O_APPEND` open onto it per spawn (the daemon need
    /// not share its handle; `main.rs`'s `open_private_log_file` already
    /// creates the file in append mode, untouched here), so an inherited
    /// descriptor keeps its original inode across a daemon restart or an
    /// `XDG_STATE_HOME` change — a long-lived supervisor keeps writing to
    /// the log it was born with. When the daemon has no log file at all
    /// (or this open fails for any other reason), the supervisor inherits
    /// the daemon's OWN stderr instead — a daemon run by hand in a
    /// terminal shows the supervisor's lines there. Called fresh from
    /// INSIDE `build` below (never hoisted out), so the descriptor is
    /// opened right alongside the rest of the command's own stdio wiring.
    fn supervisor_stderr() -> Stdio {
        let log_path = crate::paths::state_dir().join("sotd.log");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::inherit())
    }

    /// Spawn `sot-capsule supervise <state_dir> <--start|--resume>
    /// --survival <normal|degraded> --assume-no-rollback-target -- <agent
    /// argv>` DETACHED, so the supervisor authority survives the
    /// daemon's own exit — the daemon must not be its kill domain (ADR
    /// 0042 L1a). `--survival` is decided by [`spawn_detached`]'s own
    /// breakaway attempt, never guessed here.
    /// `--assume-no-rollback-target` is mandatory: `sot_log::supervisor::supervise`
    /// itself refuses (exit 69) without it pre-U4. The nesting env vars
    /// are scrubbed and `SOT_COMM_NAME` exported (Codex review finding
    /// 9) — the same contract `boot_wrapper_command`'s tmux path already
    /// gives every autostart workspace. Builds the SAME `Command` on both
    /// platforms (this function, parameterized on the survival value);
    /// only how it is actually detached — [`spawn_detached`], the second
    /// of decision 22's three forks — differs.
    ///
    /// ADR 0043 decision 23: [`super::qualified_state_root`] runs BEFORE
    /// the build — this is the ONE mechanism every capsule launch shares
    /// (create, attach start-on-attach, boot resume, the watchdog's own
    /// restart), so each of those paths refuses an
    /// unqualified root exactly here rather than needing its own copy of
    /// the check. `state_dir` (a subdirectory of the qualified root) is
    /// deliberately NOT what gets checked — the root itself is, via a
    /// fresh resolution matching `handlers.rs`'s own earlier check for a
    /// `workspace.create` (both resolve the SAME env-derived root, so they
    /// agree by construction, not by sharing a value across the wire).
    fn spawn_detached_supervisor(
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
        agent_name: &str,
        workspace_id: &str,
        slug: &str,
    ) -> std::io::Result<Child> {
        super::qualified_state_root().map_err(|msg| std::io::Error::new(ErrorKind::Unsupported, msg))?;
        let build = |survival: &str| -> Command {
            let mut cmd = Command::new(sot_capsule_exe);
            cmd.arg("supervise")
                .arg(state_dir)
                .arg(mode_flag(mode))
                .arg("--survival")
                .arg(survival)
                .arg("--assume-no-rollback-target")
                .arg("--")
                .args(agent_argv)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(supervisor_stderr());
            for var in NESTING_ENV_VARS_TO_SCRUB {
                cmd.env_remove(var);
            }
            for (k, v) in capsule_supervisor_env(workspace_id, slug, cwd, agent_name) {
                cmd.env(k, v);
            }
            cmd
        };
        spawn_detached(build, state_dir)
    }

    /// Decision 22's second fork: how a built `Command` is actually
    /// detached from the daemon so the supervisor authority survives the
    /// daemon's own exit.
    ///
    /// Windows: attempts `CREATE_BREAKAWAY_FROM_JOB` alongside
    /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` —
    /// `tokio::process::Command` re-exposes `.creation_flags()` natively
    /// (no `std::os::windows::process::CommandExt` import needed, unlike
    /// `std::process::Command`). A denied breakaway
    /// (`ERROR_ACCESS_DENIED` — this daemon's own job forbids it: CI, or
    /// a terminal that is itself inside a job) is not refused: the SAME
    /// spawn is retried without the flag, logged once, and launched
    /// `--survival degraded` — the daemon reports its containment, it
    /// never fabricates it as an error (ADR 0043 decision 32, revised).
    /// Any OTHER spawn error propagates unchanged.
    #[cfg(windows)]
    fn spawn_detached(build: impl Fn(&str) -> Command, state_dir: &Path) -> std::io::Result<Child> {
        let mut cmd = build("normal");
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                tracing::warn!(
                    state_dir = ?state_dir,
                    "capsule supervisor: this daemon's own job forbids breakaway; the supervisor \
                     is contained in it and will not outlive it (ADR 0043 decision 32)"
                );
                let mut cmd = build("degraded");
                cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
                cmd.spawn()
            }
            other => other,
        }
    }

    /// Linux: `pre_exec(setsid)` plus `Stdio::null()` on all three
    /// streams — set by [`spawn_detached_supervisor`]'s own `build`
    /// closure already, nothing further needed here. There are no job
    /// objects on Linux to break away from — this leg's own `setsid` IS
    /// the whole detachment (ADR 0043 decision 22/14). `state_dir` is
    /// unused here (only Windows's breakaway-denied mapping names it in
    /// its log line) — kept as a shared parameter so both platform twins
    /// have the same signature.
    ///
    /// `setsid`'s failure is PROPAGATED (review round, reproduced): in a
    /// FRESH fork child, immediately post-fork, pre-exec, it cannot fail
    /// for the "already a session/process-group leader" reason a plain
    /// re-run of THIS process might (a fork always starts a brand-new
    /// process that has never called `setsid` before) — `EPERM` here
    /// means something else entirely denied it (a seccomp filter, most
    /// plausibly), a real, reportable failure this must not silently
    /// swallow: a detached supervisor spawned WITHOUT a new session would
    /// stay attached to the daemon's own controlling terminal/session,
    /// silently breaking the whole point of detaching it.
    #[cfg(target_os = "linux")]
    fn spawn_detached(build: impl Fn(&str) -> Command, state_dir: &Path) -> std::io::Result<Child> {
        let _ = state_dir;
        let mut cmd = build("normal");
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn()
    }

    /// One capsule workspace's supervisor-lane status, as the daemon's
    /// own wire vocabulary — never the raw `sot_log` types, so
    /// `handlers.rs` has nothing Windows-specific to import. BLOCKING —
    /// callers run it via `spawn_blocking`.
    ///
    /// Rule B (shrink round): a workspace with no published voyage
    /// pointer (`sot_log::pointer::pointer_path`) — no state dir at all,
    /// or one that exists but nothing was ever durably published to —
    /// short-circuits to `NEVER_STARTED_PHASE` ("stopped") BEFORE
    /// attempting a connect that cannot possibly succeed; only a
    /// workspace WITH a published pointer falls through to the real
    /// query, where an ordinary failure stays `UNREACHABLE_PHASE` and a
    /// failure specifically proving the lane foreign (ADR 0030 §8
    /// decision 31c) reports `FOREIGN_PHASE` instead — the lane DID
    /// answer, just not to this daemon's build.
    pub fn phase_of(state_dir: &Path) -> &'static str {
        if let Some(phase) =
            super::phase_for_missing_pointer(sot_log::pointer::pointer_path(state_dir).is_file())
        {
            return phase;
        }
        match sot_log::supervisor_client::query_status(state_dir) {
            // The retained process handle (the second element) is not
            // this caller's concern -- a one-shot phase probe, dropped
            // (closing the handle) the instant this returns.
            Ok((report, _process)) => super::phase_str(report.phase),
            // Typed, not text (ADR 0030 §8 decision 31c): `VersionSkew`
            // is the ONLY `sot_log::Error` variant `query_status` returns
            // for a lane that answered but refused this build. Every
            // other error -- a malformed reply, a timeout, connect
            // refused -- stays `UNREACHABLE_PHASE`.
            Err(sot_log::Error::VersionSkew) => {
                note_version_skew(state_dir);
                super::FOREIGN_PHASE
            }
            Err(e) => {
                tracing::debug!(state_dir = ?state_dir, error = %e, "capsule workspace: supervisor lane unreachable");
                super::UNREACHABLE_PHASE
            }
        }
    }

    /// Log ONCE per row per daemon lifetime that a capsule row's
    /// supervisor refused this daemon's hello (ADR 0030 §8 decision 31c;
    /// the gate itself is superseded by ADR 0045 decision 7) — called
    /// only once the caller has ALREADY typed-matched
    /// `sot_log::Error::VersionSkew`, so this never fires on a merely
    /// unreachable lane. Wording covers BOTH migration-window causes
    /// (Codex review, 2026-09-11): a genuine lane-protocol mismatch, or
    /// an OLD (pre-ADR-0045) supervisor still refusing on build — this
    /// daemon cannot tell which from the wire alone, so it never claims
    /// to. `phase_of` is also the list poll's own probe, so this dedupes
    /// on `state_dir` rather than logging every poll.
    fn note_version_skew(state_dir: &Path) {
        use std::sync::{Mutex, OnceLock};
        static NOTED: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
        let mut noted = NOTED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
        if noted.insert(state_dir.to_path_buf()) {
            tracing::warn!(
                state_dir = ?state_dir,
                "the row's supervisor refused this client (another lane protocol, or a supervisor from before \
                 the protocol-only gate); end the row and recreate it, or kill only its `sot-capsule supervise` \
                 process and attach the row again (the run leg and its agent survive and are adopted)"
            );
        }
    }

    /// `workspace.delete` on a capsule workspace (and the default row's
    /// end-run path): send `end_run {reason, voyage}` on the lane,
    /// reporting the outcome via [`super::EndRunOutcome`] (see its own
    /// variant docs for the Starting/AlreadyEnded/Terminal honesty
    /// rules), THEN `stop` the authority once confirmed there is no more
    /// leg to run — including a `Terminal` authority, which has no leg
    /// to end but still needs `stop` to actually go away (see
    /// [`super::EndRunOutcome::Terminal`]'s own doc: without this arm a
    /// capsule row whose agent argv can never launch cycled
    /// Starting -> Terminal forever and was never endable from the UI).
    /// `stop` now WAITS for confirmed process exit; a failure there is
    /// only a logged warning, never a destroy failure — the outcome
    /// stays the confirmed one (`stop` only ends the AUTHORITY, never
    /// the capsule LEG, ADR 0041 adoption). The state directory is NEVER
    /// deleted here. BLOCKING — callers run it via `spawn_blocking`.
    pub fn end_run(state_dir: &Path, reason: &str) -> std::io::Result<super::EndRunOutcome> {
        use super::EndRunOutcome as R;
        use sot_log::supervisor_client::EndRunOutcome as O;
        use sot_log::wire::SupervisorPhase;

        let status = match sot_log::supervisor_client::query_status(state_dir) {
            Ok((status, _process)) => status,
            Err(e) => {
                // Unreachable is not the same claim as "not running" —
                // the caller must keep refusing a live-but-unresponsive
                // lane, never fabricate "ended" for one (see
                // `destroy_capsule_workspace`'s own doc). But liveness
                // IS knowable independent of this IPC round trip:
                // `supervisor.lock` is a kernel-held OS file lock,
                // released the instant its holder dies (`fence.rs`), so
                // a bounded (~250ms), non-blocking attempt to take it
                // settles the question for certain. Acquirable -> no
                // supervisor holds this row; release immediately (this
                // call only OBSERVES liveness, it must never itself
                // become the holder) and report the row unheld. Still
                // held (or any other lock error) -> unchanged: the
                // original "lane unreachable" refusal.
                return match sot_log::fence::lock_supervisor(state_dir) {
                    Ok(_lock) => Ok(R::Unheld),
                    Err(_) => Err(std::io::Error::other(e.to_string())),
                };
            }
        };

        match status.phase {
            SupervisorPhase::Starting => return Ok(R::Starting),
            SupervisorPhase::EndedNoRespawn => {
                // A PRIOR end already landed here and was never stopped
                // (exactly the leak this whole function closes) — a
                // fresh `EndRun` command would only be refused
                // (`Failed{"no leg is currently running"}`, since
                // `EndRun` requires `Lifecycle::Ready`; see
                // `supervisor.rs`'s `handle_command`). Skip the doomed
                // round trip; retry the stop instead of fabricating a
                // verified outcome this call never actually observed.
                stop_and_warn(state_dir, "already ended (EndedNoRespawn) before this call");
                return Ok(R::AlreadyEnded);
            }
            SupervisorPhase::Terminal => {
                // No leg is running and no fresh `EndRun` would ever be
                // admitted here (`Lifecycle::Terminal` isn't `Ready`) —
                // the honest confirmed end is stopping the stuck
                // authority itself. See `EndRunOutcome::Terminal`'s own
                // doc for why this arm exists.
                stop_and_warn(state_dir, "the authority was terminal before this call reached it");
                return Ok(R::Terminal);
            }
            SupervisorPhase::Ready | SupervisorPhase::Ending => {}
        }

        // Ready/Ending are only reachable once Recovering's own Done arm
        // has set `authority.voyage_id` (`supervisor.rs`), so this is
        // always populated here.
        let voyage = status
            .voyage
            .expect("Ready/Ending implies a voyage_id (supervisor.rs's own recovery transition)");
        let outcome = sot_log::supervisor_client::end_run(state_dir, &voyage, reason)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(match outcome {
            O::RecordVerified => {
                stop_and_warn(state_dir, "end_run confirmed verified");
                R::RecordVerified
            }
            O::RecordClosed => {
                stop_and_warn(state_dir, "end_run confirmed closed");
                R::RecordClosed
            }
            O::Failed(detail) => R::NotEnded(format!("end_run failed: {detail}")),
            O::Refused(detail) => R::NotEnded(format!("end_run refused: {detail}")),
            O::OutcomeUnknown => R::NotEnded(
                "end_run outcome unknown (the ADR's own 90s cutoff elapsed with no terminal reply)"
                    .to_string(),
            ),
        })
    }

    /// Best-effort `stop` after [`end_run`] confirms there is no more
    /// leg to run — see that function's own doc for why this exists and
    /// why a failure here is only ever logged, never propagated.
    fn stop_and_warn(state_dir: &Path, why: &'static str) {
        if let Err(e) = sot_log::supervisor_client::stop(state_dir) {
            tracing::warn!(
                state_dir = ?state_dir, error = %e, why,
                "capsule workspace: stop after end_run failed (resident supervisor leaked)"
            );
        }
    }

    /// Spawn a capsule's supervisor authority AND hand it to a watchdog
    /// task together (ADR 0042 L1a, Codex review finding 6: "hand every
    /// spawned Child to a waiter task") — the daemon has become ADR
    /// 0041's own launcher for every capsule workspace it creates or
    /// resumes. Returns synchronously once the FIRST spawn attempt is
    /// known to have succeeded or failed, so a caller (`workspace.create`,
    /// finding 1) can roll back on a synchronous failure; the watchdog
    /// itself then runs entirely in the background. Clears any prior
    /// `capsule_terminal` mark for this workspace — a fresh spawn is a
    /// fresh chance.
    ///
    /// Rule D: releases `start_supervisor`'s `starting` claim
    /// (`Workspaces::end_capsule_start`) via TWO independent,
    /// idempotently-converging mechanisms — "removes when the lane first
    /// answers or the child exits": [`spawn_starting_release_poll`]
    /// releases it the moment the lane first answers (bounded, so a leg
    /// that never binds its lane at all doesn't wedge it open forever),
    /// and [`spawn_watchdog`]'s own loop releases it on every leg-exit
    /// (first leg AND every restart) as a second, unconditional path.
    /// `HashSet::remove` is idempotent, so whichever fires first actually
    /// clears the entry; the other is a harmless no-op.
    pub fn spawn_and_watch(
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
        agent_name: &str,
        workspace_id: String,
        slug: String,
        workspaces: Workspaces,
    ) -> std::io::Result<()> {
        let child =
            spawn_detached_supervisor(sot_capsule_exe, state_dir, mode, agent_argv, cwd, agent_name, &workspace_id, &slug)?;
        workspaces.clear_capsule_terminal(&workspace_id);
        spawn_starting_release_poll(workspace_id.clone(), state_dir.to_path_buf(), workspaces.clone());
        install_watchdog(
            workspace_id,
            sot_capsule_exe.to_path_buf(),
            state_dir.to_path_buf(),
            agent_argv.to_vec(),
            cwd.to_path_buf(),
            agent_name.to_string(),
            slug,
            WatchedLeg::Spawned(child),
            workspaces,
        );
        Ok(())
    }

    /// Rule D half 1: release this workspace's `starting` claim the
    /// moment its lane first ANSWERS a status query — a typed
    /// `VersionSkew` counts (Codex review, 2026-09-11: the ADR 0045
    /// migration case is exactly a live child that answers foreign
    /// forever, never crashing into [`spawn_watchdog`]'s own child-exit
    /// release) — or the settle bound itself expires, bounded by
    /// [`LIST_LANE_DEADLINE`] so a leg that never binds a lane at all
    /// (crashes before `PipeServer::bind_supervisor`) doesn't wedge the
    /// flag open forever either.
    fn spawn_starting_release_poll(workspace_id: String, state_dir: PathBuf, workspaces: Workspaces) {
        tokio::spawn(async move {
            let deadline = Instant::now() + LIST_LANE_DEADLINE;
            loop {
                let dir = state_dir.clone();
                let outcome = tokio::task::spawn_blocking(move || sot_log::supervisor_client::query_status(&dir)).await.ok();
                // Invariant: the claim must clear on any DEFINITIVE answer, not only `Ok` -- a foreign-proto refusal is still an answer.
                if matches!(outcome, Some(Ok(_)) | Some(Err(sot_log::Error::VersionSkew))) {
                    workspaces.end_capsule_start(&workspace_id);
                    return;
                }
                if Instant::now() >= deadline {
                    // Invariant: the settle bound itself must also clear the claim -- a live, merely-unreachable child must not wedge it open forever.
                    workspaces.end_capsule_start(&workspace_id);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// The spawn path shared by `workspace.create` (mode `Start` always — a
    /// brand new workspace has no state dir yet), `pty.open`'s
    /// start-on-attach ([`ensure_started`], mode picked by
    /// [`start_mode_needed`]), and `resume_all`: locate `sot-capsule.exe`
    /// and spawn-and-watch it. ADR 0042 L1a Codex review finding 1's
    /// synchronous-failure contract applies to every caller: an `Err`
    /// here means no supervisor is running, and the caller must refuse
    /// its own op with this text rather than silently proceeding.
    ///
    /// Rule C (shrink round): does NOT create the state directory —
    /// `sot-capsule supervise` creates its own (`supervise_inner`'s first
    /// act, `rust/log/src/supervisor.rs`) once it actually runs, so a
    /// synchronous spawn failure here leaves nothing behind at all, not
    /// even an empty directory a later `phase_of` could misread.
    ///
    /// Rule D (shrink round): AT MOST ONE launch per workspace at a
    /// time — `Workspaces::try_begin_capsule_start` claims the slot
    /// ATOMICALLY (insert-if-absent under the SAME `Workspaces` mutex, no
    /// new lock type) as this function's very first act; a caller that
    /// loses the race gets `Ok(None)` — spawns nothing, not an error —
    /// so `pty.open`'s caller can still answer `attach_direct` and let
    /// the frontend's own attach-retry find the lane once it comes up.
    /// The slot is released by [`spawn_and_watch`]'s own two independent,
    /// idempotently-converging mechanisms (see that function's doc) —
    /// released HERE only on a failure that never got that far.
    pub fn start_supervisor(
        state_root: &Path,
        workspace_id: &str,
        mode: StartMode,
        agent_argv: &[String],
        project_root: &Path,
        agent_name: &str,
        slug: &str,
        workspaces: Workspaces,
    ) -> Result<Option<()>, String> {
        if !workspaces.try_begin_capsule_start(workspace_id) {
            return Ok(None);
        }
        start_supervisor_claimed(state_root, workspace_id, mode, agent_argv, project_root, agent_name, slug, workspaces).map(Some)
    }

    /// The BLOCKING half of [`start_supervisor`] (process spawn
    /// underneath [`spawn_and_watch`]), split out for ADR 0043
    /// decision 29: a caller that itself runs on a Tokio worker (the
    /// `workspace.create` handler, `resume_all`) must claim the `starting`
    /// slot SYNCHRONOUSLY — via `workspaces.try_begin_capsule_start`,
    /// BEFORE its own `spawn_blocking` scheduling yield — and only then
    /// hand the actual spawn to this function under `spawn_blocking`.
    /// Taking the claim first, rather than letting the blocking closure
    /// take it once the pool gets around to running it, closes a real
    /// window: `pty.open`'s `ensure_started` can reach the SAME
    /// freshly-created workspace_id within milliseconds of
    /// `workspace.create`'s own reply (the field latency map's own
    /// ordering), and would otherwise be free to win the claim itself
    /// while this caller's spawn is merely QUEUED — which would make
    /// THIS caller observe `Ok(None)` and roll back a workspace that the
    /// other caller is legitimately starting. Every failure path here
    /// still releases the claim, exactly as [`start_supervisor`] always
    /// did — this function assumes ONLY that the claim is already held,
    /// never that it still needs taking.
    ///
    /// Returns `Ok(())` on success (Codex review round finding 9's
    /// delete list): every caller of THIS function already holds the
    /// claim by the time it runs (either taken synchronously just above
    /// by [`start_supervisor`] itself, or by the caller directly — see
    /// `handlers.rs`'s `workspace.create` and [`resume_all`]'s own
    /// `Some(StartMode::Resume)` arm), so the `None` case
    /// [`start_supervisor`]'s own `Result<Option<()>, String>` exists
    /// for can never actually occur here — carrying it anyway would need
    /// dead, impossible-to-exercise handling at every call site.
    pub fn start_supervisor_claimed(
        state_root: &Path,
        workspace_id: &str,
        mode: StartMode,
        agent_argv: &[String],
        project_root: &Path,
        agent_name: &str,
        slug: &str,
        workspaces: Workspaces,
    ) -> Result<(), String> {
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let exe = match sot_capsule_exe() {
            Ok(exe) => exe,
            Err(e) => {
                workspaces.end_capsule_start(workspace_id);
                return Err(format!("could not locate sot-capsule.exe next to this daemon: {e}"));
            }
        };
        match spawn_and_watch(
            &exe,
            &state_dir,
            mode,
            agent_argv,
            project_root,
            agent_name,
            workspace_id.to_string(),
            slug.to_string(),
            workspaces.clone(),
        ) {
            Ok(()) => Ok(()),
            Err(e) => {
                workspaces.end_capsule_start(workspace_id);
                Err(format!("capsule supervisor spawn failed: {e}"))
            }
        }
    }

    /// Whether a capsule workspace's supervisor needs starting before
    /// `pty.open` can honestly answer `attach_direct` — and if so, with
    /// which start mode. Reuses [`phase_of`]'s own two failure phases
    /// rather than a new probe: [`NEVER_STARTED_PHASE`] (no published
    /// pointer — nothing has ever run) means `Start`; [`UNREACHABLE_PHASE`]
    /// (a pointer exists but its lane does not answer — the same "dead
    /// supervisor" case the watchdog/`resume_all` already resume with
    /// `--resume`) means `Resume`. Any answered lifecycle phase means a
    /// supervisor is already up — `None`, nothing to do. BLOCKING
    /// (`phase_of` itself is).
    ///
    /// `resume_all` (daemon boot) now matches on this SAME function
    /// (round-2 Codex finding: one decision oracle, not a second one) —
    /// `None` means adopt (a lane already answers), `Some(Resume)` means
    /// spawn (pointer published, lane dead), `Some(Start)` means the boot
    /// scan's own defensive no-op (it should never see this — its own
    /// candidate filter already requires a published pointer).
    fn start_mode_needed(state_dir: &Path) -> Option<StartMode> {
        start_mode_for_phase(phase_of(state_dir))
    }

    /// The pure half of [`start_mode_needed`], split out so
    /// [`ensure_started`] can decide on a phase it already probed once
    /// (switch-latency Phase 1: the healthy already-running path used to
    /// call [`phase_of`] — a fresh `query_status` connect+challenge round
    /// trip — TWICE per attach; this mapping itself does no I/O, so
    /// reusing an already-fetched phase string here costs nothing beyond
    /// what an already-answered lane already told the caller).
    fn start_mode_for_phase(phase: &str) -> Option<StartMode> {
        match phase {
            NEVER_STARTED_PHASE => Some(StartMode::Start),
            UNREACHABLE_PHASE => Some(StartMode::Resume),
            _ => None,
        }
    }

    /// `pty.open` on a capsule workspace: start its supervisor if it
    /// isn't already running, sharing [`start_supervisor`] — the exact
    /// spawn path `workspace.create` uses — rather than a second spawn
    /// implementation (field finding, v0.6.0-rc.2 shakedown:
    /// `workspace.create` was the ONLY path that ever started one, so a
    /// workspace registered but never created through it — the Windows
    /// default/home row, ADR 0042 L1a Codex finding 5 — answered
    /// `attach_direct` against a supervisor that was never spawned,
    /// parking the frontend on an empty pane forever). `Ok(None)` = a
    /// supervisor already answered, OR another launch is already in
    /// flight (rule D); nothing started either way. `Ok(Some(()))` = a
    /// fresh spawn succeeded (mode from [`start_mode_needed`]). `Err`
    /// mirrors `start_supervisor`'s own failure, so a caller's error
    /// payload can match `workspace.create`'s.
    ///
    /// Rule I: this DOES block on a real lane probe (`start_mode_needed`
    /// -> `phase_of` -> `query_status`) when a pointer exists and no
    /// start is in flight — the same bounded worst case `query_status`'s
    /// own doc names (connect 2s + hello 2s + status 5s ~= 9s) for a row
    /// whose supervisor died without a trace. That cost is accepted here
    /// rather than avoided: it is the only way to tell "already running"
    /// (skip) from "pointer exists but the lane is dead" (needs
    /// `--resume`) apart from just guessing one or the other. The
    /// `is_capsule_starting` check below is what SKIPS that probe when a
    /// launch is already in flight — an early exit, not the correctness
    /// guarantee (that's `start_supervisor`'s own atomic claim, closing
    /// the tiny window between this check and the call below). BLOCKING
    /// — callers run it via `spawn_blocking`.
    pub fn ensure_started(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        workspaces: Workspaces,
    ) -> Result<Option<()>, String> {
        if workspaces.is_capsule_starting(workspace_id) {
            return Ok(None);
        }
        let state_dir = super::state_dir_for(state_root, workspace_id);
        // ONE probe on the healthy already-running path (switch-latency
        // Phase 1): this used to probe the lane HERE via
        // `start_mode_needed` (which calls `phase_of`), then probe it
        // AGAIN below for the `EndedNoRespawn` check — two full
        // `query_status` round trips (connect + identity challenge +
        // status exchange, each) for a workspace that never needed a
        // spawn decision more than once. `initial_phase` is that one
        // probe; `mode_needed` is the pure decision on it, reused below
        // instead of probing a second time.
        let initial_phase = phase_of(&state_dir);
        let mode_needed = start_mode_for_phase(initial_phase);
        let spawned = match mode_needed {
            Some(mode) => {
                let argv = agent_argv(agent_kind)?;
                let result = start_supervisor(
                    state_root,
                    workspace_id,
                    mode,
                    &argv,
                    project_root,
                    agent_name,
                    slug,
                    workspaces,
                )?;
                if mode == StartMode::Resume {
                    // A resumed authority whose leg already carries the
                    // end marker settles into `EndedNoRespawn` almost
                    // instantly (a marker read, no spawn) — give it
                    // that window before the reset check below. A
                    // crash-resume (spawning a fresh leg) just stays
                    // `Starting` past it and falls through unaffected.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        if let Ok((report, _process)) =
                            sot_log::supervisor_client::query_status(&state_dir)
                        {
                            if !matches!(report.phase, sot_log::wire::SupervisorPhase::Starting) {
                                break;
                            }
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
                result
            }
            None => None,
        };
        // One answered phase still has no live leg to attach to:
        // `EndedNoRespawn` (`--resume`/`--start` deliberately never
        // resurrect it — ADR 0041's own no-resurrection rule). `reset`
        // is the ONE operation that phase admits; proceed as for a
        // fresh start (the reset transaction mints the new voyage and
        // spawns).
        //
        // `mode_needed == None` means nothing above touched the lane at
        // all (the healthy already-running path) — `initial_phase` is
        // still current, so it answers this check directly. Any other
        // branch DID just change the lane's state (a fresh spawn, or the
        // resume settle wait), so it re-probes for the truth rather than
        // trusting a phase read before that happened.
        let settled_phase = match mode_needed {
            None => initial_phase,
            Some(_) => phase_of(&state_dir),
        };
        if settled_phase == super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn) {
            return sot_log::supervisor_client::reset(&state_dir)
                .map(|_new_voyage| Some(()))
                .map_err(|e| format!("capsule workspace reset (after an ended run) failed: {e}"));
        }
        Ok(spawned)
    }

    /// What one leg's exit means for the watchdog's own decision —
    /// ADR 0042 L1a, Codex review finding 6; rule F (shrink round)
    /// simplified this from three outcomes to two.
    enum LegOutcome {
        /// Exit 0 (`EXIT_CLEAN`): the run ended normally. Never
        /// restarted — the lane (or its absence) already says
        /// everything a client needs.
        Clean,
        /// Exit 69 (`EXIT_TERMINAL`): terminal, UNCONDITIONALLY — never
        /// restarted, regardless of whether the lane still answers. Rule
        /// F: the OLD "does the lane still answer" discriminator (a
        /// dropped `ForeignFence` outcome) tried to tell apart "lost the
        /// race for `supervisor.lock`" from "a genuinely exhausted
        /// producer", but `sot-capsule supervise` already runs its OWN
        /// internal flap/retry budget (`FLAP_THRESHOLD`,
        /// `respawn_or_terminal` in `rust/log/src/supervisor.rs`) before
        /// it ever chooses to exit 69 — so a second restart layer on top,
        /// here, is always redundant at best. At worst it actively hid a
        /// real failure: a producer that will NEVER recover (e.g.
        /// `claude` missing from the daemon's PATH) burned the WHOLE
        /// daemon-side restart budget (`MAX_RESTARTS_PER_WINDOW` attempts
        /// against `RESTART_WINDOW`) before finally reaching this same
        /// terminal mark anyway — "the supervisor's own three legs, not a
        /// rolling restart loop."
        Terminal,
        /// Exit 70 (`EXIT_CONTENDED`) — round-2 Codex finding, daemon-
        /// boot-adopts-supervisor fix: the authority fence was already
        /// held by a LIVE supervisor when this leg tried to acquire it.
        /// NEVER treated as [`Terminal`] (that would mark a perfectly
        /// healthy workspace terminal out from under a run some OTHER
        /// leg is still actively serving) and never blindly retried with
        /// `--resume` either (which would likely race the SAME
        /// still-tearing-down fence again) — [`install_watchdog`] instead
        /// re-probes the lane for adoption over a short bound.
        Contended,
        /// Anything else: a genuine crash needing the restart sequence.
        Crash,
    }

    /// One capsule leg's process handle, unified so the SAME watchdog
    /// loop ([`install_watchdog`]) can wait on and classify EITHER a leg
    /// this daemon just spawned itself (`Spawned`, an owned [`Child`]) OR
    /// one it ADOPTED already alive from a previous daemon lifetime
    /// (`Adopted`, the retained process handle the supervisor lane's own
    /// challenge proved — `sot_log::supervisor_client::query_status`'s
    /// second return value). Round-2 Codex finding: an adopted leg had NO
    /// watchdog at all before this — its own eventual crash was never
    /// restarted, a 69 was never recorded, and a Terminal lane simply
    /// went quiet ("unreachable") once its process actually exited.
    enum WatchedLeg {
        Spawned(Child),
        Adopted(sot_log::supervisor_client::ChallengedProcess),
    }

    /// How long an adopted leg's blocking wait blocks before looping to
    /// check again — purely a "how often does a blocking-pool thread
    /// wake up for no reason" knob, never a correctness bound:
    /// [`sot_log::supervisor_client::ChallengedProcess::wait`] returns
    /// the INSTANT the process actually dies, so a longer interval only
    /// means fewer wasted wakeups, never slower death detection.
    const ADOPTED_LEG_WAIT_POLL: Duration = Duration::from_secs(300);

    /// Maps a confirmed (or absent) exit code to the watchdog's own
    /// outcome vocabulary — shared by both [`WatchedLeg`] arms in
    /// [`wait_and_classify`].
    fn classify_exit_code(code: Option<i32>) -> LegOutcome {
        match code {
            Some(EXIT_CLEAN) => LegOutcome::Clean,
            Some(EXIT_TERMINAL) => LegOutcome::Terminal,
            Some(EXIT_CONTENDED) => LegOutcome::Contended,
            _ => LegOutcome::Crash,
        }
    }

    /// Waits for `leg` to end and classifies the result — the SAME
    /// classification whether the leg is a child this daemon just
    /// spawned or one it adopted. A spawned child is awaited async, the
    /// normal way — `tokio::process::Child::wait` is trusted outright:
    /// the daemon is the sole, unambiguous OWNER of a supervisor it
    /// spawned itself, and single-owner reaping (review round: `Child`
    /// reaps a spawned supervisor; `ChallengedProcess` never reaps
    /// implicitly at all, only its owner ever calls `PeerProcess::reap`
    /// explicitly — see that method's own doc in `sot_log::client`) is
    /// exactly what makes that trust safe now. An adopted process has no
    /// async-awaitable primitive (`ChallengedProcess::wait` is a
    /// synchronous, bounded wait), so it is waited on ONE blocking task
    /// that owns it for the wait's whole duration, looping
    /// [`ADOPTED_LEG_WAIT_POLL`] at a time until it reports exit, then
    /// its exit code is read the same way each platform's own
    /// `ChallengedProcess` documents its own precondition: only after
    /// `wait` has already confirmed death. The THIRD of decision 22's
    /// three forks lives here: Windows' `exit_code_after_confirmed_exit`
    /// always yields a `u32` code; Linux's
    /// `exit_status_after_confirmed_exit` yields `Option<i32>` — `None`
    /// (decision 8's "exited, status unknown" tolerance, e.g. a
    /// pre-6.15 kernel without `PIDFD_GET_INFO`) is logged at warn with
    /// the pid and folds into [`LegOutcome::Crash`] below, never a panic.
    /// This daemon-side handle only ever PROVES the adopted peer's
    /// identity — it never calls `reap()`, since a daemon holding it is
    /// never that pid's owner of record (ADR 0043 decision 21, review
    /// round: the supervisor that spawned it, or its own OS parent once
    /// that supervisor is gone, is).
    async fn wait_and_classify(leg: WatchedLeg, workspace_id: &str) -> LegOutcome {
        let code = match leg {
            WatchedLeg::Spawned(mut child) => match child.wait().await {
                Ok(status) => status.code(),
                Err(e) => {
                    tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: wait() failed; treating as a crash");
                    return LegOutcome::Crash;
                }
            },
            WatchedLeg::Adopted(process) => {
                let pid = process.pid();
                let result = tokio::task::spawn_blocking(move || loop {
                    match process.wait(ADOPTED_LEG_WAIT_POLL) {
                        Ok(true) => {
                            #[cfg(windows)]
                            {
                                return process
                                    .exit_code_after_confirmed_exit()
                                    .map(|c| Some(c as i32))
                                    .map_err(|e| e.to_string());
                            }
                            #[cfg(target_os = "linux")]
                            {
                                return process
                                    .exit_status_after_confirmed_exit()
                                    .map_err(|e| e.to_string());
                            }
                        }
                        Ok(false) => continue,
                        Err(e) => return Err(e.to_string()),
                    }
                })
                .await;
                match result {
                    Ok(Ok(Some(c))) => Some(c),
                    Ok(Ok(None)) => {
                        // Linux only (Windows's arm above always yields
                        // `Some`) -- decision 8's own tolerance for
                        // "exited, status unknown." Never a panic: the
                        // leg is treated as a crash (the watchdog's
                        // restart sequence applies), with the pid named
                        // so an operator can correlate it with the
                        // system's own logs if they want to know more.
                        tracing::warn!(
                            workspace_id = %workspace_id, pid,
                            "capsule supervisor watchdog: adopted leg exited with unknown status (decision 8 tolerance); treating as a crash"
                        );
                        None
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            workspace_id = %workspace_id, error = %e,
                            "capsule supervisor watchdog: waiting on the adopted process failed; treating as a crash"
                        );
                        None
                    }
                    Err(e) => {
                        tracing::warn!(
                            workspace_id = %workspace_id, error = %e,
                            "capsule supervisor watchdog: adopted-process wait task did not complete; treating as a crash"
                        );
                        None
                    }
                }
            }
        };
        classify_exit_code(code)
    }

    /// Bound on how long [`install_watchdog`] keeps re-probing a
    /// [`LegOutcome::Contended`] leg for adoption before giving up —
    /// matches [`sot_log::transport::TEARDOWN_AGGREGATE_DEADLINE`] (the
    /// authority's own documented worst-case teardown budget: it drops
    /// its lane before releasing the fence) plus margin for this
    /// process's own connect/challenge round trip on top of that.
    const CONTENTION_RETRY_BOUND: Duration = Duration::from_secs(25);
    /// How often [`install_watchdog`] re-probes within
    /// [`CONTENTION_RETRY_BOUND`] — matches the cadence
    /// `spawn_starting_release_poll` and `workspace.list`'s own lane
    /// gather already use elsewhere in this file for "is the lane up
    /// yet" polling, just slower (contention resolving is rarer and the
    /// bound is longer).
    const CONTENTION_RETRY_INTERVAL: Duration = Duration::from_secs(2);

    /// The watchdog itself: waits for the leg to exit, classifies it, and
    /// on a crash restarts with `--resume` under ADR 0041's own launcher
    /// restart sequence (`RESTART_BACKOFFS`, at most `MAX_RESTARTS_PER_
    /// WINDOW` within `RESTART_WINDOW`), then stops and marks the
    /// workspace `capsule_terminal` — LOUDLY, via `Workspaces::
    /// mark_capsule_terminal`, which `workspace.list` reads before ever
    /// touching the (confirmed-gone) lane again. A `Terminal` leg (rule
    /// F) marks terminal immediately, on its very first occurrence, with
    /// no restart attempt at all.
    ///
    /// ONE function for both origins (round-2 Codex finding): [`leg`]
    /// starts as either a [`WatchedLeg::Spawned`] child ([`spawn_and_watch`]'s
    /// own call) or a [`WatchedLeg::Adopted`] process
    /// ([`watch_adopted_leg`]'s), and every SUBSEQUENT leg — a crash
    /// restart, or an adoption that resolves a [`LegOutcome::Contended`]
    /// — can be either kind too, decided fresh each time by what actually
    /// happens.
    ///
    /// A [`LegOutcome::Contended`] leg is neither terminal nor a normal
    /// crash: it means some OTHER process currently holds the fence
    /// (almost always the previous authority for this SAME state dir,
    /// still finishing its own teardown), so this re-probes the lane for
    /// up to [`CONTENTION_RETRY_BOUND`], at [`CONTENTION_RETRY_INTERVAL`]
    /// — the instant that OTHER lane answers, it is ADOPTED (a fresh
    /// [`WatchedLeg::Adopted`], watched exactly like any other leg); if
    /// the bound elapses with nothing answering, this simply RETURNS —
    /// no terminal mark, no restart — leaving the row to read whatever
    /// [`phase_of`] naturally reports (`unreachable`, if truly nothing is
    /// left) and the next `pty.open` attach's [`ensure_started`] to
    /// re-probe and start fresh.
    ///
    /// Rule D half 2: releases the `starting` claim
    /// (`Workspaces::end_capsule_start`) on EVERY leg's outcome —
    /// idempotent (harmless once [`spawn_starting_release_poll`], half 1,
    /// already cleared it for the success case, and ALWAYS harmless for
    /// a leg that started life adopted, which never held the claim in
    /// the first place) and correct regardless: once a leg's fate is
    /// known, "a launch is in flight" is no longer true for THIS leg (a
    /// restart, if one follows, is this SAME watchdog's own retry, not a
    /// new external request).
    fn install_watchdog(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        slug: String,
        leg: WatchedLeg,
        workspaces: Workspaces,
    ) {
        tokio::spawn(async move {
            let mut leg_opt = Some(leg);
            let mut restart_times: Vec<Instant> = Vec::new();
            loop {
                let outcome = match leg_opt.take() {
                    Some(l) => wait_and_classify(l, &workspace_id).await,
                    // A previous restart/adoption attempt itself found
                    // nothing to wait on -- counts as another crash
                    // against the same budget.
                    None => LegOutcome::Crash,
                };
                workspaces.end_capsule_start(&workspace_id);
                match outcome {
                    LegOutcome::Clean => return,
                    LegOutcome::Terminal => {
                        tracing::warn!(
                            workspace_id = %workspace_id,
                            "capsule supervisor watchdog: leg exited terminal (69) -- marking terminal, no restart"
                        );
                        workspaces.mark_capsule_terminal(&workspace_id);
                        return;
                    }
                    LegOutcome::Contended => {
                        tracing::warn!(
                            workspace_id = %workspace_id, bound = ?CONTENTION_RETRY_BOUND,
                            "capsule supervisor watchdog: leg exited contended (70) -- re-probing for adoption, no terminal mark"
                        );
                        let deadline = Instant::now() + CONTENTION_RETRY_BOUND;
                        let mut adopted = None;
                        while Instant::now() < deadline {
                            tokio::time::sleep(CONTENTION_RETRY_INTERVAL).await;
                            let dir = state_dir.clone();
                            let probe = tokio::task::spawn_blocking(move || {
                                sot_log::supervisor_client::query_status(&dir)
                            })
                            .await;
                            match probe {
                                Ok(Ok((status, process))) => {
                                    tracing::info!(
                                        workspace_id = %workspace_id, phase = phase_str(status.phase),
                                        "capsule supervisor watchdog: contention resolved -- adopted the surviving lane"
                                    );
                                    adopted = Some(process);
                                    break;
                                }
                                // A foreign holder is first met HERE when the row was
                                // never probed before this leg (a fresh spawn that
                                // exited contended) -- note it the same once-per-row way.
                                Ok(Err(sot_log::Error::VersionSkew)) => {
                                    note_version_skew(&state_dir);
                                }
                                Ok(Err(_)) => {}
                                Err(_) => {}
                            }
                        }
                        match adopted {
                            Some(process) => {
                                leg_opt = Some(WatchedLeg::Adopted(process));
                                continue;
                            }
                            None => {
                                tracing::warn!(
                                    workspace_id = %workspace_id, bound = ?CONTENTION_RETRY_BOUND,
                                    "capsule supervisor watchdog: still contended after the bound -- \
                                     leaving the row for the next attach, no terminal mark"
                                );
                                return;
                            }
                        }
                    }
                    LegOutcome::Crash => {
                        let now = Instant::now();
                        restart_times.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                        if restart_times.len() >= MAX_RESTARTS_PER_WINDOW {
                            tracing::error!(
                                workspace_id = %workspace_id, window = ?RESTART_WINDOW, max = MAX_RESTARTS_PER_WINDOW,
                                "capsule supervisor watchdog: restart budget exhausted -- giving up, marking terminal"
                            );
                            workspaces.mark_capsule_terminal(&workspace_id);
                            return;
                        }
                        let backoff = RESTART_BACKOFFS[restart_times.len().min(RESTART_BACKOFFS.len() - 1)];
                        tracing::warn!(
                            workspace_id = %workspace_id, backoff = ?backoff, attempt = restart_times.len() + 1,
                            "capsule supervisor watchdog: crashed, restarting with --resume"
                        );
                        tokio::time::sleep(backoff).await;
                        restart_times.push(Instant::now());
                        // ADR 0043 decision 29: a process spawn never runs
                        // on a Tokio worker -- this task already awaits
                        // `spawn_blocking` for its own contention probe
                        // above; the restart spawn shares the same
                        // pattern. Clones are the closure's OWN copies
                        // (`'static` + `Send`, required across the
                        // `.await` below) -- the loop's own locals are
                        // untouched and reused on the NEXT iteration.
                        let exe = sot_capsule_exe.clone();
                        let dir = state_dir.clone();
                        let argv_for_spawn = argv.clone();
                        let cwd_for_spawn = cwd.clone();
                        let agent_name_for_spawn = agent_name.clone();
                        let workspace_id_for_spawn = workspace_id.clone();
                        let slug_for_spawn = slug.clone();
                        let spawn_result = tokio::task::spawn_blocking(move || {
                            spawn_detached_supervisor(
                                &exe,
                                &dir,
                                StartMode::Resume,
                                &argv_for_spawn,
                                &cwd_for_spawn,
                                &agent_name_for_spawn,
                                &workspace_id_for_spawn,
                                &slug_for_spawn,
                            )
                        })
                        .await;
                        match spawn_result {
                            Ok(Ok(child)) => leg_opt = Some(WatchedLeg::Spawned(child)),
                            Ok(Err(e)) if e.kind() == ErrorKind::Unsupported => {
                                // `qualified_state_root` refused (ADR 0043
                                // decision 23: the state root went unqualified
                                // out from under a live row -- an `XDG_STATE_HOME`
                                // change, a remounted volume). No retry can change
                                // that without operator action -- mark terminal
                                // now (the error names the recovery).
                                tracing::error!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: unqualified state root -- marking terminal, no restart");
                                workspaces.mark_capsule_terminal(&workspace_id);
                                return;
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: restart spawn failed");
                            }
                            Err(join_err) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %join_err, "capsule supervisor watchdog: restart spawn task panicked");
                            }
                        }
                    }
                }
            }
        });
    }

    /// [`resume_all`]'s own watchdog entry point for a workspace it
    /// ADOPTED (found a lane already answering, spawned nothing) — the
    /// `WatchedLeg::Adopted` counterpart of [`spawn_and_watch`]'s
    /// `WatchedLeg::Spawned` one, sharing [`install_watchdog`] rather
    /// than a second implementation. Deliberately skips
    /// `clear_capsule_terminal`/`spawn_starting_release_poll`: those are
    /// bookkeeping for a SPAWN ATTEMPT this daemon itself just made
    /// (`try_begin_capsule_start`'s claim, a fresh chance after a stale
    /// terminal mark) — adoption never takes that claim and, at boot,
    /// this daemon's OWN `capsule_terminal` set is fresh regardless, so
    /// there is nothing here for either to do.
    fn watch_adopted_leg(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        slug: String,
        process: sot_log::supervisor_client::ChallengedProcess,
        workspaces: Workspaces,
    ) {
        install_watchdog(
            workspace_id,
            sot_capsule_exe,
            state_dir,
            argv,
            cwd,
            agent_name,
            slug,
            WatchedLeg::Adopted(process),
            workspaces,
        );
    }

    /// On daemon startup: resume every REGISTERED capsule workspace's
    /// supervisor whose voyage pointer has ALREADY been published (rule
    /// B, shrink round). A row with NO published pointer — never started
    /// at all, OR a leg that crashed before ever publishing one (the
    /// exact pre-pointer crash window ADR 0041 names) — is SKIPPED
    /// entirely: `pty.open`'s start-on-attach (`ensure_started`) is what
    /// starts those now, not this scan. Before rule B this scan launched
    /// `--resume` unconditionally for EVERY registered row, including
    /// ones with no pointer at all — `sot-capsule supervise --resume`
    /// against a workspace with no leg to adopt and no pointer to found
    /// one against simply fails (exit 69), leaving a bare state directory
    /// behind and reading back as a misleading row on the owner's own box
    /// (the field finding behind this shrink round).
    ///
    /// PROBE before spawning (the daemon-boot-adopts-supervisor fix):
    /// each candidate is decided by [`start_mode_needed`] — the SAME
    /// function `pty.open`'s start-on-attach uses, over a real
    /// [`phase_of`] query of its lane, not the pointer's mere existence
    /// (round-2 Codex finding: one decision oracle, not a second
    /// `LaneDecision` type). `None` means a supervisor from a PREVIOUS
    /// daemon lifetime is still up and answering (the daemon restarted —
    /// e.g. an FE relaunch rebooting the local daemon — while its capsule
    /// supervisors, spawned detached by ADR 0042 design, outlived it):
    /// this scan ADOPTS it — logs one line, fetches the lane's own
    /// retained process handle via a second `query_status` call, and
    /// hands it to [`watch_adopted_leg`] — rather than racing a second
    /// `--resume` leg against the live one's `supervisor.lock`. That
    /// race is exactly what this fix closes: before it, the new leg
    /// always lost the fence, exited 69 (`EXIT_TERMINAL`), and the
    /// watchdog marked the row `capsule_terminal` — wrongly, since the
    /// old supervisor was still actually running the voyage the whole
    /// time (a genuinely still-tearing-down old lane instead gets the
    /// new `EXIT_CONTENDED` — see that const's own doc — never
    /// `EXIT_TERMINAL`, so a fresh spawn's own watchdog re-probes for
    /// adoption instead of marking terminal either). An adopted row
    /// reporting `EndedNoRespawn` (a resting authority) is STILL adopted
    /// here, unconditionally — no spawn either way — because a resting
    /// authority is stopped by the end flow and restarted by reset, both
    /// #182's territory, not this scan's. Only `Some(StartMode::Resume)`
    /// (pointer published, lane doesn't answer — its supervisor really
    /// is gone) still spawns `--resume` here. `Some(StartMode::Start)`
    /// shouldn't occur (the candidate filter below already requires a
    /// published pointer) but is handled defensively as a skip, same as
    /// before this fix.
    ///
    /// A state directory with NO matching registry entry is left
    /// COMPLETELY untouched, logged once (ADR 0042: "the daemon's
    /// workspace list is the list" — an orphan is not addressable
    /// through any op, so resuming it would create a live, unaddressable
    /// process; deleted, the bare-shell fallback an earlier version used
    /// here).
    ///
    /// Rule D: a candidate that still needs a spawn goes through
    /// [`start_supervisor`] — the SAME atomically-claimed spawn path
    /// `pty.open`'s start-on-attach uses — so a candidate this scan races
    /// against a concurrent `pty.open` (the exact race the field
    /// finding's own timeline could produce) spawns nothing twice; the
    /// loser gets `Ok(None)`, logged at debug (expected, not an error).
    ///
    /// Runs off the startup critical path (finding 10): `server.rs`
    /// calls this via `tokio::spawn`, never awaited, and every probe/spawn
    /// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
    /// a semaphore — thousands of preserved workspaces cannot turn this
    /// into an unbounded synchronous fan-out before the listener binds.
    pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
        // Resolved ONCE here, not re-resolved per candidate below: cheap
        // either way (`std::env::current_exe()`), but a single resolved
        // `PathBuf` cloned into each task is simpler than a fallible call
        // repeated inside every spawned closure.
        let exe = match sot_capsule_exe() {
            Ok(e) => e,
            Err(_) => {
                tracing::warn!("capsule workspace resume-scan: could not locate sot-capsule.exe next to this daemon");
                return;
            }
        };
        // 2026-09-04 amendment: the inert default anchor is never resumed
        // here either, even on the rare box where a pointer already exists
        // for it (a hand-edited toml that dropped its agent after a prior
        // real run). Every OTHER `agent == "none"` capsule row still
        // resumes: `agent_argv("none")` is a real leg (the bare platform
        // shell). The predicate — and why its runtime term matters — is
        // `Workspaces::is_inert_default_anchor`.
        let candidates: Vec<(String, Vec<String>, PathBuf, String, String)> = workspaces
            .list()
            .into_iter()
            .filter(|ws| ws.runtime == "capsule")
            .filter(|ws| !workspaces.is_inert_default_anchor(ws))
            .filter(|ws| {
                let state_dir = super::state_dir_for(&state_root, &ws.workspace_id);
                sot_log::pointer::pointer_path(&state_dir).is_file()
            })
            .filter_map(|ws| match agent_argv(&ws.agent) {
                Ok(argv) => Some((
                    ws.workspace_id.clone(),
                    argv,
                    ws.project_root.clone(),
                    ws.agent_name.clone(),
                    ws.slug.clone(),
                )),
                Err(e) => {
                    tracing::warn!(
                        workspace_id = %ws.workspace_id, error = %e,
                        "capsule workspace resume-scan: registry entry has an unsupported agent kind; skipping"
                    );
                    None
                }
            })
            .collect();

        let semaphore = Arc::new(tokio::sync::Semaphore::new(LANE_CONCURRENCY));
        let mut joins = Vec::with_capacity(candidates.len());
        for (workspace_id, argv, cwd, agent_name, slug) in candidates {
            let permit = semaphore.clone();
            let state_root = state_root.clone();
            let workspaces = workspaces.clone();
            let exe = exe.clone();
            joins.push(tokio::spawn(async move {
                let _permit = permit.acquire_owned().await;
                let state_dir = super::state_dir_for(&state_root, &workspace_id);
                // `start_mode_needed` is documented BLOCKING (`phase_of`
                // itself is a real lane connect+challenge+status round
                // trip) -- run it the same way `workspace.list`'s own
                // lane gather already does, not directly inside this
                // async task.
                let mode = {
                    let dir = state_dir.clone();
                    tokio::task::spawn_blocking(move || start_mode_needed(&dir)).await.unwrap_or(Some(StartMode::Resume))
                };
                match mode {
                    None => {
                        // Alive: fetch the SAME lane's retained process
                        // handle via a second, explicit `query_status`
                        // call -- `start_mode_needed`'s own probe already
                        // proved it answers but (by design, shared with
                        // `ensure_started`) doesn't retain the handle this
                        // scan specifically needs to install a watchdog.
                        // A benign race: the lane could go quiet between
                        // the two calls (the old supervisor finally
                        // exiting on its own) -- this second call then
                        // simply fails and this arm logs at debug and
                        // does nothing further; the row reads whatever
                        // `phase_of` next reports, and the next attach's
                        // `ensure_started` starts it fresh if needed.
                        let dir = state_dir.clone();
                        let probe = tokio::task::spawn_blocking(move || {
                            sot_log::supervisor_client::query_status(&dir)
                        })
                        .await;
                        match probe {
                            Ok(Ok((status, process))) => {
                                tracing::info!(
                                    workspace_id = %workspace_id, phase = phase_str(status.phase),
                                    "capsule supervisor adopted (alive from a previous daemon)"
                                );
                                watch_adopted_leg(workspace_id, exe, state_dir, argv, cwd, agent_name, slug, process, workspaces);
                            }
                            Ok(Err(e)) => {
                                tracing::debug!(
                                    workspace_id = %workspace_id, error = %e,
                                    "capsule workspace resume-scan: lane went quiet between the adopt probe and the follow-up query; skipping"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule workspace resume-scan: adopt probe task did not complete");
                            }
                        }
                    }
                    Some(StartMode::Resume) => {
                        // ADR 0043 decision 29, Codex review round finding
                        // 5: the claim is taken HERE, synchronously,
                        // BEFORE `spawn_blocking` — exactly the
                        // `workspace.create` handler's own discipline —
                        // never inside the queued closure. Between this
                        // task's own `start_mode_needed` decision (above,
                        // itself already a `spawn_blocking` round trip)
                        // and a closure merely QUEUED on the blocking
                        // pool, an intervening `pty.open` (`ensure_started`)
                        // can reach the SAME freshly-observed "dead"
                        // workspace, claim, spawn, and RELEASE the slot
                        // before this closure ever runs — claiming inside
                        // the closure would then claim it a SECOND time
                        // and spawn a redundant supervisor. Taking the
                        // claim now makes that race resolve exactly like
                        // `workspace.create`'s own: whoever claims first
                        // spawns; the loser sees the slot already held and
                        // skips.
                        if workspaces.try_begin_capsule_start(&workspace_id) {
                            let workspace_id_for_log = workspace_id.clone();
                            // A separate handle, kept OUTSIDE the closure
                            // below, so a panicked spawn can still release
                            // the claim it took above (the closure's own
                            // `workspaces` is consumed by the move and
                            // never runs its own cleanup if it panics
                            // before doing so).
                            let workspaces_for_release = workspaces.clone();
                            let spawn_result = tokio::task::spawn_blocking(move || {
                                start_supervisor_claimed(&state_root, &workspace_id, StartMode::Resume, &argv, &cwd, &agent_name, &slug, workspaces)
                            })
                            .await;
                            match spawn_result {
                                Ok(Ok(())) => {
                                    tracing::info!(workspace_id = %workspace_id_for_log, "capsule workspace supervisor resumed");
                                }
                                Ok(Err(e)) => {
                                    tracing::warn!(workspace_id = %workspace_id_for_log, error = %e, "capsule workspace supervisor resume spawn failed");
                                }
                                Err(join_err) => {
                                    // Finding 5: a panic before
                                    // `start_supervisor_claimed`'s own
                                    // cleanup tasks are installed would
                                    // otherwise leave `workspace_id` wedged
                                    // "starting" forever — released here,
                                    // mirroring `workspace.create`'s own
                                    // join-error handling.
                                    workspaces_for_release.end_capsule_start(&workspace_id_for_log);
                                    tracing::warn!(workspace_id = %workspace_id_for_log, error = %join_err, "capsule workspace resume-scan: resume spawn task panicked");
                                }
                            }
                        } else {
                            tracing::debug!(workspace_id = %workspace_id, "capsule workspace resume-scan: launch already in flight; skipping");
                        }
                    }
                    Some(StartMode::Start) => {
                        // Shouldn't happen -- the candidate filter above
                        // already required a published pointer -- but
                        // stays a no-op rather than a Start: fresh starts
                        // are ensure_started's job, not this scan's.
                        tracing::debug!(
                            workspace_id = %workspace_id,
                            "capsule workspace resume-scan: no published pointer at probe time; skipping (ensure_started starts it fresh)"
                        );
                    }
                }
            }));
        }
        for j in joins {
            let _ = j.await;
        }

        log_registryless_state_dirs(&state_root, &workspaces);
    }

    /// One log line naming every `<state-root>/workspaces/*` directory
    /// with no matching registry entry — diagnostic only, never acted on
    /// (see [`resume_all`]'s own doc).
    fn log_registryless_state_dirs(state_root: &Path, workspaces: &Workspaces) {
        let dir = state_root.join("workspaces");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(e) => {
                tracing::debug!(dir = ?dir, error = %e, "capsule workspace resume-scan: could not read the state root for the registryless-directory log sweep");
                return;
            }
        };
        let known: std::collections::HashSet<String> =
            workspaces.list().into_iter().map(|ws| ws.workspace_id.clone()).collect();
        let orphans: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|id| !known.contains(id))
            .collect();
        if !orphans.is_empty() {
            tracing::warn!(
                count = orphans.len(), ids = ?orphans,
                "capsule workspace resume-scan: state directories with no matching registry entry -- \
                 left untouched (ADR 0042: the workspace list is the list)"
            );
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub use runtime::*;

/// ADR 0042 amendment (2026-09-07), "a session types into and reads a
/// sibling row": the daemon's own HEADLESS client on a capsule lane — the
/// same [`sot_log::fe_client_io::FeAttachClient`] the frontend's drawer
/// uses, run on the daemon side with no viewport and no user watching.
/// `type_into` takes the pen only long enough to deliver ONE `input` frame
/// and never resizes the pane (ADR 0041's take-on-first-input semantics,
/// applied to a second kind of client); `screen_of` attaches as a pure
/// WATCHER and never takes at all. Platform-neutral: gated the same as
/// `mod runtime` above (`FeAttachClient` itself is `#![cfg(any(windows,
/// target_os = "linux"))]`-gated in `sot_log`), so this module simply does
/// not exist on a host that cannot run a capsule row in the first place.
#[cfg(any(windows, target_os = "linux"))]
pub mod headless {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use sot_log::client::PlatformEndpoint;
    use sot_log::fe_client::TAKE_QUEUE_CAP;
    use sot_log::fe_client_io::{FeAttachClient, InputOutcome};
    use sot_log::state_dir::state_dir_hash;

    /// This daemon build's own concrete attach client — always the real
    /// platform endpoint (pipes on Windows, a Unix socket on Linux). No
    /// caller of this module ever needs to name `E` itself.
    type Client = FeAttachClient<PlatformEndpoint>;

    /// How often the daemon polls its own headless client. Independent of
    /// (and much finer than) the deadline a caller passes in — this is
    /// just the local spin-wait granularity, not a protocol budget.
    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    /// Bound for the client's own worker-thread join on every exit path
    /// (`FeAttachClient::shutdown`). Generous relative to the worker's
    /// 100 ms tick, but still a REAL bound — "on EVERY exit path: drop the
    /// client, then observe the worker's closure" (ADR 0042 amendment §3).
    const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

    /// What a headless op failed to do, and where. `phase` is one of
    /// `size` (the size gate, before any attach), `attach`, `checkpoint`,
    /// `take` (the pen was asked for but never granted, and — since
    /// nothing reached [`Client::send_input`]'s own wire flush yet —
    /// `submitted` is `false` here), `input` (a definite
    /// `input_refused_stale`; never retried by this module), `record`
    /// (the record's own verdict is UNKNOWABLE: either the wire said
    /// `input_delivery_unknown`, or the deadline expired after the input
    /// had already been handed to the lane — both mean the same thing to
    /// a caller: do not retry, and do not assume failure either), or
    /// `detach` (reserved for the shutdown bound itself; see its own doc —
    /// today this is only ever logged, never returned as an `Err`).
    #[derive(Debug)]
    pub struct HeadlessError {
        pub phase: &'static str,
        pub detail: String,
        /// `true` iff the input had already been handed to the lane
        /// (`Client::send_input` called) when the failure/expiry hit —
        /// the daemon's own signal to answer `capsule_input_unknown`
        /// rather than a flat failure (ADR 0042 amendment §2).
        pub submitted: bool,
    }

    /// `screen_of`'s own result: the row's current, visible screen.
    /// `cursor` is `(row, col)`, matching `vt100_ctt::Screen::
    /// cursor_position`'s own order.
    pub struct ScreenShot {
        pub cols: u16,
        pub rows: u16,
        pub lines: Vec<String>,
        pub cursor: Option<(u16, u16)>,
    }

    /// Types `bytes` into the row at `state_dir` as `controller_id`,
    /// taking the pen only long enough to deliver them — never resizing
    /// the pane (a headless client has no viewport to size it to). One
    /// absolute `deadline` covers attach, checkpoint, take, and the wait
    /// for the wire's own verdict on the input; every exit path drops the
    /// client and observes the worker's closure within [`SHUTDOWN_WAIT`].
    /// Returns the number of payload bytes delivered (never counting a
    /// trailing Enter byte the caller may have already folded in — this
    /// function has no opinion on that, it delivers exactly what it is
    /// given).
    pub fn type_into(
        state_dir: &Path,
        controller_id: &str,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<usize, HeadlessError> {
        if bytes.len() > TAKE_QUEUE_CAP {
            return Err(HeadlessError {
                phase: "size",
                detail: format!(
                    "payload is {} bytes, exceeding the take queue cap of {TAKE_QUEUE_CAP} bytes",
                    bytes.len()
                ),
                submitted: false,
            });
        }
        if bytes.is_empty() {
            // "an empty payload succeeds without taking" (ADR 0042
            // amendment review) — nothing to attach for.
            return Ok(0);
        }

        let mut client = attach(state_dir, controller_id)?;
        if let Err(e) = wait_for_checkpoint(&mut client, deadline) {
            client.shutdown(SHUTDOWN_WAIT);
            return Err(e);
        }

        let expected = bytes.len() as u64;
        let before = client.recorded_bytes();
        client.send_input(bytes);

        let result = loop {
            client.pump();
            if let Some(outcome) = client.last_input_outcome() {
                match outcome {
                    InputOutcome::Recorded => {
                        if client.recorded_bytes().saturating_sub(before) >= expected {
                            break Ok(bytes.len());
                        }
                        // A single `send_input` call is always flushed as
                        // ONE input frame in practice (the payload already
                        // fits under `TAKE_QUEUE_CAP`, so nothing splits
                        // it) — this branch should be unreachable, but
                        // correctness does not depend on that: keep
                        // polling for the rest, bounded by the same
                        // deadline, rather than declaring victory early.
                        if Instant::now() >= deadline {
                            break Err(HeadlessError {
                                phase: "record",
                                detail: "deadline exceeded before the whole payload was recorded"
                                    .to_string(),
                                submitted: true,
                            });
                        }
                        std::thread::sleep(POLL_INTERVAL);
                    }
                    InputOutcome::RefusedStale => {
                        break Err(HeadlessError {
                            phase: "input",
                            detail: "input refused as stale (the take epoch changed); \
                                     this op is never retried"
                                .to_string(),
                            submitted: true,
                        });
                    }
                    InputOutcome::DeliveryUnknown => {
                        break Err(HeadlessError {
                            phase: "record",
                            detail: "input delivery unknown".to_string(),
                            submitted: true,
                        });
                    }
                }
                continue;
            }
            if client.is_dead() {
                break Err(HeadlessError {
                    phase: "take",
                    detail: client.status_line().to_string(),
                    submitted: true,
                });
            }
            if Instant::now() >= deadline {
                break Err(HeadlessError {
                    phase: "record",
                    detail: "deadline exceeded waiting for the input to be recorded".to_string(),
                    submitted: true,
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        client.shutdown(SHUTDOWN_WAIT);
        result
    }

    /// Reads the current, visible screen of the row at `state_dir` as a
    /// pure WATCHER — never takes the pen, never sends input. Same
    /// deadline/shutdown discipline as [`type_into`].
    pub fn screen_of(
        state_dir: &Path,
        controller_id: &str,
        deadline: Instant,
    ) -> Result<ScreenShot, HeadlessError> {
        let mut client = attach(state_dir, controller_id)?;
        if let Err(e) = wait_for_checkpoint(&mut client, deadline) {
            client.shutdown(SHUTDOWN_WAIT);
            return Err(e);
        }
        let (rows, cols) = client.screen().size();
        let lines: Vec<String> =
            client.screen().rows(0, cols).map(|line| line.trim_end().to_string()).collect();
        let cursor = Some(client.screen().cursor_position());
        client.shutdown(SHUTDOWN_WAIT);
        Ok(ScreenShot { cols, rows, lines, cursor })
    }

    fn attach(state_dir: &Path, controller_id: &str) -> Result<Client, HeadlessError> {
        Client::attach_headless(PlatformEndpoint::default(), state_dir_hash(state_dir), controller_id.to_string()).map_err(|e| {
            HeadlessError { phase: "attach", detail: e.to_string(), submitted: false }
        })
    }

    fn wait_for_checkpoint(client: &mut Client, deadline: Instant) -> Result<(), HeadlessError> {
        loop {
            client.pump();
            if client.is_checkpointed() {
                return Ok(());
            }
            if client.is_dead() {
                return Err(HeadlessError {
                    phase: "checkpoint",
                    detail: client.status_line().to_string(),
                    submitted: false,
                });
            }
            if Instant::now() >= deadline {
                return Err(HeadlessError {
                    phase: "checkpoint",
                    detail: "deadline exceeded before a checkpoint arrived".to_string(),
                    submitted: false,
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[cfg(test)]
mod headless_size_gate_tests {
    // Pure size-gate tests: `type_into` checks the payload length BEFORE
    // ever attempting an attach, so these need no supervisor, no state
    // dir on disk, and no real process at all — a nonexistent path is
    // fine, and a real attach attempt against it would prove the test
    // wrong (the size gate must short-circuit before that).
    use super::headless::type_into;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn oversized_payload_is_refused_before_any_attach() {
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP + 1];
        let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
            .expect_err("oversized payload must be refused");
        assert_eq!(err.phase, "size");
        assert!(!err.submitted);
    }

    #[test]
    fn exactly_the_cap_is_not_oversized() {
        // The cap itself is legal — only `CAP + 1` is refused. This
        // would attempt a real attach (and fail on the nonexistent path
        // some other way), which is enough to prove the size gate did
        // NOT reject it — that failure is expected and not asserted on
        // further than "it is not the size-gate error."
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP];
        let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
            .expect_err("a nonexistent state dir cannot succeed");
        assert_ne!(err.phase, "size", "the cap itself must not trip the size gate");
    }

    #[test]
    fn empty_payload_succeeds_with_no_attach() {
        let n = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &[], deadline())
            .expect("an empty payload succeeds trivially, with nothing to attach for");
        assert_eq!(n, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_joins_workspaces_and_the_id() {
        let root = Path::new("/tmp/sot-state-root");
        assert_eq!(
            state_dir_for(root, "ws-alpha-1a2b"),
            PathBuf::from("/tmp/sot-state-root/workspaces/ws-alpha-1a2b")
        );
    }

    // Field defect (v0.6.0-rc.12): a supervisor that died out from under
    // a row (e.g. a daemon-pair converge that ended the old build's
    // supervisor) left the state dir holding only a lock FILE, and
    // `end_run` used to treat every unreachable lane identically -- kept
    // forever, with no way to tell a merely-unresponsive live holder
    // from no holder at all. `query_status` fails against this temp dir
    // either way (nothing is listening on its lane) -- what changes
    // between the two cases below is whether `supervisor.lock` itself is
    // free to take.
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn end_run_is_unheld_when_the_lock_is_free_and_kept_when_it_is_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path();

        // No supervisor.lock exists yet at all -- the lock is free to
        // take, so nobody holds this row: `Unheld`.
        match end_run(state_dir, "test reason") {
            Ok(super::EndRunOutcome::Unheld) => {}
            other => panic!("expected Ok(Unheld) with no holder present: {other:?}"),
        }

        // Something else holds the fence right now -- the lock attempt
        // must fail, so `end_run` keeps today's unreachable-lane
        // refusal (Err) rather than fabricating Unheld out from under a
        // live holder.
        let holder = sot_log::fence::lock_supervisor(state_dir).expect("take the fence");
        match end_run(state_dir, "test reason") {
            Err(_) => {}
            Ok(outcome) => {
                panic!("expected the unchanged unreachable-lane Err while the fence is held: {outcome:?}")
            }
        }
        drop(holder);
    }

    #[test]
    #[cfg(windows)]
    fn agent_argv_claude_matches_ccbs_own_flags() {
        assert_eq!(
            agent_argv("claude").unwrap(),
            vec!["claude", "--permission-mode", "auto", "/sot-session-start"]
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn agent_argv_claude_fails_closed_when_nothing_resolves() {
        // No PATH, no HOME: nothing to search, so this must refuse
        // rather than hand `sot-capsule` an unresolved bare "claude" it
        // would only fail to spawn later, one layer down. `agent_argv`
        // reads the REAL process PATH/HOME (unlike `resolve_claude`'s own
        // dependency-injected tests below), and a real dev box typically
        // DOES have a real `claude` installed somewhere on one of them —
        // so both are cleared here, under the shared env-test lock, to
        // make the "nothing resolves" precondition true regardless of
        // the host running this test.
        let _guard = self_file_env_guarded();
        let prior_path = std::env::var_os("PATH");
        let prior_home = std::env::var_os("HOME");
        std::env::remove_var("PATH");
        std::env::remove_var("HOME");
        let result = agent_argv("claude");
        match prior_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        match prior_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        assert!(result.is_err());
    }

    #[test]
    #[cfg(not(any(windows, target_os = "linux")))]
    fn agent_argv_claude_is_refused_off_windows_and_linux() {
        assert!(agent_argv("claude").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn agent_argv_none_is_the_bare_shell() {
        assert_eq!(agent_argv("none").unwrap(), vec!["cmd.exe"]);
    }

    #[test]
    #[cfg(not(windows))]
    fn agent_argv_none_is_the_login_shell_or_bin_sh() {
        // `self_file_env_guarded` doesn't itself save/restore SHELL (it
        // guards a different, fixed set of vars) — still acquired here
        // for its SERIALIZATION lock, shared with every other env-var
        // test in this file; SHELL is saved/restored by hand around it.
        let _guard = self_file_env_guarded();
        let prior_shell = std::env::var_os("SHELL");
        std::env::set_var("SHELL", "/bin/zsh");
        assert_eq!(agent_argv("none").unwrap(), vec!["/bin/zsh"]);
        std::env::remove_var("SHELL");
        assert_eq!(agent_argv("none").unwrap(), vec!["/bin/sh"]);
        match prior_shell {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
    }

    #[test]
    fn agent_argv_rejects_unsupported_kinds() {
        assert!(agent_argv("codex").is_err());
        assert!(agent_argv("bogus").is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolve_claude_finds_an_executable_on_path() {
        let dir = tempfile_test_dir();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, b"#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&claude);
        let path_var = std::ffi::OsString::from(dir.path());
        let resolved = resolve_claude(Some(&path_var), None).unwrap();
        assert_eq!(resolved, claude.to_string_lossy());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolve_claude_falls_back_to_local_bin_under_home() {
        let dir = tempfile_test_dir();
        let local_bin = dir.path().join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let claude = local_bin.join("claude");
        std::fs::write(&claude, b"#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&claude);
        // An empty PATH still finds it via the HOME-derived fallback dirs.
        let resolved = resolve_claude(None, Some(dir.path())).unwrap();
        assert_eq!(resolved, claude.to_string_lossy());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolve_claude_ignores_a_non_executable_file() {
        let dir = tempfile_test_dir();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, b"not a real program").unwrap(); // no +x
        let path_var = std::ffi::OsString::from(dir.path());
        let err = resolve_claude(Some(&path_var), None).unwrap_err();
        assert!(err.contains("claude not found"), "{err}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolve_claude_names_every_directory_it_searched() {
        let err = resolve_claude(None, None).unwrap_err();
        assert!(err.contains("claude not found"), "{err}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolve_claude_skips_a_same_named_directory_for_a_later_real_file() {
        // Review round, reproduced: a directory named `claude` passes
        // `access(X_OK)` (the execute bit on a directory means
        // "searchable") -- without the regular-file check this would
        // have wrongly "resolved" to the directory in `dir1`, never
        // reaching the REAL executable in `dir2`.
        let dir1 = tempfile_test_dir();
        std::fs::create_dir(dir1.path().join("claude")).unwrap();
        let dir2 = tempfile_test_dir();
        let real = dir2.path().join("claude");
        std::fs::write(&real, b"#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&real);
        let path_var = std::env::join_paths([dir1.path(), dir2.path()]).unwrap();
        let resolved = resolve_claude(Some(&path_var), None).unwrap();
        assert_eq!(resolved, real.to_string_lossy());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolve_claude_skips_a_relative_path_entry() {
        // Review round, reproduced: a relative PATH entry resolves
        // against the daemon's own current directory, not the eventual
        // workspace -- it must be skipped outright, never joined against
        // anything, even when (as here) it happens to be the ONLY entry.
        let path_var = std::ffi::OsString::from("relative/bin");
        let err = resolve_claude(Some(&path_var), None).unwrap_err();
        assert!(err.contains("claude not found"), "{err}");
        assert!(!err.contains("relative/bin"), "a relative entry must never even be searched: {err}");
    }

    #[cfg(target_os = "linux")]
    fn tempfile_test_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[cfg(target_os = "linux")]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn mode_flag_matches_the_sot_capsule_cli() {
        assert_eq!(mode_flag(StartMode::Start), "--start");
        assert_eq!(mode_flag(StartMode::Resume), "--resume");
    }

    #[test]
    fn phase_str_is_total_and_snake_case() {
        use sot_log::wire::SupervisorPhase;
        assert_eq!(phase_str(SupervisorPhase::Starting), "starting");
        assert_eq!(phase_str(SupervisorPhase::Ready), "ready");
        assert_eq!(phase_str(SupervisorPhase::Ending), "ending");
        assert_eq!(phase_str(SupervisorPhase::EndedNoRespawn), "ended_no_respawn");
        assert_eq!(phase_str(SupervisorPhase::Terminal), "terminal");
    }

    #[test]
    fn never_started_phase_is_distinct_from_unreachable_and_every_answered_phase() {
        // First live shakedown fix: a capsule workspace nobody has ever
        // started must read as quietly "stopped", not as the loud
        // "unreachable" a query FAILURE reports — the two must never
        // collide with each other or with any answered lifecycle phase.
        use sot_log::wire::SupervisorPhase;
        assert_ne!(NEVER_STARTED_PHASE, UNREACHABLE_PHASE);
        for p in [
            SupervisorPhase::Starting,
            SupervisorPhase::Ready,
            SupervisorPhase::Ending,
            SupervisorPhase::EndedNoRespawn,
            SupervisorPhase::Terminal,
        ] {
            assert_ne!(phase_str(p), NEVER_STARTED_PHASE);
        }
    }

    #[test]
    fn phase_for_missing_pointer_only_fires_when_the_pointer_is_absent() {
        // Rule B: a published pointer means a supervisor reached a real
        // run at least once — that case defers to the real query (`None`)
        // rather than guessing; only a genuinely absent pointer
        // short-circuits to `NEVER_STARTED_PHASE`.
        assert_eq!(
            phase_for_missing_pointer(false),
            Some(NEVER_STARTED_PHASE)
        );
        assert_eq!(phase_for_missing_pointer(true), None);
    }

    #[test]
    fn unreachable_phase_is_distinct_from_every_answered_phase() {
        use sot_log::wire::SupervisorPhase;
        for p in [
            SupervisorPhase::Starting,
            SupervisorPhase::Ready,
            SupervisorPhase::Ending,
            SupervisorPhase::EndedNoRespawn,
            SupervisorPhase::Terminal,
        ] {
            assert_ne!(phase_str(p), UNREACHABLE_PHASE);
        }
    }

    #[test]
    fn restart_budget_numbers_match_adr_0041s_own_launcher_table() {
        assert_eq!(RESTART_BACKOFFS.len(), 5);
        assert_eq!(MAX_RESTARTS_PER_WINDOW, 5);
        assert_eq!(RESTART_WINDOW, std::time::Duration::from_secs(60));
        assert_eq!(
            RESTART_BACKOFFS.map(|d| d.as_secs()),
            [1, 3, 7, 15, 30]
        );
    }

    #[test]
    fn nesting_env_scrub_list_matches_ccb() {
        // Mirrors comm/adapters/claude/bin/ccb's own `unset` line
        // exactly -- see that file for the reasoning per variable.
        assert_eq!(
            NESTING_ENV_VARS_TO_SCRUB,
            &[
                "CLAUDE_CODE_FORK_SUBAGENT",
                "CLAUDE_CODE_CHILD_SESSION",
                "CLAUDE_CODE_TEAMMATE_MODE",
                "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
                "CLAUDECODE",
                "AI_AGENT",
                "CLAUDE_CODE_SESSION_ID",
            ]
        );
    }

    // Serialized under the crate-wide `paths::ENV_TEST_LOCK` (mirrors
    // `workspaces.rs`'s own `EnvGuard` exactly — see that module's
    // comment: `cargo test` runs in parallel within one process, and
    // several modules' resolvers read the SAME env vars, HOME included).
    struct SelfFileEnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        home: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_state_host: Option<std::ffi::OsString>,
    }

    impl Drop for SelfFileEnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("HOME", &self.home),
                ("USERPROFILE", &self.userprofile),
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("SOT_STATE_HOST", &self.sot_state_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn self_file_env_guarded() -> SelfFileEnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        SelfFileEnvGuard {
            home: std::env::var_os("HOME"),
            userprofile: std::env::var_os("USERPROFILE"),
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            _serial: serial,
        }
    }

    #[test]
    fn capsule_supervisor_env_carries_slug_workspace_and_comm_home() {
        // Codex round: SOT_WORKSPACE is keyed on SLUG (finding 1 — the
        // frontend keys results/the active workspace by slug, not the
        // internal ws-<slug>-<hex> id), while the id is used only for the
        // self-file path. SOT_COMM_HOME and SOT_COMM_SELF_FILE
        // (comm-lib.sh's existing pin-the-self-file seam) are stamped
        // unconditionally so a capsule never falls back to the shared
        // per-host __nopane slot.
        let _guard = self_file_env_guarded();
        std::env::set_var("SOT_STATE_HOST", "testhost");
        std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
        let env = capsule_supervisor_env(
            "ws-myrepo-1a2b",
            "myrepo",
            Path::new("/home/me/myrepo"),
            "myrepo-myhost",
        );
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SOT_WORKSPACE"), Some("myrepo"));
        assert_eq!(get("SOT_WORKSPACE_ROOT"), Some("/home/me/myrepo"));
        assert_eq!(get("SOT_COMM_NAME"), Some("myrepo-myhost"));
        assert_eq!(get("SOT_SESSION"), Some("1"));
        assert_eq!(get("SOT_COMM_HOME"), Some("/fake-home/.sot-comm"));
        assert_eq!(
            get("SOT_COMM_SELF_FILE"),
            Some("/fake-home/.sot-comm/self/testhost__ws-myrepo-1a2b.txt")
        );
    }

    #[test]
    fn capsule_supervisor_env_omits_comm_name_when_unnamed() {
        // Codex round finding 2: NO synthesized default here — an
        // un-pinned autostart (no explicit agent_name in the request)
        // gets no SOT_COMM_NAME at all. comm-join.sh's own #148
        // auto-disambiguating derivation decides the handle instead
        // (reading SOT_COMM_SELF_FILE to know where to write it), exactly
        // as a hand-started shell would — a synthesized <slug>-<host>
        // pin would become an explicit overwrite of any existing row of
        // that name, which PROTOCOL.md's "never reuse a handle" forbids.
        // SOT_WORKSPACE/SOT_COMM_HOME/SOT_COMM_SELF_FILE stay
        // unconditional regardless.
        let _guard = self_file_env_guarded();
        std::env::set_var("SOT_STATE_HOST", "testhost");
        std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
        let env = capsule_supervisor_env("ws-anon-9f9f", "anon", Path::new("/home/me/anon"), "");
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SOT_WORKSPACE"), Some("anon"));
        assert_eq!(get("SOT_COMM_NAME"), None);
        assert_eq!(
            get("SOT_COMM_SELF_FILE"),
            Some("/fake-home/.sot-comm/self/testhost__ws-anon-9f9f.txt")
        );
    }

    #[test]
    fn capsule_comm_home_str_none_when_no_home_var_is_set() {
        let _guard = self_file_env_guarded();
        std::env::remove_var("SOT_COMM_HOME");
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
        assert_eq!(capsule_comm_home_str(), None);
    }
}
