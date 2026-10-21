//! ADR 0049 `## User isolation`: the shell's `sot_dial` and `sot_oneshot_request`, and the launch scripts'
//! `sot_socket_open`, reach a local endpoint only through the built `sotd stdio-bridge`, whose `connect_own` admits
//! only an endpoint this OS account serves; `sot_ssh_bridge` runs the far box's own bridge.

#[path = "support/sotd.rs"]
#[allow(dead_code, reason = "this suite passes sotd_program to bash and does not call sotd_command")]
mod sotd;

use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(20);

fn bash() -> Option<std::path::PathBuf> {
    if !cfg!(windows) {
        return Some("bash".into());
    }
    let found = std::env::var_os("ProgramFiles")
        .map(|p| std::path::PathBuf::from(p).join("Git/bin/bash.exe"))
        .filter(|p| p.exists());
    assert!(found.is_some() || std::env::var_os("CI").is_none(), "CI has no Git Bash at %ProgramFiles%\\Git\\bin\\bash.exe");
    found
}

/// Runs `script` under bash with comm-lib.sh's path as `$1` and `endpoint` as `$2`. The launch scripts' library is
/// beside it, at `${1%/comm/lib/comm-lib.sh}/scripts/lib/sot-daemon.sh`.
fn shell(home: &std::path::Path, script: &str, endpoint: &str) -> Option<Child> {
    let mut cmd = Command::new(bash()?);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("SOT_") {
            cmd.env_remove(key);
        }
    }
    let lib = format!("{}/../../comm/lib/comm-lib.sh", env!("CARGO_MANIFEST_DIR")).replace('\\', "/");
    cmd.args(["-c", script, "shell-dial", &lib, endpoint])
        .env("HOME", home)
        .env("SOT_COMM_HOME", home.join("comm"))
        .env("XDG_RUNTIME_DIR", home)
        .env("SOT_SELF_HOST", "shell-test")
        .env("SOTD_BIN", sotd::sotd_program().to_string_lossy().replace('\\', "/"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Some(cmd.spawn().expect("start bash"))
}

fn ping(mut child: Child) {
    let mut input = child.stdin.take().expect("bash stdin");
    let output = child.stdout.take().expect("bash stdout");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(output).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    // A parent with no sot_dial may exit before consuming this write; its stderr supplies the promised red.
    let _ = input.write_all(b"ping\n").and_then(|()| input.flush());
    let reply = rx.recv_timeout(BOUND).expect("the shell reply within its bound").expect("read the shell reply");
    drop(input);
    let (status, _, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    let said = format!("{status}; stderr {stderr:?}");
    assert!(status.success(), "sot_dial failed: {said}");
    assert_eq!(reply, "pong\n", "the bridge carries the reply: {said}");
}

/// ADR 0049 `## User isolation`: every shell path that opens a local socket refuses one another OS account listens on,
/// and that listener gets no byte: `sot_oneshot_request`, `sot_dial` with and without its bound, and the launch
/// scripts' `sot_socket_open` (scripts/lib/sot-daemon.sh, which restart-backend.sh runs too). Each path gets a listener
/// of its own: one listener's backlog of 1 would refuse a third connection for being full, not for its account.
#[cfg(unix)]
#[test]
fn a_shell_request_to_a_socket_another_account_listens_on_writes_nothing() {
    if !sot_log::test_isolated::run_isolated(
        "a_shell_request_to_a_socket_another_account_listens_on_writes_nothing",
    ) {
        return;
    }
    let home = tempfile::tempdir().expect("scratch home");
    for (path, line) in [
        (
            "oneshot",
            r#"ENDPOINT="$2" SOT_SEND_TIMEOUT=3 sot_oneshot_request '{"v":1,"id":1,"kind":"req","op":"version.query","payload":{}}' version.query"#,
        ),
        ("dial", r#"sot_dial "$2""#),
        ("bounded dial", r#"sot_dial "$2" 5"#),
        (
            "socket open",
            r#"( . "${1%/comm/lib/comm-lib.sh}/scripts/lib/sot-daemon.sh" && sot_socket_open "$SOTD_BIN" "${2#unix:}" )"#,
        ),
    ] {
        let Some(foreign) = sot_log::test_foreign::ForeignListener::start(false) else {
            return;
        };
        let script = format!(". \"$1\" || exit 1\n{line} && echo reached || echo refused\n");
        let mut child = shell(
            home.path(),
            &script,
            &format!("unix:{}", foreign.path.display()),
        )
        .expect("bash on Unix");
        drop(child.stdin.take());
        let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
        let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
        assert_eq!(foreign.finish(), 0, "{path}: the shell client sent another account's listener bytes (ADR 0049 `## User isolation`)");
        assert!(status.success(), "{path}: bash: {said}");
        assert_eq!(stdout, "refused\n", "{path}: {said}");
        // sot_socket_open discards the bridge's stderr; its socket exists, so only the bridge's refusal prints
        // `refused`.
        assert!(
            path == "socket open" || stderr.contains("another OS account listens on this socket"),
            "{path}: {said}"
        );
    }
}

/// This account's socket, in a folder private to it, is reached by every shell path that opens one: `sot_dial` with and
/// without its bound carries a request and its reply, and `sot_socket_open`'s bridge, its input empty, connects and
/// sends nothing.
#[cfg(unix)]
#[test]
fn the_shell_dial_reaches_a_socket_in_a_private_folder() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;
    let home = tempfile::Builder::new().prefix("sotsd-")
        .permissions(std::fs::Permissions::from_mode(0o700)).tempdir_in("/tmp").expect("private scratch home");
    assert!(sot_log::host::state_dir::is_private_dir(home.path()));
    let path = home.path().join("s.sock");
    let listener = UnixListener::bind(&path).expect("bind the private listener");
    listener.set_nonblocking(true).expect("nonblocking listener");
    // One connection per path, in the order below, and what each sent. A "ping" gets "pong", and every connection stays
    // open until its caller closes it, as the real bridge's contract requires.
    let (sent_tx, sent_rx) = channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + BOUND;
        let mut sent = Vec::new();
        for _ in 0..3 {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "the shell never connected within its bound"
                        );
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => panic!("accept the shell: {e}"),
                }
            };
            // macOS gives an accepted socket its listener's O_NONBLOCK (Linux does not), so the stream is made
            // blocking. Its reads are bounded by the test's wait for this thread's result below, never by a read
            // timeout: Darwin refuses SO_RCVTIMEO on an accepted AF_UNIX socket (EINVAL; see
            // rust/log/tests/macos_kernel_facts/peertoken.rs).
            stream
                .set_nonblocking(false)
                .expect("a blocking stream for the server's reads");
            let mut line = String::new();
            BufReader::new(&mut stream)
                .read_line(&mut line)
                .expect("read the caller's line");
            if line == "ping\n" {
                stream.write_all(b"pong\n").expect("write pong");
            }
            let mut tail = Vec::new();
            stream
                .read_to_end(&mut tail)
                .expect("read the caller's EOF");
            sent.push(line);
        }
        let _ = sent_tx.send(sent);
    });
    let endpoint = format!("unix:{}", path.display());
    ping(shell(home.path(), r#". "$1" && sot_dial "$2""#, &endpoint).expect("bash on Unix"));
    ping(shell(home.path(), r#". "$1" && sot_dial "$2" 5"#, &endpoint).expect("bash on Unix"));
    let script = r#". "${1%/comm/lib/comm-lib.sh}/scripts/lib/sot-daemon.sh" && sot_socket_open "$SOTD_BIN" "${2#unix:}" && echo open"#;
    let mut child = shell(home.path(), script, &endpoint).expect("bash on Unix");
    drop(child.stdin.take());
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    assert!(
        status.success() && stdout == "open\n",
        "sot_socket_open did not reach this account's socket: {status}; stdout {stdout:?}; stderr {stderr:?}"
    );
    let sent = sent_rx
        .recv_timeout(BOUND)
        .expect("the server's three connections ended within the bound");
    assert_eq!(sent, ["ping\n", "ping\n", ""], "what each path sent");
}

/// `sot_ssh_bridge`, `sot_dial`'s path to another computer: ssh runs that box's own `sotd stdio-bridge`, with
/// `--host <host>` for a hub's relay, found first in its install folder, and carries the bytes both ways, with and
/// without the bound. A stand-in `ssh` runs the far command through `sh` here, as the far box's shell would, and a
/// stand-in far `sotd` prints its arguments and then echoes its input. The far bridge's own `connect_own` is
/// `stdio_bridge.rs`'s to test.
#[test]
fn the_ssh_bridge_runs_the_far_boxs_own_bridge() {
    let Some(_) = bash() else { return };
    let home = tempfile::tempdir().expect("scratch home");
    let stub = home.path().join("stub-ssh");
    let far = home.path().join(".local/share/sot/bin");
    std::fs::create_dir_all(&stub).expect("mkdir stub-ssh");
    std::fs::create_dir_all(&far).expect("mkdir the far install folder");
    sot_log::test_exec::write_executable(
        &stub.join("ssh"),
        "#!/bin/sh\ncase \"$1\" in -G) exit 0 ;; esac\nfor far in \"$@\"; do :; done\nexec sh -c \"$far\"\n",
    );
    sot_log::test_exec::write_executable(
        &far.join("sotd"),
        "#!/bin/sh\necho \"far sotd $*\"\nexec cat\n",
    );
    let script = r#". "$1" && PATH="$HOME/stub-ssh:$PATH"
printf 'plain\n' | sot_ssh_bridge box
printf 'bounded\n' | sot_ssh_bridge box "" 5
printf 'relay\n' | sot_ssh_bridge box far1 5
printf 'dial\n' | sot_dial ssh:box/far1 5
"#;
    let mut child = shell(home.path(), script, "").expect("bash");
    drop(child.stdin.take());
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert!(status.success(), "bash: {said}");
    assert_eq!(
        stdout,
        "far sotd stdio-bridge\nplain\nfar sotd stdio-bridge\nbounded\nfar sotd stdio-bridge --host far1\nrelay\nfar sotd stdio-bridge --host far1\ndial\n",
        "{said}"
    );
}

/// The owners of the command bound, each named with the line that defines `sot_bounded` for a case: comm-lib's, its one
/// copy (the installer bounds its one ssh probe in `sotd install`).
fn owners() -> Vec<(&'static str, &'static str)> {
    vec![("comm-lib", r#". "$1" || exit 1"#)]
}

/// How long a bound case's bash may run before the test calls it hung. Bash's start, the owner's sourcing and every
/// spawn count here, and a loaded Git Bash runner slows each by seconds (hosted Windows run 38252414920 spent about ten
/// in start-up alone). The ceilings that judge the bound are timed inside the case (`CASE_CLOCK`); this only ends a hang.
const HUNG: Duration = Duration::from_secs(120);

/// The case's own clock: `t0=$(now_ms)` before the call it times and `took_since "$t0"` after it write the call's time
/// to `$HOME/took.ms`, read by perl, the interpreter the bound itself runs on. So a ceiling measures the bound under
/// test, never bash's start or the owner's sourcing, which took about ten seconds on a loaded hosted Windows runner.
const CASE_CLOCK: &str = r#"now_ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time * 1000'; }
took_since() { echo $(( $(now_ms) - $1 )) > "$HOME/took.ms"; }"#;

/// Runs `script` after `source` (an [`owners`] line) and [`CASE_CLOCK`] under this platform's bash, within `HUNG`,
/// and returns its stdout, the time of the call it timed, and its status and both streams for the caller's messages.
/// Every line is printed, in a passing run's log too (CI runs the tests with --nocapture). A script that does not
/// finish within `HUNG`, a bash that fails, or a script that timed no call fails the caller.
fn bound_case(source: &str, script: &str) -> (String, Duration, String) {
    let home = tempfile::tempdir().expect("scratch home");
    let mut child = shell(home.path(), &format!("{source}\n{CASE_CLOCK}\n{script}"), "").expect("bash");
    drop(child.stdin.take());
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(HUNG);
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert!(status.success(), "bash failed: {said}");
    let took = std::fs::read_to_string(home.path().join("took.ms"))
        .ok()
        .and_then(|ms| ms.trim().parse().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| panic!("the case timed no call: {said}"));
    eprintln!("the case's lines:\n{stdout}its stderr:\n{stderr}its call took {took:?}");
    (stdout, took, said)
}

/// `sot_bounded` keeps its deadline while CMD's group outlives CMD: CMD exits 0 at once, a descendant still holds the
/// captured output, and the bound ends the group at its deadline (124), naming CMD's own status on stderr. Every
/// platform (on Windows, for Git Bash's own programs).
#[test]
fn the_comm_bound_holds_a_group_whose_leader_exited() {
    let Some(_) = bash() else { return };
    for (owner, source) in owners() {
        let (lines, took, said) = bound_case(
            source,
            r#"t0=$(now_ms); out="$(sot_bounded 1 sh -c 'sleep 30 & echo $! > "$HOME/lead.pid"; exit 0')"; rc=$?; took_since "$t0"; echo "leader exit $rc"
kill -0 "$(cat "$HOME/lead.pid")" 2>/dev/null && echo "leader exit alive" || echo "leader exit gone"
"#,
        );
        assert_eq!(lines, "leader exit 124\nleader exit gone\n", "{owner}: {said}");
        assert!(took < Duration::from_secs(10), "{owner}: the bound returned only after {took:?}");
    }
}

/// `sot_bounded` ends CMD when CMD itself has left its process group: the bound signals CMD as well as its group, and
/// never waits on it without a bound. Unix only: Git Bash emulates process groups.
#[cfg(unix)]
#[test]
fn the_comm_bound_ends_a_command_that_left_its_group() {
    for (owner, source) in owners() {
        let (lines, took, said) = bound_case(
            source,
            r#"t0=$(now_ms); sot_bounded 1 sh -c 'echo $$ > "$HOME/leave.pid"; exec perl -e "setpgrp(0, getpgrp(getppid())) or die; exec q(sleep), q(30)"'; rc=$?; took_since "$t0"; echo "leave $rc"
kill -0 "$(cat "$HOME/leave.pid")" 2>/dev/null && echo "leave alive" || echo "leave gone"
"#,
        );
        assert_eq!(lines, "leave 124\nleave gone\n", "{owner}: {said}");
        assert!(took < Duration::from_secs(10), "{owner}: the bound returned only after {took:?}");
    }
}

/// The bound's grace periods run on the clock, not on a count of sleeps: a command that ignores TERM ends with 137 one
/// grace period (1 s) after its 1 s bound, and is gone. Unix only, as KILL escalation is.
#[cfg(unix)]
#[test]
fn the_comm_bound_kills_a_deaf_command_one_grace_period_after_its_bound() {
    for (owner, source) in owners() {
        let (lines, took, said) = bound_case(
            source,
            r#"t0=$(now_ms); sot_bounded 1 sh -c 'trap "" TERM; echo $$ > "$HOME/deaf.pid"; exec sleep 30'; rc=$?; took_since "$t0"; echo "deaf $rc"
kill -0 "$(cat "$HOME/deaf.pid")" 2>/dev/null && echo "deaf alive" || echo "deaf gone"
"#,
        );
        assert_eq!(lines, "deaf 137\ndeaf gone\n", "{owner}: {said}");
        assert!(
            took >= Duration::from_millis(1900) && took < Duration::from_millis(3500),
            "{owner}: a 1 s bound and a 1 s grace period took {took:?}"
        );
    }
}

/// A command that catches TERM and exits on it with a status of its own (ssh in its session loop exits 255) still
/// ends with the bound's 124, so its caller can name the expiry, and the group goes with it. Unix only, as the other
/// signal cases.
#[cfg(unix)]
#[test]
fn the_bound_names_its_expiry_when_the_command_catches_term() {
    for (owner, source) in owners() {
        let (lines, took, said) = bound_case(
            source,
            r#"t0=$(now_ms); sot_bounded 1 bash -c 'sleep 30 & s=$!; echo $s > "$HOME/catch.pid"; trap "kill $s; exit 255" TERM; wait'; rc=$?; took_since "$t0"; echo "catch $rc"
kill -0 "$(cat "$HOME/catch.pid")" 2>/dev/null && echo "catch alive" || echo "catch gone"
"#,
        );
        assert_eq!(lines, "catch 124\ncatch gone\n", "{owner}: {said}");
        assert!(took < Duration::from_secs(10), "{owner}: the bound returned only after {took:?}");
    }
}

/// Only the command's own process asks for its group, and it trusts the group it is then in, not setpgid's status: on
/// macOS 26 a setpgid by the parent for the same child, at the same moment, made either call fail with EPERM though the
/// group was made, and the command never ran (hosted run 39218378845). A module PERL5OPT loads into the bound's perl
/// stands in for that kernel: in the forked child its setpgrp makes the group and answers EPERM, so the command runs in
/// its own group with its own status, or answers EPERM alone, so nothing runs (125); in the bound's own perl it writes
/// down every call, and there is none. Unix only, as the other group cases.
#[cfg(unix)]
#[test]
fn the_comm_bound_trusts_the_group_not_the_status_of_its_call() {
    for (owner, source) in owners() {
        let (lines, _, said) = bound_case(
            source,
            r#"mkdir -p "$HOME/lib"
cat > "$HOME/lib/Misreport.pm" <<'PM'
package Misreport;
use Errno ();
my $bound = $$;
*CORE::GLOBAL::setpgrp = sub {
    if ($$ == $bound) {
        if (open(my $log, ">>", "$ENV{HOME}/bound.calls")) { print $log "@_\n"; close $log; }
        return CORE::setpgrp($_[0], $_[1]);
    }
    CORE::setpgrp(0, 0) if $ENV{MISREPORT} eq "made";
    $! = Errno::EPERM();
    return 0;
};
1;
PM
misreport() { (export PERL5LIB="$HOME/lib" PERL5OPT=-MMisreport MISREPORT="$1"; sot_bounded 5 sh -c 'echo "$$ $(ps -o pgid= -p $$ | tr -d " ")" > "$HOME/$MISREPORT.ran"; exit 7'); }
t0=$(now_ms); misreport made; rc=$?; took_since "$t0"; echo "made $rc"
read -r pid pgid < "$HOME/made.ran" && [ "$pid" = "$pgid" ] && echo "made led its group" || echo "made led no group"
misreport none; echo "none $?"
[ -e "$HOME/none.ran" ] && echo "none ran" || echo "none never ran"
[ -e "$HOME/bound.calls" ] && echo "the bound asked for $(cat "$HOME/bound.calls")" || echo "the bound never asked"
"#,
        );
        assert_eq!(
            lines,
            "made 7\nmade led its group\nnone 125\nnone never ran\nthe bound never asked\n",
            "{owner}: {said}"
        );
    }
}

/// `sot_bounded`, the bound each of the four timed comm calls runs under, on this platform's bash and the perl first on
/// its PATH, the interpreter those calls use: the first line shows it is perl 5 and loads POSIX. One perl process owns
/// the deadline and the command's process group. The command keeps the shell's stdin and its own status; a late command
/// returns 124 and is gone; a descendant holding a captured command's output is ended at the bound; an errexit caller
/// ends with 124; a caller killed mid-bound leaves no command behind; with no perl or a bound of 0 nothing runs (125);
/// and a command that cannot start returns 127. On Unix also: a TERM-ignoring descendant holding captured output after
/// its leader died ends with 137, and a TERM sent to the bound itself reaches the group and is escalated to KILL
/// (143). Windows' Git Bash emulates signals and process groups for its own programs, so those two are not claimed
/// there, and no case here runs a native Windows program. A group that outlives its leader, a command that leaves its
/// group and the grace period's length are the three tests above. Every command the bound must end is a `sleep 30`, so
/// a run that fails leaves nothing running for longer than that.
#[test]
fn the_comm_bound_owns_its_command_until_its_group_ends() {
    let Some(_) = bash() else { return };
    for (owner, source) in owners() {
        the_bound_owns_its_command(owner, source);
    }
}

fn the_bound_owns_its_command(owner: &str, source: &str) {
    let home = tempfile::tempdir().expect("scratch home");
    let (unix, unix_want) = if cfg!(windows) {
        ("", "")
    } else {
        (
            r#"deafheld="$(sot_bounded 1 sh -c '(trap "" TERM; exec sleep 30) & echo $! > "$HOME/deafheld.pid"; wait')"; echo "deaf held $?"
kill -0 "$(cat "$HOME/deafheld.pid")" 2>/dev/null && echo "deaf held alive" || echo "deaf held gone"
sot_bounded 5 sh -c 'trap "" TERM; echo $$ > "$HOME/cancel.pid"; exec sleep 30' &
c=$!
for _ in $(seq 1 100); do [ -s "$HOME/cancel.pid" ] && break; sleep 0.05; done
kill -TERM "$(ps -o ppid= -p "$(cat "$HOME/cancel.pid")" | tr -d ' ')"
wait "$c"; echo "cancel $?"
kill -0 "$(cat "$HOME/cancel.pid")" 2>/dev/null && echo "cancel alive" || echo "cancel gone"
"#,
            "deaf held 137\ndeaf held gone\ncancel 143\ncancel gone\n",
        )
    };
    let want = format!(
        "perl 5 with POSIX\nin\nstatus 7\nlate 124\nlate gone\nheld 124\nheld gone\nerrexit 124\norphan gone\nno perl 125\n{unix_want}missing 127\nzero 125\n"
    );
    let script = format!(
        r#"{source}
mkdir -p "$HOME/no-perl"
echo "the bound's perl: $(command -v perl), $(perl -e 'print $^V')" >&2
perl -MPOSIX -e 'printf "perl %d with POSIX\n", $]'
printf 'in\n' | sot_bounded 5 cat
sot_bounded 5 sh -c 'exit 7'; echo "status $?"
sot_bounded 1 sh -c 'echo $$ > "$HOME/late.pid"; exec sleep 30'; echo "late $?"
kill -0 "$(cat "$HOME/late.pid")" 2>/dev/null && echo "late alive" || echo "late gone"
held="$(sot_bounded 1 sh -c 'sleep 30 & echo $! > "$HOME/held.pid"; wait')"; echo "held $?"
kill -0 "$(cat "$HOME/held.pid")" 2>/dev/null && echo "held alive" || echo "held gone"
( set -e; sot_bounded 1 sh -c 'exec sleep 30'; echo unreachable ); echo "errexit $?"
( sot_bounded 1 sh -c 'echo $$ > "$HOME/orphan.pid"; exec sleep 30' ) &
w=$!
for _ in $(seq 1 100); do [ -s "$HOME/orphan.pid" ] && break; sleep 0.05; done
kill "$w"
for _ in $(seq 1 100); do kill -0 "$(cat "$HOME/orphan.pid")" 2>/dev/null || break; sleep 0.05; done
kill -0 "$(cat "$HOME/orphan.pid")" 2>/dev/null && echo "orphan alive" || echo "orphan gone"
( PATH="$HOME/no-perl"; sot_bounded 5 true 2>/dev/null ); echo "no perl $?"
{unix}sot_bounded 5 /no/such/command 2>/dev/null; echo "missing $?"
sot_bounded 0 true 2>/dev/null; echo "zero $?"
"#
    );
    let mut child = shell(home.path(), &script, "").expect("bash");
    drop(child.stdin.take());
    // About 8 s of one-second bounds and their grace periods on Unix.
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(HUNG);
    // Every line, in the log of a run that passes too (CI runs the tests with --nocapture).
    eprintln!("the bound test's lines ({owner}):\n{stdout}its stderr:\n{stderr}");
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert!(status.success(), "the bound ({owner}): bash failed: {said}");
    assert_eq!(stdout, want, "the bound ({owner}): {said}");
}

/// Windows: every shell path that opens a pipe, `sot_dial` with and without its bound, reaches a pipe this account
/// serves, carrying a request and its reply, and refuses one another account serves (epmapper, SYSTEM's).
#[cfg(windows)]
#[test]
fn the_shell_dial_reaches_only_a_pipe_this_account_serves() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ServerOptions;
    let Some(_) = bash() else { return };
    let home = tempfile::tempdir().expect("scratch home");
    for (form, dial) in [
        ("unbounded", r#". "$1" && sot_dial "$2""#),
        ("bounded", r#". "$1" && sot_dial "$2" 5"#),
    ] {
        let runtime = tokio::runtime::Runtime::new().expect("pipe runtime");
        let name = format!("sot-shell-dial-{}-{form}", std::process::id());
        let path = format!(r"\\.\pipe\{name}");
        let server = runtime
            .block_on(async { ServerOptions::new().first_pipe_instance(true).create(&path) })
            .expect("test's pipe");
        let echo = std::thread::spawn(move || {
            runtime.block_on(async move {
                tokio::time::timeout(BOUND, async move {
                    server.connect().await.expect("accept the shell");
                    let mut stream = tokio::io::BufReader::new(server);
                    let mut line = String::new();
                    stream.read_line(&mut line).await.expect("read ping");
                    assert_eq!(line, "ping\n");
                    stream
                        .get_mut()
                        .write_all(b"pong\n")
                        .await
                        .expect("write pong");
                    let mut tail = Vec::new();
                    stream
                        .read_to_end(&mut tail)
                        .await
                        .expect("read the bridge's EOF");
                })
                .await
                .expect("pipe echo within its bound");
            })
        });
        ping(shell(home.path(), dial, &format!("pipe:{name}")).expect("Git Bash"));
        echo.join().expect("pipe echo server");
        let mut child = shell(home.path(), dial, r"pipe:\\.\pipe\epmapper").expect("Git Bash");
        drop(child.stdin.take());
        let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
        let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
        assert!(
            !status.success(),
            "{form}: another account's pipe is refused: {said}"
        );
        assert!(stderr.contains("not connecting"), "{form}: {said}");
    }
}
