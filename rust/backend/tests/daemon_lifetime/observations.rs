//! What the harness reads of the real processes it runs: a bounded poll, and the report a process held at a named
//! phase barrier (`sot_log::test_barrier`) leaves in the barrier folder.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Poll `check` every 20 ms until it returns `Some`, for at most `bound`.
pub fn wait_for<T>(bound: Duration, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {bound:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A process's report at a barrier: who it is and where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub pid: i32,
    pub created: u64,
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
}

/// The folder the processes of one case report to and wait in.
pub struct BarrierDir {
    dir: PathBuf,
}

impl BarrierDir {
    pub fn new(root: &Path) -> BarrierDir {
        let dir = root.join("barriers");
        std::fs::create_dir_all(&dir).expect("create the barrier folder");
        BarrierDir { dir }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Hold the first `count` arrivals at `phase`.
    pub fn hold(&self, phase: &str, count: usize) {
        std::fs::write(self.dir.join(format!("{phase}.hold")), count.to_string())
            .expect("write the hold count");
    }

    /// The report of arrival `ticket` at `phase`, once it has reached the barrier.
    pub fn reached(&self, phase: &str, ticket: usize, bound: Duration) -> Report {
        let path = self.dir.join(format!("{phase}.{ticket}.reached"));
        wait_for(bound, &format!("arrival {ticket} at {phase}"), || {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse_report(&text))
        })
    }

    /// Whether arrival `ticket` at `phase` has reached it, without waiting.
    pub fn has_reached(&self, phase: &str, ticket: usize) -> bool {
        self.dir.join(format!("{phase}.{ticket}.reached")).exists()
    }

    /// Let arrival `ticket` at `phase` go on.
    pub fn open(&self, phase: &str, ticket: usize) {
        std::fs::write(self.dir.join(format!("{phase}.{ticket}.go")), b"")
            .expect("write the go file");
    }
}

fn parse_report(text: &str) -> Option<Report> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            line.strip_prefix(key)?
                .strip_prefix(' ')?
                .trim()
                .parse::<i64>()
                .ok()
        })
    };
    Some(Report {
        pid: field("pid")? as i32,
        created: field("created")? as u64,
        ppid: field("ppid")? as i32,
        pgid: field("pgid")? as i32,
        sid: field("sid")? as i32,
    })
}
