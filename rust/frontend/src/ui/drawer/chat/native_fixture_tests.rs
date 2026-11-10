use super::*;
use crate::net::transport::{IncomingEvt, ResolvedDial};
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
}

/// What the stand-in daemon holds: a channel of `top` messages, `ts(1)` the oldest, and what the connection carried.
struct Daemon {
    host: HostKey,
    top: u32,
    seen: Seen,
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
            OutgoingReq::ChatResolve { row } => {
                self.seen.resolves += 1;
                let mapped = serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false });
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
                    page((top.saturating_sub(19)..=top).rev().map(short).collect(), top > 20)
                };
                Some(ChatEvt::History { req, reply })
            }
            OutgoingReq::ChatPost { req } => {
                self.seen.posts.push((req.text.clone(), req.thread.as_ref().map(|t| t.as_str().to_string())));
                let reply = ChatReply::Done(sot_protocol::ops::ChatPostRes::Posted { ts: ts(self.top + 1) });
                Some(ChatEvt::Posted { req, reply })
            }
            _ => None,
        }
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

    /// A key pressed in the window: resolved by the real bindings under the real help context (so the pane the drawer maps to
    /// decides the action), then routed as `drawer_pane_key` and `chat_key` route it. A winit `KeyEvent` cannot be built
    /// outside the event loop, so those two functions themselves are not called.
    fn witness_key(&mut self, key: Key, (ctrl, alt): (bool, bool), expect: Option<Action>) -> Result<()> {
        let context = self.help_context();
        let mods = Modifiers { ctrl, alt, shift: false, super_: false };
        let action = self.bindings.resolve(&key, Some(&key), mods, context.consumes_text(), |a| context.allows(a));
        anyhow::ensure!(action == expect, "{key:?} resolved to {action:?}, not {expect:?}");
        if matches!(action, Some(Action::ScrollLineUp | Action::ScrollLineDown)) {
            anyhow::ensure!(line_scroll_target(self.focus_prompts.focus(), self.layout_help.drawer()) == LineScroll::Chat, "Alt+Up is not the chat's");
            self.chat.scroll(action == Some(Action::ScrollLineUp), Step::Line);
        } else if chat_apply(self, chat_move(action)) {
        } else if action == Some(Action::ReturnNav) {
            anyhow::ensure!(self.chat.close_thread(), "Escape closed no thread");
        } else if action == Some(Action::ChatSend) {
            self.send_chat_draft();
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
        let result = self.witness_run(&host, &events, &mut sent);
        self.chat = ChatDrawer::default();
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

    fn witness_run(&mut self, host: &HostKey, events: &Sender<(HostKey, IncomingEvt)>, sent: &mut Requests) -> Result<()> {
        let mut daemon = Daemon { host: host.clone(), top: 200, seen: Seen::default() };
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
        send(IncomingEvt::Connected {
            session_id: "session".into(),
            revision: 0,
            host: None,
            project_root: None,
            proxy: false,
            resolved: ResolvedDial::Local,
            backend_version: "test".into(),
            repl_running: Vec::new(),
        })?;
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
        // Alt+Up: the chat's view moves each time (the REPL's scroll is checked at the end).
        for (tick, following) in [(1.0, false), (-1.0, true)] {
            self.pointer.clear_residue();
            crate::ui::input::mouse::mouse_wheel(self, winit::keyboard::ModifiersState::empty(), winit::event::MouseScrollDelta::LineDelta(0.0, tick));
            self.witness_frame(&mut frames, &newest, "the wheel")?;
            anyhow::ensure!(self.chat.following() == following, "the wheel ({tick}) left following = {}", self.chat.following());
        }
        self.witness_key(Key::Named(NamedKey::ArrowUp), (false, true), Some(Action::ScrollLineUp))?;
        self.witness_frame(&mut frames, &newest, "Alt+Up")?;
        anyhow::ensure!(!self.chat.following(), "Alt+Up did not scroll the chat");
        self.witness_key(Key::Named(NamedKey::End), (false, false), Some(Action::ChatLatest))?;
        self.witness_frame(&mut frames, &newest, "End")?;
        anyhow::ensure!(self.chat.following(), "End did not follow the newest");

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
            (first.resolves, first.watches, first.snapshots, first.pages, first.threads) == (1, 1, 1, 9, 1),
            "the connection carried {first:?}"
        );

        // The link goes and comes back: one watch and one read. A lost `chat.changed` bus: one watch.
        send(IncomingEvt::Disconnected { reason: "eof".into(), refused: false })?;
        self.witness_pump(events, sent, &mut daemon)?;
        self.witness_frame(&mut frames, &newest, "disconnected")?;
        let down = daemon.seen.clone();
        send(IncomingEvt::Connected {
            session_id: "session".into(),
            revision: 0,
            host: None,
            project_root: None,
            proxy: false,
            resolved: ResolvedDial::Local,
            backend_version: "test".into(),
            repl_running: Vec::new(),
        })?;
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
