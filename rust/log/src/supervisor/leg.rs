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
    /// The parent-death lease. `O_CLOEXEC` on BOTH ends is the whole correctness of the lease: a write end leaked past
    /// an `exec` into ANY child keeps the pipe open after this process dies, and the leg then never sees its parent
    /// go. [`crate::host::fd::pipe_cloexec`] is why neither end can be caught before its flag.
    #[cfg(unix)]
    pub(super) fn create(_h: &str) -> std::io::Result<Self> {
        let (read, write) = crate::host::fd::pipe_cloexec(["lease.read", "lease.write"], false)?;
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

/// In the leg's child between `fork` and `exec`: sweeps every inherited descriptor close-on-exec, then installs the lease's
/// read end at `PARENT_LEASE_FD` with its flag clear. Async-signal-safe only.
#[cfg(unix)]
fn install_lease(read_fd: std::os::fd::RawFd) -> std::io::Result<()> {
    crate::host::inherit_none()?;
    if read_fd == PARENT_LEASE_FD {
        // `dup2(fd, fd)` is specified as a no-op that leaves
        // `FD_CLOEXEC` untouched -- exactly the one case
        // `dup2` below does NOT clear the flag for us, so it
        // is cleared directly here instead.
        let flags = unsafe { libc::fcntl(read_fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(read_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    } else if unsafe { libc::dup2(read_fd, PARENT_LEASE_FD) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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
        // FIXED fd `PARENT_LEASE_FD`, installed by this `pre_exec`. The
        // sweep (`inherit_none`) runs first and marks every descriptor
        // above stdio close-on-exec, so the leg keeps exactly stdio and
        // the lease: `Command` opens its own descriptors CLOEXEC, but on
        // macOS a socket or pipe made by another thread gets the flag by
        // a second call, and an exec between the two would keep it.
        use std::os::fd::AsRawFd;
        let read_fd = lease.0.as_raw_fd();
        command
            .arg("--parent-lease-fd")
            .arg(PARENT_LEASE_FD.to_string());
        unsafe {
            command.pre_exec(move || install_lease(read_fd));
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

/// The argv of the next leg, the one place every leg's argv is chosen: `first_leg_only` is kept while no producer
/// has run in this process (`producer_ran`), so a first leg that exited before running anything (a storage exit,
/// say) is retried with it, and stripped from every leg after: a respawn, a leg after a reset, a later voyage.
pub(super) fn leg_argv(config: &SuperviseConfig, producer_ran: bool) -> Vec<String> {
    if producer_ran {
        strip_first_leg_tokens(&config.producer_argv, &config.first_leg_only)
    } else {
        config.producer_argv.clone()
    }
}

/// `producer_argv` with every element equal to one of `tokens` removed (`leg_argv`, once a producer has run).
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
#[path = "leg_tests.rs"]
mod tests;
