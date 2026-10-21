//! Who the calling process is to messaging: its box, its row, its project and its handle. `sotd comm context` prints
//! it in the eval-able form `comm-context.sh` always printed; every other command reads it in process.

use std::path::{Path, PathBuf};

use super::derive::Kind;
use super::{paths, self_file};
use crate::comm::registry::registry::{binding, Binding};

/// The identity of the calling process.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Context {
    /// The box's declared host: the registry's `host` and the self file's key.
    pub host: String,
    /// The box's raw host name (`hostname -s`, case kept; `SOT_COMM_TEST_HOST` first): a derived handle's host piece.
    pub handle_host: String,
    /// `$SOT_WORKSPACE_ID`, the row the environment names.
    pub workspace_id: String,
    /// The row the identity names: a pinned self file's, else `$SOT_WORKSPACE_ID`.
    pub row_id: String,
    pub repo: String,
    /// The agent kind the row's start stamped as `$SOT_AGENT_KIND`; Claude when absent.
    pub kind: Kind,
    /// The canonical project root.
    pub project_root: String,
    /// The handle, or `""` (not joined, or another row's).
    pub name: String,
    /// The row whose handle the self file named, when it was discarded for being another row's.
    pub name_held_by: String,
    pub self_file: PathBuf,
    pub comm_home: PathBuf,
}

impl Context {
    pub(crate) fn registry(&self) -> PathBuf {
        self.comm_home.join("registry.json")
    }

    pub(crate) fn inbox_dir(&self) -> PathBuf {
        self.comm_home.join("inbox")
    }

    pub(crate) fn read_dir(&self) -> PathBuf {
        self.comm_home.join("read")
    }

    /// The lines `comm-context.sh` prints, each `KEY=value` with the value quoted for bash, and a bare `KEY=` when
    /// empty (a scraper reading `NAME=` gets nothing, not two quote marks).
    pub(crate) fn eval_lines(&self) -> String {
        let p = |p: &Path| paths::native(&p.to_string_lossy());
        let fields: [(&str, String); 13] = [
            ("HOST", self.host.clone()),
            ("HANDLE_HOST", self.handle_host.clone()),
            ("WORKSPACE_ID", self.workspace_id.clone()),
            ("ROW_ID", self.row_id.clone()),
            ("REPO", self.repo.clone()),
            ("PROJECT_ROOT", self.project_root.clone()),
            ("NAME", self.name.clone()),
            ("NAME_HELD_BY", self.name_held_by.clone()),
            ("SELF_FILE", p(&self.self_file)),
            ("COMM_HOME", p(&self.comm_home)),
            ("REGISTRY", p(&self.registry())),
            ("INBOX_DIR", p(&self.inbox_dir())),
            ("READ_DIR", p(&self.read_dir())),
        ];
        fields.iter().map(|(k, v)| format!("{k}={}\n", shell_quote(v))).collect()
    }
}

/// `v` as one bash word, the way `printf %q` writes it: as is when every character is plain, `$'...'` when it holds a
/// control character, else each special character escaped with a backslash.
pub(crate) fn shell_quote(v: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./-:,@%+=".contains(c) || !c.is_ascii();
    if v.chars().all(plain) && !v.starts_with('=') {
        return v.to_string();
    }
    if v.chars().any(|c| c.is_control()) {
        let mut out = String::from("$'");
        for c in v.chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    v.chars().map(|c| if plain(c) { c.to_string() } else { format!("\\{c}") }).collect()
}

/// The box's raw host name, as `hostname -s` gives it (case kept); `SOT_COMM_TEST_HOST` first, a test's pin.
pub(crate) fn raw_host(env: &crate::comm::Env) -> String {
    if let Some(h) = env.get_nonempty("SOT_COMM_TEST_HOST") {
        return h.to_string_lossy().into_owned();
    }
    let h = gethostname::gethostname().to_string_lossy().into_owned();
    h.split('.').next().unwrap_or("").trim().to_string()
}

/// The row the identity names: the pinned self file's (`<host>__<id>.txt`), else `$SOT_WORKSPACE_ID` when no self file
/// is pinned. A pinned file of any other name names no row, whatever the environment says: a test that pins its own
/// scratch identity inherits its launcher's row id, and must not declare into that row.
pub(crate) fn row_id(env: &crate::comm::Env, host: &str) -> String {
    match env.get_nonempty("SOT_COMM_SELF_FILE") {
        Some(pinned) => self_file::row_of(Path::new(pinned), host).unwrap_or_default(),
        None => env.text("SOT_WORKSPACE_ID"),
    }
}

/// Resolves the identity of a process working in `cwd`, reading the self file and the registry and writing nothing
/// but, when the comm folder is not whole and `may_write` (the agent check passed), the folder itself. `rebind` is a
/// join's: a process with a row id keeps a handle another row's entry binds, since its join moves the handle back.
/// Lines for the session go to `notes`; `Err` is the line that ends the command.
pub(crate) fn resolve(env: &crate::comm::Env, rebind: bool, cwd: &Path, may_write: impl FnOnce() -> bool, notes: &mut Vec<String>) -> Result<Context, String> {
    let host = sot_platform::host::state_dir::host_name_in(env).map_err(|e| {
        format!("comm-context: could not establish this box's declared host ({e}); aborting rather than proceeding with an unknown identity")
    })?;
    let comm_home = crate::comm::home::comm_home(env).ok_or("comm-context: no comm folder: none of SOT_COMM_HOME, HOME or USERPROFILE is set")?;
    let workspace_id = env.text("SOT_WORKSPACE_ID");
    let raw = paths::raw_root(cwd);
    let repo = paths::basename(&raw);
    let project_root = paths::canonical(&raw).map_err(|e| format!("comm-context: {e}"))?;
    let self_file = match env.get_nonempty("SOT_COMM_SELF_FILE") {
        Some(p) => PathBuf::from(p),
        None => {
            let leaf = self_file::slot_leaf(&host, &workspace_id)
                .map_err(|_| format!("comm-context: aborting before any write: no self slot can be named for host '{host}'"))?;
            comm_home.join("self").join(leaf)
        }
    };
    if !crate::comm::home::is_whole(&comm_home) && may_write() {
        notes.extend(crate::comm::home::ensure(&comm_home, env));
    }
    let mut name = self_file::name_for(&self_file, &project_root, notes);
    let row_id = row_id(env, &host);
    let mut name_held_by = String::new();
    if !name.is_empty() && !(rebind && !row_id.is_empty()) {
        if let Binding::Other { ws, .. } = binding(&comm_home.join("registry.json"), &name, &row_id) {
            let carries = if row_id.is_empty() { "no row id".to_string() } else { row_id.clone() };
            notes.push(format!("comm-context: @{name} belongs to row {ws}; this process carries {carries}, so it is not this row's inbox"));
            name_held_by = ws;
            name = String::new();
        }
    }
    Ok(Context {
        host,
        handle_host: raw_host(env),
        workspace_id,
        row_id,
        repo,
        kind: Kind::of(&env.text("SOT_AGENT_KIND")),
        project_root,
        name,
        name_held_by,
        self_file,
        comm_home,
    })
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
