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
        self.selected.select(a.clone(), Some("project".into()));
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
        anyhow::ensure!(*self.selected.host() == *a, "docs probe: the window must view host a");
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
                && !self.result_ledger.has_badge(&(b.clone(), "id-b".into())),
            "canonical spelling created a phantom strip key"
        );
        let before = (
            self.selected.host().clone(),
            self.selected.workspace_id().clone(),
            self.result_ledger.clone(),
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
                    self.selected.host().clone(),
                    self.selected.workspace_id().clone(),
                    self.result_ledger.clone()
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
                        self.selected.host().clone(),
                        self.selected.workspace_id().clone(),
                        self.result_ledger.clone()
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
        anyhow::ensure!(*self.selected.host() == *b && self.selected.workspace_id().as_deref() == Some("project")
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
            self.agent_pane.target().as_ref() == Some(&(b.clone(), "default-b".into())),
            "cycle derived an attachment name"
        );
        self.dispatch_fe_command(
            Some(a),
            FeCommand::Workspace {
                slug: Some("default".into()),
            },
        );
        anyhow::ensure!(
            *self.selected.host() == *a
                && self.selected.workspace_id().is_none()
                && self.row_presentation.default_slug() == Some("base-a"),
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
        self.nav
            .set_root(node("sessions:", "sessions", true), vec![selected]);
        self.nav.select(1);
        anyhow::ensure!(
            self.agent_pane.target().as_ref() != Some(&(b.clone(), "stored-b".into())),
            "Sessions-Enter needs a different departing attachment"
        );
        let _ = session::keys::session_enter_key(self);
        anyhow::ensure!(
            *self.selected.host() == *b
                && self.selected.workspace_id().as_deref() == Some("project")
                && self.agent_pane.target().as_ref() == Some(&(b.clone(), "stored-b".into())),
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
        self.preview_requests.record_fired("files:same.png".into());
        anyhow::ensure!(
            self.build_caption(rect).is_some()
                && self.caption_label.as_ref().unwrap().buffer.lines[0].text() == "caption a",
            "host-qualified caption render read another host's same file"
        );
        self.install_result_probe_image(a);
        anyhow::ensure!(
            self.preview_requests.roi_aim().is_some_and(|aim| !aim.ready),
            "another host's same file certified ROI readiness"
        );
        while a_rx.try_recv().is_ok() {}
        self.paint_result_probe_image(rect)?;
        anyhow::ensure!(
            self.preview_requests.roi_aim().is_some() && a_rx.try_recv().is_err(),
            "another host's ROI was consumed or reported by the active renderer"
        );
        self.switch_to_workspace(b.clone(), Some("project".into()), None, false);
        self.dispatch_fe_command(Some(b), command("id-b", "caption b"));
        self.install_result_probe_image(b);
        anyhow::ensure!(
            self.preview_requests.roi_aim().is_some_and(|aim| aim.ready),
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
            self.preview_requests.roi_aim().is_none()
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
            self.selected.workspace_id().clone(),
            "image/png".into(),
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png")).to_vec(),
            None,
            self.preview_requests.preview_generation(),
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
        self.selected.select(host.clone(), None);
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
        self.nav.tree()
            .rows()
            .get(self.nav.tree().selected())
            .map(|row| row.node.id.clone())
    }

    fn probe_install_preview(&mut self, host: &HostKey, node_id: &str, generation: u64) {
        self.on_preview(
            host.clone(),
            Some(node_id.to_string()),
            self.selected.workspace_id().clone(),
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
            self.nav.tree()
                .rows()
                .iter()
                .map(|row| row.node.id.clone())
                .collect(),
            self.nav.tree().selected(),
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
        self.nav
            .parked(&key)
            .map(|view| view.rows().iter().map(|row| row.node.id.clone()).collect())
            .unwrap_or_default()
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
                self.nav.tree().rows().len(),
                self.status.clone(),
            );
            self.deliver_old_reply(&mut wire, &old, case, &paths);
            let after = (
                self.parked_project_rows(&a),
                self.nav.tree().rows().len(),
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
        // The selection is reached as a person reaches it, by the reply and Tab; an out-of-range index is
        // unreachable that way and is `session::picker::tests::selected_account_is_chosen_by_name_not_position`'s.
        let cases: [(&str, &[&str], usize, Option<&str>); 6] = [
            ("no default", &["team", "other"], 0, Some("team")),
            ("default first, default chosen", &["default", "team"], 0, None),
            ("default first, second chosen", &["default", "team"], 1, Some("team")),
            ("default later, first chosen", &["team", "default"], 0, Some("team")),
            ("default later, default chosen", &["team", "default"], 1, None),
            ("empty list", &[], 0, None),
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
            anyhow::ensure!(self.workspace_picker.is_some(), "{label}: no picker");
            self.on_accounts_list(a.clone(), accounts);
            for _ in 0..selected {
                self.picker_cycle_account();
            }
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
            self.selected.workspace_id().clone(),
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
        anyhow::ensure!(
            self.row_directory.resolve(&a, "shared-id").is_ok()
                && self.row_directory.resolve(&b, "shared-id").is_ok(),
            "control: a disconnect removed a directory listing"
        );
        let retained = self.row_directory.prepare_replace(a.clone(), rows("<host-control-a>", "shared-id"));
        anyhow::ensure!(retained.retired().is_empty(), "control: a disconnect retired a canonical identity");
        self.kept_default_preserves_its_result_and_view(&a, &b)?;
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

    /// A kept default retains its own owed result and its cached view; only its attachment is invalidated.
    fn kept_default_preserves_its_result_and_view(
        &mut self,
        kept_host: &HostKey,
        other_host: &HostKey,
    ) -> Result<()> {
        self.switch_to_workspace(kept_host.clone(), None, None, false);
        let default = self.active_result_workspace()
            .ok_or_else(|| anyhow::anyhow!("control: default row did not resolve"))?;
        let node_id = "files:kept-default.rs";
        let preview = ("text/plain".to_string(), b"kept default preview\n".to_vec());
        self.nav.set_mode(Mode::Files);
        self.nav.set_root(
            node("files:", "base", true),
            vec![node(node_id, "kept-default.rs", false)],
        );
        self.nav.select(1);
        self.preview_requests.record_fired(node_id.into());
        self.preview_source.install(preview.0.clone(), preview.1.clone(), Some(node_id.into()), None, None);
        self.repl_model.edit_draft(DraftEdit::Replace("kept_default_input = 42".into()));
        // Park through the real switch so the tree, preview and REPL use their normal snapshot keys.
        self.switch_to_workspace(other_host.clone(), None, None, false);
        self.dispatch_fe_command(Some(kept_host), FeCommand::Preview {
            workspace: default.identity().workspace_id.clone(),
            path: "kept-default.rs".into(),
            urgent: false,
            roi: None,
            caption: None,
        });
        anyhow::ensure!(
            self.pending_result_path(kept_host, "base").as_deref() == Some("kept-default.rs"),
            "control: the default row's own result was not owed"
        );
        let owed = self.result_ledger.clone();
        self.on_workspace_destroyed(kept_host.clone(), Ok(crate::net::transport::WorkspaceDestroyedInfo {
            workspace_id: default.identity().workspace_id.clone(),
            slug: default.row_key().1.clone(),
            label: "base".into(),
            tmux_killed: false,
            toml_removed: false,
            kept: Some("run ended".into()),
        }));
        anyhow::ensure!(
            self.result_ledger == owed
                && self.pending_result_path(kept_host, "base").as_deref() == Some("kept-default.rs"),
            "control: keeping the default row removed or changed its own pending result"
        );
        // Check the parked cursor before result reveal can place it again on return.
        let tree_key = (Mode::Files, TreeScope::Workspace(default.view_key().clone()));
        let cached = self.nav.parked(&tree_key)
            .ok_or_else(|| anyhow::anyhow!("control: keeping the default row removed its cached tree"))?;
        let selected = cached.rows().get(cached.selected()).map(|row| row.node.id.clone());
        anyhow::ensure!(
            selected.as_deref() == Some(node_id),
            "control: keeping the default row reset its cached selection"
        );
        // No request is answered or frame presented here, so its badge must remain owed.
        self.switch_to_workspace(kept_host.clone(), None, None, false);
        anyhow::ensure!(
            self.active_ws_key() == *default.view_key()
                && self.nav.mode() == Mode::Files
                && self.nav.tree().selected() == 1
                && self.probe_selected_id().as_deref() == Some(node_id),
            "control: keeping the default row reset its cached tree or selection"
        );
        anyhow::ensure!(
            self.preview_source.source_copy().as_ref() == Some(&preview)
                && self.preview_source.node_id() == Some(node_id),
            "control: keeping the default row reset its cached preview"
        );
        anyhow::ensure!(
            self.repl_model.input() == "kept_default_input = 42",
            "control: keeping the default row reset its cached REPL input"
        );
        anyhow::ensure!(
            self.pending_result_path(kept_host, "base").as_deref() == Some("kept-default.rs"),
            "control: returning to the kept default cleared its result before presentation"
        );
        self.switch_to_workspace(other_host.clone(), None, None, false);
        Ok(())
    }
}

impl State {
    /// Drives the real wheel adapter on the isolated native State, including a sub-row step.
    pub(in crate::ui) fn a_wheel_step_reports_presence(&mut self) -> Result<()> {
        self.presence = PresenceClock::default();
        self.set_focus(PaneFocus::Llm);
        self.wheel_residue_y = 0.0;
        let fresh = self.presence;
        input::mouse::mouse_wheel(self, winit::keyboard::ModifiersState::empty(), MouseScrollDelta::LineDelta(0.0, 0.05));
        anyhow::ensure!(self.presence != fresh, "fractional wheel step did not advance presence");
        anyhow::ensure!(self.wheel_residue_y > 0.0 && self.wheel_residue_y < 1.0, "fractional control scrolled a whole row");
        let first = self.presence;
        input::mouse::mouse_wheel(self, winit::keyboard::ModifiersState::empty(), MouseScrollDelta::LineDelta(0.0, 1.0));
        anyhow::ensure!(self.presence != first, "whole wheel step did not advance presence");

        // Geometry stays with State: zero area refuses both a press and a clamped drag.
        let saved = (self.pane_rects.llm, self.cell_w, self.cell_h, self.chrome_origin_x, self.chrome_origin_y);
        self.pane_rects.llm = ratatui::layout::Rect::new(0, 0, 0, 3);
        let empty = self.llm_cell_at_px((0.0, 0.0), true);
        anyhow::ensure!(empty.is_none() && self.llm_cell_at_px((0.0, 0.0), false).is_none(), "zero-area pane mapped a cell");
        self.agent_pane.press(empty);
        anyhow::ensure!(self.agent_pane.selection().is_none(), "zero-area press began a selection");
        self.pane_rects.llm = ratatui::layout::Rect::new(3, 4, 8, 3);
        self.cell_w = 1.0;
        self.cell_h = 1.0;
        self.chrome_origin_x = 0.0;
        self.chrome_origin_y = 0.0;
        anyhow::ensure!(self.llm_cell_at_px((-1.0, -1.0), true).is_none()
            && self.llm_cell_at_px((100.0, 100.0), false) == Some((2, 7)), "pane press/drag mapping lost rejection or clamping");
        (self.pane_rects.llm, self.cell_w, self.cell_h, self.chrome_origin_x, self.chrome_origin_y) = saved;
        Ok(())
    }
}
