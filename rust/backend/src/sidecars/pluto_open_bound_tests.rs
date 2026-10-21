use super::Pluto;
use crate::sidecars::contract_tests::{isolated, pin_env, within};
use std::time::Duration;

/// An open the server never answers is an error at `PLUTO_OPEN_BOUND`, and its server is ended: a second open
/// starts a second one. The stand-in for julia says READY, then reads its lines and answers none.
#[tokio::test]
async fn an_open_the_child_never_answers_fails_at_its_bound() {
    if !isolated("sidecars::pluto::open_bound_tests::an_open_the_child_never_answers_fails_at_its_bound", Duration::from_secs(150)) {
        return;
    }
    let dir = tempfile::Builder::new().prefix("sot-pluto-bound-").tempdir().unwrap();
    let stub = dir.path().join("stub-julia");
    let starts = dir.path().join("starts");
    sot_log::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\necho \"READY http://127.0.0.1:1/\"\nwhile read l; do :; done\n",
            starts.display()
        ),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let _pin = pin_env(&stub, dir.path());
    let pluto = Pluto::new(dir.path().to_path_buf(), script);
    let started = || std::fs::read_to_string(&starts).map(|s| s.lines().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();

    let t0 = std::time::Instant::now();
    let err = tokio::time::timeout(Duration::from_secs(45), pluto.open_notebook(&dir.path().join("a.jl")))
        .await
        .expect("the open is still waiting at 45 s")
        .expect_err("an open the child never answers");
    let took = t0.elapsed();
    let bound = Duration::from_secs(30); // PLUTO_OPEN_BOUND, written out so this test also runs on the unbounded code
    assert!(took >= bound && took < bound + Duration::from_secs(5), "ended after {took:?}");
    let text = format!("{err:#}");
    assert!(text.contains("did not answer the open") && text.contains("within 30s"), "{text}");
    let first = started();
    assert_eq!(first.len(), 1);
    let pid: libc::pid_t = first[0].parse().unwrap();
    // SAFETY: signal 0 only probes.
    within(Duration::from_secs(5), "the unanswering server is ended", || unsafe { libc::kill(pid, 0) != 0 }).await;

    let next = {
        let pluto = pluto.clone();
        let path = dir.path().join("b.jl");
        tokio::spawn(async move { pluto.open_notebook(&path).await })
    };
    within(Duration::from_secs(5), "the next open starts a second server", || started().len() == 2).await;
    println!("pluto-bound: {text} after {took:?}; a second open started server {}", started().len());
    next.abort();
    drop(pluto);
}

/// An open whose server fails its boot answers with the child's own last stderr lines, not only that its stdout
/// closed: the stand-in for julia writes Julia's error and exits before READY.
#[tokio::test]
async fn an_open_whose_server_fails_its_boot_answers_with_its_stderr() {
    let name = "sidecars::pluto::open_bound_tests::an_open_whose_server_fails_its_boot_answers_with_its_stderr";
    if !isolated(name, Duration::from_secs(60)) {
        return;
    }
    let dir = tempfile::Builder::new().prefix("sot-pluto-fail-").tempdir().unwrap();
    let stub = dir.path().join("stub-julia");
    sot_log::test_exec::write_executable(
        &stub,
        "#!/bin/sh\necho 'ERROR: ArgumentError: Package Pluto not found' >&2\nexit 1\n".to_string(),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let _pin = pin_env(&stub, dir.path());
    let pluto = Pluto::new(dir.path().to_path_buf(), script);
    let err = pluto.open_notebook(&dir.path().join("a.jl")).await.expect_err("a server that fails its boot");
    let text = format!("{err:#}");
    let said = "stdout closed before READY; its last stderr lines: ERROR: ArgumentError: Package Pluto not found";
    assert!(text.ends_with(said), "{text}");
    drop(pluto);
}

/// The server is up from its start until it ends, whether or not the daemon opened a notebook through it: not before
/// a start, up once an open has started it, down again once the child has exited. The stand-in answers one open and
/// exits.
#[tokio::test]
async fn the_server_is_up_from_its_start_until_it_ends() {
    let name = "sidecars::pluto::open_bound_tests::the_server_is_up_from_its_start_until_it_ends";
    if !isolated(name, Duration::from_secs(60)) {
        return;
    }
    let dir = tempfile::Builder::new().prefix("sot-pluto-live-").tempdir().unwrap();
    let stub = dir.path().join("stub-julia");
    sot_log::test_exec::write_executable(
        &stub,
        "#!/bin/sh\necho 'READY http://pluto.invalid/'\nread l\necho 'URL http://pluto.invalid/a'\nsleep 1\n".to_string(),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let _pin = pin_env(&stub, dir.path());
    let pluto = Pluto::new(dir.path().to_path_buf(), script);
    assert!(!pluto.server_up().await, "no server started, none up");
    pluto.open_notebook(&dir.path().join("a.jl")).await.expect("the open");
    assert!(pluto.server_up().await, "the server answered an open");
    let began = std::time::Instant::now();
    while pluto.server_up().await {
        assert!(began.elapsed() < Duration::from_secs(20), "the server outlived its child");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(pluto);
}

/// The server is up while it is still starting, before its READY: an open waiting on the start has a running server
/// to wait for. The stand-in says READY only when the test creates `go`, so the start is held open for the check.
#[tokio::test]
async fn the_server_is_up_while_it_is_still_starting() {
    let name = "sidecars::pluto::open_bound_tests::the_server_is_up_while_it_is_still_starting";
    if !isolated(name, Duration::from_secs(60)) {
        return;
    }
    let dir = tempfile::Builder::new().prefix("sot-pluto-up-").tempdir().unwrap();
    let (stub, go, said) = (dir.path().join("stub-julia"), dir.path().join("go"), dir.path().join("said"));
    sot_log::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\necho 'READY http://pluto.invalid/'\n\
                 while read l; do echo 'URL http://pluto.invalid/edit'; done\n",
            said.display(),
            go.display()
        ),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let _pin = pin_env(&stub, dir.path());
    let pluto = Pluto::new(dir.path().to_path_buf(), script);
    assert!(!pluto.server_up().await, "no server started, none up");
    let open = {
        let (pluto, path) = (pluto.clone(), dir.path().join("a.jl"));
        tokio::spawn(async move { pluto.open_notebook(&path).await })
    };
    within(Duration::from_secs(20), "the stand-in has started", || said.exists()).await;
    assert!(!open.is_finished(), "the open waits for READY");
    assert!(pluto.server_up().await, "a server still starting, before READY, is up");
    std::fs::write(&go, "").unwrap();
    let answer = open.await.expect("the open task").expect("the open");
    assert_eq!(answer, super::Opened::Url("http://pluto.invalid/edit".to_string()));
    assert!(pluto.server_up().await, "the server is up once READY came");
    drop(pluto);
}

/// An open behind a server that has not said READY answers within `PLUTO_START_WAIT` that the server is still
/// starting, with its age and the child's own stderr lines, and logs that at warn; the start goes on, and the next
/// open joins it (one server started) and is served once READY comes. The stand-in for julia writes what a juliaup
/// launcher writes while it installs a Julia, and says READY only when the test creates `go`.
#[tokio::test]
async fn an_open_behind_a_server_still_starting_says_so_and_the_next_joins_it() {
    let name = "sidecars::pluto::open_bound_tests::an_open_behind_a_server_still_starting_says_so_and_the_next_joins_it";
    if !isolated(name, Duration::from_secs(60)) {
        return;
    }
    let log = sot_log::test_log::capture();
    let dir = tempfile::Builder::new().prefix("sot-pluto-start-").tempdir().unwrap();
    let (stub, starts, go) = (dir.path().join("stub-julia"), dir.path().join("starts"), dir.path().join("go"));
    let said = dir.path().join("said");
    sot_log::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\necho '    Checking for new Julia versions' >&2\necho '  Installing Julia 1.13.2+0.x64.linux.gnu' >&2\ntouch '{}'\n\
                 while [ ! -e '{}' ]; do sleep 0.05; done\necho 'READY http://pluto.invalid/'\nwhile read l; do echo 'URL http://pluto.invalid/edit'; done\n",
            starts.display(),
            said.display(),
            go.display()
        ),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let _pin = pin_env(&stub, dir.path());
    let pluto = Pluto::new(dir.path().to_path_buf(), script);

    // The wait is the product's `PLUTO_START_WAIT`, crossed on the paused clock once the stand-in's lines are
    // read: not before 119 s, and by 121 s.
    let t0 = tokio::time::Instant::now();
    let open = {
        let (pluto, path) = (pluto.clone(), dir.path().join("a.jl"));
        tokio::spawn(async move { pluto.open_notebook(&path).await })
    };
    within(Duration::from_secs(20), "the stand-in has written its lines", || said.exists()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(119).saturating_sub(t0.elapsed())).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!open.is_finished(), "the open answered before 119 s");
    tokio::time::advance(Duration::from_secs(121).saturating_sub(t0.elapsed())).await;
    let answer = open.await.expect("the open task").expect("a start still going is no failure");
    tokio::time::resume();
    let super::Opened::Starting(text) = answer else { panic!("the open answered a URL: {answer:?}") };
    println!("pluto-starting: {text}");
    let lines = " s; its last stderr lines: Checking for new Julia versions / Installing Julia 1.13.2+0.x64.linux.gnu";
    assert!(text.starts_with("Pluto is still starting after ") && text.ends_with(lines), "{text}");
    assert!(crate::sidecars::warned(&log.text(), "Installing Julia 1.13.2+0.x64.linux.gnu"), "{}", log.text());

    std::fs::write(&go, "").unwrap();
    let url = tokio::time::timeout(Duration::from_secs(20), pluto.open_notebook(&dir.path().join("b.jl")))
        .await
        .expect("the next open is served once the start finishes")
        .expect("the next open joins the start");
    assert_eq!(url, super::Opened::Url("http://pluto.invalid/edit".to_string()));
    let started = std::fs::read_to_string(&starts).unwrap();
    assert_eq!(started.lines().count(), 1, "the next open started a second server: {started:?}");
    drop(pluto);
}
