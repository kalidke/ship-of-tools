//! The product worker factories, read where they stand and not from their source: a Julia `Distributed` worker (the
//! worker Pluto's notebooks run in, here by the same `addprocs` call Malt's `DistributedStdlibWorker` makes) against
//! the process that started it. Pluto reaches it through `Malt.DistributedStdlibWorker`, whose constructor evaluates
//! `Distributed.addprocs(1; exeflags=...)`; `LocalManager` starts the worker with `open(detach(cmd), "r+")`, and a
//! detached process is started in a session of its own. The case records that, so the lane's ephemeral containment
//! does not assume a worker stays in the group of the process that launched it.
//!
//! Needs a Julia 1.12 (`SOT_JULIA_BIN`, else `julia` on the PATH), so it is `#[ignore]` in a plain `cargo test` and
//! run by the harness job (`-- --ignored`); with no Julia it fails and never passes by skipping.

use crate::fixture_owner::Fixture;
use std::process::{Command, Stdio};

/// What the script prints: the master's and the worker's pid, session and group, one `who pid sid pgid` line each.
const SCRIPT: &str = r#"
using Distributed
sid(p) = ccall(:getsid, Cint, (Cint,), p)
pgid(p) = ccall(:getpgid, Cint, (Cint,), p)
worker = addprocs(1)[1]
wpid = remotecall_fetch(getpid, worker)
println("master ", getpid(), " ", sid(0), " ", pgid(0))
println("worker ", wpid, " ", sid(wpid), " ", pgid(wpid))
rmprocs(worker)
"#;

#[test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
fn a_distributed_worker_starts_in_a_session_of_its_own() {
    let julia = std::env::var_os("SOT_JULIA_BIN").unwrap_or_else(|| "julia".into());
    let mut fx = Fixture::new("distributed_worker_session");
    let master = Command::new(julia)
        .args(["--startup-file=no", "-e", SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start julia");
    // Authority over the master first; the worker is learned from what the master reports and adopted while it lives.
    fx.adopt(master.id() as i32, None, "julia master")
        .expect("authority over the master");
    let out = master.wait_with_output().expect("the script ends");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "the script failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let row = |who: &str| -> Vec<i32> {
        text.lines()
            .find_map(|line| line.strip_prefix(who)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("no {who} line in:\n{text}"))
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect()
    };
    let (master_row, worker_row) = (row("master"), row("worker"));
    fx.save("master", format!("{master_row:?}"));
    fx.save("worker", format!("{worker_row:?}"));
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    let (wpid, wsid, wpgid) = (worker_row[0], worker_row[1], worker_row[2]);
    assert_eq!(
        (wsid, wpgid),
        (wpid, wpid),
        "the worker is not its own session leader: {worker_row:?} (master {master_row:?})"
    );
    assert_ne!(
        wsid, master_row[1],
        "the worker shares the master's session: {worker_row:?} (master {master_row:?})"
    );
}
