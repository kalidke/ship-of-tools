//! Updater command-routing behavior and lexical pins with explicitly documented coverage limits.

/// Pins .output(, .spawn(, .status(, spawn_command(, fork(, execvp(, CreateProcess and posix_spawn( substrings.
/// It misses starts on lines containing spawner.output(, UFCS starts, libc::system, aliases and other unlisted or differently spaced spellings.
#[test]
fn updater_process_start_spellings_are_pinned() {
    let mut found = Vec::new();
    for (path, source) in sot_log::test_scan::production_sources() {
        if !path.starts_with("rust/updater/src/") {
            continue;
        }
        for (line, text) in source.lines().enumerate() {
            if [
                ".output(",
                ".spawn(",
                ".status(",
                "spawn_command(",
                "fork(",
                "execvp(",
                "CreateProcess",
                "posix_spawn(",
            ]
            .iter()
            .any(|word| text.contains(word))
                && !text.contains("spawner.output(")
            {
                found.push(format!("{path}:{}: {}", line + 1, text.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "updater starts a process outside caller policy:\n{}",
        found.join("\n")
    );
}

/// Pins check_release, stage, prepare and prepare.rs matches async signatures to &dyn Spawner, plus the trait text.
/// It does not discover newly added public spawn-bearing entries or resolve equivalent signature spellings.
#[test]
fn four_named_updater_entries_require_spawner() {
    let sources = sot_log::test_scan::production_sources();
    let mut entries = Vec::new();
    for (path, source) in &sources {
        if !path.starts_with("rust/updater/src/") {
            continue;
        }
        for name in ["check_release", "stage", "prepare", "matches"] {
            if name == "matches" && path != "rust/updater/src/prepare.rs" {
                continue;
            }
            let needle = format!("pub async fn {name}(");
            for (tail, _) in source.match_indices(&needle) {
                let signature = &source[tail..source[tail..].find('{').unwrap() + tail];
                assert!(
                    signature.contains("&dyn Spawner"),
                    "public updater entry {path} {name} lacks caller policy"
                );
                entries.push(name);
            }
        }
    }
    entries.sort();
    assert_eq!(entries, ["check_release", "matches", "prepare", "stage"]);
    let trait_source = sources
        .iter()
        .find(|(p, _)| p == "rust/updater/src/spawn.rs")
        .unwrap();
    assert!(trait_source.1.contains("pub trait Spawner: Send + Sync"));
}

use crate::lifecycle::child_signal::Signal;
use crate::update::UpdaterSpawner;
use sot_updater::prepare::{PrepareSpec, PreparedState};
use sot_updater::{Fetcher, ReleaseIdentity, Spawner, UpdaterConfig};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(unix)]
use crate::lifecycle::start_tests::unix::Watched;
#[cfg(windows)]
use crate::lifecycle::start_tests::windows::Watched;

fn eventually_dead(watched: &Watched) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if watched.dead() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        // Unix poll can be interrupted by another owned fixture's SIGCHLD.
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn output(code: i32, stdout: impl Into<Vec<u8>>) -> std::process::Output {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    };
    std::process::Output {
        status,
        stdout: stdout.into(),
        stderr: Vec::new(),
    }
}

struct TreeFixture {
    dir: tempfile::TempDir,
    program: PathBuf,
}

impl TreeFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let program = dir.path().join("tree");
        #[cfg(windows)]
        let program = dir.path().join("tree.ps1");
        #[cfg(unix)]
        let body = "#!/bin/sh\n/bin/sh -c 'echo $$ > \"$1\"; while [ ! -e \"$2\" ]; do /bin/sleep 0.02; done' sh \"$1\" \"$2\" &\nwhile [ ! -s \"$1\" ]; do /bin/sleep 0.01; done\nwhile [ ! -e \"$3\" ]; do /bin/sleep 0.01; done\nprintf 'fixture stdout'; printf 'fixture stderr' >&2\nexit 7\n";
        #[cfg(windows)]
        let body = r#"param($ready, $cleanup, $release)
$descendant = Join-Path $PSScriptRoot 'descendant.ps1'
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = 'powershell'
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.Arguments = '-NoProfile -File "' + $descendant + '" "' + $ready + '" "' + $cleanup + '"'
$child = [System.Diagnostics.Process]::Start($psi)
while (!(Test-Path -LiteralPath $ready)) { Start-Sleep -Milliseconds 10 }
while (!(Test-Path -LiteralPath $release)) { Start-Sleep -Milliseconds 10 }
[Console]::Out.Write('fixture stdout'); [Console]::Error.Write('fixture stderr'); exit 7
"#;
        #[cfg(windows)]
        sot_log::test_exec::write_executable(&dir.path().join("descendant.ps1"),
            "param($ready, $cleanup)\n[IO.File]::WriteAllText($ready, [string]$PID); while (!(Test-Path -LiteralPath $cleanup)) { Start-Sleep -Milliseconds 20 }\n");
        sot_log::test_exec::write_executable(&program, body);
        Self { dir, program }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn command(&self) -> tokio::process::Command {
        #[cfg(unix)]
        let mut command = tokio::process::Command::new(&self.program);
        #[cfg(windows)]
        let mut command = {
            let mut c = tokio::process::Command::new("powershell");
            c.args(["-NoProfile", "-File"]).arg(&self.program);
            c
        };
        command
            .arg(self.path("ready"))
            .arg(self.path("cleanup"))
            .arg(self.path("release"));
        command.stdin(std::process::Stdio::null());
        command
    }

    async fn descendant(&self) -> Watched {
        let deadline = Instant::now() + Duration::from_secs(15);
        while std::fs::read_to_string(self.path("ready")).map_or(true, |s| s.trim().is_empty()) {
            assert!(Instant::now() < deadline, "fixture never ready");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(self.path("ready"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        Watched::open(pid)
    }

    fn cleanup(&self) {
        std::fs::write(self.path("cleanup"), b"done").unwrap();
    }
}

impl Drop for TreeFixture {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct Requests {
    signal: &'static Signal,
    tree: TreeFixture,
    root: tempfile::TempDir,
    identity: ReleaseIdentity,
    selected: Option<String>,
    seen: Mutex<Vec<String>>,
}

impl Requests {
    async fn new(zip: bool, selected: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let target = if zip {
            "windows-x86_64"
        } else {
            "linux-x86_64"
        };
        let ext = if zip { "zip" } else { "tar.gz" };
        let payload = root.path().join("payload");
        std::fs::write(&payload, b"synthetic archive").unwrap();
        let digest = sot_updater::fetch::hash::sha256_file(&payload)
            .await
            .unwrap();
        Self {
            signal: Box::leak(Box::new(Signal::new())),
            tree: TreeFixture::new(),
            root,
            identity: ReleaseIdentity {
                repo: "example/project".into(),
                tag: "v9.9.9".into(),
                version: "9.9.9".into(),
                target: target.into(),
                asset: format!("sot-9.9.9-{target}.{ext}"),
                asset_sha256: digest,
            },
            selected: selected.map(String::from),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn config(&self, fetcher: Fetcher) -> UpdaterConfig {
        UpdaterConfig {
            repo: self.identity.repo.clone(),
            current_version: "0.1.0".into(),
            fetcher,
            updates_root: self.root.path().join("updates"),
        }
    }

    fn spec(&self) -> PrepareSpec {
        PrepareSpec {
            identity: self.identity.clone(),
            repo_dir: self.root.path().join("repo"),
            stage_dir: self.root.path().join("updates/stage"),
            origin_url: Some("https://example.invalid/project".into()),
            julia_bin: Some("julia".into()),
            npm: true,
        }
    }

    fn top(&self) -> String {
        format!("sot-9.9.9-{}", self.identity.target)
    }

    fn extracted(&self, dest: &Path) -> std::io::Result<()> {
        let dir = dest.join(self.top());
        std::fs::create_dir_all(&dir)?;
        for name in if self.identity.target.starts_with("windows") {
            ["sot.exe", "sotd.exe", "sot-capsule.exe"]
        } else {
            ["sot", "sotd", "sot-capsule"]
        } {
            std::fs::write(dir.join(name), b"fixture binary")?;
        }
        Ok(())
    }

    fn response(&self, bin: &str, args: &[String]) -> std::io::Result<std::process::Output> {
        let sums = [
            "sot-9.9.9-linux-x86_64.tar.gz",
            "sot-9.9.9-macos-aarch64.tar.gz",
            "sot-9.9.9-windows-x86_64.zip",
        ]
        .map(|asset| format!("{}  {asset}\n", self.identity.asset_sha256))
        .concat();
        match bin {
            "curl" if !args.iter().any(|a| a == "-o") => Ok(output(
                0,
                br#"[{"tag_name":"v9.9.9","prerelease":false,"draft":false}]"#.to_vec(),
            )),
            "curl" => {
                let dest = Path::new(&args[args.iter().position(|a| a == "-o").unwrap() + 1]);
                let bytes = if args.last().unwrap().ends_with("SHA256SUMS") {
                    sums.as_bytes()
                } else {
                    b"synthetic archive"
                };
                std::fs::write(dest, bytes)?;
                Ok(output(0, Vec::new()))
            }
            "gh" if args[0] == "api" => Ok(output(
                0,
                br#"[{"tag_name":"v9.9.9","prerelease":false,"draft":false}]"#.to_vec(),
            )),
            "gh" => {
                assert_eq!(&args[..2], ["release", "download"]);
                let dir = Path::new(&args[args.iter().position(|a| a == "--dir").unwrap() + 1]);
                let name = &args[args.iter().position(|a| a == "--pattern").unwrap() + 1];
                std::fs::write(
                    dir.join(name),
                    if name == "SHA256SUMS" {
                        sums.as_bytes()
                    } else {
                        b"synthetic archive"
                    },
                )?;
                Ok(output(0, Vec::new()))
            }
            "tar" | "zipinfo" | "powershell"
                if args[0] == "-tzf"
                    || bin == "zipinfo"
                    || args.last().unwrap().contains("OpenRead") =>
            {
                Ok(output(0, format!("{}/\n{}/sot\n", self.top(), self.top())))
            }
            "tar" => {
                assert_eq!(args[0], "-xzf");
                self.extracted(Path::new(&args[3]))?;
                Ok(output(0, Vec::new()))
            }
            "unzip" => {
                assert_eq!(args[0], "-o");
                self.extracted(Path::new(&args[3]))?;
                Ok(output(0, Vec::new()))
            }
            "powershell" => {
                let script = args.last().unwrap();
                assert!(script.contains("Expand-Archive"));
                let dest = script
                    .split("-DestinationPath '")
                    .nth(1)
                    .unwrap()
                    .trim_end_matches('\'')
                    .replace("''", "'");
                self.extracted(Path::new(&dest))?;
                Ok(output(0, Vec::new()))
            }
            "git" => self.git(args),
            "julia" => {
                assert!(args[0].starts_with("--project="));
                assert_eq!(args[1], "-e");
                Ok(output(0, Vec::new()))
            }
            "npm" => {
                assert_eq!(args, ["ci", "--silent"]);
                Ok(output(0, Vec::new()))
            }
            _ => panic!("unexpected updater request {bin}: {args:?}"),
        }
    }

    fn git(&self, args: &[String]) -> std::io::Result<std::process::Output> {
        let args = if args[0] == "-C" { &args[2..] } else { args };
        match args[0].as_str() {
            "clone" => {
                std::fs::create_dir_all(Path::new(args.last().unwrap()).join(".git"))?;
            }
            "fetch" => assert_eq!(args, ["fetch", "--tags", "--force", "origin"]),
            "rev-parse" => return Ok(output(0, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n")),
            "status" => assert_eq!(args, ["status", "--porcelain", "-uno"]),
            "worktree" if args[1] == "prune" => assert_eq!(args.len(), 2),
            "worktree" if args[1] == "add" => {
                let checkout = Path::new(&args[3]);
                for env in [
                    "julia/kernel",
                    "julia/repl",
                    "julia/pluto",
                    "rust/backend/sidecars/mathjax",
                ] {
                    std::fs::create_dir_all(checkout.join(env))?;
                }
            }
            _ => panic!("unexpected git request: {args:?}"),
        }
        Ok(output(0, Vec::new()))
    }

    async fn public_entry(&self, entry: &str) -> String {
        let cfg = self.config(if entry == "gh" {
            Fetcher::Gh
        } else {
            Fetcher::Curl
        });
        match entry {
            "curl" | "gh" => {
                sot_updater::check_release(self, &cfg.repo, &cfg.current_version, &cfg.fetcher)
                    .await
                    .status
            }
            "stage" => format!("{:?}", sot_updater::stage(self, &cfg, &self.identity).await),
            "prepare" => {
                let spec = self.spec();
                std::fs::create_dir_all(&spec.stage_dir).unwrap();
                format!("{:?}", sot_updater::prepare::prepare(self, &spec).await)
            }
            "matches" => {
                let spec = self.spec();
                std::fs::create_dir_all(&spec.stage_dir).unwrap();
                let state = PreparedState {
                    schema: 1,
                    identity: self.identity.clone(),
                    checkout: spec.repo_dir.join("versions/v9.9.9"),
                    commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    julia_instantiated: true,
                    mathjax_deps: true,
                    prepared_at: 1,
                };
                std::fs::create_dir_all(&state.checkout).unwrap();
                state.write(&spec.stage_dir).await.unwrap();
                format!(
                    "{}",
                    PreparedState::matches(self, &spec.stage_dir, &self.identity).await
                )
            }
            _ => panic!("unknown public entry {entry}"),
        }
    }
}

impl Spawner for Requests {
    fn output<'a>(
        &'a self,
        command: &'a mut tokio::process::Command,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<std::process::Output>> + Send + 'a>,
    > {
        Box::pin(async move {
            let cmd = command.as_std();
            let bin = cmd.get_program().to_str().unwrap();
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let kind = match bin {
                "tar" if args[0] == "-tzf" => "tar-list",
                "tar" => "tar-extract",
                "powershell" if args.last().unwrap().contains("OpenRead") => "zip-list",
                "powershell" => "zip-extract",
                "zipinfo" => "zip-list",
                "unzip" => "zip-extract",
                _ => bin,
            };
            self.seen.lock().unwrap().push(kind.into());
            if self.selected.as_deref() == Some(kind) {
                return UpdaterSpawner(self.signal)
                    .output(&mut self.tree.command())
                    .await;
            }
            self.response(bin, &args)
        })
    }
}

#[tokio::test]
async fn public_entries_intercept_every_command_including_matches() {
    for entry in ["curl", "gh", "stage", "prepare", "matches"] {
        let requests = Requests::new(false, None).await;
        let answer = requests.public_entry(entry).await;
        assert!(
            answer == "ok" || answer == "true" || answer.starts_with("Ok("),
            "{entry}: {answer}"
        );
        assert!(
            !requests.seen.lock().unwrap().is_empty(),
            "public entry requested no command: {entry}"
        );
        if entry == "prepare" {
            let seen = requests.seen.lock().unwrap();
            for bin in ["git", "julia", "npm"] {
                assert!(seen.iter().any(|s| s == bin), "prepare omitted {bin}");
            }
        }
    }
    let requests = Requests::new(true, None).await;
    assert_eq!(requests.public_entry("stage").await, "Ok(true)");
    assert_eq!(
        *requests.seen.lock().unwrap(),
        ["curl", "zip-list", "zip-extract", "curl"]
    );
}

#[tokio::test]
async fn public_entry_cancellation_terminates_every_selected_descendant() {
    for (entry, selected, zip) in [
        ("curl", "curl", false),
        ("gh", "gh", false),
        ("stage", "tar-list", false),
        ("stage", "tar-extract", false),
        ("stage", "zip-list", true),
        ("stage", "zip-extract", true),
        ("prepare", "git", false),
        ("prepare", "julia", false),
        ("prepare", "npm", false),
        ("matches", "git", false),
    ] {
        let requests = Requests::new(zip, Some(selected)).await;
        let mut run = Box::pin(requests.public_entry(entry));
        let descendant = tokio::select! {
            descendant = requests.tree.descendant() => descendant,
            result = &mut run => panic!("fixture did not hold command: {entry}/{selected}: {result}"),
        };
        let result = tokio::time::timeout(Duration::from_millis(50), run).await;
        assert!(
            result.is_err(),
            "fixture failed to hold command: {entry}/{selected}"
        );
        let dead = eventually_dead(&descendant);
        requests.tree.cleanup();
        if !dead {
            assert!(
                eventually_dead(&descendant),
                "fixture cleanup did not complete"
            );
        }
        assert!(dead, "updater descendant survived caller cancellation");
    }
}

#[tokio::test]
async fn leader_exit_ends_pipe_holders_and_preserves_status_and_output() {
    let requests = Requests::new(false, Some("git")).await;
    let run = async {
        let mut command = requests.tree.command();
        tokio::time::timeout(
            Duration::from_secs(10),
            UpdaterSpawner(requests.signal).output(&mut command),
        )
        .await
    };
    let observe = async {
        let descendant = requests.tree.descendant().await;
        std::fs::write(requests.tree.path("release"), b"done").unwrap();
        descendant
    };
    let (result, descendant) = tokio::join!(run, observe);
    let dead = eventually_dead(&descendant);
    requests.tree.cleanup();
    if !dead {
        assert!(
            eventually_dead(&descendant),
            "fixture cleanup did not complete"
        );
    }
    assert!(dead, "updater descendant survived caller cancellation");
    let out = result
        .expect("leader exit did not finish output capture")
        .expect("adapter output");
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(out.stdout, b"fixture stdout");
    assert_eq!(out.stderr, b"fixture stderr");
}

#[tokio::test]
async fn simultaneous_fire_cancels_output_without_detaching_readers() {
    let requests = Requests::new(false, None).await;
    let run = async {
        let mut command = requests.tree.command();
        tokio::time::timeout(
            Duration::from_secs(10),
            UpdaterSpawner(requests.signal).output(&mut command),
        )
        .await
    };
    let fire = async {
        let descendant = requests.tree.descendant().await;
        requests.signal.fire().unwrap();
        descendant
    };
    let (result, descendant) = tokio::join!(run, fire);
    let dead = eventually_dead(&descendant);
    requests.tree.cleanup();
    if !dead {
        assert!(
            eventually_dead(&descendant),
            "fixture cleanup did not complete"
        );
    }
    assert!(dead, "updater descendant survived caller cancellation");
    let out = result.expect("fire did not finish capture").unwrap();
    assert!(!out.status.success());
}

#[tokio::test]
async fn adapter_spawn_failure_keeps_os_reason() {
    let signal = Box::leak(Box::new(Signal::new()));
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = tokio::process::Command::new(dir.path().join("absent"));
    assert_eq!(
        UpdaterSpawner(signal)
            .output(&mut cmd)
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
}
