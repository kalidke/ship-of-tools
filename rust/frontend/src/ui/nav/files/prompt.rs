//! Ctrl+N and Ctrl+D name prompts in the Files tree: the prompt state, the new-entry id and the confirm paths.

use super::*;
/// A one-line modal prompt that floats over the NavTree and steals
/// keystrokes while it's `Some` (mirrors how `WorkspacePicker` and
/// `EditState` intercept keys). Each variant carries the context the
/// confirm path needs. The enum is the extension point: new nav-pane
/// modals add a variant here and a match arm in the key handler +
/// renderer, without touching the surrounding nav code.
#[derive(Clone)]
pub(in crate::ui) enum NavPrompt {
    /// Ctrl+N in Files mode: type the name of a new file, or — with a
    /// trailing `/` — a new directory, to create in `dir_node_id`. Enter
    /// confirms (validates + fires `file.write` with empty content, or
    /// `dir.create` when the name ends with `/`), Esc cancels. `input` is
    /// the live name buffer.
    CreateFile {
        /// `files:`-prefixed id of the directory that will contain the new
        /// entry (`files:` for the project root). The new entry's id is
        /// `build_new_file_node_id(dir_node_id, input)` after stripping any
        /// trailing `/`.
        dir_node_id: String,
        /// Live name buffer, rendered after `new file or dir/: ` on the
        /// status line. `nav_prompt_push_char` guarantees any `/` in here
        /// is exactly one trailing character.
        input: String,
    },
    /// Ctrl+D in Files mode: confirm trashing the cursored file. `y`/`Y`
    /// fires `file.delete` for `node_id`; `n`/`N`/Esc/any other key cancels.
    /// No text input — it's a y/N gate. `label` is the file's display name,
    /// shown in the `delete <label>? [y/N]` status line.
    ConfirmDelete {
        /// `files:`-prefixed id of the file to delete.
        node_id: String,
        /// Display label of the row, echoed in the confirm prompt.
        label: String,
    },
    /// Ctrl+Q in navigation focus: ask whether to keep the daemon and its
    /// sessions running. `keep` is the highlighted answer (No by default);
    /// Tab flips the choice, Enter confirms by key identity, every other non-repeat key cancels, and repeats are ignored.
    ConfirmQuit { keep: bool },
    /// Ctrl+S on a raster that carries NO physical scale (ADR 0034 §4 live
    /// entry): type the pixel size in MICRONS. Enter validates + fires
    /// `preview.set_scale` (which persists the sidecar and returns the
    /// re-rendered preview), Esc cancels.
    ///
    /// Microns because that's the maintainer's standard unit for entry; the
    /// value is converted to nm at the boundary so the wire and the on-disk
    /// sidecar stay in nm (ADR 0034 §2). The typed number is the RAW/original
    /// pixel size — never the served/downsampled one — so it is sent verbatim.
    ScaleEntry {
        /// `files:`-prefixed id of the previewed raster being calibrated.
        node_id: String,
        /// Live nm-per-pixel buffer, rendered after `pixel size (nm): `.
        input: String,
    },
}

/// Outcome of a Ctrl+N create round-trip, as `finish_pending_create` needs
/// it: `FileWriteResult`'s `Ok`/`Conflict`/`Error` and `DirCreateResult`'s
/// `Ok`/`Error` collapse onto these three cases — a conflicting `file.write`
/// and an `already_exists` `dir.create` both mean the same thing here (the
/// name existed on disk already), so callers fold them onto the same variant
/// rather than `finish_pending_create` re-deriving it from a `code` string.
pub(in crate::ui) enum CreateOutcome<'a> {
    Ok,
    AlreadyExists,
    Error(&'a str),
}

/// Build + validate the `files:` node id for a new file (or, after the
/// caller strips a trailing `/`, a new directory) named `name`, created
/// inside the directory `dir_node_id`. The backend's `node_id_to_path`
/// rejects absolute ids and `..` segments, so the only safe child id is
/// `<dir_id>/<name>` with a bare name — hence the name must not contain a path
/// separator. The root dir id is `files:` (trailing colon, no segment), so we
/// suppress the joining `/` in that case: `files:` + `a.txt` → `files:a.txt`;
/// `files:sub` + `a.txt` → `files:sub/a.txt`.
///
/// Returns `Err(reason)` for an empty name or one containing `/` or `\`; the
/// caller surfaces the reason on the status line and keeps the prompt open.
/// Collision against an existing sibling is checked separately by the caller
/// (it needs the live tree rows); this helper is pure so it stays testable.
fn build_new_file_node_id(dir_node_id: &str, name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name is empty");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("name must not contain a path separator");
    }
    // `files:` already ends with the prefix's colon — no separator needed at
    // the root; any deeper dir id gets a `/` before the bare name.
    let sep = if dir_node_id.ends_with(':') { "" } else { "/" };
    Ok(format!("{dir_node_id}{sep}{name}"))
}

/// Whether typing `c` onto the live Ctrl+N prompt buffer `buf` is allowed.
/// `\` is never a name character here. `/` is accepted only as a single
/// TRAILING character — it's the "make this a directory" marker that
/// `split_create_name` strips before validating the rest of the name via
/// `build_new_file_node_id` — so a second `/`, or any character typed after
/// one, is refused. Pure so the invariant ("at most one `/`, and only at the
/// end") is testable without a live prompt buffer.
fn nav_prompt_name_char_allowed(buf: &str, c: char) -> bool {
    if c == '\\' {
        return false;
    }
    if c == '/' {
        return !buf.is_empty() && !buf.ends_with('/');
    }
    !buf.ends_with('/')
}

/// Splits the Ctrl+N prompt's trimmed input into "is this a directory" and
/// the bare name to hand `build_new_file_node_id`. A single trailing `/` —
/// `nav_prompt_name_char_allowed` guarantees the buffer can carry at most
/// one, and only trailing — marks a directory and is stripped; anything
/// else is a file name, unchanged.
fn split_create_name(name: &str) -> (bool, String) {
    match name.strip_suffix('/') {
        Some(bare) => (true, bare.to_string()),
        None => (false, name.to_string()),
    }
}

/// Whether a Files-mode tree node is a directory for delete-refusal purposes.
/// Mirrors the dir test used by upload/download/create: `kind == "dir"`, plus
/// the files root (`files:`, which has no segment) is itself a directory.
/// `file.delete` refuses directories in v1, so the FE pre-refuses them before
/// even opening the confirm prompt. Pure so the refusal is unit-testable.
fn is_directory_row(node: &TreeNode) -> bool {
    node.kind == "dir" || node.id == "files:"
}

impl State {
    /// Open the Ctrl+N new-file-or-folder prompt (Files mode only). Computes
    /// the directory the entry should land in from the cursored row: a
    /// directory row contains it directly (use its id); a file row's sibling
    /// is the new entry (use the file's parent dir id); the root falls back
    /// to `files:`. Returns false (no-op) when the cursor isn't on a `files:`
    /// row — the caller leaves the keystroke for the normal nav handler.
    pub(in crate::ui) fn begin_create_file(&mut self) -> bool {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return false;
        };
        if !row.node.id.starts_with("files:") {
            return false;
        }
        // A directory row contains the new entry directly; a non-directory
        // row (file / other) is a sibling, so the entry goes in its parent
        // dir. The files root (`files:`) is itself a directory.
        let dir_node_id = if row.node.kind == "dir" || row.node.id == "files:" {
            row.node.id.clone()
        } else {
            parent_files_node_id(&row.node.id)
        };
        self.nav_prompt = Some(NavPrompt::CreateFile {
            dir_node_id,
            input: String::new(),
        });
        self.status = "new file or dir/: ".to_string();
        self.window.request_redraw();
        true
    }

    /// Append a typed character to the active new-file-or-folder prompt's
    /// name buffer, gated by `nav_prompt_name_char_allowed` (see there for
    /// the trailing-`/`-only rule). Everything it allows is validated again
    /// on Enter by `build_new_file_node_id`.
    pub(in crate::ui) fn nav_prompt_push_char(&mut self, c: char) {
        match self.nav_prompt.as_mut() {
            Some(NavPrompt::CreateFile { input, .. }) => {
                if !nav_prompt_name_char_allowed(input, c) {
                    return;
                }
                input.push(c);
                self.window.request_redraw();
            }
            // Scale entry is a NUMBER: filter at the source (as CreateFile does
            // for separators) so only digits and a single decimal point can be
            // typed. `parse_nm_pixel_size` still validates on Enter — this
            // just stops obvious junk from ever entering the buffer.
            Some(NavPrompt::ScaleEntry { input, .. }) => {
                if c.is_ascii_digit() || (c == '.' && !input.contains('.')) {
                    input.push(c);
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    /// Backspace one char off whichever text prompt is active.
    pub(in crate::ui) fn nav_prompt_backspace(&mut self) {
        match self.nav_prompt.as_mut() {
            Some(NavPrompt::CreateFile { input, .. })
            | Some(NavPrompt::ScaleEntry { input, .. }) => {
                input.pop();
                self.window.request_redraw();
            }
            _ => {}
        }
    }

    /// Confirm the new-file-or-folder prompt: validate the name + check for a
    /// sibling collision, then fire a zero-byte `file.write` for the new id
    /// — or, when the typed name ends with `/`, a `dir.create` for the name
    /// with that slash stripped. On an invalid name or collision, surface a
    /// status message and keep the prompt open (nothing is sent). On
    /// success, remember the id so the reply can refresh the dir listing,
    /// close the prompt, and show a "creating …" status.
    pub(in crate::ui) fn confirm_create_file(&mut self) {
        let (dir_node_id, name) = match self.nav_prompt.as_ref() {
            Some(NavPrompt::CreateFile { dir_node_id, input }) => {
                (dir_node_id.clone(), input.trim().to_string())
            }
            _ => return,
        };
        let (is_dir, bare_name) = split_create_name(&name);
        let kind = if is_dir { "new dir" } else { "new file" };
        let new_id = match build_new_file_node_id(&dir_node_id, &bare_name) {
            Ok(id) => id,
            Err(reason) => {
                self.status = format!("{kind} · {reason}");
                self.window.request_redraw();
                return;
            }
        };
        // Sibling-collision guard: refuse if any existing row already carries
        // the would-be id (a file or dir of that name already lives here).
        if self.tree.rows.iter().any(|r| r.node.id == new_id) {
            self.status = format!("{kind} · '{bare_name}' already exists");
            self.window.request_redraw();
            return;
        }
        let req = if is_dir {
            OutgoingReq::DirCreate {
                node_id: new_id.clone(),
                workspace_id: self.active_workspace_id.clone(),
            }
        } else {
            OutgoingReq::FileWrite {
                node_id: new_id.clone(),
                content: String::new(),
                expected_version: None,
                workspace_id: self.active_workspace_id.clone(),
            }
        };
        if let Err(e) = self.send(req) {
            tracing::warn!(error = %e, %new_id, is_dir, "drop {kind} — channel closed");
            self.status = format!("{kind} · channel closed");
            self.window.request_redraw();
            return;
        }
        tracing::info!(%new_id, is_dir, "navtree.create → {kind}");
        self.pending_created_node_id = Some(new_id);
        self.nav_prompt = None;
        self.status = format!("creating {bare_name}…");
        self.window.request_redraw();
    }

    /// The shared tail of a Ctrl+N create round-trip: does nothing unless
    /// `node_id` matches the pending create (a late reply for an
    /// abandoned/superseded request is ignored), otherwise clears the
    /// pending marker and reports `outcome` on the status line. Called from
    /// both `file.write`'s and `dir.create`'s reply handling —
    /// `FileWriteResult`'s `Conflict` and `DirCreateResult`'s
    /// `already_exists` error both collapse to `AlreadyExists` here, since
    /// both mean "the name existed on disk already".
    pub(in crate::ui) fn finish_pending_create(&mut self, node_id: &str, outcome: CreateOutcome) {
        if self.pending_created_node_id.as_deref() != Some(node_id) {
            return;
        }
        self.pending_created_node_id = None;
        match outcome {
            CreateOutcome::Ok => {
                // Re-list the parent dir so the new entry appears without a
                // manual re-expand — same tree.children refresh the
                // delete/upload paths use.
                let parent = parent_files_node_id(node_id);
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeChildren {
                    parent_id: parent,
                    workspace_id: self.active_workspace_id.clone(),
                }) {
                    tracing::warn!(error = %e, "drop post-create tree.children refresh");
                }
                let name = node_id.rsplit(['/', ':']).next().unwrap_or(node_id);
                self.status = format!("created · {name}");
            }
            CreateOutcome::AlreadyExists => {
                self.status = "new · already exists on disk".to_string();
            }
            CreateOutcome::Error(message) => {
                self.status = format!("new failed · {message}");
            }
        }
        self.window.request_redraw();
    }

    /// Open the Ctrl+D delete-confirm prompt (Files mode only). Targets the
    /// cursored row when it's a `files:` file. Pre-refuses directories in v1
    /// (the backend rejects them with `is_directory`; we don't even open the
    /// prompt) and surfaces a status message instead. Returns false (no-op)
    /// when the cursor isn't on a deletable `files:` file row — the caller
    /// leaves the keystroke for the normal nav handler.
    pub(in crate::ui) fn begin_delete_file(&mut self) -> bool {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return false;
        };
        if !row.node.id.starts_with("files:") {
            return false;
        }
        if is_directory_row(&row.node) {
            self.status = "delete: directories not supported yet".to_string();
            self.window.request_redraw();
            return false;
        }
        self.nav_prompt = Some(NavPrompt::ConfirmDelete {
            node_id: row.node.id.clone(),
            label: row.node.label.clone(),
        });
        self.status = format!("delete {}? [y/N]", row.node.label);
        self.window.request_redraw();
        true
    }

    /// Confirm the delete prompt (`y`/`Y`): fire `file.delete` for the
    /// node id, remember it so the reply can refresh the dir listing, close
    /// the prompt, and show a "deleting …" status. On a closed channel,
    /// surface it and leave nothing pending.
    pub(in crate::ui) fn confirm_delete_file(&mut self) {
        let (node_id, label) = match self.nav_prompt.as_ref() {
            Some(NavPrompt::ConfirmDelete { node_id, label }) => (node_id.clone(), label.clone()),
            _ => return,
        };
        if let Err(e) = self.send(OutgoingReq::FileDelete {
            node_id: node_id.clone(),
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, %node_id, "drop file.delete — channel closed");
            self.status = "delete · channel closed".to_string();
            self.nav_prompt = None;
            self.window.request_redraw();
            return;
        }
        tracing::info!(%node_id, "navtree.delete_file → file.delete");
        self.pending_deleted_node_id = Some(node_id);
        self.nav_prompt = None;
        self.status = format!("deleting {label}…");
        self.window.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_new_file_node_id_joins_and_validates() {
        // Root dir (`files:`) → no separator before the bare name.
        assert_eq!(
            build_new_file_node_id("files:", "a.txt"),
            Ok("files:a.txt".to_string())
        );
        // Sub-directory → joined with a single `/`.
        assert_eq!(
            build_new_file_node_id("files:sub", "a.txt"),
            Ok("files:sub/a.txt".to_string())
        );
        assert_eq!(
            build_new_file_node_id("files:a/b", "c.jl"),
            Ok("files:a/b/c.jl".to_string())
        );
        // Surrounding whitespace is trimmed off the name.
        assert_eq!(
            build_new_file_node_id("files:sub", "  spaced.txt  "),
            Ok("files:sub/spaced.txt".to_string())
        );
        // Empty / whitespace-only names are rejected.
        assert!(build_new_file_node_id("files:", "").is_err());
        assert!(build_new_file_node_id("files:", "   ").is_err());
        // Path separators are rejected (the backend only accepts a bare
        // child segment — no nested-dir creation, no `..` traversal).
        assert!(build_new_file_node_id("files:", "a/b.txt").is_err());
        assert!(build_new_file_node_id("files:", "a\\b.txt").is_err());
        assert!(build_new_file_node_id("files:sub", "../escape.txt").is_err());
    }

    #[test]
    fn nav_prompt_name_char_allowed_takes_one_trailing_slash_only() {
        // `\` is never a name character.
        assert!(!nav_prompt_name_char_allowed("", '\\'));
        assert!(!nav_prompt_name_char_allowed("sub", '\\'));
        // A leading `/` on an empty buffer is refused (a directory still
        // needs a name).
        assert!(!nav_prompt_name_char_allowed("", '/'));
        // Ordinary chars are always fine on an empty or plain buffer.
        assert!(nav_prompt_name_char_allowed("", 's'));
        assert!(nav_prompt_name_char_allowed("sub", 'x'));
        // A single trailing `/` is accepted once the buffer is non-empty.
        assert!(nav_prompt_name_char_allowed("sub", '/'));
        // Once the buffer ends with `/`, nothing more is accepted — neither
        // another `/` (no double slash) nor an ordinary char (no embedded
        // separator via "sub/" + "x" → "sub/x").
        assert!(!nav_prompt_name_char_allowed("sub/", '/'));
        assert!(!nav_prompt_name_char_allowed("sub/", 'x'));
    }

    #[test]
    fn split_create_name_takes_a_trailing_slash_as_a_directory_marker() {
        assert_eq!(split_create_name("a.txt"), (false, "a.txt".to_string()));
        assert_eq!(split_create_name("sub/"), (true, "sub".to_string()));
        // Chained through the id builder, this is what actually reaches the
        // wire in `OutgoingReq::DirCreate`: "sub/" typed under `files:x`
        // becomes a create for `files:x/sub`.
        let (is_dir, bare) = split_create_name("sub/");
        assert!(is_dir);
        assert_eq!(
            build_new_file_node_id("files:x", &bare),
            Ok("files:x/sub".to_string())
        );
    }

    #[test]
    fn new_file_collision_detected_against_existing_sibling() {
        // Mirror the confirm-path collision guard: scan the flat rows for the
        // would-be new id. A sibling `a.txt` already under `files:sub` means
        // creating another `a.txt` there collides; `b.txt` does not.
        let mut t = TreeView::new();
        t.set_root(
            node("files:sub", "sub", true),
            vec![node("files:sub/a.txt", "a.txt", false)],
        );
        let collide = build_new_file_node_id("files:sub", "a.txt").unwrap();
        let fresh = build_new_file_node_id("files:sub", "b.txt").unwrap();
        assert!(t.rows.iter().any(|r| r.node.id == collide));
        assert!(!t.rows.iter().any(|r| r.node.id == fresh));
    }

    #[test]
    fn delete_refuses_directory_rows() {
        // Mirror the Ctrl+N tests: `begin_delete_file` pre-refuses dirs in v1,
        // so its dir test (`is_directory_row`) must reject a `dir`-kind node
        // and the files root, while accepting an ordinary file row. A directory
        // row (kind "dir").
        let dir = TreeNode {
            id: "files:sub".to_string(),
            label: "sub".to_string(),
            kind: "dir".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: Default::default(),
        };
        assert!(is_directory_row(&dir), "kind == dir is a directory");
        // The files root is itself a directory even without the "dir" kind.
        let root = TreeNode {
            id: "files:".to_string(),
            label: "/".to_string(),
            kind: "files".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: Default::default(),
        };
        assert!(is_directory_row(&root), "files: root is a directory");
        // An ordinary file row is deletable (not a directory). `node` builds a
        // file row (kind "files", no children).
        let file = node("files:sub/a.txt", "a.txt", false);
        assert!(!is_directory_row(&file), "a file row is deletable");
    }
}
