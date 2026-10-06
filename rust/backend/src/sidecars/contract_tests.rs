//! Native fixture premises on the unchanged release parent; these do not prove C1 constructor injection.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::lifecycle::child_signal::Signal;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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

fn julia_load_path(entries: &[&str]) -> String {
    entries.join(if cfg!(windows) { ";" } else { ":" })
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
        .env("JULIA_LOAD_PATH", julia_load_path(&["@", "@stdlib"]))
        .env("JULIA_PKG_OFFLINE", "true")
        .env("JULIA_PKG_SERVER", "");
    command
}

fn diagnostic_redactions() -> Vec<String> {
    [
        "HOME",
        "USERPROFILE",
        "USER",
        "USERNAME",
        "HOSTNAME",
        "COMPUTERNAME",
    ]
    .iter()
    .filter_map(|name| std::env::var(name).ok())
    .filter(|value| !value.is_empty())
    .collect()
}

fn sanitize_diagnostic(line: &str, private: &[String]) -> String {
    let mut text = line.to_owned();
    for value in private {
        text = text.replace(value, "<private>");
    }
    text.split_whitespace()
        .map(|word| {
            if word.contains('/') || word.contains('\\') {
                "<path>".to_owned()
            } else {
                word.chars().filter(|ch| !ch.is_control()).collect()
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

async fn stderr_tail(
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
) -> Option<ErrorKind> {
    let mut stderr = BufReader::new(stderr);
    loop {
        let mut line = Vec::new();
        let read = stderr.read_until(b'\n', &mut line).await;
        if !line.is_empty() {
            let mut tail = tail.lock().expect("diagnostic tail lock");
            if tail.len() == 8 {
                tail.pop_front();
            }
            tail.push_back(String::from_utf8_lossy(&line).into_owned());
        }
        match read {
            Ok(0) => return None,
            Err(error) => return Some(error.kind()),
            Ok(_) => {}
        }
    }
}

fn report_work(
    work: &Result<Result<Option<usize>, std::io::Error>, tokio::time::error::Elapsed>,
    label: &str,
) -> bool {
    let outcome = match work {
        Ok(Ok(None)) => "matched".to_owned(),
        Ok(Ok(Some(0))) => "ended-without-output".to_owned(),
        Ok(Ok(Some(lines))) => format!("ended-with-unmatched-output lines={lines}"),
        Ok(Err(error)) => format!("io-failure kind={:?}", error.kind()),
        Err(_) => "timeout".to_owned(),
    };
    let completed = matches!(work, Ok(Ok(None)));
    println!("D-L premise {label} work-outcome {outcome}");
    println!("D-L premise {label} work-completed {completed}");
    completed
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
    let private = diagnostic_redactions();
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let diagnostics = tokio::spawn(stderr_tail(
        child.stderr.take().expect("owned stderr"),
        Arc::clone(&tail),
    ));
    let work = tokio::time::timeout(Duration::from_secs(60), async {
        stdin.write_all(request).await?;
        stdin.flush().await?;
        let mut lines = 0;
        while let Some(line) = stdout.next_line().await? {
            lines += 1;
            if accepts(&line) {
                return Ok::<Option<usize>, std::io::Error>(None);
            }
        }
        Ok(Some(lines))
    })
    .await;
    signal.fire();
    drop(stdin);
    let reaped = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let diagnostic = tokio::time::timeout(Duration::from_secs(5), diagnostics).await;
    let completed = report_work(&work, label);
    match diagnostic {
        Ok(Ok(error)) => println!("D-L premise {label} stderr-read {error:?}"),
        Ok(Err(_)) => println!("D-L premise {label} stderr-read task-failure"),
        Err(_) => println!("D-L premise {label} stderr-read timeout"),
    }
    let tail = tail.lock().expect("read diagnostic tail").clone();
    if tail.is_empty() {
        println!("D-L premise {label} stderr-tail <empty>");
    }
    for line in tail {
        println!(
            "D-L premise {label} stderr-tail {}",
            sanitize_diagnostic(&line, &private)
        );
    }
    match &reaped {
        Ok(Ok(_)) => println!("D-L premise {label} direct-reap PASS"),
        Ok(Err(error)) => println!(
            "D-L premise {label} direct-reap FAIL: kind={:?}",
            error.kind()
        ),
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
        completed,
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
    let shim_path = shim.to_str().expect("fixture path is Unicode");
    command
        .current_dir(&workspace)
        .env("JULIA_LOAD_PATH", julia_load_path(&["@", shim_path, ""]));
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

#[test]
fn diagnostics_hide_private_values_and_native_paths() {
    let private = vec!["fixture-user".to_owned(), "fixture-host".to_owned()];
    assert_eq!(
        sanitize_diagnostic(
            "ERROR fixture-user on fixture-host at /fixture/repl/file.jl:7",
            &private
        ),
        "ERROR <private> on <private> at <path>"
    );
    assert_eq!(
        sanitize_diagnostic(
            r"ERROR at C:\fixture\repl\file.jl:7 ERR_MODULE_NOT_FOUND",
            &private
        ),
        "ERROR at <path> ERR_MODULE_NOT_FOUND"
    );
    println!("D-L premise sanitized-diagnostics PASS");
}
