//! The process trees the cases start inside a daemon and watch for its end. A tree is a leader, a child and a grandchild,
//! the last two ignoring TERM and HUP, the leader a session of its own (it is started detached); each writes its pid to
//! the case's folder once it is up, so the case learns the identities from the fixture itself and never from `ps`.
//! The forking tree is a leader that starts a short-lived child every millisecond, to outlast a single sweep.

use std::path::{Path, PathBuf};

/// One tree's folder: where its processes report.
pub struct Tree {
    dir: PathBuf,
}

/// The shell a tree runs; `$1` is its folder. The leader becomes `sleep` (its pid is `$$`), the child is a `sleep`, and the
/// grandchild is a `sleep` under a subshell.
const TREE: &str = r#"trap '' HUP TERM
D=$1
sleep 3152 & echo $! > "$D/child.pid"
( trap '' HUP TERM; sleep 3153 & echo $! > "$D/grandchild.pid"; wait ) &
echo $$ > "$D/leader.pid"
exec sleep 3150
"#;

/// The shell of the forking tree: it only ever forks and waits, so there is always a young child to kill.
const FORKING: &str = r#"trap '' HUP TERM
D=$1
echo $$ > "$D/leader.pid"
while :; do sleep 0.001; done
"#;

impl Tree {
    pub fn new(root: &Path, name: &str) -> Tree {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("create the tree's folder");
        std::fs::write(dir.join("tree.sh"), TREE).expect("write the tree");
        std::fs::write(dir.join("forking.sh"), FORKING).expect("write the forking tree");
        Tree { dir }
    }

    /// Where a fixture process of the case writes its pid when it reports (`name`.pid in the tree's folder).
    pub fn report_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.pid"))
    }

    pub fn dir_str(&self) -> String {
        self.dir.display().to_string()
    }

    /// The tree's script, for a document or a cell to name.
    pub fn script_path(&self, forking: bool) -> String {
        self.dir
            .join(if forking { "forking.sh" } else { "tree.sh" })
            .display()
            .to_string()
    }

    /// The words after `sh` that start the tree: its script and its folder.
    pub fn start_words(&self, forking: bool) -> String {
        format!("{} {}", self.script_path(forking), self.dir_str())
    }

    /// The command line that starts the tree, for a shell to run.
    pub fn shell_command(&self, forking: bool) -> String {
        format!(
            "sh '{}' '{}'",
            self.dir
                .join(if forking { "forking.sh" } else { "tree.sh" })
                .display(),
            self.dir.display()
        )
    }

    /// The Julia cell that ignores TERM and HUP, starts the tree detached and then spins without yielding.
    pub fn julia_cell(&self, forking: bool) -> String {
        format!(
            "ccall(:signal, Ptr{{Cvoid}}, (Cint, Ptr{{Cvoid}}), 15, Ptr{{Cvoid}}(1)); ccall(:signal, Ptr{{Cvoid}}, (Cint, Ptr{{Cvoid}}), 1, Ptr{{Cvoid}}(1)); run(detach(`sh {} {}`); wait = false); while true end",
            self.dir.join(if forking { "forking.sh" } else { "tree.sh" }).display(),
            self.dir.display()
        )
    }

    /// The same cell without the spin: it starts the tree detached and returns, so the REPL is idle and no request is open.
    pub fn julia_cell_returning(&self, forking: bool) -> String {
        let spinning = self.julia_cell(forking);
        spinning
            .strip_suffix("; while true end")
            .expect("the cell ends in its spin")
            .to_string()
    }

    /// The pids the tree's processes reported, named, once every one of them has.
    pub fn pids(&self, forking: bool) -> Option<Vec<(&'static str, i32)>> {
        let names: &[&'static str] = if forking {
            &["leader"]
        } else {
            &["leader", "child", "grandchild"]
        };
        names
            .iter()
            .map(|name| {
                std::fs::read_to_string(self.dir.join(format!("{name}.pid")))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
                    .map(|pid| (*name, pid))
            })
            .collect()
    }
}

/// The processes of the session `sid`, from `/proc` (observed, never signalled): a tree leader that is a session of its
/// own names all its descendants this way, however many generations of short-lived children it has left.
pub fn session_members(sid: i32) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| {
        let pid: i32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // After the command name: state, parent, group, session.
        let session: i32 = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(3)?
            .parse()
            .ok()?;
        (session == sid).then_some(pid)
    })
    .collect()
}
