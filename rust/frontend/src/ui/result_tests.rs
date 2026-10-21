use super::*;

impl State {
    pub(in crate::ui) fn result_target_uses_host_and_listed_identity(&mut self) -> Result<()> {
        let (a, b, mut a_rx, mut b_rx) = self.prepare_result_probe();
        self.result_host_probe(&b, &mut a_rx)?;
        self.docs_host_probe(&a, &b, &mut a_rx, &mut b_rx)?;
        self.result_identity_probe(&b, &mut b_rx)?;
        self.result_entry_controls(&a, &b, &mut a_rx, &mut b_rx)?;
        self.result_caption_roi_probe(&a, &b, &mut a_rx, &mut b_rx)?;
        println!("result-routing host_and_identity=true ingress_controls=true caption_roi=true entered_bodies=1 completed_bodies=1");
        Ok(())
    }

    fn prepare_result_probe(
        &mut self,
    ) -> (
        HostKey,
        HostKey,
        tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    ) {
        let a = "<host-a>".to_string();
        let b = "<host-b>".to_string();
        let (a_tx, mut a_rx) = tokio::sync::mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(a.clone(), a_tx), (b.clone(), b_tx)];
        self.active_host = a.clone();
        self.active_workspace_id = Some("project".into());
        let mut row_a = ws_info("project", "stored-a");
        row_a.workspace_id = "id-a".into();
        let mut row_b = ws_info("project", "stored-b");
        row_b.workspace_id = "id-b".into();
        let mut default_a = ws_info("base-a", "default-a");
        default_a.is_default = true;
        let mut default_b = ws_info("base-b", "default-b");
        default_b.is_default = true;
        self.on_workspaces(a.clone(), vec![row_a, default_a]);
        self.on_workspaces(b.clone(), vec![row_b, default_b]);
        self.pending_nav = PendingResults::default();
        while a_rx.try_recv().is_ok() {}
        while b_rx.try_recv().is_ok() {}
        (a, b, a_rx, b_rx)
    }

    fn result_host_probe(
        &mut self,
        b: &HostKey,
        a_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    ) -> Result<()> {
        self.dispatch_fe_command(
            Some(&b),
            FeCommand::Preview {
                workspace: "project".into(),
                path: "same.png".into(),
                urgent: false,
                roi: Some(RoiRect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }),
                caption: Some("from b".into()),
            },
        );
        anyhow::ensure!(self.pending_result_path(b, "project").as_deref() == Some("same.png")
            && a_rx.try_recv().is_err(), "result_target_uses_host_and_listed_identity: another host's same slug rendered in the active view");
        Ok(())
    }

    /// `docs` goes to the daemon that sent it, not the one this window is viewing: only that daemon knows the path.
    fn docs_host_probe(
        &mut self,
        a: &HostKey,
        b: &HostKey,
        a_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        b_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    ) -> Result<()> {
        anyhow::ensure!(self.active_host == *a, "docs probe: the window must view host a");
        self.dispatch_fe_command(Some(b), FeCommand::Docs { workspace: "project".into(), path: "/site/page.html".into() });
        anyhow::ensure!(
            matches!(b_rx.try_recv(), Ok(OutgoingReq::DocsOpen { .. })) && a_rx.try_recv().is_err(),
            "docs went to the viewed host, not the host that sent it"
        );
        Ok(())
    }

    fn result_identity_probe(
        &mut self,
        b: &HostKey,
        b_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    ) -> Result<()> {
        self.dispatch_fe_command(
            Some(&b),
            FeCommand::Reveal {
                workspace: "id-b".into(),
                path: "by-id.png".into(),
                urgent: false,
                roi: None,
                caption: None,
            },
        );
        anyhow::ensure!(
            self.pending_result_path(b, "project").as_deref() == Some("by-id.png")
                && !self.pending_nav.contains_key(&(b.clone(), "id-b".into())),
            "canonical spelling created a phantom strip key"
        );
        let before = (
            self.active_host.clone(),
            self.active_workspace_id.clone(),
            self.pending_nav.clone(),
        );
        self.dispatch_fe_command(
            Some(&b),
            FeCommand::Preview {
                workspace: "unknown".into(),
                path: "same.png".into(),
                urgent: true,
                roi: None,
                caption: Some("unknown".into()),
            },
        );
        anyhow::ensure!(
            before
                == (
                    self.active_host.clone(),
                    self.active_workspace_id.clone(),
                    self.pending_nav.clone()
                )
                && self.status.starts_with("result refused"),
            "unknown urgent target mutated result state"
        );
        for command in [
            FeCommand::Reveal {
                workspace: "unknown".into(),
                path: "same.png".into(),
                urgent: true,
                roi: None,
                caption: None,
            },
            FeCommand::Workspace {
                slug: Some("unknown".into()),
            },
        ] {
            self.dispatch_fe_command(Some(b), command);
            anyhow::ensure!(
                before
                    == (
                        self.active_host.clone(),
                        self.active_workspace_id.clone(),
                        self.pending_nav.clone()
                    ),
                "unknown goto or reveal mutated result state"
            );
        }
        self.dispatch_fe_command(
            Some(&b),
            FeCommand::Workspace {
                slug: Some("id-b".into()),
            },
        );
        let requests: Vec<_> = std::iter::from_fn(|| b_rx.try_recv().ok()).collect();
        anyhow::ensure!(self.active_host == *b && self.active_workspace_id.as_deref() == Some("project")
            && requests.iter().any(|r| matches!(r, OutgoingReq::PtyOpen { target: Some(name), .. } if name == "stored-b")),
            "stored attachment name was not used for the listed canonical row");
        Ok(())
    }

    fn result_entry_controls(
        &mut self,
        a: &HostKey,
        b: &HostKey,
        a_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        b_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    ) -> Result<()> {
        self.cycle_workspace(1, false);
        anyhow::ensure!(
            self.bl_pane_target.as_ref() == Some(&(b.clone(), "default-b".into())),
            "cycle derived an attachment name"
        );
        self.dispatch_fe_command(
            Some(a),
            FeCommand::Workspace {
                slug: Some("default".into()),
            },
        );
        anyhow::ensure!(
            self.active_host == *a
                && self.active_workspace_id.is_none()
                && self.default_workspace_slug.as_deref() == Some("base-a"),
            "default resolved using another host's list"
        );
        let envelope = parse_nav_envelope(
            r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"id-b","path":"envelope.png"}}"#,
        )
        .unwrap();
        self.handle_nav_envelope(b, &envelope);
        anyhow::ensure!(
            self.pending_result_path(b, "project").as_deref() == Some("envelope.png"),
            "envelope failed listed id resolution"
        );
        let mut selected = node("sessions:selected", "selected", false);
        selected.kind = "session".into();
        selected.payload = serde_json::json!({"host": b, "workspace_id":"id-b", "slug":"project", "name":"stored-b"}).as_object().unwrap().clone();
        self.tree
            .set_root(node("sessions:", "sessions", true), vec![selected]);
        self.tree.selected = 1;
        self.bl_pane_target = None;
        let _ = session::keys::session_enter_key(self);
        anyhow::ensure!(
            self.active_host == *b
                && self.active_workspace_id.as_deref() == Some("project")
                && self.bl_pane_target.as_ref() == Some(&(b.clone(), "stored-b".into())),
            "Sessions-Enter reversed a session name into a slug"
        );
        self.on_workspace_created(
            a.clone(),
            Ok(crate::net::transport::WorkspaceCreatedInfo {
                workspace_id: "new-id".into(),
                slug: "created".into(),
                label: "created".into(),
                project_root: "<root>".into(),
                session_name: "created-target".into(),
            }),
        );
        anyhow::ensure!(std::iter::from_fn(|| a_rx.try_recv().ok()).any(|r|
            matches!(r, OutgoingReq::PtyOpen { target: Some(name), .. } if name == "created-target")), "create reply lost its supplied target");
        while b_rx.try_recv().is_ok() {}
        Ok(())
    }
    fn result_caption_roi_probe(
        &mut self,
        a: &HostKey,
        b: &HostKey,
        a_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        b_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    ) -> Result<()> {
        self.switch_to_workspace(a.clone(), Some("project".into()), None, false);
        let command = |workspace: &str, caption: &str| FeCommand::Preview {
            workspace: workspace.into(),
            path: "same.png".into(),
            urgent: false,
            roi: Some(RoiRect {
                x: 1,
                y: 2,
                w: 3,
                h: 4,
            }),
            caption: Some(caption.into()),
        };
        self.dispatch_fe_command(Some(a), command("project", "caption a"));
        self.dispatch_fe_command(Some(b), command("id-b", "caption b"));
        let rect = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 320.0,
            h: 240.0,
        };
        self.preview_node_id_fired = Some("files:same.png".into());
        anyhow::ensure!(
            self.build_caption(rect).is_some()
                && self.caption_label.as_ref().unwrap().buffer.lines[0].text() == "caption a",
            "host-qualified caption render read another host's same file"
        );
        self.install_result_probe_image(a);
        anyhow::ensure!(
            self.pending_roi_aim.as_ref().is_some_and(|aim| !aim.ready),
            "another host's same file certified ROI readiness"
        );
        while a_rx.try_recv().is_ok() {}
        self.paint_result_probe_image(rect)?;
        anyhow::ensure!(
            self.pending_roi_aim.is_some() && a_rx.try_recv().is_err(),
            "another host's ROI was consumed or reported by the active renderer"
        );
        self.switch_to_workspace(b.clone(), Some("project".into()), None, false);
        self.dispatch_fe_command(Some(b), command("id-b", "caption b"));
        self.install_result_probe_image(b);
        anyhow::ensure!(
            self.pending_roi_aim.as_ref().is_some_and(|aim| aim.ready),
            "matching ROI was not certified ready"
        );
        anyhow::ensure!(
            self.build_caption(rect).is_some()
                && self.caption_label.as_ref().unwrap().buffer.lines[0].text() == "caption b",
            "matching host caption did not reach its renderer"
        );
        while b_rx.try_recv().is_ok() {}
        self.paint_result_probe_image(rect)?;
        let reports: Vec<_> = std::iter::from_fn(|| b_rx.try_recv().ok())
            .filter_map(|req| {
                if let OutgoingReq::AgentSend { text, .. } = req {
                    serde_json::from_str::<serde_json::Value>(&text).ok()
                } else {
                    None
                }
            })
            .collect();
        anyhow::ensure!(
            self.pending_roi_aim.is_none()
                && reports.len() == 1
                && reports[0]["evt"] == "preview_roi_applied"
                && reports[0]["ws"] == "id-b"
                && reports[0]["path"] == "same.png",
            "matching ROI did not apply once with its public workspace spelling"
        );
        Ok(())
    }

    fn install_result_probe_image(&mut self, host: &HostKey) {
        self.on_preview(
            host.clone(),
            Some("files:same.png".into()),
            self.active_workspace_id.clone(),
            "image/png".into(),
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png")).to_vec(),
            None,
            self.preview_req_gen,
        );
    }

    fn paint_result_probe_image(&mut self, rect: ScreenRect) -> Result<()> {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("result fixture"),
            size: wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("result fixture"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("result fixture"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            self.paint_preview_png(&mut pass, Some(rect), rect)?;
        }
        self.queue.submit([encoder.finish()]);
        Ok(())
    }
}

// ---- canonical result lifetime and attempt admission (ADR 0025) ----

type ProbeRx = tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>;
type ProbeEvents = std::sync::mpsc::Sender<(HostKey, crate::net::transport::IncomingEvt)>;

/// How a probe answers the tree requests State sends.
#[derive(Clone, Copy, PartialEq)]
enum Hold {
    Nothing,
    Children,
    Everything,
}

/// A tree request State sent, written through the real sender and pending map.
#[derive(Debug)]
struct WiredTree {
    id: u64,
    op: String,
    node_id: Option<String>,
    /// Sent as a result's own request, so its reply is the result's to complete.
    owned: bool,
}

struct Served {
    held: Vec<WiredTree>,
    previews: Vec<(String, u64)>,
}

/// The real request sender and response dispatcher, kept across the steps of one probe.
struct TreeWire<'a> {
    runtime: tokio::runtime::Runtime,
    driver: crate::net::transport::ResultTreeTestDriver<'a>,
    next_id: u64,
}

fn tree_json(paths: &[&str], parent: &str) -> Vec<serde_json::Value> {
    let prefix = if parent.is_empty() {
        String::new()
    } else {
        format!("{parent}/")
    };
    let mut dirs = std::collections::BTreeSet::new();
    let mut files = std::collections::BTreeSet::new();
    for path in paths {
        if let Some(rest) = path.strip_prefix(&prefix) {
            match rest.split_once('/') {
                Some((dir, _)) => {
                    dirs.insert(dir.to_string());
                }
                None => {
                    files.insert(rest.to_string());
                }
            }
        }
    }
    let entry = |name: &str, has_children: bool| {
        serde_json::json!({
            "id": format!("files:{prefix}{name}"),
            "label": name,
            "kind": "files",
            "has_children": has_children,
        })
    };
    dirs.iter()
        .map(|name| entry(name, true))
        .chain(files.iter().map(|name| entry(name, false)))
        .collect()
}

impl<'a> TreeWire<'a> {
    fn new(host: &HostKey, events: &'a ProbeEvents) -> Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Builder::new_current_thread().build()?,
            driver: crate::net::transport::ResultTreeTestDriver::new(host.clone(), events),
            next_id: 100,
        })
    }

    /// Send a State-emitted tree request through the real sender; `None` for any other request.
    fn register(&mut self, request: OutgoingReq) -> Result<Option<WiredTree>> {
        let shape = format!("{request:?}");
        if !["ResultTree", "TreeRoot", "TreeChildren"]
            .iter()
            .any(|kind| shape.starts_with(kind))
        {
            return Ok(None);
        }
        self.next_id += 1;
        let id = self.next_id;
        let mut wire = Vec::new();
        self.runtime
            .block_on(self.driver.send(&mut wire, id, request))?;
        let (frame, _) = self.runtime.block_on(sot_protocol::codec::read_frame(
            &mut sot_protocol::codec::buffered(&wire[..]),
        ))?;
        Ok(Some(WiredTree {
            id,
            op: frame.op.clone(),
            node_id: frame
                .payload
                .get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            owned: shape.starts_with("ResultTree"),
        }))
    }

    fn reply_with(&mut self, tree: &WiredTree, payload: serde_json::Value) {
        self.driver
            .reply(sot_protocol::Frame::res(tree.id, &tree.op, payload));
    }

    fn reply(&mut self, tree: &WiredTree, paths: &[&str]) {
        let payload = if tree.op == "tree.root" {
            serde_json::json!({
                "node": {"id": "files:", "label": "project", "kind": "files", "has_children": true},
                "children": tree_json(paths, ""),
            })
        } else {
            let parent = tree
                .node_id
                .as_deref()
                .and_then(|id| id.strip_prefix("files:"))
                .unwrap_or("");
            serde_json::json!({ "children": tree_json(paths, parent) })
        };
        self.reply_with(tree, payload);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum OldReply {
    Success,
    Error,
    Malformed,
}

/// One replacement case: the first and second result, whether a new result replaces the first, and how the first attempt's reply arrives.
struct AttemptCase {
    label: &'static str,
    first: &'static str,
    second: &'static str,
    replace: bool,
    old_reply: OldReply,
    parked: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Gone {
    Listing,
    Destroy,
    Replacement,
}

/// One removal case: how the row goes, whether an attempt is in flight when it goes, and the successor's result.
struct LifeCase {
    label: &'static str,
    gone: Gone,
    inflight: bool,
    successor: &'static str,
}

type WorkspaceInfoRow = crate::net::transport::WorkspaceInfo;

fn destroyed_info(id: &str) -> crate::net::transport::WorkspaceDestroyedInfo {
    crate::net::transport::WorkspaceDestroyedInfo {
        workspace_id: id.into(),
        slug: "project".into(),
        label: "project".into(),
        tmux_killed: true,
        toml_removed: true,
        kept: None,
    }
}

fn probe_row(
    slug: &str,
    workspace_id: &str,
    default: bool,
) -> crate::net::transport::WorkspaceInfo {
    let mut row = ws_info(slug, &format!("{slug}-session"));
    row.workspace_id = workspace_id.into();
    row.is_default = default;
    row
}

fn show_command(path: &str) -> FeCommand {
    FeCommand::Preview {
        workspace: "project".into(),
        path: path.into(),
        urgent: false,
        roi: None,
        caption: None,
    }
}

fn goto_command(slug: &str) -> FeCommand {
    FeCommand::Workspace {
        slug: Some(slug.into()),
    }
}

impl State {
    /// A fresh host with a default row and a `project` row, routed through the fixture's own channels.
    fn prepare_lifetime_probe(
        &mut self,
        host: &str,
        project_id: &str,
    ) -> (HostKey, ProbeRx, ProbeEvents) {
        let host = host.to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.conns.push((host.clone(), tx));
        let (events, receiver) = std::sync::mpsc::channel();
        self.evt_rx = receiver;
        self.active_host = host.clone();
        self.active_workspace_id = None;
        self.on_workspaces(
            host.clone(),
            vec![
                probe_row("base", &format!("id-base-{host}"), true),
                probe_row("project", project_id, false),
            ],
        );
        (host, rx, events)
    }

    /// Answer root requests at once; hold or answer the rest. Returns what is still held and the previews asked for.
    fn serve_result_requests(
        &mut self,
        wire: &mut TreeWire<'_>,
        rx: &mut ProbeRx,
        paths: &[&str],
        hold: Hold,
    ) -> Result<Served> {
        let mut served = Served {
            held: Vec::new(),
            previews: Vec::new(),
        };
        for _ in 0..8 {
            let requests: Vec<OutgoingReq> =
                std::iter::from_fn(|| rx.try_recv().ok()).collect();
            if requests.is_empty() {
                break;
            }
            for request in requests {
                if let OutgoingReq::PreviewGet {
                    node_id,
                    generation,
                    ..
                } = &request
                {
                    served.previews.push((node_id.clone(), *generation));
                    continue;
                }
                let Some(tree) = wire.register(request)? else {
                    continue;
                };
                let held = match hold {
                    Hold::Everything => true,
                    Hold::Children => tree.op == "tree.children",
                    Hold::Nothing => false,
                };
                if held {
                    served.held.push(tree);
                } else {
                    wire.reply(&tree, paths);
                }
            }
            self.drain_events();
        }
        Ok(served)
    }

    fn probe_sees_badge(&self, host: &HostKey) -> bool {
        self.badged_keys()
            .contains(&(host.clone(), "project".into()))
    }

    fn probe_selected_id(&self) -> Option<String> {
        self.tree
            .rows
            .get(self.tree.selected)
            .map(|row| row.node.id.clone())
    }

    fn probe_install_preview(&mut self, host: &HostKey, node_id: &str, generation: u64) {
        self.on_preview(
            host.clone(),
            Some(node_id.to_string()),
            self.active_workspace_id.clone(),
            "text/plain".into(),
            b"fn shown() {}\n".to_vec(),
            None,
            generation,
        );
    }

    /// Everything an obsolete completion must leave alone.
    fn probe_effects(
        &self,
        host: &HostKey,
    ) -> (Vec<String>, usize, String, bool, bool, Option<String>, bool) {
        (
            self.tree
                .rows
                .iter()
                .map(|row| row.node.id.clone())
                .collect(),
            self.tree.selected,
            self.status.clone(),
            self.pending_reveal.is_some(),
            self.pending_switch_reveal.is_some(),
            self.reveal_awaiting.clone(),
            self.probe_sees_badge(host),
        )
    }

    fn parked_project_rows(&mut self, host: &HostKey) -> Vec<String> {
        let key: TreeKey = (
            Mode::Files,
            TreeScope::Workspace((host.clone(), "project".into())),
        );
        self.tree_store
            .slot_mut(key)
            .view
            .rows
            .iter()
            .map(|row| row.node.id.clone())
            .collect()
    }

    /// The newest attempt's preview arrives and a frame is presented: that clears the badge, and nothing earlier did.
    fn finish_probe_attempt(
        &mut self,
        label: &str,
        host: &HostKey,
        previews: &[(String, u64)],
        target_id: &str,
    ) -> Result<()> {
        let (node, generation) = previews
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{label}: the replacement asked for no preview"))?;
        anyhow::ensure!(
            node == target_id,
            "{label}: the replacement previewed {node}"
        );
        self.probe_install_preview(host, &node, generation);
        anyhow::ensure!(
            self.probe_sees_badge(host),
            "{label}: the badge cleared before any presentation"
        );
        self.redraw()?;
        anyhow::ensure!(
            !self.probe_sees_badge(host),
            "{label}: the matching presentation did not clear the badge"
        );
        Ok(())
    }

    pub(in crate::ui) fn replaced_result_rejects_old_tree_attempt_before_effects(
        &mut self,
    ) -> Result<()> {
        println!("result-attempt phase=body-entered entered_bodies=1");
        let case = |label, first, second, replace, old_reply, parked| AttemptCase {
            label,
            first,
            second,
            replace,
            old_reply,
            parked,
        };
        for case in [
            case(
                "sibling",
                "src/a.rs",
                "src/b.rs",
                true,
                OldReply::Success,
                false,
            ),
            case(
                "same-node",
                "src/b.rs",
                "src/b.rs",
                true,
                OldReply::Success,
                false,
            ),
            case(
                "retry",
                "src/b.rs",
                "src/b.rs",
                false,
                OldReply::Success,
                false,
            ),
            case(
                "error",
                "src/a.rs",
                "src/b.rs",
                true,
                OldReply::Error,
                false,
            ),
            case(
                "malformed",
                "src/a.rs",
                "src/b.rs",
                true,
                OldReply::Malformed,
                false,
            ),
            case(
                "parked",
                "src/a.rs",
                "src/b.rs",
                true,
                OldReply::Success,
                true,
            ),
        ] {
            self.replacement_case(&case)?;
            println!(
                "result-attempt case={} admitted_once=true entered_bodies=1",
                case.label
            );
        }
        println!("result-attempt phase=completed ok=true entered_bodies=1 completed_bodies=1");
        Ok(())
    }

    /// The old attempt's reply arrives; whatever the replacement shows must not move.
    fn deliver_old_reply(
        &mut self,
        wire: &mut TreeWire<'_>,
        old: &WiredTree,
        case: &AttemptCase,
        paths: &[&str],
    ) {
        match case.old_reply {
            OldReply::Success => wire.reply(old, paths),
            OldReply::Error => wire.reply_with(
                old,
                serde_json::json!({"error": "old attempt failed", "code": "x"}),
            ),
            OldReply::Malformed => wire.reply_with(old, serde_json::json!({"nonsense": 1})),
        }
        self.drain_events();
    }

    fn replacement_case(&mut self, case: &AttemptCase) -> Result<()> {
        const REJECTED: &str =
            "obsolete result tree completion changed the replacement attempt before admission";
        let label = case.label;
        let paths = ["src/a.rs", "src/b.rs"];
        let (a, mut rx, events) =
            self.prepare_lifetime_probe(&format!("<host-attempt-{label}>"), "id-attempt");
        let mut wire = TreeWire::new(&a, &events)?;
        // The first attempt waits on the children of src.
        self.dispatch_fe_command(Some(&a), show_command(case.first));
        self.dispatch_fe_command(Some(&a), goto_command("project"));
        let mut first =
            self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Children)?;
        anyhow::ensure!(
            first.held.len() == 1 && first.held[0].node_id.as_deref() == Some("files:src"),
            "{label}: the first attempt did not wait on the children of src"
        );
        let old = first.held.remove(0);
        // The row is left, then shown again for the same or a replacing result, on the same parent.
        self.switch_to_workspace(a.clone(), None, None, false);
        self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
        if case.replace {
            self.dispatch_fe_command(Some(&a), show_command(case.second));
        }
        if case.parked {
            let before = (
                self.parked_project_rows(&a),
                self.tree.rows.len(),
                self.status.clone(),
            );
            self.deliver_old_reply(&mut wire, &old, case, &paths);
            let after = (
                self.parked_project_rows(&a),
                self.tree.rows.len(),
                self.status.clone(),
            );
            anyhow::ensure!(before == after, "{REJECTED}");
        }
        self.dispatch_fe_command(Some(&a), goto_command("project"));
        let second = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Children)?;
        let same_parent = |tree: &&WiredTree| tree.node_id.as_deref() == Some("files:src");
        anyhow::ensure!(
            second.held.iter().filter(same_parent).count() == 2
                && second.held.last().is_some_and(|tree| same_parent(&tree)),
            "{label}: the replacement did not ask for the same parent: {:?}",
            second.held
        );
        let (own, refreshes) = second.held.split_last().expect("held is not empty");
        let target_id = format!("files:{}", case.second);
        // The old completion arrives while the replacement waits on the same parent.
        if !case.parked {
            let before = self.probe_effects(&a);
            self.deliver_old_reply(&mut wire, &old, case, &paths);
            anyhow::ensure!(
                before == self.probe_effects(&a)
                    && std::iter::from_fn(|| rx.try_recv().ok()).next().is_none(),
                "{REJECTED}"
            );
        }
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "{label}: the badge was cleared when the attempt started"
        );
        // The ordinary refreshes of the revisited tree install their rows and finish nothing of the result's.
        for refresh in refreshes {
            wire.reply(refresh, &paths);
        }
        self.drain_events();
        anyhow::ensure!(
            self.probe_selected_id().as_deref() != Some(target_id.as_str())
                && self.probe_sees_badge(&a),
            "{label}: an ordinary tree reply completed a result-owned reveal"
        );
        // Only the replacement's own reply lands the cursor.
        wire.reply(own, &paths);
        self.drain_events();
        anyhow::ensure!(
            self.probe_selected_id().as_deref() == Some(target_id.as_str())
                && self.probe_sees_badge(&a),
            "{label}: the attempt's own reply did not land the cursor, or cleared the badge"
        );
        self.finish_probe_attempt(label, &a, &second.previews, &target_id)
    }

    /// The picker sends its selected account by name, wherever "default" sits in the list.
    #[allow(
        dead_code,
        reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one"
    )]
    pub(in crate::ui) fn first_named_account_is_sent(&mut self) -> Result<()> {
        println!("account-choice phase=body-entered entered_bodies=1");
        let (a, mut rx, _events) = self.prepare_lifetime_probe("<host-account>", "id-account");
        while rx.try_recv().is_ok() {}
        let cases: [(&str, &[&str], usize, Option<&str>); 7] = [
            ("no default", &["team", "other"], 0, Some("team")),
            ("default first, default chosen", &["default", "team"], 0, None),
            ("default first, second chosen", &["default", "team"], 1, Some("team")),
            ("default later, first chosen", &["team", "default"], 0, Some("team")),
            ("default later, default chosen", &["team", "default"], 1, None),
            ("empty list", &[], 0, None),
            ("stale index", &["default", "team"], 5, None),
        ];
        for (label, names, selected, want) in cases {
            self.begin_create_session(a.clone());
            let accounts = names
                .iter()
                .map(|name| crate::net::transport::AccountInfo {
                    name: name.to_string(),
                    kinds: Vec::new(),
                    logged_in: Default::default(),
                })
                .collect();
            let picker = self.workspace_picker.as_mut().ok_or_else(|| anyhow::anyhow!("{label}: no picker"))?;
            picker.accounts = accounts;
            picker.account_selected = selected;
            self.picker_confirm_selected("claude");
            self.picker_cancel();
            let sent = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|request| match request {
                OutgoingReq::WorkspaceCreate { account, .. } => Some(account),
                _ => None,
            });
            anyhow::ensure!(
                sent == Some(want.map(str::to_string)),
                "first_named_account_is_sent: {label}: sent {sent:?}, wanted {want:?}"
            );
        }
        println!("account-choice phase=completed ok=true entered_bodies=1 completed_bodies=1");
        Ok(())
    }

    pub(in crate::ui) fn passing_a_result_row_keeps_its_badge(&mut self) -> Result<()> {
        println!("result-badge phase=body-entered entered_bodies=1");
        let (a, mut rx, events) = self.prepare_lifetime_probe("<host-badge>", "id-badge");
        let mut wire = TreeWire::new(&a, &events)?;
        let paths = ["top.rs", "other.rs"];
        // Preview first, cursor second: neither alone clears the badge.
        self.dispatch_fe_command(Some(&a), show_command("top.rs"));
        self.dispatch_fe_command(Some(&a), goto_command("project"));
        let served =
            self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Everything)?;
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "passing_a_result_row_keeps_its_badge: the badge was cleared when the attempt started"
        );
        let (node, generation) = served
            .previews
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no preview asked for"))?;
        self.probe_install_preview(&a, &node, generation);
        self.redraw()?;
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "a matching preview without its cursor cleared the badge"
        );
        for tree in &served.held {
            wire.reply(tree, &paths);
        }
        self.drain_events();
        self.redraw()?;
        anyhow::ensure!(
            !self.probe_sees_badge(&a),
            "cursor and preview with a presentation did not clear the badge"
        );
        self.redraw()?;
        anyhow::ensure!(
            !self.probe_sees_badge(&a),
            "a later frame brought the badge back"
        );
        // A failed preview, a stale generation and another row's frame keep the next attempt's badge.
        self.switch_to_workspace(a.clone(), None, None, false);
        self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
        self.dispatch_fe_command(Some(&a), show_command("top.rs"));
        self.dispatch_fe_command(Some(&a), goto_command("project"));
        let served = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
        let (node, generation) = served
            .previews
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no preview asked for"))?;
        self.redraw()?;
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "a cursor without its preview cleared the badge"
        );
        self.on_preview_get_failed(
            a.clone(),
            Some(node.clone()),
            self.active_workspace_id.clone(),
            generation,
            "kernel_unavailable".into(),
        );
        self.redraw()?;
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "a failed preview cleared the badge"
        );
        self.probe_install_preview(&a, &node, generation.saturating_sub(1));
        self.redraw()?;
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "a stale preview generation cleared the badge"
        );
        self.switch_to_workspace(a.clone(), None, None, false);
        self.redraw()?;
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "another row's presented frame cleared the badge"
        );
        self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
        // Returning retries with the replacing result and clears it once presented.
        self.dispatch_fe_command(Some(&a), show_command("other.rs"));
        self.dispatch_fe_command(Some(&a), goto_command("project"));
        let served = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
        let (node, generation) = served
            .previews
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no preview asked for"))?;
        anyhow::ensure!(
            node == "files:other.rs",
            "the retry previewed {node}, not the replacing result"
        );
        self.probe_install_preview(&a, &node, generation);
        self.redraw()?;
        anyhow::ensure!(
            !self.probe_sees_badge(&a),
            "the retry's presentation did not clear the badge"
        );
        println!("result-badge phase=completed ok=true entered_bodies=1 completed_bodies=1");
        Ok(())
    }

    /// The successor's own completions acknowledge it once.
    fn finish_probe_successor(
        &mut self,
        label: &str,
        host: &HostKey,
        wire: &mut TreeWire<'_>,
        rx: &mut ProbeRx,
        paths: &[&str],
        new: &Served,
    ) -> Result<()> {
        for tree in &new.held {
            wire.reply(tree, paths);
        }
        self.drain_events();
        self.serve_result_requests(wire, rx, paths, Hold::Nothing)?;
        let (node, generation) = new
            .previews
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{label}: no preview"))?;
        self.probe_install_preview(host, &node, generation);
        self.redraw()?;
        anyhow::ensure!(
            !self.probe_sees_badge(host),
            "{label}: the successor's presentation did not clear its result"
        );
        Ok(())
    }

    pub(in crate::ui) fn pending_result_dies_with_its_canonical_row(&mut self) -> Result<()> {
        println!("result-lifetime phase=body-entered entered_bodies=1");
        let case = |label, gone, inflight, successor| LifeCase {
            label,
            gone,
            inflight,
            successor,
        };
        for case in [
            case("listing", Gone::Listing, false, "other.rs"),
            case("replacement", Gone::Replacement, false, "other.rs"),
            case("listing-inflight", Gone::Listing, true, "other.rs"),
            case(
                "listing-inflight-same-path",
                Gone::Listing,
                true,
                "src/a.rs",
            ),
            case("destroy-inflight", Gone::Destroy, true, "other.rs"),
            case("replacement-inflight", Gone::Replacement, true, "src/a.rs"),
        ] {
            self.removal_case(&case)?;
            println!("result-lifetime case={} entered_bodies=1", case.label);
        }
        self.pending_result_controls()?;
        println!("result-lifetime phase=completed ok=true entered_bodies=1 completed_bodies=1");
        Ok(())
    }

    /// The row goes (by a list, a destroy or a same-slug replacement) with its result owed or in flight.
    fn remove_probe_row(
        &mut self,
        case: &LifeCase,
        host: &HostKey,
        base: &WorkspaceInfoRow,
    ) -> Result<()> {
        let label = case.label;
        match case.gone {
            Gone::Destroy => {
                self.on_workspace_destroyed(host.clone(), Err("refused".into()));
                anyhow::ensure!(
                    self.pending_reveal.is_some() || self.pending_switch_reveal.is_some(),
                    "{label}: a failed destroy aborted the attempt"
                );
                self.on_workspace_destroyed(host.clone(), Ok(destroyed_info("old-id")));
            }
            Gone::Listing => self.on_workspaces(host.clone(), vec![base.clone()]),
            Gone::Replacement => self.on_workspaces(
                host.clone(),
                vec![base.clone(), probe_row("project", "new-id", false)],
            ),
        }
        let gone = "removed canonical row retained its pending result or same-slug successor inherited it";
        anyhow::ensure!(
            !self
                .badged_keys()
                .contains(&(host.clone(), "project".into()))
                && !(case.inflight
                    && (self.pending_reveal.is_some() || self.pending_switch_reveal.is_some())),
            "{gone}"
        );
        // A successor row with the same slug is listed.
        if case.gone != Gone::Replacement {
            self.on_workspaces(
                host.clone(),
                vec![base.clone(), probe_row("project", "new-id", false)],
            );
        }
        anyhow::ensure!(!self.probe_sees_badge(host), "{gone}");
        Ok(())
    }

    fn removal_case(&mut self, case: &LifeCase) -> Result<()> {
        let label = case.label;
        let paths = ["src/a.rs", "other.rs"];
        let host = format!("<host-life-{label}>");
        let (a, mut rx, events) = self.prepare_lifetime_probe(&host, "old-id");
        let mut wire = TreeWire::new(&a, &events)?;
        let base = probe_row("base", &format!("id-base-{host}"), true);
        self.dispatch_fe_command(Some(&a), show_command("src/a.rs"));
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "{label}: the result was not owed"
        );
        let mut old = Served {
            held: Vec::new(),
            previews: Vec::new(),
        };
        if case.inflight {
            self.dispatch_fe_command(Some(&a), goto_command("project"));
            old = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Everything)?;
            anyhow::ensure!(
                !old.held.is_empty(),
                "{label}: the first attempt did not start"
            );
        }
        self.remove_probe_row(case, &a, &base)?;
        self.switch_to_workspace(a.clone(), None, None, false);
        self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
        self.dispatch_fe_command(Some(&a), show_command(case.successor));
        self.dispatch_fe_command(Some(&a), goto_command("project"));
        let new = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Everything)?;
        anyhow::ensure!(
            self.probe_sees_badge(&a),
            "{label}: the successor's result was not owed"
        );
        // Everything the removed row's attempt still had in flight arrives now.
        let before = self.probe_effects(&a);
        for tree in old.held.iter().filter(|tree| tree.owned) {
            wire.reply(tree, &paths);
        }
        self.drain_events();
        if let Some((old_node, old_generation)) = old.previews.last() {
            self.probe_install_preview(&a, old_node, *old_generation);
        }
        self.redraw()?;
        let after = self.probe_effects(&a);
        anyhow::ensure!(
            before.0 == after.0
                && before.1 == after.1
                && self.probe_sees_badge(&a)
                && std::iter::from_fn(|| rx.try_recv().ok()).next().is_none(),
            "{label}: a late completion of the removed row changed its successor"
        );
        self.finish_probe_successor(label, &a, &mut wire, &mut rx, &paths, &new)
    }

    /// What leaves an owed result alone: an unchanged list, another host's row with the same id, a
    /// disconnect, a kept default row and a late destroy of a replaced id.
    fn pending_result_controls(&mut self) -> Result<()> {
        let (a, _rx_a, _events_a) =
            self.prepare_lifetime_probe("<host-control-a>", "shared-id");
        let (b, _rx_b, _events_b) =
            self.prepare_lifetime_probe("<host-control-b>", "shared-id");
        self.dispatch_fe_command(Some(&a), show_command("one.rs"));
        self.dispatch_fe_command(Some(&b), show_command("two.rs"));
        anyhow::ensure!(
            self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
            "control: results were not owed"
        );
        let rows = |host: &str, id: &str| {
            vec![
                probe_row("base", &format!("id-base-{host}"), true),
                probe_row("project", id, false),
            ]
        };
        self.on_workspaces(a.clone(), rows("<host-control-a>", "shared-id"));
        self.on_disconnected(a.clone(), "link lost".into(), false);
        self.on_workspace_destroyed(
            a.clone(),
            Ok(crate::net::transport::WorkspaceDestroyedInfo {
                workspace_id: "id-base-<host-control-a>".into(),
                slug: "base".into(),
                label: "base".into(),
                tmux_killed: false,
                toml_removed: false,
                kept: Some("run ended".into()),
            }),
        );
        anyhow::ensure!(
            self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
            "control: an unchanged list, a disconnect or a kept default row removed an owed result"
        );
        // The same slug and id on one host is replaced; the other host's row keeps its result.
        self.on_workspaces(a.clone(), rows("<host-control-a>", "second-id"));
        anyhow::ensure!(
            !self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
            "control: replacing a row on one host touched the other host's row with the same id"
        );
        self.dispatch_fe_command(Some(&a), show_command("three.rs"));
        self.on_workspace_destroyed(a.clone(), Ok(destroyed_info("shared-id")));
        anyhow::ensure!(
            self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
            "control: a late destroy of the replaced id removed its successor's result"
        );
        println!("result-lifetime controls=true entered_bodies=1");
        Ok(())
    }
}
