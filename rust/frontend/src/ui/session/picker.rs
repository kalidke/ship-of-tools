//! The new-session picker: its state, where it starts for a host, and the `State` methods that drive it to `workspace.create`.

use super::*;

/// Sessions-mode workspace picker (ADR 0014). When `State.workspace_picker`
/// is `Some(this)`, the NavTree renders this directory listing instead of
/// the Sessions list. Up/Down moves the cursor; Right drills into a
/// subdirectory (refires `directory.list`); Left ascends to the parent, landing
/// on the directory it came out of; Enter
/// commits the cursored directory as the new workspace's project_root (with the
/// ccb agent), Shift+Enter commits it as a bare session (no LLM agent); Esc
/// cancels. Commit chords are keymap-driven (session.create / .create_bare).
pub(in crate::ui) struct WorkspacePicker {
    /// The connection this picker browses and will create the workspace
    /// on — the "+ create new" row's own host, fixed for the picker's
    /// lifetime (ADR 0042 L2a). Every `directory.list`/`workspace.create`
    /// this picker fires routes via `send_to(&host, ...)`.
    host: HostKey,
    /// Whether the listing includes dot-entries. Starts ON: a hidden folder
    /// (a Julia depot, a dot-config tree) is a legitimate workspace root,
    /// and the picker exists to choose roots. `.` toggles it, the same key
    /// Files mode uses (`Action::ToggleHidden`, scope `FilesOrPicker`).
    show_hidden: bool,
    /// Absolute path of the directory we're currently showing. The
    /// title bar in the NavTree displays this so the user always knows
    /// where they are.
    current_path: String,
    /// Subdirectory rows under `current_path`. Populated by the
    /// `IncomingEvt::DirectoryList` handler when the path echoes ours.
    entries: Vec<crate::net::transport::DirEntry>,
    /// Cursor into `entries`. `0`-based; clamped on each refresh.
    selected: usize,
    /// The entry the cursor lands on when the pending listing arrives,
    /// by name (unique within one listing, and immune to a start path
    /// written with a trailing slash): the directory Left just came out of —
    /// so going back up returns you to where you went in, not the top — or
    /// the entry under the cursor before a hidden-folder toggle. Consumed by
    /// the listing that answers `current_path`.
    reveal: Option<String>,
    /// Per-session accounts: the
    /// login directories `accounts.list` reported for `host`, "default"
    /// first. Empty until the reply lands, and stays empty (the field the
    /// render/commit paths check) when the daemon only has "default" or
    /// predates the op — either way the account choice is hidden.
    accounts: Vec<crate::net::transport::AccountInfo>,
    /// Cursor into `accounts` (`Tab` cycles). `0` is always "default" when
    /// `accounts` is non-empty, so index 0 and "no choice made" both mean
    /// the same thing: the agent's own default directory.
    account_selected: usize,
}

/// The account a new row is created under: the selected one by name, or `None` (the daemon's own default)
/// for an absent or out-of-range selection and for the account named `default`, wherever it sits in the list.
pub(super) fn selected_account(picker: &WorkspacePicker) -> Option<String> {
    let account = picker.accounts.get(picker.account_selected)?;
    (account.name != "default").then(|| account.name.clone())
}

impl WorkspacePicker {
    /// A picker on `host` showing `start`, hidden folders shown, its listing and accounts not yet answered.
    pub(in crate::ui) fn open(host: HostKey, start: String) -> Self {
        Self {
            host,
            show_hidden: true,
            current_path: start,
            entries: Vec::new(),
            selected: 0,
            reveal: None,
            accounts: Vec::new(),
            account_selected: 0,
        }
    }

    pub(in crate::ui) fn current_path(&self) -> &str { &self.current_path }
    pub(in crate::ui) fn entries(&self) -> &[crate::net::transport::DirEntry] { &self.entries }
    pub(in crate::ui) fn selected(&self) -> usize { self.selected }

    /// The account the choice row names, when there is a choice.
    pub(in crate::ui) fn shown_account(&self) -> Option<&crate::net::transport::AccountInfo> {
        self.account_choice_visible().then(|| &self.accounts[self.account_selected])
    }

    /// Whether a `directory.list` reply from `host` for `path` answers this picker's pending listing.
    pub(in crate::ui) fn answers(&self, host: &HostKey, path: &str) -> bool {
        self.current_path == path && &self.host == host
    }

    /// A late or other host's listing is logged with the picker's own path and host.
    pub(in crate::ui) fn host(&self) -> &HostKey { &self.host }
    pub(in crate::ui) fn show_hidden(&self) -> bool { self.show_hidden }

    /// The accounts `host` reported, the first selected; another host's reply changes nothing.
    pub(in crate::ui) fn set_accounts(&mut self, host: &HostKey, accounts: Vec<crate::net::transport::AccountInfo>) {
        if &self.host == host {
            self.accounts = accounts;
            self.account_selected = 0;
        }
    }

    pub(in crate::ui) fn cursor_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub(in crate::ui) fn cursor_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    /// Show `path` while its listing is asked for: entries cleared, cursor at the top.
    pub(in crate::ui) fn go_to(&mut self, path: String) {
        self.current_path = path;
        self.entries.clear();
        self.selected = 0;
    }

    /// Into the cursored directory: the path to list, or `None` with no entry under the cursor.
    pub(in crate::ui) fn drill_in(&mut self) -> Option<String> {
        let path = self.entries.get(self.selected).map(|e| e.path.clone())?;
        self.go_to(path.clone());
        Some(path)
    }

    /// Up to the parent, landing the cursor on the directory left when its listing arrives: the path to list, or
    /// `None` at the top (no parent, or an empty one).
    pub(in crate::ui) fn ascend(&mut self) -> Option<String> {
        let path = std::path::Path::new(&self.current_path).parent().map(|p| p.to_string_lossy().into_owned())?;
        if path.is_empty() {
            return None;
        }
        self.reveal = std::path::Path::new(&self.current_path).file_name().map(|n| n.to_string_lossy().into_owned());
        self.go_to(path.clone());
        Some(path)
    }

    /// Flip hidden folders, keeping the cursor on its entry unless that is the one now hidden: the path to re-list.
    pub(in crate::ui) fn toggle_hidden(&mut self) -> String {
        self.show_hidden = !self.show_hidden;
        self.reveal = self.entries.get(self.selected).map(|e| e.name.clone());
        self.entries.clear();
        self.selected = 0;
        self.current_path.clone()
    }

    /// The next account, when there is a choice.
    pub(in crate::ui) fn cycle_account(&mut self) -> Option<&crate::net::transport::AccountInfo> {
        if !self.account_choice_visible() {
            return None;
        }
        self.account_selected = (self.account_selected + 1) % self.accounts.len();
        Some(&self.accounts[self.account_selected])
    }

    /// The cursored directory, or the shown one in an empty folder.
    pub(in crate::ui) fn chosen_path(&self) -> String {
        self.entries.get(self.selected).map(|e| e.path.clone()).unwrap_or_else(|| self.current_path.clone())
    }

    /// Install a listing that answers `current_path` and place the cursor:
    /// on `reveal` if the listing holds it, else where it was if that is
    /// still in range, else the top.
    pub(in crate::ui) fn land_listing(&mut self, entries: Vec<crate::net::transport::DirEntry>) {
        let reveal = self.reveal.take();
        self.entries = entries;
        let kept = if self.selected < self.entries.len() { self.selected } else { 0 };
        self.selected = reveal
            .and_then(|name| self.entries.iter().position(|e| e.name == name))
            .unwrap_or(kept);
    }

    /// Per-session accounts: true only
    /// when there's a real choice — more than "default" alone. An empty
    /// list (old daemon with no `accounts.list` handler, or the reply
    /// hasn't landed yet) reads exactly like default-only: hidden, no
    /// error surfaced. The one gate the render row, the Tab cycle, and the
    /// commit path all share, so they can't drift apart.
    pub(in crate::ui) fn account_choice_visible(&self) -> bool {
        self.accounts.len() > 1
    }
}

/// Local-host fallback for the workspace picker's starting directory
/// (`State::begin_create_session`), used only once every higher-priority
/// source (the `[sessions] new_session_root` setting, `$SOT_PROJECTS_ROOT`,
/// the target host's `remote_home`, `$SOT_REMOTE_HOME`) has come up empty.
/// `env_home` is `$HOME`; `os_home` is what `dirs::home_dir()` reports (the
/// Win32 known-folder API on Windows). Prefers `env_home` when set (an
/// explicit override wins), then `os_home`, and only degrades to a bare
/// filesystem root — which carries no `Prefix`/drive-letter component on
/// Windows — when neither is available.
///
/// A bare `/` here used to be the ONLY local fallback: `$HOME` is unset on
/// a plain Windows launch (no git-bash/MSYS in the process env), so the
/// picker's first `directory.list` request went out rooted at `/`. That
/// "works" only because a leading-slash path resolves against the
/// backend's *current* drive — and every further drill-in the picker
/// performs joins onto that same driveless string (`PathBuf::push` never
/// re-derives a dropped prefix), so the eventual `workspace.create`
/// persisted a driveless root (observed:
/// `project_root = "/Users\<user>\HomeLab\<repo>"`, no `C:`). Consulting
/// `dirs::home_dir()` — what the OS itself reports — before falling all
/// the way to `/` keeps the drive letter from the very first request.
///
/// A standalone (non-method) function so this has a seam to unit-test
/// without constructing a full `State`.
pub(super) fn picker_local_home_fallback(env_home: Option<String>, os_home: Option<PathBuf>) -> String {
    env_home
        .or_else(|| os_home.map(|h| h.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "/".to_string())
}

/// Where the create-workspace picker starts for `host`. The picker browses
/// the TARGET daemon's filesystem, so the answer must be a path on that
/// machine. `default_row_root` is that daemon's own default row, anchored
/// at its user home (ADR 0042) -- the one per-host path the frontend holds
/// for every host. For the implicit "local" host a `configured` projects
/// root counts only when it exists HERE (`local_dir_exists`; the frontend
/// runs on that machine): a backend path such as `/home/u/dev` on a Windows
/// box is ignored, so the local picker starts at the user's own directory
/// until the user sets one. Remote hosts keep the configured root first
/// (a projects dir beats a home), then the host's remote home, then the
/// daemon's default root, then the frontend's own home as the last resort
/// it always was.
pub(super) fn picker_start_for_host(
    host: &str,
    default_row_root: Option<&str>,
    configured: Option<&str>,
    remote_home: Option<&str>,
    fe_home: String,
    local_dir_exists: impl Fn(&str) -> bool,
) -> String {
    let pick = |candidates: [Option<&str>; 3]| {
        candidates
            .into_iter()
            .flatten()
            .next()
            .map(str::to_string)
            .unwrap_or(fe_home.clone())
    };
    if host == "local" {
        pick([configured.filter(|p| local_dir_exists(p)), default_row_root, None])
    } else {
        pick([configured, remote_home, default_row_root])
    }
}

#[cfg(test)]
#[path = "picker_tests.rs"]
mod tests;
