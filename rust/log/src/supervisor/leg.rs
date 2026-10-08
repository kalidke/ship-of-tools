//! The leg: pointer discovery or mint, the spawn decision, the spawn lease and the leg command line.

use super::*;
use crate::store::voyage::SEG_DIR;

// ---------------------------------------------------------------------
// Pointer discovery / mint (supervisor startup only)
// ---------------------------------------------------------------------

pub(super) fn discover_or_mint_voyage(state_dir: &Path, mode: StartMode) -> crate::Result<String> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) => Ok(id),
        PointerState::NotFound => match mode {
            StartMode::Start => {
                std::fs::create_dir_all(voyages_dir(state_dir))?;
                let id = uuid::Uuid::now_v7().to_string();
                VoyageStore::bootstrap(
                    &voyage_root_path(state_dir, &id),
                    &id,
                    RetentionClass::Archive,
                )?;
                pointer::publish(state_dir, &id)?;
                Ok(id)
            }
            StartMode::Resume => Err(err_state(
                "--resume with no drawer.voyage pointer at all: nothing to resume",
            )),
        },
        PointerState::Corrupt => Err(err_state(
            "drawer.voyage is corrupt — run `sot-capsule reset`",
        )),
        PointerState::OtherIo(e) => Err(e.into()),
    }
}

/// The start-mode table's OWN "what to do about the latest leg" half,
/// consulted ONLY when no live capsule was adopted. Checks the marker on
/// an UNSEALED leg too, not only a sealed one: the marker is written
/// mid graceful-teardown, before sealing completes.
pub(super) fn should_spawn_after_absent(
    state_dir: &Path,
    voyage_id: &str,
    mode: StartMode,
) -> crate::Result<bool> {
    if mode == StartMode::Start {
        return Ok(true);
    }
    let seg_dir = voyage_root_path(state_dir, voyage_id).join(SEG_DIR);
    match recovery::latest_leg_state(&seg_dir).map_err(crate::Error::Io)? {
        LatestLegState::NoLeg => Ok(true),
        LatestLegState::Sealed { epoch } | LatestLegState::Unsealed { epoch } => {
            let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
            Ok(!marked)
        }
    }
}

pub(super) fn leg_epoch_of(state_dir: &Path, voyage_id: &str) -> Option<u64> {
    let seg_dir = voyage_root_path(state_dir, voyage_id).join(SEG_DIR);
    match recovery::latest_leg_state(&seg_dir) {
        Ok(LatestLegState::Unsealed { epoch }) => Some(epoch),
        _ => None,
    }
}

/// whether the CURRENT leg's own recorded
/// `producer_uptime_ms` (`verify::leg_producer_uptime_ms`) proves it
/// survived at least `STABILITY_INTERVAL` — the ONLY question the
/// anti-flap counter's reset now depends on. Fail-safe direction is
/// UNSTABLE (`false`) for every case that cannot prove otherwise:
/// no leg found, no `producer_dead` frame yet on it (unsealed, or a
/// spawn failure that never reached a real producer), the field absent,
/// or the segment itself unreadable (logged, but this is an anti-flap
/// HEURISTIC, not a safety-critical decision — never escalated past a
/// warning).
pub(super) fn leg_was_stable(state_dir: &Path, voyage_id: &str) -> bool {
    let seg_dir = voyage_root_path(state_dir, voyage_id).join(SEG_DIR);
    let epoch = match recovery::latest_leg_state(&seg_dir) {
        Ok(LatestLegState::Sealed { epoch } | LatestLegState::Unsealed { epoch }) => epoch,
        Ok(LatestLegState::NoLeg) => return false,
        Err(e) => {
            note(format_args!(
                "could not read this leg's own state to judge stability ({e}); counting it \
                 unstable (N1 fail-safe)"
            ));
            return false;
        }
    };
    match verify::leg_producer_uptime_ms(&seg_dir, voyage_id, epoch) {
        Ok(Some(ms)) => Duration::from_millis(ms) >= STABILITY_INTERVAL,
        Ok(None) => false,
        Err(e) => {
            note(format_args!(
                "could not read this leg's own producer_uptime_ms ({e}); counting it unstable \
                 (N1 fail-safe)"
            ));
            false
        }
    }
}

// `DETACHED_PROCESS`: `run` gets no console of its own. `supervise` has none
// either (the daemon spawns it detached — see `rows/spawn/detach.rs`), and a
// child of a console-less parent otherwise gets a brand-new console, which
// the user's default terminal adopts as a stray window. `run`'s ConPTY is a
// separate OS object for the agent child; its own stdio stays inherited.
// Windows only: no Unix analogue exists, or is needed — the producer's own
// `setsid` (ADR 0043 decision 14) is the whole detachment there.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// ADR 0043 decision 21: the fixed fd number `--parent-lease-fd` names —
/// `build_run_command`'s own `pre_exec` installs the lease's read end
/// here, in the CHILD, via `dup2` (or, in the one case where the read end
/// already IS this fd, by clearing `CLOEXEC` on it directly with
/// `fcntl`, since `dup2(fd, fd)` is specified as a no-op that would leave
/// the flag set and the lease would close at `exec`). Matches
/// `bin/sot-capsule.rs`'s own `run` arm parser.
#[cfg(unix)]
const PARENT_LEASE_FD: std::os::fd::RawFd = 3;

/// The parent-death lease handed to ONE freshly spawned leg (ADR 0043
/// decisions 15/21) — per platform, since the mechanism differs: Windows
/// passes the lease's own kernel-object NAME as a CLI argument (cheap to
/// clone per spawn); Unix passes an owned, dup'd read-end fd that
/// [`build_run_command`]'s own `pre_exec` installs at a fixed number in
/// the child. Produced fresh for EACH spawn by [`LegLease::for_spawn`] —
/// the supervisor's own [`LegLease`] is held once, for this process's
/// whole life; this is what it hands to every leg spawned from it.
#[cfg(windows)]
pub(super) struct SpawnLease(String);
#[cfg(unix)]
pub(super) struct SpawnLease(std::os::fd::OwnedFd);

/// The supervisor's own held parent-death lease — created ONCE, at
/// startup, and kept for this process's whole life (ADR 0043 decisions
/// 15/21). Windows: exactly today's named, owned mutex. Unix: a
/// close-on-exec pipe — this process holds the WRITE end for its whole
/// life and NEVER writes to it (the read end going broken/closed, from
/// this end's own perspective, is never observed by this process at all;
/// it is the LEG's own signal, read once, non-blocking, right after it
/// acquires the writer fence — decision 15).
#[cfg(windows)]
pub(super) struct LegLease {
    name: String,
    _lease: crate::supervisor::lease_win::Lease,
}
#[cfg(unix)]
pub(super) struct LegLease {
    read: std::os::fd::OwnedFd,
    _write: std::os::fd::OwnedFd,
}

impl LegLease {
    #[cfg(windows)]
    pub(super) fn create(h: &str) -> std::io::Result<Self> {
        let name = crate::supervisor::lease_win::lease_name(h, std::process::id());
        let _lease = crate::supervisor::lease_win::create(&name)?;
        Ok(Self { name, _lease })
    }

    /// `_h` is unused on Unix — the pipe is anonymous, unlike Windows'
    /// named mutex, so there is nothing here for a state-dir hash to
    /// scope; kept as a parameter so both platforms share one call site
    /// in `supervise_inner`.
    ///
    /// The parent-death lease uses pipe2(O_CLOEXEC) on Linux and checked pipe plus fcntl on macOS. The macOS
    /// creation-to-flagging window remains; lane binding already started transport threads, so placement does not
    /// prove single-threaded creation. `O_CLOEXEC` on BOTH ends is the whole correctness of the lease: a write end
    /// leaked past an `exec` into ANY child keeps the pipe open after this process dies, and the leg then never sees
    /// its parent go. The `FD_CLOEXEC` pass below is written unconditionally rather than `cfg`-split: on Linux it
    /// re-asserts what `pipe2` already did, so the invariant is checked in code on every platform.
    #[cfg(unix)]
    pub(super) fn create(_h: &str) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let mut fds = [0i32; 2];
        #[cfg(target_os = "linux")]
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        #[cfg(not(target_os = "linux"))]
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the `pipe`/`pipe2` above just returned two freshly
        // opened, valid, uniquely-owned fds on success. Owned FIRST, so
        // an `fcntl` failure below closes both rather than leaking them.
        let read = unsafe { std::os::fd::OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { std::os::fd::OwnedFd::from_raw_fd(fds[1]) };
        crate::lane::test_progress::birth("lease.read", read.as_raw_fd());
        crate::lane::test_progress::birth("lease.write", write.as_raw_fd());
        for fd in [read.as_raw_fd(), write.as_raw_fd()] {
            if let Some(injected) = crate::lane::test_progress::flag_call() {
                return Err(injected);
            }
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(injected) = crate::lane::test_progress::flag_call() {
                return Err(injected);
            }
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self {
            read,
            _write: write,
        })
    }

    /// A fresh handle for ONE spawn attempt — Windows clones the (cheap)
    /// name string; Unix dups the read end, since the fd
    /// `build_run_command`'s own `pre_exec` installs into the child is
    /// consumed by THAT leg's own fd table, independent of every other
    /// leg this same lease will ever be handed to.
    pub(super) fn for_spawn(&self) -> std::io::Result<SpawnLease> {
        #[cfg(windows)]
        {
            Ok(SpawnLease(self.name.clone()))
        }
        #[cfg(unix)]
        {
            Ok(SpawnLease(self.read.try_clone()?))
        }
    }
}

// ADR 0042 slice L1a added `survival` as an 8th parameter — matching this file's own existing precedent
// (`run_quit`-equivalent lane loops) for a constructor whose every
// parameter is load-bearing and independently documented at its call
// sites, rather than a struct that would only exist to satisfy this lint.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_run_command(
    capsule_exe: &Path,
    voyage_root: &Path,
    voyage_id: &str,
    cols: u16,
    rows: u16,
    lease: &SpawnLease,
    survival: Survival,
    producer_argv: &[String],
) -> std::process::Command {
    let survival_flag = match survival {
        Survival::Normal => "normal",
        Survival::Degraded => "degraded",
    };
    // Legs fork from THIS process, so before their own exec resolves it,
    // `/proc/self/exe` still names the supervisor's own running inode --
    // immune to an `sot-apply` rename-over-the-path (ADR 0043 decision
    // 33's retirement clause). `argv[0]` is set to the real resolved
    // path regardless, so `ps`/`pgrep -f` still find the leg by it (a
    // magic-symlink program path has no bearing on what `ps` prints).
    // Windows has no such handle: the leg is spawned from the path, and
    // `VoyageMgmtExchange` (the supervisor<->leg management exchange,
    // `exchange.rs`) is NOT versioned -- permanently pinned `SOM0`, no
    // `proto` negotiation or build gate -- so a cross-build supervisor/
    // leg pair is unsupported there until that exchange is versioned.
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut c = std::process::Command::new("/proc/self/exe");
        c.arg0(capsule_exe);
        c
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = std::process::Command::new(capsule_exe);
    command
        .arg("run")
        .arg(voyage_root)
        .arg(voyage_id)
        .arg("--cols")
        .arg(cols.to_string())
        .arg("--rows")
        .arg(rows.to_string());
    #[cfg(windows)]
    {
        command.arg("--parent-lease-name").arg(&lease.0);
    }
    #[cfg(unix)]
    {
        // ADR 0043 decision 21: the read end reaches the leg as the
        // FIXED fd `PARENT_LEASE_FD`, installed by this `pre_exec` --
        // `Command` opens everything else `CLOEXEC` by default, so
        // nothing but this one fd leaks into the child. Async-signal-
        // safe only (this closure runs between `fork` and `exec`): every
        // call below is.
        use std::os::fd::AsRawFd;
        let read_fd = lease.0.as_raw_fd();
        command
            .arg("--parent-lease-fd")
            .arg(PARENT_LEASE_FD.to_string());
        unsafe {
            command.pre_exec(move || {
                if read_fd == PARENT_LEASE_FD {
                    // `dup2(fd, fd)` is specified as a no-op that leaves
                    // `FD_CLOEXEC` untouched -- exactly the one case
                    // `dup2` below does NOT clear the flag for us, so it
                    // is cleared directly here instead.
                    let flags = libc::fcntl(read_fd, libc::F_GETFD);
                    if flags < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(read_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                } else if libc::dup2(read_fd, PARENT_LEASE_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
        .arg("--survival")
        .arg(survival_flag)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(producer_argv);
    #[cfg(windows)]
    command.creation_flags(DETACHED_PROCESS);
    command
}

/// The argv of the next leg: `first_leg_without` is stripped while no producer
/// has run in this process (`producer_ran`) and after an unstable leg, so a
/// first leg that exited before running anything (a storage exit, say) is
/// respawned with its first-leg argv, and a healthy row that restarts keeps the
/// argv it was never wrong to keep.
pub(super) fn leg_argv(config: &SuperviseConfig, producer_ran: bool, unstable: bool) -> Vec<String> {
    if unstable || !producer_ran {
        strip_first_leg_tokens(&config.producer_argv, &config.first_leg_without)
    } else {
        config.producer_argv.clone()
    }
}

/// `producer_argv` with every element equal to one of `tokens` removed —
/// called at the very first leg this process ever spawns (unconditionally)
/// and, via `respawn_or_terminal`, at any later leg that follows one
/// classified unstable. Never called for a leg after a reset or for a
/// later voyage's own first spawn — those sites clone `producer_argv`
/// directly.
pub(super) fn strip_first_leg_tokens(producer_argv: &[String], tokens: &[String]) -> Vec<String> {
    if tokens.is_empty() {
        return producer_argv.to_vec();
    }
    producer_argv
        .iter()
        .filter(|a| !tokens.contains(a))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_or_mint_voyage_start_mints_a_fresh_voyage_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == id));
        assert!(voyage_root_path(dir.path(), &id).exists());
    }

    #[test]
    fn discover_or_mint_voyage_resume_refuses_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_or_mint_voyage(dir.path(), StartMode::Resume).is_err());
    }

    #[test]
    fn discover_or_mint_voyage_returns_the_existing_id_when_valid() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let again = discover_or_mint_voyage(dir.path(), StartMode::Resume).unwrap();
        assert_eq!(id, again);
    }

    #[test]
    fn discover_or_mint_voyage_refuses_a_corrupt_pointer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(pointer::pointer_path(dir.path()), b"not-a-uuid").unwrap();
        assert!(discover_or_mint_voyage(dir.path(), StartMode::Start).is_err());
        assert!(discover_or_mint_voyage(dir.path(), StartMode::Resume).is_err());
    }

    #[test]
    fn should_spawn_after_absent_start_mode_always_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Start).unwrap());
    }

    #[test]
    fn should_spawn_after_absent_resume_with_no_leg_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Resume).unwrap());
    }

    /// A lease end is close-on-exec at publication on every Unix and born so where `pipe2` exists.
    #[cfg(unix)]
    fn lease_end_is_close_on_exec(role: &str) {
        use crate::lane::test_progress::observe_births;
        let (lease, births, _) = observe_births(None, || LegLease::create("h").expect("the lease"));
        let birth = births
            .iter()
            .find(|b| b.role == role)
            .unwrap_or_else(|| panic!("no {role} descriptor: {births:?}"));
        #[cfg(target_os = "linux")]
        assert!(
            birth.fd_flags >= 0 && birth.fd_flags & libc::FD_CLOEXEC != 0,
            "descriptor missing FD_CLOEXEC at birth: {birth:?}"
        );
        let now = unsafe { libc::fcntl(birth.fd, libc::F_GETFD) };
        assert!(
            now >= 0 && now & libc::FD_CLOEXEC != 0,
            "{role} published without FD_CLOEXEC"
        );
        drop(lease);
    }

    #[cfg(unix)]
    #[test]
    fn lease_read_is_close_on_exec_at_birth_and_publication() {
        lease_end_is_close_on_exec("lease.read");
    }

    #[cfg(unix)]
    #[test]
    fn lease_write_is_close_on_exec_at_birth_and_publication() {
        lease_end_is_close_on_exec("lease.write");
    }

    /// Each flag call of the lease's checked pass fails for real through `create`, which then closes both ends.
    #[cfg(unix)]
    #[test]
    fn a_lease_flag_failure_closes_both_ends() {
        if !crate::test_isolated::run_isolated(
            "supervisor::leg::tests::a_lease_flag_failure_closes_both_ends",
        ) {
            return;
        }
        use crate::lane::test_progress::observe_births;
        for call in 1..=4 {
            let (made, births, calls) = observe_births(Some(call), || LegLease::create("h"));
            assert!(made.is_err(), "lease flag failure {call} was not returned");
            assert_eq!(births.len(), 2);
            assert!(
                calls >= call,
                "the injected call {call} was never made ({calls})"
            );
            for birth in births {
                let rc = unsafe { libc::fcntl(birth.fd, libc::F_GETFD) };
                assert!(
                    rc < 0,
                    "{} (fd {}) was left open after failure {call}",
                    birth.role,
                    birth.fd
                );
            }
        }
        let (made, _, calls) = observe_births(Some(5), || LegLease::create("h"));
        assert!(
            made.is_ok() && calls == 4,
            "the checked pass makes four flag calls, made {calls}"
        );
        eprintln!("lease-flag-proof calls=4 failures=4 bodies=1");
    }
}
