//! `impl Producer for PtyProducer`: the nine verbs the writer loop calls, over the pty, process group and held
//! slave that `mod.rs` sets up. A child of `pty`, so it sees `PtyProducer`'s private fields and helpers.

use super::*;

impl Producer for PtyProducer {
    type Output = File;

    fn pre_spawn_detail() -> serde_json::Value {
        // Decision 11: Unix contributes nothing to `producer_spawn.detail`
        // (Windows's `spawning_process_was_jobbed` has no Unix analogue).
        json!({})
    }

    #[allow(
        clippy::too_many_lines,
        reason = "spawns the pty child at the given size; predates the 100-line limit"
    )]
    fn spawn(argv: &[String], cols: u16, rows: u16) -> Result<Self> {
        if argv.is_empty() {
            return Err(Error::State("capsule argv is empty".into()));
        }
        // A DELIBERATE, DOCUMENTED deviation from a literal "just call
        // `Command::spawn`" implementation (reported per the brief's own
        // instruction for an impossible-as-written combination): the
        // `close_range` call in `pre_exec` below closes EVERY fd >= 3 in the CHILD before `execve` is
        // ever attempted — including the anonymous, `CLOEXEC`-marked pipe
        // `std::process::Command`'s OWN fallback fork+exec path uses to
        // report a POST-FORK failure (a failed `execve` included) back to
        // this call's `Result`, at whatever fd number it happens to land
        // on (an unavoidable, undocumented implementation detail no
        // `pre_exec` closure can see or protect). Verified empirically
        // (a minimal repro: `Command::new("/nonexistent").pre_exec(||
        // { close every fd 3..256; Ok(()) })` closes it first here too):
        // with the pipe closed, a real `execve` failure never reaches the
        // parent as `Err` at all -- the child instead aborts (`SIGABRT`)
        // when `std`'s own error-reporting `write` fails its internal
        // assertion, which `wait()` observes as an ordinary (if violent)
        // process exit, not a spawn failure. Neither property may be
        // silently dropped: the close of every fd >= 3 (the pty fds and a
        // killed holder's lock copy) and `ExitKind::SpawnFailed` must
        // stay honest (`tests/capsule/`'s own
        // `spawn_failure_is_compensated_unix`, the Unix twin of the
        // Windows spawn-failure test). The fix keeps BOTH: resolve
        // argv[0] (PATH search, exactly `execvp`'s own algorithm, if it
        // has no `/`) and probe it with `access(X_OK)` -- the same check
        // `execve` itself performs -- BEFORE ever forking, so the common
        // "doesn't exist" / "not executable" cases are caught here,
        // honestly, synchronously, with `close_range` never in the
        // picture for them at all. This does not close every possible
        // `execve`-time race (a TOCTOU deletion between this check and
        // the fork, or an exotic exec-time failure like a corrupt ELF
        // header) -- those residual cases would still surface as the
        // aborted-child shape above, exactly as they did before this
        // fix, but they are far rarer than "the path is simply wrong,"
        // which is what every realistic caller (and this crate's own
        // test) actually exercises.
        if !executable_is_resolvable(&argv[0]) {
            return Err(Error::Io(io::Error::from_raw_os_error(libc::ENOENT)));
        }
        // Captured BEFORE `Command::spawn` and moved
        // into `pre_exec` — the child's own first act verifies against
        // THIS value (`getppid()` at that point), not against whatever
        // `getppid()` a later, racing read of `std::process::id()` might
        // return. Linux-only, matching the PDEATHSIG check itself: no
        // portable non-Linux-unix equivalent exists.
        #[cfg(target_os = "linux")]
        let expected_ppid = std::process::id() as libc::pid_t;
        // Establish the disposition the pid PIN
        // depends on, rather than assuming it. `wait`'s own
        // `waitid(.., WNOWAIT)` (see the module doc's third point) needs
        // a RETAINED zombie to observe; if `SIGCHLD` is `SIG_IGN` (or
        // `SA_NOCLDWAIT` is set) the kernel auto-reaps a terminated child
        // itself, with no zombie ever left to find (`waitid` then reports
        // `ECHILD`) — and the pgid this producer's own `Drop` later
        // signals could already have been recycled to something
        // unrelated. `SIG_IGN` is inherited across `exec` from ANY
        // supervisor this process happens to run under, so establishing
        // `SIG_DFL` here, ourselves, before the fork, is the only way to
        // be sure. `ECHILD` from `waitid` after this stays a real error
        // (`observe_exit_without_reaping`'s own doc): it would now mean a
        // FOREIGN reaper raced us, a genuine invariant violation, never
        // routine.
        if unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        let mut master_fd: libc::c_int = -1;
        let mut slave_fd: libc::c_int = -1;
        // Geometry at spawn (the loop already validated it, 2x2..512x256 —
        // `Producer::spawn`'s own doc). `winsz` is passed as `&mut` so the
        // SAME call site satisfies both `openpty`'s Linux signature
        // (`winp: *const winsize`) and its non-Linux-unix one (`*mut
        // winsize`, e.g. macOS/BSD) — a `&mut T` coerces to either.
        let mut winsz = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut winsz,
            )
        };
        if rc != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
        // Both PTY ends are already owned. Check close-on-exec before publication; either flag-call failure closes both
        // ends. The openpty-to-flagging inheritance window remains on Linux and macOS. Our child's pre_exec installs
        // slave stdio and closes its inherited PTY copies; the parent keeps its slave for the run.
        for (end, fd) in [("master", master.as_raw_fd()), ("slave", slave.as_raw_fd())] {
            let flags = flag_fcntl(end, fd, libc::F_GETFD, 0).map_err(Error::Io)?;
            flag_fcntl(end, fd, libc::F_SETFD, flags | libc::FD_CLOEXEC).map_err(Error::Io)?;
        }

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        // The pty owner declares the terminal it emulates — the same two
        // lines the daemon's own tmux spawn and the frontend's drawer pty
        // set. Nothing upstream can: the supervisor inherits the daemon's
        // environment, and a daemon under a service manager has no TERM
        // at all, so without this the agent ran colourless (a white
        // status line, no markup) in every capsule row. The frontend's
        // own vt100 renders 256-colour and RGB, so both claims are true.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // ...and `NO_COLOR` does not get to contradict those two lines.
        // It is dropped here for the same reason they are set: this
        // process OWNS the terminal the producer talks to, so it is the
        // only party in a position to say what that terminal can do. A
        // `NO_COLOR` that arrives here is ambient inheritance, never a
        // statement about this row — a daemon relaunched from inside a
        // capsule hands its whole environment to the next daemon, and
        // every row's agent under it then ran colourless for no reason
        // (field report, 2026-09-17). The daemon scrubs it at the
        // supervisor spawn already; this is the LAST hop, and the only
        // one that also covers a leg started by hand, by a test
        // harness, or by a future spawn path that does not pass through
        // that scrub. A row that genuinely wants a colourless agent has
        // to say so under a name inheritance cannot forge — its own
        // producer argv, or a dedicated `SOT_*` variable translated back
        // into `NO_COLOR` right here — and since no such knob exists
        // today, none is invented here for nobody to set.
        cmd.env_remove("NO_COLOR");
        let slave_raw = slave.as_raw_fd();
        // SAFETY: this closure runs on the forked child, between fork and
        // exec — only async-signal-safe calls, per `pre_exec`'s own
        // contract. Every call below is.
        unsafe {
            cmd.pre_exec(move || {
                // PDEATHSIG armed FIRST, before
                // anything else — closes the window in which the
                // spawning THREAD could die before arming ever ran. Then
                // verify the parent is STILL the one that forked us: if
                // `getppid()` differs from `expected_ppid`, the parent
                // already died in the fork-to-arm gap and this process
                // was reparented (to a subreaper) before arming could
                // even take effect against a live parent — exit rather
                // than run on, unsupervised and undetected.
                #[cfg(target_os = "linux")]
                {
                    if libc::syscall(
                        libc::SYS_prctl,
                        PR_SET_PDEATHSIG as libc::c_long,
                        libc::SIGKILL as libc::c_long,
                        0i64,
                        0i64,
                        0i64,
                    ) < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != expected_ppid {
                        libc::_exit(1);
                    }
                }
                // Every other unix gets NO arm here, by decision and
                // not by omission: the pty this producer is built on
                // hangs up when the last MASTER fd closes, and this
                // process holds the only ones there are — on any exit
                // path it can take, `SIGKILL` included. See the module
                // doc's own "macOS gets NO twin" point for the argument
                // and for the macOS CI test that pins it.
                // New session;
                // slave becomes the controlling tty; stdio on it.
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                for fd in 0..=2 {
                    if libc::dup2(slave_raw, fd) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                // Close EVERY inherited fd ≥ 3 before exec. O_CLOEXEC
                // alone is not enough: between fork and exec this child
                // holds copies of all parent fds, and a flock lives on
                // the open file description. A dropped guard is unlocked
                // by `WriterLock`'s Drop, so this window no longer holds a
                // live holder's lock; it still holds a KILLED holder's
                // (no Drop runs) until exec. `close_range` severs those
                // references at the earliest point — including this
                // child's own inherited copies of the master and the
                // slave the PARENT is about to keep held (decision 12):
                // nothing from either leaks into the producer.
                #[cfg(target_os = "linux")]
                {
                    if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                // Non-Linux unix (ADR 0043 "Decisions for LU2" LU2b): no
                // `close_range` syscall exists, so a bounded loop from fd
                // 3 to the process's own descriptor-table limit does the
                // same job — a `close` on an fd that was never open is a
                // harmless `EBADF`, ignored (mirrors `close_range`'s own
                // "gaps are fine" semantics).
                #[cfg(not(target_os = "linux"))]
                {
                    let limit = libc::getdtablesize();
                    if limit > 3 {
                        for fd in 3..limit {
                            libc::close(fd);
                        }
                    }
                }
                Ok(())
            });
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "the capsule starts the row's agent inside the capsule's own containment (ADR 0041, ADR 0043)"
        )]
        let child = cmd.spawn().map_err(Error::Io)?;
        let pid = child.id() as libc::pid_t;
        // From here on this producer tracks the leader ITSELF, via raw
        // `waitid`/`waitpid` on `pid` — `Child`'s own `wait`/`try_wait`
        // are never called (they would REAP, which decision 13/14's own
        // fix specifically defers to `Drop`, exactly once, last). `Child`
        // holds no other resource worth keeping (no piped stdio was ever
        // requested), so dropping it here is inert.
        drop(child);

        let writer = File::from(master.try_clone().map_err(Error::Io)?);
        Ok(Self {
            writer,
            reader_fd: Some(master),
            slave: Some(slave),
            exit: Mutex::new(None),
            pid,
        })
    }

    fn take_output(&mut self) -> Self::Output {
        let fd = self
            .reader_fd
            .take()
            .expect("PtyProducer::take_output called twice");
        File::from(fd)
    }

    fn input(&mut self) -> &mut dyn Write {
        &mut self.writer
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let winsz = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // Any fd referencing the master satisfies `TIOCSWINSZ` — the
        // property lives on the pty pair, not on which duplicate fd asks
        // for it — so the already-open write side needs no extra dup.
        let rc = unsafe { libc::ioctl(self.writer.as_raw_fd(), libc::TIOCSWINSZ as _, &winsz) };
        if rc != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(())
    }

    fn wait(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut guard = self.exit.lock().unwrap();
                if guard.is_some() {
                    return Ok(true);
                }
                if let Some(exit) = self.observe_exit_without_reaping()? {
                    *guard = Some(exit);
                    return Ok(true);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(
                Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus> {
        // The trait's own doc allows confirmation
        // via EITHER `wait` or `domain_is_empty` -- a caller that
        // confirmed only through the latter would never have populated
        // this cache at all. FIX: observe once, on demand, exactly
        // like `wait` itself does (`waitid(.., WNOWAIT)`, never
        // reaping); if that ALSO finds nothing (a genuine precondition
        // violation by the caller), return a loud `Err`, never panic.
        let mut guard = self.exit.lock().unwrap();
        if guard.is_none() {
            *guard = self.observe_exit_without_reaping()?;
        }
        guard.ok_or_else(|| {
            Error::State(
                "PtyProducer: exit status requested before the leader's exit was confirmed".into(),
            )
        })
    }

    fn terminate_domain(&self) -> Result<()> {
        // ESRCH (decision 14) and EPERM: neither is a failure of this
        // call, and NEITHER is evidence of emptiness. Both mean "nothing
        // in this group could be signalled by me" -- Linux picks ESRCH
        // for a group whose only member is the leader zombie, Darwin
        // picks EPERM (POSIX permits either for "no process could be
        // signalled"), and EPERM ALSO covers a live member this uid may
        // not signal (an agent shell that ran `sudo`). This call is a
        // REQUEST; `domain_is_empty` is the only judge of what survived
        // it -- so tolerating EPERM here cannot seal anything: a live
        // member simply keeps the reap poll running until its own
        // deadline fires.
        if unsafe { libc::killpg(self.pid, libc::SIGKILL) } != 0 {
            let err = io::Error::last_os_error();
            if !matches!(err.raw_os_error(), Some(libc::ESRCH) | Some(libc::EPERM)) {
                return Err(Error::Io(err));
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    /// Live-member scan (decision 13/14):
    /// a zombie leader (deliberately unreaped until `Drop`, per the
    /// module doc) or a zombie descendant must NOT count against
    /// emptiness — only `/proc`'s own per-TASK state field reliably
    /// distinguishes "exited, awaiting reap" (`Z`) and the rarer
    /// post-exit "dead" (`X`) from anything that could still run.
    /// Round 2 found two real false-empties in the first version (which
    /// used `read_to_string` and judged each MEMBER by its own single
    /// state field): (i) a process whose MAIN thread alone has exited
    /// (`pthread_exit`) shows `Z` in its own `/proc/<pid>/stat` while its
    /// worker threads keep running — judged live here iff ANY of its
    /// tasks (`/proc/<pid>/task/*/stat`) has a state that is not `Z`/`X`;
    /// (ii) a `comm` containing a non-UTF-8 byte made `read_to_string`
    /// fail outright, silently skipping the whole entry — fixed by
    /// reading every stat file as raw BYTES (`std::fs::read`) and only
    /// ever decoding the small numeric/single-byte fields this method
    /// actually needs, never `comm` itself. A stat file or task directory
    /// that vanishes mid-scan is simply one fewer member/task to find,
    /// not a failure. ASSUMPTION this method relies on: the capsule and
    /// its producer share ONE pid namespace and ONE `/proc` — true here
    /// because the producer is this process's own direct fork child, and
    /// `hidepid` (where configured) never hides a uid's own processes
    /// from itself. macOS asks the same question through libproc; see
    /// this method's own sibling arm below.
    fn domain_is_empty(&self) -> Result<bool> {
        for entry in std::fs::read_dir("/proc").map_err(Error::Io)? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue; // not a pid directory (self, thread-self, etc.)
            }
            // A process may have exited between `read_dir`'s own listing
            // and this read -- that is simply one fewer member to find,
            // not a failure.
            let Ok(stat) = std::fs::read(format!("/proc/{name}/stat")) else {
                continue;
            };
            let Some(mut fields) = stat_fields_from_field_3(&stat) else {
                continue;
            };
            let Some(_state) = fields.next() else {
                continue;
            }; // field 3 -- NOT trusted alone, see below
            let Some(_ppid) = fields.next() else { continue }; // field 4
            let Some(pgrp) = fields.next().and_then(parse_pid_field) else {
                continue;
            }; // field 5
            if pgrp != self.pid {
                continue;
            }
            // This IS a member of our pgid -- its own (possibly
            // main-thread-only) state is not the last word; a live task
            // anywhere in the process counts.
            if any_task_is_live(name) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[cfg(target_os = "macos")]
    /// Live-member scan, the macOS twin of the Linux `/proc` scan above
    /// and with the same meaning (decision 13/14): empty iff no member of
    /// this process group is in a state that could still run.
    /// `proc_listpgrppids` is the native form of the query the Linux arm
    /// has to synthesise by walking `/proc` and comparing field 5, and
    /// `PROC_PIDTBSDINFO`'s `pbi_status` is its state field. A zombie
    /// leader this producer deliberately has not reaped (module doc) is
    /// correctly excluded, which the `killpg(pgid, 0)` probe this
    /// replaces could not do on Darwin AT ALL: Darwin answers that probe
    /// with EPERM both for a zombie-only group AND for a live member this
    /// uid may not signal (a `sudo` descendant), and no widening of a
    /// signalling errno can tell those two apart -- reading it as empty
    /// would seal a capsule over a live process in its own domain.
    /// FAIL CLOSED throughout: any member we cannot classify, and any
    /// listing we cannot complete, counts as LIVE, never as absent, so
    /// the worst case is teardown's own reap deadline firing loudly
    /// rather than a record sealed over a survivor.
    fn domain_is_empty(&self) -> Result<bool> {
        // The group can grow between sizing and listing, so a listing
        // that exactly fills its buffer is not provably complete: ask
        // again with more slack. Bounded -- a group that keeps outrunning
        // the buffer is a group with members, which is the answer anyway.
        const LISTING_ATTEMPTS: usize = 4;
        const SLACK_MEMBERS: usize = 16;

        let mut pids: Vec<libc::pid_t> = Vec::new();
        let mut listed = None;
        for _ in 0..LISTING_ATTEMPTS {
            // A NULL buffer sizes the listing (libproc's own idiom); a
            // negative return is a real failure and is NEVER Ok(true).
            let sized = unsafe { libc::proc_listpgrppids(self.pid, std::ptr::null_mut(), 0) };
            if sized < 0 {
                return Err(Error::Io(io::Error::last_os_error()));
            }
            let members = sized as usize / std::mem::size_of::<libc::pid_t>();
            pids.clear();
            pids.resize(members + SLACK_MEMBERS, 0);
            let bytes = std::mem::size_of_val(pids.as_slice()) as libc::c_int;
            let filled = unsafe {
                libc::proc_listpgrppids(self.pid, pids.as_mut_ptr().cast::<libc::c_void>(), bytes)
            };
            if filled < 0 {
                return Err(Error::Io(io::Error::last_os_error()));
            }
            if filled == bytes {
                continue; // buffer filled exactly -- the group may have outgrown it
            }
            listed = Some(filled as usize / std::mem::size_of::<libc::pid_t>());
            break;
        }
        let Some(listed) = listed else {
            return Ok(false); // could not complete a listing: fail closed
        };

        for &pid in &pids[..listed] {
            if pid <= 0 {
                continue; // padding, never a member
            }
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let want = std::mem::size_of::<libc::proc_bsdinfo>();
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    std::ptr::addr_of_mut!(info).cast::<libc::c_void>(),
                    want as libc::c_int,
                )
            };
            if got == want as libc::c_int {
                if info.pbi_status == libc::SZOMB {
                    continue; // exited, awaiting reap -- it can never run again
                }
                return Ok(false);
            }
            // The member exited between the listing and this query: one
            // fewer member to find, exactly as the Linux arm treats a
            // stat file that vanishes mid-scan. Anything else (EPERM on a
            // foreign-uid member, a short read) leaves it unclassified,
            // and an unclassified member is a live one.
            if io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    /// Decision 16's shape, applied here: a platform with no way to
    /// ENUMERATE a process group has no honest answer to this question,
    /// so it refuses rather than guessing from a signalling errno --
    /// emptiness is judged by one mechanism and that mechanism lists
    /// members. Unreachable in practice: `capsule::run` already fails
    /// closed for these targets at its own top, before any producer
    /// exists. This deletes a wrong answer; it removes no capability.
    fn domain_is_empty(&self) -> Result<bool> {
        Err(Error::Unsupported(
            "domain_is_empty: this unix cannot enumerate a process group",
        ))
    }

    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        // Dropping the held slave (a real close(2) on the last owned
        // reference to it in THIS process) is what finally lets the
        // master observe EOF/EIO -- see the module doc's first point.
        // No actual blocking work happens here (unlike ConPTY's
        // `ClosePseudoConsole`); the spawned thread exists only to
        // satisfy this trait method's `JoinHandle` return shape.
        // Idempotent: a second call finds `None` already and does
        // nothing further.
        self.slave = None;
        std::thread::spawn(|| {})
    }
}
