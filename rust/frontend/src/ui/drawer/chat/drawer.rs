//! The chat drawer's model: the owner, the row it follows and the reads in flight. State sends what its methods return.

use std::collections::HashMap;
use std::time::Instant;

use sot_protocol::chat::{Ts, MAX_NAME};
use sot_protocol::ops::{ChatCode, ChatHistoryReq, ChatPostKind, ChatPostReq, ChatResolveRes, ChatTarget};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::ResultRowIdentity;
use crate::ui::render::quad::ScreenRect;

use super::compose::{Compose, ComposeText, Edit};
use super::owner::{dead, ChatOwner, Fill, Out, PostShown};
use super::row_channel::{RowChannel, Stand};
use super::thread::Thread;
use super::view::{ChatHit, ChatView, Step};

/// A `chat.history` request in flight. `orphan`: its reader gave it up, so its reply is dropped.
struct Read {
    req: ChatHistoryReq,
    orphan: bool,
}

/// Every read in flight, per host, so two equal reads are never in flight together (replies are matched by value and
/// may come back in either order). A read equal to one in flight waits in `held`.
#[derive(Default)]
struct Flight {
    reads: HashMap<HostKey, Vec<Read>>,
    held: Vec<Out>,
}

impl Flight {
    /// What may go out now; a read equal to one in flight is held, and a read that goes is in flight.
    fn send(&mut self, outs: Vec<Out>) -> Vec<Out> {
        let mut go = Vec::new();
        for out in outs {
            let (host, OutgoingReq::ChatHistory { req }) = &out else { go.push(out); continue };
            let reads = self.reads.entry(host.clone()).or_default();
            if reads.iter().any(|r| &r.req == req) {
                self.held.push(out);
            } else {
                reads.push(Read { req: req.clone(), orphan: false });
                go.push(out);
            }
        }
        go
    }

    /// A reply came: its one equal entry leaves; whether that entry was an orphan, and so the reply is dropped.
    fn answered(&mut self, host: &HostKey, req: &ChatHistoryReq) -> bool {
        let Some(reads) = self.reads.get_mut(host) else { return false };
        reads.iter().position(|r| &r.req == req).is_some_and(|i| reads.remove(i).orphan)
    }

    /// A reader gave the read up: a read still held is cancelled, one in flight stays as an orphan.
    fn abandon(&mut self, host: &HostKey, req: &ChatHistoryReq) {
        let held = |o: &Out| matches!(o, (h, OutgoingReq::ChatHistory { req: r }) if h == host && r == req);
        if let Some(i) = self.held.iter().position(held) {
            self.held.remove(i);
        } else if let Some(r) = self.reads.get_mut(host).and_then(|v| v.iter_mut().find(|r| &r.req == req)) {
            r.orphan = true;
        }
    }

    /// A link changed: nothing of that host is in flight, and no reply will come. Another host's reads stay. A read of
    /// that host still held is cancelled by `owner_outs` and, for the thread, `give_up_read` (a link change is a give-up).
    fn clear(&mut self, host: &HostKey) {
        self.reads.remove(host);
    }
}

#[derive(Default)]
pub(in crate::ui) struct ChatDrawer {
    owner: ChatOwner,
    rows: RowChannel,
    flight: Flight,
    view: ChatView,
    /// The open thread, which holds the channel's view meanwhile: the one switch for which collection the drawer shows.
    thread: Option<Thread>,
    compose: Compose,
    /// How many compose rows the last frame drew at the drawer's foot: the records' body is that much shorter.
    foot: u16,
    /// The words of the last post's outcome, until the status line takes them.
    notice: Option<String>,
    /// The name being typed at the drawer's foot, while Enter found none set: the keys edit it, not the draft.
    asking: Option<String>,
}

/// What the status line says of a post's outcome; a post made says nothing (its draft is gone).
fn post_words(shown: &PostShown) -> Option<String> {
    Some(match shown {
        PostShown::Posted(_) => return None,
        PostShown::Refused(ChatCode::BadRequest) => "not posted: the request is not valid; check [chat] name in settings.toml".into(),
        PostShown::Refused(code) => format!("not posted: {}", code.text()),
        PostShown::Limited(secs, assumed) => wait_words(*secs, *assumed),
        PostShown::Unknown => "may have been posted: check the channel before sending it again".into(),
        PostShown::Outdated => "not posted: the hub must be updated to post".into(),
    })
}

/// The status line's words for a rate limit: the seconds left, with `about` when the wait is a guess.
fn wait_words(secs: u64, assumed: bool) -> String {
    format!("not posted: rate limited; wait {}{secs} s", if assumed { "about " } else { "" })
}

impl ChatDrawer {
    /// The frame: follow the wanted row (see `RowChannel`) and pace the owner. The flag asks for another frame.
    pub(super) fn tick(&mut self, wanted: Option<ResultRowIdentity>, now: Instant) -> (Vec<Out>, bool) {
        let step = self.rows.tick(wanted, now);
        let mut outs = Vec::new();
        // The prompt belongs to the draft being posted and ends where its thread ends: a changed row, a hidden drawer and an
        // unmapped row all leave the row unmapped for the frame, so a message typed later is never typed into the name.
        if self.rows.stand() != Stand::Mapped {
            self.cancel_name();
        }
        if step.changed {
            // Another row's records are not the reader's place, and its thread is no one's.
            self.close_thread();
            self.view.reset();
            outs = self.owner_outs(ChatOwner::stop);
        }
        outs.extend(step.resolve);
        outs.extend(self.owner_outs(|o| o.tick(now)));
        outs.extend(self.thread_outs(|t| t.tick(now)));
        (outs, step.settling)
    }

    /// When the owner's next short-paced watch is due (`ChatOwner::next_ask`): the instant to wake the window for.
    pub(in crate::ui) fn next_ask(&self) -> Option<Instant> {
        self.owner.next_ask()
    }

    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) -> Vec<Out> {
        self.rows.on_connection(host, up);
        self.cancel_name();
        self.flight.clear(host);
        let mut outs = self.owner_outs(|o| o.on_connection(host, up, now));
        if self.thread.as_ref().is_some_and(|t| t.host() == host) {
            if up {
                outs.extend(self.restart_thread());
            } else {
                self.give_up_read();
            }
        }
        outs
    }

    pub(super) fn on_bus_lost(&mut self, host: &HostKey, bus: &str, now: Instant) -> Vec<Out> {
        self.owner_outs(|o| o.on_bus_lost(host, bus, now))
    }

    pub(super) fn on_evt(&mut self, host: &HostKey, evt: ChatEvt, now: Instant) -> Vec<Out> {
        match evt {
            // The wanted row's answer decides its watch: `mapped` watches, anything else (unmapped, invalid, refused, an
            // old daemon) ends it. An answer for a row no longer wanted is nothing.
            ChatEvt::Resolved { row, reply } if self.rows.wants(host, &row) => {
                self.rows.answered(&reply, now);
                match reply {
                    ChatReply::Done(ChatResolveRes::Mapped { workspace, channel, .. }) => {
                        let target = ChatTarget { row, workspace, channel };
                        self.owner_outs(|o| o.watch(host.clone(), target, now))
                    }
                    _ => self.owner_outs(ChatOwner::stop),
                }
            }
            ChatEvt::Resolved { .. } => Vec::new(),
            ChatEvt::History { req, reply } => {
                let orphan = self.flight.answered(host, &req);
                let mut outs = match (orphan, req.thread.is_some()) {
                    (true, _) => Vec::new(),
                    (false, true) => self.thread_outs(|t| t.on_reply(host, &req, reply, now)),
                    (false, false) => self.owner_outs(|o| o.on_evt(host, ChatEvt::History { req, reply }, now)),
                };
                let held = std::mem::take(&mut self.flight.held);
                outs.extend(self.flight.send(held));
                outs
            }
            ChatEvt::Posted { req, reply } => {
                let mut made = None;
                let mut outs = self.owner_outs(|o| {
                    made = o.on_posted(host, &req, reply, now);
                    Vec::new()
                });
                if let Some(row) = made {
                    self.compose.clear(&ResultRowIdentity { host: host.clone(), workspace_id: row });
                    // A post into the open thread: the thread is read again whole.
                    if req.thread.as_ref().is_some_and(|p| self.thread.as_ref().is_some_and(|t| t.is(host, &req.target, p))) {
                        outs.extend(self.restart_thread());
                    }
                }
                outs
            }
            evt => self.owner_outs(|o| o.on_evt(host, evt, now)),
        }
    }

    /// Enter: posts `row`'s draft once, to the channel and thread it was begun in, under `name` (`[chat] name`, `None` when unset or
    /// blank). It needs a draft with something in it, no post waiting and no `limited` wait running; with no name it asks for
    /// one (`asking`) and sends nothing. The draft stays until a reply proves the post.
    pub(in crate::ui) fn send(&mut self, row: Option<&ResultRowIdentity>, name: Option<&str>, now: Instant) -> Result<Vec<Out>, String> {
        let Some(row) = row else { return Err("no row to write for".into()) };
        if self.owner.posting_row().is_some() {
            return Err("posting…".into());
        }
        let Some(draft) = self.compose.draft(row).filter(|d| !d.text.trim().is_empty()) else { return Err("nothing to post".into()) };
        // No name set: the foot asks for one, the draft stays, and nothing is sent; the next Enter posts it.
        let Some(name) = name else {
            // The prompt is for a row whose channel is mapped (`tick` ends it anywhere else): any other stand says its own words.
            let stand = self.rows.stand();
            if stand != Stand::Mapped {
                return Err(format!("not posted: {}", stand.words(self.rows.host())));
            }
            self.ask_name();
            return Ok(Vec::new());
        };
        if let Some((secs, assumed)) = self.owner.waiting(now) {
            return Err(wait_words(secs, assumed));
        }
        let req = ChatPostReq { target: draft.dest.clone(), thread: draft.thread.clone(), kind: ChatPostKind::Person, name: name.to_string(), text: draft.text.clone() };
        let host = row.host.clone();
        Ok(self.owner_outs(|o| o.post(host, req).into_iter().collect()))
    }

    /// The foot asks for the posting name; a name half typed stays.
    pub(super) fn ask_name(&mut self) {
        self.asking.get_or_insert_with(String::new);
    }

    /// The name typed so far, while the foot asks for one.
    pub(in crate::ui) fn asking(&self) -> Option<&str> {
        self.asking.as_deref()
    }

    /// A key edits the name being asked for; nothing when the foot is not asking. A paste with a newline or another control
    /// character, or one past `MAX_NAME` bytes of name, is refused whole with words (never joined or cut), the name as it was.
    pub(in crate::ui) fn edit_name(&mut self, edit: Edit) -> Result<(), &'static str> {
        let Some(name) = self.asking.as_mut() else { return Ok(()) };
        let add = match edit {
            Edit::Text(s) if s.chars().any(char::is_control) => return Err("not pasted: a name holds no newline or other control character"),
            Edit::Text(s) => s,
            Edit::Space => " ",
            Edit::Backspace => {
                name.pop();
                return Ok(());
            }
            Edit::Newline => return Ok(()),
        };
        if name.len() + add.len() > MAX_NAME {
            return Err("name is full: nothing added");
        }
        name.push_str(add);
        Ok(())
    }

    /// The foot stops asking (Escape, or the row, the link or the view it asked in is gone); the draft was never touched.
    pub(in crate::ui) fn cancel_name(&mut self) {
        self.asking = None;
    }

    /// The words of the post outcome that has not yet been said.
    pub(in crate::ui) fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// The title: the channel, then the words of the outcome (the row's, or for a mapped row the fill's).
    pub(in crate::ui) fn title(&self) -> String {
        let host = self.rows.host();
        let words = match (self.rows.stand(), self.owner.fill()) {
            (Stand::Mapped, Some(Fill::Needed | Fill::Reading)) => Stand::Resolving.words(host),
            // The older pages are the channel's: a thread shows none of them.
            (Stand::Mapped, Some(Fill::Complete)) if self.thread.is_some() => String::new(),
            (Stand::Mapped, Some(Fill::Complete)) => self.owner.older_words().unwrap_or_default(),
            (Stand::Mapped, Some(Fill::Gap(Some(ChatCode::Unavailable)))) => "the hub's chat service is not running: its daemon log says why".into(),
            // A target refused for good is asked again only at a row or link change.
            (Stand::Mapped, Some(Fill::Gap(Some(code)))) if dead(code) => code.text().into(),
            (Stand::Mapped, Some(Fill::Gap(code))) => format!("not up to date: {}", code.map_or("no snapshot", ChatCode::text)),
            (stand, _) => stand.words(host),
        };
        let channel = self.owner.channel().map(|c| format!("{} ", self.rows.label(c))).unwrap_or_default();
        // Each phrase is its own: the thread's, then the link's and the watch's.
        let phrases: Vec<String> = self.thread.iter().map(Thread::words).chain([words]).filter(|w| !w.is_empty()).collect();
        let thread = if self.thread.is_some() { "· thread " } else { "" };
        format!(" chat · {channel}{thread}{}", if phrases.is_empty() { String::new() } else { format!("{} ", phrases.join(" · ")) })
    }

    /// Whether the drawer says `no messages`: of the channel, so never over a thread.
    pub(in crate::ui) fn no_messages(&self) -> bool {
        self.thread.is_none() && self.owner.no_messages()
    }

    /// Places the held records in `body` (the drawer's pixels) and shapes those that reach it, and returns the frame's
    /// rectangles for the drawer's `inside` (`view::ChatView::backs`), in painting order. They come only from this layout, so
    /// the frame cannot take the last frame's.
    pub(in crate::ui) fn layout(&mut self, fonts: &mut cosmic_text::FontSystem, body: ScreenRect, scale: f32, inside: ScreenRect) -> Vec<crate::ui::render::cells::CellQuad> {
        let now = chrono::Local::now();
        // The thread's records are its own; the channel's reader keeps the place it left.
        let records = self.thread.as_ref().map_or_else(|| self.owner.records(), Thread::records);
        self.view.layout(fonts, &records, body, scale, now.timestamp(), *now.offset());
        if self.thread.is_none() {
            self.owner.anchored(self.view.anchor.as_ref().map(|a| a.0.clone()));
            self.owner.reaches(self.view.at_oldest);
        }
        self.view.backs(inside)
    }

    /// A key or the wheel moves the reader through the records (`view::ChatView::scroll`).
    pub(in crate::ui) fn scroll(&mut self, up: bool, step: Step) {
        self.view.scroll(up, step);
    }

    /// A mark key moves the mark (`view::ChatView::mark`).
    pub(in crate::ui) fn mark(&mut self, older: bool) {
        self.view.mark(older);
    }

    /// The reader asks for the newest.
    pub(in crate::ui) fn latest(&mut self) {
        self.view.latest();
    }

    /// ArrowRight: opens the marked message's thread. The channel's view is parked in it, so its position and mark return at close;
    /// the owner holds no anchor and asks for no page meanwhile. Nothing while a thread is open or none is marked.
    pub(in crate::ui) fn open_thread(&mut self) -> Vec<Out> {
        let Some(row) = self.rows.row().cloned() else { return Vec::new() };
        let (Some(mark), Some(target), None) = (self.view.mark.clone(), self.watch_for(&row), &self.thread) else { return Vec::new() };
        let Some((parent, deleted)) = self.owner.records().into_iter().find(|(r, _)| r.ts == mark) else { return Vec::new() };
        self.thread = Some(Thread::new(row.host, target.clone(), parent.clone(), deleted, self.parked_view()));
        self.owner.anchored(None);
        self.owner.reaches(false);
        self.thread_outs(Thread::start)
    }

    /// The channel's view, set aside for a thread. Nothing selectable is parked: a press made in the thread could not
    /// clear a selection held in the view parked behind it, and it would come back at close.
    fn parked_view(&mut self) -> super::view::ChatView {
        let mut view = std::mem::take(&mut self.view);
        view.clear_selection();
        view
    }

    /// Escape: closes the open thread; the channel's view returns, its mark on the parent or, if that gave way, passed on.
    pub(in crate::ui) fn close_thread(&mut self) -> bool {
        let Some(t) = self.thread.take() else { return false };
        t.reading.iter().for_each(|req| self.flight.abandon(t.host(), req));
        self.view = t.parked;
        self.owner.reserve(0);
        true
    }

    /// The one restart of the open thread's walk: the read in flight is given up first (an equal reply still coming can then
    /// never be taken as the restart's answer), then the first page is asked for.
    fn restart_thread(&mut self) -> Vec<Out> {
        self.give_up_read();
        self.thread_outs(Thread::start)
    }

    /// The open thread's read in flight is given up, so one held behind it is cancelled and its reply, if it comes, is dropped.
    fn give_up_read(&mut self) {
        if let Some((host, req)) = self.thread.as_mut().and_then(|t| t.reading.take().map(|r| (t.host().clone(), r))) {
            self.flight.abandon(&host, &req);
        }
    }

    /// Runs `f` on the open thread, tells the owner how much of the bound it holds, and sends its reads through the set.
    fn thread_outs(&mut self, f: impl FnOnce(&mut Thread) -> Vec<Out>) -> Vec<Out> {
        let outs = self.thread.as_mut().map(f).unwrap_or_default();
        self.owner.reserve(self.thread.as_ref().map_or(0, Thread::len));
        self.flight.send(outs)
    }

    #[cfg(test)]
    pub(super) fn point_at(&self, ts: &Ts, line: usize, byte: usize) -> Option<(f32, f32)> {
        self.view.point_at(ts, line, byte)
    }

    #[cfg(test)]
    pub(super) fn thread_len(&self) -> usize {
        self.thread.as_ref().map_or(0, Thread::len)
    }

    /// The chat is not shown: no record is prepared.
    pub(in crate::ui) fn hide(&mut self) {
        self.close_thread();
        self.cancel_name();
        self.view.hide();
    }

    /// The selection's view calls: a press, the drag and the copy (`view::ChatView`).
    pub(in crate::ui) fn hit(&self, at: (f32, f32)) -> Option<ChatHit> {
        self.view.hit(at)
    }

    pub(in crate::ui) fn press(&mut self, hit: Option<ChatHit>) -> bool {
        self.view.press(hit)
    }

    pub(in crate::ui) fn extend_drag(&mut self, at: (f32, f32)) -> bool {
        self.view.extend_drag(at)
    }

    pub(in crate::ui) fn release(&mut self) {
        self.view.release();
    }

    /// What `chat.copy_selection` copies (`view::ChatView::copy_text`).
    pub(in crate::ui) fn copy_text(&self) -> Option<String> {
        self.view.copy_text()
    }

    pub(in crate::ui) fn areas(&self) -> Vec<crate::ui::render::text::ExtraArea<'_>> {
        self.view.areas()
    }

    /// The watched channel, if `row` (host and id) is the row the frame follows: a watch is ever the followed row's, so
    /// a row the frame has not yet moved to has none.
    fn watch_for(&self, row: &ResultRowIdentity) -> Option<&ChatTarget> {
        (self.rows.row() == Some(row)).then(|| self.owner.target()).flatten()
    }

    /// The person edits the draft of `row`, the row the chat shows at the key (not the last frame's, which a row switch has
    /// not yet moved). A draft begins in the watched channel only if that watch is `row`'s; a refusal is the status
    /// line's words.
    pub(in crate::ui) fn edit(&mut self, row: Option<&ResultRowIdentity>, edit: Edit) -> Result<(), &'static str> {
        let Some(row) = row else { return Err("no row to write for") };
        if self.owner.posting_row() == Some((&row.host, row.workspace_id.as_str())) {
            return Err("posting…");
        }
        let dest = self.watch_for(row).cloned();
        self.compose.edit(row, dest.as_ref().map(|t| (t, self.thread.as_ref().map(Thread::parent_ts))), edit)
    }

    /// Rows a listing retired lose their drafts.
    pub(in crate::ui) fn retire(&mut self, rows: &[ResultRowIdentity]) {
        self.compose.retire(rows);
    }

    /// What the compose rows are made of, while a channel is known to write to: the shown row's draft and where it goes
    /// (`reply in thread of h:mm AM`, the parent's time with its date before it for another day, `project::when`, then
    /// ` in <channel>` when the draft's channel is not the watched one, or
    /// `to <channel>`, spelled as the title spells it), else where the next draft would go: the open thread, or the
    /// watched channel. The foot is silent only when the destination is what the title shows: no thread in the draft,
    /// none open, and the draft's channel the watched one.
    pub(in crate::ui) fn compose_text(&self, focused: bool) -> Option<ComposeText> {
        let now = chrono::Local::now();
        self.compose_text_at(focused, now.timestamp(), *now.offset())
    }

    /// `compose_text` at a given time and offset, which the parent's time in `reply in thread of` is shown against.
    pub(super) fn compose_text_at(&self, focused: bool, now: i64, offset: chrono::FixedOffset) -> Option<ComposeText> {
        if let Some(name) = &self.asking {
            return Some(ComposeText::asking(name, focused));
        }
        let draft = self.rows.row().and_then(|r| self.compose.draft(r));
        let dest = draft.map(|d| &d.dest).or_else(|| self.rows.row().and_then(|r| self.watch_for(r)))?;
        // With no draft, the line names where the next one would go: the open thread's parent.
        let thread = draft.map_or_else(|| self.thread.as_ref().map(Thread::parent_ts), |d| d.thread.as_ref());
        let elsewhere = self.owner.channel() != Some(dest.channel.as_str());
        let to = match thread {
            // A thread draft says its channel too when the title has moved to another one: the post goes where it was begun.
            Some(p) => {
                let at = if elsewhere { format!(" in {}", self.rows.label(dest.channel.as_str())) } else { String::new() };
                Some(format!("reply in thread of {}{at}", super::project::when(p.as_str(), now, offset)))
            }
            None if self.thread.is_none() && !elsewhere => None,
            None => Some(format!("to {}", self.rows.label(dest.channel.as_str()))),
        };
        Some(ComposeText::of(draft.map(|d| d.text.as_str()), to, focused))
    }

    pub(in crate::ui) fn set_foot(&mut self, rows: u16) {
        self.foot = rows;
    }

    pub(in crate::ui) fn foot(&self) -> u16 {
        self.foot
    }

    /// The wanted row's draft: its text and the channel it goes to.
    #[cfg(test)]
    pub(super) fn draft(&self) -> Option<(&str, &str)> {
        let d = self.compose.draft(self.rows.row()?)?;
        Some((d.text.as_str(), d.dest.channel.as_str()))
    }

    /// The anchor the owner holds for the trim.
    #[cfg(test)]
    pub(super) fn owner_anchor(&self) -> Option<sot_protocol::chat::Ts> {
        self.owner.anchor_held()
    }

    /// A test places the reader at the oldest record, as a frame's layout would.
    #[cfg(test)]
    pub(super) fn reach_oldest(&mut self, yes: bool) {
        self.owner.reaches(yes);
    }

    #[cfg(test)]
    pub(super) fn held(&self) -> usize {
        self.owner.records().len()
    }

    #[cfg(test)]
    pub(super) fn placed_text(&self) -> Vec<String> {
        self.view.placed_text()
    }

    #[cfg(test)]
    pub(super) fn marked(&self) -> Option<sot_protocol::chat::Ts> {
        self.view.mark.clone()
    }

    #[cfg(test)]
    pub(super) fn following(&self) -> bool {
        self.view.anchor.is_none()
    }

    #[cfg(test)]
    pub(super) fn fill(&self) -> Option<Fill> {
        self.owner.fill()
    }

    /// Runs `f` on the owner. A read the owner dropped (its watch stopped or replaced, or its link changed) is given up before
    /// the outs are sent, so a read `f` itself asks for is never cancelled by the same call; a read answered is already out
    /// of the set. The check compares reads by value, so an owner call must not give a read up and ask for an equal one in
    /// the same call.
    fn owner_outs(&mut self, f: impl FnOnce(&mut ChatOwner) -> Vec<Out>) -> Vec<Out> {
        let before = [self.owner.reading(), self.owner.paging()];
        let watched = self.owner.target().cloned();
        let outs = f(&mut self.owner);
        // A thread lives only under its channel's watch: a watch replaced by another channel's or stopped closes it.
        if self.owner.target() != watched.as_ref() {
            self.close_thread();
        }
        for (host, req) in before.into_iter().flatten().filter(|b| Some(b) != self.owner.reading().as_ref() && Some(b) != self.owner.paging().as_ref()) {
            self.flight.abandon(&host, &req);
        }
        if let Some(outcome) = self.owner.take_report() {
            self.notice = post_words(&outcome);
        }
        self.flight.send(outs)
    }
}
