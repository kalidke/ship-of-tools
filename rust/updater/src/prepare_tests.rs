use super::*;
use crate::spawn::tests::{output, RecordingSpawner};
use std::sync::Mutex;

const KERNEL_PROJECT: &str = "name = \"ShipToolsKernel\"\n";
const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Fixture {
    root: PathBuf,
    spec: PrepareSpec,
    mode: &'static str,
    dirty: Mutex<bool>,
    /// No `refs/tags/*` resolves, as for a build of the line between tags.
    tagless: bool,
    /// No non-tag revision resolves: a commit origin does not have.
    commit_missing: bool,
    commands: Mutex<Vec<Vec<String>>>,
}

impl Fixture {
    fn new(mode: &'static str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("sot-updater-prepare-{}", crate::unique::suffix()));
        let spec = PrepareSpec {
            identity: ReleaseIdentity {
                repo: "example/project".into(),
                tag: "v9.9.9".into(),
                version: "9.9.9".into(),
                target: "linux-x86_64".into(),
                asset: "sot-9.9.9-linux-x86_64.tar.gz".into(),
                asset_sha256: "ab".repeat(32),
            },
            repo_dir: root.join("repo"),
            stage_dir: root.join("updates/v9.9.9"),
            origin_url: Some("https://example.invalid/project".into()),
            julia_bin: (mode != "none").then(|| "julia".into()),
            npm: true,
        };
        std::fs::create_dir_all(&spec.stage_dir).unwrap();
        Self {
            root,
            spec,
            mode,
            dirty: Mutex::new(false),
            tagless: false,
            commit_missing: false,
            commands: Mutex::new(Vec::new()),
        }
    }

    fn respond(
        &self,
        command: &tokio::process::Command,
    ) -> std::io::Result<std::process::Output> {
        let cmd = command.as_std();
        let bin = cmd.get_program().to_str().unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        self.commands.lock().unwrap().push(
            std::iter::once(bin.to_string())
                .chain(args.clone())
                .collect(),
        );
        match bin {
            "git" => self.git(&args),
            "julia" => {
                let project =
                    PathBuf::from(args[0].strip_prefix("--project=").expect("Julia project"));
                assert_eq!(args[1], "-e");
                let instantiate = args[2].contains("Pkg.instantiate");
                if (self.mode == "instantiate" && instantiate)
                    || (self.mode == "load" && !instantiate)
                {
                    std::fs::write(project.join("Project.toml"), "# rewritten\n")?;
                    *self.dirty.lock().unwrap() = true;
                }
                if self.mode == "untracked" {
                    std::fs::write(project.join("Manifest.toml"), "build product")?;
                }
                if !instantiate {
                    #[allow(clippy::disallowed_methods, reason = "a data copy of a manifest, no program is run from it")]
                    std::fs::copy(project.join("Project.toml"), self.root.join("witness"))?;
                }
                Ok(output(0, Vec::new()))
            }
            "npm" => {
                assert_eq!(args, ["ci", "--silent"]);
                Ok(output(0, Vec::new()))
            }
            _ => panic!("unexpected preparation command {bin}: {args:?}"),
        }
    }

    fn git(&self, args: &[String]) -> std::io::Result<std::process::Output> {
        let (dir, args) = if args[0] == "-C" {
            (Path::new(&args[1]), &args[2..])
        } else {
            (self.root.as_path(), args)
        };
        match args[0].as_str() {
            "clone" => {
                std::fs::create_dir_all(Path::new(args.last().unwrap()).join(".git"))?;
            }
            "fetch" => {
                assert_eq!(args, ["fetch", "--tags", "--force", "origin"]);
            }
            "rev-parse" => {
                let is_tag = args[1].starts_with("refs/tags/");
                let found = dir.exists() && !(self.tagless && is_tag) && !(self.commit_missing && !is_tag);
                return Ok(output(if found { 0 } else { 1 }, if found { COMMIT.as_bytes() } else { b"" }));
            }
            "status" => {
                assert_eq!(args, ["status", "--porcelain", "-uno"]);
                return Ok(output(
                    0,
                    if *self.dirty.lock().unwrap() {
                        b" M Project.toml\n".as_slice()
                    } else {
                        b""
                    },
                ));
            }
            "checkout" => {
                assert_eq!(args, ["checkout", "--", "."]);
                if self.mode != "irreparable" {
                    for env in ["julia/kernel", "julia/repl", "julia/pluto"] {
                        std::fs::write(dir.join(env).join("Project.toml"), KERNEL_PROJECT)?;
                    }
                    *self.dirty.lock().unwrap() = false;
                }
            }
            "worktree" => match args[1].as_str() {
                "add" => {
                    assert_eq!(args[2], "--detach");
                    assert_eq!(args[4], COMMIT);
                    let checkout = Path::new(&args[3]);
                    for env in ["julia/kernel", "julia/repl", "julia/pluto"] {
                        std::fs::create_dir_all(checkout.join(env))?;
                        std::fs::write(
                            checkout.join(env).join("Project.toml"),
                            KERNEL_PROJECT,
                        )?;
                    }
                    std::fs::create_dir_all(checkout.join("rust/backend/sidecars/mathjax"))?;
                    std::fs::write(checkout.join("README.md"), "hello")?;
                }
                "prune" => assert_eq!(args.len(), 2),
                "remove" => {
                    assert_eq!(args[2], "--force");
                    std::fs::remove_dir_all(&args[3])?;
                }
                _ => panic!("unexpected worktree request: {args:?}"),
            },
            _ => panic!("unexpected git request: {args:?}"),
        }
        Ok(output(0, Vec::new()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).expect("fixture cleanup");
    }
}

#[tokio::test]
async fn prepare_creates_versioned_worktree_from_origin() {
    let fixture = Fixture::new("none");
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let state = prepare(&spawner, &fixture.spec).await.unwrap();
    assert_eq!(
        state.checkout,
        fixture.spec.repo_dir.join("versions/v9.9.9")
    );
    assert!(state.checkout.join("README.md").exists());
    assert!(!state.julia_instantiated);
    assert_eq!(state.commit, COMMIT);
    assert!(state.mathjax_deps);
    let again = prepare(&spawner, &fixture.spec).await.unwrap();
    assert_eq!(again.commit, state.commit);
    assert!(
        PreparedState::matches(&spawner, &fixture.spec.stage_dir, &fixture.spec.identity).await
    );
    std::fs::remove_dir_all(&state.checkout).unwrap();
    assert!(
        !PreparedState::matches(&spawner, &fixture.spec.stage_dir, &fixture.spec.identity)
            .await
    );
    let rebuilt = prepare(&spawner, &fixture.spec).await.unwrap();
    assert_eq!(rebuilt.commit, state.commit);
    assert!(rebuilt.checkout.join("README.md").exists());
    let requests = fixture.commands.lock().unwrap();
    for action in ["clone", "fetch", "worktree", "rev-parse", "status"] {
        assert!(
            requests.iter().any(|r| r.iter().any(|a| a == action)),
            "missing {action} request"
        );
    }
}

fn dev_identity(build: &str) -> ReleaseIdentity {
    ReleaseIdentity {
        repo: "example/project".into(),
        tag: format!("v0.6.6-rc9.14-dev+{build}"),
        version: format!("0.6.6-rc9.14-dev+{build}"),
        target: "linux-x86_64".into(),
        asset: format!("sot-0.6.6-rc9.14-dev+{build}-linux-x86_64.tar.gz"),
        asset_sha256: "ab".repeat(32),
    }
}

#[tokio::test]
async fn a_dev_build_prepares_its_commit_without_a_tag() {
    let mut fixture = Fixture::new("none");
    fixture.tagless = true;
    fixture.spec.identity = dev_identity("aaaaaaaaa");
    ReadyManifest::new(dev_identity("aaaaaaaaa"), Some(COMMIT.into())).write(&fixture.spec.stage_dir).await.unwrap();
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let state = prepare(&spawner, &fixture.spec).await.unwrap();
    assert_eq!(state.commit, COMMIT);
    assert_eq!(state.checkout, fixture.spec.repo_dir.join("versions/v0.6.6-rc9.14-dev+aaaaaaaaa"));
    assert!(state.checkout.join("README.md").exists());
    let resolved = format!("{COMMIT}^{{commit}}");
    assert!(
        fixture.commands.lock().unwrap().iter().any(|c| c.ends_with(&["rev-parse".into(), resolved.clone()])),
        "prepare never resolved the recorded commit"
    );
}

#[tokio::test]
async fn a_dev_build_whose_version_is_not_its_commit_is_refused() {
    for build in ["bbbbbbbbb", "aaaaaaaaa-dirty", "src"] {
        let mut fixture = Fixture::new("none");
        fixture.tagless = true;
        fixture.spec.identity = dev_identity(build);
        ReadyManifest::new(dev_identity(build), Some(COMMIT.into())).write(&fixture.spec.stage_dir).await.unwrap();
        let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
        let err = format!("{:#}", prepare(&spawner, &fixture.spec).await.unwrap_err());
        assert!(err.contains("is not the commit it was built from"), "{build}: {err}");
    }
}

#[tokio::test]
async fn a_dev_build_without_a_commit_names_the_cause() {
    let mut fixture = Fixture::new("none");
    fixture.tagless = true;
    fixture.spec.identity = dev_identity("aaaaaaaaa");
    ReadyManifest::new(dev_identity("aaaaaaaaa"), None).write(&fixture.spec.stage_dir).await.unwrap();
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let err = format!("{:#}", prepare(&spawner, &fixture.spec).await.unwrap_err());
    assert!(err.contains("needs COMMIT in SHA256SUMS"), "{err}");
}

#[tokio::test]
async fn a_dev_commit_origin_lacks_is_refused() {
    let mut fixture = Fixture::new("none");
    fixture.tagless = true;
    fixture.commit_missing = true;
    fixture.spec.identity = dev_identity("aaaaaaaaa");
    ReadyManifest::new(dev_identity("aaaaaaaaa"), Some(COMMIT.into())).write(&fixture.spec.stage_dir).await.unwrap();
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let err = format!("{:#}", prepare(&spawner, &fixture.spec).await.unwrap_err());
    assert!(err.contains("not present after fetch"), "{err}");
}

#[tokio::test]
async fn a_release_without_its_tag_still_fails() {
    let mut fixture = Fixture::new("none");
    fixture.tagless = true;
    ReadyManifest::new(fixture.spec.identity.clone(), Some(COMMIT.into()))
        .write(&fixture.spec.stage_dir)
        .await
        .unwrap();
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let err = prepare(&spawner, &fixture.spec).await.unwrap_err().to_string();
    assert!(err.contains("tag v9.9.9 not present after fetch"), "{err}");
}

#[tokio::test]
async fn prepare_restores_a_tracked_file_the_env_step_rewrote() {
    let fixture = Fixture::new("instantiate");
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let state = prepare(&spawner, &fixture.spec).await.unwrap();
    assert!(state.julia_instantiated);
    assert_eq!(
        std::fs::read_to_string(state.checkout.join("julia/kernel/Project.toml")).unwrap(),
        KERNEL_PROJECT
    );
    assert_eq!(std::fs::read_to_string(fixture.root.join("witness")).unwrap(), KERNEL_PROJECT,
        "the load test ran against a still-rewritten Project.toml — restore must happen before the load test, not after");
}

#[tokio::test]
async fn prepare_restores_a_tracked_file_the_load_test_rewrote() {
    let fixture = Fixture::new("load");
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let state = prepare(&spawner, &fixture.spec).await.unwrap();
    assert!(state.julia_instantiated);
    assert!(
        worktree_clean(&spawner, &state.checkout).await,
        "a file the load test dirtied must still be restored before the version is armed"
    );
}

#[tokio::test]
async fn prepare_keeps_untracked_build_products() {
    let fixture = Fixture::new("untracked");
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    let state = prepare(&spawner, &fixture.spec).await.unwrap();
    assert!(
        state.checkout.join("julia/kernel/Manifest.toml").exists(),
        "restoring tracked files must not sweep what instantiate produced"
    );
    assert!(worktree_clean(&spawner, &state.checkout).await);
}

#[tokio::test]
async fn restore_refuses_dirt_it_cannot_repair() {
    let fixture = Fixture::new("irreparable");
    let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
    *fixture.dirty.lock().unwrap() = true;
    let err = restore_to_tag(&spawner, &fixture.root, "v9.9.9")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("v9.9.9"), "must name the tag: {err}");
    assert!(err.contains("will not be armed"), "{err}");
}
