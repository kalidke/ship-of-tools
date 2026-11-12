# rust/platform: platform (contract)

The bottom of the workspace: one answer per machine fact and one primitive per OS mechanism, for every crate above it:
the local connection (a bounded client of a socket or pipe on this box, reaching only this account) and the masking of
page secrets in logs among them.
The crate (`sot-platform`) depends on nothing of ours; sot-capsule (the capsule), sot-protocol (wire), the backend, the
window and the updater depend on it, and nothing re-exports it. It holds no runtime state and starts no process for
anyone but the native launcher it hands out.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on, and the test that holds it (the Tests section lists
where each lives).

1. **A machine fact has one answer.** The state folder (`sot_state_dir`), the config folder (`sot_config_dir`), the
   runtime base (`runtime_sot_dir`), the host name (`host_name`, `SOT_SELF_HOST` over the kernel's name), the OS
   account (`own_account_id`) and the folder hash lane sockets are named by (`state_dir_hash`) are each defined once,
   here, from an `Env` value the caller supplies or the process's own; a second rule anywhere is a defect. On Unix an
   empty `XDG_STATE_HOME` uses the same home fallback as an unset one
   (`an_empty_xdg_state_home_uses_the_home_fallback`). Raw host
   name readers that remain, each for a format that must not change under installed releases: the comm handle's host
   component (`rust/backend/src/comm/identity/context.rs`), the updater's stage lock record (`rust/updater/src/lock.rs`)
   and the window's resume file name (`rust/frontend/src/ui/persist/resume.rs`); they fold with their owners' passes.
   `host_name` returns `Err`, never a guessed name. A runtime base a process binds and another dials agrees only
   while the environment does not change; the supported setups keep it fixed for a boot (Linux: linger and the user
   manager's `XDG_RUNTIME_DIR`; macOS: none; Windows: no runtime base). A boot without logind binds at
   `/tmp/sot-<uid>`.
2. **A publication is durable and never clobbers.** `publish_noreplace` renames without replacing, flushes the
   renamed file (Windows) and then the parent; the caller flushes the source first (`write_durable` does); `write_durable` (opens the record's folder once, by path, following a link, and only a folder opens: a file, pipe or device there is refused at once (on Windows by the relative open itself); then, relative to that open folder, a stale `<name>.tmp` is removed (on Windows with POSIX semantics), a fresh one is created refusing an existing name and following no link, written and synced, renamed over the record by `write_beneath`'s steps, and the folder synced) and `remove_durable` are the one write and delete for a record a later start acts on; on Windows a
   removal has no parent sync (Win32 has no write-through delete). Rests on
   renameat2/renamex_np/MoveFileEx for a publication, the rename relative to the record's folder (Unix `renameat`,
   Windows by handle) for a record, and the folder's flush (on Windows `FlushFileBuffers` on a folder handle); only
   linux, macos and windows build.
3. **A lock is the kernel's.** A fence, the daemon's single-instance lock and a held folder's lock are released at
   once by a dropped guard (`WriterLock`'s Drop unlocks) and when the holder dies and the last descriptor of the locked
   file closes; a forked child that kept a copy holds it. The descriptors this crate opens are close-on-exec, so no
   exec'd child inherits one; a `HandoverLock` only closes on drop, so a copy in a child keeps the fence held, by design. A contended lock fails within
   `RETRY_DEADLINE_MS` as "lock held". Kernel file locks are taken only in these guards and `InboxLock`
   (messaging's, in `rust/backend/src/comm/mail/inbox.rs`); `rust/clippy.toml` refuses them elsewhere.
4. **A held folder stays while its maker lives.** (Its lifetime is the `HeldDir` guard's or the maker process's,
   whichever the lock outlasts; on Windows the folder's parent is assumed private to the account.) `remove_ended` removes a folder only once it can take the folder's
   lock, which `HeldDir::create` takes under a staging name before renaming the lock file to the name a removal
   looks for; another computer sharing the folder, another pid namespace or a reused pid never removes a live
   maker's folder. A link, a file, another account's folder and a folder whose lock cannot be opened stay. Across
   computers this needs locks every client sees (NFS with its lock service), and only while the maker's client
   holds its lease. A temporary folder on a file system that refuses locks stops `HeldDir::create`.
5. **A descriptor is born close-on-exec.** A socket, pipe or pty made through `socket_cloexec`, `pipe_cloexec` or
   `openpty_cloexec` is never, between its birth and its flag, in a child forked through a guarded spawn (Linux in the
   creating call; macOS under a process-wide birth lock every fork site takes around its fork, `fork_guard`);
   `inherit_none` is the belt for descriptors made elsewhere (std, tokio, mio). Windows hardens a process's own stdio
   handles (`harden_own_stdio`) and makes owner-only descriptors at birth (`owner_protected_pipe_descriptor`).
6. **A local reply is trusted only after the challenge.** The OS steps (the peer's account and pid as the OS
   recorded them, then equality of identity) precede the wire steps (`exchange_identity`, over an
   `IdentityExchange` the caller's protocol provides: the capsule's `RowHelloExchange`); `run_with_deadline` bounds the wire steps, and the OS steps read kernel
   records without waiting on the peer. Two outcomes differ: the full challenge earns `Proven` (OS identity and the
   wire's pid and creation time agree), while `authenticate_server` proves the account alone (`PeerAuthenticated`).
   A wrong, partial or late reply is `Foreign` or `Undetermined`; a second complete reply inside the same read is
   `Foreign`, and bytes after a conclusive answer are the exchange's to refuse (the row hello's do). On macOS the peer is
   attributed by `LOCAL_PEERPID` and its start time; the assumptions are stated in `challenge_macos.rs`'s module doc. A TCP connection on loopback is admitted only when the OS names this account as its owner
   (`peer_owner::serve_own`). Rests on SO_PEERCRED/pidfd, LOCAL_PEERPID/kqueue, the pipe's SID and impersonation
   level, the TCP owner tables.
7. **Storage exhaustion is known by its native code.** ENOSPC and EDQUOT; on Windows ERROR_DISK_FULL,
   ERROR_HANDLE_DISK_FULL and ERROR_DISK_QUOTA_EXCEEDED, read from the `io::Error` an `Error::Io` carries, never from
   text and never from a transport error (`native_storage_code`; the capsule reads its own error type through it). `preflight_volume` and Windows `io_ctx` return such
   an error as itself.
8. **A volume is proved before a store opens on it.** `preflight_volume` refuses a root whose filesystem cannot do
   the operations the store uses (create, no-clobber rename, fsync), a network filesystem above all (NFS answers EINVAL
   to every `renameat2` flag), so a state root on a network home points `XDG_STATE_HOME` at local disk. It does not
   prove persistent backing: rows refuses volatile storage itself (`rows/spawn/state_root.rs`). The Windows
   preflight has no test.
9. **A restart is paced.** A connection or child restarted through `Redial` waits from its caller's floor,
   doubling to its cap, and starts over only after one that lasted `STABLE` (60 s) or at a person's request; an
   answered hello, a bare connect or a generation alone does not restart it.
10. **A directory pin opens only a directory (Unix).** `dir_identity` and `PinnedDir::open` open with `O_DIRECTORY`,
    so a FIFO or any other non-directory fails at once and never waits. `PinnedDir` exposes its held Unix descriptor
    through `AsFd`: an `openat` walk resolves from the same base directory object after a rename of it or
    an ancestor, on Linux and macOS. This does not strengthen macOS's legacy `pinned_path`, which still returns a
    pathname. On Windows the identity open uses backup semantics and accepts an ordinary file (gap); the pin supplies
    no handle-relative descendant open.
11. **Private means private (Unix).** `is_private_dir` and `secure_private_dir` accept only an owner-only directory
    that is not a link; `secure_socket_dir` secures each level under the runtime base the same way. `ensure_private_dir`
    is the weaker form: it creates the folder, follows a symlink and repairs the mode to 0700. On Windows
    `is_private_dir` is only `is_dir`; the account's profile folders are the protection. A refusal from the OS keeps
    the OS error as its source (`Error::Path`).
12. **A job ends what is in it when its last handle closes (Windows).** `host::winjob::AnonymousJob` is an anonymous,
    non-inheritable job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` alone and no breakaway permitted, so every process
    started inside it stays inside and the kernel ends them all when the last handle closes, however the holder died.
    `assign(&Child)` puts a started child in (guarantee 18); sequencing a termination and resuming a suspended start are the caller's (containment's and the
    capsule's ConPTY agent). Limits: a process started through a broker (WMI, COM activation, a scheduled task, a service)
    is outside the job.

13. **A local connection is bounded, cancellable and reaches only this account.** `transport::connect_own` connects
    to a socket or pipe by path and then proves, before the caller writes, that this OS account serves it (the
    listener's account as the kernel recorded it at `listen()`, or the pipe server's SID); a different account's
    listener is refused. The connect is bounded by `CONNECT_BOUND` (2 s): a busy endpoint (a full backlog, a claimed
    pipe instance, an interrupted attempt) is retried within it (macOS may answer a full backlog with an immediate
    `ConnectionRefused` instead, which reads as absent), an absent one (no listener, no pipe) is refused at the
    first attempt and never charged to the bound. A `Client` (`SocketClient`, `PipeClient`) reads and writes blocking
    but is cancelled from any thread: `cancel` ends a blocked read or write at once (Unix by `shutdown(2)`, Windows by
    `CancelIoEx`), and a cancelled client refuses every later call; a second same-direction call is refused as
    `ConcurrentSubmit` before it touches the OS; `Ok(0)` is ordered EOF, never a spurious completion. On Unix a terminal write
    failure latches the client like a cancel. `accept_policy` is the one rule an accept loop applies to an accept
    error: retry, sleep `ACCEPT_SLEEP` on resource exhaustion, or end. Rests on `shutdown(2)`/`CancelIoEx`, the
    kernel's peer records (SO_PEERCRED, LOCAL_PEERCRED, the pipe's SID) and the identification impersonation level.
    Limits: on Windows a write that fails synchronously does not latch the pipe client (the slot stays idle and
    a later call reaches the OS again; the latch test, `rust/capsule/tests/socket_unix/client.rs`, is Unix-only); an attempt or wait in progress finishes before the bound is read (a 20 ms sleep, a 200 ms pipe wait), so
    the bound is not an exact elapsed time; Windows `connect_own` opens at identification level but cannot refuse a
    pipe before that open.
14. **A page secret or a Slack token is masked in the logs written through `RedactingWriter`.** `secret::redact` replaces the value after every
    `secret=` (up to the first delimiter), every maximal run of exactly 32 lowercase hex characters and every Slack
    token (`xapp-`, or `xox`, a lowercase letter and `-`, then a value ending at the same delimiters, prefix
    included; a bare prefix is left; by form, with no list of values) with `<redacted>`, and leaves every other byte unchanged (borrowed when nothing matched); `RedactingWriter` applies it
    to each write and reports the whole buffer written. Wired through it: the daemon's log and the window's log. Limits:
    the supervisor's own diagnostics go straight to its stderr and are not masked; a secret split across two writes is
    not seen (each write is masked alone).
15. **A file is opened for reading only if regular, and a small configuration file is read bounded.**
    `host::open_regular(path)` opens without blocking or taking a controlling terminal (Unix `O_NONBLOCK | O_NOCTTY`)
    and refuses on the open handle anything but a regular file (`InvalidInput`: a FIFO or a device never waits); files'
    figure and crop source are opened through it. `host::read_regular(path, cap)` returns the file's text through it,
    reads at most `cap` bytes, and refuses a larger file or invalid UTF-8 as `InvalidData` with a message of its own; a missing file is
    `NotFound`. Chat's two files and hosts.toml (the store and sync included) are read through it. Limit: only a FIFO's
    wait is ended; a regular file on a hung network mount still blocks in the OS. On Windows a directory is refused by
    std's open (`PermissionDenied`) before the handle check; the cap and text rule apply, and a named pipe's name is
    not promised refused. No test runs a named pipe there.
16. **A test holds a thread at a marked line until it releases it, and no hold outlives its bounds.** Product code marks a
    line with `hold_point!("<subsystem>.<place>", key?)` (sync) or `hold_point_async!` (async: the arrival waits on `spawn_blocking`, so paused tokio time cannot pass the hold; a wait lost with its
    runtime marks its ticket lost, and the arm fails by name with a message naming the lost waiter); the name is a literal of `a-z 0-9 . _ -`, checked at compile time, and a key is formatted only
    when an arm asks for one. A test arms the point with `test_hold::Holds::here().arm(point).key(k).hold(n)`: arrivals
    whose key matches take tickets 0, 1, 2 in claim order, tickets below `n` wait until `release(t)`, the rest pass and
    are counted; `reached(t)` waits for arrival `t`. Bounds: every test-side wait 30 s (`WAIT`), a held arrival 60 s
    (`GIVE_UP`), after which it goes on; `give_up_after` and `reached_within` refuse a longer bound. No product-side
    path panics or refuses: whatever an arrival meets is recorded on its ticket, and the arm fails the test by name (with
    the usual cause) at its next call, `reached`, `reached_async`, `release`, `arrivals`, or its drop; never an exit or a
    hang. Every wait uses the system clock, so paused tokio time fires no bound. An arm is exclusive per point (a second
    waits for the first), each arm has its own wake, and `release` of an arrival that has not arrived or is not held is
    refused. A sync point held, or a sync `reached`, on a current-thread runtime's own thread is not detected (tokio has
    no stable way to tell): it stops the test that must release it, gives up at its bound and fails the arm, naming
    `hold_point_async!` / `reached_async`. Dropping an arm releases every held arrival and never panics while the test
    is already panicking. Both profiles expand a point to `if ON { arrive(..) }` (`ON` is `cfg!(debug_assertions)`), and `arrive`
    and `arrive_async` have one signature each with a body only under debug assertions: a release caller type-checks
    against the same signatures, no key is evaluated, and the optimizer drops the call and the name (`strings` on a
    release build shows none). rustdoc's debug assertions are the doc test crate's, while the library is built as
    the profile says, so `cargo test --release --doc` runs the release expansion and bodies; no gate runs it (the gate's
    `cargo check --release -p sot-chat` covers only what sot-chat calls). One limit: on a current-thread runtime whose OS
    refuses a blocking thread, tokio's `spawn_blocking` panics and the ticket stays held.
    A child started with `SOT_TEST_HOLDS` (`Holds::folder(dir)`, then `env()` on the child's command before it starts; read
    once per process, and only with debug assertions) is held at a marked line by the same arm, tickets and bounds, through
    files of that folder: tickets are numbered across every process sharing it by an exclusive file create (`<point>.<t>.at`),
    a release, the arm's drop and a give-up are files (`.go`, `.all`, `.gaveup`), a wait cancelled before it ran (a dropped runtime) is a file (`.lost`) that fails the arm by name, the arm's terms are renamed into place
    whole, and no file is deleted while the folder lives, so one folder takes one arm of a point for its life (a second is
    refused). A held child always goes on by its give-up (at most 60 s, whatever the arm file says; arrivals after the arm's drop take no ticket) and the arm fails by
    name; an arrival before the arm is not seen; a child killed while held records no give-up. A daemon passes the
    variable on to the supervisors it starts, so one folder serves both. Limits:
    the `.lost` write checks before it creates, so one made after the arm's final scan goes unseen (a window of microseconds); the arming
    process's own arrivals pass uncounted; arms of one point serialize even with distinct keys in a process; in a
    test binary whose other tests reach the same point the arm names a key only its own arrivals carry.
17. **A write beneath a root follows no link, whether present when it runs or put there while it runs, inside the root.** `host::write_beneath(root, rel, bytes)`
    takes a `root` that is its own canonical path and opens its last name without following it, then each
    folder below it relative to the one before, the same way (Unix: `O_NOFOLLOW|O_DIRECTORY`, a missing folder made with
    `mkdirat` and opened the same way; Windows: one relative `NtCreateFile` per name with `FILE_OPEN_REPARSE_POINT`, a
    missing folder made by `FILE_OPEN_IF`, and a name-surrogate reparse tag, which is a symlink, junction or mount
    point, refused). It creates its temporary file exclusively under a fresh `.sot-<16 hex>.tmp`
    name, renames and syncs relative to the same folder (Windows: the file is held with share-read alone, so no other
    open renames or deletes it, and is renamed by its handle, retried while a scanner holds the name; a denied rename is tried again with POSIX semantics; the handle closes at the rename, before the folder's sync, since it then holds the final name without delete sharing, which no other writer's rename replaces); `Ok` means the entry at the temporary name was renamed into
    place. No path is used after the root, and every name is converted before the root is opened. A root that is not
    its own canonical path (one that does not exist, a link to nothing included, is `NotFound`), a `rel` that is not
    `/`-joined plain names (on Windows none holding `\` or `:`), and a link or file at a folder's name are
    `InvalidInput`. A Windows root is its own canonical path when std's canonical form without its verbatim prefix equals
    it, ASCII case aside. A relative root, and a root that is a file, are `InvalidInput`, the relative one before any file system call. Windows answers some refusals by other kinds (its root is opened for generic read and write first, so a read-only file or one held without write sharing answers its own kind): a folder removed after its open is `PermissionDenied`, and a root under a file, or spelled through `..` to a folder that does not exist, is `NotFound`. Limits: on Windows a final name another program holds open with delete sharing (an open handle) is replaced (its open keeps the old file) and fails the write after the retry window when it does not; the rule holds on a local volume, and on a network share the server resolves each name again from the share's root. On Windows a name with a trailing dot or space, or a device name, is created literally, and a Win32 path to the returned name can reach another entry or a device, as can a short (8.3) alias. A crash between the create and the rename leaves a `.sot-<hex>.tmp` that no later write removes.
    Folders made before a later error stay. After the rename the old entry is gone: an error in the folder sync leaves
    the entry renamed into place at the name. The returned path names
    the file only while nobody renames its folders. A mount inside the root is followed on Unix and refused on Windows. On Unix a folder with write and
    search but no read permission is refused; on Windows each folder open asks for list, add-file, add-subfolder and read-attributes access. Entries inside the root may change while the call runs: a folder that
    becomes a link before its open is refused (a missing folder's make and open count as its open), one renamed after
    its open, a link put at its name, keeps the write in the folder that was opened, one removed after its open
    takes no write, a link planted at the final name, to a file or a folder, is replaced and never followed (a folder there makes the write an error and is kept), and concurrent writers of one name
    each publish a whole file. Out of scope:
    a process that changes the root's own entry or a folder above it, or moves a folder of the root outside it.
    `host::create_dir_beneath(root, rel)` is its folder twin: the same root rule and the same walk, which makes no
    folder, then the last name created relative to its parent's handle (Unix `mkdirat`; Windows `NtCreateFile` with
    `FILE_CREATE` and `FILE_DIRECTORY_FILE|FILE_OPEN_REPARSE_POINT`), and the parent synced. A missing parent is
    `NotFound` and nothing is made; an entry of any kind at the name, a link included, is `AlreadyExists` and is left as
    it is; a link or file at a parent is `InvalidInput`; a failed sync of the parent leaves the folder made and answers the error; on Windows a name a Win32 path does not reach as itself (a trailing dot or space, a device name) is made literally, as for the write; a parent renamed or swapped after its open leaves the new
    folder in the parent that was opened. Its caller is `FilesMode::create_dir`.

18. **A process can bound itself, and a job takes an assigned child.** Each primitive is one OS mechanism, called by the process before it reads any input. These end it by an exit code the parent reads, with no signal death, core file or crash report: the heap cap, `exit_now`, and on Unix the `SIGABRT` handler of `exit_on_crash` and the `SIGXCPU` handler of `limit_cpu`. These end it otherwise: `end_with_parent` and `limit_cpu`'s hard limit by `SIGKILL`, and on Windows `exit_on_crash` only keeps a dialog away (the status is the OS's own crash status; on the one computer where it was seen, no report was written either); `close_inherited` ends nothing. `host::heap_cap::HeapCap` wraps `System` as the binary's `#[global_allocator]`; disarmed it costs one relaxed load and the `System` call, and after `arm(cap, code)` it counts the bytes requested (`realloc` by its growth, `alloc_zeroed` and frees included; signed, since a block freed after arming may predate it) and past `cap` (and one request larger than `cap` by itself, before it is added, so no request can wrap the count) ends the process through `exit_now(code)`, never returning null, because a null allocation is an abort; `high_water()` is the most requested bytes held at once since arming, the unit `cap` is in. `host::bound` has `exit_now(code)` (`_exit`; Windows `TerminateProcess` on itself: no handler, no destructor, no allocation), `close_inherited()` (Unix: every descriptor above stderr), `exit_on_crash(code)` (Unix: a `SIGABRT` handler that calls `_exit(code)`, which turns an abort, a panic abort and Rust's stack-overflow report into an exit; Windows: `SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX)`), `limit_cpu(seconds, code)` (Unix: `RLIMIT_CPU` soft `seconds`, hard `seconds + 1`, and a `SIGXCPU` handler that calls `_exit(code)`; a zero is one second on Linux), `end_with_parent()` (Linux: `PR_SET_PDEATHSIG` with `SIGKILL`; elsewhere nothing) and `take_stdout() -> File` (the process's stdout for its caller alone: a duplicate of descriptor 1, close-on-exec, which is the returned file; Windows: a duplicate of the standard output handle, not inheritable; then descriptor 1 leads to the null device, and on Windows the standard output handle and the C runtime's descriptor 1 as well, so a print, a `write(1, ..)` or a library's `printf` after the call reaches nobody; a process whose stdout carries a protocol calls it before it reads and writes only to the file). `AnonymousJob::assign(&Child)` (Windows) puts a child that is already running in the job (guarantee 12). Limits: the cap counts requested bytes, not the resident size (mappings of files and thread stacks are outside it); a block allocated before `arm` and freed after it credits its size, so the bound is `cap` plus what was held at arming, and `high_water()` then under-reports by the same amount; `arm` is called once, before another thread allocates; a request `System` refuses stays counted. `close_inherited` reaches every number only through Linux's `close_range`; the loop used elsewhere, and on a kernel that refuses the call, reaches only `getdtablesize()`, so a descriptor above a lowered soft limit stays open; it closes what the process's libraries hold as well, so it is a process's first act, before a thread starts or a library opens a descriptor it keeps, and on macOS it also closes what the frameworks a binary links opened before `main` (a guarded descriptor is a crash), a premise only a real child's test on a macOS runner can show. `end_with_parent` is Linux only, fires when the thread that started the child ends (not only when the parent process does) and does not notice a parent already gone before the call. `limit_cpu` and `close_inherited` have no Windows counterpart (the parent's wall clock and a job are its bounds). `take_stdout` is called after `close_inherited` (which would close its copy), once, before another thread writes to stdout; what still reaches the old stdout is a copy of descriptor 1 or of the handle made before the call, a Unix process forked without an exec (close-on-exec acts at exec) and, on Windows, a raw handle value cached before the call (the runtime's `_dup2` closes the original handle); a console or terminal opened by name is not stdout and is not touched; on an error the caller writes nothing to stdout (Unix: stdout unchanged and the copy closed; Windows: it may already lead to the null device); the explicit `SetStdHandle` is for a GUI-subsystem caller (in a console program `_dup2` sets the handle itself); stderr is untouched; the file is unbuffered, so each `write` is a system call. `exit_on_crash` does not cover a crash by another signal (`SIGSEGV` outside a stack overflow, `SIGBUS`, `SIGILL`); on Windows its `code` is unused. A job's assignment comes after the child's creation: until it returns the child, and anything it started in that time, is outside the job, so a caller assigns before it gives the child input, and kills and reaps the child when the assignment fails. The handlers use `_exit` and an atomic load alone (async-signal-safe). Seen (hosted runners macos-15 and windows-latest, 2026-11-11, a throwaway workflow on a probe branch, not kept): macOS enforces `RLIMIT_CPU` and delivers `SIGXCPU` (the CPU test exits with its code), and the `SIGABRT` handler turned an abort and a stack overflow into the code, which is the evidence that no crash happened (the folder listing had no control; it showed no new file in `~/Library/Logs/DiagnosticReports` or `/Library/Logs/DiagnosticReports` 30 s after the crash tests); that macOS has no memory limit was not tested, and the allocator does not rely on one. Windows: a child assigned to an `AnonymousJob` after `spawn` ended 50 ms after the job's handle closed, with the test process itself in a job. Linux, with `exit_on_crash` or the `SIGXCPU` handler removed: the child dies by signal with the core-dumped flag set (the tests set `RLIMIT_CORE` to 1 byte, which stops a piped dump as well, so no core is written). `take_stdout`, seen on the same two runners (2026-11-12, run 43140527564 of a probe branch, not kept): its test passed on both, so on macOS and on Windows a print, a raw write and, on Windows, a write to the standard output handle and to the runtime's descriptor 1 after the call never reached the parent, and the private copy's marker did; the drawing child's test of a print between replies passed on both; a second run (43142395356) passed the returned file's close-on-exec assertion on macOS and its not-inheritable assertion on Windows. Not tested: stdout as a console on Windows (the runners' is a pipe). Seen once on a Windows 11 computer (2026-11-12, the drawing child's crash tests on a debug build, error reporting at its defaults): a stack overflow after `exit_on_crash` ended the child with `0xc00000fd` in 0.15 to 1.3 s, with no error window, no error-reporting process, and no new crash dump, error report or Application-log event; other Windows settings (a machine-wide dump setting, error reporting set to always ask) were not tried.

Platform facts a caller meets and may not fix (item 57 of the 0.6.7 list): an MSYS program that execs another is a
new Windows process whose Windows parent has exited, so a native parent walk alone stops short of the session (Git
Bash's `/proc/<pid>/winpid` carries the chain); a Windows console is cp437 unless the process sets UTF-8, so a prompt
glyph read back through a pipe becomes `?` and a gate comparing a pane line to it never matches; an antivirus scanner
or indexer can hold a freshly written or extracted file open for seconds, so a publication's and a record's rename
retries within `RETRY_DEADLINE_MS` (a removal and the held folder's staging rename do not); the removal of a record's stale temporary file deletes with POSIX semantics, which frees its name though another program holds the file open with delete sharing, and a holder without delete sharing fails the removal's open at once, unretried; the updater's extraction retries longer; the background service on Linux is
the user manager's unit (`deploy/sotd.service`), and the runtime base is stable only while that user manager runs (linger),
so a boot without it binds sockets at `/tmp/sot-<uid>`.

## Owns
The folders and host name above (`host/state_dir.rs`); durable publication and remove (`host/durable.rs`); a record's durable write and the write beneath a root with its folder twin (`host/beneath.rs`, its arms `beneath_unix.rs` and `beneath_windows.rs`);
kernel locks and held folders (`host/lock.rs`, `host/held_dir.rs`); descriptor birth and the native process-birth
launcher (`host/fd.rs`, `host/process_tree/`, one native C file); the directory pin (`host/pinned_dir.rs`); the volume
preflight; storage recognition; `Redial`; the Windows descriptors and SIDs (`host/winsec.rs`, `host/winhandle.rs`) the anonymous kill-on-close job with its assignment (`host/winjob.rs`, guarantees 12 and 18) and the primitives of a process that bounds itself (`host/bound.rs`, `host/heap_cap.rs`, guarantee 18);
the private-dir and socket-dir checks; the local connection (`transport/`: `connect_own`, the two clients, the accept
policy, `Client` and `TransportError`) and the log masking (`secret.rs`); the test support every crate's tests share
(`test_wait`, `test_isolated`, `test_exec`, `test_log`, `test_scan`, `test_copy`, `test_foreign`, `test_hold` and, under
`native-barrier`, `test_barrier`); the resource folder (`resource_dir`, `SOT_RESOURCE_ROOT`) and the verbatim-path
strip; the peer challenge's OS steps, `PeerIdentity` and `PeerProcess`, the exchange trait and the deadline
(`identity/`); the account (`os_account`) and loopback owner (`peer_owner`). The crate's `Error` is `Io`, `Path` (an OS error with what was being done) or `State`; the
backend matches `State` for a held lock (`rows/run/end_run.rs`) and `Io` for an existing target (`agents/trust_declaration.rs`),
and upper crates otherwise convert it. The resource folder and the native launcher are owned APIs without a guarantee
of their own: `resource_dir` has no test (a gap), the launcher is proved by the backend's `native_premises` tests.

## One TOML parser (stated, not yet built)
The `toml` crate is the one parser. Four hand-written readers disagree today: hosts.toml (topology), the rows
registry, the window's settings and the window's resume file. Platform offers the crate and nothing more; each owner
converts its own reader in its own pass and then deletes it.

## Calls in
The OS (renameat2, renamex_np, MoveFileEx, flock, LockFileEx, SO_PEERCRED, pidfd, kqueue, the TCP owner tables), the
C compiler for the native launcher (build time), and std, libc, windows-sys (its `Wdk_Foundation`, `Wdk_Storage_FileSystem` and `Win32_System_Kernel` features for the Windows write beneath a root, `Win32_System_Diagnostics_Debug` for the error mode), tokio and tracing (peer_owner only),
gethostname, sha2, thiserror, getrandom; tracing-subscriber and tempfile for the test support alone.

## Calls out
Everything above; each name is a path under `sot_platform::`: `host::state_dir::{sot_state_dir, sot_config_dir,
runtime_sot_dir, host_name, kernel_host_name, state_dir_hash, is_private_dir}`, `host::{publish_noreplace,
write_durable, write_beneath, create_dir_beneath, open_regular, read_regular, remove_durable, lock_writer, lock_supervisor, try_lock_daemon, HeldDir,
remove_ended, preflight_volume,
storage_exhaustion, dir_identity, PinnedDir, inherit_none, harden_own_stdio}`, `host::fd::*`, `host::winjob::{AnonymousJob, JobError}` (Windows), `host::heap_cap::HeapCap`, `host::bound::{exit_now, close_inherited, exit_on_crash, limit_cpu, end_with_parent, take_stdout}`, `host::process_tree::*`,
`host::redial::{Redial, STABLE}`, `host::env::Env`, `identity::challenge::*`, `identity::exchange::{IdentityExchange,
ExchangeDecode}`, `identity::deadline::run_with_deadline`, `identity::os_account::own_account_id`,
`identity::peer_owner::serve_own`, `transport::{Client, TransportError, CONNECT_BOUND, transport_error_to_io}`,
`transport::connect_own::connect_own`, `transport::accept_policy::{accept_policy, AcceptAction, ACCEPT_SLEEP}`,
`transport::socket_unix::SocketClient`, `transport::pipe_win::PipeClient`, `secret::{redact, RedactingWriter}`, and the shell and script twins (`sot_host`, `check_remote_fs`) which follow these
rules and are owned by them.

## Does not
Know a row, the row socket's frames or an op (the `TransportError` variants for an endpoint reached
through a daemon, and for a resolved row path, are built by those endpoints, topology's and the capsule's, never here). Decide what a challenge outcome means for readiness or adoption
(the caller does). Decide when to restart (callers pass the floor and cap). Hold connections or children. Parse
TOML.

## Platforms
Per OS: Linux (pidfd, procfs start ticks, renameat2, per-child fork guard by O_CLOEXEC), macOS (kqueue exit watch,
LOCAL_PEERPID, pidversion, the fork birth lock, renamex_np), Windows (SDDL and SID helpers, MoveFileEx,
LockFileEx, GetExtendedTcpTable, impersonation-level probe). Bounding oneself: Unix signals and `rlimit`s, Linux's `close_range` and `PR_SET_PDEATHSIG`, Windows' error mode and job assignment. No fourth OS builds.

## Tests
One per guarantee. Paths are under `rust/platform/src/` unless they start `tests/` (platform's own integration tests) or
`rust/capsule/`: those are tests that need the capsule's row server or its `sot-capsule` binary (a real `RowServer` answering
the hello, `CARGO_BIN_EXE_sot-capsule`), which platform cannot depend on, and stay in `rust/capsule/tests/`. A test of a
Windows or macOS arm runs on the hosted runner for its OS.

| # | guarantee | test |
|---|---|---|
| 1 | one answer, `host_name` errs | `host/state_dir_tests.rs`: `the_state_folder_has_one_rule_and_the_config_folder_is_its_sibling_rule`, `the_daemons_state_folder_is_the_state_folder_and_falls_back_to_tmp_without_a_home` (and its Windows pair), `host_name_prefers_sot_self_host_when_set` and the three `host_name_falls_back_*`/`_takes_*` cases, `a_raw_host_name_gives_its_first_lowercase_label_or_an_error`; `host/state_dir_hash_tests.rs`; `host/state_dir_runtime_dir_tests.rs`; `identity/os_account_tests.rs` |
| 2 | durable, no clobber | the record's write (in `host/beneath_tests.rs`): `a_folder_swapped_before_the_create_keeps_the_record_in_the_opened_folder`, `a_folder_swapped_after_the_create_keeps_the_record_in_the_opened_folder` (Unix), `a_record_whose_folder_is_reached_through_a_link_is_written_in_the_target`, `a_file_at_the_records_folder_answers_at_once`, `a_fifo_at_the_records_folder_answers_at_once` (Unix); Windows, in `host/durable_tests.rs`: `a_stale_temporary_name_held_open_with_delete_sharing_does_not_stop_the_write`; `host/durable_tests.rs`: `publish_noreplace_refuses_an_existing_target_and_leaves_both_files`, `write_durable_replaces_a_record_and_leaves_no_temporary_file`, `remove_durable_is_idempotent_and_accepts_a_missing_folder`, `ensure_container_*` |
| 3 | kernel lock | `host/lock_tests.rs`: `a_dropped_writer_lock_is_free_while_a_forked_child_waits_to_exec`, `a_dropped_daemon_lock_is_free_while_a_forked_child_waits_to_exec`, `a_child_spawned_while_the_daemon_lock_is_held_never_holds_it`, `try_lock_daemon_excludes_a_second_holder`, `lock_supervisor_refuses_a_second_concurrent_holder` |
| 4 | held folder | `host/held_dir_tests.rs`: `a_held_folder_goes_once_its_maker_has_ended_and_only_then`, `a_held_folder_goes_while_a_removal_holds_its_lock_file_open` |
| 5 | cloexec births | `host/fd_tests.rs` (socket, pipe, pty, the fork birth lock); `tests/winhandle_windows.rs` |
| 6 | challenge | `identity/challenge_unix_tests.rs`, `identity/challenge_identity_tests.rs`, `identity/deadline_tests.rs`, `identity/peer_owner/tests.rs`, `identity/exit_watch_macos_tests.rs`; `rust/capsule/tests/challenge_unix.rs`, `challenge_macos.rs`, `macos_kernel_facts/`; `tests/other_account.rs` |
| 7 | storage codes | `host/storage_tests.rs`: `storage_codes_are_recognized_by_native_code_only`; `rust/capsule/src/storage_tests.rs`: `a_transport_error_is_never_storage_exhaustion` |
| 8 | volume preflight | `host/volume_tests.rs` |
| 9 | redial | `host/redial_tests.rs`: `a_short_connection_keeps_the_doubling`, `a_connection_that_lasted_stable_starts_over`, `a_reset_starts_over` |
| 10 | dir pin | `host/pinned_dir_tests.rs`: `a_fifo_fails_at_once_and_never_waits_for_a_writer`, `a_directory_pin_rejects_an_ordinary_file`, `a_borrowed_directory_pin_keeps_its_folder_after_an_ancestor_swap` |
| 11 | private dir | `host/state_dir_is_private_dir_tests.rs`; `host/paths_secure_private_dir_tests.rs`: `a_missing_parent_is_created_and_the_leaf_is_private`, `ensure_private_dir_makes_an_owner_only_folder_that_is_private` |
| 12 | job ends at last close (Windows) | `rust/capsule/tests/conpty.rs`: `containment_kills_child_and_grandchild` (`:147`); `rust/capsule/tests/capsule/windows_only.rs`: `the_agent_job_refuses_a_breakaway` (`:488`); containment's `contain_tests.rs` `contain_job_permits_no_breakaway` reads the flags only |
| 13 | local connection | `transport/accept_policy_tests.rs`; `tests/connect_own.rs`: `a_socket_this_account_listens_on_is_reached_wherever_it_is`, `a_socket_another_account_listens_on_is_refused`, `a_full_backlog_ends_the_connect_within_its_bound`, `a_missing_socket_is_not_found` (and the Windows pipe cases); the capsule's `rust/capsule/tests/socket_unix/` and `rust/capsule/tests/pipe_win/` (cancel, concurrent submit, the connector's bounds), `rust/capsule/tests/socket_unix/cloexec.rs` |
| 14 | secret masking | `secret_tests.rs`: `a_slack_token_is_masked_whole_and_a_bare_prefix_is_not`, `a_logged_secret_is_masked_in_the_file`, `only_a_run_of_exactly_32_lowercase_hex_characters_is_masked`, `one_write_call_masks_the_whole_buffer_and_reports_it_all_written` |
| 15 | regular open, bounded read | `host/regular_tests.rs`: `a_regular_file_reads_up_to_the_cap_and_no_further`, `a_planted_fifo_or_device_is_refused_without_a_wait` |
| 16 | hold tool | `hold_tests.rs` (child cases through a rerun of the test binary: `a_child_started_with_the_folder_is_held_at_its_line_and_released`, `two_children_sharing_a_folder_take_distinct_tickets`, `the_arm_drop_releases_a_held_child`; the cases by name: `a_held_arrival_waits_at_its_line_until_released`, `an_arrival_nobody_releases_gives_up_within_its_bound_and_fails_the_arm`, `an_async_point_holds_under_paused_time`, ...); the name rule is `hold.rs`'s `compile_fail` doc test |
| 17 | write beneath a root, and its folder twin | `host/beneath_tests.rs`: `a_link_at_any_folder_takes_no_write`, `the_root_is_refused_when_its_entry_is_a_link_or_it_is_not_canonical`, `a_name_that_is_not_one_plain_entry_is_refused`, `a_planted_temporary_name_is_never_written_through`, `a_link_at_the_final_name_is_replaced_not_followed` (file and folder links), `a_temporary_file_replaced_before_the_rename_writes_nothing_outside` (Unix), `a_root_that_does_not_resolve_answers_by_what_is_wrong`, `each_write_creates_its_temporary_file_under_a_fresh_name`, `missing_folders_are_created_and_the_path_is_returned`, `a_folder_swapped_for_a_link_after_its_open_keeps_the_write_inside`, `a_folder_swapped_for_a_link_before_its_open_takes_no_write`, `a_missing_folder_is_opened_after_its_make_like_any_folder`, `a_folder_with_another_below_it_swapped_after_its_open_keeps_the_write_inside`, `a_folder_removed_after_its_open_takes_no_write`, `a_final_name_planted_before_the_rename_is_replaced_not_followed`, `an_error_at_every_step_leaves_no_temporary_file`, `two_writers_of_one_name_each_publish_a_whole_file`, `a_writer_of_one_name_publishes_while_another_syncs_its_folder`, `a_folder_at_the_final_name_is_an_error_and_is_kept`; `create_dir_beneath`: `a_folder_is_created_at_its_name_and_the_path_is_returned`, `a_link_at_any_parent_takes_no_folder`, `an_entry_at_the_name_is_already_exists`, `a_missing_parent_is_not_found_and_nothing_is_made`, `a_name_that_is_not_one_plain_entry_takes_no_folder`, `a_parent_swapped_for_a_link_after_its_open_takes_no_folder`; Windows: `the_temporary_file_cannot_be_renamed_away_while_it_is_written`, `a_root_spelled_in_another_case_is_its_own_canonical_path`, `a_final_name_a_reader_holds_open_is_replaced`, `a_final_name_held_without_delete_sharing_fails_the_write`; every system: `only_a_name_surrogate_tag_is_a_link` |
| 18 | bound self, job assignment | `host/heap_cap_tests.rs`: `an_armed_heap_cap_ends_the_process_with_its_code`, `a_heap_cap_counts_realloc_and_zeroed`, `a_heap_cap_counts_what_is_held_and_reports_its_high_water`, `one_request_larger_than_the_cap_cannot_switch_the_cap_off`, `a_heap_cap_counts_growth_not_size_credits_a_shrink_and_keeps_the_peak`; `host/bound_tests.rs`: `exit_now_ends_with_its_code`, `close_inherited_leaves_no_descriptor_above_stderr` (Unix), `an_abort_after_exit_on_crash_is_an_exit` (Unix; an abort and a stack overflow), `a_process_past_its_cpu_limit_exits_with_its_code` (Unix), `a_child_ends_with_its_parent` (Linux), `only_the_private_copy_reaches_the_parents_stdout` (a print, a raw write and, on Windows, the handle and the runtime's descriptor, all after the call; the parent sees the private copy's marker alone; the copy is close-on-exec, on Windows not inheritable), `exit_on_crash_sets_the_windows_error_mode` (Windows; the mode, not the dialog); `host/winjob_tests.rs`: `an_assigned_child_ends_with_its_job` (Windows). Each body that ends its own process runs as a child of its test (`bound_tests.rs`'s `run_child`, the entry record of `test_isolated`), because `run_isolated` wants exit 0 |

## Rules
- Every subscriber a test installs goes through `test_log`: `capture()` reads this thread's events without formatter
  timestamps or colour; `install()` preserves the supplied subscriber's formatting and timer. Behavioral tests exercise
  first-callsite routing, parallel captures and guard restoration; new callers use this owner. Existing exceptions, not
  edited here: the wake's `comm/wake/attempt_tests.rs` and the updater's `update_tests.rs` install a `tracing_subscriber`
  directly.
- A Rust source-text scan that exists reads through `test_scan`, and none cuts a file at its first `#[cfg(test)]`
  (`test_scan_tests.rs`: `no_scan_cuts_a_file_at_its_first_cfg_test`); an existing scan is never extended, and no new
  test proves what code does by reading source text.
- A fixture selects its executable on the child command. T2's actual SSH fixtures compare the parent PATH and SHELL before, during and after execution; no lexical allowlist proves environment preservation.
- Test-program writers use `sot_platform::test_exec::write_executable` (feature `test-support`; its Linux FIFO proof observes actual parent descriptors while the child writer is active), a copy of a built program included (`write_executable(&to, std::fs::read(&from)?)`, never `std::fs::copy`, whose writer is the test process); its separate Unix writer leaves no writable program descriptor in the test process. The retained real descriptor test proves this owner property; `std::fs::copy` is in `rust/clippy-tests/clippy.toml`'s disallowed methods, so a copy of a program is a compile error in the clippy all-targets run, and a data copy carries `#[allow(clippy::disallowed_methods, reason = ...)]` on its statement.
- Named test reruns use `test_isolated`; direct fixtures retain child wait/status, termination, entry and separate output observations before raising failures. Regression proofs require observed prerequisites and the selected failure's exact cause; wrong-cause controls use the same verifier. Duration/deadline APIs share one wait implementation; byte output preserves valid text and escapes invalid UTF-8. Wrapped fixtures retain launcher status and both streams, validate the native role/PID witness against exact entry, and share direct fixtures' wait/drain/finalization implementation; ordinary direct fixtures still validate their spawned child PID.
