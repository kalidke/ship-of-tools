//! The `State` methods that drive the new-session picker to `workspace.create`.

use super::*;
use super::picker::{picker_local_home_fallback, picker_start_for_host, selected_account};

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
            .row_directory
            .rows(&host)
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
        self.workspace_picker = Some(WorkspacePicker::open(host.clone(), start.clone()));
        if let Err(e) = self.send_to(
            &host,
            crate::net::transport::OutgoingReq::DirectoryList {
                path: start.clone(),
                include_hidden: true,
            },
        ) {
            tracing::warn!(target: "sot::ui::session::picker", error = %e, %host, %start, "drop initial directory.list — channel closed");
        }
        // Per-session accounts: ask
        // the daemon that will OWN the row, never the frontend's own disk
        // — the login must exist where the session runs. An old daemon's
        // reply parses to an empty list (see `PendingKind::AccountsList`),
        // which keeps the choice hidden exactly like a fresh daemon that
        // only reports "default".
        if let Err(e) = self.send_to(&host, crate::net::transport::OutgoingReq::AccountsList) {
            tracing::warn!(target: "sot::ui::session::picker", error = %e, %host, "drop initial accounts.list — channel closed");
        }
        self.status.set(format!("create workspace · {host} · picker @ {start}"));
        self.window.request_redraw();
    }

    /// Move the picker's cursor up by one row (saturating).
    pub(in crate::ui) fn picker_cursor_up(&mut self) {
        if let Some(p) = self.workspace_picker.as_mut() {
            p.cursor_up();
            self.window.request_redraw();
        }
    }

    /// Move the picker's cursor down by one row (clamped to entries
    /// length). Zero-entry directories pin the cursor at 0.
    pub(in crate::ui) fn picker_cursor_down(&mut self) {
        if let Some(p) = self.workspace_picker.as_mut() {
            p.cursor_down();
            self.window.request_redraw();
        }
    }

    /// Drill into the cursored directory: re-fire `directory.list` for
    /// its path and clear the entry list pending the response. Updates
    /// `current_path` immediately so the title reflects where the user
    /// is going even before the listing lands.
    pub(in crate::ui) fn picker_drill_in(&mut self) {
        let Some(p) = self.workspace_picker.as_mut() else {
            return;
        };
        let host = p.host().clone();
        if let Some(path) = p.drill_in() {
            let include_hidden = self.workspace_picker.as_ref().is_some_and(|p| p.show_hidden());
            if let Err(e) = self.send_to(
                &host,
                crate::net::transport::OutgoingReq::DirectoryList { path: path.clone(), include_hidden },
            ) {
                tracing::warn!(target: "sot::ui::session::picker", error = %e, %path, "drop directory.list (drill-in)");
            }
            self.status.set(format!("picker · {path}"));
            self.window.request_redraw();
        }
    }

    /// Ascend to the parent of the picker's `current_path`. Re-fires
    /// the listing so the parent's entries populate.
    pub(in crate::ui) fn picker_ascend(&mut self) {
        let Some(p) = self.workspace_picker.as_mut() else {
            return;
        };
        let host = p.host().clone();
        // The directory being left is an entry of its parent: the cursor
        // lands on it when the parent's listing arrives.
        if let Some(path) = p.ascend() {
            let include_hidden = self.workspace_picker.as_ref().is_some_and(|p| p.show_hidden());
            if let Err(e) = self.send_to(
                &host,
                crate::net::transport::OutgoingReq::DirectoryList { path: path.clone(), include_hidden },
            ) {
                tracing::warn!(target: "sot::ui::session::picker", error = %e, %path, "drop directory.list (ascend)");
            }
            self.status.set(format!("picker · {path}"));
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
        let path = p.toggle_hidden();
        let (host, include_hidden) = (p.host().clone(), p.show_hidden());
        if let Err(e) = self.send_to(
            &host,
            crate::net::transport::OutgoingReq::DirectoryList { path: path.clone(), include_hidden },
        ) {
            tracing::warn!(target: "sot::ui::session::picker", error = %e, %path, "drop directory.list (toggle hidden)");
        }
        self.status.set(format!(
            "picker · {path} · hidden folders {}",
            if include_hidden { "shown" } else { "hidden" }
        ));
        self.window.request_redraw();
    }

    /// `Tab` in the picker: cycle to the next account. No-op when `accounts` has 0 or
    /// 1 entries (the choice is hidden — either only "default" exists, or the daemon
    /// never answered `accounts.list`).
    pub(in crate::ui) fn picker_cycle_account(&mut self) {
        let Some(acct) = self.workspace_picker.as_mut().and_then(WorkspacePicker::cycle_account) else {
            return;
        };
        let suffix = if acct.any_logged_in() { "" } else { " (not logged in)" };
        self.status.set(format!("picker · account: {}{suffix}", acct.name));
        self.window.request_redraw();
    }

    pub(in crate::ui) fn picker_confirm_selected(&mut self, agent: &str) {
        let Some(path) = self.workspace_picker.as_ref().map(WorkspacePicker::chosen_path) else {
            return;
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
            .map(|p| p.host().clone())
            .unwrap_or_else(|| self.selected.host().clone());
        let label = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "workspace".to_string());
        // Only the selected name default means the daemon's default account.
        let account = self.workspace_picker.as_ref().and_then(selected_account);
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
            tracing::warn!(target: "sot::ui::session::picker", error = %e, %host, %label, %path, "drop workspace.create — channel closed");
            self.status.set("create failed · channel closed".to_string());
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
        self.selected.note_create_requested(std::time::Instant::now());
        // Status line reflects which Enter the user pressed (ADR 0031):
        // Enter=claude workspace · Shift+Enter=bare · Ctrl+Enter=codex.
        let kind = match agent {
            "claude" => "workspace",
            "codex" => "codex workspace",
            _ => "bare session",
        };
        self.status.set(format!("create {kind} · '{label}' @ {path} (registering…)"));
        self.window.request_redraw();
    }

    /// Cancel the picker without creating anything.
    pub(in crate::ui) fn picker_cancel(&mut self) {
        if self.workspace_picker.is_some() {
            self.workspace_picker = None;
            self.status.set("create cancelled".to_string());
            self.window.request_redraw();
        }
    }
}
