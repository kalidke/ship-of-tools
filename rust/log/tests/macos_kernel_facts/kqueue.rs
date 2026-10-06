//! Facts 3-6: the kqueue EVFILT_PROC / NOTE_EXIT death watch.

use super::*;

// ---------------------------------------------------------------------------
// Facts 3-6: the kqueue death watch (EVFILT_PROC / NOTE_EXIT)
// ---------------------------------------------------------------------------

/// How long a `NOTE_EXIT` is given to arrive after the process it names has
/// been killed. Kernel delivery is immediate; this is slack, not a
/// measurement -- the same role `PTY_REAP_TIMEOUT` plays above.
const EXIT_EVENT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a drain that expects to find NOTHING waits before its silence is
/// believed. The non-blocking drain is the one `reverify` will actually do;
/// this second, bounded one is what makes "nothing" mean "the knote is
/// detached" rather than "the kernel had not got to it yet".
const SILENCE_TIMEOUT: Duration = Duration::from_millis(250);
/// How many short-lived children fact 6 spawns to read the pid allocator.
/// The design's own figure. Each is an exec of `/usr/bin/true`, so this is
/// well under a second on a cold runner.
const PID_SAMPLES: usize = 200;
/// How many further processes are created and reaped under a SPENT knote in
/// fact 6, to see whether it re-arms for somebody else.
const REUSE_PROBE_CHILDREN: usize = 24;

/// A fresh `kqueue` fd, closed on drop -- the one-handle-one-kernel-object
/// shape the macOS `PeerProcess` will have (no shared kqueue, no registry).
fn new_kqueue() -> OwnedFd {
    // SAFETY: `kqueue` takes no arguments and returns an owned fd or -1.
    let fd = unsafe { libc::kqueue() };
    assert!(
        fd >= 0,
        "kqueue() failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `fd` was just returned by `kqueue` and is owned by nobody else.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// A zeroed `kevent`, which has no `Default`. Note the struct is
/// `#[repr(packed(4))]` on Apple targets: read its fields BY VALUE into
/// locals, never by reference (`&ev.ident`, or an implicit `{}` borrow in a
/// format string, does not compile).
fn empty_kevent() -> libc::kevent {
    libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// The outcome of one `EV_ADD | EV_RECEIPT` of `EVFILT_PROC`/`NOTE_EXIT`.
///
/// `EV_RECEIPT` is the design's own choice (§1 "Registration uses
/// `EV_ADD | EV_RECEIPT`"): it forces the kernel to answer every change with
/// an `EV_ERROR` event carrying the errno in `data`, so an attach failure
/// arrives as an ordinary event instead of as a `kevent` return of -1 the
/// caller would have to disambiguate from "no events were ready". Both
/// spellings are handled here anyway, because which one this kernel uses is
/// itself part of what these tests record.
struct Attach {
    /// `kevent`'s own return: 1 = one receipt, 0 = none, -1 = the call failed.
    rc: i32,
    /// What the kernel said about the ATTACH: 0 = attached, else an errno.
    errno: i32,
    /// Everything observed, for the assertion messages and the CI log.
    detail: String,
}

fn attach_note_exit(kq: RawFd, pid: u32) -> Attach {
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let mut out = [empty_kevent()];
    // NEVER a null timeout: on this call it would mean "block forever".
    let ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `kq` is a live kqueue fd owned by the caller; the changelist
    // and eventlist are live locals whose lengths match the counts passed;
    // `ts` is a live local, not null.
    let rc = unsafe { libc::kevent(kq, &change, 1, out.as_mut_ptr(), 1, &ts) };
    if rc < 0 {
        let errno = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(-1);
        return Attach {
            rc,
            errno,
            detail: format!("kevent(EV_ADD|EV_RECEIPT) returned -1, errno={errno}"),
        };
    }
    if rc == 0 {
        return Attach {
            rc,
            errno: -1,
            detail: "kevent returned 0 receipts for a one-entry EV_RECEIPT changelist \
                     -- EV_RECEIPT is not behaving as documented on this kernel"
                .to_string(),
        };
    }
    // Field reads are by value: `libc::kevent` is packed on Apple targets.
    let (ident, filter, flags, data) = (out[0].ident, out[0].filter, out[0].flags, out[0].data);
    let errno = if flags & libc::EV_ERROR != 0 {
        data as i32
    } else {
        0
    };
    Attach {
        rc,
        errno,
        detail: format!(
            "receipt: ident={ident} filter={filter} flags=0x{flags:x} data={data} (errno={errno})"
        ),
    }
}

/// One drained event, or the absence of one. Every field is recorded rather
/// than unwrapped away: on this path the shape of a non-answer is as much the
/// fact as the answer.
struct Drained {
    rc: i32,
    errno: i32,
    ident: u64,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: i64,
}

fn describe_event(d: &Drained) -> String {
    format!(
        "rc={} errno={} ident={} filter={} flags=0x{:x} fflags=0x{:x} data={}",
        d.rc, d.errno, d.ident, d.filter, d.flags, d.fflags, d.data
    )
}

/// Drain at most one event, waiting at most `bound`.
///
/// `Duration::ZERO` becomes `timespec { 0, 0 }` -- a NON-BLOCKING poll, which
/// is what `reverify` will do. It must never become a null pointer, which
/// `kevent` reads as "block forever": that is the one mistake in this design
/// that type-checks, passes review, and hangs the supervisor's first tick
/// (design §3). The mapping is spelled out here so the tests exercise the
/// same conversion the implementation will.
fn drain_one(kq: RawFd, bound: Duration) -> Drained {
    let ts = libc::timespec {
        tv_sec: i64::try_from(bound.as_secs()).unwrap_or(i64::MAX) as libc::time_t,
        tv_nsec: i64::from(bound.subsec_nanos()) as libc::c_long,
    };
    let mut out = [empty_kevent()];
    // SAFETY: `kq` is a live kqueue fd owned by the caller; no changelist is
    // passed (null + 0 is the documented spelling for "only drain"); the
    // eventlist is a live local of the length passed; `ts` is a live local.
    let rc = unsafe { libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), 1, &ts) };
    let errno = if rc < 0 {
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(-1)
    } else {
        0
    };
    // Field reads are by value: `libc::kevent` is packed on Apple targets.
    let (ident, filter, flags, fflags, data) = (
        out[0].ident,
        out[0].filter,
        out[0].flags,
        out[0].fflags,
        out[0].data,
    );
    Drained {
        rc,
        errno,
        ident: if rc >= 1 { ident as u64 } else { 0 },
        filter: if rc >= 1 { filter } else { 0 },
        flags: if rc >= 1 { flags } else { 0 },
        fflags: if rc >= 1 { fflags } else { 0 },
        data: if rc >= 1 { data as i64 } else { 0 },
    }
}

/// A child that lives until it is killed, with no stdio of its own: these
/// tests want nothing from it but a pid that is alive on demand.
fn spawn_sleeper() -> Child {
    Command::new("/bin/sleep")
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn /bin/sleep")
}

/// Create one process, let it exit, reap it; return the pid it held.
fn spawn_and_reap_one() -> u32 {
    let mut child = Command::new("/usr/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn /usr/bin/true");
    let pid = child.id();
    child.wait().expect("reap /usr/bin/true");
    pid
}

/// Fact 3 — is `NOTE_EXIT` delivered once, and what does the NEXT drain say?
///
/// The death watch treats "an exit was ever delivered" as proof the instance
/// is gone, and `reverify` asks that question with a non-blocking drain. Both
/// halves of the contract depend on the answer here and they pull in opposite
/// directions, which is why this is a test and not a comment:
///
/// - if the knote is spent after one delivery, the `AtomicBool` latch in the
///   design is LOAD-BEARING -- it supplies the stickiness a pidfd gets from
///   the kernel for free, and a second `wait` without it would block for the
///   full timeout and then report the process alive: the exact inversion of
///   the truth;
/// - if delivery were level-triggered (pidfd-like) or repeatable, the latch
///   would be redundant and the drain could be a plain re-poll.
///
/// So a red result here is not a bug to silence; it is the design input that
/// decides whether the latch exists.
#[test]
fn note_exit_is_delivered_exactly_once_and_the_knote_is_then_spent() {
    let mut child = spawn_sleeper();
    let pid = child.id();
    let kq = new_kqueue();

    let attach = attach_note_exit(kq.as_raw_fd(), pid);
    fact(&format!(
        "fact 3 attach to a LIVE child (pid {pid}): rc={} errno={} {}",
        attach.rc, attach.errno, attach.detail
    ));
    assert_eq!(
        attach.errno, 0,
        "EV_ADD of EVFILT_PROC/NOTE_EXIT on a LIVE same-user child (pid {pid}) FAILED: {}. \
         The macOS death watch has no other instance-bound primitive -- a `pidversion` \
         re-read is a second sample, not a watch, and `task_for_pid` is entitlement-gated -- \
         so if this is not \
         available the port needs a different mechanism, not a relaxed test.",
         attach.detail
    );

    child.kill().expect("kill the watched child");

    let first = drain_one(kq.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    fact(&format!("fact 3 first drain: {}", describe_event(&first)));
    assert_eq!(
        first.rc, 1,
        "the watched child (pid {pid}) was SIGKILLed and no NOTE_EXIT arrived within \
         {EXIT_EVENT_TIMEOUT:?}: {}. The registration succeeded, so a watch that never \
         fires means `wait` would hang to its deadline and report a dead process alive.",
        describe_event(&first)
    );
    assert_eq!(
        first.flags & libc::EV_ERROR,
        0,
        "the event delivered for pid {pid} is an EV_ERROR, not an exit: {}",
        describe_event(&first)
    );
    assert_eq!(
        first.ident,
        u64::from(pid),
        "the event names ident={} but the watched child is pid {pid}: {}",
        first.ident,
        describe_event(&first)
    );
    assert_eq!(
        first.filter,
        libc::EVFILT_PROC,
        "the event came from filter={}, not EVFILT_PROC ({}): {}",
        first.filter,
        libc::EVFILT_PROC,
        describe_event(&first)
    );
    assert!(
        first.fflags & libc::NOTE_EXIT != 0,
        "the event for pid {pid} carries no NOTE_EXIT bit: {}",
        describe_event(&first)
    );

    // THE question the latch exists for. Non-blocking, exactly as `reverify`
    // will ask it.
    let second = drain_one(kq.as_raw_fd(), Duration::ZERO);
    fact(&format!(
        "fact 3 second drain (non-blocking): {}",
        describe_event(&second)
    ));
    assert_eq!(
        second.rc, 0,
        "a SECOND drain of the same kqueue returned an event ({}). NOTE_EXIT is therefore \
         NOT once-only on this kernel -- it is repeatable or level-triggered. That is a \
         DESIGN INPUT, not a defect in this test: it would make the `AtomicBool` latch \
         redundant and change what `reverify`'s drain means. Rule on it; do not relax \
         this assertion.",
        describe_event(&second)
    );

    // Reaping is the other thing that could plausibly re-deliver: the design
    // calls `reap` (waitpid WNOHANG) only after this event, so pin that the
    // reap itself puts nothing back on the queue.
    let status = child.wait().expect("reap the watched child");
    let third = drain_one(kq.as_raw_fd(), SILENCE_TIMEOUT);
    fact(&format!(
        "fact 3 third drain, after waitpid ({status}): {}",
        describe_event(&third)
    ));
    assert_eq!(
        third.rc, 0,
        "reaping the child (pid {pid}, {status}) delivered a further event ({}) on a \
         knote whose NOTE_EXIT had already been consumed.",
        describe_event(&third)
    );

    fact(&format!(
        "fact 3 CONFIRMED: NOTE_EXIT for pid {pid} was delivered exactly once (data={}, \
         i.e. the wait status) and the knote is then spent -- the latch is load-bearing",
        first.data
    ));
}

/// Fact 4 — what happens when you attach to a ZOMBIE?
///
/// The challenge registers the watch between the same-user check and the
/// identity exchange, so it can meet a peer that has already exited but has
/// not been reaped. The design is safe under either answer and branches on
/// it -- `ESRCH` means "not attachable", and who owns the pid decides what
/// that means (our own child: provably exited, since the zombie pins the pid;
/// a peer we did not spawn: `Undetermined`). The branch must be chosen by a
/// test rather than by a comment, which is this test.
///
/// There is exactly one answer the design CANNOT absorb, and it is the one
/// this test is really hunting: an attach that SUCCEEDS and then never fires.
/// That would be a watch that reports a dead process alive forever.
#[test]
fn attaching_to_an_unreaped_zombie_either_fails_esrch_or_fires_at_once() {
    let mut child = spawn_sleeper();
    let pid = child.id();

    // The witness watch, registered while the child is still alive, is how
    // this test knows the child has EXITED without calling `wait` on it --
    // `waitpid` would reap it and destroy the very state under measurement.
    let witness = new_kqueue();
    let witness_attach = attach_note_exit(witness.as_raw_fd(), pid);
    assert_eq!(
        witness_attach.errno, 0,
        "the witness attach to a live child (pid {pid}) failed: {}",
        witness_attach.detail
    );
    child.kill().expect("kill the child that becomes the zombie");
    let seen = drain_one(witness.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    assert_eq!(
        seen.rc, 1,
        "the witness watch never reported the exit of pid {pid} ({}), so this run never \
         reached the state it exists to measure -- an exited, UNREAPED process. Fact 3 \
         says whether the watch itself is the problem.",
        describe_event(&seen)
    );
    // `child` is deliberately NOT waited on here: the process is a zombie,
    // and while we hold it the kernel cannot recycle its pid, so `pid` below
    // unambiguously names it.

    let kq = new_kqueue();
    let attach = attach_note_exit(kq.as_raw_fd(), pid);
    fact(&format!(
        "fact 4 attach to an UNREAPED ZOMBIE (pid {pid}): rc={} errno={} {}",
        attach.rc, attach.errno, attach.detail
    ));

    if attach.errno != 0 {
        assert_eq!(
            attach.errno,
            libc::ESRCH,
            "attaching to an unreaped zombie (pid {pid}) failed with errno={}, which is \
             neither success nor ESRCH ({}): {}. The design reads ESRCH as \"not \
             attachable\" and gives it an owner-dependent meaning; a third errno has no \
             reading at all and must be ruled on.",
            attach.errno,
            libc::ESRCH,
            attach.detail
        );
        fact(&format!(
            "fact 4 BRANCH A: EV_ADD on an unreaped zombie fails ESRCH -- `proc_find` does \
             not return zombies. probe_macos::SpawnedChild must read ESRCH as 'already \
             exited' (the zombie pins the pid); ChallengedProcess must read it as \
             Undetermined."
        ));
    } else {
        let ev = drain_one(kq.as_raw_fd(), EXIT_EVENT_TIMEOUT);
        fact(&format!(
            "fact 4 BRANCH B drain after attaching to a zombie: {}",
            describe_event(&ev)
        ));
        assert_eq!(
            ev.rc, 1,
            "EV_ADD on an unreaped zombie (pid {pid}) SUCCEEDED and then delivered NOTHING \
             within {EXIT_EVENT_TIMEOUT:?}: {}. This is the ONE outcome the death watch \
             cannot absorb: the registration reports success, so nothing fails closed, and \
             the exit it was meant to report has already happened and will never happen \
             again -- every caller reads 'still alive' forever. The registration ordering \
             in the challenge would have to change. Do not relax this assertion.",
            describe_event(&ev)
        );
        assert_eq!(
            ev.ident,
            u64::from(pid),
            "the immediate event names ident={} but the zombie is pid {pid}: {}",
            ev.ident,
            describe_event(&ev)
        );
        assert!(
            ev.fflags & libc::NOTE_EXIT != 0,
            "the immediate event for zombie pid {pid} carries no NOTE_EXIT bit: {}",
            describe_event(&ev)
        );
        fact(
            "fact 4 BRANCH B: EV_ADD on an unreaped zombie SUCCEEDS and fires NOTE_EXIT at \
             once. Both owners can then read it the same way -- attach, drain, and an \
             immediate event means 'already exited'.",
        );
    }

    let status = child.wait().expect("reap the zombie");
    fact(&format!("fact 4: the zombie (pid {pid}) reaped: {status}"));
}

/// Fact 5 — attaching to a pid whose process is fully gone must FAIL.
///
/// This is the fail-closed path the registration ordering rests on. The
/// challenge attaches BEFORE it writes its request, so that a peer which died
/// in the window cannot yield a silently-dead watch: the attach itself
/// refuses, and the challenge returns `Undetermined`. If `EV_ADD` instead
/// succeeded on a pid with no process behind it, that refusal would not
/// exist and the ordering argument would collapse.
#[test]
fn attaching_to_a_reaped_pid_fails_with_esrch() {
    let mut child = spawn_sleeper();
    let pid = child.id();
    child.kill().expect("kill the child before reaping it");
    let status = child.wait().expect("reap the child");
    // From here `pid` names nothing: the process is gone and its pid is back
    // in the allocator's pool. Darwin allocates pids sequentially and wraps
    // at PID_MAX (fact 6), so handing this exact number to a new process
    // between the two statements below would take a full wrap -- roughly 1e5
    // intervening process creations. If this test ever goes red having
    // ATTACHED, check that first, but read it as the fact having changed.
    let kq = new_kqueue();
    let attach = attach_note_exit(kq.as_raw_fd(), pid);
    fact(&format!(
        "fact 5 attach to a REAPED pid ({pid}, {status}): rc={} errno={} {}",
        attach.rc, attach.errno, attach.detail
    ));

    assert_ne!(
        attach.errno, 0,
        "EV_ADD of EVFILT_PROC/NOTE_EXIT on a fully REAPED pid ({pid}, {status}) SUCCEEDED: \
         {}. There is then no fail-closed path at all: a watch registered on a peer that \
         died in the challenge window would attach to nothing, report no exit, and be read \
         as 'the peer is alive' forever. The whole registration ordering depends on this \
         refusal.",
        attach.detail
    );
    assert_eq!(
        attach.errno,
        libc::ESRCH,
        "EV_ADD on a fully REAPED pid ({pid}) failed with errno={}, not ESRCH ({}): {}. \
         The implementation maps ESRCH to a specific, owner-dependent meaning (fact 4); \
         another errno has no mapping and must be ruled on rather than lumped in.",
        attach.errno,
        libc::ESRCH,
        attach.detail
    );

    let ev = drain_one(kq.as_raw_fd(), SILENCE_TIMEOUT);
    assert_eq!(
        ev.rc, 0,
        "the attach to reaped pid {pid} failed with ESRCH and yet the kqueue delivered an \
         event ({}) -- a failed registration must leave no knote behind.",
        describe_event(&ev)
    );

    fact(&format!(
        "fact 5 CONFIRMED: EV_ADD on a reaped pid ({pid}) fails ESRCH and leaves no knote"
    ));
}

/// Fact 6 — the load-bearing assumption of the whole macOS port, and the
/// limits of what a test on a CI runner can say about it.
///
/// The claim the port rests on: a knote is bound to a *process*, not to a pid
/// *number*, so a registration that has NOT fired is a proof of identity --
/// which is what lets `terminate` send `kill(pid, SIGKILL)` by number and
/// what lets `reverify` answer from the queue alone.
///
/// WHAT THIS TEST PROVES:
///
/// 1. Darwin allocates pids sequentially (strictly increasing, at most one
///    wraparound across {PID_SAMPLES} consecutive children). This is the
///    bound the design quotes and has so far only asserted in prose: reuse of
///    one specific number requires a full wrap, on the order of 1e5
///    intervening process creations. A fork storm makes those creations take
///    LONGER, so the bound does not shrink under load.
/// 2. A knote fires for ITS target and not for another process: a watch on a
///    second, still-living child stays silent while the first one dies.
/// 3. A SPENT knote never re-arms. After its NOTE_EXIT is consumed and its
///    target reaped, it stays silent across the creation, exit and reaping of
///    {REUSE_PROBE_CHILDREN} further processes.
///
/// WHAT THIS TEST DOES *NOT* PROVE: that a knote fails to fire for a process
/// which actually receives the watched pid number. Forcing that requires
/// wrapping PID_MAX -- ~1e5 process creations on a shared CI runner, whose
/// own processes are also drawing from the same allocator, so the landing is
/// not even deterministic. This test deliberately does not attempt it, and
/// asserts instead that none of the probe children DID receive the watched
/// pid -- which is precisely the reason it cannot observe a reuse, made
/// explicit rather than left as an unstated gap.
///
/// The instance-binding claim therefore rests on three pinned facts and one
/// unobserved step: the kernel resolves the ident to a process AT ATTACH TIME
/// and refuses when there is none (fact 5), delivery is once-only and the
/// knote is then spent (fact 3), a spent knote never re-arms (3 above) -- and
/// the unobserved step is that the kernel does not re-resolve a stored ident
/// against a later process. Nothing short of a real wrap observes that step;
/// if a future reader finds a way to force one cheaply, this is the test to
/// extend.
#[test]
#[allow(clippy::too_many_lines, reason = "one test scenario: pid reuse needs a full sequential wrap")]
fn pid_reuse_needs_a_full_sequential_wrap_and_a_spent_knote_never_rearms() {
    // --- 1. the allocator ---------------------------------------------------
    let mut pids = Vec::with_capacity(PID_SAMPLES);
    for _ in 0..PID_SAMPLES {
        pids.push(spawn_and_reap_one());
    }
    let descents: Vec<(u32, u32)> = pids
        .windows(2)
        .filter(|w| w[1] <= w[0])
        .map(|w| (w[0], w[1]))
        .collect();
    fact(&format!(
        "fact 6 allocator: {PID_SAMPLES} children, first={} last={} descents={:?}",
        pids[0],
        pids[PID_SAMPLES - 1],
        descents
    ));
    assert!(
        descents.len() <= 1,
        "Darwin no longer allocates pids sequentially: across {PID_SAMPLES} consecutive \
         children the pid went down or stood still {} times ({descents:?}). At most one \
         such step is a wraparound at PID_MAX; more than one means the allocator has been \
         randomised. That would delete the only bound the port has on pid reuse -- \
         `terminate`'s numeric `kill(pid, SIGKILL)` and the challenge's attach window are \
         both argued from \"reuse of a specific number takes a full wrap\". Do not relax \
         this assertion; it is the premise that would have changed.",
        descents.len()
    );

    // --- 2. a knote fires for its own target only ---------------------------
    let mut first = spawn_sleeper();
    let mut second = spawn_sleeper();
    let first_pid = first.id();
    let second_pid = second.id();
    let kq_first = new_kqueue();
    let kq_second = new_kqueue();
    for (kq, pid) in [(&kq_first, first_pid), (&kq_second, second_pid)] {
        let attach = attach_note_exit(kq.as_raw_fd(), pid);
        assert_eq!(
            attach.errno, 0,
            "attach to live child pid {pid} failed: {}",
            attach.detail
        );
    }

    first.kill().expect("kill the first child");
    let ev = drain_one(kq_first.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    assert_eq!(
        ev.rc, 1,
        "the watch on pid {first_pid} did not report its exit ({}); fact 3 covers the \
         delivery itself, so this run cannot speak to what follows.",
        describe_event(&ev)
    );
    assert_eq!(
        ev.ident,
        u64::from(first_pid),
        "the watch on pid {first_pid} reported ident={} instead: {}",
        ev.ident,
        describe_event(&ev)
    );
    let bystander = drain_one(kq_second.as_raw_fd(), SILENCE_TIMEOUT);
    assert_eq!(
        bystander.rc, 0,
        "the watch registered on pid {second_pid} (still alive) fired when a DIFFERENT \
         process (pid {first_pid}) exited: {}. A knote that reports other processes' \
         exits names nothing, and an un-fired registration would stop being a proof of \
         anything.",
        describe_event(&bystander)
    );
    let first_status = first.wait().expect("reap the first child");

    // --- 3. a spent knote never re-arms -------------------------------------
    let mut probe_pids = Vec::with_capacity(REUSE_PROBE_CHILDREN);
    for _ in 0..REUSE_PROBE_CHILDREN {
        probe_pids.push(spawn_and_reap_one());
    }
    assert!(
        !probe_pids.contains(&first_pid),
        "one of the {REUSE_PROBE_CHILDREN} probe children was handed pid {first_pid} -- the \
         very number this test's spent knote watches. That is the reuse this test says it \
         cannot force, so if it happens the run is no longer the approximation documented \
         above: record what the spent knote did ({:?}) and rewrite this test around the \
         real observation.",
        probe_pids
    );
    let spent = drain_one(kq_first.as_raw_fd(), SILENCE_TIMEOUT);
    assert_eq!(
        spent.rc, 0,
        "the SPENT knote for pid {first_pid} ({first_status}) delivered a further event \
         ({}) after {REUSE_PROBE_CHILDREN} unrelated processes were created and reaped. \
         It has re-armed for somebody, which is the failure mode the whole macOS identity \
         argument excludes.",
        describe_event(&spent)
    );

    // The second watch is still live and still silent: its target never died.
    let still_alive = drain_one(kq_second.as_raw_fd(), Duration::ZERO);
    assert_eq!(
        still_alive.rc, 0,
        "the watch on pid {second_pid} fired although that child is still running: {}",
        describe_event(&still_alive)
    );
    second.kill().expect("kill the second child");
    let ev_second = drain_one(kq_second.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    assert_eq!(
        ev_second.rc, 1,
        "the watch on pid {second_pid} never reported its own exit ({}) -- having stayed \
         silent for the right reason, it must still speak for the right one.",
        describe_event(&ev_second)
    );
    assert_eq!(
        ev_second.ident,
        u64::from(second_pid),
        "the watch on pid {second_pid} reported ident={} at its own exit: {}",
        ev_second.ident,
        describe_event(&ev_second)
    );
    let second_status = second.wait().expect("reap the second child");

    fact(&format!(
        "fact 6 CONFIRMED (to the limit stated on the test): pids sequential over \
         {PID_SAMPLES} children with {} wrap(s); the watch on {first_pid} ({first_status}) \
         fired once for its own target and stayed spent across {REUSE_PROBE_CHILDREN} \
         later processes; the watch on {second_pid} ({second_status}) stayed silent \
         throughout and then reported its own exit",
        descents.len()
    ));
}
