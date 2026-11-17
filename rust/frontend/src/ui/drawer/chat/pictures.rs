//! The chat's pictures owner: the thumbnails of the image files the view places, asked for one at a time per host,
//! decoded within bounds on the UI thread and held downscaled, at most `MAX_HELD`. It returns the requests to send and
//! sends nothing itself; only `ChatDrawer` calls it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use image::{ImageReader, Limits, RgbaImage};
use sot_protocol::chat::{ChatFile, PICTURE_MIMES};
use sot_protocol::ops::{ChatCode, ChatFileReq, ChatFileVariant, ChatTarget};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatPicture, ChatReply, OutgoingReq};

use super::owner::{Out, LIMITED_PACE, PACE};

/// Thumbnail requests in flight per host: one, so a daemon's two transfer permits (chat guarantee 32) always leave one for
/// a row's file post while a window loads pictures.
const MAX_IN_FLIGHT: usize = 1;
/// The pictures held or wanted, apart from those in flight; past it the least recently placed leaves first. A frame places
/// at most this many, so a placed picture never pushes out another placed in the same frame.
pub(super) const MAX_HELD: usize = 64;
/// The files refused for the window session; past it the oldest refusal is forgotten (and asked again if placed again).
pub(super) const MAX_FINAL: usize = 1024;
/// The decoder's bounds for one thumbnail: its sides, checked before any pixel buffer, and what it may allocate.
const MAX_SIDE: u32 = 2048;
const MAX_ALLOC: u64 = 32 * 1024 * 1024;
/// A held picture's long side.
const HELD_SIDE: u32 = 360;
/// A `failed` thumbnail is asked again at most this many times, `PACE` apart.
const RETRIES: u8 = 3;

/// A picture's identity: the host it was read from and its record's workspace, channel and file id.
type Key = (HostKey, String, String, String);

enum Stand {
    /// Asked for at `due` or later; `failed` is how many `failed` answers came before.
    Wanted { due: Instant, failed: u8 },
    Flight { failed: u8 },
    /// Drawn by the tiles, which are the next landing's: nothing reads the picture yet.
    Held(#[allow(dead_code)] RgbaImage),
}

struct Entry {
    stand: Stand,
    /// The watch it was last placed under: a request names this target.
    target: ChatTarget,
    /// The placement it was last placed in (`ChatPictures::placement`).
    placed: u64,
    /// The file's summary as last placed.
    file: ChatFile,
}

pub(super) struct ChatPictures {
    entries: HashMap<Key, Entry>,
    /// The files refused for the window session, never asked again while known. A file whose `failed` asks ran out keeps
    /// the summary it was placed with: placed again with another (its record's edit, such as Slack's thumbnail arriving),
    /// it is wanted again. `order` is oldest first, for `MAX_FINAL`.
    finals: HashMap<Key, Option<ChatFile>>,
    order: VecDeque<Key>,
    /// The count of placements so far: the last one's entries are the ones asked for.
    placement: u64,
    connected: HashSet<HostKey>,
    /// Hosts whose daemon does not know `chat.file`; cleared when the host's link changes.
    unsupported: HashSet<HostKey>,
    /// The decode (`decode`; a test's stand-in).
    decode: fn(&[u8]) -> Option<RgbaImage>,
}

impl Default for ChatPictures {
    fn default() -> Self {
        Self { entries: HashMap::new(), finals: HashMap::new(), order: VecDeque::new(), placement: 0, connected: HashSet::new(), unsupported: HashSet::new(), decode }
    }
}

/// Decodes a thumbnail's bytes once: one of wire's `PICTURE_MIMES` by its own bytes, under `Limits` (`MAX_SIDE`, checked
/// before any pixel buffer, and `MAX_ALLOC` for every chunk it parses), then held downscaled to `HELD_SIDE` on its long side.
/// `None` refuses it.
pub(super) fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let mut r = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    if !r.format().is_some_and(|f| PICTURE_MIMES.contains(&f.to_mime_type())) {
        return None;
    }
    let mut limits = Limits::default();
    (limits.max_image_width, limits.max_image_height, limits.max_alloc) = (Some(MAX_SIDE), Some(MAX_SIDE), Some(MAX_ALLOC));
    r.limits(limits);
    let rgba = r.decode().ok()?.to_rgba8();
    let long = rgba.width().max(rgba.height());
    if long <= HELD_SIDE {
        return Some(rgba);
    }
    let fit = |side: u32| ((u64::from(side) * u64::from(HELD_SIDE) / u64::from(long)) as u32).max(1);
    Some(image::imageops::thumbnail(&rgba, fit(rgba.width()), fit(rgba.height())))
}

impl ChatPictures {
    /// The frame placed these records' files under `target` on `host` (no host: nothing is placed). Only their pictures are
    /// asked for, the first `MAX_HELD` of them; a refused one is skipped, unless its retries ran out and its summary changed.
    pub(super) fn place<'a>(&mut self, at: Option<(&HostKey, &ChatTarget)>, files: impl IntoIterator<Item = (&'a str, &'a str, &'a ChatFile)>, now: Instant) {
        self.placement += 1;
        let Some((host, target)) = at else { return };
        let mine = files.into_iter().filter(|(ws, ch, f)| *ws == target.workspace && *ch == target.channel.as_str() && f.is_picture());
        for (ws, ch, f) in mine.take(MAX_HELD) {
            let key = (host.clone(), ws.to_string(), ch.to_string(), f.id.clone());
            match self.finals.get(&key) {
                Some(Some(seen)) if seen != f => {
                    self.finals.remove(&key);
                    self.order.retain(|k| k != &key);
                }
                Some(_) => continue,
                None => {}
            }
            let e = self.entries.entry(key).or_insert_with(|| Entry { stand: Stand::Wanted { due: now, failed: 0 }, target: target.clone(), placed: 0, file: f.clone() });
            (e.target, e.placed, e.file) = (target.clone(), self.placement, f.clone());
        }
        self.bound();
    }

    /// Each host's requests in flight.
    fn flying(&self) -> HashMap<HostKey, usize> {
        let mut flying: HashMap<HostKey, usize> = HashMap::new();
        for (k, e) in &self.entries {
            if matches!(e.stand, Stand::Flight { .. }) {
                *flying.entry(k.0.clone()).or_default() += 1;
            }
        }
        flying
    }

    /// When an ask is next owed: the earliest `due` of the last placement's wanted pictures on a connected host that knows the
    /// op and has room in flight, so the window wakes for it; `None` when none would go.
    pub(super) fn next_ask(&self) -> Option<Instant> {
        let flying = self.flying();
        self.entries
            .iter()
            .filter(|(k, e)| e.placed == self.placement && self.connected.contains(&k.0) && !self.unsupported.contains(&k.0) && flying.get(&k.0).copied().unwrap_or(0) < MAX_IN_FLIGHT)
            .filter_map(|(_, e)| match e.stand {
                Stand::Wanted { due, .. } => Some(due),
                _ => None,
            })
            .min()
    }

    /// The thumbnails to ask for now: those of the last placement that are due, on a connected host that knows the op, at most
    /// `MAX_IN_FLIGHT` in flight per host.
    pub(super) fn asks(&mut self, now: Instant) -> Vec<Out> {
        let mut flying = self.flying();
        let mut due: Vec<(&Key, &mut Entry)> = self
            .entries
            .iter_mut()
            .filter(|(k, e)| e.placed == self.placement && matches!(e.stand, Stand::Wanted { due, .. } if due <= now) && self.connected.contains(&k.0) && !self.unsupported.contains(&k.0))
            .collect();
        due.sort_by(|a, b| a.0.cmp(b.0));
        let mut outs = Vec::new();
        for (k, e) in due {
            let n = flying.entry(k.0.clone()).or_default();
            if *n >= MAX_IN_FLIGHT {
                continue;
            }
            *n += 1;
            let Stand::Wanted { failed, .. } = e.stand else { continue };
            e.stand = Stand::Flight { failed };
            outs.push((k.0.clone(), OutgoingReq::ChatFile { req: ChatFileReq { target: e.target.clone(), file: k.3.clone(), variant: ChatFileVariant::Thumb } }));
        }
        outs
    }

    /// A thumbnail's reply, taken only for a request in flight: decoded and held (a decode that fails or panics is final), or
    /// by its code final, asked again after `PACE` (`failed` at most `RETRIES` times; `busy`, `unavailable`), or after
    /// `LIMITED_PACE` (`limited`). Then the next asks.
    pub(super) fn on_reply(&mut self, host: &HostKey, req: &ChatFileReq, reply: ChatReply<ChatPicture>, now: Instant) -> Vec<Out> {
        let key = (host.clone(), req.target.workspace.clone(), req.target.channel.as_str().to_string(), req.file.clone());
        let decode = self.decode;
        let Some(e) = self.entries.get_mut(&key) else { return self.asks(now) };
        let Stand::Flight { failed } = e.stand else { return self.asks(now) };
        // `Err`: final, with the summary a changed one asks again against, for a file whose retries ran out.
        let next = match reply {
            ChatReply::Done(p) => catch_unwind(AssertUnwindSafe(|| decode(&p.bytes))).ok().flatten().map(Stand::Held).ok_or(None),
            ChatReply::Refused(ChatCode::NotAnImage | ChatCode::TooLarge | ChatCode::BadRequest) => Err(None),
            ChatReply::Refused(ChatCode::Busy | ChatCode::Unavailable) => Ok(Stand::Wanted { due: now + PACE, failed }),
            ChatReply::Refused(ChatCode::Limited) => Ok(Stand::Wanted { due: now + LIMITED_PACE, failed }),
            // `failed` (Slack may have no thumbnail yet), and any other code: asked again, a bounded number of times.
            ChatReply::Refused(_) if failed < RETRIES => Ok(Stand::Wanted { due: now + PACE, failed: failed + 1 }),
            ChatReply::Refused(_) => Err(Some(e.file.clone())),
            ChatReply::Unsupported => {
                self.unsupported.insert(host.clone());
                Ok(Stand::Wanted { due: now, failed })
            }
        };
        match next {
            Ok(stand) => e.stand = stand,
            Err(file) => self.refuse(key, file),
        }
        self.bound();
        self.asks(now)
    }

    /// `key` is refused for the window session; past `MAX_FINAL` the oldest refusal is forgotten.
    fn refuse(&mut self, key: Key, file: Option<ChatFile>) {
        self.entries.remove(&key);
        if self.finals.insert(key.clone(), file).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MAX_FINAL {
            if let Some(old) = self.order.pop_front() {
                self.finals.remove(&old);
            }
        }
    }

    /// A link came or went: nothing of that host is in flight, so each is wanted again, asked at once when the link is up.
    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) {
        if up {
            self.connected.insert(host.clone());
        } else {
            self.connected.remove(host);
        }
        self.unsupported.remove(host);
        for (_, e) in self.entries.iter_mut().filter(|(k, _)| &k.0 == host) {
            if let Stand::Flight { failed } = e.stand {
                e.stand = Stand::Wanted { due: now, failed };
            }
        }
    }

    /// Past `MAX_HELD` entries not in flight, the least recently placed leave.
    fn bound(&mut self) {
        let mut idle: Vec<(u64, Key)> = self.entries.iter().filter(|(_, e)| !matches!(e.stand, Stand::Flight { .. })).map(|(k, e)| (e.placed, k.clone())).collect();
        let over = idle.len().saturating_sub(MAX_HELD);
        idle.sort();
        idle.into_iter().take(over).for_each(|(_, k)| drop(self.entries.remove(&k)));
    }

    /// The held picture of `file` in `channel` on `host`, if any.
    #[cfg(test)]
    pub(super) fn held(&self, host: &str, workspace: &str, channel: &str, file: &str) -> Option<&RgbaImage> {
        match self.entries.get(&(host.to_string(), workspace.to_string(), channel.to_string(), file.to_string()))?.stand {
            Stand::Held(ref p) => Some(p),
            _ => None,
        }
    }

    /// How many pictures are held.
    #[cfg(test)]
    pub(super) fn held_count(&self) -> usize {
        self.entries.values().filter(|e| matches!(e.stand, Stand::Held(_))).count()
    }
}

#[cfg(test)]
#[path = "pictures_tests.rs"]
mod tests;
