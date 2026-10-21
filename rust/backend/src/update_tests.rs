use super::*;
use sot_updater::pending::PendingPointer;
use sot_updater::UnreadableInstall;
use sot_updater::identity::DEFAULT_REPO;

/// Every updater command is a contained child of the signal: its two streams and its status come back whole, and a
/// command its caller drops or a fire ends the command's whole tree.
#[cfg(unix)]
mod spawner {
    use super::*;
    use crate::lifecycle::child_signal::{tests::Leftover, Signal};
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

fn topo(text: &str) -> sot_protocol::topology::Topology {
    sot_protocol::topology::parse(text).expect("fixture must parse")
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

/// The recorded block is process-wide: tests that record or clear it run one at a time.
static STAGE_BLOCK_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    sot_log::test_wait::run_within(
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

/// The block a failed commit recorded stops describing reality the moment the next attempt stages: the prepare that
/// follows can take an hour, and `update.check` must not report the old reason meanwhile.
#[cfg(unix)]
#[tokio::test]
async fn a_successful_stage_clears_the_recorded_block_before_prepare() {
    let _block = STAGE_BLOCK_TEST.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (cfg, id) = staged_release(dir.path()).await;
    record_stage_block(&id.tag, "Access is denied. (os error 5)");
    let staged = stage_prepare_arm_with(&cfg, &id, None, Writer::Automatic).await;
    assert!(matches!(staged, Ok(Pipeline::Staged)), "{staged:?}");
    assert_eq!(overlay_stage_block(ok_outcome(id)).status, "ok");
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
    let _block = STAGE_BLOCK_TEST.lock().await;
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
    sot_log::test_exec::write_executable(&exe, std::fs::read(std::env::current_exe().unwrap()).unwrap());
    let probe = |record: Option<&str>| {
        match record {
            Some(record) => std::fs::write(prefix.join("install.json"), record).unwrap(),
            None => drop(std::fs::remove_file(prefix.join("install.json"))),
        }
        let ran = sot_log::test_wait::run_within(
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
    let _block = STAGE_BLOCK_TEST.blocking_lock();
    clear_stage_block(); // isolate from any other test's leftover state
    record_stage_block("v9.9.9", "Access is denied. (os error 5)");

    let blocked = overlay_stage_block(ok_outcome(fake_identity("v9.9.9")));
    assert_eq!(blocked.status, "update blocked: Access is denied. (os error 5)");

    let unrelated = overlay_stage_block(ok_outcome(fake_identity("v9.9.10")));
    assert_eq!(unrelated.status, "ok");

    clear_stage_block();
    let after_clear = overlay_stage_block(ok_outcome(fake_identity("v9.9.9")));
    assert_eq!(after_clear.status, "ok");
}
