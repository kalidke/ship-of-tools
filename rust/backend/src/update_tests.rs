use super::*;
use sot_updater::pending::PendingPointer;
use sot_updater::UnreadableInstall;
use sot_updater::identity::DEFAULT_REPO;

/// Every updater command is a contained child of the signal: its two streams and its status come back whole, and a
/// command its caller drops or a fire ends the command's whole tree.
#[cfg(unix)]
mod spawner {
    use super::*;
    use sot_containment::{leftover::Leftover, Signal};
    use sot_updater::Spawner;
    use std::time::{Duration, Instant};

    fn a_signal() -> &'static Signal {
        Box::leak(Box::new(Signal::new()))
    }

    /// A command that starts a grandchild, writes its pid to `pid_file` and waits for it.
    fn tree_command(pid_file: &std::path::Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "sleep 3180 & echo $! > \"$1\"; wait", "sh"])
            .arg(pid_file);
        command
    }

    /// A command that leaves a file in its temporary folder, as Pkg leaves a package download there when the call
    /// that made it does not reach its end, writes the file's path to `report`, then runs `then`. It finds the
    /// folder as Julia's `tempdir()` does, by `TMPDIR`: a bare `mktemp` would not, since macOS's makes its file in
    /// the account's own temporary folder (`getconf DARWIN_USER_TEMP_DIR`) whatever `TMPDIR` names.
    fn leaving_command(report: &std::path::Path, then: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("sh");
        let leave = format!("f=$(mktemp \"${{TMPDIR:-/tmp}}/left.XXXXXX\") && echo \"$f\" > \"$1\" && {then}");
        command
            .args(["-c", &leave, "sh"])
            .arg(report);
        command
    }

    /// The file a [`leaving_command`] named, removed when the test ends however it ends.
    struct Left(std::path::PathBuf);

    impl Left {
        fn named_in(report: &std::path::Path) -> Self {
            Self(std::fs::read_to_string(report).expect("the command named its file").trim().into())
        }

        /// Whether the file is gone within 10 s.
        async fn goes(&self) -> bool {
            let began = Instant::now();
            while self.0.exists() && began.elapsed() < Duration::from_secs(10) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            !self.0.exists()
        }
    }

    impl Drop for Left {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn pid_written(file: &std::path::Path) {
        let began = Instant::now();
        while std::fs::read_to_string(file).map_or(true, |t| t.trim().is_empty()) {
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "the updater command never started its child"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn the_spawner_returns_both_streams_and_the_status() {
        let signal = a_signal();
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "echo out; echo err >&2; exit 3"]);
        let output = UpdaterSpawner(signal)
            .output(&mut command)
            .await
            .expect("the command ran");
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
        assert!(
            signal.held_groups().is_empty(),
            "a finished command's tree is still held"
        );
    }

    #[tokio::test]
    async fn a_dropped_updater_command_ends_its_tree() {
        let signal = a_signal();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pid");
        let mut command = tree_command(&file);
        let task =
            tokio::spawn(async move { UpdaterSpawner(signal).output(&mut command).await });
        pid_written(&file).await;
        let grandchild = Leftover::of_file(&file);
        assert_eq!(
            signal.held_groups().len(),
            1,
            "the running command's tree is not held"
        );
        task.abort();
        let _ = task.await;
        assert!(
            grandchild.gone(),
            "the dropped command's grandchild outlived it"
        );
        assert!(signal.held_groups().is_empty());
    }

    /// What a command leaves in its temporary folder goes with it: a julia step that exits on an error Pkg rethrows
    /// leaves its download there, and nothing else removes it.
    #[tokio::test]
    async fn a_finished_updater_commands_temporary_files_go_with_it() {
        let signal = a_signal();
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("left");
        let output = UpdaterSpawner(signal)
            .output(&mut leaving_command(&report, "exit 1"))
            .await
            .expect("the command ran");
        assert_eq!(output.status.code(), Some(1));
        let left = Left::named_in(&report);
        assert!(
            left.goes().await,
            "the finished command's temporary file is still there: {}",
            left.0.display()
        );
    }

    /// A command its caller drops (a step past its bound) leaves nothing in its temporary folder either.
    #[tokio::test]
    async fn a_dropped_updater_commands_temporary_files_go_with_it() {
        let signal = a_signal();
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("left");
        let mut command = leaving_command(&report, "exec sleep 3180");
        let task =
            tokio::spawn(async move { UpdaterSpawner(signal).output(&mut command).await });
        pid_written(&report).await;
        let left = Left::named_in(&report);
        task.abort();
        let _ = task.await;
        assert!(
            left.goes().await,
            "the dropped command's temporary file is still there: {}",
            left.0.display()
        );
        assert!(signal.held_groups().is_empty());
    }

    #[tokio::test]
    async fn a_fire_ends_a_running_updater_command_tree() {
        let signal = a_signal();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pid");
        let mut command = tree_command(&file);
        let task =
            tokio::spawn(async move { UpdaterSpawner(signal).output(&mut command).await });
        pid_written(&file).await;
        let grandchild = Leftover::of_file(&file);
        signal.fire().expect("fire");
        assert!(
            grandchild.gone(),
            "the fired signal left the grandchild running"
        );
        let _ = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the command outlived the fire");
        let mut late = tokio::process::Command::new("sh");
        late.args(["-c", "exit 0"]);
        assert!(
            UpdaterSpawner(signal).output(&mut late).await.is_err(),
            "a command started after the fire"
        );
    }
}

/// The daemon's release repo: the default when the variable is unset, empty
/// or blank, the trimmed value otherwise; the prior value is restored.
#[test]
fn repo_from_env_trims_and_defaults() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = crate::paths::EnvGuard::capture("SOT_UPDATE_REPO");
    std::env::remove_var("SOT_UPDATE_REPO");
    assert_eq!(repo_from_env(), DEFAULT_REPO);
    std::env::set_var("SOT_UPDATE_REPO", "");
    assert_eq!(repo_from_env(), DEFAULT_REPO);
    std::env::set_var("SOT_UPDATE_REPO", "   ");
    assert_eq!(repo_from_env(), DEFAULT_REPO);
    std::env::set_var("SOT_UPDATE_REPO", "  fork/x  ");
    assert_eq!(repo_from_env(), "fork/x");
}

/// Only a backend role prepares Julia. prepare_julia returns the shared resolver's selected path or error unchanged; directory spelling does not establish alias identity.
#[test]
fn the_update_prepare_runs_the_resolvers_julia() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = crate::paths::EnvGuard::capture("SOT_JULIA_BIN");
    std::env::set_var("SOT_JULIA_BIN", "relative-julia");
    let refusal = crate::sidecars::julia::resolve_bin().unwrap_err().to_string();
    assert_eq!(prepare_julia(true), Err(refusal), "the resolver's error is returned unchanged");
    assert_eq!(prepare_julia(false), Ok(None), "a frontend-only role prepares no julia, whatever the override");
    // A path that does not exist, under a folder with the name an alias lives in, is a selection for the spawn
    // to fail on, not evidence of an alias.
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("WindowsApps").join("julia");
    std::env::set_var("SOT_JULIA_BIN", &stub);
    assert_eq!(prepare_julia(true), Ok(Some(stub.to_string_lossy().into_owned())));
    assert_eq!(prepare_julia(false), Ok(None));
}

/// A Windows prepare with no Julia anywhere skips the Julia steps; a Julia the resolver refused is an error on every
/// platform, and the others refuse with the resolver's reason.
#[test]
fn a_windows_prepare_without_julia_skips_it_but_not_a_refused_one() {
    let none = || Err(JuliaError::NotFound("no julia".to_string()));
    let refused = || Err(JuliaError::Refused("SOT_JULIA_BIN=\"julia\" is not an absolute path".to_string()));
    assert_eq!(julia_or_skip(none(), true), Ok(None), "Windows: the launcher instantiates at first start");
    assert_eq!(julia_or_skip(none(), false), Err("no julia".to_string()), "elsewhere the reason ends the stage");
    assert_eq!(
        julia_or_skip(refused(), true),
        Err("SOT_JULIA_BIN=\"julia\" is not an absolute path".to_string()),
        "Windows: a refusal names itself and ends the stage"
    );
    assert_eq!(julia_or_skip(Ok("/x/julia".to_string()), true), Ok(Some("/x/julia".to_string())), "a resolved julia is used");
}

/// Once a shutdown has begun its own exit stands: the update's is
/// skipped (ruling f).
#[tokio::test]
async fn update_exit_yields_to_shutdown() {
    let leases = Leases::new(Some("boot".into()), None, None, false);
    async fn exit_code(leases: &Leases) -> Option<i32> {
        let mut code = None;
        exit_for_update(leases, |c| code = Some(c)).await;
        code
    }
    assert_eq!(
        exit_code(&leases).await,
        Some(sot_protocol::ops::lease::EXIT_UPDATE_RESTART),
        "no shutdown under way: the update exits 75"
    );
    let leases = Leases::new(Some("boot".into()), None, None, false);
    leases.begin_close();
    assert_eq!(
        exit_code(&leases).await,
        None,
        "the update exits 75 during a shutdown"
    );
    leases.finish_shutdown(0, Vec::new()).unwrap();
    assert_eq!(
        exit_code(&leases).await,
        None,
        "the update exits 75 after the shutdown's final record"
    );
}

/// An update committed first stands: a close that comes after it does not begin, so the daemon cannot be sent down the
/// close's exit 0 instead of the restart.
#[tokio::test]
async fn a_committed_update_is_not_undone_by_a_later_close() {
    let leases = Leases::new(Some("boot".into()), None, None, false);
    let mut code = None;
    exit_for_update(&leases, |c| code = Some(c)).await;
    assert_eq!(code, Some(sot_protocol::ops::lease::EXIT_UPDATE_RESTART));
    leases.begin_close();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), leases.gone())
            .await
            .is_err(),
        "a close began after the update was committed"
    );
    assert!(!leases.commit_update(), "a second update committed");
}

fn topo(text: &str) -> sot_topology::Topology {
    sot_topology::parse(text).expect("fixture must parse")
}

// The regression this pins: `install.role != "remote"` used to decide
// this; install.json carries no role any more (plan step 6), so a
// daemon box and a frontend-only box must still take the branch they
// always did, now read from the declared topology first, install.json's
// recorded `daemon` bit second (a listless box), `true` last.

#[test]
fn a_daemon_box_still_prepares_julia_and_npm() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.host-2]\n\
             daemon = true\n",
    );
    assert!(backend_role_from_topology(Some(&t), "host-2", None));
}

#[test]
fn a_frontend_entry_wants_a_backend_for_its_own_daemon() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n\
             frontend = true\n",
    );
    assert!(backend_role_from_topology(Some(&t), "laptop", None));
    assert!(backend_role_from_topology(Some(&t), "laptop", Some(true)));
}

// An install from before the installer gave a frontend entry a daemon records `daemon: false` and runs none, so its
// prepare is the light one.
#[test]
fn a_frontend_entry_whose_install_runs_no_daemon_prepares_light() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n\
             frontend = true\n",
    );
    assert!(!backend_role_from_topology(Some(&t), "laptop", Some(false)));
}

#[test]
fn a_box_the_topology_does_not_name_falls_back_to_the_recorded_bit() {
    let t = topo("hub = \"hubbox\"\n[host.hubbox]\ndaemon = true\n");
    assert!(backend_role_from_topology(Some(&t), "nowhere", None));
}

#[test]
fn no_topology_and_no_recorded_bit_defaults_to_true() {
    assert!(backend_role_from_topology(None, "anything", None));
}

// The case this closes: a listless frontend-only-over-ssh install has
// no hosts.toml at all, so nothing but install.json's own recorded
// `daemon: false` can tell the update path to skip Julia/npm here.
#[test]
fn a_listless_frontend_only_box_uses_its_recorded_bit() {
    assert!(!backend_role_from_topology(None, "laptop", Some(false)));
}

#[test]
fn a_listless_daemon_box_uses_its_recorded_bit() {
    assert!(backend_role_from_topology(None, "host-2", Some(true)));
}

// The declared topology is canonical and can change without a
// reinstall — a stale recorded bit from install time must never win
// over what the list says NOW.
#[test]
fn the_declared_topology_wins_over_a_stale_recorded_bit() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.host-2]\n\
             daemon = true\n",
    );
    // install.json still says "daemon: false" from an install run
    // before this host was added to the list as a daemon.
    assert!(backend_role_from_topology(Some(&t), "host-2", Some(false)));
}

// The update path reads this host's own entry in the declared topology:
// the name comes from `SOT_SELF_HOST` here, and only that entry's
// `daemon` flag decides (the recorded bit is `true` and must not win).
#[test]
fn backend_role_wanted_reads_this_hosts_declared_entry() {
    let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = (std::env::var_os("SOT_HOSTS"), std::env::var_os("SOT_SELF_HOST"));
    let dir = tempfile::tempdir().unwrap();
    let hosts = dir.path().join("hosts.toml");
    std::fs::write(
        &hosts,
        "hub = \"mw21-hub\"\n[host.mw21-hub]\ndaemon = true\n[host.mw21-laptop]\nfrontend = true\n[host.mw21-guest]\n",
    )
    .unwrap();
    let install: InstallManifest =
        serde_json::from_value(json!({"schema": 1, "prefix": "/nowhere", "daemon": true})).unwrap();
    std::env::set_var("SOT_HOSTS", &hosts);
    std::env::set_var("SOT_SELF_HOST", "mw21-laptop");
    let laptop = backend_role_wanted(&install);
    std::env::set_var("SOT_SELF_HOST", "mw21-guest");
    let guest = backend_role_wanted(&install);
    std::env::set_var("SOT_SELF_HOST", "mw21-hub");
    let hub = backend_role_wanted(&install);
    for (key, value) in [("SOT_HOSTS", saved.0), ("SOT_SELF_HOST", saved.1)] {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert!(laptop, "a frontend-only entry runs a daemon of its own, so its prepare is a full one");
    assert!(!guest, "an entry with neither key wants no backend role");
    assert!(hub, "a daemon entry wants the backend role");
}

// The shape that must be stopped and the shapes that must not: through the real declared-host read, a host the
// topology names with neither `daemon` nor `frontend` runs no sotd; a frontend host runs its own, and a host the
// topology does not name is not said to be a guest.
#[test]
fn only_a_host_named_with_neither_key_runs_no_sotd() {
    let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = (std::env::var_os("SOT_HOSTS"), std::env::var_os("SOT_SELF_HOST"));
    let dir = tempfile::tempdir().unwrap();
    let hosts = dir.path().join("hosts.toml");
    std::fs::write(
        &hosts,
        "hub = \"hubbox\"\n[host.hubbox]\ndaemon = true\n[host.guest]\n[host.laptop]\nfrontend = true\n",
    )
    .unwrap();
    std::env::set_var("SOT_HOSTS", &hosts);
    let answers = ["guest", "laptop", "hubbox", "unlisted"].map(|me| {
        std::env::set_var("SOT_SELF_HOST", me);
        host_runs_no_sotd()
    });
    for (key, value) in [("SOT_HOSTS", saved.0), ("SOT_SELF_HOST", saved.1)] {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert_eq!(answers, [true, false, false, false]);
}

/// An owner of the update's state for a test, with nothing armed wherever it looks.
fn a_status() -> Status {
    Status::new(Chase::Unchecked, false, || None)
}

/// The next state a reader was told, within 30 s.
async fn next_told(told: &mut tokio::sync::mpsc::UnboundedReceiver<sot_protocol::UpdateState>) -> sot_protocol::UpdateState {
    tokio::time::timeout(Duration::from_secs(30), told.recv()).await.expect("a state within 30 s").expect("the reader is alive")
}

/// A pointer for `version` armed at `<updates>/pending-<target>.json`, for this platform's target, and the target. With
/// no install record beside the folder there is nothing to compare it with, so it fits.
fn arm_on_disk(updates: &std::path::Path, version: &str, over: Option<(&str, &str)>) -> String {
    let target = sot_updater::platform::this_platform().unwrap().to_string();
    let identity = ReleaseIdentity {
        target: target.clone(),
        asset: sot_updater::platform::platform_asset(version).unwrap(),
        ..fake_identity(&format!("v{version}"))
    };
    let state = PreparedState { schema: 1, identity: identity.clone(), checkout: "/x".into(), commit: "cc".into(), julia_instantiated: true, mathjax_deps: false, prepared_at: 1 };
    let stage = sot_updater::stage_dir(updates, &identity);
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("prepared.json"), serde_json::to_string(&state).unwrap()).unwrap();
    let pointer = PendingPointer { identity, ..pointer("v0.0.0", over) };
    std::fs::write(sot_updater::pending::pointer_path(updates, &target), serde_json::to_string(&pointer).unwrap()).unwrap();
    target
}

/// An owner that looks for its armed pointer in `updates`, which it has not read yet.
fn a_status_over(updates: &std::path::Path, target: &str) -> Status {
    let (updates, target) = (updates.to_path_buf(), target.to_string());
    Status::new(Chase::Unchecked, false, move || Some(Disk { updates: updates.clone(), target: target.clone(), unit: false }))
}

/// A stage of `version` that failed with `cause`, as the pipeline writes it.
fn stage_failed(version: &str, cause: &str) -> Chase {
    Chase::NotApplied { version: version.into(), cause: PipelineError::Stage(cause.into()).to_string(), block: Some(cause.into()) }
}

/// A release folder for this platform that stages, and the config and identity that stage it.
#[cfg(unix)]
async fn staged_release(dir: &std::path::Path) -> (UpdaterConfig, ReleaseIdentity) {
    let target = sot_updater::platform::this_platform().unwrap();
    let asset = sot_updater::platform::platform_asset("9.9.9").unwrap();
    let top = dir.join("build").join(format!("sot-9.9.9-{target}"));
    std::fs::create_dir_all(&top).unwrap();
    for f in ["sot", "sotd", "sot-capsule"] {
        std::fs::write(top.join(f), f).unwrap();
    }
    let release = dir.join("release");
    std::fs::create_dir_all(&release).unwrap();
    sot_platform::test_wait::run_within(
        std::process::Command::new("tar")
            .arg("-czf")
            .arg(release.join(&asset))
            .arg("-C")
            .arg(dir.join("build"))
            .arg(top.file_name().unwrap()),
        Duration::from_secs(30),
    )
    .ok();
    let digest = sot_updater::fetch::hash::sha256_file(&release.join(&asset)).await.unwrap();
    std::fs::write(release.join("SHA256SUMS"), format!("{digest}  {asset}\n")).unwrap();
    let sums = std::fs::read_to_string(release.join("SHA256SUMS")).unwrap();
    let id = sot_updater::fetch::sums::discover(&sot_updater::fetch::sums::parse_sums(&sums).unwrap(), "example/project", target).unwrap();
    let cfg = UpdaterConfig {
        repo: "example/project".into(),
        current_version: "0.1.0".into(),
        fetcher: Fetcher::Dir(release),
        updates_root: dir.join("updates"),
    };
    (cfg, id)
}

/// The block a failed commit recorded stops describing reality the moment the next attempt begins: the prepare that
/// follows can take an hour, and `update.check` must not report the old reason meanwhile.
#[cfg(unix)]
#[tokio::test]
async fn a_successful_stage_clears_the_recorded_block_before_prepare() {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, id) = staged_release(dir.path()).await;
    let status = a_status();
    status.set_chase(stage_failed(&id.version, "Access is denied. (os error 5)"));
    assert_eq!(overlay_stage_block(&status, ok_outcome(id.clone())).status, "update blocked: Access is denied. (os error 5)");
    stage_prepare_arm_inner(&status, &cfg, &id).await;
    assert!(sot_updater::is_staged(&cfg.updates_root, &id).await, "the stage landed");
    assert_eq!(overlay_stage_block(&status, ok_outcome(id)).status, "ok");
}

/// Each step of the daemon's pipeline is the state a reader is told, in order, on a release folder that stages and has
/// no install to prepare for: found by the check, preparing while it stages, then found with the reason nothing more
/// was done. A stage that fails is not applied with the step's words, marks only its own release for `update.check`,
/// and the next attempt clears it. A run turned away because another is under way writes nothing.
#[cfg(unix)]
#[tokio::test]
async fn each_step_of_the_pipeline_is_the_state_a_reader_sees() {
    use sot_protocol::UpdatePhase as P;
    let dir = tempfile::tempdir().unwrap();
    let (cfg, id) = staged_release(dir.path()).await;
    let status = a_status();
    let mut told = status.told_at_each_change();
    let brief = |s: sot_protocol::UpdateState| (s.phase, s.version, s.detail);

    status.checked(chase_of_check(&ok_outcome(id.clone())));
    assert_eq!(brief(next_told(&mut told).await), (P::Found, "9.9.9".to_string(), String::new()));
    stage_prepare_arm_inner(&status, &cfg, &id).await;
    assert_eq!(brief(next_told(&mut told).await), (P::Preparing, "9.9.9".to_string(), String::new()));
    assert_eq!(brief(next_told(&mut told).await), (P::Found, "9.9.9".to_string(), "no install record here to prepare for".to_string()));
    assert_eq!(status.state().line(), "9.9.9 found: no install record here to prepare for");

    // The same release offered from a folder that holds no asset, into a fresh updates folder: the stage fails.
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let broken = UpdaterConfig { fetcher: Fetcher::Dir(empty), updates_root: dir.path().join("updates-2"), ..cfg.clone() };
    stage_prepare_arm_inner(&status, &broken, &id).await;
    assert_eq!(next_told(&mut told).await.phase, P::Preparing);
    let failed = next_told(&mut told).await;
    let block = status.stage_block("9.9.9").expect("a failed stage is the block of its own release");
    assert_eq!((failed.phase, failed.version.as_str(), failed.detail.as_str()), (P::NotApplied, "9.9.9", format!("staging update failed: {block}").as_str()));
    assert_eq!(status.stage_block("9.9.10"), None, "a failed stage of one release never marks another");
    assert_eq!(overlay_stage_block(&status, ok_outcome(fake_identity("v9.9.10"))).status, "ok");

    // A run turned away because one is under way writes nothing.
    PIPELINE_RUNNING.store(true, std::sync::atomic::Ordering::Release);
    stage_prepare_arm(&status, &cfg, &id).await;
    PIPELINE_RUNNING.store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(status.state().phase, P::NotApplied, "the run that was turned away wrote nothing");

    stage_prepare_arm_inner(&status, &cfg, &id).await;
    assert_eq!(next_told(&mut told).await.phase, P::Preparing);
    assert_eq!(next_told(&mut told).await.phase, P::Found);
    assert_eq!(status.stage_block("9.9.9"), None, "the next attempt clears the block");
}

/// What a run of the pipeline leaves of the chase claims no more than the run did: an arm leaves the disk to say ready
/// and never says up to date, a release the arm refused or kept a newer one over is not applied with the arm's own
/// sentence, a stage with nothing to prepare for is found with the reason, and a step that failed is not applied with
/// the step's words, the stage's cause alone being the block.
#[test]
fn the_chase_a_run_leaves_claims_no_more_than_the_run_did() {
    let id = fake_identity("v9.9.9");
    let not_applied = |cause: &str, block: Option<&str>| Chase::NotApplied { version: "9.9.9".into(), cause: cause.into(), block: block.map(String::from) };
    let unreadable = UnreadableInstall { path: "/p/install.json".into(), cause: "EOF while parsing".into(), missing: false };
    let table: Vec<(Result<Pipeline, PipelineError>, Chase)> = vec![
        (Ok(Pipeline::Armed), Chase::Armed { version: "9.9.9".into() }),
        (Ok(Pipeline::Staged), Chase::Found { version: "9.9.9".into(), detail: "no install record here to prepare for".into() }),
        (Ok(Pipeline::NotArmed(ArmOutcome::MarkedBad)), not_applied("this release crash-looped after an apply and is marked bad", None)),
        (Ok(Pipeline::NotArmed(ArmOutcome::NotAboveInstall)), not_applied("the release is not above the installed build", None)),
        (Ok(Pipeline::NotArmed(ArmOutcome::KeptNewer)), not_applied("a newer release is already armed", None)),
        (Ok(Pipeline::NotArmed(ArmOutcome::Pinned)), not_applied("the install is pinned", None)),
        (Ok(Pipeline::NotArmed(ArmOutcome::DevInstall)), not_applied("the install is a dev build", None)),
        (Ok(Pipeline::NotArmed(ArmOutcome::UnreadableInstall(unreadable.clone()))), not_applied(&unreadable.sentence(), None)),
        (Err(PipelineError::Stage("Access is denied. (os error 5)".into())), not_applied("staging update failed: Access is denied. (os error 5)", Some("Access is denied. (os error 5)"))),
        (Err(PipelineError::Prepare("no julia".into())), not_applied("preparing update failed: no julia", None)),
        (Err(PipelineError::Arm("the lock is held".into())), not_applied("arming update failed: the lock is held", None)),
    ];
    for (result, chase) in table {
        assert_eq!(chase_at_end(&id, &result), chase, "{result:?}");
    }
}

/// The disk is read again when a run of the pipeline ends and when a daily check begins, with nobody asking: an owner
/// that has never read its folder says ready after each, for a pointer that was armed there meanwhile.
#[cfg(unix)]
#[test]
fn a_runs_end_and_a_checks_start_read_the_disk_again() {
    use sot_protocol::UpdatePhase as P;
    let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = crate::paths::EnvGuard::capture("SOT_UPDATE_ROOT");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let armed = dir.path().join("armed").join("updates");
    std::fs::create_dir_all(&armed).unwrap();
    let target = arm_on_disk(&armed, "9.9.9", None);

    // A run's end: the release folder stages and has no install to prepare for, so the chase ends at found; the disk
    // holds a pointer for that release, which only the reading at the run's end can have told.
    let after_a_run = a_status_over(&armed, &target);
    assert_eq!(after_a_run.state().phase, P::Unchecked, "nothing read yet");
    let (cfg, id) = rt.block_on(staged_release(dir.path()));
    rt.block_on(stage_prepare_arm_inner(&after_a_run, &cfg, &id));
    assert_eq!((after_a_run.state().phase, after_a_run.state().version.as_str()), (P::Ready, "9.9.9"), "{:?}", after_a_run.state());

    // A check's start: the check finds a pinned install and reaches no release source.
    let pinned = dir.path().join("pinned");
    std::fs::create_dir_all(pinned.join("updates")).unwrap();
    std::fs::write(pinned.join("install.json"), r#"{"schema":1,"prefix":"/p","version":"0.7.3","pinned":true}"#).unwrap();
    std::env::set_var("SOT_UPDATE_ROOT", pinned.join("updates"));
    let after_a_check = a_status_over(&armed, &target);
    let (leases, (tx, _rx)) = (Leases::new(Some("boot".into()), None, None, false), broadcast::channel(4));
    let updater = Updater { current: "0.7.3".into(), dev: false, mode: Mode::Notify, repo: "o/r".into() };
    rt.block_on(run_check_once(&updater, &after_a_check, &tx, &|| 0, &leases));
    let state = after_a_check.state();
    assert_eq!((state.phase, state.version.as_str(), state.checked_s.is_some()), (P::Ready, "9.9.9", true), "{state:?}");
}

/// The probe of `the_daemons_own_install_decides_what_its_state_reads` runs in a copy of this binary placed under a
/// prefix (a no-op without its variable): `boot` makes the owner a daemon starts with and waits for the state its own
/// first reading tells; `guest` runs the pipeline for a release above the install. The state's line is printed between
/// markers.
#[test]
fn probe_the_update_state_from_a_prefix() {
    let Some(mode) = std::env::var_os("SOT_TEST_PROBE_UPDATE_STATE") else { return };
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let said = rt.block_on(async {
        if mode == "guest" {
            let (status, id) = (a_status(), fake_identity("v9.9.9"));
            let cfg = UpdaterConfig { repo: id.repo.clone(), current_version: "9.9.8".into(), fetcher: Fetcher::Dir("/nowhere".into()), updates_root: "/nowhere".into() };
            stage_prepare_arm_inner(&status, &cfg, &id).await;
            return status.state().line();
        }
        let status = boot_status();
        let began = std::time::Instant::now();
        while status.state().phase != sot_protocol::UpdatePhase::Ready && began.elapsed() < Duration::from_secs(10) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        status.state().line()
    });
    println!("PROBE[{said}]");
}

/// The owner a daemon starts with reads the install its binary runs from: the pointer armed under that prefix, for this
/// platform, told before anything asks (its first reading is its own), with what applies it taken from the install
/// record's service. And the pipeline of a daemon whose host the topology gives neither `daemon` nor `frontend` says
/// off, where the same release on a host that runs a daemon is not applied with the refusal's words.
#[test]
fn the_daemons_own_install_decides_what_its_state_reads() {
    let scratch = tempfile::tempdir().unwrap();
    let prefix = scratch.path().join("opt");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::create_dir_all(prefix.join("updates")).unwrap();
    let exe = prefix.join("bin").join(std::env::current_exe().unwrap().file_name().unwrap());
    sot_platform::test_exec::write_executable(&exe, std::fs::read(std::env::current_exe().unwrap()).unwrap());
    let hosts = scratch.path().join("hosts.toml");
    std::fs::write(&hosts, "hub = \"us-hub\"\n[host.us-hub]\ndaemon = true\n[host.us-guest]\n").unwrap();
    let probe = |mode: &str, me: &str, record: &str| {
        std::fs::write(prefix.join("install.json"), record).unwrap();
        let ran = sot_platform::test_wait::run_within(
            std::process::Command::new(&exe)
                .args(["--exact", "update::tests::probe_the_update_state_from_a_prefix", "--nocapture", "--test-threads=1"])
                .env("SOT_TEST_PROBE_UPDATE_STATE", mode)
                .env("SOT_HOSTS", &hosts)
                .env("SOT_SELF_HOST", me)
                .env_remove("SOT_UPDATE_ROOT"),
            Duration::from_secs(60),
        );
        let text = ran.stdout;
        text.split_once("PROBE[").and_then(|(_, rest)| rest.lines().next()?.strip_suffix(']')).map(str::to_string).unwrap_or(text)
    };
    let record = |version: &str, service: &str| format!(r#"{{"schema":1,"prefix":"/p","version":"{version}","commit":"c0","daemon":true,"service":"{service}"}}"#);

    arm_on_disk(&prefix.join("updates"), "9.9.9", Some(("9.9.8", "c0")));
    assert_eq!(
        probe("boot", "us-hub", &record("9.9.8", "systemd")),
        "9.9.9 ready: applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)"
    );
    assert_eq!(probe("boot", "us-hub", &record("9.9.8", "none")), "9.9.9 ready: applies when Ship of Tools is next started on that computer");

    assert_eq!(probe("guest", "us-guest", &record("9.9.8", "none")), "off: hosts.toml gives this host neither daemon nor frontend, so it takes no update of its own");
    assert_eq!(probe("guest", "us-hub", &record("9.9.9", "none")), "9.9.9 not applied: v9.9.9 is not above the installed 9.9.9");
}

/// A check that is off or blocked says why in the state. A dev build and update mode off run no check and no task, so
/// the state says so from the start; a pinned install and an install record that cannot be read are found by the
/// check, which reaches no release source for either.
#[test]
fn a_check_that_is_off_or_blocked_says_why_in_the_state() {
    use sot_protocol::UpdatePhase as P;
    let updater = |dev, mode| Updater { current: "0.7.3".into(), dev, mode, repo: "o/r".into() };
    assert_eq!(updater(true, Mode::Notify).start_chase(), Chase::Off { why: "dev build".into() });
    assert_eq!(updater(false, Mode::Off).start_chase(), Chase::Off { why: "update mode off".into() });
    assert_eq!(updater(false, Mode::Auto).start_chase(), Chase::Unchecked);
    let start = Status::new(updater(true, Mode::Notify).start_chase(), false, || None).state();
    assert_eq!((start.phase, start.line().as_str(), start.checked_s), (P::Off, "off: dev build", None), "no check ran");

    let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = crate::paths::EnvGuard::capture("SOT_UPDATE_ROOT");
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("updates")).unwrap();
    std::env::set_var("SOT_UPDATE_ROOT", dir.path().join("updates"));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let status = a_status();
    let leases = Leases::new(Some("boot".into()), None, None, false);
    let (tx, _rx) = broadcast::channel(4);
    let check = |record: &str| {
        std::fs::write(dir.path().join("install.json"), record).unwrap();
        rt.block_on(run_check_once(&updater(false, Mode::Notify), &status, &tx, &|| 0, &leases));
        status.state()
    };

    let pinned = check(r#"{"schema":1,"prefix":"/p","version":"0.7.3","pinned":true}"#);
    assert_eq!(pinned.phase, P::Off);
    assert_eq!(pinned.line(), "off: pinned at 0.7.3: automatic updates stay off for this install until a run that names no version");
    assert_eq!(pinned.mark(), None, "the person's own choice has no mark");

    let cut = check(r#"{"schema":1,"prefix":"/p","version":"0.7.3","pinned":tr"#);
    assert_eq!(cut.phase, P::Blocked);
    assert!(cut.detail.starts_with(UNREADABLE_LEAD), "{}", cut.detail);
    assert!(cut.detail.contains(if cfg!(windows) { "install.ps1 -Version" } else { "install.sh --version" }), "names the repair run: {}", cut.detail);
    assert_eq!(cut.mark().map(|mark| mark.long).as_deref(), Some("updates off: install record unreadable"));
    assert!(cut.checked_s.is_some(), "the check that found it ran");

    // Both of the record's sentences still name the repair run after they cross the wire, on a path long enough that
    // the sentence passes the wire's bound: the bound keeps a sentence's end.
    let run = if cfg!(windows) { "`install.ps1 -Version <version>`" } else { "`install.sh --version <version>`" };
    for missing in [false, true] {
        let path = format!("/home/{}/.local/share/sot/install.json", "a".repeat(120));
        let why = UnreadableInstall { path: path.into(), cause: "EOF while parsing a value at line 1 column 57".into(), missing }.sentence();
        assert!(why.len() > sot_protocol::ops::MAX_UPDATE_DETAIL, "the sentence is past the bound: {}", why.len());
        let told = serde_json::to_value(Status::new(Chase::Blocked { why }, false, || None).state()).unwrap();
        let read: sot_protocol::UpdateState = serde_json::from_value(told).unwrap();
        assert!(read.line().contains(run) && read.line().starts_with("off: install.json "), "missing {missing}: {}", read.line());
    }
}

/// Each step says so on the log, which `sotd update stage` sends to stderr: the stage and, when there is an install to
/// prepare for, the prepare and the arm.
#[cfg(unix)]
#[tokio::test]
async fn the_pipeline_logs_a_line_for_each_step() {
    #[derive(Clone, Default)]
    struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (cfg, id) = staged_release(dir.path()).await;
    let buf = Buf::default();
    let sink = buf.clone();
    let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(move || sink.clone()).finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    stage_prepare_arm_with(&cfg, &id, None, Writer::Automatic).await.unwrap();
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("staging"), "{log}");
}

fn installed(version: &str, commit: &str) -> InstalledBuild {
    InstalledBuild { version: version.into(), commit: Some(commit.into()), pinned: false }
}

fn pointer(tag: &str, over: Option<(&str, &str)>) -> PendingPointer {
    PendingPointer {
        schema: 1,
        identity: fake_identity(tag),
        checkout: "/x".into(),
        commit: "cc".into(),
        armed_at: 0,
        armed_over_version: over.map(|o| o.0.into()),
        armed_over_commit: over.map(|o| o.1.into()),
    }
}

// The shared-home red: a daemon that booted on rc9.12 offers rc9.13 into an install that is already on rc9.14.
#[test]
fn a_daemon_arms_only_a_declared_host_and_only_above_the_install() {
    let id = fake_identity("v0.6.6-rc9.13");
    let at = |v: &str| installed(v, "c0");
    assert!(arm_refusal(false, Some(&at("0.6.6-rc9.12")), &id).is_none());
    assert!(arm_refusal(false, None, &id).is_none(), "no install manifest: nothing to compare");
    assert!(arm_refusal(false, Some(&at("0.6.6-rc9.13")), &id).unwrap().contains("not above"));
    assert!(arm_refusal(false, Some(&at("0.6.6-rc9.14-dev+d94ed0fac")), &id).unwrap().contains("dev build"));
    // Not a release above a dev stamp either.
    let above = fake_identity("v0.6.6-rc9.14");
    assert!(arm_refusal(false, Some(&at("0.6.6-rc9.14-dev+d94ed0fac")), &above).unwrap().contains("dev build"));
    assert!(arm_refusal(true, Some(&at("0.6.6-rc9.12")), &id).unwrap().contains("neither daemon nor frontend"));
}

// An operator's downgrade is not undone by the next daily check: the run that names a version pins the install.
#[test]
fn a_pinned_install_is_never_armed_over() {
    let id = fake_identity("v0.6.6-rc9.14");
    let pinned = InstalledBuild { pinned: true, ..installed("0.6.6-rc9.12", "c0") };
    assert!(arm_refusal(false, Some(&installed("0.6.6-rc9.12", "c0")), &id).is_none());
    assert_eq!(
        arm_refusal(false, Some(&pinned), &id).unwrap(),
        "pinned at 0.6.6-rc9.12: automatic updates stay off for this install until a run that names no version"
    );
    assert!(!pinned.admits("0.6.6-rc9.14"));
}

#[test]
fn update_apply_refuses_a_foreign_host_and_a_pointer_that_no_longer_fits() {
    let fits = pointer("v0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")));
    let now = installed("0.6.6-rc9.13", "c0");
    assert!(apply_refusal(false, Ok(Some(now.clone())), &fits).is_none());
    assert!(apply_refusal(false, Ok(None), &fits).is_none(), "no install manifest: nothing to compare");
    assert!(apply_refusal(true, Ok(Some(now)), &fits).unwrap().contains("neither daemon nor frontend"));
    let moved = installed("0.6.6-rc9.14-dev+d94ed0fac", "c1");
    assert!(apply_refusal(false, Ok(Some(moved.clone())), &fits).unwrap().contains("will not apply"));
    // A pointer armed before the record existed: judged by its version alone.
    let old = pointer("v0.6.6-rc9.13", None);
    assert!(apply_refusal(false, Ok(Some(moved)), &old).unwrap().contains("dev build"));
}

#[test]
fn update_apply_refuses_a_pointer_armed_over_an_install_it_cannot_read() {
    let cut = UnreadableInstall { path: "/p/install.json".into(), cause: "EOF while parsing".into(), missing: false };
    let armed = pointer("v0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")));
    let why = apply_refusal(false, Err(cut.clone()), &armed).unwrap();
    assert!(why.contains("will not apply") && why.contains(&cut.sentence()), "{why}");
    assert!(apply_refusal(false, Err(cut), &pointer("v0.6.6-rc9.14", None)).is_none(), "no record: the apply's tag check judges it");
}

/// The probe `a_check_at_a_non_default_prefix...` runs in a copy of this binary placed under a prefix: what
/// `check_basis` says there, printed between markers (the test is a no-op without its marker variable).
#[test]
fn probe_check_basis_from_a_prefix() {
    if std::env::var_os("SOT_TEST_PROBE_CHECK_BASIS").is_none() {
        return;
    }
    let process = Updater { current: "0.6.6-rc9.12".into(), dev: false, mode: Mode::Notify, repo: "o/r".into() };
    println!("PROBE[{:?}]", process.check_basis());
}

// The review's measurement: the exe's location decides the root whenever install.json is there, parsed or not.
// At a prefix that is not the default one, with a cut record, the check must not fall back to the boot version or
// read another install's (SOT_UPDATE_ROOT here stands for the other install a fallback would reach).
#[test]
fn a_check_at_a_non_default_prefix_over_a_cut_record_says_so() {
    let scratch = tempfile::tempdir().unwrap();
    let (prefix, other) = (scratch.path().join("opt"), scratch.path().join("other"));
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::create_dir_all(other.join("updates")).unwrap();
    std::fs::write(other.join("install.json"), r#"{"schema":1,"prefix":"/o","version":"0.6.5"}"#).unwrap();
    let exe = prefix.join("bin").join(std::env::current_exe().unwrap().file_name().unwrap());
    sot_platform::test_exec::write_executable(&exe, std::fs::read(std::env::current_exe().unwrap()).unwrap());
    let probe = |record: Option<&str>| {
        match record {
            Some(record) => std::fs::write(prefix.join("install.json"), record).unwrap(),
            None => drop(std::fs::remove_file(prefix.join("install.json"))),
        }
        let ran = sot_platform::test_wait::run_within(
            std::process::Command::new(&exe)
                .args(["--exact", "update::tests::probe_check_basis_from_a_prefix", "--nocapture", "--test-threads=1"])
                .env("SOT_TEST_PROBE_CHECK_BASIS", "1")
                .env("SOT_UPDATE_ROOT", other.join("updates")),
            Duration::from_secs(60),
        );
        let text = ran.stdout;
        text.split_once("PROBE[").and_then(|(_, rest)| rest.lines().next()?.strip_suffix(']')).map(str::to_string).unwrap_or(text)
    };
    let cut = probe(Some(r#"{"schema":1,"prefix":"/p","version":"0.6.7","pinned":tr"#));
    assert!(cut.starts_with("Err(\"disabled: install.json cannot be read at "), "{cut}");
    assert!(cut.contains(&prefix.join("install.json").display().to_string().replace('\\', "\\\\")), "names this install's file: {cut}");
    let run = if cfg!(windows) { "install.ps1 -Version" } else { "install.sh --version" };
    assert!(cut.contains(run), "names the remedy: {cut}");
    let whole = probe(Some(r#"{"schema":1,"prefix":"/p","version":"0.6.7"}"#));
    assert_eq!(whole, r#"Ok("0.6.7")"#, "the exe's own record decides, not the other install's");
    // Missing: on Windows, beside bin\sotd.exe, it is refused like a cut record; elsewhere absent is no install and
    // the check falls to the other root, as before.
    std::fs::write(prefix.join("bin").join("sotd.exe"), b"").unwrap();
    let missing = probe(None);
    if cfg!(windows) {
        assert!(missing.starts_with("Err(\"disabled: install.json is missing at "), "{missing}");
        assert!(missing.contains("install.ps1 -Version") && !missing.contains("repair"), "{missing}");
    } else {
        assert_eq!(missing, r#"Ok("0.6.5")"#, "Unix: absent is no install");
    }
}

// The channel comes from the install on disk, not from the version the process booted with.
#[test]
fn a_check_follows_the_install_not_the_process() {
    let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("SOT_UPDATE_ROOT");
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("updates")).unwrap();
    std::env::set_var("SOT_UPDATE_ROOT", dir.path().join("updates"));
    let installed_at = |version: &str| {
        std::fs::write(dir.path().join("install.json"), format!(r#"{{"schema":1,"prefix":"/p","version":"{version}"}}"#)).unwrap();
    };
    let process_on_an_rc = Updater { current: "0.6.6-rc9.12".into(), dev: false, mode: Mode::Notify, repo: "o/r".into() };
    let none = process_on_an_rc.check_basis();
    installed_at("0.6.7");
    let stable = process_on_an_rc.check_basis();
    installed_at("0.6.7-dev+abc");
    let dev = process_on_an_rc.check_basis();
    std::fs::write(dir.path().join("install.json"), r#"{"schema":1,"prefix":"/p","version":"0.6.6-rc9.12","pinned":true}"#).unwrap();
    let pinned = process_on_an_rc.check_basis();
    match saved {
        Some(v) => std::env::set_var("SOT_UPDATE_ROOT", v),
        None => std::env::remove_var("SOT_UPDATE_ROOT"),
    }
    assert_eq!(none, Ok("0.6.6-rc9.12".to_string()), "no install: the process's version");
    assert_eq!(stable, Ok("0.6.7".to_string()), "the install's channel, not the process's rc");
    assert!(dev.is_err(), "a dev install is no basis for a release check");
    assert_eq!(
        pinned,
        Err("disabled: pinned at 0.6.6-rc9.12: automatic updates stay off for this install until a run that names no version".to_string()),
        "a pinned install is no basis either, and the verdict says why"
    );
}

// An install.json that cannot be read may hold a pin, so the check does not fall back to the boot version.
#[test]
fn a_check_over_an_unreadable_install_says_so() {
    let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("SOT_UPDATE_ROOT");
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("updates")).unwrap();
    std::env::set_var("SOT_UPDATE_ROOT", dir.path().join("updates"));
    std::fs::write(dir.path().join("install.json"), r#"{"schema":1,"prefix":"/p","version":"0.6.7","pinned":tr"#).unwrap();
    let process = Updater { current: "0.6.6-rc9.12".into(), dev: false, mode: Mode::Notify, repo: "o/r".into() };
    let cut = process.check_basis();
    std::fs::write(dir.path().join("install.json"), "\u{feff}{\"schema\":1,\"prefix\":\"/p\",\"version\":\"0.6.7\"}").unwrap();
    let bom = process.check_basis();
    match saved {
        Some(v) => std::env::set_var("SOT_UPDATE_ROOT", v),
        None => std::env::remove_var("SOT_UPDATE_ROOT"),
    }
    let said = cut.unwrap_err();
    assert!(said.starts_with("disabled: install.json cannot be read"), "{said}");
    assert!(said.contains(if cfg!(windows) { "install.ps1 -Version" } else { "install.sh --version" }), "the status names the remedy: {said}");
    assert_eq!(bom, Ok("0.6.7".to_string()), "a BOM alone is not unreadable");
}

#[test]
fn a_failure_names_every_context_down_to_its_cause() {
    let e = anyhow::anyhow!("os error 2").context("fetching tags into base clone");
    assert_eq!(chain(&e), "fetching tags into base clone: os error 2");
}

fn fake_identity(tag: &str) -> ReleaseIdentity {
    ReleaseIdentity {
        repo: "kalidke/ship-of-tools".into(),
        tag: tag.into(),
        version: tag.trim_start_matches('v').into(),
        target: "linux-x86_64".into(),
        asset: format!("sot-{}-linux-x86_64.tar.gz", tag.trim_start_matches('v')),
        asset_sha256: "0".repeat(64),
    }
}

fn ok_outcome(id: ReleaseIdentity) -> CheckOutcome {
    CheckOutcome {
        latest: id.version.clone(),
        identity: Some(id),
        update_available: true,
        status: "ok".into(),
    }
}

// Defect 0c: a check that itself succeeded ("ok") must still surface a
// release stuck failing to commit — that silence is exactly how one box
// hid never having updated. Pins the tag-scoping too: a DIFFERENT
// release's check is not tainted by an older tag's recorded block.
#[test]
fn a_recorded_stage_block_overlays_status_for_its_own_tag_only() {
    let status = a_status();
    status.set_chase(stage_failed("9.9.9", "Access is denied. (os error 5)"));

    let blocked = overlay_stage_block(&status, ok_outcome(fake_identity("v9.9.9")));
    assert_eq!(blocked.status, "update blocked: Access is denied. (os error 5)");

    let unrelated = overlay_stage_block(&status, ok_outcome(fake_identity("v9.9.10")));
    assert_eq!(unrelated.status, "ok");

    // Only a failed stage is a block: a prepare that failed is told by the state alone.
    status.set_chase(Chase::NotApplied { version: "9.9.9".into(), cause: "preparing update failed: no julia".into(), block: None });
    let after_clear = overlay_stage_block(&status, ok_outcome(fake_identity("v9.9.9")));
    assert_eq!(after_clear.status, "ok");
}
