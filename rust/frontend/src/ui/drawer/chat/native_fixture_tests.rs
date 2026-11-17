use super::*;
use crate::net::transport::{ChatReply, IncomingEvt, ResolvedDial};
use crate::ui::drawer::repl::DraftEdit;
use crate::ui::input::global_keys::{line_scroll_target, LineScroll};
use crate::ui::input::keybindings::Modifiers;
use sot_protocol::chat::Ts;
use sot_protocol::ops::{ChatHistoryRes, ChatResolveRes, ChatUpper, ChatWatchRes, ChatWatchState};
use std::sync::mpsc::Sender;

type Requests = tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>;

/// The chat requests the connection carried, by kind.
#[derive(Debug, Default, Clone, PartialEq)]
struct Seen {
    resolves: u32,
    watches: u32,
    snapshots: u32,
    pages: u32,
    threads: u32,
    posts: Vec<(String, Option<String>)>,
    connects: u32,
    /// The workspaces whose channel lists were asked for.
    lists: Vec<Option<String>>,
    /// The files whose original was asked for.
    originals: Vec<String>,
}

/// What the stand-in daemon holds: a channel of `top` messages, `ts(1)` the oldest, and what the connection carried.
struct Daemon {
    host: HostKey,
    top: u32,
    seen: Seen,
    /// The hub answers `unconfigured` to a resolve until a connect is made.
    unconfigured: bool,
    /// The newest three messages each carry two picture files, and a thumbnail is answered with a small picture.
    figures: bool,
}

fn ts(i: u32) -> Ts {
    Ts::parse(&format!("1700000000.{i:06}")).unwrap()
}

fn reply_ts(parent: u32, k: u32) -> Ts {
    Ts::parse(&format!("1700000001.{:06}", parent * 10 + k)).unwrap()
}

/// A record whose text starts with its own time, so a test can tell which records the frame prepared.
fn record(at: &Ts, thread: &Ts, filler: usize, replies: u32) -> serde_json::Value {
    let mut text = format!("{} ", at.as_str());
    while text.len() < filler {
        text.push_str("lorem ipsum dolor sit amet ");
    }
    text.truncate(filler.max(1));
    serde_json::json!({ "workspace": "WS1", "channel": "CH1", "ts": at, "thread_ts": thread,
        "author": { "id": "U1", "name": "Ada" }, "origin": "member", "text": text, "reply_count": replies, "files": [] })
}

fn page(records: Vec<serde_json::Value>, next: bool) -> ChatReply<ChatHistoryRes> {
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "records": records, "next": next.then_some("more") })).unwrap())
}

impl Daemon {
    /// The reply the daemon gives a request, by its kind and value; none for a request the chat does not own.
    fn answer(&mut self, req: OutgoingReq) -> Option<ChatEvt> {
        let top = self.top;
        let short = |i: u32| record(&ts(i), &ts(i), 60, if i == top { 3 } else { 0 });
        match req {
            OutgoingReq::ChatResolve { row } if self.unconfigured => {
                self.seen.resolves += 1;
                // The row's channel is in WS2, whose connection failed; WS1 runs.
                let off = serde_json::json!({ "state": "unconfigured", "reason": "tokens_refused", "workspace": "WS2" });
                Some(ChatEvt::Resolved { row, reply: ChatReply::Done(serde_json::from_value::<ChatResolveRes>(off).unwrap()) })
            }
            OutgoingReq::ChatConnect { .. } => {
                (self.seen.connects, self.unconfigured) = (self.seen.connects + 1, false);
                Some(ChatEvt::Connect { reply: ChatReply::Done(sot_protocol::ops::ChatConnectRes { workspace: "WS2".into(), name: Some("Beta Lab".into()) }) })
            }
            OutgoingReq::ChatWorkspaces => {
                let all = serde_json::json!({ "default": "WS1", "workspaces": [
                    { "id": "WS1", "name": "Alpha Lab", "running": true }, { "id": "WS2", "name": "Beta Lab", "running": !self.unconfigured } ] });
                Some(ChatEvt::Workspaces { reply: ChatReply::Done(serde_json::from_value(all).unwrap()) })
            }
            OutgoingReq::ChatChannels { workspace } => {
                self.seen.lists.push(workspace.clone());
                let list = serde_json::json!({ "channels": [{ "id": "CH1", "name": "general", "private": false, "member": true }] });
                Some(ChatEvt::Channels { workspace, reply: ChatReply::Done(serde_json::from_value(list).unwrap()) })
            }
            OutgoingReq::ChatResolve { row } => {
                self.seen.resolves += 1;
                // The hub names the channel (a nameless answer is asked again, `RowChannel`), so one resolve is all this carries.
                let mapped = serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false, "name": "general" });
                Some(ChatEvt::Resolved { row, reply: ChatReply::Done(serde_json::from_value::<ChatResolveRes>(mapped).unwrap()) })
            }
            OutgoingReq::ChatWatch { target } => {
                self.seen.watches += 1;
                Some(ChatEvt::Watched { target, reply: ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete }) })
            }
            OutgoingReq::ChatHistory { req } => {
                let reply = if let Some(parent) = &req.thread {
                    self.seen.threads += 1;
                    let at = (1..=top).find(|i| &ts(*i) == parent)?;
                    let mut all = vec![short(at)];
                    all.extend((1..=3).map(|k| record(&reply_ts(at, k), parent, 40, 0)));
                    page(all, false)
                } else if let Some(ChatUpper::Before(before)) = &req.upper {
                    self.seen.pages += 1;
                    let older: Vec<u32> = (1..=top).rev().filter(|i| ts(*i).as_str() < before.as_str()).take(20).collect();
                    let more = older.last().is_some_and(|i| *i > 1);
                    page(older.into_iter().map(short).collect(), more)
                } else {
                    self.seen.snapshots += 1;
                    let figured = |i: u32| {
                        let mut r = short(i);
                        if self.figures && i + 3 > top {
                            // The newest message shows one file twice (a file shared again in its channel).
                            r["files"] = (0..2).map(|k| serde_json::json!({ "id": if i == top { "SAME".to_string() } else { format!("F{i}-{k}") }, "name": "figure", "mime": "image/png", "bytes": 100, "thumbnail": true })).collect();
                        }
                        r
                    };
                    page((top.saturating_sub(19)..=top).rev().map(figured).collect(), top > 20)
                };
                Some(ChatEvt::History { req, reply })
            }
            OutgoingReq::ChatPost { req } => {
                self.seen.posts.push((req.text.clone(), req.thread.as_ref().map(|t| t.as_str().to_string())));
                let reply = ChatReply::Done(sot_protocol::ops::ChatPostRes::Posted { ts: ts(self.top + 1) });
                Some(ChatEvt::Posted { req, reply })
            }
            OutgoingReq::ChatFile { req } if self.figures => {
                if req.variant == sot_protocol::ops::ChatFileVariant::Original {
                    self.seen.originals.push(req.file.clone());
                }
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::RgbaImage::from_pixel(64, 48, image::Rgba(figure_color(&req.file))).write_to(&mut bytes, image::ImageFormat::Png).ok()?;
                Some(ChatEvt::File { req, reply: ChatReply::Done(crate::net::transport::ChatPicture { mime: "image/png".into(), bytes: bytes.into_inner() }) })
            }
            _ => None,
        }
    }
}

/// The solid colors a figure's picture is served in: none of them is a color any other pixel of the window has.
const FIGURE_COLORS: [[u8; 4]; 3] = [[253, 1, 254, 255], [2, 253, 1, 255], [251, 253, 3, 255]];

/// The color of the picture of file `id`.
fn figure_color(id: &str) -> [u8; 4] {
    FIGURE_COLORS[id.bytes().map(usize::from).sum::<usize>() % FIGURE_COLORS.len()]
}

/// Whether `pixel` is a figure's color, a few levels off where the sampler blends.
fn is_figure(pixel: &image::Rgba<u8>) -> bool {
    FIGURE_COLORS.iter().any(|c| pixel.0.iter().zip(c).all(|(a, b)| a.abs_diff(*b) <= 3))
}

/// The link's hello, as the window's connection reports it.
fn connected() -> IncomingEvt {
    IncomingEvt::Connected {
        session_id: "session".into(),
        revision: 0,
        host: None,
        project_root: None,
        proxy: false,
        resolved: ResolvedDial::Local,
        backend_version: "test".into(),
        repl_running: Vec::new(),
        posts_as_person: true,
    }
}

/// The 95th percentile of the frame times, in milliseconds.
fn p95(ms: &[f64]) -> f64 {
    let mut sorted = ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get((sorted.len() * 95 / 100).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0)
}

impl State {
    /// One frame through the window's own `redraw`: it presents (the capture's count rises by one) and its prepared areas
    /// hold the record the reader is anchored on, or the newest while following (the thread's frames hold their own).
    fn witness_frame(&mut self, frames: &mut Vec<f64>, newest: &Ts, label: &str) -> Result<f64> {
        let before = self.capture.frames();
        let began = Instant::now();
        self.redraw()?;
        let ms = began.elapsed().as_secs_f64() * 1000.0;
        frames.push(ms);
        anyhow::ensure!(self.capture.frames() == before + 1, "{label}: the frame did not present");
        if self.chat.held() > 0 && self.chat.thread_len() == 0 {
            let wanted = self.chat.owner_anchor().unwrap_or_else(|| newest.clone());
            let placed = self.chat.placed_text();
            anyhow::ensure!(
                placed.iter().any(|t| t.contains(wanted.as_str())) && !self.chat.areas().is_empty(),
                "{label}: the prepared areas do not hold {}: {} placed",
                wanted.as_str(),
                placed.len()
            );
        }
        Ok(ms)
    }

    /// Frames, each with the worker's outcomes settled as the frame's upkeep settles them, until the status line no longer says a
    /// picture is coming (the child answered, or failed), within 20 s.
    fn witness_settle(&mut self, frames: &mut Vec<f64>, newest: &Ts, label: &str) -> Result<()> {
        let begun = Instant::now();
        while begun.elapsed() < std::time::Duration::from_secs(20) {
            self.settle_svg_draws();
            self.witness_frame(frames, newest, label)?;
            if self.status.text() != "opening the picture…" {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::bail!("{label}: the picture never settled")
    }

    /// Frames until the chat's scroll has come to rest: each presents, each but the last asks for the next, and the motion
    /// ends within a few lengths.
    fn witness_rest(&mut self, frames: &mut Vec<f64>, newest: &Ts, label: &str) -> Result<()> {
        let begun = Instant::now();
        while begun.elapsed() < std::time::Duration::from_secs(5) {
            // The frame itself must ask for the next: a pacer left dirty by the key or the last frame would hide its absence.
            self.pacer.clear_dirty();
            self.witness_frame(frames, newest, label)?;
            if !self.chat.moving() {
                return Ok(());
            }
            anyhow::ensure!(self.pacer.is_dirty(), "{label}: a frame mid-motion did not ask for the next");
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        anyhow::bail!("{label}: the scroll never came to rest")
    }

    /// A key pressed in the window: resolved by the real bindings under the real help context (so the pane the drawer maps to
    /// decides the action), then routed as `drawer_pane_key` and `chat_key` route it, the Connect step's `connect_action`
    /// first. A winit `KeyEvent` cannot be built outside the event loop, so those two functions themselves are not called.
    fn witness_key(&mut self, key: Key, (ctrl, alt): (bool, bool), expect: Option<Action>) -> Result<()> {
        self.witness_chord(key, Modifiers { ctrl, alt, shift: false, super_: false }, expect)
    }

    /// `witness_key` with any modifiers (Shift too).
    fn witness_chord(&mut self, key: Key, mods: Modifiers, expect: Option<Action>) -> Result<()> {
        let (ctrl, alt) = (mods.ctrl, mods.alt);
        let context = self.help_context();
        let action = self.bindings.resolve(&key, Some(&key), mods, context.consumes_text(), |a| context.allows(a));
        anyhow::ensure!(action == expect, "{key:?} resolved to {action:?}, not {expect:?}");
        if super::connect::connect_action(self, action, &key, false, (ctrl, alt, false), "witness") {
        } else if matches!(action, Some(Action::ScrollLineUp | Action::ScrollLineDown)) {
            anyhow::ensure!(line_scroll_target(self.focus_prompts.focus(), self.layout_help.drawer()) == LineScroll::Chat, "Alt+Up is not the chat's");
            let (now, length) = self.scroll_ask();
            self.chat.scroll(action == Some(Action::ScrollLineUp), Step::Line, now, length);
        } else if chat_apply(self, chat_move(action)) {
        } else if action == Some(Action::ReturnNav) {
            anyhow::ensure!(self.chat.close_thread(), "Escape closed no thread");
        } else if action == Some(Action::ChatThreadClose) {
            anyhow::ensure!(self.leave_chat_thread(), "ArrowLeft closed no thread");
        } else if action == Some(Action::ChatSend) {
            self.send_chat_draft();
        } else if action == Some(Action::ChatOpenImage) {
            self.open_chat_image();
        } else if action == Some(Action::Paste) {
            let text = read_clipboard_text().ok_or_else(|| anyhow::anyhow!("the clipboard gave nothing back"))?;
            self.edit_chat_draft(Edit::Text(&text));
        } else if let Some(edit) = draft_edit(action, &key, (ctrl, alt, false)) {
            self.edit_chat_draft(edit);
        } else {
            anyhow::bail!("{key:?} reached no chat handler");
        }
        Ok(())
    }

    /// Answers every request the window sent, through the window's event queue, then drains it (which also runs
    /// `tick_chat`), until a drain sends nothing: each request is answered before the next frame.
    fn witness_pump(&mut self, events: &Sender<(HostKey, IncomingEvt)>, sent: &mut Requests, daemon: &mut Daemon) -> Result<()> {
        for _ in 0..64 {
            self.drain_events();
            let mut answered = false;
            while let Ok(req) = sent.try_recv() {
                if let Some(evt) = daemon.answer(req) {
                    events.send((daemon.host.clone(), IncomingEvt::Chat(evt))).map_err(|_| anyhow::anyhow!("the fixture's event channel closed"))?;
                    answered = true;
                }
            }
            if !answered {
                return Ok(());
            }
        }
        anyhow::bail!("the daemon is still being asked after 64 rounds")
    }

    /// The real window runs the chat through its real State routing: a listed row shows Chat, the daemon's replies come through the
    /// event queue, and every step presents. It prints the frame times and asserts none (the machine's GPU decides them).
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn a_chat_of_200_messages_scrolls_posts_and_recovers(&mut self) -> Result<()> {
        self.witness_with(Self::witness_run)
    }

    /// The Connect step through the window's real key path: a paste there is a token, never the row's draft, and the two
    /// tokens leave in one `chat.connect`.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn a_paste_in_the_connect_step_is_a_token_and_never_the_draft(&mut self) -> Result<()> {
        self.witness_with(Self::connect_run)
    }

    /// Runs `run` on a chat of its own (one link, one row), and gives the window back as it was.
    fn witness_with(&mut self, run: fn(&mut Self, &HostKey, &Sender<(HostKey, IncomingEvt)>, &mut Requests) -> Result<()>) -> Result<()> {
        let host: HostKey = "<chat-host>".to_string();
        let (tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
        let (events, receiver) = std::sync::mpsc::channel();
        let saved = (
            std::mem::replace(&mut self.conns, vec![(host.clone(), tx)]),
            std::mem::replace(&mut self.evt_rx, receiver),
            (self.selected.host().clone(), self.selected.workspace_id().clone()),
            self.settings.chat_name.replace("Ada".into()),
            (self.layout_help.drawer(), self.focus_prompts.focus()),
        );
        self.chat = ChatDrawer::default();
        let length = self.settings.smooth_scroll_ms;
        let result = run(self, &host, &events, &mut sent);
        self.chat = ChatDrawer::default();
        self.settings.smooth_scroll_ms = length;
        let (conns, evt_rx, (saved_host, saved_row), name, (drawer, focus)) = saved;
        (self.conns, self.evt_rx, self.settings.chat_name) = (conns, evt_rx, name);
        self.selected.select(saved_host, saved_row);
        if self.layout_help.drawer() != drawer {
            let tenant = if drawer == DrawerContent::Closed { DrawerContent::Chat } else { drawer };
            self.layout_help.toggle_drawer(tenant, focus);
        }
        self.set_focus(focus);
        result
    }

    /// Lists one row, shows Chat with the drawer focused, and brings the link up past the resolve's debounce.
    fn witness_show(&mut self, host: &HostKey, events: &Sender<(HostKey, IncomingEvt)>) -> Result<()> {
        let mut row = crate::ui::nav::ws_info("chat", "chat-session");
        row.workspace_id = "ROW1".into();
        self.on_workspaces(host.clone(), vec![row]);
        self.switch_to_workspace(host.clone(), Some("chat".into()), None, false);
        self.layout_help.toggle_drawer(DrawerContent::Chat, self.focus_prompts.focus());
        anyhow::ensure!(self.layout_help.chat_shown(), "the drawer did not show Chat");
        self.set_focus(PaneFocus::Repl);
        events.send((host.clone(), connected())).map_err(|_| anyhow::anyhow!("the fixture's event channel closed"))?;
        self.drain_events();
        std::thread::sleep(crate::ui::app::NAV_FIRE_DEBOUNCE + std::time::Duration::from_millis(100));
        Ok(())
    }

    /// The chat's pictures through the window's real frames: the tiles' boxes and the thumbnails' textures are drawn in the one
    /// pass (every frame presents), a thumbnail's arrival moves no record or tile, and each picture shown gets a texture. Then
    /// `Primary+o` on the marked message asks one original, and its answer is the preview's source, with no node.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn a_chats_pictures_show_and_one_opens_in_the_preview(&mut self) -> Result<()> {
        self.witness_with(Self::figures_run)
    }

    fn figures_run(&mut self, host: &HostKey, events: &Sender<(HostKey, IncomingEvt)>, sent: &mut Requests) -> Result<()> {
        let mut daemon = Daemon { host: host.clone(), top: 6, seen: Seen::default(), unconfigured: false, figures: true };
        let mut frames = Vec::new();
        let newest = ts(6);
        self.witness_show(host, events)?;
        // The resolve, the watch and the snapshot are answered; nothing is placed yet, so no thumbnail is asked.
        self.witness_pump(events, sent, &mut daemon)?;
        // The records and their tiles are placed and drawn before any thumbnail has arrived: a box each.
        self.witness_frame(&mut frames, &newest, "first frame")?;
        anyhow::ensure!(self.chat.held() == 7, "the snapshot holds {} records", self.chat.held());
        let before = (self.chat.placed_tops(), self.chat.tiles().len());
        anyhow::ensure!(before.1 >= 2, "{} tiles are placed before any thumbnail", before.1);
        anyhow::ensure!(self.chat.textures() == 0, "a texture is made before its picture arrived");
        // Each thumbnail is answered, and every frame after presents with the records and tiles where they were.
        for _ in 0..12 {
            self.witness_pump(events, sent, &mut daemon)?;
            self.witness_frame(&mut frames, &newest, "a thumbnail")?;
            anyhow::ensure!((self.chat.placed_tops(), self.chat.tiles().len()) == before, "a thumbnail's arrival moved a record or a tile");
        }
        let made = self.chat.textures();
        anyhow::ensure!(made >= 1, "no thumbnail was drawn: {made} textures");
        // A frame after all of them draws from the textures it has: none is made again.
        self.witness_frame(&mut frames, &newest, "settled")?;
        anyhow::ensure!(self.chat.textures() == made, "the textures changed with nothing new");
        // The pass is read by pixels: a frame read back through the selfie. Each tile whose center is in the body shows its picture's
        // color there (so a thumbnail is over its box and under nothing, and the newest message's two tiles of one file both
        // show it), and no figure-colored pixel lies outside the body.
        let path = std::env::temp_dir().join(format!("sot-chat-thumbnails-{}.png", std::process::id()));
        self.capture.request_selfie(path.clone());
        self.witness_frame(&mut frames, &newest, "read back")?;
        let shot = image::open(&path).map_err(|e| anyhow::anyhow!("the frame was not read back: {e}")).map(|i| i.to_rgba8());
        let _ = std::fs::remove_file(&path);
        let shot = shot?;
        let body = self.chat.records_body().ok_or_else(|| anyhow::anyhow!("no body"))?;
        let (bx, by, bw, bh) = crate::ui::render::scissor::scissor_px(body, shot.dimensions()).ok_or_else(|| anyhow::anyhow!("an empty body"))?;
        let (mut checked, mut cut) = (0, 0);
        for t in self.chat.tiles() {
            let (cx, cy) = ((t.rect.x + t.rect.w / 2.0).round() as u32, (t.rect.y + t.rect.h / 2.0).round() as u32);
            cut += usize::from(t.rect.y < body.y || t.rect.y + t.rect.h > body.y + body.h);
            if (bx..bx + bw).contains(&cx) && (by..by + bh).contains(&cy) {
                let got = shot.get_pixel(cx, cy).0;
                anyhow::ensure!(got.iter().zip(figure_color(&t.file)).all(|(a, b)| a.abs_diff(b) <= 3), "the center of the tile of {} is {got:?}, not its picture's", t.file);
                checked += 1;
            }
        }
        anyhow::ensure!(checked >= 2 && cut >= 1, "{checked} tile centers read, {cut} tiles cut by the body: the frame does not exercise the clip");
        let stray = shot.enumerate_pixels().filter(|(x, y, p)| is_figure(p) && !((bx..bx + bw).contains(x) && (by..by + bh).contains(y))).count();
        anyhow::ensure!(stray == 0, "{stray} figure-colored pixels lie outside the body");
        // ArrowUp marks the newest, which shows two pictures; Primary+o asks its first one's original, once, and the answer is
        // installed as the preview's source with no node, the fired target as it was.
        self.preview_requests.record_fired("files:fig.png".into());
        let fired = self.preview_requests.refetch_target().map(str::to_owned);
        self.witness_key(Key::Named(NamedKey::ArrowUp), (false, false), Some(Action::ChatMarkOlder))?;
        self.witness_frame(&mut frames, &newest, "marked")?;
        // A preview of the cursored file is in flight when the picture is opened: its late reply must not replace the picture.
        let in_flight = self.next_preview_gen();
        self.witness_key(Key::Character("o".into()), (true, false), Some(Action::ChatOpenImage))?;
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(daemon.seen.originals == ["SAME"], "originals asked: {:?}", daemon.seen.originals);
        // The original is with the window's own bounded child: the cursored file's late reply comes meanwhile and is dropped.
        let (active, row) = (self.selected.host().clone(), self.selected.workspace_id().clone());
        self.on_preview(active, Some("files:fig.png".into()), row, "text/plain".into(), b"the file".to_vec(), None, in_flight);
        let begun = Instant::now();
        self.witness_settle(&mut frames, &newest, "drawn by the child")?;
        println!("chat-open child_ms={}", begun.elapsed().as_millis());
        anyhow::ensure!(self.status.text() == "picture shown in the preview", "the status line says {:?}", self.status.text());
        anyhow::ensure!(self.preview_source.node_id().is_none() && self.preview_source.mime() == Some("image/png"), "the preview shows {:?} of {:?}", self.preview_source.mime(), self.preview_source.node_id());
        anyhow::ensure!(self.preview_requests.refetch_target().map(str::to_owned) == fired, "the open changed the fired target");
        anyhow::ensure!(self.raster.is_shown(), "the opened picture is not the pane's raster");
        self.witness_frame(&mut frames, &newest, "opened")?;
        // Nothing that acts on what is shown names the fired file under the picture: no region (crop, its paste to the agent,
        // fe-state, the saved view), no title, no scale prompt.
        anyhow::ensure!(self.raster.roi().is_none(), "the picture's region is recorded as {:?}", self.raster.roi().map(|r| r.node_id.clone()));
        anyhow::ensure!(self.preview_pane_name().is_none(), "the pane is titled {:?} over the picture", self.preview_pane_name());
        anyhow::ensure!(!self.begin_scale_entry(), "the scale prompt opened for the fired file over the picture");
        // Nor the fired file's caption under the picture, nor `e`'s edit of the fired file.
        let row_key = self.active_result_row_key().ok_or_else(|| anyhow::anyhow!("no row key for the caption"))?;
        self.overlays.set_caption(row_key.clone(), "files:fig.png".into(), "the fired file's caption".into());
        let pane = crate::ui::render::quad::ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 };
        let caption = self.build_caption(pane).is_some();
        self.overlays.clear_caption(&row_key, "files:fig.png");
        anyhow::ensure!(!caption, "the fired file's caption is drawn under the picture");
        while sent.try_recv().is_ok() {}
        crate::ui::preview::editor::keys::enter_editor(self);
        let reads = std::iter::from_fn(|| sent.try_recv().ok()).filter(|r| matches!(r, OutgoingReq::FileRead { .. })).count();
        anyhow::ensure!(reads == 0 && !self.editor.is_editing(), "`e` over the picture read the fired file for editing");
        // An original cut by a link change is given up at State, said on the status line, and never asked again by itself;
        // the host's one transfer is free again, so the next Primary+o asks one.
        let asked = |sent: &mut Requests| std::iter::from_fn(|| sent.try_recv().ok()).filter(|r| matches!(r, OutgoingReq::ChatFile { req } if req.variant == sot_protocol::ops::ChatFileVariant::Original)).count();
        self.witness_key(Key::Character("o".into()), (true, false), Some(Action::ChatOpenImage))?;
        self.drain_events();
        anyhow::ensure!(asked(sent) == 1, "the open was not asked");
        anyhow::ensure!(self.status.text() == "opening the picture…", "the status line says {:?}", self.status.text());
        let send = |evt: IncomingEvt| events.send((host.clone(), evt)).map_err(|_| anyhow::anyhow!("the fixture's event channel closed"));
        send(IncomingEvt::Disconnected { reason: "eof".into(), refused: false })?;
        self.drain_events();
        anyhow::ensure!(self.status.text().contains("picture not opened: the connection to <chat-host> changed"), "the status line says {:?}", self.status.text());
        send(connected())?;
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(daemon.seen.originals == ["SAME"], "an original was asked again by itself: {:?}", daemon.seen.originals);
        self.witness_key(Key::Character("o".into()), (true, false), Some(Action::ChatOpenImage))?;
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(daemon.seen.originals == ["SAME", "SAME"], "after the link's return, originals asked: {:?}", daemon.seen.originals);
        self.witness_settle(&mut frames, &newest, "drawn again")?;
        // A second picture opened while the first is with the child: the one shown is the newest asked for, and the first's
        // outcome is dropped unsaid (it neither installs nor makes the second's job stale).
        anyhow::ensure!(figure_color("SAME") != figure_color("F4-0"), "the two files share a color");
        // The real child draws both; the first's answer is held back 1 s, so the second is handed on while the first is with
        // the worker, and the second's 300 ms, so the first's is settled while the second is still there, whatever the frames.
        let draws = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let draw: crate::ui::preview::image::svg_draws::Draw = std::sync::Arc::new(move |jobs, stop| {
            let outcomes = crate::ui::preview::image::svg_run::draw_in_own_child(jobs, stop);
            let held = [1000, 300][draws.fetch_add(1, std::sync::atomic::Ordering::SeqCst).min(1)];
            std::thread::sleep(std::time::Duration::from_millis(held));
            outcomes
        });
        let real = std::mem::replace(&mut self.svg_draws, crate::ui::preview::image::svg_draws::SvgDraws::new(crate::ui::preview::image::svg_draws::QUEUED_BYTES, || {}, draw));
        self.witness_key(Key::Character("o".into()), (true, false), Some(Action::ChatOpenImage))?;
        self.witness_pump(events, sent, &mut daemon)?;
        for _ in 0..2 {
            self.witness_key(Key::Named(NamedKey::ArrowUp), (false, false), Some(Action::ChatMarkOlder))?;
            self.witness_frame(&mut frames, &newest, "marked an older one")?;
        }
        self.witness_key(Key::Character("o".into()), (true, false), Some(Action::ChatOpenImage))?;
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(daemon.seen.originals.ends_with(&["SAME".to_string(), "F4-0".to_string()]), "originals asked: {:?}", daemon.seen.originals);
        let settled = self.witness_settle(&mut frames, &newest, "two opens");
        self.svg_draws = real;
        settled?;
        anyhow::ensure!(self.status.text() == "picture shown in the preview", "the second open ends {:?}", self.status.text());
        let shown = self.preview_source.bytes().and_then(|b| image::load_from_memory_with_format(b, image::ImageFormat::Png).ok()).map(|i| i.to_rgba8().get_pixel(0, 0).0);
        anyhow::ensure!(shown == Some(figure_color("F4-0")), "the preview shows {shown:?}, not the second picture's {:?}", figure_color("F4-0"));
        // The child made to fail on purpose (a stand-in for it): one that exits, one past its wall, one that answers garbage.
        // Each is a refusal on the status line, the picture already shown stays, and the window goes on presenting frames.
        #[cfg(unix)]
        for (script, words) in [
            ("exit 3", "picture not opened: the drawing ended"),
            ("/usr/bin/printf 'SVG1'; exec /bin/sleep 30", "picture not opened: took past 1 s"),
            (r"/usr/bin/printf 'SVG1N\001\000\000\000\001\000\000\000\004\000\000\000\000\000\000\000abcd'", "picture not opened: the drawing's reply was not what was asked"),
        ] {
            let bounds = crate::ui::preview::image::svg_run::Bounds { wall: std::time::Duration::from_secs(1), heap: 1 << 30, cpu: 30 };
            let draw: crate::ui::preview::image::svg_draws::Draw = std::sync::Arc::new(move |jobs, stop| {
                let mut command = std::process::Command::new("/bin/sh");
                command.args(["-c", script]);
                crate::ui::preview::image::svg_run::draw_in_child(command, jobs, &bounds, stop)
            });
            let real = std::mem::replace(&mut self.svg_draws, crate::ui::preview::image::svg_draws::SvgDraws::new(crate::ui::preview::image::svg_draws::QUEUED_BYTES, || {}, draw));
            let shown = self.preview_source.generation();
            self.witness_key(Key::Character("o".into()), (true, false), Some(Action::ChatOpenImage))?;
            self.witness_pump(events, sent, &mut daemon)?;
            let settled = self.witness_settle(&mut frames, &newest, script);
            self.svg_draws = real;
            settled?;
            anyhow::ensure!(self.status.text().starts_with(words), "{script}: the status line says {:?}", self.status.text());
            anyhow::ensure!(self.preview_source.generation() == shown && self.raster.is_shown(), "{script}: the shown picture changed");
        }
        println!("chat-thumbnails tiles={} textures={made} centers={checked} cut={cut}", before.1);
        Ok(())
    }

    fn connect_run(&mut self, host: &HostKey, events: &Sender<(HostKey, IncomingEvt)>, sent: &mut Requests) -> Result<()> {
        let mut daemon = Daemon { host: host.clone(), top: 3, seen: Seen::default(), unconfigured: false, figures: false };
        let send = |evt: IncomingEvt| events.send((host.clone(), evt)).map_err(|_| anyhow::anyhow!("the fixture's event channel closed"));
        self.witness_show(host, events)?;
        self.witness_pump(events, sent, &mut daemon)?;
        // The hello's word reaches the hint through the window's own event route: this link's hello said the daemon posts as the
        // person (`connected()`), so the empty draft's hint says so and nothing of an older daemon.
        let hint = self.chat.compose_text(true);
        anyhow::ensure!(
            matches!(hint, Some(super::compose::ComposeText::Hint { as_person: Some(true), .. })),
            "the hint does not carry the hello's word"
        );
        // A draft begun in the mapped channel; then the hub's chat goes unconfigured, seen at the link's return.
        anyhow::ensure!(!self.keys_take_secrets(), "a mapped channel's keys are taken as secret");
        self.witness_key(Key::Character("a".into()), (false, false), None)?;
        anyhow::ensure!(self.chat.draft().map(|d| d.0) == Some("a"), "the draft was not begun");
        daemon.unconfigured = true;
        send(IncomingEvt::Disconnected { reason: "eof".into(), refused: false })?;
        self.witness_pump(events, sent, &mut daemon)?;
        send(connected())?;
        self.witness_pump(events, sent, &mut daemon)?;
        self.redraw()?;
        anyhow::ensure!(self.chat.in_setup(), "unconfigured began no Connect step");
        // B1 delta 7: the row's channel is in a workspace whose connection failed while another runs, so the step offers
        // Ctrl+Shift+K, taken by the step's own keys: the other workspace's channel list opens in its place; Escape gives
        // the step back.
        let chord = if cfg!(target_os = "macos") { "Cmd+Shift+K" } else { "Ctrl+Shift+K" };
        let pin = self.chat.body().map(|b| b.pin).unwrap_or_default();
        anyhow::ensure!(pin.contains(&format!("{chord}  open the channel list of another workspace")), "the step's keys say {pin:?}");
        let shift_k = Modifiers { ctrl: !cfg!(target_os = "macos"), alt: false, shift: true, super_: cfg!(target_os = "macos") };
        self.witness_chord(Key::Character("K".into()), shift_k, Some(Action::ChatSetup))?;
        anyhow::ensure!(self.chat.picking() && !self.chat.in_setup(), "the chord opened no channel list");
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(daemon.seen.lists == [Some("WS1".to_string())], "the lists asked were {:?}", daemon.seen.lists);
        anyhow::ensure!(self.chat.picking(), "the row's unconfigured answer took the list away");
        self.chat.dismiss_picker();
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(self.chat.in_setup(), "Escape over the list did not give the step back");
        // While the step has the keys, a typed character is neither logged nor shown as the last key.
        anyhow::ensure!(self.keys_take_secrets(), "the Connect step's keys are not taken as secret");
        let typed = crate::ui::input::keypress::key_texts(&Key::Character("q".into()), self.keys_take_secrets());
        anyhow::ensure!(typed == ("<typed>".to_string(), "<typed>".to_string()), "a typed key is said as {typed:?}");
        // Ctrl+V three times through the window's clipboard: each is a token, the draft is untouched, one connect goes.
        for token in [format!("{}{}", "xoxb-", "Q".repeat(40)), format!("{}{}", "xoxp-", "Q".repeat(40)), format!("{}{}", "xapp-", "Q".repeat(40))] {
            write_clipboard_text(token).map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
            self.witness_key(Key::Character("v".into()), (true, false), Some(Action::Paste))?;
            anyhow::ensure!(self.chat.draft().map(|d| d.0) == Some("a"), "a paste in the Connect step reached the draft");
        }
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(daemon.seen.connects == 1, "{} connects went", daemon.seen.connects);
        anyhow::ensure!(self.chat.title() == " chat · connected ", "the title says {:?}", self.chat.title());
        let head = self.chat.body().map(|b| b.head).unwrap_or_default();
        anyhow::ensure!(head.starts_with("Ship of Tools is connected to the Slack workspace Beta Lab."), "the connected screen says {head:?}");
        self.redraw()?;
        Ok(())
    }

    fn witness_run(&mut self, host: &HostKey, events: &Sender<(HostKey, IncomingEvt)>, sent: &mut Requests) -> Result<()> {
        let mut daemon = Daemon { host: host.clone(), top: 200, seen: Seen::default(), unconfigured: false, figures: false };
        let mut frames = Vec::new();
        let send = |evt: IncomingEvt| events.send((host.clone(), evt)).map_err(|_| anyhow::anyhow!("the fixture's event channel closed"));
        let mut row = crate::ui::nav::ws_info("chat", "chat-session");
        row.workspace_id = "ROW1".into();
        self.on_workspaces(host.clone(), vec![row]);
        self.switch_to_workspace(host.clone(), Some("chat".into()), None, false);
        let repl_before = (self.repl_model.input().to_string(), self.repl_model.scroll());
        self.repl_model.edit_draft(DraftEdit::Replace("1 + 1".into()));
        let repl_draft = (self.repl_model.input().to_string(), self.repl_model.scroll());
        anyhow::ensure!(repl_draft.0 == "1 + 1", "the REPL's draft was not set: {repl_draft:?}");

        // Lists a row and shows Chat; the link is up; the row resolves once it has stayed the same for the debounce.
        self.layout_help.toggle_drawer(DrawerContent::Chat, self.focus_prompts.focus());
        anyhow::ensure!(self.layout_help.chat_shown(), "the drawer did not show Chat");
        self.set_focus(PaneFocus::Repl);
        send(connected())?;
        self.drain_events();
        std::thread::sleep(crate::ui::app::NAV_FIRE_DEBOUNCE + std::time::Duration::from_millis(100));
        let mut newest = ts(200);
        self.witness_pump(events, sent, &mut daemon)?;
        self.witness_frame(&mut frames, &newest, "first frame")?;
        anyhow::ensure!(self.chat.held() == 20, "the snapshot holds {} records", self.chat.held());

        // Older pages up to 200 records: the reader at the oldest held record asks for the page below it.
        for step in 0..200 {
            if self.chat.held() == 200 && daemon.seen.pages == 9 {
                break;
            }
            self.witness_key(Key::Named(NamedKey::PageUp), (false, false), Some(Action::ScrollPageUp))?;
            self.witness_frame(&mut frames, &newest, &format!("paging step {step}"))?;
            self.witness_pump(events, sent, &mut daemon)?;
        }
        anyhow::ensure!(self.chat.held() == 200 && daemon.seen.pages == 9, "held {} after {} pages", self.chat.held(), daemon.seen.pages);

        // PageUp to the oldest and PageDown back, a frame at each step.
        let oldest = ts(1);
        for step in 0..600 {
            self.witness_frame(&mut frames, &newest, &format!("PageUp {step}"))?;
            if self.chat.placed_text().iter().any(|t| t.contains(oldest.as_str())) {
                break;
            }
            self.witness_key(Key::Named(NamedKey::PageUp), (false, false), Some(Action::ScrollPageUp))?;
        }
        anyhow::ensure!(self.chat.placed_text().iter().any(|t| t.contains(oldest.as_str())), "PageUp never reached the oldest record");
        for step in 0..600 {
            self.witness_key(Key::Named(NamedKey::PageDown), (false, false), Some(Action::ScrollPageDown))?;
            self.witness_frame(&mut frames, &newest, &format!("PageDown {step}"))?;
            if self.chat.following() {
                break;
            }
        }
        anyhow::ensure!(self.chat.following(), "PageDown never returned to the newest");

        // The wheel through the window's own adapter, one tick up and one down (three rows, a line step in the chat), then
        // Alt+Up: the chat's view moves each time (the REPL's scroll is checked at the end). The window is a harness instance,
        // which asks with zero (and so did the paging above); these scrolls name their own length, ease over frames, and each
        // check of where one landed waits for the motion to rest. An arrival of the motion at the newest ends it there.
        self.settings.smooth_scroll_ms = 150;
        for (tick, following) in [(1.0, false), (-1.0, true)] {
            self.pointer.clear_residue();
            crate::ui::input::mouse::mouse_wheel(self, winit::keyboard::ModifiersState::empty(), winit::event::MouseScrollDelta::LineDelta(0.0, tick));
            anyhow::ensure!(self.chat.following() == !following, "the wheel ({tick}) moved the chat at the event, not over frames");
            self.witness_frame(&mut frames, &newest, "the wheel")?;
            anyhow::ensure!(!following || self.chat.moving() && !self.chat.following(), "the wheel ({tick}) began no motion");
            self.witness_rest(&mut frames, &newest, "the wheel")?;
            anyhow::ensure!(self.chat.following() == following, "the wheel ({tick}) left following = {}", self.chat.following());
        }
        self.witness_key(Key::Named(NamedKey::ArrowUp), (false, true), Some(Action::ScrollLineUp))?;
        self.witness_rest(&mut frames, &newest, "Alt+Up")?;
        anyhow::ensure!(!self.chat.following(), "Alt+Up did not scroll the chat");
        // End moves at once, with nothing left owed.
        self.witness_key(Key::Named(NamedKey::End), (false, false), Some(Action::ChatLatest))?;
        self.witness_frame(&mut frames, &newest, "End")?;
        anyhow::ensure!(self.chat.following() && !self.chat.moving(), "End did not follow the newest at once");

        // ArrowUp marks the newest, ArrowRight opens its thread (read by value and answered), Escape closes it.
        self.witness_key(Key::Named(NamedKey::ArrowUp), (false, false), Some(Action::ChatMarkOlder))?;
        self.witness_frame(&mut frames, &newest, "ArrowUp")?;
        anyhow::ensure!(self.chat.marked() == Some(newest.clone()), "ArrowUp marked {:?}", self.chat.marked());
        self.witness_key(Key::Named(NamedKey::ArrowRight), (false, false), Some(Action::ChatThreadOpen))?;
        self.witness_pump(events, sent, &mut daemon)?;
        self.witness_frame(&mut frames, &newest, "thread")?;
        anyhow::ensure!(self.chat.thread_len() == 4 && daemon.seen.threads == 1, "the thread holds {} after {} reads", self.chat.thread_len(), daemon.seen.threads);
        self.witness_key(Key::Named(NamedKey::Escape), (false, false), Some(Action::ReturnNav))?;
        self.witness_frame(&mut frames, &newest, "Escape")?;
        anyhow::ensure!(self.chat.marked() == Some(newest.clone()), "the mark did not return with the channel");
        // ArrowRight opens it again, and ArrowLeft over the empty draft closes it as Escape did.
        self.witness_key(Key::Named(NamedKey::ArrowRight), (false, false), Some(Action::ChatThreadOpen))?;
        self.witness_pump(events, sent, &mut daemon)?;
        self.witness_frame(&mut frames, &newest, "thread again")?;
        anyhow::ensure!(self.chat.thread_len() == 4, "the thread holds {} again", self.chat.thread_len());
        self.witness_key(Key::Named(NamedKey::ArrowLeft), (false, false), Some(Action::ChatThreadClose))?;
        self.witness_frame(&mut frames, &newest, "ArrowLeft")?;
        anyhow::ensure!(self.chat.thread_len() == 0 && self.chat.marked() == Some(newest.clone()), "ArrowLeft left the thread open or the mark away");

        // A resize with 256 records of 8 KiB: the held 200 are edited to 8 KiB and 56 newer arrive, by `chat.changed`.
        let batches: Vec<Vec<u32>> = (1..=256).collect::<Vec<u32>>().chunks(20).map(<[u32]>::to_vec).collect();
        for batch in batches {
            let put: Vec<_> = batch.iter().map(|i| record(&ts(*i), &ts(*i), 8 * 1024, 0)).collect();
            let changed = serde_json::json!({ "workspace": "WS1", "channel": "CH1", "put": put, "deleted": [] });
            send(IncomingEvt::Chat(ChatEvt::Changed(serde_json::from_value(changed).unwrap())))?;
        }
        self.witness_pump(events, sent, &mut daemon)?;
        (daemon.top, newest) = (256, ts(256));
        anyhow::ensure!(self.chat.held() == 256, "{} records held", self.chat.held());
        self.witness_frame(&mut frames, &newest, "256 records of 8 KiB")?;
        let size = self.window.inner_size();
        // The canvas takes the new size (the grid and the drawer's body); the X window itself cannot change inside one callback.
        self.canvas.resize(self.gpu.queue(), size.width - 160, size.height - 80);
        let resize_ms = self.witness_frame(&mut frames, &newest, "resize")?;
        self.canvas.resize(self.gpu.queue(), size.width, size.height);
        self.witness_frame(&mut frames, &newest, "resize back")?;

        // A typed character, a paste through the window's clipboard handle, and Enter: one post to the channel.
        self.witness_key(Key::Character("a".into()), (false, false), None)?;
        write_clipboard_text("b c".into()).map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
        self.witness_key(Key::Character("v".into()), (true, false), Some(Action::Paste))?;
        self.witness_frame(&mut frames, &newest, "draft")?;
        self.witness_key(Key::Named(NamedKey::Enter), (false, false), Some(Action::ChatSend))?;
        self.witness_pump(events, sent, &mut daemon)?;
        anyhow::ensure!(daemon.seen.posts == [("ab c".to_string(), None)], "posts: {:?}", daemon.seen.posts);
        self.witness_frame(&mut frames, &newest, "posted")?;

        let first = daemon.seen.clone();
        anyhow::ensure!(
            (first.resolves, first.watches, first.snapshots, first.pages, first.threads) == (1, 1, 1, 9, 2),
            "the connection carried {first:?}"
        );

        // The link goes and comes back: one watch and one read. A lost `chat.changed` bus: one watch.
        send(IncomingEvt::Disconnected { reason: "eof".into(), refused: false })?;
        self.witness_pump(events, sent, &mut daemon)?;
        self.witness_frame(&mut frames, &newest, "disconnected")?;
        let down = daemon.seen.clone();
        send(connected())?;
        self.witness_pump(events, sent, &mut daemon)?;
        self.witness_frame(&mut frames, &newest, "reconnected")?;
        let back = daemon.seen.clone();
        anyhow::ensure!(
            (back.watches - down.watches, back.snapshots - down.snapshots) == (1, 1),
            "the link's return brought {down:?} then {back:?}"
        );
        send(IncomingEvt::BusLost { bus: sot_protocol::op::CHAT_CHANGED.into(), skipped: 1 })?;
        self.witness_pump(events, sent, &mut daemon)?;
        self.witness_frame(&mut frames, &newest, "bus lost")?;
        anyhow::ensure!(daemon.seen.watches - back.watches == 1, "bus.lost brought {} watches", daemon.seen.watches - back.watches);
        anyhow::ensure!(daemon.seen.snapshots - back.snapshots == 1, "bus.lost brought {} snapshot reads", daemon.seen.snapshots - back.snapshots);

        anyhow::ensure!((self.repl_model.input(), self.repl_model.scroll()) == (repl_draft.0.as_str(), repl_draft.1), "the REPL's draft or scroll moved");
        self.repl_model.edit_draft(DraftEdit::Replace(repl_before.0));
        println!(
            "chat-witness frames={} max_ms={:.1} p95_ms={:.1} resize_ms={resize_ms:.1}",
            frames.len(),
            frames.iter().copied().fold(0.0, f64::max),
            p95(&frames)
        );
        Ok(())
    }
}
