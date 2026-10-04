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
    pub(in crate::ui) host: HostKey,
    /// Whether the listing includes dot-entries. Starts ON: a hidden folder
    /// (a Julia depot, a dot-config tree) is a legitimate workspace root,
    /// and the picker exists to choose roots. `.` toggles it, the same key
    /// Files mode uses (`Action::ToggleHidden`, scope `FilesOrPicker`).
    show_hidden: bool,
    /// Absolute path of the directory we're currently showing. The
    /// title bar in the NavTree displays this so the user always knows
    /// where they are.
    pub(in crate::ui) current_path: String,
    /// Subdirectory rows under `current_path`. Populated by the
    /// `IncomingEvt::DirectoryList` handler when the path echoes ours.
    pub(in crate::ui) entries: Vec<crate::net::transport::DirEntry>,
    /// Cursor into `entries`. `0`-based; clamped on each refresh.
    pub(in crate::ui) selected: usize,
    /// The entry the cursor lands on when the pending listing arrives,
    /// by name (unique within one listing, and immune to a start path
    /// written with a trailing slash): the directory Left just came out of —
    /// so going back up returns you to where you went in, not the top — or
    /// the entry under the cursor before a hidden-folder toggle. Consumed by
    /// the listing that answers `current_path`.
    reveal: Option<String>,
    /// Per-session accounts (owner-simplified brief, 2026-09-15): the
    /// login directories `accounts.list` reported for `host`, "default"
    /// first. Empty until the reply lands, and stays empty (the field the
    /// render/commit paths check) when the daemon only has "default" or
    /// predates the op — either way the account choice is hidden.
    pub(in crate::ui) accounts: Vec<crate::net::transport::AccountInfo>,
    /// Cursor into `accounts` (`Tab` cycles). `0` is always "default" when
    /// `accounts` is non-empty, so index 0 and "no choice made" both mean
    /// the same thing: the agent's own default directory.
    pub(in crate::ui) account_selected: usize,
}

impl WorkspacePicker {
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

    /// Per-session accounts (owner-simplified brief, 2026-09-15): true only
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
fn picker_local_home_fallback(env_home: Option<String>, os_home: Option<PathBuf>) -> String {
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
fn picker_start_for_host(
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

impl State {
    /// Sessions-mode workspace picker entry point (ADR 0014). Opens a
    /// directory-tree browser rooted at `$SOT_PROJECTS_ROOT` (or
    /// `$HOME` if that's unset/missing), kicks off the first
    /// `directory.list` request, and parks the cursor on the first
    /// entry once it arrives. The legacy label-only prompt
    /// (`begin_create_session` + `confirm_create_session`) was
    /// superseded — users browse to an existing directory rather than
    /// typing a path that might not exist.
    /// `host` is the connection the new workspace is created ON — the
    /// Sessions-mode "+ create new" row's own host (ADR 0042 L2a: each
    /// host group carries its own create row, so this is a row-targeted
    /// op, routed via `send_to` rather than whatever's currently active).
    pub(in crate::ui) fn begin_create_session(&mut self, host: HostKey) {
        // Default-root for the picker. Priority:
        //   0. `[sessions] new_session_root` setting — the user's configured
        //      projects root (a BACKEND path); the knob for "start the picker
        //      at my dev dir, not $HOME".
        //   1. $SOT_PROJECTS_ROOT — explicit env override, e.g. someone
        //      wants the picker to start under a specific projects dir.
        //   2. The launcher-set SOT_REMOTE_HOME, if it propagated (a
        //      per-host `remote_home` config field used to feed this tier
        //      too — deleted with hosts.toml, topology plan lane D: the
        //      daemon's own default-row root, tier below, is the query-not-
        //      guess replacement `workspace.list` already serves).
        //   3+. Frontend's own $HOME, then the OS-reported home dir, then
        //      the filesystem root — see `picker_local_home_fallback`,
        //      whose doc comment explains why step 3 alone (a bare `/`)
        //      was a Windows drive-letter bug.
        //   Every tier above names a path on some BACKEND; the picker
        //   browses the TARGET host's filesystem, so for the implicit
        //   "local" host they are wrong by construction (field defect
        //   2026-09-05: a Windows box proposed the remote backend's Linux
        //   home to its own local daemon). Each host's daemon reports its
        //   default row, anchored at that machine's user home (ADR 0042),
        //   which is the one per-host path the frontend always holds --
        //   see `picker_start_for_host`.
        let default_row_root = self
            .workspace_lists
            .get(&host)
            .and_then(|l| l.iter().find(|w| w.is_default))
            .map(|w| w.project_root.clone());
        let configured = self
            .settings
            .new_session_root
            .clone()
            .or_else(|| std::env::var("SOT_PROJECTS_ROOT").ok());
        let remote_home = std::env::var("SOT_REMOTE_HOME").ok();
        let fe_home = picker_local_home_fallback(std::env::var("HOME").ok(), dirs::home_dir());
        let start = picker_start_for_host(
            &host,
            default_row_root.as_deref(),
            configured.as_deref(),
            remote_home.as_deref(),
            fe_home,
            |p| std::path::Path::new(p).is_dir(),
        );
        self.workspace_picker = Some(WorkspacePicker {
            host: host.clone(),
            show_hidden: true,
            current_path: start.clone(),
            entries: Vec::new(),
            selected: 0,
            reveal: None,
            accounts: Vec::new(),
            account_selected: 0,
        });
        if let Err(e) = self.send_to(
            &host,
            crate::net::transport::OutgoingReq::DirectoryList {
                path: start.clone(),
                include_hidden: true,
            },
        ) {
            tracing::warn!(error = %e, %host, %start, "drop initial directory.list — channel closed");
        }
        // Per-session accounts (owner-simplified brief, 2026-09-15): ask
        // the daemon that will OWN the row, never the frontend's own disk
        // — the login must exist where the session runs. An old daemon's
        // reply parses to an empty list (see `PendingKind::AccountsList`),
        // which keeps the choice hidden exactly like a fresh daemon that
        // only reports "default".
        if let Err(e) = self.send_to(&host, crate::net::transport::OutgoingReq::AccountsList) {
            tracing::warn!(error = %e, %host, "drop initial accounts.list — channel closed");
        }
        self.status = format!("create workspace · {host} · picker @ {start}");
        self.window.request_redraw();
    }

    /// Move the picker's cursor up by one row (saturating).
    pub(in crate::ui) fn picker_cursor_up(&mut self) {
        if let Some(p) = self.workspace_picker.as_mut() {
            if p.selected > 0 {
                p.selected -= 1;
            }
            self.window.request_redraw();
        }
    }

    /// Move the picker's cursor down by one row (clamped to entries
    /// length). Zero-entry directories pin the cursor at 0.
    pub(in crate::ui) fn picker_cursor_down(&mut self) {
        if let Some(p) = self.workspace_picker.as_mut() {
            if p.selected + 1 < p.entries.len() {
                p.selected += 1;
            }
            self.window.request_redraw();
        }
    }

    /// Drill into the cursored directory: re-fire `directory.list` for
    /// its path and clear the entry list pending the response. Updates
    /// `current_path` immediately so the title reflects where the user
    /// is going even before the listing lands.
    pub(in crate::ui) fn picker_drill_in(&mut self) {
        let Some(host) = self.workspace_picker.as_ref().map(|p| p.host.clone()) else {
            return;
        };
        let next = match self.workspace_picker.as_ref() {
            Some(p) => p.entries.get(p.selected).map(|e| e.path.clone()),
            None => None,
        };
        if let Some(path) = next {
            if let Some(p) = self.workspace_picker.as_mut() {
                p.current_path = path.clone();
                p.entries.clear();
                p.selected = 0;
            }
            let include_hidden = self.workspace_picker.as_ref().is_some_and(|p| p.show_hidden);
            if let Err(e) = self.send_to(
                &host,
                crate::net::transport::OutgoingReq::DirectoryList { path: path.clone(), include_hidden },
            ) {
                tracing::warn!(error = %e, %path, "drop directory.list (drill-in)");
            }
            self.status = format!("picker · {path}");
            self.window.request_redraw();
        }
    }

    /// Ascend to the parent of the picker's `current_path`. Re-fires
    /// the listing so the parent's entries populate.
    pub(in crate::ui) fn picker_ascend(&mut self) {
        let Some(host) = self.workspace_picker.as_ref().map(|p| p.host.clone()) else {
            return;
        };
        let parent = match self.workspace_picker.as_ref() {
            Some(p) => std::path::Path::new(&p.current_path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned()),
            None => None,
        };
        if let Some(path) = parent {
            if path.is_empty() {
                return;
            }
            if let Some(p) = self.workspace_picker.as_mut() {
                // The directory being left is an entry of its parent: land
                // the cursor on it when the parent's listing arrives.
                p.reveal = std::path::Path::new(&p.current_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                p.current_path = path.clone();
                p.entries.clear();
                p.selected = 0;
            }
            let include_hidden = self.workspace_picker.as_ref().is_some_and(|p| p.show_hidden);
            if let Err(e) = self.send_to(
                &host,
                crate::net::transport::OutgoingReq::DirectoryList { path: path.clone(), include_hidden },
            ) {
                tracing::warn!(error = %e, %path, "drop directory.list (ascend)");
            }
            self.status = format!("picker · {path}");
            self.window.request_redraw();
        }
    }

    /// Commit the *cursored sub-directory* as the new workspace's
    /// `project_root`. Falls back to `current_path` if the picker has
    /// no entries (so committing in an empty directory still works).
    /// Label is derived from the basename. Fires `workspace.create`;
    /// the response handler closes the picker and refreshes the
    /// Sessions list.
    /// `.` in the picker: flip hidden entries and re-list the same folder.
    pub(in crate::ui) fn picker_toggle_hidden(&mut self) {
        let Some(p) = self.workspace_picker.as_mut() else {
            return;
        };
        p.show_hidden = !p.show_hidden;
        // Keep the cursor on the entry it was on, unless that entry is the
        // one now hidden.
        p.reveal = p.entries.get(p.selected).map(|e| e.name.clone());
        p.entries.clear();
        p.selected = 0;
        let (host, path, include_hidden) = (p.host.clone(), p.current_path.clone(), p.show_hidden);
        if let Err(e) = self.send_to(
            &host,
            crate::net::transport::OutgoingReq::DirectoryList { path: path.clone(), include_hidden },
        ) {
            tracing::warn!(error = %e, %path, "drop directory.list (toggle hidden)");
        }
        self.status = format!(
            "picker · {path} · hidden folders {}",
            if include_hidden { "shown" } else { "hidden" }
        );
        self.window.request_redraw();
    }

    /// `Tab` in the picker: cycle to the next account (owner-simplified
    /// brief, 2026-09-15). No-op when `accounts` has 0 or 1 entries (the
    /// choice is hidden — either only "default" exists, or the daemon
    /// never answered `accounts.list`).
    pub(in crate::ui) fn picker_cycle_account(&mut self) {
        let Some(p) = self.workspace_picker.as_mut() else {
            return;
        };
        if !p.account_choice_visible() {
            return;
        }
        p.account_selected = (p.account_selected + 1) % p.accounts.len();
        let acct = &p.accounts[p.account_selected];
        let suffix = if acct.any_logged_in() { "" } else { " (not logged in)" };
        self.status = format!("picker · account: {}{suffix}", acct.name);
        self.window.request_redraw();
    }

    pub(in crate::ui) fn picker_confirm_selected(&mut self, agent: &str) {
        let path = match self.workspace_picker.as_ref() {
            Some(p) => p
                .entries
                .get(p.selected)
                .map(|e| e.path.clone())
                .unwrap_or_else(|| p.current_path.clone()),
            None => return,
        };
        self.commit_workspace_create(path, agent);
    }

    fn commit_workspace_create(&mut self, path: String, agent: &str) {
        // The picker's own host, not `active_host` (ADR 0042 L2a) — the
        // "+ create new" row that opened this picker belongs to a specific
        // host group, and the workspace must be created there regardless
        // of which connection is currently active.
        let host = self
            .workspace_picker
            .as_ref()
            .map(|p| p.host.clone())
            .unwrap_or_else(|| self.active_host.clone());
        let label = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "workspace".to_string());
        // Per-session accounts: index 0 ("default") and an empty/absent
        // accounts list both mean "no choice" — `None` either way, so the
        // daemon resolves its own default directory.
        let account = self.workspace_picker.as_ref().and_then(|p| {
            if p.account_selected == 0 {
                None
            } else {
                p.accounts.get(p.account_selected).map(|a| a.name.clone())
            }
        });
        if let Err(e) = self.send_to(
            &host,
            crate::net::transport::OutgoingReq::WorkspaceCreate {
                label: label.clone(),
                project_root: path.clone(),
                autostart_claude: agent == "claude",
                agent: agent.to_string(),
                account,
            },
        ) {
            tracing::warn!(error = %e, %host, %label, %path, "drop workspace.create — channel closed");
            self.status = "create failed · channel closed".to_string();
            return;
        }
        // LU6a design-review amendment: the "since request" clock the
        // eventual capsule attach's log lines report starts HERE for a
        // create — the daemon round trip to actually create the
        // workspace is part of the user-perceived latency, not just the
        // attach that follows once `WorkspaceCreated` lands.
        // `attach_session_to_bl` (always reached via `switch_to_workspace`
        // from that reply) takes this, not merely reads it, so a stale
        // value can't leak into a later, unrelated switch.
        self.pending_capsule_create_requested_at = Some(std::time::Instant::now());
        // Status line reflects which Enter the user pressed (ADR 0031):
        // Enter=claude workspace · Shift+Enter=bare · Ctrl+Enter=codex.
        let kind = match agent {
            "claude" => "workspace",
            "codex" => "codex workspace",
            _ => "bare session",
        };
        self.status = format!("create {kind} · '{label}' @ {path} (registering…)");
        self.window.request_redraw();
    }

    /// Cancel the picker without creating anything.
    pub(in crate::ui) fn picker_cancel(&mut self) {
        if self.workspace_picker.is_some() {
            self.workspace_picker = None;
            self.status = "create cancelled".to_string();
            self.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Workspace-create picker root: a Windows FE with no `$HOME` in its
    // process env must not seed the picker (and hence `workspace.create`'s
    // `project_root`) with a driveless `/` — see `picker_local_home_fallback`.

    /// Field defect (2026-09-05): on a Windows box, "+ create new" under
    /// the LOCAL host started the picker at the remote backend's Linux home.
    #[test]
    fn local_host_ignores_a_configured_root_that_does_not_exist_here() {
        let start = picker_start_for_host(
            "local",
            Some(r"C:\Users\u"),
            Some("/home/u/dev"),
            Some("/home/u"),
            "C:/fe-home".to_string(),
            |_| false,
        );
        assert_eq!(start, r"C:\Users\u");
    }

    #[test]
    fn local_host_honours_a_configured_root_that_exists_here() {
        let start = picker_start_for_host(
            "local",
            Some(r"C:\Users\u"),
            Some(r"C:\Users\u\dev"),
            None,
            "C:/fe-home".to_string(),
            |p| p.ends_with("dev"),
        );
        assert_eq!(start, r"C:\Users\u\dev");
    }

    #[test]
    fn local_host_falls_back_to_the_frontends_home_without_a_default_row() {
        let start = picker_start_for_host("local", None, None, Some("/home/u"), "C:/fe-home".to_string(), |_| false);
        assert_eq!(start, "C:/fe-home");
    }

    #[test]
    fn remote_host_keeps_configured_then_remote_home_then_default_row() {
        let cfg = picker_start_for_host("host-4", Some("/home/u"), Some("/home/u/dev"), Some("/home/u"), "C:/fe".into(), |_| false);
        assert_eq!(cfg, "/home/u/dev");
        let home = picker_start_for_host("host-4", Some("/home/u"), None, Some("/home/remote"), "C:/fe".into(), |_| false);
        assert_eq!(home, "/home/remote");
        let row = picker_start_for_host("host-4", Some("/home/u"), None, None, "C:/fe".into(), |_| false);
        assert_eq!(row, "/home/u");
        let last = picker_start_for_host("host-4", None, None, None, "C:/fe".into(), |_| false);
        assert_eq!(last, "C:/fe");
    }

    #[test]
    fn picker_local_home_fallback_prefers_env_home() {
        assert_eq!(
            picker_local_home_fallback(
                Some("/home/u/explicit".to_string()),
                Some(PathBuf::from(r"C:\Users\u"))
            ),
            "/home/u/explicit"
        );
    }

    #[test]
    fn picker_local_home_fallback_uses_os_home_over_bare_root() {
        // This is the regression: before the fix, an absent `$HOME` fell
        // straight to "/" even when the OS could report a real home dir.
        assert_eq!(
            picker_local_home_fallback(None, Some(PathBuf::from("/home/u"))),
            "/home/u"
        );
    }

    #[test]
    fn picker_local_home_fallback_falls_back_to_root_when_nothing_resolves() {
        assert_eq!(picker_local_home_fallback(None, None), "/");
    }

    #[test]
    #[cfg(windows)]
    fn picker_local_home_fallback_keeps_drive_letter_backslash_form() {
        assert_eq!(
            picker_local_home_fallback(None, Some(PathBuf::from(r"C:\Users\u\HomeLab\r"))),
            r"C:\Users\u\HomeLab\r"
        );
    }

    #[test]
    #[cfg(windows)]
    fn picker_local_home_fallback_keeps_drive_letter_forward_slash_form() {
        assert_eq!(
            picker_local_home_fallback(None, Some(PathBuf::from("C:/Users/u/HomeLab/r"))),
            "C:/Users/u/HomeLab/r"
        );
    }

    fn account(name: &str, kinds: &[&str], logged_in: &[(&str, bool)]) -> crate::net::transport::AccountInfo {
        crate::net::transport::AccountInfo {
            name: name.to_string(),
            kinds: kinds.iter().map(|s| s.to_string()).collect(),
            logged_in: logged_in.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    fn picker_with_accounts(accounts: Vec<crate::net::transport::AccountInfo>) -> WorkspacePicker {
        WorkspacePicker {
            host: "local".to_string(),
            show_hidden: true,
            current_path: "/tmp".to_string(),
            entries: Vec::new(),
            selected: 0,
            reveal: None,
            accounts,
            account_selected: 0,
        }
    }

    fn picker_dir(path: &str) -> crate::net::transport::DirEntry {
        crate::net::transport::DirEntry {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            has_children: true,
        }
    }

    /// Going back up with Left returns you to where you went in: the
    /// directory just left is an entry of the parent, and the cursor lands
    /// on it instead of the top.
    #[test]
    fn picker_listing_lands_the_cursor_on_the_directory_left_behind() {
        let mut p = picker_with_accounts(Vec::new());
        p.reveal = Some("c".to_string());
        p.land_listing(vec![picker_dir("/tmp/a"), picker_dir("/tmp/b"), picker_dir("/tmp/c")]);
        assert_eq!(p.selected, 2);
        assert_eq!(p.reveal, None, "consumed by the listing that answered");
    }

    /// The remembered entry is not in the listing (a dot-directory, with
    /// hidden folders now off): the top, never a stale index.
    #[test]
    fn picker_listing_falls_back_to_the_top_when_the_entry_is_gone() {
        let mut p = picker_with_accounts(Vec::new());
        p.reveal = Some(".hidden".to_string());
        p.land_listing(vec![picker_dir("/tmp/a"), picker_dir("/tmp/b")]);
        assert_eq!(p.selected, 0);
    }

    /// Nothing remembered: an in-range cursor stays put, and one the
    /// listing no longer reaches goes to the top.
    #[test]
    fn picker_listing_keeps_an_in_range_cursor_and_clamps_the_rest() {
        let mut p = picker_with_accounts(Vec::new());
        p.selected = 1;
        p.land_listing(vec![picker_dir("/tmp/a"), picker_dir("/tmp/b")]);
        assert_eq!(p.selected, 1);
        p.selected = 5;
        p.land_listing(vec![picker_dir("/tmp/a")]);
        assert_eq!(p.selected, 0);
    }

    /// The new-session prompt's account list handling (owner-simplified
    /// brief, 2026-09-15): default-only hides the field.
    #[test]
    fn account_choice_hidden_when_only_default() {
        let p = picker_with_accounts(vec![account("default", &["claude"], &[("claude", true)])]);
        assert!(!p.account_choice_visible());
    }

    /// An old daemon with no `accounts.list` handler (or one whose reply
    /// fails to parse) surfaces as an empty accounts list — same hidden
    /// treatment as default-only, no error.
    #[test]
    fn account_choice_hidden_when_daemon_never_answered() {
        let p = picker_with_accounts(Vec::new());
        assert!(!p.account_choice_visible());
    }

    #[test]
    fn account_choice_visible_with_a_second_declared_account() {
        let p = picker_with_accounts(vec![
            account("default", &["claude"], &[("claude", true)]),
            account("team", &["claude"], &[("claude", true)]),
        ]);
        assert!(p.account_choice_visible());
    }

    /// Not-logged-in entries are marked, but still selectable — a
    /// never-logged-in folder is a NORMAL choice (owner ruling): the
    /// row's own pane runs the login on first start.
    #[test]
    fn account_not_logged_in_is_marked_and_still_selectable() {
        let logged_out = account("team", &["claude"], &[("claude", false)]);
        assert!(!logged_out.any_logged_in());
        let logged_in = account("default", &["claude"], &[("claude", true)]);
        assert!(logged_in.any_logged_in());
        // Cycling never skips a not-logged-in entry — it's a full member
        // of the rotation, just annotated.
        let mut p = picker_with_accounts(vec![logged_in, logged_out]);
        assert_eq!(p.account_selected, 0);
        p.account_selected = (p.account_selected + 1) % p.accounts.len();
        assert_eq!(p.account_selected, 1);
        assert!(!p.accounts[p.account_selected].any_logged_in());
    }
}
