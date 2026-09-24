// capsule_workspace.rs — ADR 0042 slice L1a / ADR 0043 decision 22: the
// daemon's capsule workspace runtime, on Windows AND Linux. One
// `sot-capsule supervise <state-dir>` authority per capsule workspace,
// spawned DETACHED so it survives the daemon's own exit — the daemon is
// never its kill domain. Which platform is chosen is exactly TWO forks
// inside `mod runtime` (the capsule executable's name and the detach
// mechanism — that module's own doc says why this used to be three)
// — everything else in that module is byte-identical on both platforms.
// `runtime: "tmux"`
// rows stay exactly what they are today; this module never touches
// them. ADR 0042's rule now holds on Linux too (L6 / this repo's B6
// lane, once the bridge gave a capsule row a remote attach path):
// `workspace.create`'s absent `runtime` resolves to "capsule" here
// just as it does on Windows; `"tmux"` still exists for an operator's
// explicit ask, and old rows retire by attrition.
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
/// 3. `statfs` the resolved root and refuse a root that is not durable
///    enough to keep RESUMING capsule rows from across a daemon restart
///    — a SECOND, daemon-side deny list answering a different question
///    than [`sot_log::state_dir::preflight_volume`]'s own remote-fs one:
///    that one asks whether the store's primitives work at all (tmpfs
///    passes — see its own doc); this one asks about durability, which
///    tmpfs/ramfs answer no to regardless of how well they support
///    rename/fsync. Each Unix asks it the way ITS OWN `statfs(2)`
///    actually answers, exactly as `sot_log::fsutil::remote_volume_name`
///    already splits: Linux matches the `f_type` magic against its own
///    volatile list ([`linux_only`]); macOS has no tmpfs or ramfs to
///    name at all, and a RAM disk there mounts as plain `apfs`/`hfs`, so
///    a fstype list would be an empty gesture — Darwin's own honest
///    answer is the `MNT_LOCAL` mount flag, "stored locally"
///    ([`macos_only`]), which is also the one check that catches a
///    network home whose fstype spelling `preflight_volume`'s substring
///    list happens to miss. Then `preflight_volume` itself, mapped to
///    its `Display` text. Windows: unchanged — the existing NTFS-only
///    arm already refuses everything this would and more.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn qualified_state_root() -> Result<PathBuf, String> {
    let root = sot_log::state_dir::sot_state_dir()
        .ok_or_else(|| format!("could not resolve this machine's state root ({STATE_ROOT_HINT} unset)"))?;
    // Security review addendum (item 2b, v0.6.5 macOS field report): the
    // runtime dir and sockets already get `is_private_dir`'s
    // symlink/ownership/mode check; this root -- what `XDG_STATE_HOME`
    // selects -- did not, and on a shared host it can sit under a
    // world-writable sticky parent (e.g. /scratch), where an attacker-
    // precreated or symlinked directory would receive session records.
    // `secure_private_dir` is the create-or-verify guard the tmux socket
    // dir already uses for exactly this threat model (atomic 0700 create
    // when absent; symlink/owner/mode-checked, never trusted, when
    // present) -- reused here rather than a second bespoke check.
    crate::paths::secure_private_dir(&root)
        .map_err(|e| format!("could not secure the state root {root:?}: {e}"))?;
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
    #[cfg(target_os = "macos")]
    {
        if !macos_only::mounted_locally(&root)
            .map_err(|e| format!("could not statfs the state root {root:?}: {e}"))?
        {
            return Err(format!(
                "state root {root:?} is not on a locally attached filesystem: capsule records need \
                 durable, non-volatile storage (set XDG_STATE_HOME to a local disk; ADR 0043 decision 23)"
            ));
        }
    }
    sot_log::state_dir::preflight_volume(&root).map_err(|e| e.to_string())?;
    Ok(root)
}

/// The invariant [`qualified_state_root`] alone cannot enforce (it never
/// sees a project root): a workspace's file watcher recursively watches
/// `project_root`, holding directory handles under it, and on Windows an
/// open handle blocks a rename of the directory it is in -- so
/// `voyages\<uuid>` publication fails with a sharing violation whenever a
/// capsule's state tree sits inside a directory this daemon watches. A
/// capsule row's state directory must never lie inside (or at) its
/// project root. Each side is canonicalized first, falling back to its
/// given spelling if that fails, so a symlinked root is judged by what it
/// resolves to, not its spelling.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn state_root_inside_project(state_dir: &Path, project_root: &Path) -> bool {
    let state_dir = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let project_root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    crate::paths::path_within_root(&state_dir, &project_root)
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

/// [`linux_only`]'s Darwin twin: the same "is this root durable enough
/// to resume capsule rows from" question, asked the way macOS actually
/// answers it. There is no `f_type` magic to match (that field does not
/// exist in Darwin's `statfs`) and no tmpfs or ramfs to match it
/// against — a macOS RAM disk is `hdiutil` + `newfs_hfs`, and it mounts
/// as ordinary `apfs`/`hfs`, indistinguishable by type from the
/// internal disk. So this asks the mount flag instead: `MNT_LOCAL`, "the
/// filesystem is stored locally", which is false for exactly the mounts
/// a capsule state root must never sit on (NFS, SMB, WebDAV, AFP, a
/// `fuse-t` network bridge) and true for the internal disk and any
/// directly attached volume. Strictly wider than the fstype spelling
/// `sot_log::fsutil::remote_volume_name` matches by substring, which is
/// why it is worth having as this daemon's own second gate rather than
/// leaving the whole question to `preflight_volume`.
#[cfg(target_os = "macos")]
mod macos_only {
    use std::path::Path;

    /// `true` iff `dir`'s mount carries `MNT_LOCAL`. Mirrors
    /// `sot_log::fsutil`'s own macOS `statfs` call shape rather than
    /// adding a second one with different error handling.
    pub(super) fn mounted_locally(dir: &Path) -> std::io::Result<bool> {
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
        Ok((buf.f_flags & libc::MNT_LOCAL as u32) != 0)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_build_directory_is_mounted_locally() {
            // A CI runner's and a developer's checkout are both on the
            // boot volume; a state root that is NOT local is exactly what
            // `qualified_state_root` exists to refuse, so this asserts
            // the true half against a directory that certainly qualifies.
            assert!(mounted_locally(std::path::Path::new(".")).expect("statfs ."));
        }
    }
}

/// The agent argv `sot-capsule supervise` spawns as its producer.
/// `"claude"` and `"codex"` (Unix only) each get their own launcher
/// recipe, sharing ONE resume token, `--continue`, stripped from a row's
/// first-ever leg ([`first_leg_without_continue`]). `"none"` is the bare
/// platform shell; every other kind is refused, never substituted.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn agent_argv(agent_kind: &str) -> Result<Vec<String>, String> {
    match agent_kind {
        "none" => Ok(vec![none_argv()]),
        "claude" => claude_argv(),
        "codex" => codex_argv(),
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
fn none_argv() -> String {
    "cmd.exe".to_string()
}
#[cfg(not(windows))]
fn none_argv() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// The one shared builder for claude's launch flags (ADR 0046 decision
/// 4): a capsule spawn ([`claude_argv`], always `resume: true`, no extra
/// flags — a supervised row always resumes its root's own conversation)
/// and `sotd agent-exec` (`main.rs`, `resume: false`, the caller's own
/// flags — `--continue` is `agent-exec`'s caller's choice, never added
/// for it) build the SAME shape from here, so the two can never drift
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
/// (`--continue`, no skill argv) is unrelated to claude's.
fn claude_recipe(resume: bool, extra: &[String]) -> Vec<String> {
    let mut argv = vec![
        "claude".to_string(),
        "--permission-mode".to_string(),
        "auto".to_string(),
    ];
    if resume {
        argv.push("--continue".to_string());
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
fn claude_argv() -> Result<Vec<String>, String> {
    Ok(claude_recipe(true, &[]))
}
/// macOS lane: widened from `target_os = "linux"` to `unix`, a DELETION
/// of the third arm that used to refuse here ("claude has no capsule
/// launcher on this host"). That refusal outlived its reason.
/// [`resolve_claude`] — the whole resolution rule — is already
/// `cfg(unix)` and already exercised on macOS by [`agent_exec_argv`],
/// which is how `ccb` itself launches there; a launcher recipe that
/// differs from `agent-exec`'s only by `--continue` cannot need a
/// narrower platform gate than the resolver it calls. The remaining
/// macOS gap is in the capsule RUNTIME, not in this argv (see `mod
/// runtime`'s own gate below), and a stale refusal here would only
/// mislabel that gap.
#[cfg(unix)]
fn claude_argv() -> Result<Vec<String>, String> {
    let claude = resolve_claude(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )?;
    let mut argv = claude_recipe(true, &[]);
    argv[0] = claude;
    Ok(argv)
}

/// `"codex"`'s capsule recipe: `ccx --capsule --continue`. `--capsule`
/// keys `ccx`'s capsule behavior directly (never an inherited env var)
/// and is never stripped; `--continue` is the shared first-leg token.
/// macOS lane: `unix`, not `target_os = "linux"` — `ccx` is the same
/// shell script installed to the same `~/.local/bin` on every Unix, so
/// the Linux gate here was naming the install layout of one host, not a
/// mechanism. The separate `not(any(windows, linux))` arm that refused
/// "codex has no capsule launcher on this host" is DELETED with it.
#[cfg(unix)]
fn codex_argv() -> Result<Vec<String>, String> {
    let ccx = resolve_ccx(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )?;
    Ok(vec![ccx, "--capsule".to_string(), "--continue".to_string()])
}

/// The declared-capability line [`check_capsule_capable`] greps for.
#[cfg(windows)]
fn codex_argv() -> Result<Vec<String>, String> {
    Err("codex has no capsule launcher on Windows (ccx is a bash script with no .ps1 counterpart)".to_string())
}

/// Unix-only argv for `sotd agent-exec <kind> [flags…]` (ADR 0046
/// decision 4) — the argv `main.rs` execs THIS process into directly,
/// never spawned as a child. Shares [`claude_recipe`] with the capsule
/// spawn path above (`resume: false` — `agent-exec` never adds
/// `--continue` itself; that stays the daemon's own default for a
/// capsule row, in [`claude_argv`]) but calls [`resolve_claude`]
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
    match kind {
        "claude" => {
            let claude = resolve_claude(
                std::env::var_os("PATH").as_deref(),
                std::env::var_os("HOME").map(PathBuf::from).as_deref(),
            )?;
            let mut recipe = claude_recipe(false, extra);
            recipe[0] = claude;
            Ok(recipe)
        }
        other => {
            // Every kind besides "claude"/"none" is already Err from
            // `agent_argv` itself (propagated verbatim, `?`); "none" is
            // the one kind that succeeds there but still has no recipe
            // HERE (see doc above), so it falls through to its own
            // refusal below.
            agent_argv(other)?;
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
/// producer never repeats a PATH search of its own (`sot_log::producer_pty`'s
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
/// by THIS process — mirrors `sot_log::producer_pty`'s own
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

/// `sot-capsule supervise`'s own `--first-leg-without --continue`, passed
/// only for [`StartMode::Start`] (a row's first-ever run) so that leg's own
/// producer argv starts a fresh claude conversation. The daemon never
/// passes it again for a resumed or restarted supervisor; the supervisor's
/// own self-heal, using this SAME token, is what strips `--continue` a
/// second time for a leg that follows an unstable one (see
/// [`agent_argv`]'s own doc).
pub fn first_leg_without_continue(mode: StartMode) -> &'static [&'static str] {
    match mode {
        StartMode::Start => &["--first-leg-without", "--continue"],
        StartMode::Resume => &[],
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationIntent {
    /// May resume, and may retire+reset an ended run.
    Selection,
    /// May resume, but never resets an ended run.
    Reconnect,
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

/// Converts to the local `Phase` (R10); [`phase_str`] stays for the wire mapping.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn local_phase(phase: sot_log::wire::SupervisorPhase) -> crate::workspaces::Phase {
    use crate::workspaces::Phase;
    use sot_log::wire::SupervisorPhase as SP;
    match phase {
        SP::Starting => Phase::Starting,
        SP::Ready => Phase::Ready,
        SP::Ending => Phase::Ending,
        SP::EndedNoRespawn => Phase::EndedNoRespawn,
        SP::Terminal => Phase::Terminal,
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
/// `NO_COLOR` rides the same path for the same reason: a daemon that
/// inherited it (a relaunch fired from inside a capsule hands its env to
/// the next daemon) painted every row's agent colourless on Windows,
/// where the pty sets no `TERM`/`COLORTERM` and `NO_COLOR` overrides
/// ConPTY's own VT detection (field report, 2026-09-17).
#[cfg_attr(not(windows), allow(dead_code))]
pub const NESTING_ENV_VARS_TO_SCRUB: &[&str] = &[
    "CLAUDE_CODE_FORK_SUBAGENT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_TEAMMATE_MODE",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "CLAUDECODE",
    "AI_AGENT",
    "CLAUDE_CODE_SESSION_ID",
    "NO_COLOR",
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
/// this fixes). Bare `SOT_SOCKET` (ADR 0046 decision 1, S4: no typed
/// prefix, Unix only), `SOT_WORKSPACE`/`SOT_WORKSPACE_ID` (and
/// `SOT_WORKSPACE_ROOT`/
/// `SOT_SESSION`/`SOT_MANUAL`) reuse [`crate::awareness::awareness_env`]
/// verbatim — ONE builder, not a second copy that could drift — keyed on
/// `slug` (Codex round finding 1: the frontend keys results and the
/// active workspace by SLUG, not the internal `ws-<slug>-<hex>` id;
/// `workspace_id` is used ALSO below, for the state dir and self-file
/// paths, where stability and uniqueness matter more than the display
/// shape). `SOT_COMM_NAME` is set ONLY for an explicitly requested
/// `agent_name` (Codex round finding 2: a synthesized default here would
/// become an explicit pin that OVERWRITES any existing registry row of
/// that name — exactly what PROTOCOL.md's "never reuse a handle" forbids,
/// and a hand-started session in the same repo on the same host derives
/// precisely `<slug>-<host>` on its own). `SOT_COMM_SELF_FILE`
/// (`<comm_home>/self/<host>__<workspace_id>.txt`, comm-lib.sh's EXISTING
/// pin-the-self-file-path seam — already used by its own test suite, and
/// already honoured unchanged by both `comm-context.sh`, the reader, and
/// `comm-join.sh`, the writer) is what actually gives the capsule its own
/// slot: `comm-join.sh`'s #148 auto-disambiguating derivation decides the
/// handle and writes it there. The session inside the capsule now also
/// DECLARES it via `agent.join` over this same pinned `SOT_SOCKET`
/// (`Workspace.agent_handle`, `handlers::handle_agent_join`); a declared
/// `agent_handle` wins when present, but the daemon's OWN read-back of
/// that same file (`handlers::capsule_comm_handle`) stays as the
/// FALLBACK for a row with no declaration yet (manager review, S5) —
/// deleted only with family H once every row has cycled onto `agent.join`.
/// Pure (no I/O beyond env reads): exercised by
/// the cross-platform test suite even though
/// [`runtime::spawn_detached_supervisor`], its only caller, is gated to
/// Windows and Linux only.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn capsule_supervisor_env(workspace_id: &str, slug: &str, cwd: &Path, agent_name: &str) -> Vec<(String, String)> {
    let mut env = crate::awareness::awareness_env(Some(slug), Some(cwd), Some(workspace_id));
    if !agent_name.is_empty() {
        env.push(("SOT_COMM_NAME".to_string(), agent_name.to_string()));
    }
    if let Some(comm_home) = capsule_comm_home_str() {
        let host = crate::workspaces::declared_host();
        let self_file = format!("{}/self/{}__{}.txt", comm_home.trim_end_matches('/'), host, workspace_id);
        env.push(("SOT_COMM_HOME".to_string(), comm_home));
        env.push(("SOT_COMM_SELF_FILE".to_string(), self_file));
    }
    // The `ccb` launcher's own PATH rule, promoted to the daemon: a leg
    // inherits the SERVICE's PATH, which has no `~/.local/bin` (CLAUDE.md's
    // documented gotcha — the same reason [`claude_argv`] full-paths
    // `claude`), so a capsule session found neither `gh`, the comm
    // launchers, nor any user-installed tool a tmux row's login shell
    // sees (found 2026-09-12: the first capsule-row release cut failed
    // its preflight on the system's ancient `gh`). Windows relies on the
    // daemon's own PATH already reaching everything, as `claude_argv`
    // documents.
    #[cfg(not(windows))]
    env.extend(agent_env(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    ));
    env
}

/// `~/.local/bin` on `PATH`, prepended once — the ONE copy of this rule
/// (ADR 0046 decision 4), shared by every Unix agent-launch path: a
/// capsule supervisor spawn ([`capsule_supervisor_env`] above) and `sotd
/// agent-exec` (`main.rs`, execing THIS process into the agent directly)
/// both call it rather than keeping two copies of "prepend once, only
/// when absent." Pure — takes `PATH`/`HOME` explicitly rather than
/// reading `std::env` itself (mirrors [`resolve_claude`]'s own
/// testability rule) — so it is unit-tested without mutating global
/// process state. Returns a single `PATH` entry to add, or nothing when
/// `~/.local/bin` is already on it or there is no `HOME` to derive it
/// from. Windows relies on the daemon's/`claude`'s own `PATH` already
/// reaching everything (`claude_argv`'s doc) — this is never called
/// there.
#[cfg(not(windows))]
pub fn agent_env(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Vec<(String, String)> {
    let Some(home) = home else {
        return Vec::new();
    };
    let local_bin = home.join(".local").join("bin");
    let inherited = path_var.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if inherited.split(':').any(|dir| Path::new(dir) == local_bin) {
        return Vec::new();
    }
    let joined = if inherited.is_empty() {
        local_bin.display().to_string()
    } else {
        format!("{}:{inherited}", local_bin.display())
    };
    vec![("PATH".to_string(), joined)]
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
    /// PATH). A `Terminal` authority admits no fresh `EndRun` anyway
    /// (`supervisor.rs`'s `handle_command` gates `EndRun` on
    /// `Lifecycle::Ready`) — so this sends `stop` instead (admitted
    /// unconditionally, regardless of lifecycle: `SupervisorOp::Stop`'s
    /// own admission has no lifecycle gate) and waits for its confirmed
    /// exit. Without this arm the row was UNENDABLE: `workspace.destroy`
    /// kept reporting `NotEnded` forever, because nothing ever told the
    /// stuck authority to stop.
    ///
    /// `Terminal` alone does NOT prove the leg died (Codex review,
    /// 2026-09-11: watchdog restart-budget exhaustion, a failed
    /// adoption, or a kill/wait failure on the leg itself can all reach
    /// `Terminal` with a leg still running) — this variant is reported
    /// ONLY once the SAME [`runtime::absence_proof`] `Unheld` uses has
    /// independently confirmed no leg holds the voyage either; a leg
    /// still present is reported (`end_run` keeps the row), never
    /// silently orphaned.
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
    /// call disagreed with) AND BOTH halves of decision 33's destroy
    /// proof came back absent: a bounded, non-blocking attempt to take
    /// `supervisor.lock` on the same state dir succeeded — nobody holds
    /// the AUTHORITY over this row (the kernel released the fence the
    /// instant its last holder died — `sot_log::fence`) — AND
    /// [`runtime::leg_absent`] independently proved no LEG holds the
    /// voyage's own `writer.lock` either (`voyage.rs`). No authority AND
    /// no leg is safe to treat as `Removable`, same as `Terminal`.
    /// Fence free but a leg still present is NOT this variant — a leg
    /// with no authority left to end it is reported instead of silently
    /// orphaned (see `end_run`'s own doc). Distinct from `NotEnded`: that
    /// variant means the lane DID answer and refused/failed the request
    /// — a live, responsive holder, never fabricated as ended. Field
    /// defect closed (v0.6.0-rc.12): a supervisor that died out from
    /// under a row (a daemon-pair converge that ended the old build)
    /// left the row permanently `Kept`/unendable, because `query_status`
    /// failing was the ONLY signal this function ever consulted.
    Unheld,
    /// The state directory itself does not exist AND `query_status`'s
    /// connect failure conclusively proves no supervisor answers this
    /// row's lane (`runtime::is_definitely_orphaned` — decision 27's own
    /// "no listener at all" classification, never a mere timeout or a
    /// foreign/undetermined challenge). Distinct from `Unheld`: that
    /// variant proves absence by taking the fence and the writer lock;
    /// neither lock can even be attempted here because both live INSIDE
    /// the missing directory (`fence.rs`, `voyage_root_path`) — so there
    /// is nowhere left for either to exist, let alone be held. This row
    /// has no durable record and no supervisor that could be alive under
    /// this daemon's state root — safe to remove, same as `Unheld`, but
    /// reported under its own name (`orphan_removed`) rather than folded
    /// into "no supervisor held the row", which would misstate that a
    /// row here ever really ran under this daemon.
    Orphaned,
}

/// The daemon's capsule runtime — spawning, watching, querying, and
/// ending a supervisor over `sot_log::supervisor_client`. Platform
/// chosen by exactly TWO forks inside (ADR 0043 decision 22): the
/// capsule executable's name ([`CAPSULE_EXE`]) and the detach mechanism
/// ([`spawn_detached`]'s two twins) — everything else below is
/// byte-identical on both platforms. (A third fork, the adopted leg's
/// exit-status read, existed here before decision 33 deleted the
/// adopted-leg watch entirely — a watchdog now exists only for a
/// `Child` this daemon itself spawned.)
/// This platform's `sot-capsule` sibling file name — kept OUTSIDE `mod
/// runtime` (windows/linux only, see that module's own doc) so the
/// daemon-startup sanity check right below it compiles and runs on every
/// platform `sotd` ships for, macOS included, even though the runtime
/// itself does not yet spawn the binary there. Duplicates `mod runtime`'s
/// own `CAPSULE_EXE` value rather than reaching across the cfg boundary —
/// the two are pinned together by the test below.
#[cfg(windows)]
const CAPSULE_SIBLING_NAME: &str = "sot-capsule.exe";
#[cfg(not(windows))]
const CAPSULE_SIBLING_NAME: &str = "sot-capsule";

/// Whether the `sot-capsule` sibling binary exists next to `daemon_exe`
/// and (on Unix) is executable — ADR 0043 decision 22's sibling-binary
/// contract, checked once at daemon startup (`main.rs`, right after arg
/// parsing, before the socket is bound). `false` here after an in-place
/// upgrade means a pre-0.6 `sot-apply` swapped only `sot`/`sotd` and left
/// the newer `sot-capsule` unstaged (finding 1, v0.6.5 macOS field
/// report): every capsule row then blinks "supervisor lane not
/// answering" forever while the journal claims a start that produced no
/// process. Pure path/metadata check, no `current_exe()` call, so it is
/// unit-testable against a plain temp directory.
pub fn capsule_sibling_present(daemon_exe: &Path) -> bool {
    let sibling = daemon_exe.with_file_name(CAPSULE_SIBLING_NAME);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&sibling)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        sibling.is_file()
    }
}

#[cfg(test)]
mod capsule_sibling_present_tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sot-capsule-sibling-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn present_when_executable_sibling_exists() {
        let dir = scratch_dir("present");
        let daemon = dir.join("sotd");
        let sibling = dir.join(CAPSULE_SIBLING_NAME);
        std::fs::write(&sibling, b"#!/bin/sh\n").expect("write sibling");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        assert!(capsule_sibling_present(&daemon));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn absent_when_sibling_missing() {
        let dir = scratch_dir("absent");
        let daemon = dir.join("sotd");
        assert!(!capsule_sibling_present(&daemon));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn absent_when_sibling_not_executable() {
        let dir = scratch_dir("noexec");
        let daemon = dir.join("sotd");
        let sibling = dir.join(CAPSULE_SIBLING_NAME);
        std::fs::write(&sibling, b"#!/bin/sh\n").expect("write sibling");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(!capsule_sibling_present(&daemon));
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// macOS lane, the one gate this milestone could NOT widen, and exactly
/// why — so the next reader does not mistake its absence for an
/// oversight. Everything below `sot-capsule` needs is already there on
/// Darwin: the `supervise` subcommand is built and shipped in the macOS
/// release archive, `sot_log::supervisor_client` compiles for macOS, the
/// death watch is a kqueue `NOTE_EXIT` knote and the parent-death lease
/// works. The single blocker is [`spawned_identity`] (see its own doc):
/// this daemon must read a FRESHLY SPAWNED supervisor's identity in the
/// same unit that supervisor will later report over the wire, and on
/// macOS that unit is the kernel's `pidversion`, which is readable ONLY
/// out of an audit token — from `mach_task_self()` for oneself, or from
/// a socket peer's `LOCAL_PEERTOKEN`. Neither exists for a child this
/// daemon has only just forked: `task_for_pid` on anyone else is
/// entitlement-gated, and no `proc_pidinfo`/`sysctl` flavor carries
/// `p_idversion`. `spawn_detached_supervisor_with_identity` treats an
/// unreadable identity as a spawn FAILURE (kills and reaps the child),
/// so widening this gate without first ruling on where macOS gets that
/// identity would make every capsule row on a Mac die at spawn —
/// precisely the silent half-a-mechanism this lane exists to prevent.
/// The two candidate shapes, both lifecycle rulings rather than code
/// this lane may choose between: take the identity from the lane's own
/// first answered `query_status` (`probe` already returns it) under a
/// bounded post-spawn wait, or let `SupervisorIdentity` carry a
/// "pending, pinned by a kqueue handle" state that the first observation
/// resolves. Until one is ratified, a macOS host's `workspace.create`
/// keeps today's tmux path, unchanged and working — a row that runs,
/// not a row that vanishes.
#[cfg(any(windows, target_os = "linux"))]
mod runtime {
    use super::{
        agent_argv, capsule_supervisor_env, first_leg_without_continue, mode_flag, ActivationIntent,
        StartMode, FOREIGN_PHASE, LANE_CONCURRENCY, MAX_RESTARTS_PER_WINDOW, NESTING_ENV_VARS_TO_SCRUB,
        NEVER_STARTED_PHASE, RESTART_BACKOFFS, RESTART_WINDOW, UNREACHABLE_PHASE,
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
    /// over). Linux's own escape is not a job flag but a transient user
    /// scope (`systemd-run --user --scope`) — see the Linux twin of
    /// [`spawn_detached`] below for that platform's attempt-then-contained
    /// shape.
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
    /// `not(windows)`, not `target_os = "linux"`: the extensionless name
    /// is a Unix fact, not a Linux one, and the release archive stages it
    /// next to `sotd` on the macOS leg exactly as it does on the Linux
    /// one — so this arm is already correct for the day `mod runtime`'s
    /// own gate widens, and gating it narrower would only make that day's
    /// diff bigger without naming an invariant of its own.
    #[cfg(not(windows))]
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
    /// escape attempt, never guessed here; on Linux that SAME attempt
    /// also decides `scoped`, `build`'s second parameter — whether the
    /// head this closure constructs is `systemd-run --user --scope … --
    /// <sot-capsule>` (the escape) or `<sot-capsule>` directly (bare) —
    /// so the shared tail (`supervise`, `state_dir`, mode, survival,
    /// `--assume-no-rollback-target`, argv, cwd, stdio, env) is written
    /// ONCE regardless of which head it lands on (Codex review deletion:
    /// the earlier design built the bare command first and REWROTE it
    /// into the scoped one via `Command::as_std()` accessors afterward —
    /// gone; this closure just branches on `scoped` up front instead, so
    /// there is no second log-file open, no workspace id reconstructed
    /// from a path, and no future `env_clear` call that replay could ever
    /// silently lose). `scoped` is always `false` on Windows (no scope
    /// concept there) — only how the head is built differs; only how the
    /// result is actually detached — [`spawn_detached`], the second of
    /// decision 22's three forks — differs per platform.
    /// `--assume-no-rollback-target` is mandatory: `sot_log::supervisor::supervise`
    /// itself refuses (exit 69) without it pre-U4. The nesting env vars
    /// are scrubbed and `SOT_COMM_NAME` exported (Codex review finding
    /// 9) — the same contract `boot_wrapper_command`'s tmux path already
    /// gives every autostart workspace.
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
    ///
    /// A second refusal right after it: [`super::state_root_inside_project`]
    /// against `cwd` (every caller passes its `project_root` here) —
    /// unlike the root check above this one DOES need `state_dir`, since
    /// nesting is a property of THIS row, not of the machine.
    fn spawn_detached_supervisor(
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
        agent_name: &str,
        workspace_id: &str,
        slug: &str,
        agent_kind: &str,
        account: &str,
    ) -> std::io::Result<Child> {
        super::qualified_state_root().map_err(|msg| std::io::Error::new(ErrorKind::Unsupported, msg))?;
        if super::state_root_inside_project(state_dir, cwd) {
            return Err(std::io::Error::new(
                ErrorKind::Unsupported,
                format!(
                    "state directory {state_dir:?} lies inside the project root {cwd:?}: this \
                     workspace's own file watcher would hold directory handles under it, and on \
                     Windows an open handle blocks the renames capsule publication depends on \
                     (point this machine's state root, {}, outside the project tree)",
                    super::STATE_ROOT_HINT
                ),
            ));
        }
        // Accounts brief: the SAME refusal `workspace.create` already
        // ran once, re-run here so a folder that vanished BETWEEN create
        // and this spawn (or a watchdog restart) refuses loudly instead
        // of silently starting on the default directory. `ErrorKind::
        // Unsupported` (never retried by the watchdog -- see its own
        // "no retry, marking terminal" arm) matches `qualified_state_root`'s
        // own classification just above: both are operator-fixable, not
        // transient.
        let account_env_extra = match crate::accounts::account_home() {
            Some(home) => {
                let extra = crate::accounts::account_env(agent_kind, account, &home)
                    .map_err(|msg| std::io::Error::new(ErrorKind::Unsupported, msg))?;
                // Accounts brief: link the shared entries now that account_env
                // has proved the folder exists (sharing ruling: accounts.rs
                // module doc). Same refusal shape as account_env's own error
                // just above; ensure_account_links itself no-ops for an empty
                // account or "default", so no guard is needed here.
                crate::accounts::ensure_account_links(&home, account)
                    .map_err(|msg| std::io::Error::new(ErrorKind::Unsupported, msg))?;
                extra
            }
            None if account.is_empty() || account == "default" => Vec::new(),
            None => {
                return Err(std::io::Error::new(
                    ErrorKind::Unsupported,
                    format!("no home directory to resolve account {account:?} against"),
                ));
            }
        };
        let build = |survival: &str, scoped: bool| -> Command {
            let mut cmd = if scoped {
                let mut c = Command::new("systemd-run");
                c.arg("--user")
                    .arg("--scope")
                    .arg("--quiet")
                    .arg("--collect")
                    .arg("--description")
                    .arg(format!("sot-capsule {workspace_id}"))
                    .arg("--")
                    .arg(sot_capsule_exe);
                c
            } else {
                Command::new(sot_capsule_exe)
            };
            cmd.arg("supervise")
                .arg(state_dir)
                .arg(mode_flag(mode))
                .arg("--survival")
                .arg(survival)
                .arg("--assume-no-rollback-target")
                .args(first_leg_without_continue(mode))
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
            for (k, v) in &account_env_extra {
                cmd.env(k, v);
            }
            cmd
        };
        spawn_detached(build, state_dir, workspace_id)
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
    /// Any OTHER spawn error propagates unchanged. `scoped` has no
    /// Windows meaning (no scope concept there) — `build` is always
    /// called with `false`; `workspace_id` is the Linux twin's own
    /// concern (its scoped `--description` string), unused here but
    /// shared across the signature both platforms call through.
    #[cfg(windows)]
    fn spawn_detached(
        build: impl Fn(&str, bool) -> Command,
        state_dir: &Path,
        workspace_id: &str,
    ) -> std::io::Result<Child> {
        let _ = workspace_id;
        let mut cmd = build("normal", false);
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                tracing::warn!(
                    state_dir = ?state_dir,
                    "capsule supervisor: this daemon's own job forbids breakaway; the supervisor \
                     is contained in it and will not outlive it (ADR 0043 decision 32)"
                );
                let mut cmd = build("degraded", false);
                cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
                cmd.spawn()
            }
            other => other,
        }
    }

    /// Bounds a wedged user bus so a launch never hangs.
    ///
    /// This constant and the three items after it ([`STDERR_DRAIN_BOUND`],
    /// [`drain_stderr_bounded`], [`user_scope_available`]) are correctly
    /// Linux-only and stay that way: every one of them exists to bound a
    /// `systemd-run --user --scope` probe, and the thing they are
    /// escaping -- a `KillMode=control-group` user service whose cgroup
    /// reaps everything the daemon leaves behind -- has no Darwin
    /// counterpart. launchd's own reaping is by PROCESS GROUP, and
    /// `pre_exec(setsid)` in the shared spawn below already leaves it;
    /// so on macOS the bare detached spawn IS the normal-survival case
    /// and there is nothing to probe, no degraded fallback to fall to,
    /// and no bus to wedge. A macOS `spawn_detached` is therefore the
    /// Linux one with the whole probe deleted, not a port of it -- which
    /// is why these four have no `cfg(unix)` future and are left alone.
    #[cfg(target_os = "linux")]
    const USER_SCOPE_PROBE_BOUND: Duration = Duration::from_secs(5);

    /// Bounds draining a probe child's stderr AFTER it has already
    /// exited (Codex review, reproduced: extracted code took 7 s when a
    /// wrapper exited but left stderr inherited by a still-running
    /// GRANDCHILD — `read_to_string` blocks until EVERY holder of the
    /// pipe's write end closes it, not just the immediate child whose own
    /// exit [`USER_SCOPE_PROBE_BOUND`]'s loop already observed). A
    /// separate bound from that one: the process-exit wait and the
    /// stderr drain can each hang for their own, independent reason.
    #[cfg(target_os = "linux")]
    const STDERR_DRAIN_BOUND: Duration = Duration::from_secs(1);

    /// Drains `pipe` to EOF or [`STDERR_DRAIN_BOUND`], whichever comes
    /// first, by reading it on a throwaway thread and joining that with a
    /// bounded `recv_timeout` — the only way to cap a blocking
    /// `read_to_string` without relying on the pipe's own non-blocking
    /// mode. Past the bound the read is simply abandoned (its thread
    /// leaks, but harmlessly: nothing else waits on it, and the pipe's
    /// own fd closes when the thread eventually finishes or the process
    /// exits) — the caller gets whatever text arrived in time, which for
    /// a probe's own diagnostic stderr is "none" in the timeout case,
    /// never a hang.
    #[cfg(target_os = "linux")]
    fn drain_stderr_bounded(mut pipe: impl std::io::Read + Send + 'static, bound: Duration) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            let _ = tx.send(buf);
        });
        rx.recv_timeout(bound).unwrap_or_default()
    }

    /// The escape [`spawn_detached`]'s Linux twin attempts before falling
    /// back to a contained, degraded spawn (ADR 0043 decision 32): does a
    /// reachable `systemd --user` manager grant a transient scope at all?
    /// `systemd-run` fails before it ever execs the payload when it
    /// cannot, indistinguishable from the payload's own instant death, so
    /// this is a probe of the CAPABILITY (`/bin/true`), never a cache of
    /// a past answer — a user manager can appear or vanish between
    /// launches, and a launch is rare next to a whole supervisor's
    /// lifetime. `Err`'s message is the probe's own stderr, verbatim
    /// where there is any, drained under its own separate bound
    /// ([`drain_stderr_bounded`]).
    #[cfg(target_os = "linux")]
    fn user_scope_available() -> std::io::Result<()> {
        let mut command = std::process::Command::new("systemd-run");
        command
            .args(["--user", "--scope", "--quiet", "--", "/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let deadline = Instant::now() + USER_SCOPE_PROBE_BOUND;
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    format!("systemd-run --user --scope did not answer within {USER_SCOPE_PROBE_BOUND:?}"),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if status.success() {
            return Ok(());
        }
        let stderr = child
            .stderr
            .take()
            .map(|pipe| drain_stderr_bounded(pipe, STDERR_DRAIN_BOUND))
            .unwrap_or_default();
        let stderr = stderr.trim();
        Err(std::io::Error::other(if stderr.is_empty() {
            format!("systemd-run --user --scope exited {status} with no stderr")
        } else {
            stderr.to_string()
        }))
    }

    /// Linux: attempts the platform's escape from the daemon's own kill
    /// domain — a transient user scope, probed once per launch by
    /// [`user_scope_available`]. A granted probe launches `build("normal",
    /// true)` — the SAME closure that would have built the bare command,
    /// just pointed at the `systemd-run … --scope` head instead (Codex
    /// review deletion: no second construction, no
    /// `Command::as_std()` replay of an already-built command); a denied
    /// probe launches `build("degraded", false)`, one warn line naming
    /// `workspace_id`, `state_dir` and the denial (ADR 0043 decision 32).
    /// `pre_exec(setsid)` runs on EITHER head: a `systemd-run --scope`
    /// child execs the supervisor in place (verified on systemd 249), so
    /// the session id set here before `systemd-run`'s OWN exec survives
    /// into the supervisor unchanged, same as the bare spawn. A spawn
    /// error after a GRANTED probe propagates here unchanged — never a
    /// retry into the bare branch.
    ///
    /// macOS, when `mod runtime`'s gate widens: this function's body
    /// minus the probe -- `build("normal", false)` plus the same
    /// `pre_exec(setsid)`, and `"normal"` is the honest survival value
    /// there, not a concession. launchd reaps a stopped job by killing
    /// its process group unless `AbandonProcessGroup` is set, and
    /// `setsid` puts the supervisor in a brand-new session and process
    /// group before the exec, so it is already outside that domain.
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
    fn spawn_detached(
        build: impl Fn(&str, bool) -> Command,
        state_dir: &Path,
        workspace_id: &str,
    ) -> std::io::Result<Child> {
        let mut cmd = match user_scope_available() {
            Ok(()) => {
                tracing::info!(
                    workspace_id,
                    "capsule supervisor: launching in a transient user scope (ADR 0043 decision 32)"
                );
                build("normal", true)
            }
            Err(e) => {
                tracing::warn!(
                    workspace_id,
                    state_dir = ?state_dir,
                    error = %e,
                    "capsule supervisor: no transient user scope available; this supervisor \
                     shares the daemon's kill domain (ADR 0043 decision 32)"
                );
                build("degraded", false)
            }
        };
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

    /// One status round trip: wire string plus the identity-carrying `Observation` it implies. BLOCKING.
    pub fn probe(state_dir: &Path) -> (&'static str, crate::workspaces::Observation) {
        use crate::workspaces::{Observation, SupervisorIdentity};
        if let Some(phase) =
            super::phase_for_missing_pointer(sot_log::pointer::pointer_path(state_dir).is_file())
        {
            return (phase, Observation::Stopped);
        }
        match sot_log::supervisor_client::query_status(state_dir) {
            // The retained process handle (the second element) is not
            // this caller's concern -- a one-shot phase probe, dropped
            // (closing the handle) the instant this returns.
            Ok((report, _process)) => {
                let observation = Observation::Phase {
                    phase: super::local_phase(report.phase),
                    supervisor: SupervisorIdentity { pid: report.pid, created: report.created },
                    voyage: report.voyage.as_deref().and_then(|v| v.parse().ok()),
                };
                (super::phase_str(report.phase), observation)
            }
            // Typed, not text (ADR 0030 §8 decision 31c): `VersionSkew`
            // is the ONLY `sot_log::Error` variant `query_status` returns
            // for a lane that answered but refused this build. Every
            // other error -- a malformed reply, a timeout, connect
            // refused -- stays `UNREACHABLE_PHASE`.
            Err(sot_log::Error::VersionSkew) => {
                note_version_skew(state_dir);
                (super::FOREIGN_PHASE, Observation::Foreign)
            }
            Err(e) => {
                tracing::debug!(state_dir = ?state_dir, error = %e, "capsule workspace: supervisor lane unreachable");
                (super::UNREACHABLE_PHASE, Observation::Failed)
            }
        }
    }

    /// [`probe`]'s wire string alone, for a caller with no row to observe into (`watchdog_may_act`).
    pub fn phase_of(state_dir: &Path) -> &'static str {
        probe(state_dir).0
    }

    /// Adopts the observation's supervisor as this row's epoch if it differs, then feeds it (guarded callers only).
    fn observe_with_adoption(ws: &crate::workspaces::Workspace, observation: crate::workspaces::Observation) {
        if let crate::workspaces::Observation::Phase { supervisor, .. } = &observation {
            if ws.current_supervisor() != Some(*supervisor) {
                ws.begin_supervisor_epoch(*supervisor);
            }
        }
        super::observer::observe(ws, observation);
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
    /// `root_canonicalized` (Fable review, safety): true only when the
    /// caller (`destroy_capsule_workspace`) successfully canonicalized
    /// the STATE ROOT before building `state_dir` — see that call site's
    /// own doc for why an un-canonicalized root makes the orphan proof
    /// below unsafe (a symlinked root can make this call dial a
    /// different lane address than a live supervisor, spawned while its
    /// own `state_dir` existed, actually bound). `false` disables the
    /// orphan proof outright and keeps today's unconditional
    /// `state_dir_missing` refusal, regardless of what `query_status`'s
    /// connect returned.
    pub fn end_run(
        state_dir: &Path,
        reason: &str,
        root_canonicalized: bool,
    ) -> std::io::Result<super::EndRunOutcome> {
        use super::EndRunOutcome as R;
        use sot_log::supervisor_client::EndRunOutcome as O;
        use sot_log::wire::SupervisorPhase;

        let status = match sot_log::supervisor_client::query_status(state_dir) {
            Ok((status, _process)) => status,
            Err(e) => {
                // Recoverability (ADR 0043 decision 33): a row is
                // removed only after a confirmed end or a PROVEN absence
                // of BOTH authority (fence) and leg (writer.lock) — a
                // missing state dir proves neither BY ITSELF, and fence
                // creation there fails outright (`CREATE_NEW` needs the
                // directory), so `absence_proof` below cannot even be
                // attempted. It is checked and reported FIRST, distinct
                // from every other "lane unreachable" case — never a
                // licence to recreate anything (`leg_absent`'s own
                // caller, `destroy_capsule_workspace`, never does).
                //
                // STAT STRICTLY (Fable review): `fs::metadata`, not
                // `Path::is_dir()` — that helper swallows every error
                // (permission denied, a stale network-mount handle, any
                // other I/O failure) into a bare `false`, which used to
                // read identically to "genuinely absent". Only
                // `ErrorKind::NotFound` means missing; anything else
                // keeps refusing with ITS OWN detail, never folded into
                // `state_dir_missing` and never treated as grounds for
                // the orphan proof — a network mount hiccup must never
                // remove a row whose record exists.
                match std::fs::metadata(state_dir) {
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => {
                        return Err(std::io::Error::other(
                            "state dir path exists but is not a directory",
                        ));
                    }
                    Err(stat_err) if stat_err.kind() == std::io::ErrorKind::NotFound => {
                        // A missing directory is not automatically a dead
                        // end, though: `supervisor.lock` and every
                        // voyage's `writer.lock` live INSIDE `state_dir`
                        // (`fence.rs`, `voyage_root_path`), so if the
                        // directory is gone neither lock can possibly be
                        // held BY THIS ROW anywhere else — the one thing
                        // that could still be alive is a supervisor
                        // process answering THIS row's lane, which is
                        // addressed by a hash of the CANONICAL path
                        // (`state_dir_hash`) in the runtime dir, not a
                        // file under `state_dir` — so it stays reachable
                        // regardless of whether the directory exists,
                        // PROVIDED the caller resolved that canonical
                        // form (`root_canonicalized`). `query_status`'s
                        // own connect already tested exactly that, and
                        // `e`'s shape says whether it was conclusive:
                        // `is_definitely_orphaned` accepts only decision
                        // 27's own "no listener at all" classification
                        // (`TransportError::is_endpoint_absent`: connect
                        // refused or nothing there), never a timeout or a
                        // foreign/undetermined challenge, either of which
                        // means SOMETHING answered and the row must keep
                        // refusing. Proven absent -> `Orphaned` (no
                        // durable record, no reachable authority, no
                        // possible lock holder); otherwise the original
                        // unchanged refusal.
                        return if root_canonicalized && is_definitely_orphaned(&e) {
                            Ok(R::Orphaned)
                        } else {
                            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "state_dir_missing"))
                        };
                    }
                    Err(stat_err) => {
                        return Err(std::io::Error::other(format!(
                            "state dir stat failed (not proof of absence): {stat_err}"
                        )));
                    }
                }
                // Unreachable is not the same claim as "not running" —
                // the caller must keep refusing a live-but-unresponsive
                // lane, never fabricate "ended" for one (see
                // `destroy_capsule_workspace`'s own doc). [`absence_proof`]
                // settles it independently of this IPC round trip; a
                // fence-stage failure keeps the ORIGINAL "lane
                // unreachable" text (`e`), unchanged.
                return match absence_proof(state_dir) {
                    Ok(true) => Ok(R::Unheld),
                    Ok(false) => Err(std::io::Error::other("a leg is running with no authority")),
                    Err(NotProven::LegCheckFailed(detail)) => Err(std::io::Error::other(detail)),
                    Err(NotProven::FenceUnavailable) => Err(std::io::Error::other(e.to_string())),
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
                // No fresh `EndRun` would ever be admitted here
                // (`Lifecycle::Terminal` isn't `Ready`), so the authority
                // is stopped first — but `Terminal` alone does NOT prove
                // the leg died (Codex review, 2026-09-11: it is also
                // reached by the watchdog's own exhausted restart budget,
                // by a failed adoption, or by a kill/wait failure on the
                // leg itself, none of which confirm the leg is gone).
                // This is therefore NOT a confirmed end on its own — the
                // SAME independent [`absence_proof`] the unreachable arm
                // above uses decides whether the row is actually Removable.
                stop_and_warn(state_dir, "the authority was terminal before this call reached it");
                return match absence_proof(state_dir) {
                    Ok(true) => Ok(R::Terminal),
                    Ok(false) => {
                        Err(std::io::Error::other("a leg is running with no authority (terminal)"))
                    }
                    Err(NotProven::LegCheckFailed(detail)) => Err(std::io::Error::other(detail)),
                    Err(NotProven::FenceUnavailable) => Err(std::io::Error::other(
                        "the authority did not release its fence after being stopped",
                    )),
                };
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

    /// Why [`absence_proof`] could not prove either outcome — distinct
    /// variants only so each of [`end_run`]'s TWO call sites (the
    /// unreachable-lane arm and the `Terminal` arm) can keep its own
    /// honest wording: a fence-stage failure means something else may
    /// still hold the AUTHORITY (each caller already knows its own
    /// reason to say there), while a leg-check failure carries
    /// [`leg_absent`]'s own message forward instead of being discarded
    /// for a caller-supplied one (Codex review, 2026-09-11 — the old code
    /// replaced `leg_absent`'s own error with the outer `query_status`
    /// error, losing the actual reason absence wasn't proven).
    enum NotProven {
        FenceUnavailable,
        LegCheckFailed(String),
    }

    /// ADR 0043 decision 33's own absence proof, shared by every
    /// [`end_run`] arm that reaches a state with no live leg to end and
    /// so gets no live `end_run` round trip over the lane: the
    /// unreachable-lane arm (no supervisor answers at all) and the
    /// `Terminal` arm (a lane that DID answer, but whose authority admits
    /// no fresh `EndRun` and was just told to stop — Codex review,
    /// 2026-09-11: `Terminal` is also reached by watchdog restart-budget
    /// exhaustion and by a failed kill/wait on the leg itself, neither of
    /// which proves the leg died, so `Terminal` alone is not a confirmed
    /// end). A bounded, non-blocking attempt to take `supervisor.lock`
    /// settles the AUTHORITY half — acquirable means no supervisor holds
    /// this row; released immediately (this call only OBSERVES, it must
    /// never itself become the holder) — and [`leg_absent`] independently
    /// settles the LEG half. `Ok(true)`: neither is held — nothing is
    /// running here. `Ok(false)`: the leg is running with no authority
    /// left to end it — reported, never silently orphaned. `Err`:
    /// absence is NOT proven either way, so the caller must keep the row
    /// rather than guess.
    fn absence_proof(state_dir: &Path) -> Result<bool, NotProven> {
        let _fence =
            sot_log::fence::lock_supervisor(state_dir).map_err(|_| NotProven::FenceUnavailable)?;
        leg_absent(state_dir).map_err(NotProven::LegCheckFailed)
        // `_fence` drops here, right after `leg_absent`'s own single
        // observation -- observe only, never become the holder.
    }

    /// `end_run`'s missing-state-dir proof: whether `query_status`'s own
    /// connect failure `e` is conclusive that NOTHING answers this row's
    /// lane, reusing decision 27's own classification
    /// (`TransportError::is_endpoint_absent`: `ECONNREFUSED`/`ENOENT`/
    /// their Windows equivalents, returned on the FIRST attempt — never
    /// the outcome of a retried-but-still-busy endpoint, which means a
    /// listener exists). Only the connect step itself produces a
    /// `sot_log::Error::Transport` — a challenge that answered `Foreign`/
    /// `Undetermined`/`VersionSkew`, or a reply-stage failure, is a
    /// DIFFERENT `Error` variant and correctly falls through to `false`
    /// here, because each of those means something DID answer. A `true`
    /// result, together with the caller's own `!state_dir.is_dir()`
    /// check, is the missing directory's own version of
    /// [`absence_proof`]: nowhere left for `supervisor.lock` or any
    /// voyage's `writer.lock` to exist (both live under `state_dir`), and
    /// no supervisor reachable at the one address that does not depend on
    /// the directory existing.
    fn is_definitely_orphaned(e: &sot_log::Error) -> bool {
        matches!(e, sot_log::Error::Transport(te) if te.is_endpoint_absent())
    }

    #[cfg(test)]
    mod is_definitely_orphaned_tests {
        use super::*;
        use sot_log::transport::TransportError;

        fn io_transport(kind: std::io::ErrorKind) -> sot_log::Error {
            sot_log::Error::Transport(TransportError::Io {
                op: "connect",
                source: std::io::Error::new(kind, "test"),
            })
        }

        #[test]
        fn no_listener_at_all_is_orphaned() {
            // Decision 27's own "absent" shapes -- what `query_status`'s
            // connect step actually returns on the first attempt when
            // nothing is bound at this row's lane address at all.
            assert!(is_definitely_orphaned(&io_transport(std::io::ErrorKind::NotFound)));
            assert!(is_definitely_orphaned(&io_transport(std::io::ErrorKind::ConnectionRefused)));
        }

        #[test]
        fn a_reachable_but_refusing_lane_is_never_orphaned() {
            // The lane DID answer (a foreign build, a version skew, or
            // some other live-but-unresponsive shape) -- none of these
            // may ever be folded into "nothing is running here".
            assert!(!is_definitely_orphaned(&sot_log::Error::VersionSkew));
            assert!(!is_definitely_orphaned(&sot_log::Error::State(
                "supervisor lane challenge: foreign".to_string()
            )));
        }

        #[test]
        fn a_busy_or_timed_out_endpoint_is_never_orphaned() {
            // Decision 27: a busy endpoint is retried WITHIN the connect
            // bound and only surfaces as `Err` once genuinely ambiguous
            // (e.g. a timeout) -- that must stay unproven, never treated
            // as absent.
            assert!(!is_definitely_orphaned(&io_transport(std::io::ErrorKind::TimedOut)));
            assert!(!is_definitely_orphaned(&io_transport(std::io::ErrorKind::PermissionDenied)));
        }
    }

    /// Whether a `lock_writer` failure is genuine contention (its OWN
    /// bounded-retry exhaustion, `fsutil.rs`) rather than some OTHER
    /// refusal that happens to share `Error::State`'s shape — Windows:
    /// `open_lock_file`'s reparse-point refusal is the one other producer
    /// of `Error::State` on this exact call (Codex review, 2026-09-11:
    /// conflating the two used to report a live leg for what was actually
    /// a security refusal). Matched on `lock_writer`'s own fixed message
    /// prefix rather than a new `sot_log::Error` variant — that ONE
    /// crate-wide `State` variant already serves dozens of unrelated call
    /// sites (see its own doc), so a new variant there is a much wider
    /// change than this one call site needs.
    pub fn is_lock_contention(detail: &str) -> bool {
        detail.starts_with("lock held by another process:")
    }

    /// The LEG half of decision 33's destroy proof — [`absence_proof`]
    /// calls this only once the AUTHORITY half (the supervisor fence) is
    /// already proven free, never on its own. Reads the published
    /// pointer to find which voyage the row last bound, then takes the
    /// SAME bounded, non-blocking primitive `open_for_writing` itself
    /// uses on that voyage's `writer.lock` (`voyage.rs`) — held by a live
    /// leg, never by the authority — and releases it at once (this call
    /// only OBSERVES, it must never become the holder). `Ok(true)`: the
    /// lock was acquirable — no leg holds this voyage. `Ok(false)`: the
    /// lock is held — a leg lives with no authority left to end it.
    /// `Err`: the pointer itself is not a valid voyage id, or the lock
    /// attempt failed for a reason OTHER than contention — either way,
    /// absence is NOT proven, so the caller must keep the row rather than
    /// guess.
    pub fn leg_absent(state_dir: &Path) -> Result<bool, String> {
        let voyage = match sot_log::pointer::validate(state_dir) {
            sot_log::pointer::PointerState::Valid(id) => id,
            other => return Err(format!("voyage pointer is not valid: {other:?}")),
        };
        let root = sot_log::supervisor::voyage_root_path(state_dir, &voyage);
        match sot_log::lock_writer(&root.join("writer.lock")) {
            Ok(lock) => {
                drop(lock); // observe only -- never become the holder
                Ok(true)
            }
            Err(sot_log::Error::State(detail)) if is_lock_contention(&detail) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
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
    /// itself then runs entirely in the background.
    ///
    /// ADR 0043 decision 33: no claim to release any more — the CALLER
    /// holds this workspace's row guard (`Workspaces::capsule_guard`) for
    /// the whole spawn attempt (`start_supervisor`'s own doc), so nothing
    /// here needs to signal "the launch is no longer in flight" the way
    /// the old `starting` claim did.
    ///
    /// Order (supervisor-epoch ruling): spawn, SETTLE, adopt, install
    /// the watchdog — the settle moved inward by one frame from
    /// [`start_supervisor`], where it used to sit. Same 2s bound, same
    /// caller's guard, same total latency; what changes is that the
    /// window between spawn and first answered status now contains no
    /// watchdog at all, so no terminal mark and no second spawn can
    /// land inside it. The phase this settles to is what the caller
    /// reports.
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
    ) -> std::io::Result<&'static str> {
        // Accounts brief: resolved from the registry HERE, the one place
        // every spawn path (create, resume, the watchdog's own restart
        // below) already converges with both `workspace_id` and
        // `workspaces` in hand -- rather than threading two more scalars
        // through every caller up the chain (`start_supervisor`,
        // `resume_locked`, `ensure_started`, …), which never otherwise
        // need to know an agent's KIND, only its already-resolved argv.
        // `unwrap_or_default` (kind "", account "") on a row gone by now
        // degrades to `account_env`'s own empty-account no-op below --
        // never worse than the row simply not existing.
        let (agent_kind, account) = workspaces
            .resolve(Some(&workspace_id))
            .map(|ws| (ws.agent(), ws.account.clone()))
            .unwrap_or_default();
        let child = spawn_detached_supervisor(
            sot_capsule_exe, state_dir, mode, agent_argv, cwd, agent_name, &workspace_id, &slug, &agent_kind, &account,
        )?;
        // The supervisor authors its own identity; this daemon only
        // LEARNS it, here, from the first status the settle draws out.
        // A settle that yields no `Phase` leaves the row unclaimed --
        // best-effort by design, since the background observer adopts
        // whenever the lane does answer.
        let (phase, observation) = settle_after_spawn(state_dir, &workspace_id);
        let identity = identity_of(&observation);
        if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
            observe_with_adoption(&ws, observation);
        }
        install_watchdog(
            workspace_id,
            sot_capsule_exe.to_path_buf(),
            state_dir.to_path_buf(),
            agent_argv.to_vec(),
            cwd.to_path_buf(),
            agent_name.to_string(),
            slug,
            agent_kind,
            account,
            child,
            identity,
            workspaces,
        );
        Ok(phase)
    }

    /// The spawn path shared by `workspace.create` (mode `Start` always — a
    /// brand new workspace has no state dir yet), `pty.open`'s
    /// start-on-attach ([`ensure_started`], mode picked by
    /// [`start_mode_for_phase`]), and `resume_all` (via [`resume_locked`]):
    /// locate `sot-capsule.exe` and spawn-and-watch it. ADR 0042 L1a Codex
    /// review finding 1's
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
    /// ADR 0043 decision 33: the CALLER holds this workspace's row guard
    /// (`Workspaces::capsule_guard`) for this whole call — every one does:
    /// `ensure_started` and `resume_if_absent`/`resume_locked` take it at
    /// their own entry, `workspace.create` and `resume_all` take it
    /// around their own call site (`handlers.rs`, this module's
    /// `resume_all`). With the guard already held by every caller, at
    /// most one spawn attempt per row can ever be in flight.
    ///
    /// Codex review (2026-09-11): establishes lane REACHABILITY before
    /// returning, not merely a successful spawn — [`settle_after_spawn`],
    /// still under the caller's own guard, now performed one frame
    /// inward by [`spawn_and_watch`] and simply handed back here. Every
    /// spawner converges on this ONE wait: fresh attach
    /// (`ensure_started`'s Start arm), a resume (`resume_locked`),
    /// `workspace.create`, and `resume_all` all call this function and
    /// get it for free. The watchdog's own
    /// restart is the one spawner that does NOT — it never installs a
    /// SECOND watchdog on top of its own loop, so it calls
    /// [`spawn_detached_supervisor`] directly and then
    /// [`settle_after_spawn`] itself, the same shared wait.
    pub fn start_supervisor(
        state_root: &Path,
        workspace_id: &str,
        mode: StartMode,
        agent_argv: &[String],
        project_root: &Path,
        agent_name: &str,
        slug: &str,
        workspaces: Workspaces,
    ) -> Result<&'static str, String> {
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let exe = match sot_capsule_exe() {
            Ok(exe) => exe,
            Err(e) => return Err(format!("could not locate sot-capsule.exe next to this daemon: {e}")),
        };
        spawn_and_watch(
            &exe,
            &state_dir,
            mode,
            agent_argv,
            project_root,
            agent_name,
            workspace_id.to_string(),
            slug.to_string(),
            workspaces.clone(),
        )
        .map_err(|e| format!("capsule supervisor spawn failed: {e}"))
    }

    /// Bound for [`settle_after_spawn`] — the ONE deadline every spawn
    /// path shares (Codex review, 2026-09-11): fresh attach, boot resume,
    /// create, and the watchdog's own restart all wait this long, no
    /// more and no less, for a freshly spawned authority to become
    /// observable before the row's guard (held by every one of them for
    /// this whole wait) is released.
    const SPAWN_SETTLE_DEADLINE: Duration = Duration::from_secs(2);

    /// Waits under the caller's guard for a spawn to settle, polling until [`SPAWN_SETTLE_DEADLINE`] (timeout WARNS). BLOCKING.
    fn settle_after_spawn(state_dir: &Path, workspace_id: &str) -> (&'static str, crate::workspaces::Observation) {
        let starting_phase = super::phase_str(sot_log::wire::SupervisorPhase::Starting);
        let deadline = Instant::now() + SPAWN_SETTLE_DEADLINE;
        loop {
            let (phase, observation) = probe(state_dir);
            if phase != UNREACHABLE_PHASE && phase != starting_phase {
                return (phase, observation);
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    workspace_id = %workspace_id, phase, deadline = ?SPAWN_SETTLE_DEADLINE,
                    "capsule workspace: lane did not settle within the post-spawn deadline"
                );
                return (phase, observation);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether a capsule workspace's supervisor needs starting given an
    /// ALREADY-PROBED `phase` — no I/O itself (switch-latency Phase 1:
    /// the healthy already-running path used to call [`phase_of`] a
    /// SECOND time here; reusing an already-fetched phase string costs
    /// nothing beyond what an already-answered lane already told the
    /// caller). [`NEVER_STARTED_PHASE`] (no published pointer) means
    /// `Start`; [`UNREACHABLE_PHASE`] means `Resume`; any answered
    /// lifecycle phase means a supervisor is already up — `None`.
    fn start_mode_for_phase(phase: &str) -> Option<StartMode> {
        match phase {
            NEVER_STARTED_PHASE => Some(StartMode::Start),
            UNREACHABLE_PHASE => Some(StartMode::Resume),
            _ => None,
        }
    }

    /// Invariant (Codex review round 6/7 BLOCKERs): EVERY decision point
    /// -- the initial probe AND the phase a spawn THIS call itself just
    /// made settles at -- acts ONLY on a genuinely RESTING phase:
    /// `ready`, `ended_no_respawn`, `terminal`, never-started
    /// (`stopped`), `foreign` (a peer identity check already resolved
    /// it, same as before this lane), or `unreachable` with NO live
    /// watchdog (nothing else will ever act on it, so this caller must).
    /// Every OTHER phase — `starting`, `ending`, or `unreachable` while a
    /// watchdog owns it — is TRANSIENT: the row is mid-flight to
    /// somewhere else, and deciding from a snapshot of it is exactly the
    /// bug this closes (round 6: a Selection reading a fleeting
    /// `starting` after the watchdog's own settle gave up; round 7: the
    /// SAME snapshot read straight from this call's OWN spawn, with no
    /// watchdog involved at all — an adopted row's dead supervisor,
    /// `--resume`d fresh, can report `starting` through its own recovery
    /// for many seconds). `ensure_started`'s own loop is what waits out
    /// a transient phase; this function only tells the two apart.
    fn is_resting_phase(phase: &str, watchdog_owns_it: bool) -> bool {
        phase == NEVER_STARTED_PHASE
            || phase == FOREIGN_PHASE
            || phase == UNREACHABLE_PHASE && !watchdog_owns_it
            || phase == super::phase_str(sot_log::wire::SupervisorPhase::Ready)
            || phase == super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn)
            || phase == super::phase_str(sot_log::wire::SupervisorPhase::Terminal)
    }

    #[cfg(test)]
    mod is_resting_phase_tests {
        use super::*;

        #[test]
        fn resting_phases_never_wait_regardless_of_watchdog() {
            for phase in [NEVER_STARTED_PHASE, FOREIGN_PHASE, "ready", "ended_no_respawn", "terminal"] {
                assert!(is_resting_phase(phase, true), "{phase} must be resting with a watchdog");
                assert!(is_resting_phase(phase, false), "{phase} must be resting with no watchdog");
            }
        }

        #[test]
        fn unreachable_rests_only_with_no_watchdog() {
            assert!(is_resting_phase(UNREACHABLE_PHASE, false), "nobody else will ever act -- this caller must");
            assert!(!is_resting_phase(UNREACHABLE_PHASE, true), "a live watchdog owns the restart -- wait for it");
        }

        #[test]
        fn transient_phases_always_wait_even_with_no_watchdog() {
            // Codex review round 6/7 BLOCKERs: a settle timeout (the
            // watchdog's own, OR this call's own fresh spawn) can return
            // "starting" -- this must NEVER be treated as a decision
            // point, watchdog or not (an adopted authority with no
            // watchdog at all is still mid-flight here too).
            for phase in ["starting", "ending"] {
                assert!(!is_resting_phase(phase, true), "{phase} is transient even with a watchdog");
                assert!(!is_resting_phase(phase, false), "{phase} is transient even with no watchdog");
            }
        }
    }

    /// The guard-HELD body shared by [`resume_if_absent`] (which takes
    /// the row's guard itself, around this whole call), [`ensure_started`]
    /// (which already holds it for its own whole call, on the
    /// `UNREACHABLE_PHASE` arm), and `handlers.rs`'s
    /// `destroy_capsule_workspace` (which holds the SAME row guard
    /// across this call and its own following `end_run`, so it must
    /// reach this guard-free inner directly rather than through
    /// [`resume_if_absent`] — a second `blocking_lock` on a guard this
    /// caller already holds would deadlock) — ADR 0043 decision 33.
    /// `pub` for that cross-module reach; still crate-internal in effect
    /// (`mod runtime` itself is private, re-exported only within this
    /// crate via `capsule_workspace`'s own `pub use runtime::*`).
    /// Rechecks, now that the guard is actually held: the row is still
    /// registered (`Err` — "unknown workspace" — a concurrent remover
    /// could have removed it while this call waited for the lock); if
    /// the watchdog already observed it `Phase::Terminal` (latched), reports that phase without touching the lane
    /// again. Otherwise probes once: any phase OTHER than
    /// [`UNREACHABLE_PHASE`] is returned as-is — nothing to resume (in
    /// particular, a missing state dir reads `NEVER_STARTED_PHASE` here
    /// and this returns WITHOUT ever spawning — never a licence to
    /// recreate one). Only a genuinely unreachable lane spawns, and only
    /// with `StartMode::Resume` — this is the resume path, never the
    /// create one — via [`start_supervisor`], which settles before
    /// returning (`Ok`) or reports why it could not spawn at all
    /// (`Err`). Never sends `reset`: an `EndedNoRespawn` settle is
    /// reported as-is here — retiring it is attach's own job (R3), not
    /// resume's.
    pub fn resume_locked(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        workspaces: Workspaces,
    ) -> Result<&'static str, String> {
        let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
            return Err("unknown workspace".to_string());
        };
        if ws.phase() == crate::workspaces::Phase::Terminal {
            return Ok(super::phase_str(sot_log::wire::SupervisorPhase::Terminal));
        }
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let (phase, observation) = probe(&state_dir);
        if phase != UNREACHABLE_PHASE {
            // Already answering -- no spawn, but still an adoption if this row had none yet.
            observe_with_adoption(&ws, observation);
            return Ok(phase);
        }
        // Ruling: the watchdog is the single writer of restarts for a
        // child this daemon spawned. A row whose watchdog is still alive
        // reports its own (unreachable) phase as-is -- the caller waits
        // as it would for Starting -- rather than racing a second spawn
        // in front of the watchdog's own backoff and restart budget. A
        // row with no watchdog (never started, terminal, or a live
        // authority merely ADOPTED at boot) reaches the spawn below
        // unchanged.
        if ws.watchdog_owner().is_some() {
            return Ok(phase);
        }
        let argv = agent_argv(agent_kind)?;
        start_supervisor(state_root, workspace_id, StartMode::Resume, &argv, project_root, agent_name, slug, workspaces)
    }

    /// R2's own resume path (ADR 0043 decision 33): a row whose lane has
    /// gone quiet is resumed, in place, by the operation that needed it
    /// live — never by a list. BLOCKING (`phase_of`, `query_status`, and
    /// — when it actually resumes — a process spawn are all real I/O):
    /// callers run it via `spawn_blocking`. Takes this workspace's OWN
    /// guard (`Workspaces::capsule_guard`) for its whole duration —
    /// `Err("unknown workspace")` if the row is not currently registered
    /// (that call itself refuses to mint an orphan guard entry, Codex
    /// review 2026-09-11 — no lock to even take); [`resume_locked`]'s own
    /// doc has what the SECOND recheck, once the lock is actually held,
    /// covers. Headless callers (`handlers.rs`'s `pty.input`/
    /// `pty.screen`) use this in place of a bare [`phase_of`] read so a
    /// row whose supervisor died between two ops resumes itself rather
    /// than answering `NotReady` forever; `pty.open`'s own attach path
    /// keeps using [`ensure_started`] instead — it needs the `Start` arm
    /// this function deliberately does not have (resume-only intent: it
    /// never starts a row that has no pointer published at all, and it
    /// never sends `reset`).
    pub fn resume_if_absent(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        workspaces: Workspaces,
    ) -> Result<&'static str, String> {
        let Some(guard) = workspaces.capsule_guard(workspace_id) else {
            return Err("unknown workspace".to_string());
        };
        let _held = guard.blocking_lock();
        resume_locked(state_root, workspace_id, agent_kind, agent_name, slug, project_root, workspaces)
    }

    /// Bounds how many re-probe PASSES [`ensure_started`] spends waiting
    /// for a transient phase to settle (a watchdog mid-restart; or
    /// simply a still-starting/still-recovering authority, watchdog or
    /// not) before falling back to reporting the row's phase unacted-on,
    /// exactly as it did before that wait existed. A pass count, not
    /// wall-clock time (round 9: a wall-clock deadline, even one started
    /// at the first `WaitForSettle`, still ticks down while this call
    /// merely waits to ACQUIRE the guard -- behind a watchdog's own
    /// 7/15/30s backoff, say -- time that was never this budget's to
    /// spend either). NOT a tight bound in wall-clock terms: a pass that
    /// re-enters the retire arm pays a `stop` plus a fresh
    /// `SPAWN_SETTLE_DEADLINE` (2s) each time, so 50 passes can hold the
    /// row's guard for minutes on a row whose recovery keeps outlasting
    /// that deadline -- accepted rather than a second counter, since the
    /// fallback is always the honest "still unacted-on" report this
    /// function already gives, never a wrong decision.
    const ACTIVATION_MAX_REPROBES: u32 = 50;

    /// How often [`ensure_started`] re-probes a transient phase while
    /// waiting. No progress signal to race against it (Codex review
    /// round 8: a `Notify` here saved at most one interval's worth of
    /// latency, never correctness, and cost a real subscription-ordering
    /// hazard to close properly -- deleted).
    const ACTIVATION_REPROBE_INTERVAL: Duration = Duration::from_millis(200);

    /// The ONE shared activation boundary for every caller: guard, inert-anchor refusal, then start/resume/(Selection-only) retire+reset. BLOCKING.
    pub fn ensure_started(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        intent: ActivationIntent,
        workspaces: Workspaces,
    ) -> Result<Option<()>, String> {
        let Some(guard) = workspaces.capsule_guard(workspace_id) else {
            return Err("unknown workspace".to_string());
        };
        let mut reprobes: u32 = 0;
        // Round-9 BLOCKER: the identity of the fresh authority THIS
        // activation itself spawned to retire an ended row, carried
        // across passes -- see `ensure_started_locked`'s own doc for why.
        let mut own_spawn: Option<crate::workspaces::SupervisorIdentity> = None;
        loop {
            let held = guard.blocking_lock();
            // Rechecked under the guard -- a concurrent remover could have removed the row while this call waited.
            let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
                return Err("unknown workspace".to_string());
            };
            // R1: the ONE place this check lives, under the guard against the CURRENT row.
            if workspaces.is_inert_default_anchor(&ws) {
                return Ok(None);
            }
            // Clear-on-attempt happens HERE inside the guard so two serialized attempts can't interleave.
            ws.set_activation_error(None);
            match ensure_started_locked(
                state_root, workspace_id, agent_kind, agent_name, slug, project_root, intent, workspaces.clone(),
                &mut own_spawn,
            ) {
                LockedStep::Done(result) => {
                    if let Err(detail) = &result {
                        ws.set_activation_error(Some(detail.clone()));
                    }
                    return result;
                }
                // Ruling: never drop the caller's intent, and never
                // decide from a transient phase (see `is_resting_phase`).
                // Release the guard (a watchdog, or the authority's own
                // recovery, needs it free to make ANY progress) and
                // sleep one re-probe interval, then re-acquire and
                // re-run this WHOLE decision from scratch with the SAME
                // original intent -- so a Selection on a run that comes
                // back EndedNoRespawn still retires and resets, never
                // silently "succeeds" on a stale snapshot mid-transition.
                LockedStep::WaitForSettle => {
                    // Test-only: one marker per reprobe cycle, so a test
                    // can wait for (or count) this loop's own progress
                    // instead of guessing a sleep duration. No-op unless
                    // `SOT_TEST_ACTIVATION_BARRIER` is set.
                    crate::server::record_test_activation_marker("waitforsettle");
                    reprobes += 1;
                    if reprobes > ACTIVATION_MAX_REPROBES {
                        // The budget is spent: report the phase as the
                        // pre-ruling code always did for this exact
                        // situation -- attempted, still unsettled, spawn
                        // deferred to whatever is already in flight.
                        // Pre-existing, filed for later (round 10 record):
                        // this `Ok(Some(()))` is indistinguishable on the
                        // wire from a genuine success, so `pty.open`
                        // answers `attach_direct` onto a row that may
                        // still be `ended_no_respawn`, with no
                        // `activation_error` set -- exhaustion itself is
                        // not surfaced as a caller-visible failure.
                        return Ok(Some(()));
                    }
                    drop(held);
                    // BLOCKING (this whole function is): plain sleep, no
                    // tokio runtime handle needed, same as
                    // `settle_after_spawn`'s own wait.
                    std::thread::sleep(ACTIVATION_REPROBE_INTERVAL);
                }
            }
        }
    }

    /// What one guard-held attempt at [`ensure_started_locked`] concluded.
    enum LockedStep {
        /// Genuinely resolved -- nothing further to do differently.
        Done(Result<Option<()>, String>),
        /// The row's phase is transient, not a resting point a decision
        /// may be made from -- see [`is_resting_phase`]'s own doc for
        /// the invariant, and [`ensure_started`]'s own loop for the wait.
        WaitForSettle,
    }

    /// [`ensure_started`]'s guard-held body, after membership/inert-anchor/activation-error clear.
    ///
    /// `own_spawn` is [`ensure_started`]'s own local, carried across
    /// `WaitForSettle` passes (round-9 BLOCKER): the identity of the
    /// fresh authority a PRIOR pass of THIS SAME activation spawned to
    /// retire an ended row. Without it, a transient `retired_phase`
    /// below returns `WaitForSettle`, the next pass re-runs this whole
    /// function from scratch, sees `ended_no_respawn` again, and (with
    /// no memory of the spawn it just did) retires AGAIN -- stopping the
    /// authority this same activation only just spawned and spawning
    /// another. A row whose `--resume` recovery takes longer than
    /// `SPAWN_SETTLE_DEADLINE` on every attempt then cycles stop, spawn,
    /// settle, wait, stop forever: no `reset` is ever reached, so the
    /// row can never start a new run. Recognizing "the current resident
    /// IS the fresh binary I already spawned" breaks that cycle: reset
    /// it directly, no second stop, no second spawn.
    fn ensure_started_locked(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        intent: ActivationIntent,
        workspaces: Workspaces,
        own_spawn: &mut Option<crate::workspaces::SupervisorIdentity>,
    ) -> LockedStep {
        let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
            return LockedStep::Done(Err("unknown workspace".to_string()));
        };
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let (initial_phase, initial_observation) = probe(&state_dir);
        observe_with_adoption(&ws, initial_observation);
        // Ruling: never drop the caller's intent, and never decide from
        // a transient snapshot -- see `is_resting_phase`'s own doc for
        // the invariant, and `ensure_started`'s own loop for what
        // happens on `WaitForSettle` (this checks BEFORE computing
        // `mode`: `start_mode_for_phase` already maps a transient
        // `starting` read to `None`, "nothing to do", which is exactly
        // the stale-snapshot bug this closes).
        if !is_resting_phase(initial_phase, ws.watchdog_owner().is_some()) {
            return LockedStep::WaitForSettle;
        }
        // R4c: Reconnect permits only StartMode::Resume -- never a row's first-ever start.
        let mode = start_mode_for_phase(initial_phase);
        let mode = if intent == ActivationIntent::Reconnect && mode != Some(StartMode::Resume) { None } else { mode };
        let (spawned, settled_phase) = match mode {
            Some(StartMode::Start) => {
                let argv = match agent_argv(agent_kind) {
                    Ok(a) => a,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                let phase = match start_supervisor(
                    state_root, workspace_id, StartMode::Start, &argv, project_root, agent_name, slug, workspaces.clone(),
                ) {
                    Ok(p) => p,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                (Some(()), phase)
            }
            Some(StartMode::Resume) => {
                let phase = match resume_locked(
                    state_root, workspace_id, agent_kind, agent_name, slug, project_root, workspaces.clone(),
                ) {
                    Ok(p) => p,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                (Some(()), phase)
            }
            None => (None, initial_phase),
        };
        // Ruling (round 7 BLOCKER): the phase a spawn THIS call itself
        // just made settles at is EXACTLY as liable to be transient as
        // the initial probe was -- a watchdog now owns the fresh child
        // either way (`Start`/`Resume` both install one via
        // `spawn_and_watch`), so only "resting or not" is left to ask.
        // `mode == None` means `settled_phase == initial_phase`, already
        // proven resting above -- asking again is cheap and uniform.
        // Round 10: NOT gated to Selection -- a round-9 attempt to skip
        // this wait for Reconnect broke a slow `--resume` recovery:
        // `settle_after_spawn` reads `unreachable` (the fresh child not
        // listening yet) past its own 2s deadline just as readily as
        // `starting`, so an ungated Reconnect returned `Ok` at once onto
        // a row with nothing actually resumed yet, and the bridge's next
        // connect failed `lane_absent` instead of converging -- the exact
        // scenario case 4 below exercises. Both intents wait.
        if !is_resting_phase(settled_phase, true) {
            return LockedStep::WaitForSettle;
        }
        // One answered phase still has no live leg to attach to:
        // `EndedNoRespawn` (`--resume`/`--start` deliberately never
        // resurrect it — ADR 0041's own no-resurrection rule). A new run
        // never starts on a resident authority; replacement requires a
        // confirmed stop (ADR 0043 decision 33's retirement clause).
        let ended_phase = super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn);
        if settled_phase == ended_phase {
            // RB: a passive Reconnect never resets an ended run -- only a real Selection may retire+reset it below.
            if intent == ActivationIntent::Reconnect {
                return LockedStep::Done(Ok(spawned));
            }
            // Round-9 BLOCKER fast path: a PRIOR pass of this SAME
            // activation already retired this row (see `own_spawn`'s own
            // doc above) and the resident authority is STILL that exact
            // fresh spawn -- reset it directly, no second stop, no
            // second spawn. Without this, a transient `retired_phase`
            // below sends this function back to `WaitForSettle`, and the
            // NEXT pass re-enters this exact branch from scratch with no
            // memory of the spawn it just made, stopping and respawning
            // AGAIN -- a row whose recovery consistently outlasts
            // `SPAWN_SETTLE_DEADLINE` would then never converge.
            let already_fresh = own_spawn.is_some_and(|identity| ws.current_supervisor() == Some(identity));
            if !already_fresh {
                // Retire the resting authority (attach's own job, not
                // resume's) before minting a new run over it: the WAITING
                // `stop` (confirmed exit, never `stop_and_warn`) first -- an
                // `Err` here leaves the row untouched, nothing replaced --
                // then a fresh spawn via the same guarded resume body every
                // other caller uses. Sending `reset` straight to the OLD
                // resident process (the prior behaviour) would let IT mint
                // the new voyage and spawn the new leg from whatever binary
                // it cached at its own start.
                if let Err(e) = sot_log::supervisor_client::stop(&state_dir) {
                    return LockedStep::Done(Err(format!("capsule workspace retire (stop before reset) failed: {e}")));
                }
                let argv = match agent_argv(agent_kind) {
                    Ok(a) => a,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                let retired_phase = match start_supervisor(
                    state_root, workspace_id, StartMode::Resume, &argv, project_root, agent_name, slug, workspaces.clone(),
                ) {
                    Ok(p) => p,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                // Remember which authority THIS pass just spawned (a
                // fresh probe, not `retired_phase` alone, since that
                // carries no identity) so a LATER pass -- if this one's
                // own settle below finds it still transient -- recognizes
                // its own child instead of retiring it all over again.
                // Always OVERWRITES, never merely sets: a probe that
                // comes back anything other than `Phase` (the spawn died
                // before this could even read it, say) must CLEAR the
                // old identity too, or a later pass could wrongly credit
                // this attempt with a PRIOR pass's now-dead spawn.
                use crate::workspaces::Observation;
                *own_spawn = match probe(&state_dir) {
                    (_, Observation::Phase { supervisor, .. }) => Some(supervisor),
                    _ => None,
                };
                // Ruling (round 8): the SAME rule applies to the retire
                // arm's own respawn -- a transient `retired_phase` (this
                // fresh authority still recovering) is not yet the
                // honest "it settled somewhere other than
                // ended_no_respawn" failure reported below; wait for it
                // to actually rest first, same as every other decision
                // point in this function.
                if !is_resting_phase(retired_phase, true) {
                    return LockedStep::WaitForSettle;
                }
                if retired_phase != ended_phase {
                    return LockedStep::Done(Err(format!(
                        "capsule workspace retire: resumed authority settled to {retired_phase} instead of ended_no_respawn"
                    )));
                }
            }
            return LockedStep::Done(match sot_log::supervisor_client::reset(&state_dir) {
                // Mints a fresh voyage on the SAME epoch; the observer's next round supersedes the latch.
                Ok(_new_voyage) => Ok(Some(())),
                Err(e) => Err(format!("capsule workspace reset (after retiring an ended run) failed: {e}")),
            });
        }
        LockedStep::Done(Ok(spawned))
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
        /// Exit 70 (`EXIT_CONTENDED`): the authority fence was already
        /// held by a LIVE supervisor when this leg tried to acquire it —
        /// almost always the previous authority for this SAME state dir,
        /// still finishing its own teardown. NEVER treated as
        /// [`Terminal`] (that would mark a perfectly healthy workspace
        /// terminal out from under a run some OTHER leg is still
        /// actively serving). ADR 0043 decision 33 (shrink round): no
        /// longer re-probed for adoption either — [`install_watchdog`]
        /// logs and returns, leaving the row for the next attach's own
        /// [`resume_if_absent`]/[`ensure_started`] to find and resume
        /// under the row's guard, same as any other quiet lane.
        Contended,
        /// Anything else: a genuine crash needing the restart sequence.
        Crash,
    }

    /// Maps a confirmed (or absent) exit code to the watchdog's own
    /// outcome vocabulary.
    fn classify_exit_code(code: Option<i32>) -> LegOutcome {
        match code {
            Some(EXIT_CLEAN) => LegOutcome::Clean,
            Some(EXIT_TERMINAL) => LegOutcome::Terminal,
            Some(EXIT_CONTENDED) => LegOutcome::Contended,
            _ => LegOutcome::Crash,
        }
    }

    /// Waits for `child` to end and classifies the result.
    /// `tokio::process::Child::wait` is trusted outright: the daemon is
    /// the sole, unambiguous owner of a supervisor it spawned itself —
    /// ADR 0043 decision 33, "a watchdog exists only for a `Child` the
    /// daemon launched." There is no adopted twin any more: an authority
    /// `resume_all` merely finds already alive at boot is never watched
    /// at all (see that function's own doc); if it later goes quiet, the
    /// next attach's `resume_if_absent`/`ensure_started` spawns and
    /// watches a FRESH leg, which this function then does own.
    async fn wait_and_classify(mut child: Child, workspace_id: &str) -> LegOutcome {
        let code = match child.wait().await {
            Ok(status) => status.code(),
            Err(e) => {
                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: wait() failed; treating as a crash");
                return LegOutcome::Crash;
            }
        };
        classify_exit_code(code)
    }

    /// Whether it is still THIS watchdog's business to act on
    /// `workspace_id`, checked under the row's own guard (the caller
    /// proves it by already holding it) before EITHER mutation the
    /// watchdog can make: a restart, or a terminal mark. Rechecks, now
    /// that the guard is actually held, that the row is still
    /// registered, not already marked terminal, AND still genuinely
    /// unreachable (`phase_of`). The window this closes is real, not
    /// merely theoretical: `wait_and_classify` itself takes no lock, so
    /// between a leg's confirmed exit and this watchdog's own task
    /// actually reaching the guard, a stale attach's own
    /// `ensure_started`/`resume_if_absent` (on a separate blocking-pool
    /// thread — genuinely concurrent with this async task on a
    /// multi-worker runtime) can win the guard FIRST and resume the row
    /// itself, installing its own fresh watchdog. Any of the three false
    /// means some OTHER actor already settled this row's fate while this
    /// watchdog waited — a restart would then spawn a REDUNDANT
    /// authority, and a terminal mark would misreport a row a fresh
    /// authority is already serving. BLOCKING (`phase_of`): callers run
    /// it via `spawn_blocking`.
    fn watchdog_may_act(workspace_id: &str, state_dir: &Path, workspaces: &Workspaces) -> bool {
        let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
            tracing::debug!(workspace_id = %workspace_id, "capsule supervisor watchdog: row no longer registered; stopping");
            return false;
        };
        if ws.phase() == crate::workspaces::Phase::Terminal {
            return false;
        }
        if phase_of(state_dir) != UNREACHABLE_PHASE {
            tracing::debug!(
                workspace_id = %workspace_id,
                "capsule supervisor watchdog: row was already resumed by another actor while this watchdog waited for the guard; not acting"
            );
            return false;
        }
        true
    }

    /// The identity a settle learned, if its lane answered at all — the
    /// ONE place a daemon-spawned supervisor's identity now comes from
    /// (supervisor-epoch ruling: the supervisor authors it, this daemon
    /// only learns it over the lane, on every platform). Every caller
    /// OVERWRITES with this, never merely sets: a settle that came back
    /// anything other than `Phase` must clear the previous leg's
    /// identity too, or a later terminal mark could be credited to a
    /// prior, now-dead spawn.
    fn identity_of(observation: &crate::workspaces::Observation) -> Option<crate::workspaces::SupervisorIdentity> {
        match observation {
            crate::workspaces::Observation::Phase { supervisor, .. } => Some(*supervisor),
            _ => None,
        }
    }

    /// Mints an ownership token for one watchdog install — unique for
    /// this daemon's lifetime, which is the whole guarantee
    /// `Workspace::watchdog_owner` needs (see that field's own doc).
    fn next_watchdog_owner() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// A watchdog's exit-classification observation; no guard needed.
    /// `Some` judges the mark against the identity the leg's own settle
    /// learned, exactly as before. `None` — a leg that exited before
    /// ever answering its lane, which `sot-capsule supervise` does on
    /// every bootstrap failure (it returns `EXIT_TERMINAL` from three
    /// sites ahead of its own accept loop) — marks
    /// [`crate::workspaces::Observation::TerminalUnclaimed`] instead, so
    /// the row still latches `terminal` rather than reading `stopped`
    /// and re-spawning the same instant failure on every attach.
    fn observe_terminal(workspaces: &Workspaces, workspace_id: &str, identity: Option<crate::workspaces::SupervisorIdentity>) {
        if let Some(ws) = workspaces.resolve(Some(workspace_id)) {
            let observation = match identity {
                Some(supervisor) => {
                    crate::workspaces::Observation::Phase { phase: crate::workspaces::Phase::Terminal, supervisor, voyage: None }
                }
                None => crate::workspaces::Observation::TerminalUnclaimed,
            };
            super::observer::observe(&ws, observation);
        }
    }

    /// Test-only barrier at the top of the watchdog's own Crash-arm
    /// restart attempt, BEFORE it ever takes the row's guard: when
    /// `SOT_TEST_ACTIVATION_BARRIER` is set, blocks until the test
    /// creates `<that path>.watchdog-restart` -- a file SEPARATE from
    /// the main barrier, so a test can hold the watchdog and
    /// `pty.open`'s own activation independently and so prove either
    /// lock ordering deterministically (Codex review round 6 SHOULD-FIX:
    /// replace an uncontrolled race with exactly this). No-op in
    /// production; gives up past a generous bound rather than hang a
    /// forgotten release forever.
    async fn wait_for_test_watchdog_restart_barrier() {
        let Ok(barrier_path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
            return;
        };
        let path = std::path::PathBuf::from(format!("{barrier_path}.watchdog-restart"));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while !path.is_file() {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(path = ?path, "watchdog restart test barrier: released by timeout, not by the test");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// The watchdog itself: waits for the leg to exit, classifies it, and
    /// on a crash restarts with `--resume` under ADR 0041's own launcher
    /// restart sequence (`RESTART_BACKOFFS`, at most `MAX_RESTARTS_PER_
    /// WINDOW` within `RESTART_WINDOW`), then observes the workspace `Terminal` (latched — `workspace.list` reads
    /// it from memory, rule F). A `Contended` leg (decision 33) logs and returns outright.
    ///
    /// ADR 0043 decision 33: "a watchdog exists only for a `Child` the
    /// daemon launched" — `child` starts as [`spawn_and_watch`]'s own
    /// freshly-spawned process, and every SUBSEQUENT leg (a crash
    /// restart) is a fresh spawn too. There is no adopted counterpart —
    /// see `resume_all`'s own doc for what an already-alive authority
    /// gets instead (nothing, until it goes quiet and a fresh attach
    /// resumes and watches it). `None` from [`Workspaces::capsule_guard`]
    /// at entry (the row already gone by the time this task got to ask)
    /// means there is nothing to watch at all.
    ///
    /// R4a: `Terminal` reports immediately, no guard, identity-judged; `Crash` holds the guard across recheck/backoff/spawn.
    fn install_watchdog(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        slug: String,
        // Accounts brief: captured once, at the same spot `argv`/`cwd`/
        // `agent_name` already are (the row's own resolved values at
        // spawn time -- account never changes mid-row this release, no
        // `workspace.set` yet), and carried unchanged into every
        // crash-restart spawn below.
        agent_kind: String,
        account: String,
        child: Child,
        initial_identity: Option<crate::workspaces::SupervisorIdentity>,
        workspaces: Workspaces,
    ) {
        tokio::spawn(async move {
            let Some(capsule_guard) = workspaces.capsule_guard(&workspace_id) else {
                return;
            };
            // Ruling: the watchdog is the single writer of restarts for a
            // child this daemon spawned. This guard is the ONE place the
            // row's `watchdog_owner` fact is announced (constructor) and
            // retracted (Drop) -- a compare-and-clear against THIS
            // task's own token, never a plain clear, so a superseded
            // watchdog's belated cleanup can never erase a replacement's
            // ownership set after it. One token per task, minted once:
            // ownership is a property of the WATCHDOG, not of whichever
            // leg it currently holds, so a respawn has nothing to
            // announce here.
            struct WatchdogOwnerGuard {
                workspaces: Workspaces,
                workspace_id: String,
                token: u64,
            }
            impl WatchdogOwnerGuard {
                fn new(workspaces: Workspaces, workspace_id: String) -> Self {
                    let token = next_watchdog_owner();
                    if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
                        ws.set_watchdog_owner(token);
                    }
                    Self { workspaces, workspace_id, token }
                }
            }
            impl Drop for WatchdogOwnerGuard {
                fn drop(&mut self) {
                    if let Some(ws) = self.workspaces.resolve(Some(&self.workspace_id)) {
                        ws.clear_watchdog_owner_if(self.token);
                    }
                }
            }
            let _watchdog_owner_guard = WatchdogOwnerGuard::new(workspaces.clone(), workspace_id.clone());
            // What each exit classification is judged against: the
            // identity the CURRENT leg's own settle learned, or `None`
            // when its lane never answered.
            let mut current_identity = initial_identity;
            let mut leg_opt = Some(child);
            let mut restart_times: Vec<Instant> = Vec::new();
            loop {
                let outcome = match leg_opt.take() {
                    Some(c) => wait_and_classify(c, &workspace_id).await,
                    // A previous restart attempt itself found nothing to
                    // wait on -- counts as another crash against the
                    // same budget.
                    None => LegOutcome::Crash,
                };
                match outcome {
                    LegOutcome::Clean => return,
                    LegOutcome::Terminal => {
                        tracing::warn!(
                            workspace_id = %workspace_id,
                            "capsule supervisor watchdog: leg exited terminal (69) -- marking terminal, no restart"
                        );
                        observe_terminal(&workspaces, &workspace_id, current_identity);
                        return;
                    }
                    LegOutcome::Contended => {
                        tracing::info!(
                            workspace_id = %workspace_id,
                            "capsule supervisor watchdog: leg exited contended (70) -- another authority holds the fence; leaving the row for the next attach"
                        );
                        return;
                    }
                    LegOutcome::Crash => {
                        // Decided before taking the guard -- giving up needs no recheck (R4a).
                        let now = Instant::now();
                        restart_times.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                        if restart_times.len() >= MAX_RESTARTS_PER_WINDOW {
                            tracing::error!(
                                workspace_id = %workspace_id, window = ?RESTART_WINDOW, max = MAX_RESTARTS_PER_WINDOW,
                                "capsule supervisor watchdog: restart budget exhausted -- giving up, marking terminal"
                            );
                            observe_terminal(&workspaces, &workspace_id, current_identity);
                            return;
                        }
                        wait_for_test_watchdog_restart_barrier().await;
                        let _held = capsule_guard.lock().await;
                        let may_act = {
                            let dir = state_dir.clone();
                            let wsid = workspace_id.clone();
                            let workspaces = workspaces.clone();
                            tokio::task::spawn_blocking(move || watchdog_may_act(&wsid, &dir, &workspaces))
                                .await
                                .unwrap_or(false)
                        };
                        if !may_act {
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
                        // on a Tokio worker. Clones are the closure's OWN
                        // copies (`'static` + `Send`, required across the
                        // `.await` below) -- the loop's own locals are
                        // untouched and reused on the NEXT iteration.
                        let exe = sot_capsule_exe.clone();
                        let dir = state_dir.clone();
                        let argv_for_spawn = argv.clone();
                        let cwd_for_spawn = cwd.clone();
                        let agent_name_for_spawn = agent_name.clone();
                        let workspace_id_for_spawn = workspace_id.clone();
                        let slug_for_spawn = slug.clone();
                        let agent_kind_for_spawn = agent_kind.clone();
                        let account_for_spawn = account.clone();
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
                                &agent_kind_for_spawn,
                                &account_for_spawn,
                            )
                        })
                        .await;
                        match spawn_result {
                            Ok(Ok(child)) => {
                                // Settle BEFORE this guard drops — the
                                // SAME shared wait `spawn_and_watch`
                                // itself uses; see `settle_after_spawn`'s
                                // own doc for why a fresh spawn cannot
                                // skip this without reopening the exact
                                // guard-release race this restart's own
                                // recheck above just closed. A fresh leg
                                // begins a fresh epoch exactly as the
                                // first spawn does: by being adopted
                                // from what it answers.
                                let settle_dir = state_dir.clone();
                                let settle_wsid = workspace_id.clone();
                                let settled = tokio::task::spawn_blocking(move || settle_after_spawn(&settle_dir, &settle_wsid)).await;
                                // Always OVERWRITE, never merely set: a
                                // settle that yields no `Phase` clears
                                // the PREVIOUS leg's identity, or the
                                // next terminal mark would be credited
                                // to a spawn that is already dead.
                                current_identity = match &settled {
                                    Ok((_phase, observation)) => identity_of(observation),
                                    Err(_join_err) => None,
                                };
                                if let (Ok((_phase, observation)), Some(ws)) = (settled, workspaces.resolve(Some(&workspace_id))) {
                                    observe_with_adoption(&ws, observation);
                                }
                                leg_opt = Some(child);
                            }
                            Ok(Err(e)) if e.kind() == ErrorKind::Unsupported => {
                                // `qualified_state_root` refused (ADR 0043
                                // decision 23: the state root went unqualified
                                // out from under a live row -- an `XDG_STATE_HOME`
                                // change, a remounted volume). No retry can change
                                // that without operator action -- mark terminal
                                // now (the error names the recovery).
                                tracing::error!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: unqualified state root -- marking terminal, no restart");
                                observe_terminal(&workspaces, &workspace_id, current_identity);
                                return;
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: restart spawn failed");
                            }
                            Err(join_err) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %join_err, "capsule supervisor watchdog: restart spawn task panicked");
                            }
                        }
                        // `_held` drops here -- released only once the
                        // new leg exists, or the attempt has failed.
                    }
                }
            }
        });
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
    /// ADR 0043 decision 33 (Codex review, 2026-09-11): every candidate's
    /// guard is taken FIRST, via `try_lock` — never blocking, and a row
    /// already busy (an attach's own `ensure_started`/`resume_if_absent`
    /// got there first) is simply skipped, that caller's own attempt
    /// being the one that counts — and the decision itself is then made
    /// UNDER that guard by [`resume_locked`], the SAME function every
    /// other resume path shares: a row still alive (from a previous
    /// daemon lifetime, spawned detached by ADR 0042 design, outliving
    /// the daemon that just restarted) reports its own current phase and
    /// spawns nothing — no second `--resume` leg races the live one's
    /// `supervisor.lock`, and no watchdog is installed for a process this
    /// daemon did not itself launch (decision 33: "a watchdog exists only
    /// for a `Child` the daemon launched"). If that authority later goes
    /// quiet, the next attach's own `resume_if_absent`/`ensure_started`
    /// spawns and watches a FRESH leg under the SAME row's guard —
    /// reactive, same as every other resume path (a list never resumes).
    /// A genuinely unreachable row spawns `--resume`, settling before
    /// this task returns (`resume_locked` -> `start_supervisor` ->
    /// `settle_after_spawn`).
    ///
    /// A state directory with NO matching registry entry is left
    /// COMPLETELY untouched, logged once (ADR 0042: "the daemon's
    /// workspace list is the list" — an orphan is not addressable
    /// through any op, so resuming it would create a live, unaddressable
    /// process; deleted, the bare-shell fallback an earlier version used
    /// here).
    ///
    /// Runs off the startup critical path (finding 10): `server.rs`
    /// calls this via `tokio::spawn`, never awaited, and every probe/spawn
    /// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
    /// a semaphore — thousands of preserved workspaces cannot turn this
    /// into an unbounded synchronous fan-out before the listener binds.
    pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
        // 2026-09-04 amendment: the inert default anchor is never resumed
        // here either, even on the rare box where a pointer already exists
        // for it (a hand-edited toml that dropped its agent after a prior
        // real run). Every OTHER `agent == "none"` capsule row still
        // resumes: `agent_argv("none")` is a real leg (the bare platform
        // shell). The predicate — and why its runtime term matters — is
        // `Workspaces::is_inert_default_anchor`.
        let capsule_rows: Vec<Arc<crate::workspaces::Workspace>> = workspaces
            .list()
            .into_iter()
            .filter(|ws| ws.runtime == "capsule")
            .filter(|ws| !workspaces.is_inert_default_anchor(ws))
            .collect();

        // Covers both boot load and adoption of a still-live prior authority.
        for ws in &capsule_rows {
            super::observer::ensure_running(&workspaces, ws);
        }

        let candidates: Vec<(String, String, PathBuf, String, String)> = capsule_rows
            .into_iter()
            .filter(|ws| {
                let state_dir = super::state_dir_for(&state_root, &ws.workspace_id);
                sot_log::pointer::pointer_path(&state_dir).is_file()
            })
            .map(|ws| {
                (
                    ws.workspace_id.clone(),
                    ws.agent(),
                    ws.project_root.clone(),
                    ws.agent_name(),
                    ws.slug.clone(),
                )
            })
            .collect();

        let semaphore = Arc::new(tokio::sync::Semaphore::new(LANE_CONCURRENCY));
        let mut joins = Vec::with_capacity(candidates.len());
        for (workspace_id, agent_kind, project_root, agent_name, slug) in candidates {
            let permit = semaphore.clone();
            let state_root = state_root.clone();
            let workspaces = workspaces.clone();
            joins.push(tokio::spawn(async move {
                let _permit = permit.acquire_owned().await;
                let Some(guard) = workspaces.capsule_guard(&workspace_id) else {
                    return;
                };
                let lock_result = guard.try_lock();
                match lock_result {
                    Ok(_held) => {
                        let workspace_id_for_log = workspace_id.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            resume_locked(&state_root, &workspace_id, &agent_kind, &agent_name, &slug, &project_root, workspaces)
                        })
                        .await;
                        match result {
                            Ok(Ok(phase)) => {
                                tracing::info!(workspace_id = %workspace_id_for_log, phase, "capsule workspace resume-scan: row resolved");
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(workspace_id = %workspace_id_for_log, error = %e, "capsule workspace resume-scan: row resolution failed");
                            }
                            Err(join_err) => {
                                tracing::warn!(workspace_id = %workspace_id_for_log, error = %join_err, "capsule workspace resume-scan: resume task panicked");
                            }
                        }
                    }
                    Err(_) => {
                        tracing::debug!(workspace_id = %workspace_id, "capsule workspace resume-scan: row's guard busy; skipping");
                    }
                }
            }));
        }
        for j in joins {
            let _ = j.await;
        }

        log_registryless_state_dirs(&state_root, &workspaces);
        log_orphaned_state_dirs(&state_root, &workspaces);
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

    /// The reverse of [`log_registryless_state_dirs`]: a REGISTERED
    /// capsule row whose `state_dir` does not exist under THIS daemon's
    /// own state root — a DIAGNOSTIC candidate list only, never a proof.
    /// A row that went through `workspace.create` and reached the
    /// registry always had a `start_supervisor` call succeed there (a
    /// failed spawn rolls the row and its toml back before either is
    /// persisted, `handlers.rs`'s create rollback), and that success is
    /// exactly what creates the directory — but a row can ALSO reach the
    /// registry by a pre-seeded or hand-authored toml that has never been
    /// through `workspace.create` at all (a legitimate, tested shape:
    /// "Rule H" in `capsule_workspaces.rs`'s own integration suite), and
    /// reads identically here — no state dir, no pointer, "never started
    /// yet" — RIGHT UP UNTIL its first attach spawns it for real. This
    /// function cannot and does not try to tell the two apart (see
    /// `workspace.list`'s own phase, which deliberately does not either —
    /// decision 33's Rule B: "neither has ever had a real run"); it only
    /// NAMES every such row, once, at boot — never from the per-row
    /// lifecycle observer's own `POLL_INTERVAL` loop, which would
    /// otherwise repeat the same line forever — so an operator staring at
    /// `sotd.log` after the field defect this closes (a leaked scratch-
    /// daemon registry row) has a lead to start from. Removal is still
    /// only ever `workspace.destroy`'s to decide, via `end_run`'s own
    /// PROOF (a real lane connect, decision 27's absent shape) — which
    /// needs no such distinction either: nothing running is nothing
    /// running, whether the row is freshly seeded or truly abandoned.
    /// STAT STRICTLY, matching `end_run`: only `ErrorKind::NotFound` is
    /// "missing" — a permission or I/O error says nothing about whether
    /// the directory is actually gone, so it is skipped (not logged)
    /// rather than guessed at.
    fn log_orphaned_state_dirs(state_root: &Path, workspaces: &Workspaces) {
        for ws in workspaces.list() {
            if ws.runtime != "capsule" {
                continue;
            }
            let state_dir = super::state_dir_for(state_root, &ws.workspace_id);
            match std::fs::metadata(&state_dir) {
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                _ => continue,
            }
            tracing::warn!(
                workspace_id = %ws.workspace_id, slug = %ws.slug, state_dir = ?state_dir,
                "capsule workspace resume-scan: registered row has no state directory under this \
                 daemon's state root -- reads as an ordinary stopped row (indistinguishable here \
                 from one simply never started yet); workspace.destroy removes it once no \
                 supervisor answers its lane"
            );
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub use runtime::*;

/// One lifecycle observer per capsule row -- the SINGLE writer of `Workspace::phase`.
#[cfg(any(windows, target_os = "linux"))]
pub mod observer {
    use super::{local_phase, phase_for_missing_pointer, state_dir_for};
    use crate::workspaces::{Observation, SupervisorIdentity, Workspace, Workspaces};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    /// Matches the attach client's own liveness poll interval.
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    /// The ONE call site that feeds an observation into `ws`'s phase cell; a rejection logs at debug, never an error.
    pub(crate) fn observe(ws: &Workspace, observation: Observation) {
        if !ws.apply_phase_observation(observation) {
            tracing::debug!(workspace_id = %ws.workspace_id, "capsule workspace observer: observation rejected (stale, or a latched terminal phase)");
        }
    }

    /// Idempotent: ensures a lifecycle-observer task runs for `ws`, never from `Workspaces::insert`.
    pub fn ensure_running(workspaces: &Workspaces, ws: &Arc<Workspace>) {
        if workspaces.has_observer(&ws.workspace_id) {
            return;
        }
        let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
            return;
        };
        let state_dir = state_dir_for(&state_root, &ws.workspace_id);
        let persistent = sot_log::supervisor_client::Persistent::new(&state_dir);
        // Obtained outside the loop so removal can interrupt a round blocked in `spawn_blocking`.
        let cancel_handle = persistent.cancel_handle();
        let cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || cancel_handle.cancel());
        let ws_for_task = ws.clone();
        let workspaces_for_task = workspaces.clone();
        let workspace_id = ws.workspace_id.clone();
        let handle = tokio::spawn(run(ws_for_task, state_dir, persistent, workspaces_for_task));
        workspaces.install_observer(&workspace_id, handle, cancel);
    }

    /// Immediate first round, then every `POLL_INTERVAL`; exits once its row is gone.
    async fn run(
        ws: Arc<Workspace>,
        state_dir: PathBuf,
        mut persistent: sot_log::supervisor_client::Persistent,
        workspaces: Workspaces,
    ) {
        loop {
            if workspaces.resolve(Some(&ws.workspace_id)).is_none() {
                return;
            }
            let dir = state_dir.clone();
            let (returned, observation) = tokio::task::spawn_blocking(move || {
                let obs = poll_once(&dir, &mut persistent);
                (persistent, obs)
            })
            .await
            .unwrap_or_else(|_join_err| {
                (sot_log::supervisor_client::Persistent::new(&state_dir), Observation::Failed)
            });
            persistent = returned;
            observe(&ws, observation);
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// One BLOCKING round: no pointer -> `Stopped`; otherwise one `status` call via [`local_phase`].
    fn poll_once(state_dir: &Path, persistent: &mut sot_log::supervisor_client::Persistent) -> Observation {
        if phase_for_missing_pointer(sot_log::pointer::pointer_path(state_dir).is_file()).is_some() {
            return Observation::Stopped;
        }
        match persistent.status() {
            Ok(report) => Observation::Phase {
                phase: local_phase(report.phase),
                supervisor: SupervisorIdentity { pid: report.pid, created: report.created },
                voyage: report.voyage.as_deref().and_then(|v| v.parse().ok()),
            },
            Err(sot_log::Error::VersionSkew) => Observation::Foreign,
            Err(_) => Observation::Failed,
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[cfg(test)]
mod observer_tests {
    use super::observer::observe;
    use crate::workspaces::{Observation, Phase, SupervisorIdentity, Workspace, Workspaces};
    use std::path::PathBuf;

    /// A registered capsule row with its epoch begun at `supervisor` (RA).
    fn seeded_capsule_row(supervisor: SupervisorIdentity) -> std::sync::Arc<Workspace> {
        let ws = unclaimed_capsule_row();
        ws.begin_supervisor_epoch(supervisor);
        ws
    }

    /// The same row with NO epoch yet -- the empty cell the ruling's
    /// addition (a) is about: a row the daemon has spawned for but whose
    /// lane has not yet answered, or one only ever read as stopped.
    fn unclaimed_capsule_row() -> std::sync::Arc<Workspace> {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "observer-test",
            PathBuf::from("/tmp/sot-observer-test"),
            false,
            "none".to_string(),
            String::new(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        reg.insert(ws)
    }

    fn identity(pid: u32, created: u64) -> SupervisorIdentity {
        SupervisorIdentity { pid, created }
    }

    fn phase_obs(phase: Phase, supervisor: SupervisorIdentity) -> Observation {
        Observation::Phase { phase, supervisor, voyage: None }
    }

    #[test]
    fn begin_supervisor_epoch_resets_phase_voyage_and_failures() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        assert_eq!(ws.phase(), Phase::Stopped, "a fresh epoch starts Stopped");
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        ws.begin_supervisor_epoch(identity(2, 200));
        assert_eq!(ws.phase(), Phase::Stopped, "a new epoch resets phase");
    }

    /// RA blocker 2: judged by identity EQUALITY, never by timestamp -- a same-tick stranger is never "newer."
    #[test]
    fn a_different_supervisor_is_rejected_even_with_an_equal_or_newer_timestamp() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, phase_obs(Phase::Ending, identity(2, 100)));
        assert_eq!(ws.phase(), Phase::Ready, "a same-tick stranger must never be accepted");

        observe(&ws, phase_obs(Phase::Ending, identity(3, 500)));
        assert_eq!(ws.phase(), Phase::Ready, "a newer-timestamped stranger must never be accepted");
    }

    #[test]
    fn unreachable_needs_two_consecutive_failed_rounds() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Ready, "a single failed round must not move the phase");

        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Unreachable);

        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, Observation::Failed);
        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Ready, "a success between two failures resets the count");
    }

    /// RA blocker 1: `Terminal` is supervisor-scoped, not voyage-scoped -- applies regardless of phase/voyage.
    #[test]
    fn a_terminal_observation_with_no_voyage_applies_even_over_ended_no_respawn() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        let v1 = uuid::Uuid::from_u128(1);
        observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn);

        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal, "Terminal must not be hidden behind an EndedNoRespawn latch");
    }

    #[test]
    fn a_terminal_observation_latches_and_only_a_fresh_epoch_clears_it() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal);

        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Terminal, "terminal latches within its own epoch");
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Terminal, "terminal latches across a failed round too");

        // Only a fresh epoch (a new spawn or adoption) clears it.
        let b = identity(2, 200);
        ws.begin_supervisor_epoch(b);
        observe(&ws, phase_obs(Phase::Ready, b));
        assert_eq!(ws.phase(), Phase::Ready);
    }

    /// `EndedNoRespawn` latches for its VOYAGE within the epoch; a strictly newer voyage clears it.
    #[test]
    fn ended_no_respawn_latches_for_its_voyage() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        let v1 = uuid::Uuid::from_u128(1);
        let v2 = uuid::Uuid::from_u128(2);

        observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn);

        observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn, "the same voyage reported again must never clear the latch");

        observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, voyage: Some(v2) });
        assert_eq!(ws.phase(), Phase::Ready, "a strictly newer voyage supersedes the latch");
    }

    /// Addition (a): an empty cell takes the FIRST prover -- and only
    /// the first. The rejection half is the same rule RA blocker 2
    /// asserts, re-run against the adopting branch to prove the adoption
    /// is scoped to `None` and never widens who may claim a claimed row.
    #[test]
    fn an_empty_cell_adopts_its_first_prover_and_then_judges_strangers() {
        let a = identity(1, 100);
        let ws = unclaimed_capsule_row();
        assert_eq!(ws.current_supervisor(), None, "an unclaimed row has no epoch");

        // Two failed rounds first: an empty cell that is already
        // `Unreachable` must still adopt, and adoption must reset the
        // failure count, exactly as `begin_supervisor_epoch` does.
        observe(&ws, Observation::Failed);
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Unreachable);

        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.current_supervisor(), Some(a), "the first prover becomes the epoch");
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, phase_obs(Phase::Ending, identity(2, 100)));
        assert_eq!(ws.phase(), Phase::Ready, "a claimed cell still rejects a stranger");
        assert_eq!(ws.current_supervisor(), Some(a));
    }

    /// Addition (b): a bootstrap-failing leg latches its row `terminal`
    /// without an identity, and a later genuine authority adopts and
    /// clears it -- a row whose lane answers is not terminal.
    #[test]
    fn terminal_unclaimed_latches_an_empty_cell_and_a_later_authority_clears_it() {
        let ws = unclaimed_capsule_row();
        assert!(ws.apply_phase_observation(Observation::TerminalUnclaimed));
        assert_eq!(ws.phase(), Phase::Terminal);
        assert_eq!(ws.current_supervisor(), None, "the mark leaves the cell claimable");

        observe(&ws, Observation::Stopped);
        assert_eq!(ws.phase(), Phase::Terminal, "the latch holds against a plain stopped read");

        let a = identity(7, 700);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready, "an authority that actually answers clears the latch");
        assert_eq!(ws.current_supervisor(), Some(a));
    }

    /// The other half of (b): a cell claimed at any point in this
    /// daemon's life is closed to it, so a stale watchdog can never
    /// latch a row it no longer owns.
    #[test]
    fn terminal_unclaimed_is_refused_by_a_claimed_cell() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));

        assert!(!ws.apply_phase_observation(Observation::TerminalUnclaimed));
        assert_eq!(ws.phase(), Phase::Ready);
    }

    /// `activation_error` is retained until the next attempt; orthogonal to phase.
    #[test]
    fn activation_error_is_independent_of_phase_and_clears_on_the_next_attempt() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        assert_eq!(ws.activation_error(), None);

        ws.set_activation_error(Some("capsule spawn failed: boom".to_string()));
        assert_eq!(ws.phase(), Phase::Stopped, "an activation error must never move phase on its own");

        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal);
        assert_eq!(
            ws.activation_error(),
            Some("capsule spawn failed: boom".to_string()),
            "a phase observation must never clear a pending activation_error -- only the next attempt does"
        );

        ws.set_activation_error(None);
        assert_eq!(ws.activation_error(), None);
    }
}

/// ADR 0042 amendment (2026-09-07), "a session types into and reads a
/// sibling row": the daemon's own HEADLESS client on a capsule lane — the
/// same [`sot_log::fe_client_io::FeAttachClient`] the frontend's drawer
/// uses, run on the daemon side with no viewport and no user watching.
/// `type_into` takes the pen only long enough to deliver ONE `input` frame
/// and never resizes the pane (ADR 0041's take-on-first-input semantics,
/// applied to a second kind of client); `screen_of` attaches as a pure
/// WATCHER and never takes at all. Platform-neutral: gated the same as
/// `mod runtime` above, so this module simply does not exist on a host
/// that cannot run a capsule row in the first place. (macOS lane,
/// corrected: `FeAttachClient` is NO LONGER what gates this —
/// `sot_log::fe_client_io` is ungated since ADR 0045 decision 1, and only
/// its `PlatformEndpoint`-typed default is cfg'd. The reason is now
/// solely `mod runtime`'s own; when that widens, so does this.)
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
        let result = send_and_wait_recorded(&mut client, bytes, deadline);
        client.shutdown(SHUTDOWN_WAIT);
        result
    }

    /// The send/wait-for-verdict loop, factored out of [`type_into`] so
    /// [`write_and_enter`] can call it twice (text, then Enter) on ONE
    /// continuous attach — a mid-sequence pen loss then surfaces as an
    /// ordinary `RefusedStale`/`is_dead()` on the same client.
    fn send_and_wait_recorded(client: &mut Client, bytes: &[u8], deadline: Instant) -> Result<usize, HeadlessError> {
        let expected = bytes.len() as u64;
        let before = client.recorded_bytes();
        client.send_input(bytes);
        loop {
            client.pump();
            if let Some(outcome) = client.last_input_outcome() {
                match outcome {
                    InputOutcome::Recorded => {
                        if client.recorded_bytes().saturating_sub(before) >= expected {
                            return Ok(bytes.len());
                        }
                        // A single `send_input` call is always flushed as
                        // ONE input frame in practice (the payload already
                        // fits under `TAKE_QUEUE_CAP`, so nothing splits
                        // it) — this branch should be unreachable, but
                        // correctness does not depend on that: keep
                        // polling for the rest, bounded by the same
                        // deadline, rather than declaring victory early.
                        if Instant::now() >= deadline {
                            return Err(HeadlessError {
                                phase: "record",
                                detail: "deadline exceeded before the whole payload was recorded"
                                    .to_string(),
                                submitted: true,
                            });
                        }
                        std::thread::sleep(POLL_INTERVAL);
                    }
                    InputOutcome::RefusedStale => {
                        return Err(HeadlessError {
                            phase: "input",
                            detail: "input refused as stale (the take epoch changed); \
                                     this op is never retried"
                                .to_string(),
                            submitted: true,
                        });
                    }
                    InputOutcome::DeliveryUnknown => {
                        return Err(HeadlessError {
                            phase: "record",
                            detail: "input delivery unknown".to_string(),
                            submitted: true,
                        });
                    }
                }
                continue;
            }
            if client.is_dead() {
                return Err(HeadlessError {
                    phase: "take",
                    detail: client.status_line().to_string(),
                    submitted: true,
                });
            }
            if Instant::now() >= deadline {
                return Err(HeadlessError {
                    phase: "record",
                    detail: "deadline exceeded waiting for the input to be recorded".to_string(),
                    submitted: true,
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// A capsule row's SPLIT `enter: true` write: text, a bounded PACING
    /// wait, then Enter as its own write — makes NO submission claim (a
    /// screen snapshot cannot prove one) and never retries either write.
    ///
    /// 1. [`type_into`]'s own size gate runs first, before any attach.
    /// 2. Writes the text — a hard error, never retried, on any failure.
    /// 3. PACES only (claims nothing): waits for the screen to hold still
    ///    `quiet_budget`, bounded overall at `pacing_budget`.
    /// 4. Writes Enter under the text's own grant (headless never
    ///    re-takes): a pen change in between is refused stale by the
    ///    supervisor, so `enter_sent: false`. Once the text is recorded
    ///    (step 2), this ALWAYS answers `Ok`: any Enter failure means
    ///    `enter_sent: false`, never a hard `Err` (a retried caller could
    ///    double the text).
    /// 5. Never retries: the worker's auto-retake after a stale refusal
    ///    is disabled for headless transactions.
    ///
    /// RESIDUAL RISK: this never checks whether a human already has an
    /// unsent draft here (parity with the old tmux path, which never did
    /// either) — a real check needs attach-proto v3's pen-holder signal
    /// (`PenSnapshot`/`holder`), left to the Stage 2 resident-attach lanes.
    ///
    /// Returns `(bytes_written, enter_sent)`: `enter_sent` says only that
    /// the Enter byte was written and recorded, never that codex treated
    /// it as a submitted turn.
    pub fn write_and_enter(
        state_dir: &Path,
        controller_id: &str,
        text: &[u8],
        op_budget: Duration,
        quiet_budget: Duration,
        pacing_budget: Duration,
    ) -> Result<(usize, bool), HeadlessError> {
        if text.len() > TAKE_QUEUE_CAP {
            return Err(HeadlessError {
                phase: "size",
                detail: format!(
                    "payload is {} bytes, exceeding the take queue cap of {TAKE_QUEUE_CAP} bytes",
                    text.len()
                ),
                submitted: false,
            });
        }

        let mut client = attach(state_dir, controller_id)?;
        if let Err(e) = wait_for_checkpoint(&mut client, Instant::now() + op_budget) {
            client.shutdown(SHUTDOWN_WAIT);
            return Err(e);
        }

        let n = if text.is_empty() {
            0
        } else {
            match send_and_wait_recorded(&mut client, text, Instant::now() + op_budget) {
                Ok(n) => n,
                Err(e) => {
                    client.shutdown(SHUTDOWN_WAIT);
                    return Err(e);
                }
            }
        };
        // `SOT_TEST_PACING_HOLD` (test-only, the `SOT_TEST_ACTIVATION_BARRIER`
        // convention): hold pacing to its full bound. Terminal output batches,
        // so a scripted test load cannot keep the screen changing every poll.
        let pacing_hold = std::env::var_os("SOT_TEST_PACING_HOLD").is_some();

        let pacing_deadline = Instant::now() + pacing_budget;
        let mut previous = current_lines(&client);
        let mut last_change_at = Instant::now();
        loop {
            let quiet_elapsed = !pacing_hold && Instant::now().duration_since(last_change_at) >= quiet_budget;
            if quiet_elapsed || Instant::now() >= pacing_deadline {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
            client.pump();
            let lines = current_lines(&client);
            if lines != previous {
                previous = lines;
                last_change_at = Instant::now();
            }
        }

        // Doc above: once the text is recorded, always Ok.
        let enter_sent = send_and_wait_recorded(&mut client, &[0x0d], Instant::now() + op_budget).is_ok();

        client.shutdown(SHUTDOWN_WAIT);
        Ok((n, enter_sent))
    }

    /// Current screen lines, top to bottom, trailing spaces trimmed —
    /// [`screen_of`]'s own shape, off an already-pumped client.
    fn current_lines(client: &Client) -> Vec<String> {
        let (_, cols) = client.screen().size();
        client.screen().rows(0, cols).map(|line| line.trim_end().to_string()).collect()
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
    use super::headless::{type_into, write_and_enter};
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

    #[test]
    fn write_and_enter_oversized_payload_is_refused_before_any_attach() {
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP + 1];
        let budget = Duration::from_secs(5);
        let err = write_and_enter(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, budget, budget, budget)
            .expect_err("oversized payload must be refused");
        assert_eq!(err.phase, "size");
        assert!(!err.submitted);
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

    // Field defect: a scratch daemon watched a project root that
    // CONTAINED its own state root, so the daemon's file watcher held
    // directory handles inside it; on Windows an open directory handle
    // blocks a rename, so `voyages\<uuid>` publication failed with a
    // sharing violation and the supervisor exited terminal 69. These
    // cover the predicate that now refuses that layout up front, at
    // capsule create and again right before every spawn.
    #[test]
    fn state_root_inside_project_true_when_a_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("project");
        let state_root = project_root.join("state").join("workspaces").join("ws-1");
        std::fs::create_dir_all(&state_root).unwrap();
        assert!(state_root_inside_project(&state_root, &project_root));
    }

    #[test]
    fn state_root_inside_project_true_when_equal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shared-root");
        std::fs::create_dir_all(&root).unwrap();
        assert!(state_root_inside_project(&root, &root));
    }

    #[test]
    fn state_root_inside_project_false_for_a_same_prefix_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("project");
        let state_root = dir.path().join("project-state"); // same prefix, NOT nested
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&state_root).unwrap();
        assert!(!state_root_inside_project(&state_root, &project_root));
    }

    #[test]
    #[cfg(unix)]
    fn state_root_inside_project_resolves_a_symlinked_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let real_project = dir.path().join("real-project");
        let state_root = real_project.join("state");
        std::fs::create_dir_all(&state_root).unwrap();
        let link = dir.path().join("project-link");
        std::os::unix::fs::symlink(&real_project, &link).unwrap();
        // The caller passes the SYMLINK as the project root; the state
        // root is a real descendant of what it resolves to.
        assert!(state_root_inside_project(&state_root, &link));
    }

    // Field defect (v0.6.0-rc.12): a supervisor that died out from under
    // a row (e.g. a daemon-pair converge that ended the old build's
    // supervisor) left the state dir holding only a lock FILE, and
    // `end_run` used to treat every unreachable lane identically -- kept
    // forever, with no way to tell a merely-unresponsive live holder
    // from no holder at all. `query_status` fails against this temp dir
    // in every case below (nothing is listening on its lane) -- decision
    // 33 needs BOTH the fence AND the leg independently proven absent
    // before `Unheld`; a missing pointer (nothing to check the leg
    // against), a present leg, or a still-held fence each keep the row
    // instead.
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn end_run_is_unheld_only_when_both_the_fence_and_the_leg_are_proven_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path();

        // No supervisor.lock AND no published pointer at all -- the
        // fence is free, but there is nothing to prove the leg absent
        // against: uncertain, never fabricated as `Unheld`.
        match end_run(state_dir, "test reason", true) {
            Err(_) => {}
            Ok(outcome) => panic!("expected Err with no pointer to check the leg against: {outcome:?}"),
        }

        // A published pointer naming a real voyage root whose own
        // `writer.lock` exists and is free -- BOTH halves now proven
        // absent.
        let voyage_id = "a1b2c3d4-e5f6-4890-9abc-def012345678";
        sot_log::pointer::publish(state_dir, voyage_id).expect("publish the pointer");
        let voyage_root = sot_log::supervisor::voyage_root_path(state_dir, voyage_id);
        std::fs::create_dir_all(&voyage_root).expect("voyage root");
        std::fs::write(voyage_root.join("writer.lock"), b"").expect("writer.lock file");
        match end_run(state_dir, "test reason", true) {
            Ok(super::EndRunOutcome::Unheld) => {}
            other => panic!("expected Ok(Unheld) with no authority and no leg: {other:?}"),
        }

        // A leg holds the voyage's own `writer.lock` -- reported
        // instead of silently orphaned.
        let leg = sot_log::lock_writer(&voyage_root.join("writer.lock")).expect("take the writer lock");
        match end_run(state_dir, "test reason", true) {
            Err(_) => {}
            Ok(outcome) => panic!("expected Err while a leg holds the row with no authority: {outcome:?}"),
        }
        drop(leg);

        // Something else holds the SUPERVISOR fence right now -- the
        // lock attempt must fail regardless of the leg, so `end_run`
        // keeps the unreachable-lane refusal (Err) rather than
        // fabricating `Unheld` out from under a live holder.
        let holder = sot_log::fence::lock_supervisor(state_dir).expect("take the fence");
        match end_run(state_dir, "test reason", true) {
            Err(_) => {}
            Ok(outcome) => {
                panic!("expected the unchanged unreachable-lane Err while the fence is held: {outcome:?}")
            }
        }
        drop(holder);
    }

    // The direct writer.lock cases (`leg_absent`'s own Ok(true)/Ok(false)
    // on a real held/free lock) are already exercised through `end_run`
    // by `end_run_is_unheld_only_when_both_the_fence_and_the_leg_are_proven_absent`
    // above (including the no-pointer-published Err case) -- this test
    // instead targets what `leg_absent` cannot organically produce on
    // Linux at all: the OTHER, non-contention refusal `lock_writer` can
    // report (Windows' reparse-point check, `fsutil.rs`) sharing the SAME
    // `Error::State` shape as genuine contention. `is_lock_contention` is
    // the pure predicate that tells them apart (Codex review,
    // 2026-09-11); this is its regression test -- pure string matching,
    // independent of any real lock file, but `is_lock_contention` itself
    // lives inside `mod runtime`, gated like every other function this
    // module's tests reach.
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn lock_contention_is_recognized_only_by_its_own_message() {
        assert!(
            is_lock_contention("lock held by another process: \"/tmp/x/writer.lock\""),
            "lock_writer's own bounded-retry-exhaustion text must be recognized as contention"
        );
        assert!(
            !is_lock_contention(
                "writer.lock at \"/tmp/x/writer.lock\" is a reparse point — refusing a redirected fence"
            ),
            "a reparse-point refusal is not contention -- absence must stay unproven, not read as \"a leg lives\""
        );
        assert!(!is_lock_contention("some unrelated State error"));
    }

    #[test]
    fn claude_recipe_places_extra_flags_before_the_skill() {
        assert_eq!(
            claude_recipe(false, &["--x".to_string()]),
            vec!["claude", "--permission-mode", "auto", "--x", "/sot-session-start"]
        );
    }

    #[test]
    fn claude_recipe_resume_keeps_continue_before_the_skill() {
        assert_eq!(
            claude_recipe(true, &[]),
            vec!["claude", "--permission-mode", "auto", "--continue", "/sot-session-start"]
        );
    }

    #[test]
    #[cfg(unix)]
    fn agent_exec_argv_claude_never_adds_continue() {
        // ADR 0046 decision 4's own invariant: `agent-exec` never adds
        // `--continue` itself (that stays the daemon's own default for a
        // capsule spawn, in `claude_argv`) — it builds the SAME recipe
        // shape with `resume: false` and the caller's own flags. Widened
        // from Linux-only to any Unix (CI fix): this is the exact case
        // that broke on macOS when `agent_exec_argv` routed resolution
        // through the capsule launcher's own, narrower refusal there.
        let _guard = self_file_env_guarded();
        let dir = tempfile::tempdir().expect("tempdir");
        let claude = dir.path().join("claude");
        std::fs::write(&claude, b"#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("PATH", dir.path());
        std::env::remove_var("HOME");
        let argv = agent_exec_argv(
            "claude",
            &["--continue".to_string(), "--x".to_string()],
        )
        .unwrap();
        // The caller MAY pass its own "--continue" through `extra` (as
        // `ccb --continue` does) -- `agent_exec_argv` never adds a SECOND
        // one of its own; this only asserts the recipe's fixed shape
        // around whatever the caller supplied.
        assert_eq!(
            argv,
            vec![
                claude.to_string_lossy().into_owned(),
                "--permission-mode".to_string(),
                "auto".to_string(),
                "--continue".to_string(),
                "--x".to_string(),
                "/sot-session-start".to_string(),
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn agent_exec_argv_none_is_refused_it_has_no_shared_recipe() {
        // "none" is a legitimate `agent_argv` kind (the bare platform
        // shell) but shares no `--permission-mode`/skill recipe shape
        // with "claude" -- `agent-exec` refuses it regardless of whether
        // PATH/HOME would resolve anything, so neither is touched here.
        let err = agent_exec_argv("none", &[]).unwrap_err();
        assert!(err.contains("none"), "got: {err}");
    }

    #[test]
    #[cfg(unix)]
    fn agent_exec_argv_unknown_kind_prints_agent_argvs_own_error() {
        assert_eq!(
            agent_exec_argv("bogus", &[]).unwrap_err(),
            agent_argv("bogus").unwrap_err()
        );
    }

    #[test]
    #[cfg(windows)]
    fn agent_argv_claude_matches_the_daemons_own_recipe() {
        // Renamed (ADR 0046 decision 4): `ccb` no longer bakes these
        // flags in itself -- it execs through `sotd agent-exec claude
        // "$@"`, which shares this SAME `claude_recipe` builder. This
        // still pins the Windows capsule-spawn shape (`claude_recipe(true,
        // &[])` verbatim, no absolute-path resolution -- Windows relies
        // on the daemon's own `PATH`, `claude_argv`'s own doc).
        assert_eq!(
            agent_argv("claude").unwrap(),
            vec!["claude", "--permission-mode", "auto", "--continue", "/sot-session-start"]
        );
    }

    #[test]
    #[cfg(unix)]
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
        assert!(agent_argv("bogus").is_err());
    }

    /// Guards PATH/HOME/SOT_COMM_HOME for one `agent_argv("codex")` call.
    #[cfg(unix)]
    fn with_codex_env<T>(path: Option<&std::path::Path>, home: Option<&std::path::Path>, comm_home: Option<&std::path::Path>, f: impl FnOnce() -> T) -> T {
        let _guard = self_file_env_guarded();
        let prior = (std::env::var_os("PATH"), std::env::var_os("HOME"), std::env::var_os("SOT_COMM_HOME"));
        match path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        match home {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        match comm_home {
            Some(p) => std::env::set_var("SOT_COMM_HOME", p),
            None => std::env::remove_var("SOT_COMM_HOME"),
        }
        let result = f();
        match prior.0 {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        match prior.1 {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prior.2 {
            Some(v) => std::env::set_var("SOT_COMM_HOME", v),
            None => std::env::remove_var("SOT_COMM_HOME"),
        }
        result
    }

    #[cfg(unix)]
    fn write_stub_ccx(path: &Path) {
        std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        set_executable(path);
    }

    #[test]
    #[cfg(unix)]
    fn agent_argv_codex_resolves_ccx_and_ends_in_capsule_continue() {
        let dir = tempfile_test_dir();
        let ccx = dir.path().join("ccx");
        write_stub_ccx(&ccx);

        let result = with_codex_env(Some(dir.path()), None, None, || agent_argv("codex"));
        assert_eq!(
            result.unwrap(),
            vec![ccx.to_string_lossy().into_owned(), "--capsule".to_string(), "--continue".to_string()]
        );
    }

    #[test]
    #[cfg(unix)]
    fn agent_argv_codex_fails_closed_when_nothing_resolves() {
        let result = with_codex_env(None, None, None, || agent_argv("codex"));
        assert!(result.is_err());
    }

    #[test]
    #[cfg(windows)]
    fn agent_argv_codex_is_refused_on_windows() {
        let err = agent_argv("codex").unwrap_err();
        assert!(err.contains("ccx"), "got: {err}");
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn resolve_claude_ignores_a_non_executable_file() {
        let dir = tempfile_test_dir();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, b"not a real program").unwrap(); // no +x
        let path_var = std::ffi::OsString::from(dir.path());
        let err = resolve_claude(Some(&path_var), None).unwrap_err();
        assert!(err.contains("claude not found"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn resolve_claude_names_every_directory_it_searched() {
        let err = resolve_claude(None, None).unwrap_err();
        assert!(err.contains("claude not found"), "{err}");
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
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

    #[test]
    #[cfg(unix)]
    fn resolve_ccx_finds_an_executable_on_path() {
        let dir = tempfile_test_dir();
        let ccx = dir.path().join("ccx");
        std::fs::write(&ccx, b"#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&ccx);
        let path_var = std::ffi::OsString::from(dir.path());
        assert_eq!(resolve_ccx(Some(&path_var), None).unwrap(), ccx.to_string_lossy());
    }

    #[test]
    #[cfg(unix)]
    fn resolve_ccx_falls_back_to_local_bin_under_home() {
        let dir = tempfile_test_dir();
        let local_bin = dir.path().join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let ccx = local_bin.join("ccx");
        std::fs::write(&ccx, b"#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&ccx);
        assert_eq!(resolve_ccx(None, Some(dir.path())).unwrap(), ccx.to_string_lossy());
    }

    #[test]
    #[cfg(unix)]
    fn resolve_ccx_names_every_directory_it_searched() {
        let err = resolve_ccx(None, None).unwrap_err();
        assert!(err.contains("ccx not found"), "{err}");
    }

    #[cfg(unix)]
    fn tempfile_test_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[cfg(unix)]
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
    fn first_leg_without_continue_only_on_start() {
        assert_eq!(first_leg_without_continue(StartMode::Start), ["--first-leg-without", "--continue"]);
        assert!(first_leg_without_continue(StartMode::Resume).is_empty());
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
                "NO_COLOR",
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
        sot_self_host: Option<std::ffi::OsString>,
        path: Option<std::ffi::OsString>,
    }

    impl Drop for SelfFileEnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("HOME", &self.home),
                ("USERPROFILE", &self.userprofile),
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("SOT_SELF_HOST", &self.sot_self_host),
                ("PATH", &self.path),
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
            sot_self_host: std::env::var_os("SOT_SELF_HOST"),
            path: std::env::var_os("PATH"),
            _serial: serial,
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn agent_env_prepends_local_bin_to_path_once() {
        // The ccb launcher's PATH rule, now the ONE shared function (ADR
        // 0046 decision 4): PATH with no ~/.local/bin gets it prepended
        // -- and a PATH that already reaches it is left alone (no
        // duplicate entry). Pure/DI'd (mirrors `resolve_claude`'s own
        // tests) -- no env mutation, no guard needed.
        let env = agent_env(
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(Path::new("/fake-home")),
        );
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("PATH"), Some("/fake-home/.local/bin:/usr/bin:/bin"));

        let env = agent_env(
            Some(std::ffi::OsStr::new("/fake-home/.local/bin:/usr/bin")),
            Some(Path::new("/fake-home")),
        );
        assert!(env.is_empty(), "already on PATH: nothing to stamp");
    }

    #[test]
    #[cfg(not(windows))]
    fn agent_env_is_empty_with_no_home_to_derive_it_from() {
        let env = agent_env(Some(std::ffi::OsStr::new("/usr/bin:/bin")), None);
        assert!(env.is_empty());
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
        //
        // ADR 0046 decision 1 (manager review, S2/S4): the leg ALSO
        // carries a bare SOT_SOCKET and SOT_WORKSPACE_ID — every
        // pane/capsule alike, so `comm-join.sh`'s `agent.join` can always
        // find its owner daemon. No SOT_SELF_HOST pin (an override on the
        // daemon's own process already reaches this child by inheritance).
        // `set_own_endpoint` is idempotent (the daemon binds exactly one
        // listener per process) and process-global (`OnceLock`), so this
        // pins the value itself here rather than trusting whatever another
        // test in this binary may have already set it to.
        crate::awareness::set_own_endpoint(Path::new("/fake-home/.local/state/sot/session.sock"));
        let _guard = self_file_env_guarded();
        std::env::set_var("SOT_SELF_HOST", "testhost");
        std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
        let env = capsule_supervisor_env(
            "ws-myrepo-1a2b",
            "myrepo",
            Path::new("/home/me/myrepo"),
            "myrepo-myhost",
        );
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SOT_WORKSPACE"), Some("myrepo"));
        assert_eq!(get("SOT_WORKSPACE_ID"), Some("ws-myrepo-1a2b"));
        assert_eq!(get("SOT_WORKSPACE_ROOT"), Some("/home/me/myrepo"));
        assert_eq!(get("SOT_COMM_NAME"), Some("myrepo-myhost"));
        assert_eq!(get("SOT_SESSION"), Some("1"));
        assert_eq!(get("SOT_HOST"), None, "no SOT_SELF_HOST pin -- inheritance carries an override, if any");
        if cfg!(unix) {
            // `OWN_ENDPOINT` is process-global (`OnceLock`) -- another test
            // in this binary may have already pinned it to a different
            // exact path, so this asserts the BARE SHAPE only (S4: no
            // typed unix:/pipe: prefix), never an exact value.
            assert!(
                get("SOT_SOCKET").is_some_and(|s| !s.starts_with("unix:") && !s.starts_with("pipe:")),
                "SOT_SOCKET must be a bare path, got {:?}",
                get("SOT_SOCKET")
            );
        } else {
            assert_eq!(get("SOT_SOCKET"), None, "S4: SOT_SOCKET is pinned on Unix only");
        }
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
        std::env::set_var("SOT_SELF_HOST", "testhost");
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
