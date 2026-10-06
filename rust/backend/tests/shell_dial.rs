//! ADR 0049 `## User isolation`: the shell's `sot_dial` and `sot_oneshot_request` reach a local endpoint only through
//! the built `sotd stdio-bridge`, whose `connect_own` admits only an endpoint this OS account serves.

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
    let script = r#". "$1" && ENDPOINT="$2" SOT_SEND_TIMEOUT=3 sot_oneshot_request '{"v":1,"id":1,"kind":"req","op":"version.query","payload":{}}' version.query"#;
    let mut child = shell(home.path(), script, &endpoint).expect("bash on Unix");
    drop(child.stdin.take());
    let (status, _, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    let accepted = listener.accept();
    std::fs::remove_file(&path).expect("remove the test's socket");
    assert!(matches!(accepted, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the shell client connected to a socket whose folder is not private to this OS account (ADR 0049 `## User isolation`)");
    assert!(!status.success(), "the shell request must fail");
    assert!(stderr.contains("is not a private folder of this OS account"), "{stderr}");
}

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
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + BOUND;
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "the shell never connected within its bound");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("accept the shell: {e}"),
            }
        };
        stream.set_read_timeout(Some(BOUND)).expect("bound the server read");
        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).expect("read ping");
        assert_eq!(line, "ping\n");
        stream.write_all(b"pong\n").expect("write pong");
        // Keep the endpoint open until the caller closes stdin, as the real bridge's contract requires.
        let mut tail = Vec::new();
        stream.read_to_end(&mut tail).expect("read the bridge's EOF");
    });
    let endpoint = format!("unix:{}", path.display());
    ping(shell(home.path(), r#". "$1" && sot_dial "$2" 5"#, &endpoint).expect("bash on Unix"));
    server.join().expect("echo server");
}

/// `sot_bounded`, the bound each of the four timed comm calls runs under, on this platform's bash, with a `timeout`
/// and a `gtimeout` that are not coreutils first on PATH (the bound uses neither). One perl process owns the deadline
/// and the command's process group. The command keeps the shell's stdin and its own status, `sot_ssh_bridge` carries
/// its input, a late command returns 124 and is gone, a descendant holding a captured command's output ends with it,
/// an errexit caller ends with 124, a caller killed mid-bound leaves no command behind, with no perl or a bound of 0
/// nothing runs (125), and a command that cannot start returns 127. On Unix also: a command that ignores TERM, and a
/// TERM-ignoring descendant holding captured output after its leader died, end with 137, and a TERM sent to the
/// bound itself reaches the group and is escalated to KILL (143). Windows' Git Bash emulates signals and process
/// groups, so those three are not claimed there.
#[test]
fn the_comm_bound_owns_its_command_until_its_group_ends() {
    let Some(_) = bash() else { return };
    let home = tempfile::tempdir().expect("scratch home");
    let (unix, unix_want) = if cfg!(windows) {
        ("", "")
    } else {
        (
            r#"sot_bounded 1 sh -c 'trap "" TERM; echo $$ > "$HOME/deaf.pid"; exec tail -f /dev/null'; echo "deaf $?"
kill -0 "$(cat "$HOME/deaf.pid")" 2>/dev/null && echo "deaf alive" || echo "deaf gone"
deafheld="$(sot_bounded 1 sh -c '(trap "" TERM; exec tail -f /dev/null) & echo $! > "$HOME/deafheld.pid"; wait')"; echo "deaf held $?"
kill -0 "$(cat "$HOME/deafheld.pid")" 2>/dev/null && echo "deaf held alive" || echo "deaf held gone"
sot_bounded 5 sh -c 'trap "" TERM; echo $$ > "$HOME/cancel.pid"; exec tail -f /dev/null' &
c=$!
for _ in $(seq 1 100); do [ -s "$HOME/cancel.pid" ] && break; sleep 0.05; done
kill -TERM "$(ps -o ppid= -p "$(cat "$HOME/cancel.pid")" | tr -d ' ')"
wait "$c"; echo "cancel $?"
kill -0 "$(cat "$HOME/cancel.pid")" 2>/dev/null && echo "cancel alive" || echo "cancel gone"
"#,
            "deaf 137\ndeaf gone\ndeaf held 137\ndeaf held gone\ncancel 143\ncancel gone\n",
        )
    };
    let want = format!(
        "in\nstatus 7\nssh-in\nlate 124\nlate gone\nheld 124\nheld gone\nerrexit 124\norphan gone\nno perl 125\n{unix_want}missing 127\nzero 125\n"
    );
    let script = format!(
        r#". "$1" && PATH="$HOME/no-gnu:$HOME/stub-ssh:$PATH"
mkdir -p "$HOME/stub-ssh" "$HOME/no-gnu" "$HOME/no-perl"
printf '#!/bin/sh\ncase "$1" in -G) exit 0 ;; esac\nexec cat\n' > "$HOME/stub-ssh/ssh"
printf '#!/bin/sh\necho "ERROR: Invalid syntax."\nexit 1\n' > "$HOME/no-gnu/timeout"
cp "$HOME/no-gnu/timeout" "$HOME/no-gnu/gtimeout"
chmod +x "$HOME/stub-ssh/ssh" "$HOME/no-gnu/timeout" "$HOME/no-gnu/gtimeout"
printf 'in\n' | sot_bounded 5 cat
sot_bounded 5 sh -c 'exit 7'; echo "status $?"
printf 'ssh-in\n' | sot_ssh_bridge stubhost "" 5
sot_bounded 1 sh -c 'echo $$ > "$HOME/late.pid"; exec tail -f /dev/null'; echo "late $?"
kill -0 "$(cat "$HOME/late.pid")" 2>/dev/null && echo "late alive" || echo "late gone"
held="$(sot_bounded 1 sh -c 'tail -f /dev/null & echo $! > "$HOME/held.pid"; wait')"; echo "held $?"
kill -0 "$(cat "$HOME/held.pid")" 2>/dev/null && echo "held alive" || echo "held gone"
( set -e; sot_bounded 1 sh -c 'exec tail -f /dev/null'; echo unreachable ); echo "errexit $?"
( sot_bounded 1 sh -c 'echo $$ > "$HOME/orphan.pid"; exec tail -f /dev/null' ) &
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
    let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    assert!(status.success(), "the bound: bash failed: {stderr}");
    assert_eq!(stdout, want, "the bound: {stderr}");
}

#[cfg(windows)]
#[test]
fn the_shell_dial_reaches_only_a_pipe_this_account_serves() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ServerOptions;
    let Some(_) = bash() else { return };
    let runtime = tokio::runtime::Runtime::new().expect("pipe runtime");
    let name = format!("sot-shell-dial-{}", std::process::id());
    let path = format!(r"\\.\pipe\{name}");
    let server = runtime.block_on(async { ServerOptions::new().first_pipe_instance(true).create(&path) }).expect("test's pipe");
    let echo = std::thread::spawn(move || runtime.block_on(async move {
        tokio::time::timeout(BOUND, async move {
            server.connect().await.expect("accept the shell");
            let mut stream = tokio::io::BufReader::new(server);
            let mut line = String::new();
            stream.read_line(&mut line).await.expect("read ping");
            assert_eq!(line, "ping\n");
            stream.get_mut().write_all(b"pong\n").await.expect("write pong");
            let mut tail = Vec::new();
            stream.read_to_end(&mut tail).await.expect("read the bridge's EOF");
        }).await.expect("pipe echo within its bound");
    }));
    let home = tempfile::tempdir().expect("scratch home");
    ping(shell(home.path(), r#". "$1" && sot_dial "$2" 5"#, &format!("pipe:{name}")).expect("Git Bash"));
    echo.join().expect("pipe echo server");
    let mut child = shell(home.path(), r#". "$1" && sot_dial "$2" 5"#, r"pipe:\\.\pipe\epmapper").expect("Git Bash");
    drop(child.stdin.take());
    let (status, _, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    assert!(!status.success(), "another account's pipe is refused");
    assert!(stderr.contains("not connecting"), "{stderr}");
}
