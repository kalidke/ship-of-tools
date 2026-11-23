//! Park and return through the real State. Only the requests and replies cross fixture-owned channels.
use super::*;
use crate::ui::app::NAV_FIRE_DEBOUNCE;
use crate::ui::preview::pane::PREVIEW_RENDERS;
use anyhow::ensure;
use std::time::{Duration, Instant};

use crate::net::transport::{IncomingEvt, PreviewLink};
use tokio::sync::watch;

fn probe(state: &mut State, label: &str, id: &str) -> (HostKey, ProbeRx, ProbeEvents, watch::Sender<PreviewLink>) {
    let (host, rx, events) = state.prepare_lifetime_probe(label, id);
    let (link, _) = watch::channel(PreviewLink::Ready);
    state.hosts.preview_links.insert(host.clone(), link.clone());
    (host, rx, events, link)
}

const FILE: &str = "files:a.md";

fn visit(state: &mut State, host: &HostKey, row: Option<&str>) {
    state.switch_to_workspace(host.clone(), row.map(str::to_string), None, false);
}

fn reply(state: &mut State, node: Option<&str>, bytes: Vec<u8>, generation: u64) {
    state.on_preview(state.selected.host().clone(), node.map(str::to_string), state.selected.workspace_id().clone(),
        "text/markdown".into(), bytes, None, generation);
}

fn put(state: &mut State, node: Option<&str>, bytes: &[u8]) {
    let generation = state.next_preview_gen();
    if let Some(id) = node { state.preview_requests.record_fired(id.into()); }
    reply(state, node, bytes.to_vec(), generation);
}

// Advance the two frame inputs a person normally waits for. No scheduler timing enters the assertions.
fn settle(state: &mut State) -> Result<()> {
    state.nav_cursor.settled((state.nav.mode(), state.nav.tree().selected()), Duration::ZERO);
    state.redraw_at(Instant::now() + NAV_FIRE_DEBOUNCE)
}

fn back(state: &mut State, host: &HostKey) {
    visit(state, host, None);
    visit(state, host, Some("project"));
}

fn select_file(state: &mut State, node_id: &str) {
    let node = |id: &str, dir: bool| sot_protocol::TreeNode {
        id: id.into(), label: id.into(), kind: if dir { "dir" } else { "file" }.into(),
        has_children: dir, badges: Vec::new(), payload: Default::default(),
    };
    state.nav.set_mode(Mode::Files);
    state.nav.set_root(node("files:", true), vec![node(node_id, false)]);
    state.nav.select(1);
    state.maybe_fire_preview();
}

fn hello(state: &mut State, host: &HostKey) {
    state.on_connected(host.clone(), "fixture".into(), 0, None, false, crate::net::transport::ResolvedDial::Local, "test".into());
}

impl State {
    pub(in crate::ui) fn a_return_checks_its_parked_file_once(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = probe(self, "<return-once>", "return-once");
        visit(self, &host, Some("project"));
        put(self, Some(FILE), b"parked");
        visit(self, &host, None);
        put(self, Some("files:b.md"), b"passing");
        let early = Instant::now();
        visit(self, &host, Some("project"));
        self.redraw_at(early)?;
        visit(self, &host, None);
        self.redraw_at(early)?;
        visit(self, &host, Some("project"));
        self.redraw_at(early)?;
        ensure!(previews_asked(&mut rx).is_empty(), "a pass-through fetched a preview");
        ensure!(self.probe_preview_text().contains("parked"), "parked file missing on arrival");
        settle(self)?; settle(self)?;
        let asked = previews_asked(&mut rx);
        ensure!(asked.len() == 1 && asked[0].0 == FILE && asked[0].1.as_deref() == Some("project"), "return sent {asked:?}");
        self.on_preview("<different host>".into(), Some(FILE.into()), Some("project".into()), "text/plain".into(), b"foreign".to_vec(), None, asked[0].2);
        self.on_preview_get_failed(host.clone(), Some(FILE.into()), None, asked[0].2, "wrong workspace".into());
        visit(self, &host, None);
        let late = |s: &mut State| s.on_preview(host.clone(), Some(FILE.into()), Some("project".into()),
            "text/markdown".into(), b"late".to_vec(), None, asked[0].2);
        late(self);
        ensure!(self.preview_source.bytes() == Some(b"passing".as_slice()));
        visit(self, &host, Some("project"));
        late(self);
        ensure!(self.preview_source.bytes() == Some(b"parked".as_slice()));
        hello(self, &host);
        let reconnect = previews_asked(&mut rx);
        ensure!(reconnect.len() == 1 && reconnect[0].0 == FILE, "reconnect sent {reconnect:?}");
        reply(self, Some(FILE), b"reconnected".to_vec(), reconnect[0].2);
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "the return duplicated the reconnect's read");
        Ok(())
    }

    pub(in crate::ui) fn a_return_keeps_equal_bytes_and_replaces_changed_bytes(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = probe(self, "<return-bytes>", "return-bytes");
        visit(self, &host, Some("project"));
        let text = "A paragraph on this page.\n\n".repeat(180);
        put(self, Some(FILE), text.as_bytes());
        for (mime, bytes, changes) in [("text/markdown", text.as_bytes(), false), ("text/plain", text.as_bytes(), true), ("text/plain", b"replacement".as_slice(), true)] {
            back(self, &host);
            settle(self)?;
            let asked = previews_asked(&mut rx);
            self.shaped.scroll_by(4, Instant::now(), Duration::ZERO);
            let scroll = self.shaped.scroll();
            let installed = self.preview_source.generation();
            let renders = PREVIEW_RENDERS.get();
            let generation = asked.first().map_or_else(|| self.next_refetch_gen(), |r| r.2);
            self.on_preview(host.clone(), Some(FILE.into()), Some("project".into()), mime.into(), bytes.to_vec(), None, generation);
            ensure!(PREVIEW_RENDERS.get() - renders == usize::from(changes) && (self.preview_source.generation() != installed) == changes,
                "mime={mime}, changed={changes}: wrong install or render count");
            ensure!(self.preview_source.bytes() == Some(bytes) && self.preview_source.mime() == Some(mime));
            ensure!(self.shaped.scroll() == if changes { 0.0 } else { scroll }, "unexpected scroll after return");
            ensure!(asked.len() == 1, "return sent {asked:?}");
        }
        Ok(())
    }

    pub(in crate::ui) fn a_return_checks_only_the_file_on_screen(&mut self) -> Result<()> {
        let (host, mut rx, _events, _link) = probe(self, "<return-sources>", "return-sources");
        visit(self, &host, Some("project"));
        for source in ["start", "canvas", "editor", "folder", "chat", "docs", "refused"] {
            match source {
                "start" => {},
                "canvas" => { put(self, Some(FILE), b"covered"); self.ink.set_canvas(true); },
                "editor" => { self.ink.set_canvas(false); put(self, Some(FILE), b"file"); self.editor.arm_file(FILE.into()); self.editor.open_file(FILE, true, "edited".into(), "v".into()); },
                "folder" => { self.editor.close(); put(self, Some("files:folder"), b"# folder\n\nFiles-mode root. Navigate the tree to preview individual files.\n"); },
                "chat" => { self.ink.set_canvas(false); self.install_chat_picture("text/plain".into(), b"no file".to_vec()); },
                "docs" => put(self, Some("docs:page"), b"documentation"),
                _ => { put(self, Some(FILE), b"previous"); self.preview_requests.record_fired("files:other".into());
                       self.preview_requests.refuse(Some("files:other")); },
            }
            visit(self, &host, None);
            visit(self, &host, Some("project"));
            settle(self)?;
            let asked = previews_asked(&mut rx);
            anyhow::ensure!(asked.is_empty(), "{source} sent {asked:?}");
            if source == "canvas" || source == "editor" {
                self.ink.set_canvas(false); self.editor.close(); settle(self)?;
                ensure!(previews_asked(&mut rx).is_empty(), "uncovering scheduled a read");
                back(self, &host); settle(self)?;
                let next = previews_asked(&mut rx);
                ensure!(next.len() == 1 && next[0].0 == FILE, "next return after {source} sent {next:?}");
                reply(self, Some(FILE), b"uncovered".to_vec(), next[0].2);
            }
        }
        let (host, mut rx, events, _link) = probe(self, "<return-tree>", "return-tree");
        let mut wire = TreeWire::new(&host, &events)?;
        let first = self.probe_follow_visit(&host, "project", &mut wire, &mut rx)?;
        self.probe_answer_readme(&host, "project", "behind the chat", first);
        self.install_chat_picture("text/plain".into(), b"chat bytes".to_vec());
        let selected = "files:unanswered.md";
        select_file(self, selected);
        let original = previews_asked(&mut rx);
        ensure!(original.len() == 1 && original[0].0 == selected);
        back(self, &host);
        ensure!(self.preview_source.node_id().is_none() && self.preview_requests.refetch_target().is_none());
        settle(self)?;
        let followed = previews_asked(&mut rx);
        ensure!(followed.len() == 1 && followed[0].0 == selected && followed[0].2 > original[0].2, "cursor follow over chat sent {followed:?}");
        reply(self, Some(selected), b"old".to_vec(), original[0].2);
        ensure!(self.preview_source.node_id().is_none());
        reply(self, Some(selected), b"selected".to_vec(), followed[0].2);
        ensure!(self.preview_source.node_id() == Some(selected));
        // Give the badge its own listed tree, independent of the cursor selection above.
        let (host, mut rx, events, _link) = probe(self, "<return-badge>", "return-badge");
        let mut wire = TreeWire::new(&host, &events)?;
        let first = self.probe_follow_visit(&host, "project", &mut wire, &mut rx)?;
        self.probe_answer_readme(&host, "project", "row file", first);
        visit(self, &host, None);
        let evt = sot_protocol::FeCommandEvt { v: 1, cmd: "preview".into(),
            args: serde_json::json!({"workspace":"project", "path":"result.md"}), target: None, target_serial: None };
        self.dispatch_fe_command(Some(&host), route_fe_command(&evt, "fe@test").unwrap());
        self.drift.take_parse_fire("stale.jl");
        self.drift.install("stale.jl", "old".into());
        visit(self, &host, Some("project"));
        let served = self.serve_result_requests(&mut wire, &mut rx, &["README.md", "result.md"], Hold::Nothing)?;
        settle(self)?;
        let late = previews_asked(&mut rx);
        anyhow::ensure!(served.previews.len() == 1 && served.previews[0].0 == "files:result.md" && late.is_empty(),
            "badge fetched {:?}, followed by {late:?}", served.previews);
        anyhow::ensure!(self.drift.take_parse_fire("stale.jl"), "a return kept the other row's drift hash");
        Ok(())
    }
}

impl State {
    pub(in crate::ui) fn a_return_yields_to_existing_reads(&mut self) -> Result<()> {
        use crate::ui::preview::requests::continue_page_key;
        let (host, mut rx, events, _link) = probe(self, "<return-busy>", "return-busy");
        visit(self, &host, Some("project"));
        let paged = |s: &mut State, page, generation| s.on_preview(host.clone(), Some(FILE.into()), Some("project".into()),
            "text/markdown".into(), format!("page {page}").into_bytes(), Some(serde_json::json!({"page":page,"page_count":3})), generation);
        for kind in ["page", "zoom", "change", "reconnect"] {
            for before_switch in [true, false] {
                let g = self.next_preview_gen();
                self.preview_requests.record_fired(FILE.into());
                paged(self, 1, g);
                if !before_switch { back(self, &host); }
                match kind {
                    "page" => ensure!(continue_page_key(self, Some(Action::PageNext))),
                    "zoom" => { self.raster.reset_zoom(); self.raster.zoom_in(4.0); self.maybe_reraster_page(); }
                    "change" => self.on_preview_changed(host.clone(), serde_json::json!({"workspace_id":"project","node_id":FILE,"kind":"modified"})),
                    _ => hello(self, &host),
                }
                let sent = previews_asked(&mut rx);
                ensure!(sent.len() == 1, "{kind} sent {sent:?}");
                if before_switch {
                    // A reply for the other row does not retire this read of the same generation.
                    self.on_preview_get_failed(host.clone(), Some(FILE.into()), None, sent[0].2, "another row".into());
                    back(self, &host);
                }
                settle(self)?;
                let overlap = previews_asked(&mut rx);
                ensure!(overlap.is_empty() && self.preview_requests.preview_generation() == sent[0].2, "{kind}/{before_switch} displaced by {overlap:?}");
                let renders = PREVIEW_RENDERS.get();
                paged(self, if kind == "page" { 2 } else { 1 }, sent[0].2);
                ensure!(PREVIEW_RENDERS.get() == renders + 1 && self.pages.page() == Some((if kind == "page" { 2 } else { 1 }, 3)), "{kind} failed to install");
                if kind == "zoom" && !before_switch { ensure!(self.pages.raster_zoom() > 1.0); }
                settle(self)?;
                ensure!(previews_asked(&mut rx).is_empty(), "{kind} left another read owed");
            }
        }
        // An answer actually delivered while parked releases the wire slot, even though main drops its bytes.
        put(self, Some(FILE), b"unpaged before parking");
        self.on_preview_changed(host.clone(), serde_json::json!({"workspace_id":"project","node_id":FILE,"kind":"modified"}));
        let held = previews_asked(&mut rx);
        ensure!(held.len() == 1);
        visit(self, &host, None);
        paged(self, 2, held[0].2);
        visit(self, &host, Some("project")); settle(self)?;
        let next = previews_asked(&mut rx);
        ensure!(next.len() == 1 && self.pages.page().is_none(), "parked reply left {next:?} on {:?}", self.pages.page());
        paged(self, 1, next[0].2);
        // The reconnect still completes a scale save through its ordinary install, including for identical bytes.
        self.preview_requests.arm_scale_save(FILE.into(), "12".into());
        back(self, &host);
        hello(self, &host);
        let reconnect = previews_asked(&mut rx);
        ensure!(reconnect.len() == 1 && reconnect[0].0 == FILE);
        paged(self, 1, reconnect[0].2);
        ensure!(self.status.text().contains("saved"), "scale save: {}", self.status.text());
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty());
        // A told change gets its independent read even while the return is on the wire.
        put(self, Some(FILE), b"page 1");
        back(self, &host); settle(self)?;
        let check = previews_asked(&mut rx);
        ensure!(check.len() == 1, "idle return sent {check:?}");
        self.on_preview_changed(host.clone(), serde_json::json!({"workspace_id":"project","node_id":FILE,"kind":"modified"}));
        let change = previews_asked(&mut rx);
        ensure!(change.len() == 1 && change[0].2 > check[0].2);
        reply(self, Some(FILE), b"obsolete".to_vec(), check[0].2);
        ensure!(self.preview_source.bytes() == Some(b"page 1".as_slice()));
        paged(self, 1, change[0].2);
        // A new selection made during the settle owns the next install and leaves no check queued.
        back(self, &host);
        let file = "files:chosen.md";
        select_file(self, file);
        let personal = previews_asked(&mut rx);
        ensure!(personal.len() == 1 && personal[0].0 == file);
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty() && self.preview_requests.preview_generation() == personal[0].2);
        reply(self, Some(file), b"chosen".to_vec(), personal[0].2);
        ensure!(self.preview_source.node_id() == Some(file));
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty());
        events.send((host.clone(), crate::net::transport::IncomingEvt::BusLost { bus: sot_protocol::op::PREVIEW_CHANGED.into(), skipped: 4 }))?;
        self.drain_events(); settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "a bus gap fetched the preview");
        back(self, &host); settle(self)?;
        let idle = previews_asked(&mut rx);
        ensure!(idle.len() == 1, "the next idle return never read the file");
        reply(self, Some(file), b"chosen".to_vec(), idle[0].2);
        // A disconnected row already owes the normal reconnect read.
        use crate::net::transport::IncomingEvt;
        events.send((host.clone(), IncomingEvt::Disconnected { reason: "fixture".into(), refused: false }))?;
        self.drain_events(); back(self, &host); settle(self)?;
        let offline = previews_asked(&mut rx);
        ensure!(offline.is_empty(), "return spent a request while disconnected: {offline:?}");
        events.send((host.clone(), IncomingEvt::Connected {
            session_id: "fixture".into(), revision: 0, host: None, project_root: None, proxy: false,
            resolved: crate::net::transport::ResolvedDial::Local, backend_version: "fixture".into(),
            repl_running: Vec::new(), posts_as_person: true,
        }))?;
        self.drain_events(); settle(self)?;
        let reconnected = previews_asked(&mut rx);
        ensure!(reconnected.len() == 1 && reconnected[0].0 == file);
        Ok(())
    }

    pub(in crate::ui) fn a_return_checks_only_a_drawn_preview(&mut self) -> Result<()> {
        use crate::ui::persist::settings::{PresetMode, Slot};
        let (host, mut rx, _, _link) = probe(self, "<return-layout>", "return-layout");
        visit(self, &host, Some("project"));
        let png = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png"));
        let (preset, laptop) = (self.settings.preset, self.settings.laptop.clone());
        self.settings.preset = PresetMode::Laptop;
        for (layout, drawn) in [("omitted", false), ("drawer closed", false), ("drawer open", true), ("maximized nav", false), ("full drawer", false), ("column", true)] {
            self.settings.laptop = laptop.clone();
            self.layout_help = crate::ui::input::layout_help::LayoutHelp::new(DrawerContent::Closed, false, false, false);
            self.set_focus(PaneFocus::NavTree);
            let g = self.next_preview_gen();
            self.preview_requests.record_fired(FILE.into());
            self.on_preview(host.clone(), Some(FILE.into()), Some("project".into()), "image/png".into(), png.to_vec(), None, g);
            back(self, &host);
            if layout.starts_with("drawer") || layout == "omitted" {
                self.settings.laptop.columns.retain(|c| c.slot != Slot::Preview);
                self.settings.laptop.drawer = (layout != "omitted").then_some(Slot::Preview);
                if layout == "drawer open" { self.layout_help.toggle_drawer(DrawerContent::Repl, PaneFocus::NavTree); }
            } else if layout == "maximized nav" {
                self.layout_help.layout_action(Some(Action::MaximizePane), &self.help_context(), PaneFocus::NavTree, self.layout_gates());
            } else if layout == "full drawer" {
                self.set_focus(PaneFocus::Repl);
                let base = self.settings.resolve_preset(self.monitor_aspect);
                for _ in 0..3 {
                    if self.layout_help.full_shown(base, PaneFocus::Repl) { break; }
                    self.layout_help.cycle_drawer(DrawerContent::Repl, PaneFocus::Repl, base);
                }
            }
            settle(self)?;
            ensure!((!self.pane_rects.preview.is_empty()) == drawn, "fixture {layout} drew {:?}", self.pane_rects.preview);
            let reads = previews_asked(&mut rx);
            ensure!(reads.len() == usize::from(drawn), "{layout} sent {reads:?}");
            if let Some(read) = reads.first() {
                self.on_preview(host.clone(), Some(FILE.into()), Some("project".into()), "image/png".into(), png.to_vec(), None, read.2);
            }
            self.settings.laptop = laptop.clone();
            self.layout_help = crate::ui::input::layout_help::LayoutHelp::new(DrawerContent::Closed, false, false, false);
            settle(self)?;
            ensure!(previews_asked(&mut rx).is_empty(), "{layout} queued a check until uncovered");
        }
        self.settings.preset = preset;
        self.settings.laptop = laptop;
        Ok(())
    }
}

fn parked(state: &mut State, name: &str) -> (HostKey, ProbeRx, ProbeEvents, watch::Sender<PreviewLink>) {
    let (host, rx, events, link) = probe(state, name, name);
    visit(state, &host, Some("project"));
    put(state, Some(FILE), b"parked");
    (host, rx, events, link)
}

impl State {
    pub(in crate::ui) fn a_queued_read_survives_a_failed_dial(&mut self) -> Result<()> {
        use crate::ui::preview::requests::continue_page_key;
        let (other, _rx, _, _other_link) = probe(self, "other-connection", "other");
        let (host, mut rx, _, link) = parked(self, "queued-read");
        for paged in [true, false] {
            let g = self.next_preview_gen();
            self.on_preview(host.clone(), Some(FILE.into()), Some("project".into()), "text/markdown".into(), b"parked".to_vec(),
                paged.then(|| serde_json::json!({"page":1,"page_count":3})), g);
            link.send_replace(PreviewLink::Down);
            self.on_disconnected(host.clone(), "offline".into(), false);
            if paged {
                ensure!(continue_page_key(self, Some(Action::PageNext)));
            } else {
                self.on_preview_changed(host.clone(), serde_json::json!({"workspace_id":"project","node_id":FILE,"kind":"modified"}));
            }
            let queued = previews_asked(&mut rx);
            ensure!(queued.len() == 1);
            visit(self, &other, None);
            self.on_disconnected(host.clone(), "failed dial".into(), false);
            hello(self, &host);
            link.send_replace(PreviewLink::Ready);
            visit(self, &host, Some("project"));
            settle(self)?;
            let extra = previews_asked(&mut rx);
            ensure!(extra.is_empty(), "the queued read was displaced by {extra:?}");
            reply(self, Some(FILE), b"queued answer".to_vec(), queued[0].2);
            ensure!(self.preview_source.bytes() == Some(b"queued answer".as_slice()));
            back(self, &host); settle(self)?;
            let next = previews_asked(&mut rx);
            ensure!(next.len() == 1, "the ordinary reply did not release the slot");
            reply(self, Some(FILE), b"queued answer".to_vec(), next[0].2);
        }
        Ok(())
    }

    pub(in crate::ui) fn a_return_preserves_image_calibration(&mut self) -> Result<()> {
        let (host, mut rx, events, _link) = parked(self, "scale-skips");
        visit(self, &host, None); put(self, Some("files:other"), b"other");
        visit(self, &host, Some("project"));
        let png = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png"));
        let image = |s: &mut State, scale: Option<f64>, g| {
            let extras = scale.map(|nm| serde_json::json!({"physical_scale":{"unit":"nm","axes":[{"name":"x","nm_per_px":nm}]}}));
            s.on_preview(host.clone(), Some(FILE.into()), Some("project".into()), "image/png".into(), png.to_vec(), extras, g);
        };
        let g = self.next_preview_gen();
        image(self, None, g);
        let save = |s: &mut State| -> Result<u64> {
            use crate::ui::preview::image::overlay::ScalePromptContext;
            s.send_scale(FILE, 2.0).map_err(anyhow::Error::msg)?;
            s.mark_scale_saved(FILE.into(), "2".into());
            Ok(s.preview_requests.preview_generation())
        };
        let fail = |s: &mut State| -> Result<()> {
            events.send((host.clone(), IncomingEvt::ScaleSetFailed { workspace_id: Some("project".into()), node_id: FILE.into(), message: "rejected".into() }))?;
            s.drain_events();
            Ok(())
        };
        save(self)?;
        let newer = save(self)?;
        fail(self)?; // The older failure clears the prompt's marker while the newer save still owns its generation.
        visit(self, &host, None); settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "another row retired the newer save");
        visit(self, &host, Some("project")); settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty() && self.preview_requests.preview_generation() == newer, "return retired the newer save");
        image(self, Some(2.0), newer);
        ensure!(self.preview_source.scale().is_some());
        back(self, &host); settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "return requested a calibrated image");

        let g = self.next_preview_gen(); image(self, None, g);
        save(self)?; fail(self)?;
        // Another file's successful read releases the wire generation, but cannot clear this file's failed save.
        visit(self, &host, None); put(self, Some(FILE), b"other row");
        visit(self, &host, Some("project")); settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "another row cleared the failed scale save");
        let g = self.next_preview_gen(); image(self, None, g);
        back(self, &host); settle(self)?;
        let read = previews_asked(&mut rx);
        ensure!(read.len() == 1, "ordinary read did not release the failed save");
        image(self, Some(3.0), read[0].2);
        ensure!(self.preview_source.scale().is_some(), "equal image bytes hid a new calibration");
        Ok(())
    }

    pub(in crate::ui) fn a_paged_raster_keeps_its_zoom_on_return(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = parked(self, "pdf-skip");
        let png = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png"));
        let g = self.next_preview_gen();
        self.on_preview(host.clone(), Some(FILE.into()), Some("project".into()), "image/png".into(), png.to_vec(),
            Some(serde_json::json!({"page":2, "page_count":4})), g);
        back(self, &host);
        self.raster.zoom_in(2.0);
        let zoom = self.raster.zoom();
        settle(self)?;
        ensure!(zoom > 1.0 && self.raster.zoom() == zoom);
        ensure!(previews_asked(&mut rx).is_empty(), "return requested a fit-size copy of the parked page");
        ensure!(self.pages.page() == Some((2,4)) && self.preview_requests.preview_generation() == g);
        Ok(())
    }

    pub(in crate::ui) fn restoring_the_active_session_skips_the_check(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = parked(self, "same-row");
        self.switch_to_workspace(host.clone(), Some("project".into()), None, true);
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "the active row fetched its file again");
        Ok(())
    }

    pub(in crate::ui) fn a_return_waits_for_the_departed_rows_check(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = parked(self, "one-check");
        visit(self, &host, None);
        put(self, Some("files:other"), b"other row");
        for failed in [false, true] {
            visit(self, &host, Some("project")); settle(self)?;
            let first = previews_asked(&mut rx);
            ensure!(first.len() == 1);
            visit(self, &host, None); settle(self)?;
            ensure!(previews_asked(&mut rx).is_empty(), "the other parked file issued a second check");
            let g = self.next_preview_gen();
            reply(self, Some("files:other"), b"newer read".to_vec(), g);
            back(self, &host); settle(self)?;
            ensure!(previews_asked(&mut rx).is_empty(), "two checks outstanding after leaving the first row");
            if failed {
                self.on_preview_get_failed(host.clone(), Some(FILE.into()), Some("project".into()), first[0].2, "failed".into());
            } else {
                reply(self, Some(FILE), b"late".to_vec(), first[0].2);
            }
            ensure!(self.preview_source.bytes() == Some(b"parked".as_slice()));
            visit(self, &host, None); settle(self)?;
            let other = previews_asked(&mut rx);
            ensure!(other.len() == 1 && other[0].0 == "files:other", "terminal answer did not release the window slot");
            reply(self, Some("files:other"), b"other row".to_vec(), other[0].2);
        }
        Ok(())
    }

    pub(in crate::ui) fn a_preamble_on_another_host_suppresses_the_return(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = parked(self, "preamble-skip");
        let (other, _) = watch::channel(PreviewLink::Preamble);
        self.hosts.preview_links.insert("preamble host".into(), other.clone());
        back(self, &host);
        other.send_replace(PreviewLink::Ready);
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "a check was armed during another host's preamble");
        back(self, &host);
        other.send_replace(PreviewLink::Preamble);
        settle(self)?;
        other.send_replace(PreviewLink::Ready);
        ensure!(previews_asked(&mut rx).is_empty(), "a check was sent during another host's preamble");
        back(self, &host); settle(self)?;
        let next = previews_asked(&mut rx);
        ensure!(next.len() == 1, "the first return after the preamble never checked");
        reply(self, Some(FILE), b"parked".to_vec(), next[0].2);
        Ok(())
    }

    pub(in crate::ui) fn a_return_uses_only_the_current_connection(&mut self) -> Result<()> {
        let (host, mut rx, _, link) = parked(self, "live-connection");
        self.hosts.host_connected.insert(host.clone(), true);
        back(self, &host);
        // The transport already ended; the frame still has its previous Connected observation.
        link.send_replace(PreviewLink::Down);
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "a stale Connected event queued a read");
        link.send_replace(PreviewLink::Ready);
        back(self, &host); settle(self)?;
        let queued = std::iter::from_fn(|| rx.try_recv().ok()).find(|r| matches!(r, OutgoingReq::PreviewGet { .. } | OutgoingReq::ReturnPreview { .. }));
        let check = queued.ok_or_else(|| anyhow::anyhow!("no check on the live connection"))?;
        ensure!(!check.replayable(), "the return can cross a reconnect");
        let g = self.preview_requests.preview_generation();
        // A check left without a reply on that connection dies with it. The following reconnect may complete
        // before the window gets to handle either event.
        link.send_replace(PreviewLink::Down);
        link.send_replace(PreviewLink::Preamble);
        link.send_replace(PreviewLink::Ready);
        back(self, &host); settle(self)?;
        let fresh = previews_asked(&mut rx);
        ensure!(fresh.len() == 1 && fresh[0].2 > g, "retired connection still occupied the return slot");
        reply(self, Some(FILE), b"old connection".to_vec(), g);
        ensure!(self.preview_source.bytes() == Some(b"parked".as_slice()));
        reply(self, Some(FILE), b"current".to_vec(), fresh[0].2);
        Ok(())
    }

    pub(in crate::ui) fn a_replaced_folder_keeps_its_installed_page(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = parked(self, "replaced-folder");
        put(self, Some(FILE), b"# a.md\n\nFiles-mode root. Navigate the tree to preview individual files.\n");
        back(self, &host);
        select_file(self, FILE);
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "the new tree kind overrode the installed directory page");
        Ok(())
    }

    pub(in crate::ui) fn a_close_dialog_covers_the_returned_preview(&mut self) -> Result<()> {
        let (host, mut rx, _, _link) = parked(self, "close-dialog");
        back(self, &host);
        ensure!(self.focus_prompts.ask_quit(true));
        let drawn = settle(self);
        self.focus_prompts.cancel();
        drawn?;
        ensure!(previews_asked(&mut rx).is_empty(), "the close dialog allowed a return check");
        settle(self)?;
        ensure!(previews_asked(&mut rx).is_empty(), "dismissing the dialog issued the skipped check");
        Ok(())
    }
}
