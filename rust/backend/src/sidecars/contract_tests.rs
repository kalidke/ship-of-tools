//! Native fixture premises on the unchanged release parent; these do not prove C1 constructor injection.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::lifecycle::child_signal::Signal;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

fn isolated_premise(name: &str) -> bool {
    if std::env::var("SOT_TEST_D_L_PREMISE").as_deref() == Ok(name) {
        sot_log::test_isolated::enter(name);
        return true;
    }
    let (mut command, entry) = sot_log::test_isolated::test_command(name);
    command
        .env("SOT_TEST_D_L_PREMISE", name)
        .stdin(Stdio::null());
    #[allow(
        clippy::disallowed_methods,
        reason = "bounded premise child through test_isolated::test_command, enter and assert_once"
    )]
    let child = command.spawn().expect("start isolated premise body");
    let pid = child.id();
    let (status, output, errors) =
        sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(90));
    print!("{output}{errors}");
    entry.assert_once(pid);
    assert!(status.success(), "native premise body failed");
    false
}

fn executable(name: &str) -> PathBuf {
    let search = std::env::var_os("PATH").expect("premise setup: executable search unavailable");
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&search)
        .map(|entry| entry.join(&filename))
        .find(|candidate| candidate.is_absolute() && candidate.is_file())
        .expect("premise setup: required native executable missing")
}

fn copy_folder(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("create owned fixture folder");
    for entry in std::fs::read_dir(source).expect("read repository fixture source") {
        let entry = entry.expect("fixture source entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture source kind").is_dir() {
            copy_folder(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy fixture source");
        }
    }
}

fn isolated_julia(root: &Path, project: &Path) -> Command {
    let mut command = Command::new(executable("julia"));
    command
        .args([
            "--startup-file=no",
            "--history-file=no",
            "--compiled-modules=no",
        ])
        .arg(format!("--project={}", project.display()))
        .env("JULIA_DEPOT_PATH", root.join("depot"))
        .env("JULIA_LOAD_PATH", "@:@stdlib")
        .env("JULIA_PKG_OFFLINE", "true")
        .env("JULIA_PKG_SERVER", "");
    command
}

async fn signal_exchange(
    mut command: Command,
    request: &[u8],
    accepts: impl Fn(&str) -> bool,
    label: &str,
) {
    let signal = Box::leak(Box::new(Signal::new()));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = signal
        .spawn(&mut command)
        .expect("premise setup: native child did not spawn");
    let mut stdin = child.stdin.take().expect("owned stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("owned stdout")).lines();
    let mut stderr = child.stderr.take().expect("owned stderr");
    let diagnostics = tokio::spawn(async move {
        let mut text = String::new();
        stderr.read_to_string(&mut text).await.map(|_| text)
    });
    let work = tokio::time::timeout(Duration::from_secs(60), async {
        stdin.write_all(request).await?;
        stdin.flush().await?;
        while let Some(line) = stdout.next_line().await? {
            if accepts(&line) {
                return Ok::<bool, std::io::Error>(true);
            }
        }
        Ok(false)
    })
    .await;
    signal.fire();
    drop(stdin);
    let reaped = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let diagnostic = tokio::time::timeout(Duration::from_secs(5), diagnostics).await;
    let module_missing =
        matches!(&diagnostic, Ok(Ok(Ok(text))) if text.contains("ERR_MODULE_NOT_FOUND"));
    println!("D-L premise {label} module-missing {module_missing}");
    println!(
        "D-L premise {label} work-completed {}",
        matches!(work, Ok(Ok(true)))
    );
    match &reaped {
        Ok(Ok(_)) => println!("D-L premise {label} direct-reap PASS"),
        Ok(Err(error)) => println!("D-L premise {label} direct-reap FAIL: {error}"),
        Err(_) => println!("D-L premise {label} direct-reap FAIL: deadline exceeded"),
    }
    assert!(
        matches!(reaped, Ok(Ok(_))),
        "premise cleanup: direct-child reap unconfirmed"
    );
    drop(child);
    assert_eq!(signal.live(), 0, "premise cleanup: owner still registered");
    println!("D-L premise {label} cleanup PASS");
    assert!(
        matches!(work, Ok(Ok(true))),
        "premise setup: real native work did not complete"
    );
    println!("D-L premise {label} real-work PASS");
    assert!(
        signal.spawn(&mut command).is_err(),
        "premise: fired Signal permitted another start"
    );
    assert_eq!(signal.live(), 0, "premise: refused start created an owner");
    println!("D-L premise {label} refused-start PASS");
}

#[tokio::test]
async fn julia_private_signal_premise() {
    if !isolated_premise("sidecars::contract_tests::julia_private_signal_premise") {
        return;
    }
    let root = tempfile::tempdir().expect("owned fixture root").keep();
    let shim = root.join("shim");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../julia/repl");
    copy_folder(&source.join("src"), &shim.join("src"));
    std::fs::copy(source.join("Project.toml"), shim.join("Project.toml"))
        .expect("copy shim project");
    let mut command = isolated_julia(&root, &shim);
    command.args([
        "-e",
        "using ShipToolsRepl; ShipToolsRepl.serve(stdin, stdout)",
    ]);
    let request = b"{\"v\":1,\"id\":71,\"kind\":\"req\",\"op\":\"repl.eval\",\"payload\":{\"code\":\"40 + 2\",\"eval_id\":71}}\n";
    signal_exchange(
        command,
        request,
        |line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|frame| {
                frame["payload"]["frame"]["kind"] == "value"
                    && frame["payload"]["frame"]["text"] == "42"
            })
        },
        "Julia",
    )
    .await;
    std::fs::remove_dir_all(&root).expect("remove fixture only after confirmed reap");
}

#[tokio::test]
async fn mathjax_private_signal_premise() {
    if !isolated_premise("sidecars::contract_tests::mathjax_private_signal_premise") {
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("sidecars/mathjax/render.mjs");
    let mut command = Command::new(executable("node"));
    command.arg(script);
    signal_exchange(
        command,
        b"{\"id\":71,\"tex\":\"x^2\",\"display\":false}\n",
        |line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|frame| {
                frame["id"] == 71
                    && frame["svg"]
                        .as_str()
                        .is_some_and(|svg| svg.contains("<svg"))
            })
        },
        "MathJax",
    )
    .await;
}

#[tokio::test]
async fn offline_stdlib_add_premise() {
    if !isolated_premise("sidecars::contract_tests::offline_stdlib_add_premise") {
        return;
    }
    let root = tempfile::tempdir()
        .expect("owned package fixture root")
        .keep();
    let shim = root.join("shim");
    let workspace = root.join("workspace with spaces");
    std::fs::create_dir(&workspace).expect("create bare workspace");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../julia/repl");
    copy_folder(&source.join("src"), &shim.join("src"));
    std::fs::copy(source.join("Project.toml"), shim.join("Project.toml"))
        .expect("copy shim project");
    let project_before = std::fs::read(shim.join("Project.toml")).expect("read copied project");
    let registry = root.join("depot/registries/Fixture");
    std::fs::create_dir_all(&registry).expect("create owned minimal registry");
    std::fs::write(registry.join("Registry.toml"), "name = \"Fixture\"\nuuid = \"a58b057d-d7df-4e29-a186-405f45c2cafd\"\nrepo = \"\"\n[packages]\n").expect("write owned minimal registry");
    let mut command = isolated_julia(&root, &workspace);
    let separator = if cfg!(windows) { ';' } else { ':' };
    command.current_dir(&workspace).env(
        "JULIA_LOAD_PATH",
        format!("@{separator}{}{separator}", shim.display()),
    );
    command.args(["-e", "using ShipToolsRepl, Pkg; @assert dirname(Base.active_project()) == pwd(); Pkg.offline(true); Pkg.add(\"LinearAlgebra\"; io=devnull); @assert isfile(joinpath(pwd(), \"Project.toml\")); @assert isfile(joinpath(pwd(), \"Manifest.toml\")); println(\"D-L premise offline-stdlib-add PASS\"); flush(stdout); readline(stdin)"]);
    signal_exchange(
        command,
        b"\n",
        |line| line == "D-L premise offline-stdlib-add PASS",
        "offline-package",
    )
    .await;
    assert!(
        std::fs::read(shim.join("Project.toml")).expect("read copied project after add")
            == project_before,
        "premise: package operation edited copied shim project"
    );
    assert!(
        !shim.join("Manifest.toml").exists(),
        "premise: package operation edited copied shim"
    );
    println!("D-L premise copied-shim-unchanged PASS");
    std::fs::remove_dir_all(&root).expect("remove package fixture only after confirmed reap");
}
