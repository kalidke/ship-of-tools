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
    assert!(status.success(), "sot_dial failed: {stderr}");
    assert_eq!(reply, "pong\n", "the bridge carries the reply");
}

/// Every shell path that opens a local socket refuses one outside a private folder, and its listener is never connected
/// to: `sot_oneshot_request`, `sot_dial` with and without its bound, and the launch scripts' `sot_socket_open`
/// (scripts/lib/sot-daemon.sh, which restart-backend.sh runs too).
#[cfg(unix)]
#[test]
fn a_shell_request_to_a_socket_outside_a_private_folder_writes_nothing() {
    use std::os::unix::net::UnixListener;
    let path = std::path::PathBuf::from(format!("/tmp/sot-shell-dial-{}.sock", std::process::id()));
    assert!(!sot_log::host::state_dir::is_private_dir(std::path::Path::new("/tmp")));
    let listener = UnixListener::bind(&path).expect("bind the public-folder listener");
    listener.set_nonblocking(true).expect("nonblocking listener");
    let home = tempfile::tempdir().expect("scratch home");
    let endpoint = format!("unix:{}", path.display());
    let script = r#". "$1" || exit 1
ENDPOINT="$2" SOT_SEND_TIMEOUT=3 sot_oneshot_request '{"v":1,"id":1,"kind":"req","op":"version.query","payload":{}}' version.query && echo "oneshot reached" || echo "oneshot refused"
sot_dial "$2" && echo "dial reached" || echo "dial refused"
sot_dial "$2" 5 && echo "bounded dial reached" || echo "bounded dial refused"
( . "${1%/comm/lib/comm-lib.sh}/scripts/lib/sot-daemon.sh" && sot_socket_open "$SOTD_BIN" "${2#unix:}" ) && echo "socket open reached" || echo "socket open refused"
"#;
    let mut child = shell(home.path(), script, &endpoint).expect("bash on Unix");
    drop(child.stdin.take());
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    let accepted = listener.accept();
    std::fs::remove_file(&path).expect("remove the test's socket");
    assert!(matches!(accepted, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "a shell path connected to a socket whose folder is not private to this OS account (ADR 0049 `## User isolation`)");
    assert!(status.success(), "bash: {stderr}");
    assert_eq!(
        stdout, "oneshot refused\ndial refused\nbounded dial refused\nsocket open refused\n",
        "{stderr}"
    );
    assert!(
        stderr.contains("is not a private folder of this OS account"),
        "{stderr}"
    );
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
    let server = std::thread::spawn(move || {
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
            // macOS gives an accepted socket its listener's O_NONBLOCK (Linux does not), and a read timeout bounds
            // only a blocking read: without this the read fails at once there, with WouldBlock.
            stream
                .set_read_timeout(Some(BOUND))
                .expect("bound the server read");
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
        sent
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
        "sot_socket_open did not reach this account's socket: {stdout}{stderr}"
    );
    assert_eq!(
        server.join().expect("echo server"),
        ["ping\n", "ping\n", ""],
        "what each path sent"
    );
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
    assert!(status.success(), "bash: {stderr}");
    assert_eq!(
        stdout,
        "far sotd stdio-bridge\nplain\nfar sotd stdio-bridge\nbounded\nfar sotd stdio-bridge --host far1\nrelay\nfar sotd stdio-bridge --host far1\ndial\n",
        "{stderr}"
    );
}

/// Runs `script` after comm-lib under this platform's bash, within `BOUND`, and returns its stdout and how long it
/// took. Every line is printed, in a passing run's log too (CI runs the tests with --nocapture). A script that does not
/// finish within `BOUND`, or a bash that fails, fails the caller.
fn bound_case(script: &str) -> (String, Duration) {
    let home = tempfile::tempdir().expect("scratch home");
    let started = std::time::Instant::now();
    let mut child = shell(home.path(), &format!(". \"$1\" || exit 1\n{script}"), "").expect("bash");
    drop(child.stdin.take());
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    let took = started.elapsed();
    eprintln!("the case's lines:\n{stdout}its stderr:\n{stderr}it took {took:?}");
    assert!(status.success(), "bash failed: {stderr}");
    (stdout, took)
}

/// `sot_bounded` keeps its deadline while CMD's group outlives CMD: CMD exits 0 at once, a descendant still holds the
/// captured output, and the bound ends the group at its deadline (124), naming CMD's own status on stderr. Every
/// platform (on Windows, for Git Bash's own programs).
#[test]
fn the_comm_bound_holds_a_group_whose_leader_exited() {
    let Some(_) = bash() else { return };
    let (lines, took) = bound_case(
        r#"out="$(sot_bounded 1 sh -c 'sleep 30 & echo $! > "$HOME/lead.pid"; exit 0')"; echo "leader exit $?"
kill -0 "$(cat "$HOME/lead.pid")" 2>/dev/null && echo "leader exit alive" || echo "leader exit gone"
"#,
    );
    assert_eq!(lines, "leader exit 124\nleader exit gone\n");
    assert!(
        took < Duration::from_secs(10),
        "the bound returned only after {took:?}"
    );
}

/// `sot_bounded` ends CMD when CMD itself has left its process group: the bound signals CMD as well as its group, and
/// never waits on it without a bound. Unix only: Git Bash emulates process groups.
#[cfg(unix)]
#[test]
fn the_comm_bound_ends_a_command_that_left_its_group() {
    let (lines, took) = bound_case(
        r#"sot_bounded 1 sh -c 'echo $$ > "$HOME/leave.pid"; exec perl -e "setpgrp(0, getpgrp(getppid())) or die; exec q(sleep), q(30)"'; echo "leave $?"
kill -0 "$(cat "$HOME/leave.pid")" 2>/dev/null && echo "leave alive" || echo "leave gone"
"#,
    );
    assert_eq!(lines, "leave 124\nleave gone\n");
    assert!(
        took < Duration::from_secs(10),
        "the bound returned only after {took:?}"
    );
}

/// The bound's grace periods run on the clock, not on a count of sleeps: a command that ignores TERM ends with 137 one
/// grace period (1 s) after its 1 s bound, and is gone. Unix only, as KILL escalation is.
#[cfg(unix)]
#[test]
fn the_comm_bound_kills_a_deaf_command_one_grace_period_after_its_bound() {
    let (lines, took) = bound_case(
        r#"sot_bounded 1 sh -c 'trap "" TERM; echo $$ > "$HOME/deaf.pid"; exec sleep 30'; echo "deaf $?"
kill -0 "$(cat "$HOME/deaf.pid")" 2>/dev/null && echo "deaf alive" || echo "deaf gone"
"#,
    );
    assert_eq!(lines, "deaf 137\ndeaf gone\n");
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_millis(3500),
        "a 1 s bound and a 1 s grace period took {took:?}"
    );
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
        r#". "$1" && mkdir -p "$HOME/no-perl"
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
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND * 2);
    // Every line, in the log of a run that passes too (CI runs the tests with --nocapture).
    eprintln!("the bound test's lines:\n{stdout}its stderr:\n{stderr}");
    assert!(status.success(), "the bound: bash failed: {stderr}");
    assert_eq!(stdout, want, "the bound: {stderr}");
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
        let (status, _, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
        assert!(
            !status.success(),
            "{form}: another account's pipe is refused"
        );
        assert!(stderr.contains("not connecting"), "{form}: {stderr}");
    }
}
