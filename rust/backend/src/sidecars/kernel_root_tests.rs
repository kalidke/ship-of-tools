use super::*;
use crate::sidecars::contract_tests::executable;

/// A kernel project whose `ShipToolsKernel` answers the hello and then every request with the root it was given and
/// its `LOAD_PATH`, run by the real julia: what reaches the child is what the generation's argv and environment say.
fn echo_kernel(dir: &Path) -> PathBuf {
    let project = dir.join("kp");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("Project.toml"), "name = \"ShipToolsKernel\"\nuuid = \"7a0f3c52-5c1e-4d2b-9a6e-0b1f5d3e8c11\"\n").unwrap();
    std::fs::write(
        project.join("src/ShipToolsKernel.jl"),
        r#"module ShipToolsKernel
esc(s) = replace(replace(String(s), "\\" => "\\\\"), "\"" => "\\\"")
function serve(io_in::IO, io_out::IO; project_root::AbstractString = pwd())
    readline(io_in)
    println(io_out, "{\"id\":1,\"payload\":{\"protocol\":1}}"); flush(io_out)
    for line in eachline(io_in)
        id = match(r"\"id\":(\d+)", line).captures[1]
        lp = join(["\"" * esc(p) * "\"" for p in LOAD_PATH], ",")
        println(io_out, "{\"id\":$id,\"payload\":{\"root\":\"$(esc(project_root))\",\"load_path\":[$lp]}}"); flush(io_out)
    end
end
end
"#,
    )
    .unwrap();
    let julia = {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        executable("julia")
    };
    tests::STUB_BIN.lock().unwrap().push((project.clone(), julia.to_string_lossy().into_owned()));
    project
}

/// One request to a kernel started on `root`; its answer.
async fn ask(root: &Path) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let project = echo_kernel(dir.path());
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, mut status_rx) = watch::channel(Status::Starting);
    let task = tokio::spawn(supervisor_loop(project, root.to_path_buf(), status, sig, KernelBounds::PRODUCT));
    // One bound for the whole start: a kernel that fails its generation changes the status again and again.
    let tx = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if let Status::Running(tx) = status_rx.borrow_and_update().clone() {
                return tx;
            }
            status_rx.changed().await.expect("status channel");
        }
    })
    .await
    .expect("the kernel never answered its hello");
    let answer = submit_and_await(&tx, "echo", json!({}), KERNEL_REQUEST_TIMEOUT).await.expect("the kernel serves a request");
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
    answer
}

/// The workspace root reaches the kernel as data. The case that would break it: the root spliced into the Julia source
/// the child runs, where `$(...)` is code and a quote or backslash is syntax.
#[tokio::test]
async fn a_workspace_root_reaches_the_kernel_as_data_not_as_source() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("r$(error(123))\"q\\z");
    std::fs::create_dir(&root).unwrap();
    let answer = ask(&root).await;
    assert_eq!(answer["root"], json!(root.to_str().unwrap()), "{answer}");
}

/// The kernel's load path holds no user environment: the default environment (`@v#.#`) is not in it, whatever
/// `JULIA_LOAD_PATH` the daemon inherited.
#[tokio::test]
async fn the_kernel_s_load_path_holds_no_user_environment() {
    let root = tempfile::tempdir().unwrap();
    let answer = ask(root.path()).await;
    let entries: Vec<&str> = answer["load_path"].as_array().expect("load_path").iter().map(|e| e.as_str().unwrap()).collect();
    assert!(entries.iter().all(|e| *e == "@" || *e == "@stdlib"), "{entries:?}");
}
