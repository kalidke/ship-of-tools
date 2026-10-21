use super::*;
use crate::lifecycle::child_signal::tests::Leftover;
use std::time::Duration;

/// Holds `ENV_TEST_LOCK` and pins the julia the daemon resolves, so a
/// render does not depend on a julia being installed; restores both
/// variables it touches.
pub(super) struct JuliaPin {
    _serial: std::sync::MutexGuard<'static, ()>,
    saved: [(&'static str, Option<std::ffi::OsString>); 2],
}

impl JuliaPin {
    pub(super) fn new(julia: &std::path::Path, quarto_julia: Option<&str>) -> Self {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = [
            ("SOT_JULIA_BIN", std::env::var_os("SOT_JULIA_BIN")),
            ("QUARTO_JULIA", std::env::var_os("QUARTO_JULIA")),
        ];
        std::env::set_var("SOT_JULIA_BIN", julia);
        match quarto_julia {
            Some(v) => std::env::set_var("QUARTO_JULIA", v),
            None => std::env::remove_var("QUARTO_JULIA"),
        }
        Self { _serial, saved }
    }
}

impl Drop for JuliaPin {
    fn drop(&mut self) {
        for (key, value) in self.saved.iter_mut() {
            match value.take() {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Run `stub` (a script body) as `quarto render` to its end on a private signal.
fn run_stub_quarto(dir: &std::path::Path, body: &str) -> std::io::Result<Option<std::process::Output>> {
    let stub = dir.join("stub-quarto");
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\n{body}"));
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let program = stub.to_string_lossy().into_owned();
    rt.block_on(run_quarto(&program, dir, std::ffi::OsStr::new("doc.qmd"), "out.html", true, QUARTO_RUN_BOUND, sig))
}

/// The shutdown signal kills the render's whole process group, engines included.
#[tokio::test]
async fn shutdown_kills_the_quarto_render_and_its_children() {
    let dir = tempfile::tempdir().unwrap();
    let _pin = JuliaPin::new(&dir.path().join("julia"), None);
    let pid_file = dir.path().join("engine.pid");
    let engine = Leftover::of_file(pid_file.clone());
    let stub = dir.path().join("stub-quarto");
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n", pid_file.display()));
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let task = tokio::spawn(async move {
        run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, QUARTO_RUN_BOUND, sig).await
    });
    let began = std::time::Instant::now();
    while !pid_file.exists() || std::fs::read_to_string(&pid_file).unwrap().trim().is_empty() {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub render never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sig.fire().expect("fire");
    let done = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the render outlived the shutdown")
        .expect("render task")
        .expect("run_quarto");
    assert!(done.is_none(), "a killed render has no output");
    assert!(sig.held_groups().is_empty());
    assert!(engine.gone(), "the engine child survived the shutdown");
}

/// An engine that holds the render's pipes after its launcher exits dies
/// with the launcher's tree, so the render ends by itself and no signal
/// is needed.
#[tokio::test]
async fn quarto_engine_dies_when_its_launcher_exits() {
    let dir = tempfile::tempdir().unwrap();
    let _pin = JuliaPin::new(&dir.path().join("julia"), None);
    let pid_file = dir.path().join("engine.pid");
    let engine = Leftover::of_file(pid_file.clone());
    let stub = dir.path().join("stub-quarto");
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nsleep 3102 &\necho $! > {}\nexit 0\n", pid_file.display()));
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let task = tokio::spawn(async move {
        run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, QUARTO_RUN_BOUND, sig).await
    });
    let done = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the render outlived its launcher's exit")
        .expect("render task")
        .expect("run_quarto");
    assert!(done.is_some_and(|out| out.status.success()), "the render did not finish on its own");
    assert!(sig.held_groups().is_empty());
    assert!(engine.gone(), "the engine survived its launcher's exit");
}

/// Quarto runs `QUARTO_JULIA` for its julia engine and otherwise a bare
/// `julia`, which on Windows can be an app-execution alias that runs
/// outside the daemon's containment; the daemon hands it the one julia
/// it resolves for every other child.
#[test]
fn quarto_is_given_the_daemons_julia() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = (std::env::var_os("SOT_JULIA_BIN"), std::env::var_os("QUARTO_JULIA"));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("qj");
    let stub = dir.path().join("stub-quarto");
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nprintf '%s' \"$QUARTO_JULIA\" > {}\n", out.display()));
    let julia = dir.path().join("julia");
    std::env::set_var("SOT_JULIA_BIN", &julia);
    std::env::remove_var("QUARTO_JULIA");
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let ran = rt.block_on(run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, QUARTO_RUN_BOUND, sig));
    for (key, value) in [("SOT_JULIA_BIN", saved.0), ("QUARTO_JULIA", saved.1)] {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    ran.expect("run_quarto");
    assert_eq!(
        std::fs::read_to_string(&out).unwrap_or_default(),
        julia.to_string_lossy(),
        "quarto was not given the daemon's julia"
    );
}

/// A group's number is its leader's pid and is free the moment the
/// leader is reaped: a tree still held after that names whatever process
/// next takes the number. The launcher here exits at once; a descendant
/// in its own session keeps the render's pipes open, so the render is
/// still running when the registry is read.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn quarto_never_holds_a_freed_group_number() {
    let dir = tempfile::tempdir().unwrap();
    let _pin = JuliaPin::new(&dir.path().join("julia"), None);
    let sleeper = dir.path().join("sleeper");
    let leftover = Leftover::of_file(sleeper.clone());
    leftover.kept_alive();
    let stub = dir.path().join("stub-quarto");
    // The pid is written once the sleeper has its own session: until then a kill of the launcher's group takes it too.
    let body = "setsid sleep 120 &\np=$!\nwhile [ \"$(cut -d' ' -f6 /proc/$p/stat)\" = \"$(cut -d' ' -f6 /proc/$$/stat)\" ]; do sleep 0.02; done\n";
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\n{body}echo $p > {}\nexit 0\n", sleeper.display()));
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let task = tokio::spawn(async move {
        run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, QUARTO_RUN_BOUND, sig).await
    });
    let began = std::time::Instant::now();
    while std::fs::read_to_string(&sleeper).map(|s| s.trim().is_empty()).unwrap_or(true) {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub render never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "the render ended while a descendant held its pipes");
    for group in sig.held_groups() {
        // SAFETY: signal 0 only probes the pid.
        assert_eq!(unsafe { libc::kill(group, 0) }, 0, "a held tree names a process-group number nothing holds");
    }
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the render outlived the shutdown")
        .expect("render task")
        .expect("run_quarto");
}

/// A real resolver error fails the render before Quarto starts, with the resolver's diagnostic preserved.
#[test]
fn quarto_refuses_what_the_resolver_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let _pin = JuliaPin::new(std::path::Path::new("relative-julia"), None);
    let refusal = crate::sidecars::julia::resolve_bin().unwrap_err().to_string();
    let out = dir.path().join("qj");
    let ran = run_stub_quarto(dir.path(), &format!("printf '%s' \"$QUARTO_JULIA\" > {}\n", out.display()));
    assert!(!out.exists(), "a refused julia still rendered");
    let err = ran.expect_err("a refused julia must fail the render");
    assert_eq!(err.to_string(), refusal, "the resolver's diagnostic is preserved");
}

/// The daemon's julia replaces one the environment already names.
#[test]
fn an_inherited_quarto_julia_is_overridden() {
    let dir = tempfile::tempdir().unwrap();
    let julia = dir.path().join("julia");
    let _pin = JuliaPin::new(&julia, Some("inherited-julia"));
    let out = dir.path().join("qj");
    run_stub_quarto(dir.path(), &format!("printf '%s' \"$QUARTO_JULIA\" > {}\n", out.display())).expect("run_quarto");
    assert_eq!(
        std::fs::read_to_string(&out).unwrap_or_default(),
        julia.to_string_lossy(),
        "quarto kept an inherited QUARTO_JULIA"
    );
}
