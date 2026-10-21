use std::process::Command;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

/// The bash a shell client runs under: Git Bash on Windows, `bash` elsewhere. `None` only where Git Bash is
/// missing and no CI is running; in CI a missing bash fails the test.
fn bash() -> Option<std::path::PathBuf> {
    if !cfg!(windows) {
        return Some("bash".into());
    }
    let found = std::env::var_os("ProgramFiles")
        .map(|p| std::path::PathBuf::from(p).join("Git").join("bin").join("bash.exe"))
        .filter(|p| p.exists());
    assert!(found.is_some() || std::env::var_os("CI").is_none(), "CI has no Git Bash at %ProgramFiles%\\Git\\bin\\bash.exe");
    found
}

/// The shell client's hello as its builder, `sot_hello_frame`, prints it: one request frame whose payload is a
/// `hello` at this `PROTOCOL_VERSION`, naming the host and this process's OS account, with no token. The builder
/// writes its `"protocol"` by hand, so this run is what holds it to `PROTOCOL_VERSION`.
#[test]
fn the_shell_hello_is_this_protocols_hello() {
    let Some(bash) = bash() else { return };
    let lib =
        format!("{}/../../comm/lib/comm-lib.sh", env!("CARGO_MANIFEST_DIR")).replace('\\', "/");
    let home = tempfile::tempdir().expect("scratch home");
    let ran = sot_platform::test_wait::run_within(
        Command::new(bash)
            .args(["-c", &format!(". '{lib}' && sot_hello_frame")])
            .env("HOME", home.path())
            .env("SOT_COMM_HOME", home.path().join(".sot-comm"))
            .env("SOT_SELF_HOST", "hello-test-host")
            .env_remove("SOT_WORKSPACE"),
        BOUND,
    )
    .ok();
    let frame: super::Frame =
        serde_json::from_str(&ran.stdout).expect("the shell hello is one JSON frame");
    assert_eq!(
        (frame.kind, frame.op.as_str()),
        (super::Kind::Req, "hello"),
        "the shell hello's envelope"
    );
    assert!(
        frame.payload.get("token").is_none(),
        "the shell client's hello sends no token: {}",
        frame.payload
    );
    let hello: super::ops::HelloReq =
        serde_json::from_value(frame.payload).expect("its payload is a hello request");
    assert_eq!(
        hello.protocol,
        super::PROTOCOL_VERSION,
        "the shell client's hello protocol"
    );
    assert_eq!(
        hello.host.as_deref(),
        Some("hello-test-host"),
        "the shell client's hello host"
    );
    let own = sot_platform::identity::os_account::own_account_id()
        .expect("the OS issues this process's account");
    assert_eq!(
        hello.os_user.as_deref(),
        Some(own.as_str()),
        "the shell client's hello account"
    );
}

/// ADR 0049 `## User isolation`: a hello names the OS account its process runs as, and two accounts on one
/// host are told apart by that string, so every client must write the same one. The Rust builders use
/// `own_account_id()`; the shell client reads `_sot_os_user`, the launcher's PowerShell the process token's
/// user SID. This runs the other two and holds each to the first, on whichever OS runs the test.
#[test]
fn every_client_declares_the_same_account() {
    let own = sot_platform::identity::os_account::own_account_id().expect("the OS issues this process's account");
    if let Some(bash) = bash() {
        let lib = format!("{}/../../comm/lib/comm-lib.sh", env!("CARGO_MANIFEST_DIR")).replace('\\', "/");
        let scratch = std::env::temp_dir().join(format!("sot-protocol-os-user-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch home");
        let ran = sot_platform::test_wait::run_within(
            Command::new(bash)
                .args(["-c", &format!(". '{lib}' && _sot_os_user")])
                .env("HOME", &scratch)
                .env("SOT_COMM_HOME", scratch.join(".sot-comm")),
            BOUND,
        );
        let _ = std::fs::remove_dir_all(&scratch);
        let ran = ran.ok();
        assert_eq!(ran.stdout.trim(), own, "the shell client's account: {ran}");
    }
    #[cfg(windows)]
    {
        let ran = sot_platform::test_wait::run_within(
            Command::new("powershell").args([
                "-NoProfile",
                "-Command",
                "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            ]),
            BOUND,
        )
        .ok();
        assert_eq!(ran.stdout.trim(), own, "the launcher's account: {ran}");
    }
}
