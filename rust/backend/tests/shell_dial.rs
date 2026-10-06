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

/// `sot_bounded`, the one bound a timed comm command runs under, on this platform's bash: as the box has it (GNU
/// `timeout` or `gtimeout` when present), and as the watchdog (an empty `_SOT_TIMEOUT_BIN` takes it). Either way the
/// command keeps the shell's stdin and its own status, and one still running at the bound ends with 124 and is gone
/// when the call returns. One that ignores TERM ends with 137 (not on Windows, whose Git Bash emulates signals).
#[test]
fn the_comm_bound_keeps_stdin_and_status_and_ends_a_late_command() {
    let Some(_) = bash() else { return };
    let home = tempfile::tempdir().expect("scratch home");
    let (deaf, want) = if cfg!(windows) {
        ("", "in\nstatus 7\nlate 124\nlate gone\n")
    } else {
        (
            r#"sot_bounded 1 sh -c 'trap "" TERM; echo $$ > "$HOME/deaf.pid"; exec tail -f /dev/null'; echo "deaf $?"
kill -0 "$(cat "$HOME/deaf.pid")" 2>/dev/null && echo "deaf alive" || echo "deaf gone"
"#,
            "in\nstatus 7\nlate 124\nlate gone\ndeaf 137\ndeaf gone\n",
        )
    };
    for (path, prelude) in [("as this box has it", ""), ("the watchdog", "_SOT_TIMEOUT_BIN=")] {
        let script = format!(
            r#". "$1" && {prelude}
printf 'in\n' | sot_bounded 5 cat
sot_bounded 5 sh -c 'exit 7'; echo "status $?"
sot_bounded 1 sh -c 'echo $$ > "$HOME/late.pid"; exec tail -f /dev/null'; echo "late $?"
kill -0 "$(cat "$HOME/late.pid")" 2>/dev/null && echo "late alive" || echo "late gone"
{deaf}"#
        );
        let mut child = shell(home.path(), &script, "").expect("bash");
        drop(child.stdin.take());
        let (status, stdout, stderr) = sot_log::test_isolated::drain(child).wait_within(BOUND);
        assert!(status.success(), "the bound, {path}: bash failed: {stderr}");
        assert_eq!(stdout, want, "the bound, {path}: {stderr}");
    }
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
