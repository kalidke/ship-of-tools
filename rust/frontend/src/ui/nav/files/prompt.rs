//! Files prompt admission, validation, sends and pending operations. FocusPrompts owns editing and storage.

use super::*;
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

/// Splits the Ctrl+N prompt's trimmed input into "is this a directory" and
/// the bare name to hand `build_new_file_node_id`. A single trailing `/` —
/// `FocusPrompts::push_char` guarantees the buffer can carry at most
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
        let Some(row) = self.nav.tree().rows().get(self.nav.tree().selected()) else {
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
        let effect = self.focus_prompts.open_create(dir_node_id);
        self.apply_prompt_effect(effect);
        true
    }

    /// Edit through the owner's variant-specific input rules.
    pub(in crate::ui) fn nav_prompt_push_char(&mut self, c: char) {
        if self.focus_prompts.push_char(c) { self.window.request_redraw(); }
    }

    /// Backspace redraws even an empty text prompt.
    pub(in crate::ui) fn nav_prompt_backspace(&mut self) {
        if self.focus_prompts.backspace() { self.window.request_redraw(); }
    }

    /// Confirm the new-file-or-folder prompt: validate the name + check for a
    /// sibling collision, then fire a zero-byte `file.write` for the new id
    /// — or, when the typed name ends with `/`, a `dir.create` for the name
    /// with that slash stripped. On an invalid name or collision, surface a
    /// status message and keep the prompt open (nothing is sent). On
    /// success, remember the id so the reply can refresh the dir listing,
    /// close the prompt, and show a "creating …" status.
    pub(in crate::ui) fn confirm_create_file(&mut self) {
        confirm_create(self);
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
        if !self.file_ops.settle_created(node_id) {
            return;
        }
        match outcome {
            CreateOutcome::Ok => {
                // Re-list the parent dir so the new entry appears without a
                // manual re-expand — same tree.children refresh the
                // delete/upload paths use.
                let parent = parent_files_node_id(node_id);
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeChildren {
                    parent_id: parent,
                    workspace_id: self.selected.workspace_id().clone(),
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
        let Some(row) = self.nav.tree().rows().get(self.nav.tree().selected()) else {
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
        let effect = self.focus_prompts.open_delete(row.node.id.clone(), row.node.label.clone());
        self.apply_prompt_effect(effect);
        true
    }

    /// Confirm the delete prompt (`y`/`Y`): fire `file.delete` for the
    /// node id, remember it so the reply can refresh the dir listing, close
    /// the prompt, and show a "deleting …" status. On a closed channel,
    /// surface it and leave nothing pending.
    pub(in crate::ui) fn confirm_delete_file(&mut self) {
        confirm_delete(self);
    }
}

/// Production confirmation port; drafts are owned before callbacks.
pub(in crate::ui) trait FilePromptContext {
    fn prompts(&mut self) -> &mut FocusPrompts;
    fn file_exists(&self, node_id: &str) -> bool;
    fn send_create(&mut self, node_id: &str, is_dir: bool) -> Result<(), String>;
    fn send_delete(&mut self, node_id: &str) -> Result<(), String>;
    fn mark_created(&mut self, node_id: String);
    fn mark_deleted(&mut self, node_id: String);
    fn prompt_effect(&mut self, effect: PromptEffect);
}

pub(in crate::ui) fn confirm_create(context: &mut impl FilePromptContext) {
    let Some((dir_node_id, name)) = context.prompts().create_draft() else { return; };
    let (is_dir, bare_name) = split_create_name(&name);
    let kind = if is_dir { "new dir" } else { "new file" };
    let new_id = match build_new_file_node_id(&dir_node_id, &bare_name) {
        Ok(id) => id,
        Err(reason) => {
            context.prompt_effect(PromptEffect::message(format!("{kind} · {reason}")));
            return;
        }
    };
    if context.file_exists(&new_id) {
        context.prompt_effect(PromptEffect::message(format!("{kind} · '{bare_name}' already exists")));
        return;
    }
    if let Err(e) = context.send_create(&new_id, is_dir) {
        tracing::warn!(error = %e, %new_id, is_dir, "drop {kind} — channel closed");
        context.prompt_effect(PromptEffect::message(format!("{kind} · channel closed")));
        return;
    }
    tracing::info!(%new_id, is_dir, "navtree.create → {kind}");
    context.mark_created(new_id);
    context.prompts().complete_create(true);
    context.prompt_effect(PromptEffect::message(format!("creating {bare_name}…")));
}

pub(in crate::ui) fn confirm_delete(context: &mut impl FilePromptContext) {
    let Some((node_id, label)) = context.prompts().delete_target() else { return; };
    if let Err(e) = context.send_delete(&node_id) {
        tracing::warn!(error = %e, %node_id, "drop file.delete — channel closed");
        context.prompts().complete_delete();
        context.prompt_effect(PromptEffect::message("delete · channel closed"));
        return;
    }
    tracing::info!(%node_id, "navtree.delete_file → file.delete");
    context.mark_deleted(node_id);
    context.prompts().complete_delete();
    context.prompt_effect(PromptEffect::message(format!("deleting {label}…")));
}

impl FilePromptContext for State {
    fn prompts(&mut self) -> &mut FocusPrompts { &mut self.focus_prompts }
    fn file_exists(&self, node_id: &str) -> bool { self.nav.tree().rows().iter().any(|r| r.node.id == node_id) }
    fn send_create(&mut self, node_id: &str, is_dir: bool) -> Result<(), String> {
        let req = if is_dir {
            OutgoingReq::DirCreate { node_id: node_id.to_string(), workspace_id: self.selected.workspace_id().clone() }
        } else {
            OutgoingReq::FileWrite { node_id: node_id.to_string(), content: String::new(), expected_version: None,
                workspace_id: self.selected.workspace_id().clone() }
        };
        self.send(req).map_err(|e| e.to_string())
    }
    fn send_delete(&mut self, node_id: &str) -> Result<(), String> {
        self.send(OutgoingReq::FileDelete { node_id: node_id.to_string(), workspace_id: self.selected.workspace_id().clone() })
            .map_err(|e| e.to_string())
    }
    fn mark_created(&mut self, node_id: String) { self.file_ops.mark_created(node_id); }
    fn mark_deleted(&mut self, node_id: String) { self.file_ops.mark_deleted(node_id); }
    fn prompt_effect(&mut self, effect: PromptEffect) { self.apply_prompt_effect(effect); }
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
