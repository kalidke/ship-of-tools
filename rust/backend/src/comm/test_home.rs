//! A scratch comm folder for messaging's unit tests: every variable a command reads is this test's while the guard
//! lives, the tests that use one run one at a time (`paths::ENV_TEST_LOCK`), and no daemon answers.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The variables a messaging command reads, every one captured and cleared or set by [`TestHome::new`].
const KEYS: [&str; 14] = [
    "SOT_COMM_HOME",
    "SOT_SELF_HOST",
    "SOT_COMM_TEST_HOST",
    "SOT_COMM_SELF_FILE",
    "SOT_WORKSPACE_ID",
    "SOT_COMM_NAME",
    "SOT_COMM_EXPERTISE",
    "SOT_SOCKET",
    "SOT_BACKEND_LABEL",
    "SOT_HOSTS",
    "SOT_LOCK_WAIT_SECS",
    "SOT_INBOX_LOCK_WAIT_SECS",
    "SOT_COMM_MSYS",
    "SOT_COMM_BIN",
];

pub(crate) struct TestHome {
    _serial: std::sync::MutexGuard<'static, ()>,
    _env: Vec<crate::paths::EnvGuard>,
    _dir: tempfile::TempDir,
    /// The comm folder.
    pub home: PathBuf,
    /// The project a command run by this test process belongs to (its working folder's checkout), canonical.
    pub project: PathBuf,
}

impl TestHome {
    /// The comm folder `<dir>/comm` on the box `box1`, no topology, no daemon.
    pub(crate) fn new() -> TestHome {
        let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = KEYS.iter().map(|k| crate::paths::EnvGuard::capture(*k)).collect();
        for k in KEYS {
            std::env::remove_var(k);
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("comm");
        let cwd = std::env::current_dir().expect("cwd");
        let project = PathBuf::from(crate::comm::identity::paths::canonical(&crate::comm::identity::paths::raw_root(&cwd)).expect("project"));
        std::env::set_var("SOT_COMM_HOME", &home);
        std::env::set_var("SOT_SELF_HOST", "box1");
        std::env::set_var("SOT_COMM_TEST_HOST", "box1");
        std::env::set_var("SOT_SOCKET", dir.path().join("no-daemon.sock"));
        std::env::set_var("SOT_HOSTS", dir.path().join("hosts.toml"));
        std::env::set_var("SOT_LOCK_WAIT_SECS", "2");
        crate::comm::home::ensure(&home);
        TestHome { _serial: serial, _env: env, _dir: dir, home, project }
    }

    pub(crate) fn root(&self) -> String {
        crate::comm::identity::paths::native(&self.project.to_string_lossy())
    }

    pub(crate) fn registry(&self) -> PathBuf {
        self.home.join("registry.json")
    }

    /// The registry's `agents`.
    pub(crate) fn agents(&self) -> Value {
        let doc: Value = serde_json::from_slice(&std::fs::read(self.registry()).expect("registry")).expect("json");
        doc["agents"].clone()
    }

    /// Sets `.agents[h]` to `entry`, as a session's join would have.
    pub(crate) fn put(&self, h: &str, entry: Value) {
        let mut doc: Value = serde_json::from_slice(&std::fs::read(self.registry()).expect("registry")).expect("json");
        doc["agents"][h] = entry;
        std::fs::write(self.registry(), serde_json::to_vec_pretty(&doc).unwrap()).expect("write registry");
    }

    /// A live entry for `h` on this box, rooted at `root`.
    pub(crate) fn live(&self, h: &str, root: &str) {
        let now = crate::comm::registry::registry::iso8601_utc_now();
        self.put(h, json!({"host": "box1", "workspace_id": "", "repo": "r", "root": root, "last_seen": now}));
    }

    /// The inbox lines of `h`.
    pub(crate) fn inbox(&self, h: &str) -> Vec<Value> {
        std::fs::read_to_string(self.home.join("inbox").join(format!("{h}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("an inbox line is JSON"))
            .collect()
    }

    /// Writes the inbox lock record naming this process's own lock manager, as the folder's hub would at its start, so
    /// a command here appends itself.
    pub(crate) fn own_record(&self) {
        let id = crate::comm::mail::inbox::lock_identity(&self.home.join("inbox"));
        std::fs::write(self.home.join(crate::comm::mail::inbox::LOCK_RECORD), format!("{id}\n")).expect("record");
    }

    /// The project's folder name: a command's `repo`.
    pub(crate) fn repo(&self) -> String {
        crate::comm::identity::paths::basename(&crate::comm::identity::paths::raw_root(&std::env::current_dir().expect("cwd")))
    }

    /// A self file for this process's identity at `path`.
    pub(crate) fn self_file(&self, path: &Path, name: &str) {
        crate::comm::identity::self_file::write(path, name, &self.repo(), &self.root()).expect("self file");
    }

    /// The shared slot of a session outside any row on this box.
    pub(crate) fn nopane(&self) -> PathBuf {
        self.home.join("self").join("box1__nopane.txt")
    }
}
