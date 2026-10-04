//! A reply's pending entry, the guard that reports lost figure fetches, and `handle_response_frame`.

use super::*;

/// Internal bookkeeping so a response frame can be deserialized as the right
/// type. Inserted when the writer sends a request; consumed when the matching
/// reply id arrives.
#[derive(Debug)]
pub(super) enum PendingKind {
    TreeChildren {
        parent_id: String,
        workspace_id: Option<String>,
    },
    TreeRoot {
        workspace_id: Option<String>,
    },
    /// Tree-provenance redesign: both kernel-request tree loaders now CARRY
    /// the workspace they were fired for (previously discarded here, which
    /// left their replies un-keyable — a late Modules reply could clobber
    /// another workspace's tree with no way to detect it; the v0.4.3 saga's
    /// last open hole).
    ModulesList {
        workspace_id: Option<String>,
    },
    ProjectScan {
        workspace_id: Option<String>,
        generation: u64,
    },
    MarkdownTokenize {
        lang: String,
        source_hash: u64,
    },
    ConceptRead {
        target: String,
        /// Switch-latency Phase 1: the workspace this read was fired for,
        /// plus the request generation — both threaded straight through
        /// to `IncomingEvt::ConceptRead` unchanged. See that variant.
        workspace_id: Option<String>,
        generation: u64,
    },
    ConceptWrite {
        target: String,
    },
    FileRead {
        node_id: String,
    },
    FileWrite {
        node_id: String,
    },
    FileDelete {
        node_id: String,
    },
    DirCreate {
        node_id: String,
    },
    MathRender {
        latex: String,
        display: bool,
    },
    ImageCrop {
        node_id: String,
    },
    FileParse {
        path: String,
        workspace_id: Option<String>,
    },
    FunctionMethods {
        module: String,
        name: String,
        workspace_id: Option<String>,
    },
    PreviewGet {
        node_id: String,
        workspace_id: Option<String>,
        /// Switch-latency Phase 1: preview-slot request generation, threaded
        /// straight through to `IncomingEvt::Preview`. See that variant.
        generation: u64,
    },
    /// Reply to `preview.set_scale`. Carries the SAME `PreviewGetRes` envelope
    /// as a normal preview, so it decodes with the existing type and surfaces
    /// as `IncomingEvt::Preview` — the chrome installs it through the one
    /// preview path it already has (ADR 0034 §5).
    SetScale {
        node_id: String,
        workspace_id: Option<String>,
        /// Same preview-slot generation as `PreviewGet` — set_scale and an
        /// ordinary preview.get share one consumer slot.
        generation: u64,
    },
    FigureGet {
        url: String,
    },
    ReplEval {
        eval_id: u64,
    },
    /// ADR 0042 slice L1b fix 1: carries the `target` THIS request
    /// named (mirrors `PtyOpenReq.target`) — the reply (an ordinary
    /// size confirmation, or an `attach_direct` refusal) is always about
    /// THIS target, never whatever row happens to be selected by the
    /// time the reply lands.
    PtyOpen {
        target: Option<String>,
    },
    DirectoryList,
    WorkspaceCreate,
    WorkspaceList,
    AccountsList,
    WorkspaceDestroy,
    PlutoOpen,
    VideoOpen,
    DocsOpen,
    QuartoOpen,
    ReplRunFile {
        eval_id: u64,
        path: String,
        fresh: bool,
    },
    /// A `file.download` is streaming. The pending entry is re-inserted on
    /// each non-`eof` chunk (one request id, many response frames). `file` is
    /// lazily created on the first chunk so a backend error before any chunk
    /// leaves no empty file behind.
    FileDownload {
        dest: PathBuf,
        file: Option<std::fs::File>,
    },
    /// A `file.upload` chunk awaiting its ack. 1:1 — each chunk is its own
    /// request id, so no re-insert.
    FileUpload,
    /// A `monitor.subscribe` awaiting its cadence + roster reply (ADR 0020).
    MonitorSubscribe,
    /// A `monitor.history` awaiting its windowed per-host series (ADR 0020).
    MonitorHistory,
}

/// RAII wrapper around `run_protocol`'s per-connection reply-correlation
/// map. `run_protocol` can exit through many paths — a bad read/write via
/// `?`, hello rejection, the outer reconnect loop tearing the task down —
/// and every one of them used to just drop the map in place, silently
/// discarding whatever requests were still in flight. For most
/// `PendingKind`s that's harmless (the UI re-fires on the next user
/// action), but a lost `FigureGet` has no such recovery: the GPU side's
/// `figure_pending` has no way to learn the reply is never coming, so
/// `dispatch_pending_figures` treats the url as "still in flight" forever
/// (field report round 2). `Drop` is the one place that runs on every exit
/// path without needing to touch each of them, so it carries the
/// invariant here: every fired `figure.get` terminates in exactly one of
/// `FigureLoaded` / `FigureGetFailed`, connection loss included.
///
/// `Deref`/`DerefMut` to the inner map so every existing `pending.insert`
/// / `&mut pending` call site in `run_protocol` needs no change.
pub(super) struct PendingGuard<'a> {
    pub(super) map: HashMap<u64, PendingKind>,
    pub(super) evt_tx: &'a StdSender<(HostKey, IncomingEvt)>,
    /// ADR 0042 L2a: which connection this guard belongs to, so its Drop
    /// tags the `FigureGetFailed` flush the same way every other send on
    /// this connection is tagged.
    pub(super) host: HostKey,
}

impl std::ops::Deref for PendingGuard<'_> {
    type Target = HashMap<u64, PendingKind>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl std::ops::DerefMut for PendingGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        let urls: Vec<String> = self
            .map
            .drain()
            .filter_map(|(_, kind)| match kind {
                PendingKind::FigureGet { url } => Some(url),
                _ => None,
            })
            .collect();
        for url in urls {
            let _ = self
                .evt_tx
                .send((self.host.clone(), IncomingEvt::FigureGetFailed { url }));
        }
    }
}

/// Route a frame to the right `IncomingEvt`. Replies look up `id` in the
/// pending map to decide how to deserialize; everything else falls through
/// to the catch-all `Event` evt so the GPU thread can at least see it.
pub(super) fn handle_response_frame(
    frame: Frame,
    mut blob: Option<Vec<u8>>,
    pending: &mut HashMap<u64, PendingKind>,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    host: &HostKey,
) {
    // Same tag-at-the-send pattern as `run_protocol`'s own `emit` — this
    // function carries essentially all of a connection's inbound sends, so
    // it needs its own copy rather than threading `run_protocol`'s closure
    // across a function boundary.
    let emit = |ev: IncomingEvt| {
        let _ = evt_tx.send((host.clone(), ev));
    };
    if let Some(kind) = pending.remove(&frame.id) {
        match kind {
            PendingKind::TreeChildren {
                parent_id,
                workspace_id,
            } => {
                // Backend error frames ({error, code}) are legitimate
                // responses — surface them instead of tripping the struct
                // parse below ("missing field children") and dropping.
                if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(%parent_id, error = %err, "tree.children answered with error");
                    emit(IncomingEvt::TreeChildrenFailed {
                        workspace_id,
                        parent_id,
                        error: err.to_string(),
                    });
                    return;
                }
                match serde_json::from_value::<TreeChildrenRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::TreeChildren {
                            workspace_id,
                            parent_id,
                            children: res.children,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %parent_id, "tree.children res parse failed");
                        emit(IncomingEvt::TreeChildrenFailed {
                            workspace_id,
                            parent_id,
                            error: e.to_string(),
                        });
                    }
                }
            }
            PendingKind::TreeRoot { workspace_id } => {
                match serde_json::from_value::<TreeRootRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::TreeRoot {
                            workspace_id,
                            root: res.node,
                            children: res.children,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tree.root res parse failed");
                    }
                }
            }
            PendingKind::ModulesList { workspace_id } => {
                // The KERNEL_REQUEST envelope returns the kernel's response
                // payload verbatim. modules.list shape after Linux's
                // 4e1c8c0 is `{modules: [{name, uuid, is_main, path}, ...]}`.
                let modules: Vec<ModuleInfo> = frame
                    .payload
                    .get("modules")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                let name = m.get("name").and_then(|n| n.as_str())?;
                                let path = m.get("path").and_then(|p| p.as_str()).map(String::from);
                                Some(ModuleInfo {
                                    name: name.to_string(),
                                    path,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if modules.is_empty() {
                    tracing::warn!(payload = %frame.payload, "modules.list returned no modules");
                }
                emit(IncomingEvt::ModulesList {
                    workspace_id,
                    modules,
                });
            }
            PendingKind::ProjectScan { workspace_id, generation } => {
                // KERNEL_REQUEST returns the kernel's response payload
                // verbatim. project.scan shape is described in
                // ShipToolsKernel.handle_project_scan: `{project_root,
                // package_name, entry_file, modules: [...]}`.
                let payload = frame.payload;
                let project_root = payload
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let package_name = payload
                    .get("package_name")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let entry_file = payload
                    .get("entry_file")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(error = %err, "project.scan returned error");
                    emit(IncomingEvt::ProjectScan {
                        workspace_id,
                        project_root,
                        package_name,
                        entry_file,
                        modules: Vec::new(),
                        generation,
                    });
                } else {
                    let modules = payload
                        .get("modules")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().map(parse_scan_module).collect())
                        .unwrap_or_default();
                    emit(IncomingEvt::ProjectScan {
                        workspace_id,
                        project_root,
                        package_name,
                        entry_file,
                        modules,
                        generation,
                    });
                }
            }
            PendingKind::MarkdownTokenize { lang, source_hash } => {
                // Wire shape: `{ lang, spans: [{ start, end, kind }] }`.
                // We echo `source_hash` from our pending state back to the
                // chrome so it can route into the per-fence cache without
                // the backend knowing about our hashing scheme.
                let payload = frame.payload;
                let spans = payload
                    .get("spans")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| {
                                let start = s.get("start")?.as_u64()? as usize;
                                let end = s.get("end")?.as_u64()? as usize;
                                let kind = s.get("kind")?.as_str()?.to_string();
                                Some(MarkdownToken { start, end, kind })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                emit(IncomingEvt::MarkdownTokens {
                    lang,
                    source_hash,
                    spans,
                });
            }
            PendingKind::ConceptRead {
                target,
                workspace_id,
                generation,
            } => {
                match serde_json::from_value::<ConceptReadRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::ConceptRead {
                            target: res.target,
                            workspace_id,
                            exists: res.exists,
                            content: res.content,
                            generation,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %target, "concept.read res parse failed");
                    }
                }
            }
            PendingKind::MathRender { latex, display } => {
                let is_display = display;
                match serde_json::from_value::<MathRenderRes>(frame.payload) {
                    Ok(res) => {
                        // SVG bytes ride as the framing blob.
                        // `MathRenderRes::blob` carries the descriptor
                        // (len/type) for documentation; the actual bytes
                        // are the `blob` argument from `read_frame`. Skip
                        // when the blob is missing — backend bug or a
                        // weird transport edge — log and move on.
                        let _ = res.blob;
                        match blob {
                            Some(svg_bytes) => {
                                emit(IncomingEvt::MathRendered {
                                    latex,
                                    svg_bytes,
                                    ex: res.ex,
                                    display: res.display,
                                });
                            }
                            None => {
                                tracing::warn!(
                                    latex_len = latex.len(),
                                    is_display,
                                    "math.render reply missing blob bytes"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, latex_len = latex.len(), is_display,
                            "math.render res parse failed");
                    }
                }
            }
            PendingKind::ImageCrop { node_id } => {
                if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
                    emit(IncomingEvt::ImageCropFailed {
                        node_id,
                        message: err.to_string(),
                    });
                } else {
                    match serde_json::from_value::<ImageCropRes>(frame.payload) {
                        Ok(res) => {
                            emit(IncomingEvt::ImageCropped {
                                node_id,
                                path: res.path,
                                x: res.x,
                                y: res.y,
                                w: res.w,
                                h: res.h,
                                src_w: res.src_w,
                                src_h: res.src_h,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "image.crop res parse failed");
                        }
                    }
                }
            }
            PendingKind::ConceptWrite { target } => {
                // Three shapes: the happy-path `ConceptWriteRes`, a
                // `stale_write` envelope (`{error, code: "stale_write",
                // ...}`), or any other `{error, code, ...}` failure.
                // Surface the right variant so the chrome can react
                // without re-parsing the wire shape itself.
                let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
                {
                    if code == "stale_write" {
                        ConceptWriteResult::Stale
                    } else {
                        let message = frame
                            .payload
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(no message)")
                            .to_string();
                        ConceptWriteResult::Error {
                            code: code.to_string(),
                            message,
                        }
                    }
                } else {
                    match serde_json::from_value::<ConceptWriteRes>(frame.payload) {
                        Ok(res) => ConceptWriteResult::Ok {
                            path: res.path,
                            written: res.written,
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, %target,
                                "concept.write res parse failed");
                            ConceptWriteResult::Error {
                                code: "parse_failed".to_string(),
                                message: e.to_string(),
                            }
                        }
                    }
                };
                emit(IncomingEvt::ConceptWriteDone { target, result });
            }
            PendingKind::FileRead { node_id } => {
                match serde_json::from_value::<FileReadRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::FileRead {
                            node_id: res.node_id,
                            exists: res.exists,
                            content: res.content,
                            version: res.version,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %node_id, "file.read res parse failed");
                    }
                }
            }
            PendingKind::FileWrite { node_id } => {
                // Mirror the backend's three shapes: happy-path FileWriteRes, a
                // `{code: "conflict", current_content, current_version}`
                // envelope, or any other `{error, code}` failure.
                let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
                {
                    if code == "conflict" {
                        let current_content = frame
                            .payload
                            .get("current_content")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let current_version = frame
                            .payload
                            .get("current_version")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        FileWriteResult::Conflict {
                            current_content,
                            current_version,
                        }
                    } else {
                        let message = frame
                            .payload
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(no message)")
                            .to_string();
                        FileWriteResult::Error {
                            code: code.to_string(),
                            message,
                        }
                    }
                } else {
                    match serde_json::from_value::<FileWriteRes>(frame.payload) {
                        Ok(res) => FileWriteResult::Ok {
                            path: res.path,
                            version: res.version,
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, %node_id, "file.write res parse failed");
                            FileWriteResult::Error {
                                code: "parse_failed".to_string(),
                                message: e.to_string(),
                            }
                        }
                    }
                };
                emit(IncomingEvt::FileWriteDone { node_id, result });
            }
            PendingKind::FileDelete { node_id } => {
                // Mirror the backend's two shapes: happy-path FileDeleteRes or
                // any `{error, code}` failure (`is_directory`, `not_found`, …).
                let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
                {
                    let message = frame
                        .payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no message)")
                        .to_string();
                    FileDeleteResult::Error {
                        code: code.to_string(),
                        message,
                    }
                } else {
                    match serde_json::from_value::<FileDeleteRes>(frame.payload) {
                        Ok(res) => FileDeleteResult::Ok {
                            path: res.path,
                            trashed: res.trashed,
                            trash_path: res.trash_path,
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, %node_id, "file.delete res parse failed");
                            FileDeleteResult::Error {
                                code: "parse_failed".to_string(),
                                message: e.to_string(),
                            }
                        }
                    }
                };
                emit(IncomingEvt::FileDeleteDone { node_id, result });
            }
            PendingKind::DirCreate { node_id } => {
                // Mirror the backend's two shapes: happy-path DirCreateRes or
                // any `{error, code}` failure (`already_exists`, `bad_node_id`, …).
                let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
                {
                    let message = frame
                        .payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no message)")
                        .to_string();
                    DirCreateResult::Error {
                        code: code.to_string(),
                        message,
                    }
                } else {
                    match serde_json::from_value::<DirCreateRes>(frame.payload) {
                        Ok(res) => DirCreateResult::Ok { path: res.path },
                        Err(e) => {
                            tracing::warn!(error = %e, %node_id, "dir.create res parse failed");
                            DirCreateResult::Error {
                                code: "parse_failed".to_string(),
                                message: e.to_string(),
                            }
                        }
                    }
                };
                emit(IncomingEvt::DirCreateDone { node_id, result });
            }
            PendingKind::FileParse { path, workspace_id } => {
                // `file.parse` returns either {ast_hash, path, definitions}
                // or {error, code, ast_hash?} on parse failure. The hash
                // is computed from raw bytes before the parser runs, so
                // it's present even on parse failure; the definitions
                // array is absent or empty in that case. Outright kernel
                // errors (file missing / outside root) leave both absent;
                // surface nothing then so the chrome stays neutral.
                let hash = frame
                    .payload
                    .get("ast_hash")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let Some(ast_hash) = hash else {
                    // warn, not debug: this silently wedged the drift badge
                    // at "checking…" for a whole capture run before anyone
                    // saw the actual error payload (2026-07-02).
                    tracing::warn!(
                        %path,
                        payload = %frame.payload,
                        "file.parse returned no ast_hash — drift check failed, un-latching for retry"
                    );
                    emit(IncomingEvt::FileParseFailed { workspace_id, path });
                    return;
                };
                let definitions: Vec<DefinitionInfo> = frame
                    .payload
                    .get("definitions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|d| {
                                let name = d.get("name").and_then(|v| v.as_str())?.to_string();
                                let kind = d
                                    .get("kind")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let line = d.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
                                let parent =
                                    d.get("parent").and_then(|v| v.as_str()).map(String::from);
                                let ast_hash =
                                    d.get("ast_hash").and_then(|v| v.as_str()).map(String::from);
                                Some(DefinitionInfo {
                                    name,
                                    kind,
                                    line,
                                    parent,
                                    ast_hash,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                emit(IncomingEvt::FileParsed {
                    workspace_id,
                    path,
                    ast_hash,
                    definitions,
                });
            }
            PendingKind::PreviewGet {
                node_id,
                workspace_id,
                generation,
            } => {
                // An error envelope (`{"error", "code"}` — e.g.
                // `code: "kernel_unavailable"`) fails `PreviewGetRes`
                // deserialization (both its fields are required), so check
                // for it FIRST — same convention `PendingKind::SetScale`
                // below already uses for the same wire op. Surfaced on the
                // status line via `PreviewGetFailed`, carrying the same
                // ownership pair `IncomingEvt::Preview` echoes below so a
                // stale failure can be dropped the same way a stale success
                // is.
                if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
                    let code = frame
                        .payload
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("error");
                    tracing::warn!(%node_id, %code, %err, "preview.get failed");
                    emit(IncomingEvt::PreviewGetFailed {
                        node_id: Some(node_id),
                        workspace_id,
                        generation,
                        message: format!("{code}: {err}"),
                    });
                    return;
                }
                // Same shape as the connect-time preview.get: a typed
                // PreviewGetRes envelope plus a length-prefixed blob the
                // codec already pulled out. Emit the existing
                // `IncomingEvt::Preview` so the chrome handler reuses
                // the same routing it does at startup, but include the
                // node id + workspace so figure-url resolution can
                // anchor against the right markdown directory + go to
                // the right workspace.
                match serde_json::from_value::<PreviewGetRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::Preview {
                            node_id: Some(node_id),
                            workspace_id,
                            mime: res.mime,
                            bytes: blob.unwrap_or_default(),
                            extras: res.extras,
                            generation,
                        });
                    }
                    Err(e) => {
                        // A reply that is neither an `{error, code}`
                        // envelope (handled above) nor a valid
                        // `PreviewGetRes` — the shape a MISSING FILE
                        // produces, whose only trace used to be this warn
                        // plus `reveal: target still absent`. That silence
                        // actively misled: it was once read as a rendering
                        // regression when the file simply did not exist.
                        // `PendingKind::FigureGet` below already routes
                        // both causes down one terminal path; the pane the
                        // person is actually looking at gets the same.
                        tracing::warn!(error = %e, "preview.get res parse failed");
                        emit(IncomingEvt::PreviewGetFailed {
                            node_id: Some(node_id),
                            workspace_id,
                            generation,
                            message: format!("preview unavailable: {e}"),
                        });
                    }
                }
                return;
            }
            PendingKind::SetScale {
                node_id,
                workspace_id,
                generation,
            } => {
                // ADR 0034 §5: the backend persisted the sidecar and returned
                // the RE-RENDERED preview in the same PreviewGetRes envelope,
                // with `extras.physical_scale` already rescaled for the served
                // image. Decode with the existing type and emit the existing
                // `IncomingEvt::Preview` so the chrome installs it through the
                // ONE preview path it already has — no second install path to
                // drift out of sync (the failure mode behind F2/R4-R6).
                //
                // This also has to be a REPLY, not an unsolicited push: replies
                // are correlated by frame id via `pending.remove`, so a pushed
                // preview frame would find no entry and be dropped silently.
                //
                // Rejections come back as an `error` payload under the same
                // frame id; surface them so the prompt's "saving…" resolves
                // instead of hanging.
                if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
                    let code = frame
                        .payload
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("error");
                    tracing::warn!(%node_id, %code, %err, "preview.set_scale rejected");
                    emit(IncomingEvt::ScaleSetFailed {
                        node_id,
                        message: format!("{code}: {err}"),
                    });
                    return;
                }
                match serde_json::from_value::<PreviewGetRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::Preview {
                            node_id: Some(node_id),
                            workspace_id,
                            mime: res.mime,
                            bytes: blob.unwrap_or_default(),
                            extras: res.extras,
                            generation,
                        });
                    }
                    Err(e) => {
                        // Same rule as the plain `preview.get` arm above: a
                        // malformed reply is a FAILED set_scale, not a
                        // no-op. Silence here leaves the prompt's "saving…"
                        // resolved-looking while nothing was served.
                        tracing::warn!(error = %e, "preview.set_scale res parse failed");
                        emit(IncomingEvt::ScaleSetFailed {
                            node_id,
                            message: format!("scale set but preview unavailable: {e}"),
                        });
                    }
                }
                return;
            }
            PendingKind::FigureGet { url } => {
                // Same wire shape as PreviewGet, routed to the chrome's
                // figure cache via a different IncomingEvt so the
                // active markdown buffer isn't replaced.
                //
                // Field report: an early `figure.get` (fired before the
                // backend has the target PNG yet) answers with `{error,
                // code}` — this used to warn-and-drop with no event at
                // all, so `url` never left `figure_pending` and every
                // later reload skipped it forever (dispatch_pending_figures
                // treats "pending" as "already in flight, don't refire").
                // No separate check for the error envelope is needed:
                // `PreviewGetRes` requires `mime` and `blob`, neither of
                // which an `{error, code}` payload carries, so it always
                // falls into the parse-failure arm below — one path,
                // both causes, both terminate as `FigureGetFailed`.
                match serde_json::from_value::<PreviewGetRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::FigureLoaded {
                            url,
                            mime: res.mime,
                            bytes: blob.unwrap_or_default(),
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %url, "figure.get failed or unparseable");
                        emit(IncomingEvt::FigureGetFailed { url });
                    }
                }
                return;
            }
            PendingKind::FunctionMethods {
                module,
                name,
                workspace_id,
            } => {
                // Reply shape: `{methods: [{module, name, file, line, sig, ast_hash}, ...]}`
                // or `{error, code}` on bad_request / module_not_found /
                // function_not_found. We surface an empty list in the
                // error case so the chrome still applies (no children),
                // rather than leaving the row in a "loading…" limbo.
                let methods: Vec<MethodInfo> = frame
                    .payload
                    .get("methods")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                let sig = m.get("sig").and_then(|v| v.as_str())?.to_string();
                                let file = m
                                    .get("file")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let line = m.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
                                let ast_hash =
                                    m.get("ast_hash").and_then(|v| v.as_str()).map(String::from);
                                Some(MethodInfo {
                                    sig,
                                    file,
                                    line,
                                    ast_hash,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str()) {
                    tracing::warn!(%module, %name, code, "function.methods returned error");
                }
                emit(IncomingEvt::FunctionMethodsReceived {
                    workspace_id,
                    module,
                    name,
                    methods,
                });
            }
            PendingKind::ReplEval { eval_id } => {
                // Synchronous-collect per ADR 0009: the response carries
                // the full frame list. Streamed delivery is a planned
                // enhancement and shifts the routing
                // off this path; this arm only needs to handle the
                // collected-at-once payload.
                match serde_json::from_value::<ReplEvalRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::ReplEvalDone {
                            eval_id: res.eval_id,
                            elapsed_ms: res.elapsed_ms,
                            frames: res.frames,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, eval_id, "repl.eval res parse failed");
                    }
                }
            }
            PendingKind::PtyOpen { target } => {
                // ADR 0042 slice L1b: a capsule row's `pty.open` is
                // refused with `{error, code: "attach_direct",
                // state_dir}` — this build's daemon has no tmux runtime,
                // so that refusal is the ONLY reply `pty.open` ever
                // gets; there is no size-confirmation success case left
                // to parse. `target` is THIS request's own target (fix
                // 1) — the chrome corrects/attaches that row, not
                // whatever is currently selected.
                if is_attach_direct(&frame.payload) {
                    emit(IncomingEvt::PtyAttachDirect { target });
                } else {
                    let error = pty_open_failure_reason(&frame.payload);
                    tracing::warn!(?target, %error, payload = ?frame.payload, "pty.open res was not attach_direct");
                    emit(IncomingEvt::PtyOpenFailed { target, error });
                }
            }
            PendingKind::DirectoryList => {
                match serde_json::from_value::<sot_protocol::DirectoryListRes>(frame.payload) {
                    Ok(res) => {
                        let entries: Vec<DirEntry> = res
                            .entries
                            .into_iter()
                            .map(|e| DirEntry {
                                name: e.name,
                                path: e.path,
                                has_children: e.has_children,
                            })
                            .collect();
                        emit(IncomingEvt::DirectoryList {
                            path: res.path,
                            entries,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "directory.list res parse failed");
                    }
                }
            }
            PendingKind::WorkspaceCreate => {
                // Backend returns either WorkspaceCreateRes on success
                // or `{error, code}` on failure (no_such_path etc).
                // Distinguish by presence of `workspace_id`.
                let payload = frame.payload;
                let result = if payload.get("workspace_id").is_some() {
                    match serde_json::from_value::<sot_protocol::WorkspaceCreateRes>(payload) {
                        Ok(r) => Ok(WorkspaceCreatedInfo {
                            workspace_id: r.workspace_id,
                            slug: r.slug,
                            label: r.label,
                            project_root: r.project_root,
                            session_name: r.session_name,
                        }),
                        Err(e) => Err(format!("workspace.create res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                emit(IncomingEvt::WorkspaceCreated { result });
            }
            PendingKind::WorkspaceList => {
                match serde_json::from_value::<WorkspaceListRes>(frame.payload) {
                    Ok(res) => {
                        let workspaces: Vec<WorkspaceInfo> = res
                            .workspaces
                            .into_iter()
                            .map(|w| WorkspaceInfo {
                                workspace_id: w.workspace_id,
                                slug: w.slug,
                                label: w.label,
                                project_root: w.project_root,
                                session_name: w.session_name,
                                kernel_running: w.kernel_running,
                                is_default: w.is_default,
                                agent: w.agent,
                                autostart_claude: w.autostart_claude,
                                agent_name: w.agent_name,
                                agent_handle: w.agent_handle,
                                task: w.task,
                                agent_state: w.agent_state,
                                agent_summary: w.agent_summary,
                                agent_status_at: w.agent_status_at,
                                repl_state: w.repl_state,
                                runtime: w.runtime,
                                phase: w.phase,
                                account: w.account,
                            })
                            .collect();
                        emit(IncomingEvt::Workspaces { workspaces });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "workspace.list res parse failed");
                    }
                }
            }
            // Per-session accounts (owner-simplified brief, 2026-09-15): a
            // daemon that has no `accounts.list` handler (old build) or
            // whose reply otherwise fails to parse as `AccountsListRes` is
            // treated as an empty list — default-only, no error surfaced.
            // The chrome hides the account choice entirely on an empty list.
            PendingKind::AccountsList => {
                let accounts = serde_json::from_value::<sot_protocol::AccountsListRes>(frame.payload)
                    .map(|r| r.accounts)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| AccountInfo {
                        name: a.name,
                        kinds: a.kinds,
                        logged_in: a.logged_in.into_iter().collect(),
                    })
                    .collect();
                emit(IncomingEvt::AccountsList { accounts });
            }
            PendingKind::WorkspaceDestroy => {
                // Same shape as WorkspaceCreate: success carries the
                // canonical fields (workspace_id etc.), failure carries
                // `{error, code}`. Distinguish by presence of
                // `workspace_id` since the protocol re-uses the op
                // response frame for both.
                let payload = frame.payload;
                let result = if payload.get("workspace_id").is_some() {
                    match serde_json::from_value::<sot_protocol::WorkspaceDestroyRes>(payload) {
                        Ok(r) => Ok(WorkspaceDestroyedInfo {
                            workspace_id: r.workspace_id,
                            slug: r.slug,
                            label: r.label,
                            tmux_killed: r.tmux_killed,
                            toml_removed: r.toml_removed,
                            kept: r.kept,
                        }),
                        Err(e) => Err(format!("workspace.destroy res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                emit(IncomingEvt::WorkspaceDestroyed { result });
            }
            PendingKind::PlutoOpen => {
                let payload = frame.payload;
                let result = if payload.get("url").is_some() {
                    match serde_json::from_value::<PlutoOpenRes>(payload) {
                        Ok(r) => Ok(r.url),
                        Err(e) => Err(format!("pluto.open res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                emit(IncomingEvt::PlutoOpened { result });
            }
            PendingKind::VideoOpen => {
                let payload = frame.payload;
                let result = if payload.get("url").is_some() {
                    match serde_json::from_value::<VideoOpenRes>(payload) {
                        Ok(r) => Ok(r.url),
                        Err(e) => Err(format!("video.open res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                emit(IncomingEvt::VideoOpened { result });
            }
            PendingKind::DocsOpen => {
                let payload = frame.payload;
                let result = if payload.get("url").is_some() {
                    match serde_json::from_value::<DocsOpenRes>(payload) {
                        Ok(r) => Ok(r.url),
                        Err(e) => Err(format!("docs.open res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                emit(IncomingEvt::DocsOpened { result });
            }
            PendingKind::QuartoOpen => {
                let payload = frame.payload;
                // The rendered HTML rides the framing blob, like math.render's
                // SVG: a `--embed-resources` Quarto doc routinely exceeds the
                // codec's 1 MiB *envelope* cap (a 1.2 MB HTML base64s to
                // 1.61 MiB), and an oversize envelope killed the whole
                // connection — the FE saw eof and rebuilt the nav tree.
                //
                // The legacy `html_base64` arm stays because the FE and the
                // daemon are separate binaries on separate hosts and roll out
                // independently; accepting both shapes makes the deploy order
                // irrelevant instead of leaving a window where `o` is broken.
                let result = if let Some(bytes) = blob {
                    Ok(bytes)
                } else if let Some(b64) = payload.get("html_base64").and_then(|v| v.as_str()) {
                    // Read the field off the JSON rather than through
                    // `QuartoOpenRes` on purpose: the struct is the daemon's to
                    // reshape for the blob move, and this arm must keep
                    // compiling either way.
                    base64::engine::general_purpose::STANDARD
                        .decode(b64.as_bytes())
                        .map_err(|e| format!("quarto.open base64 decode: {e}"))
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                emit(IncomingEvt::QuartoOpened { result });
            }
            PendingKind::ReplRunFile {
                eval_id,
                path,
                fresh,
            } => {
                // Success = `frames` present; error envelopes carry
                // `{error, code}` per the handler contract. Same shape
                // pattern as WorkspaceCreate / PlutoOpen above.
                let payload = frame.payload;
                let result = if payload.get("frames").is_some() {
                    match serde_json::from_value::<ReplRunFileRes>(payload) {
                        Ok(r) => Ok(ReplRunFileInfo {
                            eval_id: r.eval_id,
                            path: r.path,
                            fresh: r.fresh,
                            elapsed_ms: r.elapsed_ms,
                            project_dir: r.project_dir,
                            project_source: r.project_source,
                            frames: r.frames,
                        }),
                        Err(e) => Err(format!("repl.run_file res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = path;
                let _ = fresh;
                emit(IncomingEvt::ReplRunFileDone { eval_id, result });
            }
            PendingKind::FileDownload { dest, mut file } => {
                let frame_id = frame.id;
                let payload = frame.payload;
                // A download error replies with `{error, code}` and no chunk.
                if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(error = %err, dest = %dest.display(), "file.download failed");
                    if file.is_some() {
                        let _ = std::fs::remove_file(&dest);
                    }
                    emit(IncomingEvt::FileTransferFailed {
                        op: "download",
                        message: err.to_string(),
                    });
                } else {
                    match serde_json::from_value::<FileChunk>(payload) {
                        Ok(chunk) => {
                            let bytes = blob.take().unwrap_or_default();
                            // Create (truncating) the dest on the first chunk so a
                            // pre-chunk error never leaves an empty file behind.
                            if file.is_none() {
                                match std::fs::File::create(&dest) {
                                    Ok(f) => file = Some(f),
                                    Err(e) => {
                                        tracing::warn!(error = %e, dest = %dest.display(),
                                            "file.download: cannot create dest");
                                        emit(IncomingEvt::FileTransferFailed {
                                            op: "download",
                                            message: format!("create {}: {e}", dest.display()),
                                        });
                                        return;
                                    }
                                }
                            }
                            let mut write_err: Option<String> = None;
                            if let Some(f) = file.as_mut() {
                                use std::io::{Seek, SeekFrom, Write};
                                if let Err(e) = f
                                    .seek(SeekFrom::Start(chunk.offset))
                                    .and_then(|_| f.write_all(&bytes))
                                {
                                    write_err = Some(format!("write {}: {e}", dest.display()));
                                }
                            }
                            if let Some(msg) = write_err {
                                tracing::warn!(dest = %dest.display(), %msg, "file.download write failed");
                                let _ = std::fs::remove_file(&dest);
                                emit(IncomingEvt::FileTransferFailed {
                                    op: "download",
                                    message: msg,
                                });
                            } else {
                                let written = chunk.offset + bytes.len() as u64;
                                emit(IncomingEvt::FileDownloadProgress {
                                    dest: dest.clone(),
                                    written,
                                    total: chunk.total,
                                    eof: chunk.eof,
                                });
                                if !chunk.eof {
                                    // One request id, many chunks: keep the
                                    // transfer (with its open file handle) alive
                                    // for the next streamed frame.
                                    pending
                                        .insert(frame_id, PendingKind::FileDownload { dest, file });
                                }
                                // On eof: pending stays removed; `file` drops here,
                                // flushing + closing the completed download.
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "file.download chunk parse failed");
                            let _ = std::fs::remove_file(&dest);
                            emit(IncomingEvt::FileTransferFailed {
                                op: "download",
                                message: format!("bad chunk: {e}"),
                            });
                        }
                    }
                }
            }
            PendingKind::FileUpload => {
                let payload = frame.payload;
                if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(error = %err, "file.upload failed");
                    emit(IncomingEvt::FileTransferFailed {
                        op: "upload",
                        message: err.to_string(),
                    });
                } else {
                    match serde_json::from_value::<FileUploadAck>(payload) {
                        Ok(ack) => {
                            emit(IncomingEvt::FileUploadAck {
                                offset: ack.offset,
                                done: ack.done,
                                final_name: ack.final_name,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "file.upload ack parse failed");
                            emit(IncomingEvt::FileTransferFailed {
                                op: "upload",
                                message: format!("bad ack: {e}"),
                            });
                        }
                    }
                }
            }
            PendingKind::MonitorSubscribe => {
                match serde_json::from_value::<MonitorSubscribeRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::MonitorSubscribed {
                            hosts: res.hosts,
                            interval_s: res.interval_s,
                        });
                    }
                    Err(e) => tracing::warn!(error = %e, "monitor.subscribe res parse failed"),
                }
            }
            PendingKind::MonitorHistory => {
                match serde_json::from_value::<MonitorHistoryRes>(frame.payload) {
                    Ok(res) => {
                        emit(IncomingEvt::MonitorHistory { hosts: res.hosts });
                    }
                    Err(e) => tracing::warn!(error = %e, "monitor.history res parse failed"),
                }
            }
        }
        let _ = blob; // remaining ops carry no blob (download took its own)
        return;
    }
    // Streamed REPL output frame (`repl.frame` evt, ADR 0009 phase-2). One
    // frame, pushed as produced; the consumer appends it to the in-flight
    // scrollback entry live (`Done` is terminal).
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::REPL_FRAME {
        match serde_json::from_value::<ReplFrameEvt>(frame.payload) {
            Ok(ev) => {
                emit(IncomingEvt::ReplFrameStreamed {
                    eval_id: ev.eval_id,
                    workspace_id: ev.workspace_id,
                    frame: ev.frame,
                });
            }
            Err(e) => tracing::warn!(error = %e, "repl.frame evt parse failed"),
        }
        return;
    }
    // Live server-metrics tick (`monitor.tick` evt, ADR 0020). One sample per
    // host at the subscribed cadence; the consumer appends each to its ring.
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::MONITOR_TICK {
        match serde_json::from_value::<MonitorTickEvt>(frame.payload) {
            Ok(ev) => {
                emit(IncomingEvt::MonitorTick { hosts: ev.hosts });
            }
            Err(e) => tracing::warn!(error = %e, "monitor.tick evt parse failed"),
        }
        return;
    }
    emit(IncomingEvt::Event {
        op: frame.op,
        payload: frame.payload,
    });
    if blob.is_some() {
        // Spike handlers don't emit unsolicited blobs; drop and move on.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-seam regression for the round-1 fix: an `{error, code}` reply to
    /// a `figure.get` must produce EXACTLY one `FigureGetFailed`, driven
    /// through the actual `handle_response_frame` dispatcher (not a
    /// reimplementation of its logic) — and must consume the pending entry
    /// so nothing is left to strand.
    #[test]
    fn figure_get_error_envelope_emits_exactly_one_figure_get_failed() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            7,
            PendingKind::FigureGet {
                url: "figures/dead.png".to_string(),
            },
        );
        let frame = Frame::res(
            7,
            op::PREVIEW_GET,
            serde_json::json!({"error": "not_found", "code": "not_found"}),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(
            events.len(),
            1,
            "exactly one event for one figure.get error reply, got {events:?}"
        );
        match &events[0] {
            (h, IncomingEvt::FigureGetFailed { url }) => {
                assert_eq!(h, &host);
                assert_eq!(url, "figures/dead.png");
            }
            other => panic!("expected FigureGetFailed, got {other:?}"),
        }
        assert!(
            pending.is_empty(),
            "the reply must consume its pending entry"
        );
    }

    /// A malformed-but-not-explicit-error payload (missing the required
    /// `mime`/`blob` fields without an `error` key either) must ALSO reach
    /// `FigureGetFailed` through the same parse-failure arm — this is what
    /// makes the explicit error-envelope pre-check provably redundant
    /// (deleted per the round-1 simplicity audit).
    #[test]
    fn figure_get_unparseable_reply_without_error_key_still_fails_terminal() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            9,
            PendingKind::FigureGet {
                url: "figures/weird.png".to_string(),
            },
        );
        let frame = Frame::res(9, op::PREVIEW_GET, serde_json::json!({"unexpected": true}));
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], (h, IncomingEvt::FigureGetFailed { url }) if h == &host && url == "figures/weird.png")
        );
    }

    /// Version-skew seam: a NEW frontend against an OLD daemon (no
    /// `workspace.activate` support). `OutgoingReq::WorkspaceActivate`
    /// deliberately stamps NO `PendingKind` (see its send-site comment) —
    /// so an old daemon's generic unknown-op reply (`server.rs`'s `other =>`
    /// catch-all: `{"error": "unknown op: workspace.activate"}`, same `op`
    /// and `id` echoed back) finds nothing in `pending` and MUST fall all
    /// the way through to the generic `IncomingEvt::Event` catch-all —
    /// which gpu.rs's own catch-all (`else { tracing::debug!(%op, "evt"); }`)
    /// only logs at debug and does nothing else. This is what makes the skew
    /// safe: no status-line error, no retry, no effect on the switch that
    /// sent it. A real `WorkspaceActivateRes` success reply lands the exact
    /// same way (also no `PendingKind`), so this one test covers both.
    #[test]
    fn workspace_activate_reply_with_no_pending_falls_through_to_generic_event() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        let frame = Frame::res(
            11,
            op::WORKSPACE_ACTIVATE,
            serde_json::json!({"error": "unknown op: workspace.activate"}),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1);
        match &events[0] {
            (h, IncomingEvt::Event { op, payload }) => {
                assert_eq!(h, &host);
                assert_eq!(op, op::WORKSPACE_ACTIVATE);
                assert_eq!(
                    payload.get("error").and_then(|v| v.as_str()),
                    Some("unknown op: workspace.activate")
                );
            }
            other => panic!(
                "expected the generic IncomingEvt::Event fallback (no status-line \
                 variant exists for this op), got {other:?}"
            ),
        }
        assert!(
            pending.is_empty(),
            "no PendingKind was ever inserted for workspace.activate — nothing to strand"
        );
    }

    /// Fix 2 (disconnect strands pending forever): dropping the guard —
    /// standing in for `run_protocol` returning through any of its exit
    /// paths — must flush every outstanding `FigureGet` as
    /// `FigureGetFailed`, and must NOT invent events for other pending
    /// kinds (those have no permanent-skip state on the GPU side, so
    /// losing them silently on disconnect is the pre-existing, accepted
    /// behavior).
    #[test]
    fn pending_guard_flushes_figure_gets_on_drop_not_other_kinds() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let host = "test-host".to_string();
        {
            let mut guard = PendingGuard {
                map: HashMap::new(),
                evt_tx: &evt_tx,
                host: host.clone(),
            };
            guard.insert(
                1,
                PendingKind::FigureGet {
                    url: "figures/a.png".to_string(),
                },
            );
            guard.insert(
                2,
                PendingKind::FigureGet {
                    url: "figures/b.png".to_string(),
                },
            );
            guard.insert(3, PendingKind::PtyOpen { target: None });
            // `guard` drops here — the connection-loss scenario.
        }
        let mut urls: Vec<String> = evt_rx
            .try_iter()
            .map(|(h, evt)| {
                assert_eq!(h, host);
                match evt {
                    IncomingEvt::FigureGetFailed { url } => url,
                    other => panic!("only FigureGet entries should flush on drop, got {other:?}"),
                }
            })
            .collect();
        urls.sort();
        assert_eq!(
            urls,
            vec!["figures/a.png".to_string(), "figures/b.png".to_string()]
        );
    }

    /// Real-seam regression, same shape as `figure_get_error_envelope_...`
    /// above: an `attach_direct` refusal to `pty.open`, driven through the
    /// actual `handle_response_frame` dispatcher, must produce exactly one
    /// `PtyAttachDirect` carrying the exact `target` this request's own
    /// `PendingKind::PtyOpen` entry named — L1b fix 1's whole point: the
    /// reply is about THAT row, not whatever the pending map happened to
    /// be keyed against.
    #[test]
    fn attach_direct_reply_emits_exactly_one_pty_attach_direct_with_its_own_target() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            9,
            PendingKind::PtyOpen {
                target: Some("sot-be-alpha".to_string()),
            },
        );
        let frame = Frame::res(
            9,
            op::PTY_OPEN,
            serde_json::json!({
                "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
                "code": "attach_direct",
                "state_dir": "/state/workspaces/ws-9",
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(
            events.len(),
            1,
            "exactly one event for one attach_direct reply, got {events:?}"
        );
        match &events[0] {
            (h, IncomingEvt::PtyAttachDirect { target }) => {
                assert_eq!(h, &host);
                assert_eq!(target.as_deref(), Some("sot-be-alpha"));
            }
            other => panic!("expected PtyAttachDirect, got {other:?}"),
        }
    }

    #[test]
    fn non_attach_direct_pty_open_reply_emits_pty_open_failed() {
        // This build's daemon never answers `pty.open` with anything but
        // an `attach_direct` refusal (every row is a capsule) — anything
        // else used to be warned-and-dropped, leaving the pane `Pending`
        // forever with buffered keystrokes and no visible explanation.
        // It must now surface as `PtyOpenFailed` so the chrome can show
        // a reason.
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            11,
            PendingKind::PtyOpen {
                target: Some("sot-be-beta".to_string()),
            },
        );
        let frame = Frame::res(11, op::PTY_OPEN, serde_json::json!({"cols": 80, "rows": 24}));
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (h, IncomingEvt::PtyOpenFailed { target, error }) => {
                assert_eq!(h, &host);
                assert_eq!(target.as_deref(), Some("sot-be-beta"));
                assert_eq!(error, "unsupported daemon reply");
            }
            other => panic!("expected PtyOpenFailed, got {other:?}"),
        }
    }

    #[test]
    fn non_attach_direct_pty_open_reply_uses_the_replys_own_code_as_the_reason() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            12,
            PendingKind::PtyOpen {
                target: Some("sot-be-gamma".to_string()),
            },
        );
        let frame = Frame::res(
            12,
            op::PTY_OPEN,
            serde_json::json!({"error": "boom", "code": "bad_target"}),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (_, IncomingEvt::PtyOpenFailed { error, .. }) => assert_eq!(error, "bad_target"),
            other => panic!("expected PtyOpenFailed, got {other:?}"),
        }
    }

    // --- Switch-latency Phase 1: the generation/owner fields transport.rs
    // threads through `PendingKind` are exactly what the request stamped.
    // The chrome's accept/reject DECISION (`reply_is_current`) lives in
    // gpu.rs and is tested there; this only proves the plumbing.

    #[test]
    fn concept_read_reply_echoes_the_workspace_and_generation_it_was_fired_with() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            21,
            PendingKind::ConceptRead {
                target: "MyModule.myfunction".to_string(),
                workspace_id: Some("ws-a".to_string()),
                generation: 7,
            },
        );
        let frame = Frame::res(
            21,
            op::CONCEPT_READ,
            serde_json::json!({
                "target": "MyModule.myfunction",
                "exists": true,
                "content": "hello",
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (
                h,
                IncomingEvt::ConceptRead {
                    target,
                    workspace_id,
                    exists,
                    content,
                    generation,
                },
            ) => {
                assert_eq!(h, &host);
                assert_eq!(target, "MyModule.myfunction");
                assert_eq!(workspace_id.as_deref(), Some("ws-a"));
                assert!(*exists);
                assert_eq!(content, "hello");
                assert_eq!(*generation, 7);
            }
            other => panic!("expected ConceptRead, got {other:?}"),
        }
    }

    #[test]
    fn preview_get_reply_echoes_the_generation_it_was_fired_with() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            22,
            PendingKind::PreviewGet {
                node_id: "files:a.md".to_string(),
                workspace_id: Some("ws-b".to_string()),
                generation: 42,
            },
        );
        let frame = Frame::res(
            22,
            op::PREVIEW_GET,
            serde_json::json!({
                "mime": "text/markdown",
                "blob": {"len": 0, "mime": "text/markdown"},
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, Some(Vec::new()), &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (
                h,
                IncomingEvt::Preview {
                    node_id,
                    workspace_id,
                    generation,
                    ..
                },
            ) => {
                assert_eq!(h, &host);
                assert_eq!(node_id.as_deref(), Some("files:a.md"));
                assert_eq!(workspace_id.as_deref(), Some("ws-b"));
                assert_eq!(*generation, 42);
            }
            other => panic!("expected Preview, got {other:?}"),
        }
    }

    /// A `preview.get` error envelope (as `handle_preview_get` sends for
    /// e.g. `code: "kernel_unavailable"`) must surface as
    /// `PreviewGetFailed`, not silently fail `PreviewGetRes` deserialization
    /// — and must echo the SAME ownership pair the success path does, so a
    /// stale one can be dropped identically.
    #[test]
    fn preview_get_error_envelope_emits_preview_get_failed() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            9,
            PendingKind::PreviewGet {
                node_id: "files:data.h5".to_string(),
                workspace_id: Some("ws-a".to_string()),
                generation: 3,
            },
        );
        let frame = Frame::res(
            9,
            op::PREVIEW_GET,
            serde_json::json!({
                "error": "Julia kernel unavailable: julia exited at once",
                "code": "kernel_unavailable",
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (h, IncomingEvt::PreviewGetFailed { node_id, workspace_id, generation, message }) => {
                assert_eq!(h, &host);
                assert_eq!(node_id.as_deref(), Some("files:data.h5"));
                assert_eq!(workspace_id.as_deref(), Some("ws-a"));
                assert_eq!(*generation, 3);
                assert_eq!(message, "kernel_unavailable: Julia kernel unavailable: julia exited at once");
            }
            other => panic!("expected PreviewGetFailed, got {other:?}"),
        }
    }
}
