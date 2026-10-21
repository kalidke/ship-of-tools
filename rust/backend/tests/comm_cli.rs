//! Messaging's commands as sessions run them: the installed wrappers (`comm-send.sh`, `comm-poll.sh`, `comm-join.sh`,
//! `comm-context.sh`, the subagent guard), staged flat from this tree, running this build's `sotd comm` on a scratch
//! comm folder with no daemon and no topology. And the one registry lock across processes: concurrent `sotd comm`
//! writers lose no update. On Windows, the agent-layer walk across Git Bash's process table.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sot_platform::test_wait::run_within;
#[cfg(target_os = "linux")]
use sot_platform::test_wait::Ran;

const BOUND: Duration = Duration::from_secs(60);

/// The bash the wrappers run under: Git Bash on Windows (never WSL's), else the one on PATH.
fn bash() -> Command {
    Command::new(if cfg!(windows) { r"C:\Program Files\Git\bin\bash.exe" } else { "bash" })
}

/// `p` with `/` for its separators, as a Git Bash script reads a path.
fn fwd(p: &Path) -> String {
    // Git Bash refuses a verbatim `//?/C:/...` path; the plain drive path is the same folder.
    let s = p.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").map(str::to_string).unwrap_or(s)
}

/// `p` without a verbatim `\\?\` prefix: `cmd.exe` cannot run a batch file named that way ("The system cannot find the path
/// specified."), though the folder is the same.
fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

#[test]
fn a_verbatim_path_loses_its_prefix_for_cmd() {
    assert_eq!(plain(Path::new(r"\\?\C:\Users\x\agents")), PathBuf::from(r"C:\Users\x\agents"));
    assert_eq!(plain(Path::new("/tmp/x")), PathBuf::from("/tmp/x"));
}

/// A scratch box: its home, its comm folder, the staged scripts, a project folder for each session.
struct Scratch {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
}

impl Scratch {
    fn new() -> Scratch {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
        let bin = root.join("bin");
        let stage = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm/tests/stage-bin.sh");
        run_within(bash().arg(fwd(&stage)).arg(fwd(&bin)), BOUND).ok();
        Scratch { _tmp: tmp, root, bin }
    }

    fn comm(&self) -> PathBuf {
        self.root.join("comm")
    }

    /// The command line a session in project `who` runs a wrapper with: its own private self file (outside any row),
    /// this build's sotd, the box's comm folder, no daemon to find.
    #[cfg(unix)]
    fn session(&self, who: &str, script: &str, args: &[&str]) -> Command {
        self.session_under(None, who, script, args)
    }

    /// [`Scratch::session`] under the umask `umask` when one is named, else the runner's.
    #[cfg(unix)]
    fn session_under(&self, umask: Option<&str>, who: &str, script: &str, args: &[&str]) -> Command {
        let project = self.root.join("projects").join(who);
        std::fs::create_dir_all(&project).expect("project");
        let mut c = bash();
        if let Some(m) = umask {
            c.arg("-c").arg(format!(r#"umask {m}; exec bash "$0" "$@""#));
        }
        c.arg(fwd(&self.bin.join(script)))
            .args(args)
            .current_dir(&project)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.join("home"))
            .env("SOTD_BIN", env!("CARGO_BIN_EXE_sotd"))
            .env("SOT_COMM_HOME", self.comm())
            .env("SOT_COMM_SELF_FILE", self.root.join(format!("self-{who}.txt")))
            .env("SOT_SELF_HOST", "box1")
            .env("SOT_COMM_TEST_HOST", "box1")
            .env("SOT_SOCKET", self.root.join("no-daemon.sock"))
            .env("SOT_HOSTS", self.root.join("no-hosts.toml"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"));
        c
    }

    #[cfg(target_os = "linux")]
    fn run(&self, who: &str, script: &str, args: &[&str]) -> Ran {
        run_within(&mut self.session(who, script, args), BOUND)
    }

    /// Writes the inbox lock record naming this box's lock manager, as the folder's hub would at its start: so `sotd
    /// comm send` appends itself.
    #[cfg(target_os = "linux")]
    fn own_lock_record(&self) {
        let ctx = self.run("alpha", "comm-context.sh", &[]).ok();
        assert!(ctx.stdout.contains("COMM_HOME="), "{ctx}");
        let id = lock_identity_of(&self.comm().join("inbox"));
        std::fs::write(self.comm().join("inbox-lock-manager"), format!("{id}\n")).expect("record");
    }
}

/// The lock manager of `dir` as the daemon names it, from the daemon's own code (included by path: the crate is a
/// binary). Linux only, as that code reaches the rest of the binary on macOS.
#[cfg(target_os = "linux")]
#[path = "../src/comm/mail/inbox.rs"]
#[allow(dead_code, reason = "this suite needs only the lock manager's name")]
mod comm_inbox;

#[cfg(target_os = "linux")]
fn lock_identity_of(dir: &Path) -> String {
    comm_inbox::lock_identity(dir)
}

/// A session joins, another sends it a message, it reads it once, and every word is the one the scripts always
/// printed: `Joined sot-comm as @…`, `  filed -> @…`, `[ts] [from:repo] msg`, `No new messages.`.
#[cfg(target_os = "linux")]
#[test]
fn a_join_a_send_and_a_poll_through_the_wrappers() {
    let b = Scratch::new();
    let j = b.run("alpha", "comm-join.sh", &[]).ok();
    assert!(j.stdout.starts_with("Joined sot-comm as @alpha-box1  (alpha on box1)."), "{j}");
    b.run("beta", "comm-join.sh", &[]).ok();
    b.own_lock_record();
    let ctx = b.run("alpha", "comm-context.sh", &[]).ok();
    assert!(ctx.stdout.contains("\nNAME=alpha-box1\n") && ctx.stdout.contains("\nROW_ID=\n"), "{ctx}");
    let sent = b.run("beta", "comm-send.sh", &["@alpha-box1", "it's", "here"]);
    assert_eq!((sent.status.code(), sent.stdout.as_str()), (Some(0), "  filed -> @alpha-box1\n"), "{sent}");
    let mut stdin_send = b.session("beta", "comm-send.sh", &["@alpha-box1", "-"]);
    stdin_send.stdin(std::process::Stdio::piped());
    let body = "a body with `backquotes`, $(no command) and 'quotes'\n";
    let mut child = stdin_send.stdout(std::process::Stdio::piped()).spawn().expect("spawn");
    std::io::Write::write_all(child.stdin.as_mut().expect("stdin"), body.as_bytes()).expect("write body");
    drop(child.stdin.take());
    let (status, out, err) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    assert!(status.success() && out.contains("filed -> @alpha-box1"), "{status} {out} {err}");
    let polled = b.run("alpha", "comm-poll.sh", &[]).ok();
    let lines: Vec<&str> = polled.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{polled}");
    assert!(lines[0].ends_with("] [beta-box1:beta] it's here"), "{polled}");
    assert!(lines[1].ends_with("] [beta-box1:beta] a body with `backquotes`, $(no command) and 'quotes'"), "{polled}");
    assert_eq!(b.run("alpha", "comm-poll.sh", &[]).ok().stdout, "No new messages.\n");
    let unknown = b.run("beta", "comm-send.sh", &["@nobody", "x"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(unknown.stderr.starts_with("FAILED -> @nobody: "), "{unknown}");
}

/// Everything a session's commands write in the comm folder is its user's alone, under any umask the caller has.
#[cfg(target_os = "linux")]
#[test]
fn the_comm_folder_is_the_users_alone_under_an_open_umask() {
    use std::os::unix::fs::PermissionsExt;
    let b = Scratch::new();
    let open = |who: &str, script: &str, args: &[&str]| run_within(&mut b.session_under(Some("000"), who, script, args), BOUND).ok();
    open("alpha", "comm-join.sh", &[]);
    open("beta", "comm-join.sh", &[]);
    b.own_lock_record();
    open("beta", "comm-send.sh", &["@alpha-box1", "x"]);
    open("alpha", "comm-poll.sh", &[]);
    let mut seen = 0;
    let mut walk = vec![b.comm()];
    while let Some(dir) = walk.pop() {
        for e in std::fs::read_dir(&dir).expect("read the comm folder") {
            let p = e.expect("entry").path();
            if p.ends_with("inbox-lock-manager") {
                continue; // the test wrote it, as the hub's daemon would
            }
            let mode = std::fs::metadata(&p).expect("stat").permissions().mode();
            assert_eq!(mode & 0o077, 0, "{} is open to others: {mode:o}", p.display());
            seen += 1;
            if p.is_dir() {
                walk.push(p);
            }
        }
    }
    assert!(seen >= 8, "only {seen} entries in the comm folder");
}

/// The subagent guard: a subagent's run of an installed comm script is denied, a session's own is not.
#[cfg(unix)]
#[test]
fn the_subagent_guard_wrapper_denies_a_subagent_and_never_a_session() {
    let b = Scratch::new();
    let guard = |input: &str| {
        let mut c = b.session("alpha", "comm-subagent-guard.sh", &[]);
        c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
        let mut child = c.spawn().expect("spawn");
        std::io::Write::write_all(child.stdin.as_mut().expect("stdin"), input.as_bytes()).expect("write");
        drop(child.stdin.take());
        let (status, out, _) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
        assert!(status.success());
        out
    };
    let cmd = r#"{"tool_name":"Bash","tool_input":{"command":"comm-send.sh @x hi"}}"#;
    assert_eq!(guard(cmd), "", "a session's own input carries no agent_id");
    let sub = r#"{"agent_id":"a1","tool_name":"Bash","tool_input":{"command":"comm-send.sh @x hi"}}"#;
    assert!(guard(sub).contains(r#""permissionDecision":"deny""#));
}

/// One registry lock for every process: 16 `sotd comm` processes each putting its own entry at once leave 16 entries,
/// none lost to a write made against a stale read.
#[test]
fn concurrent_writers_in_other_processes_lose_no_update() {
    let b = Scratch::new();
    run_within(Command::new(env!("CARGO_BIN_EXE_sotd")).args(["comm", "ensure-home"]).env("SOT_COMM_HOME", b.comm()), BOUND).ok();
    let writers: Vec<_> = (0..16)
        .map(|i| {
            let mut c = Command::new(env!("CARGO_BIN_EXE_sotd"));
            c.args(["comm", "registry", "put", &format!("w{i}")])
                .env("SOT_COMM_HOME", b.comm())
                .env("SOT_SELF_HOST", "box1")
                .env("SOT_LOCK_WAIT_SECS", "30")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = c.spawn().expect("spawn");
            std::io::Write::write_all(child.stdin.as_mut().expect("stdin"), format!("{{\"n\":{i}}}").as_bytes()).expect("write");
            drop(child.stdin.take());
            child
        })
        .collect();
    for w in writers {
        let (status, _, err) = sot_platform::test_isolated::drain(w).wait_within(BOUND);
        assert!(status.success(), "{err}");
    }
    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(b.comm().join("registry.json")).unwrap()).unwrap();
    assert_eq!(doc["agents"].as_object().map(|a| a.len()), Some(16), "{doc}");
    assert!(!b.comm().join(".registry.lock").exists(), "the lock was released");
}

/// Windows: a comm command run through Git Bash, with its MSYS exec chain (`bash -c "bash script"` forks and execs, as
/// Claude Code's Bash tool does), reads the agents above it: none under a plain native parent, one under one stand-in
/// agent, and two (a second agent inside a session) refused. The stand-ins are copies of cmd.exe named after the agents;
/// each runs the next level's batch file.
#[cfg(windows)]
#[test]
fn the_windows_walk_counts_the_agents_above_git_bash() {
    const GIT_BASH: &str = r"C:\Program Files\Git\bin\bash.exe";
    assert!(Path::new(GIT_BASH).exists(), "Git Bash is missing at {GIT_BASH}");
    let b = Scratch::new();
    let dir = plain(&b.root.join("agents"));
    std::fs::create_dir_all(&dir).unwrap();
    let cmd = PathBuf::from(std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into()));
    for agent in ["claude.exe", "codex.exe"] {
        #[allow(clippy::disallowed_methods, reason = "a data copy of cmd.exe, run only after the copy is closed")]
        std::fs::copy(&cmd, dir.join(agent)).unwrap();
    }
    let script = dir.join("run.sh");
    std::fs::write(
        &script,
        format!(
            "export SOTD_BIN='{}' SOT_COMM_HOME='{}'\n. '{}/comm-lib.sh' && sot_require_agent >/dev/null; echo \"rc=$?\"\n",
            fwd(Path::new(env!("CARGO_BIN_EXE_sotd"))),
            fwd(&b.comm()),
            fwd(&b.bin)
        ),
    )
    .unwrap();
    let level2 = dir.join("level2.bat");
    std::fs::write(&level2, format!("@\"{GIT_BASH}\" -c \"bash '{}'\"\r\n", fwd(&script))).unwrap();
    let level1 = dir.join("level1.bat");
    std::fs::write(&level1, format!("@\"{}\" /d /c \"{}\"\r\n", dir.join("codex.exe").display(), level2.display())).unwrap();
    for staged in [&level1, &level2, &script, &dir.join("codex.exe"), &dir.join("claude.exe"), Path::new(GIT_BASH)] {
        assert!(staged.exists() && !staged.to_string_lossy().starts_with(r"\\?\"), "cmd cannot read {}", staged.display());
    }
    // The verdict is the staged script's last `rc=` line; a failure says what the script printed, so a missing line
    // shows its cause (the exit code, both streams) in the one hosted run.
    let run = |program: &Path, batch: &Path| {
        let ran = run_within(Command::new(program).args(["/d", "/c"]).arg(batch), BOUND);
        let rc = ran.stdout.lines().filter(|l| l.starts_with("rc=")).last().unwrap_or_default().to_string();
        let account = format!("exit {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}", ran.status.code(), ran.stdout, ran.stderr);
        (rc, account)
    };
    let (rc, account) = run(&cmd, &level2);
    assert_eq!(rc, "rc=0", "no agent above: {account}");
    let (rc, account) = run(&dir.join("claude.exe"), &level2);
    assert_eq!(rc, "rc=0", "the session's own agent: {account}");
    let (rc, account) = run(&dir.join("claude.exe"), &level1);
    assert_eq!(rc, "rc=1", "a second agent inside the session is refused: {account}");
}
